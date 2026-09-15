//! The audio readback facility: the decoded PCM of a playing `WaveSoundBuffer`.
//!
//! Part of [`crate::plugin_api`]: a plugin crate's production dependencies are
//! `krkr-engine` and `krkr-tjs2` only, and the engine does not depend on
//! `krkr-audio`, so the engine cannot read the decoded-PCM tap itself.  This
//! module is the seam: it owns the vocabulary ([`WavePcmSource`],
//! [`WavePcmWindow`]) and the per-process slot a plugin that *can* name
//! `krkr-audio` fills ([`set_wave_pcm_source`]), and it answers the questions
//! the engine's `WaveSoundBuffer.getVisBuffer` and the sample-reading plugins
//! ask — which samples the instance at the play cursor is rendering, plus the
//! instance id they belong to ([`wave_audio_instance`]).
//!
//! Reference: `tTJSNI_WaveSoundBuffer::GetVisBuffer`
//! (`krkrz/src/core/sound/win32/WaveImpl.cpp:3274-3330`) reads
//! `numsamples` samples starting `aheadsamples` frames after the play
//! position, converts them to 16-bit (`CopyVisBuffer`, `:3253-3272`) and
//! returns how many it wrote — `0` when the buffer is not playing, has no
//! visualization buffer, or the requested channel count is neither mono nor
//! the stream's own.  `getSample.dll` (`getSample/main.cpp:24`, `:92`) and
//! `fftgraph.dll` (`fftgraph/Main.cpp:36-65`) are the reference consumers;
//! both call that method through a raw `short*` this engine cannot hand out,
//! which is why the read here is a Rust window instead (see
//! [`WaveSoundBuffer.getVisBuffer`](crate::native::classes)'s array
//! destination for the script-facing half of the same fetch).
//!
//! **When the source is missing.** A host with no audio backend (the browser
//! shells, `--virtual-audio`, a test that never installs one) has no tap, so
//! [`read_wave_pcm_window`] answers `None` and every consumer returns the
//! reference's not-playing value instead of inventing samples.  The web shell
//! is that host even though its dependency graph now carries `krkr-audio`:
//! no `AudioSystem` is installed there, so nothing publishes a tap.  That is the
//! same answer the reference gives for a buffer that is not playing; it is not
//! a lie about a playing one — [`WavePcmSource`] is the only sample source this
//! engine has, and a plugin that needs different samples installs its own.
//!
//! **The one playback path with no samples.** A sound kira decodes itself — a
//! storage load whose policy is `Streaming` (`AudioLoadPolicy::Auto` picks it
//! for a looping or BGM buffer) — keeps its frames in kira's private decode
//! queue, so `krkr-audio` registers no tap for it and reads for that instance
//! answer `None`, exactly as for a host with no backend.  A statically loaded
//! buffer (SE, voice, preloaded sounds, and any buffer that carries filters)
//! does register one.  Closing the streaming gap needs a file decoder of our
//! own in `krkr-audio` (`register_sound_tap` documents the two routes).
//!
//! **Thread contract.** The engine reads windows on the script thread and the
//! source reads the tap (which is lock-protected and never blocks a producer,
//! see `krkr-audio`'s `pcm_tap` module), so [`WavePcmSource: Send + Sync`] is a
//! hard requirement.

use std::sync::{Arc, Mutex, OnceLock};

use krkr_tjs2::runtime::{ObjectHandle, Runtime};

use crate::KrkrHost;

pub use krkr_core::{AudioInstanceId, PcmAudioSpec};

/// Where a window sits relative to the instance's play cursor, in the
/// reference's terms: `GetVisBuffer(dest, numsamples, channels, aheadsamples)`
/// reads `ahead` frames ahead of the play position and then `samples` frames.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WavePcmRequest {
    /// Frames the window starts past the play position (`aheadsamples`,
    /// `WaveImpl.cpp:3296-3298`).
    pub ahead_frames: u32,
    /// Frames the window covers (`numsamples`).
    pub frames: u32,
}

impl WavePcmRequest {
    pub const fn new(ahead_frames: u32, frames: u32) -> Self {
        Self {
            ahead_frames,
            frames,
        }
    }
}

/// Playback state of the tapped instance, as the reference's guards test it
/// (`DSBufferPlaying && BufferPlaying`, `WaveImpl.cpp:3282`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WavePcmState {
    /// Frames are being rendered; the window is the audio playing now.
    Playing,
    /// Playback is paused: the reference answers `0` for the same reason, the
    /// samples are not being rendered.
    Paused,
    /// The instance has stopped; the reference answers `0`.
    Stopped,
}

/// One read of a playing instance's decoded PCM.
#[derive(Clone, Debug, PartialEq)]
pub struct WavePcmWindow {
    /// Format of the tapped samples (`Format`, `WaveImpl.cpp:3284-3300`).
    pub spec: PcmAudioSpec,
    pub state: WavePcmState,
    /// Interleaved samples (`spec.channels` values per frame) for the
    /// requested window, zero-filled where no decoded audio is available.
    pub samples: Vec<f32>,
    /// How many frames of `samples` carry decoded audio (`0` is the
    /// reference's "nothing written").
    pub available_frames: u32,
}

impl WavePcmWindow {
    /// The window's samples as the reference's 16-bit samples, one channel:
    /// `channels == 1` asks for the mono downmix `TVPConvertFloatPCMTo16bits`
    /// makes (`sound/WaveIntf.cpp:124-146`), otherwise the interleaved stream
    /// samples (`:111-123`).
    pub fn to_i16(&self, channels: u32) -> Vec<i16> {
        let source_channels = self.spec.channels.max(1) as usize;
        if channels == 1 && source_channels > 1 {
            let scale = 32768.0 / source_channels as f32;
            self.samples
                .chunks_exact(source_channels)
                .map(|frame| {
                    // `float nc = 32768.0f / channels; ... t += sample * nc`,
                    // then rounded away from zero and clamped (`:126-143`).
                    let mixed: f32 = frame.iter().sum::<f32>() * scale;
                    let rounded = if mixed > 0.0 {
                        (mixed + 0.5) as i32
                    } else {
                        (mixed - 0.5) as i32
                    };
                    rounded.clamp(-32768, 32767) as i16
                })
                .collect()
        } else {
            // `def__Z32RisaPCMConvertLoopFloat32ToInt16PvPKvj`: scale by
            // 32768, clamp to the 16-bit range, truncate toward zero.
            self.samples
                .iter()
                .map(|sample| (sample * 32768.0).clamp(-32768.0, 32767.0) as i16)
                .collect()
        }
    }
}

/// The decoded-PCM source of the process's audio backend.
pub trait WavePcmSource: Send + Sync {
    /// Reads the window of `id`, or `None` when no such instance is tapped.
    ///
    /// The window is relative to the instance's play cursor: `ahead_frames`
    /// past it, `frames` long.  A state other than [`WavePcmState::Playing`]
    /// may still return a window — the reference's callers treat a non-playing
    /// instance as "no samples", which is what the state is for.
    fn read_window(&self, id: AudioInstanceId, request: WavePcmRequest) -> Option<WavePcmWindow>;
}

/// Installs the process-wide source, replacing the previous one, and returns
/// it.
///
/// The installer is a plugin (or a shell) that can name the audio backend:
/// `krkr-audio`'s `active_pcm_tap()` plus [`wave_audio_instance`] is the whole
/// implementation, and `krkr-plugins`' audio module ships one.  Install from a
/// plugin's `register`; the slot is process-wide, not per runtime.
pub fn set_wave_pcm_source(source: Arc<dyn WavePcmSource>) -> Option<Arc<dyn WavePcmSource>> {
    let slot = source_slot();
    let mut guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.replace(source)
}

/// The installed source, when a plugin or shell installed one.
pub fn wave_pcm_source() -> Option<Arc<dyn WavePcmSource>> {
    let slot = source_slot();
    let guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.clone()
}

/// Installs `source` unless a source is already installed, and answers the one
/// now in place.
///
/// This is what an audio plugin uses at registration: the process's default is
/// the tap-backed adapter, and the first plugin to register installs it.  A
/// shell (or a test) that installed its own source first keeps it — the slot
/// would otherwise be replaced by whichever plugin happened to register last.
pub fn install_wave_pcm_source(source: Arc<dyn WavePcmSource>) -> Arc<dyn WavePcmSource> {
    let slot = source_slot();
    let mut guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.get_or_insert(source).clone()
}

/// Removes the installed source, so every read answers as if no audio backend
/// existed.  This is what a test that installed a source restores afterwards
/// (the slot is process-wide), and what a shell that shuts its audio down
/// would call.
pub fn clear_wave_pcm_source() -> Option<Arc<dyn WavePcmSource>> {
    let slot = source_slot();
    let mut guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.take()
}

fn source_slot() -> &'static Mutex<Option<Arc<dyn WavePcmSource>>> {
    static SLOT: OnceLock<Mutex<Option<Arc<dyn WavePcmSource>>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// The audio instance a `WaveSoundBuffer` object plays under.
///
/// Every wave instance gets one when the engine constructs it
/// (`native/classes.rs`, the same id its `AudioCommand`s carry), so this is the
/// name the tap and the audio commands agree on.  `None` for an object that is
/// not a wave buffer (or a class object, which has no instance).
pub fn wave_audio_instance(
    runtime: &Runtime<KrkrHost>,
    buffer: ObjectHandle,
) -> Option<AudioInstanceId> {
    let buffer = runtime.bound_this(buffer).unwrap_or(buffer);
    runtime
        .host()
        .native_audio_buffer(buffer)
        .map(|buffer| buffer.id)
}

/// Whether the instance is rendering frames right now — the reference's
/// `DSBufferPlaying && BufferPlaying` guard (`WaveImpl.cpp:3282`), minus the
/// `useVisBuffer` flag, which stays a class-level property in this engine with
/// no consumer.
pub fn wave_audio_is_playing(runtime: &Runtime<KrkrHost>, buffer: ObjectHandle) -> bool {
    let buffer = runtime.bound_this(buffer).unwrap_or(buffer);
    runtime
        .host()
        .native_audio_buffer(buffer)
        .is_some_and(|buffer| buffer.playing && !buffer.paused)
}

/// The plugin-facing half of `getVisBuffer`: the window of `buffer`'s decoded
/// PCM, when the instance exists, is playing and a source is installed.
///
/// This is the Rust window the reference hands through a `short*`; the engine's
/// script-facing `getVisBuffer` calls the same source and writes the same
/// samples into a TJS array.
pub fn read_wave_pcm_window(
    runtime: &Runtime<KrkrHost>,
    buffer: ObjectHandle,
    request: WavePcmRequest,
) -> Option<WavePcmWindow> {
    if !wave_audio_is_playing(runtime, buffer) {
        return None;
    }
    let id = wave_audio_instance(runtime, buffer)?;
    let source = wave_pcm_source()?;
    let window = source.read_window(id, request)?;
    matches!(window.state, WavePcmState::Playing).then_some(window)
}
