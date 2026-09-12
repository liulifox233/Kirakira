//! Decoded-PCM tap for playing audio instances.
//!
//! `getSample.dll`, `fftgraph` and the engine's `WaveSoundBuffer.filters` DSP
//! chain all need the decoded PCM of a *playing* `WaveSoundBuffer` plus a
//! sample-accurate cursor to index it. kira 0.12 exposes neither:
//! `StaticSoundHandle::position()` and `StreamingSoundHandle::position()` return
//! an `f64` source position refreshed once per render batch, the static sound's
//! decoded frames stay inside `StaticSoundData::frames`, and streaming sounds
//! decode on kira's own thread into a private frame queue.
//!
//! This module is that tap:
//!
//! * [`PcmTap`] is the registry the audio worker owns; it maps an
//!   [`AudioInstanceId`] to one live tap instance.
//! * [`PcmTapFeed`] is the write side. Sample producers (the live-PCM movie
//!   decoder, the preloaded buffer of a static sound) push decoded frames
//!   through it, and the control plane reports playback position and state.
//! * [`PcmTap::read`] is the read side used by the engine/plugins: it returns a
//!   window of interleaved `f32` frames around the rendered-frame cursor,
//!   zero-filled where no decoded audio is available.
//!
//! # Cursor semantics
//!
//! The cursor is a **rendered-frame clock**: it advances one frame per frame of
//! audio the sound actually renders, and it is expressed in frames (never in
//! `f64` seconds). It is monotonic while playing and survives a loop wrap,
//! because a loop is a repetition of the stream, not a rewind of the clock.
//!
//! | event | cursor |
//! | ----- | ------ |
//! | playing | advances by the frames rendered |
//! | loop wrap | keeps counting (never resets to 0) |
//! | seek | moves with the stream: forward seeks advance it, backward seeks move it back |
//! | pause | frozen; position updates only re-anchor the source |
//! | stop | frozen at its last value; the readable window becomes silent |
//!
//! The coordinate space the cursor counts in is the feed's:
//!
//! * A feed that **publishes coordinates** ([`PcmTapFeed::push_at`], the live
//!   PCM movie decoder) works in absolute stream frames: coordinate `c` is the
//!   audio the sound renders at stream frame `c`. The tap sets the cursor from
//!   the reported render position directly, so a window at the cursor addresses
//!   exactly what is playing no matter how far the producer has decoded ahead of
//!   playback — the producer, which knows the stream position of every chunk it
//!   emits, is the only side that can keep that alignment.
//! * A feed that **attaches a decoded buffer** ([`PcmTapFeed::attach_decoded`],
//!   static sounds) works in unrolled buffer frames: the cursor keeps counting
//!   across loop wraps and the decoded buffer is addressed through the reported
//!   source position.
//!
//! The source (stream) frame the cursor points at is reported as
//! [`PcmTapSnapshot::source_frame`]. Reads are relative to the cursor:
//! [`PcmTapWindow::back_frames`] reaches into the recent past (what `fftgraph`
//! visualises) and [`PcmTapWindow::ahead_frames`] looks ahead (the reference
//! engine's `sampleAhead`).
//!
//! # Capacity and availability
//!
//! A tap instance keeps at most [`PcmTap::capacity_frames`] frames, i.e.
//! [`DEFAULT_CAPACITY_FRAMES`] = 65_536 frames by default (about 1.4 s at
//! 48 kHz; ~512 KiB of interleaved stereo `f32`). A published coordinate stays
//! readable until it is overwritten, i.e. while it is within `capacity_frames`
//! of the feed's write head — which is what lets the frames a producer decodes
//! ahead of playback (up to kira's 16 384-frame queue) stay readable as the
//! cursor catches up.
//!
//! Nothing is allocated or copied until somebody reads: a push-fed instance
//! allocates its ring on the first read and ignores publishes before that, so
//! the tap can sit inside the audio path on every platform at no cost. Audio the
//! producer emitted before the tap was armed was never captured, and a read over
//! those coordinates reports zero [`PcmTapSnapshot::available_frames`]: silence,
//! never audio from somewhere else. The same holds for coordinates the producer
//! never published (a gap left by a dropped publish or a stalled producer), for
//! coordinates already overwritten, for a window before the stream start or past
//! the end of a non-looping sound, and for every read after
//! [`PcmTapFeed::stop`]. This mirrors `GetVisBuffer`'s "return how many samples
//! were written, 0 when the sound is not playing"
//! (`krkrz/src/core/sound/win32/WaveImpl.cpp:3274`).
//!
//! # Concurrency
//!
//! Readers take the instance's buffer lock and copy the window out. The
//! per-frame write path ([`PcmTapFeed::push_at`]) only ever uses `try_lock`, so
//! a read can never block a producer: a publish that loses that race is dropped
//! (counted by [`PcmTapFeed::dropped_pushes`]), the coordinates it covered stay
//! unpublished, and later publishes resume at their own coordinates — a read
//! over the hole is silence rather than stale audio. The control-plane writers,
//! [`PcmTapFeed::attach_decoded`] and [`PcmTapFeed::stop`], take the lock
//! normally: they run on the audio control thread, never in the render callback,
//! so the only reader that can delay them is one copying a window. Position and
//! state updates are plain atomics, so nothing else in the control plane blocks
//! either.

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
};

use kira::Frame;
use krkr_core::{AudioInstanceId, PcmAudioSpec};

/// Default ring capacity in frames (per channel; the sample buffer holds
/// `capacity_frames * channels` interleaved values).
///
/// 65_536 frames is about 1.4 s at 48 kHz, comfortably above the 16_384-frame
/// queue kira keeps between a streaming decoder and the renderer
/// (`kira/src/sound/streaming/sound/decode_scheduler.rs:19`), so the frames a
/// decoder pushed ahead of playback stay readable when the cursor catches up.
pub const DEFAULT_CAPACITY_FRAMES: u32 = 65_536;

/// Hard cap on the frames a single read returns, so a script asking for an
/// absurd window cannot make the engine allocate without bound.
pub const MAX_READ_FRAMES: u32 = 1 << 20;

const STATE_PLAYING: u8 = 0;
const STATE_PAUSED: u8 = 1;
const STATE_STOPPED: u8 = 2;

const SOURCE_UNKNOWN: u64 = u64::MAX;

/// Playback state of a tapped instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PcmTapState {
    /// Frames are being rendered; the cursor advances.
    Playing,
    /// Playback is paused; the cursor is frozen.
    Paused,
    /// Playback stopped; the readable window is silent.
    Stopped,
}

impl PcmTapState {
    fn from_u8(value: u8) -> Self {
        match value {
            STATE_PAUSED => Self::Paused,
            STATE_STOPPED => Self::Stopped,
            _ => Self::Playing,
        }
    }
}

/// The window a reader asks for, relative to the rendered-frame cursor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PcmTapWindow {
    /// Frames read *before* the cursor: the most recently rendered audio.
    pub back_frames: u32,
    /// Frames read from the cursor onward. This is the reference engine's
    /// `sampleAhead` offset: `0` starts the window exactly at the playback
    /// position.
    pub ahead_frames: u32,
}

impl PcmTapWindow {
    /// A window of `frames` frames ending at the cursor (the recent past).
    pub const fn recent(frames: u32) -> Self {
        Self {
            back_frames: frames,
            ahead_frames: 0,
        }
    }

    /// A window of `frames` frames starting at the cursor.
    pub const fn ahead(frames: u32) -> Self {
        Self {
            back_frames: 0,
            ahead_frames: frames,
        }
    }

    /// The total number of frames the window covers, clamped to
    /// [`MAX_READ_FRAMES`].
    pub const fn frames(&self) -> u32 {
        let total = self.back_frames as u64 + self.ahead_frames as u64;
        if total > MAX_READ_FRAMES as u64 {
            MAX_READ_FRAMES
        } else {
            total as u32
        }
    }
}

/// One read of a playing instance's PCM tap.
#[derive(Clone, Debug, PartialEq)]
pub struct PcmTapSnapshot {
    /// Format of the tapped samples.
    pub spec: PcmAudioSpec,
    pub state: PcmTapState,
    /// Rendered-frame cursor of the instance.
    pub cursor: u64,
    /// Source (stream) frame the cursor points at, when it is known.
    pub source_frame: Option<u64>,
    /// Rendered-frame coordinate of `frames[0]`. The requested window starts at
    /// `cursor - back_frames`, but a window that reaches before the start of
    /// playback is clamped, and `first_frame` reports where it really starts.
    pub first_frame: u64,
    /// Interleaved samples (`spec.channels` values per frame) for the requested
    /// window. Frames without decoded audio are zero.
    pub frames: Vec<f32>,
    /// How many frames of `frames` carry decoded audio. `0` means the whole
    /// window is silence: nothing decoded yet, overwritten, paused before the
    /// first frame, or stopped.
    pub available_frames: u32,
    /// This instance's ring capacity in frames.
    pub capacity_frames: u32,
}

/// Registry of PCM taps, keyed by [`AudioInstanceId`].
///
/// `AudioSystem::pcm_tap()` hands out clones of this handle; the audio worker
/// registers and retires instances while the engine reads them.
#[derive(Clone)]
pub struct PcmTap {
    inner: Arc<TapShared>,
}

struct TapShared {
    capacity_frames: u32,
    instances: Mutex<BTreeMap<AudioInstanceId, Arc<TapInstance>>>,
}

/// Write side of one tap instance.
///
/// Cloning is cheap (`Arc`); the control plane keeps one clone per playing
/// slot and the sample producer gets another.
#[derive(Clone)]
pub struct PcmTapFeed {
    instance: Arc<TapInstance>,
}

struct TapInstance {
    id: AudioInstanceId,
    spec: PcmAudioSpec,
    capacity_frames: u32,
    /// Whether the cursor is the feed's own coordinate ([`TapMode::Ring`]) or an
    /// unrolled buffer clock ([`TapMode::Decoded`]).
    mode: AtomicU8,
    state: AtomicU8,
    cursor: AtomicU64,
    source_frame: AtomicU64,
    total_frames: AtomicU64,
    looping: AtomicBool,
    dropped_pushes: AtomicU64,
    buffers: Mutex<TapBuffers>,
}

/// How a feed's frames are addressed.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TapMode {
    /// The feed publishes absolute coordinates ([`PcmTapFeed::push_at`]): the
    /// cursor is the render position in that same space.
    Ring = 0,
    /// The feed attached a decoded buffer ([`PcmTapFeed::attach_decoded`]): the
    /// cursor unrolls loops and the buffer is addressed through the source.
    Decoded = 1,
}

impl TapMode {
    fn from_u8(value: u8) -> Self {
        if value == Self::Decoded as u8 {
            Self::Decoded
        } else {
            Self::Ring
        }
    }
}

#[derive(Default)]
struct TapBuffers {
    /// Ring of published frames, addressed by the feed's coordinate:
    /// coordinate `c` lives at slot `c % capacity_frames`. `None` until the
    /// first read arms the tap.
    ring: Option<Vec<f32>>,
    /// Coordinate just past the last published frame.
    write_head: u64,
    /// Lowest coordinate whose ring contents are still valid. A publish that
    /// does not continue `write_head` leaves a hole: everything older is
    /// dropped from the readable range so the hole can never be read as audio.
    valid_from: u64,
    /// Full decoded buffer of a static sound, shared with kira's sound data.
    decoded: Option<Arc<[Frame]>>,
}

impl Default for PcmTap {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY_FRAMES)
    }
}

impl PcmTap {
    /// Creates a registry whose instances keep at most `capacity_frames`
    /// frames.
    pub fn new(capacity_frames: u32) -> Self {
        Self {
            inner: Arc::new(TapShared {
                capacity_frames: capacity_frames.max(1),
                instances: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    /// Ring capacity used for new instances, in frames.
    pub fn capacity_frames(&self) -> u32 {
        self.inner.capacity_frames
    }

    /// Registers (or replaces) the tap of `id`. The returned feed is the write
    /// side of the fresh instance; any previous instance for the id is dropped,
    /// so readers see the new sound rather than stale samples of the old one.
    pub fn register(&self, id: AudioInstanceId, spec: PcmAudioSpec) -> PcmTapFeed {
        let instance = Arc::new(TapInstance {
            id,
            spec,
            capacity_frames: self.inner.capacity_frames,
            mode: AtomicU8::new(TapMode::Ring as u8),
            state: AtomicU8::new(STATE_PLAYING),
            cursor: AtomicU64::new(0),
            source_frame: AtomicU64::new(SOURCE_UNKNOWN),
            total_frames: AtomicU64::new(0),
            looping: AtomicBool::new(false),
            dropped_pushes: AtomicU64::new(0),
            buffers: Mutex::new(TapBuffers::default()),
        });
        let mut instances = self.lock_instances();
        instances.insert(id, Arc::clone(&instance));
        PcmTapFeed { instance }
    }

    /// Retires the tap of `id`; readers get `None` instead of a snapshot until
    /// the id is registered again.
    pub fn remove(&self, id: AudioInstanceId) -> bool {
        self.lock_instances().remove(&id).is_some()
    }

    /// Whether `id` currently has a tap instance.
    pub fn contains(&self, id: AudioInstanceId) -> bool {
        self.lock_instances().contains_key(&id)
    }

    /// The write side of `id`, when it is registered.
    pub fn feed(&self, id: AudioInstanceId) -> Option<PcmTapFeed> {
        self.lock_instances().get(&id).map(|instance| PcmTapFeed {
            instance: Arc::clone(instance),
        })
    }

    /// Registered instance ids, in ascending order.
    pub fn instance_ids(&self) -> Vec<AudioInstanceId> {
        self.lock_instances().keys().copied().collect()
    }

    /// Reads a window relative to the instance's rendered-frame cursor.
    pub fn read(&self, id: AudioInstanceId, window: PcmTapWindow) -> Option<PcmTapSnapshot> {
        let instance = self.lock_instances().get(&id).cloned()?;
        Some(instance.read(window))
    }

    /// Reads the `frames` frames that were rendered most recently (ending at
    /// the cursor).
    pub fn read_recent(&self, id: AudioInstanceId, frames: u32) -> Option<PcmTapSnapshot> {
        self.read(id, PcmTapWindow::recent(frames))
    }

    /// Playback state of `id`, when it is registered.
    pub fn state(&self, id: AudioInstanceId) -> Option<PcmTapState> {
        self.lock_instances().get(&id).map(|i| i.state())
    }

    /// Rendered-frame cursor of `id`, when it is registered.
    pub fn cursor(&self, id: AudioInstanceId) -> Option<u64> {
        self.lock_instances()
            .get(&id)
            .map(|i| i.cursor.load(Ordering::Relaxed))
    }

    /// Format of `id`'s samples, when it is registered.
    pub fn spec(&self, id: AudioInstanceId) -> Option<PcmAudioSpec> {
        self.lock_instances().get(&id).map(|i| i.spec)
    }

    fn lock_instances(
        &self,
    ) -> std::sync::MutexGuard<'_, BTreeMap<AudioInstanceId, Arc<TapInstance>>> {
        self.inner
            .instances
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl PcmTapFeed {
    /// Id this feed belongs to.
    pub fn id(&self) -> AudioInstanceId {
        self.instance.id
    }

    /// Format of the tapped samples.
    pub fn spec(&self) -> PcmAudioSpec {
        self.instance.spec
    }

    /// Ring capacity of this instance in frames.
    pub fn capacity_frames(&self) -> u32 {
        self.instance.capacity_frames
    }

    /// Current playback state.
    pub fn state(&self) -> PcmTapState {
        self.instance.state()
    }

    /// Publishes decoded frames (interleaved, matching [`PcmTapFeed::spec`]) at
    /// the coordinate `first_frame`.
    ///
    /// `first_frame` is the producer's own coordinate for the first frame of
    /// `samples`: the stream frame the sound renders it at. Coordinate `c` then
    /// holds the audio rendered at `c`, which is the same space
    /// [`PcmTapFeed::set_source_position`] reports, so a read at the cursor
    /// addresses exactly what is playing no matter how far the producer has
    /// decoded ahead. A producer that also emits silence for a stalled source
    /// must publish it (or skip its coordinates), otherwise the coordinates that
    /// follow describe audio the sound never played.
    ///
    /// Coordinates from different publishes must be contiguous; a gap (a dropped
    /// publish, a stalled producer that skipped coordinates) makes everything
    /// older than the new range unreadable, so the hole can never be served as
    /// audio. Until the first read arms the instance the publish is inert (no
    /// allocation, no copy).
    pub fn push_at(&self, first_frame: u64, samples: &[f32]) {
        self.instance.publish_at(first_frame, samples);
    }

    /// Attaches the full decoded buffer of a static sound (shared with kira,
    /// never copied) so that any window inside it is readable, including the
    /// recent past of a loop wrap and the frames ahead of the cursor.
    pub fn attach_decoded(&self, frames: Arc<[Frame]>, looping: bool) {
        self.instance.attach_decoded(frames, looping);
    }

    /// Reports the render position, e.g. from `SoundHandle::position()`.
    ///
    /// For a feed that publishes coordinates ([`PcmTapFeed::push_at`]) this is
    /// the stream frame being rendered, and the cursor becomes exactly that
    /// frame: reads at the cursor address the audio playing right now. For a
    /// feed with an attached decoded buffer ([`PcmTapFeed::attach_decoded`]) the
    /// reported frame is the source position: a loop wrap keeps the cursor
    /// counting, a forward seek moves it forward and a backward seek moves it
    /// back. Paused or stopped instances do not move at all.
    pub fn set_source_position(&self, frame: u64) {
        self.instance.set_source_position(frame);
    }

    /// Advances the rendered-frame cursor by `frames` frames (used by feeds
    /// that know the render clock exactly, and by tests).
    pub fn advance_rendered(&self, frames: u64) {
        self.instance.advance_frames(frames);
    }

    /// Positions the stream at `frame` (a seek): the cursor moves with the
    /// stream so reads stay aligned with the audio being played.
    pub fn notify_seek(&self, frame: u64) {
        self.instance.seek_to_source(frame);
    }

    /// Freezes (`true`) or resumes (`false`) the cursor.
    pub fn set_paused(&self, paused: bool) {
        self.instance.set_paused(paused);
    }

    /// Marks the sound stopped: the readable window is zeroed.
    pub fn stop(&self) {
        self.instance.stop();
    }

    /// Pushes dropped because a reader held the buffer lock. Non-zero means the
    /// tap briefly could not publish decoded audio; reads over that range are
    /// silence rather than stale samples.
    pub fn dropped_pushes(&self) -> u64 {
        self.instance.dropped_pushes.load(Ordering::Relaxed)
    }
}

impl std::fmt::Debug for PcmTapFeed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PcmTapFeed")
            .field("id", &self.instance.id)
            .field("spec", &self.instance.spec)
            .field("state", &self.instance.state())
            .finish_non_exhaustive()
    }
}

impl TapInstance {
    fn channels(&self) -> usize {
        self.spec.channels.max(1) as usize
    }

    fn mode(&self) -> TapMode {
        TapMode::from_u8(self.mode.load(Ordering::Relaxed))
    }

    fn state(&self) -> PcmTapState {
        PcmTapState::from_u8(self.state.load(Ordering::Relaxed))
    }

    fn source_position(&self) -> Option<u64> {
        let value = self.source_frame.load(Ordering::Relaxed);
        (value != SOURCE_UNKNOWN).then_some(value)
    }

    /// Total frames of a looping stream, when the loop length is known.
    fn loop_total(&self) -> Option<u64> {
        let total = self.total_frames.load(Ordering::Relaxed);
        (self.looping.load(Ordering::Relaxed) && total > 0).then_some(total)
    }

    fn advance_frames(&self, frames: u64) {
        if frames == 0 || self.state() != PcmTapState::Playing {
            return;
        }
        let previous = self.cursor.load(Ordering::Relaxed);
        let cursor = previous.saturating_add(frames);
        self.cursor.store(cursor, Ordering::Relaxed);
        match (self.mode(), self.source_position(), self.loop_total()) {
            // A published-coordinate feed shares the render space with the
            // cursor, so the source follows it one to one.
            (TapMode::Ring, Some(_), _) => self.source_frame.store(cursor, Ordering::Relaxed),
            (TapMode::Decoded, Some(source), Some(total)) => self
                .source_frame
                .store(source.saturating_add(frames) % total, Ordering::Relaxed),
            (TapMode::Decoded, Some(source), None) => self
                .source_frame
                .store(source.saturating_add(frames), Ordering::Relaxed),
            (_, None, _) => {}
        }
    }

    fn set_source_position(&self, frame: u64) {
        match self.state() {
            PcmTapState::Stopped => return,
            PcmTapState::Paused => {
                // Frozen cursor: keep the reported position fresh so resuming
                // does not replay the frames rendered while paused.
                self.source_frame.store(frame, Ordering::Relaxed);
                return;
            }
            PcmTapState::Playing => {}
        }
        if self.mode() == TapMode::Ring {
            // The feed publishes absolute stream coordinates, so the render
            // position *is* the cursor: reads then address the audio being
            // played no matter how far the producer is decoding ahead.
            self.cursor.store(frame, Ordering::Relaxed);
            self.source_frame.store(frame, Ordering::Relaxed);
            return;
        }
        let Some(previous) = self.source_position() else {
            self.source_frame.store(frame, Ordering::Relaxed);
            return;
        };
        if frame >= previous {
            self.advance_frames(frame - previous);
        } else if let Some(total) = self.loop_total() {
            // Loop wrap: the stream restarted, the rendered-frame clock did not.
            let advance = total.saturating_sub(previous).saturating_add(frame % total);
            self.advance_frames(advance);
        } else {
            // Backward jump without a known loop: the stream moved back.
            self.seek_to_source(frame);
            return;
        }
        self.source_frame.store(frame, Ordering::Relaxed);
    }

    fn seek_to_source(&self, frame: u64) {
        if self.state() == PcmTapState::Stopped {
            return;
        }
        if self.mode() == TapMode::Ring {
            // The stream moved, so the cursor moves with it.
            self.cursor.store(frame, Ordering::Relaxed);
            self.source_frame.store(frame, Ordering::Relaxed);
            return;
        }
        if let Some(previous) = self.source_position() {
            let delta = frame as i64 - previous as i64;
            let cursor = self.cursor.load(Ordering::Relaxed) as i64;
            let moved = cursor.saturating_add(delta).max(0) as u64;
            self.cursor.store(moved, Ordering::Relaxed);
        }
        self.source_frame.store(frame, Ordering::Relaxed);
    }

    fn set_paused(&self, paused: bool) {
        if self.state() == PcmTapState::Stopped {
            return;
        }
        self.state.store(
            if paused { STATE_PAUSED } else { STATE_PLAYING },
            Ordering::Relaxed,
        );
    }

    fn stop(&self) {
        self.state.store(STATE_STOPPED, Ordering::Relaxed);
        let mut buffers = self.lock_buffers();
        if let Some(ring) = buffers.ring.as_mut() {
            ring.fill(0.0);
        }
        buffers.decoded = None;
        buffers.write_head = 0;
        buffers.valid_from = 0;
    }

    fn publish_at(&self, first_frame: u64, samples: &[f32]) {
        if samples.is_empty() || self.state() == PcmTapState::Stopped {
            return;
        }
        let channels = self.channels();
        let frames = (samples.len() / channels) as u64;
        if frames == 0 {
            return;
        }
        let Ok(mut buffers) = self.buffers.try_lock() else {
            // A reader is copying a window. Never block the producer: drop this
            // chunk. Its coordinates stay unpublished, so reads over them are
            // silence, and the next publish — which carries its own coordinate —
            // closes the gap.
            self.dropped_pushes.fetch_add(1, Ordering::Relaxed);
            return;
        };
        if buffers.ring.is_none() {
            // Nobody has read this instance yet: the tap is inert.
            return;
        }
        let capacity = self.capacity_frames as u64;
        // A chunk larger than the ring keeps only its tail, but its head still
        // occupies coordinates and must not be served as if it were audio; the
        // capacity bound below drops those coordinates, exactly like a wrapped
        // ring.
        let chunk_skip = frames.saturating_sub(capacity);
        let end = first_frame.saturating_add(frames);
        if first_frame != buffers.write_head {
            // The publish does not continue the previous range: everything
            // older belongs to a hole that must never read as audio. A
            // contiguous publish must keep the older range readable.
            buffers.valid_from = first_frame;
        }
        let mut head = first_frame.saturating_add(chunk_skip);
        {
            let ring = buffers.ring.as_mut().expect("ring is allocated");
            for frame in chunk_skip as usize..frames as usize {
                let slot = (head % capacity) as usize * channels;
                ring[slot..slot + channels]
                    .copy_from_slice(&samples[frame * channels..(frame + 1) * channels]);
                head = head.saturating_add(1);
            }
        }
        debug_assert_eq!(head, end);
        buffers.write_head = buffers.write_head.max(end);
        buffers.valid_from = buffers
            .valid_from
            .max(buffers.write_head.saturating_sub(capacity));
    }

    fn attach_decoded(&self, frames: Arc<[Frame]>, looping: bool) {
        if self.state() == PcmTapState::Stopped {
            return;
        }
        let mut buffers = self.lock_buffers();
        self.total_frames
            .store(frames.len() as u64, Ordering::Relaxed);
        self.looping.store(looping, Ordering::Relaxed);
        buffers.decoded = Some(frames);
        self.mode.store(TapMode::Decoded as u8, Ordering::Relaxed);
        if self.source_position().is_none() {
            self.source_frame.store(0, Ordering::Relaxed);
        }
    }

    fn read(&self, window: PcmTapWindow) -> PcmTapSnapshot {
        let channels = self.channels();
        let mut buffers = self.lock_buffers();
        if buffers.ring.is_none()
            && buffers.decoded.is_none()
            && self.state() != PcmTapState::Stopped
        {
            // The first read arms a push-fed instance: allocate the ring here,
            // so an unread tap stays allocation- and copy-free. Coordinates are
            // the feed's own and survive arming, so the cursor is untouched:
            // audio the producer emitted before this point was never captured
            // and reads as silence.
            buffers.ring = Some(vec![0.0; self.capacity_frames as usize * channels]);
        }

        let state = self.state();
        let cursor = self.cursor.load(Ordering::Relaxed);
        let source = self.source_position();
        let back = window.back_frames.min(MAX_READ_FRAMES) as u64;
        let ahead = window.ahead_frames.min(MAX_READ_FRAMES) as u64;
        let first_frame = cursor.saturating_sub(back);
        let count = (cursor - first_frame + ahead).min(MAX_READ_FRAMES as u64) as usize;

        let mut frames = vec![0.0_f32; count * channels];
        let available = if state == PcmTapState::Stopped {
            0
        } else if let Some(decoded) = &buffers.decoded {
            self.fill_from_decoded(decoded, cursor, source, first_frame, count, &mut frames)
        } else if let Some(ring) = &buffers.ring {
            TapBuffers::fill_from_ring(
                ring,
                &buffers,
                self.capacity_frames,
                channels,
                first_frame,
                count,
                &mut frames,
            )
        } else {
            0
        };

        PcmTapSnapshot {
            spec: self.spec,
            state,
            cursor,
            source_frame: source,
            first_frame,
            frames,
            available_frames: available as u32,
            capacity_frames: self.capacity_frames,
        }
    }

    fn fill_from_decoded(
        &self,
        decoded: &[Frame],
        cursor: u64,
        source: Option<u64>,
        first_frame: u64,
        count: usize,
        out: &mut [f32],
    ) -> usize {
        let Some(anchor) = source else {
            return 0;
        };
        let channels = self.channels();
        let total = self.total_frames.load(Ordering::Relaxed);
        let looping = self.looping.load(Ordering::Relaxed);
        let mut available = 0;
        for index in 0..count {
            let coordinate = first_frame + index as u64;
            let offset = coordinate as i64 - cursor as i64;
            let source = anchor as i64 + offset;
            let source = if looping && total > 0 {
                Some(source.rem_euclid(total as i64) as u64)
            } else {
                (source >= 0).then_some(source as u64)
            };
            let Some(frame) = source.and_then(|source| decoded.get(source as usize)) else {
                continue;
            };
            let slot = index * channels;
            match channels {
                1 => out[slot] = (frame.left + frame.right) * 0.5,
                _ => {
                    out[slot] = frame.left;
                    out[slot + 1] = frame.right;
                }
            }
            available += 1;
        }
        available
    }

    fn lock_buffers(&self) -> std::sync::MutexGuard<'_, TapBuffers> {
        self.buffers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[cfg(test)]
    fn ring_allocated(&self) -> bool {
        self.lock_buffers().ring.is_some()
    }
}

impl TapBuffers {
    fn fill_from_ring(
        ring: &[f32],
        buffers: &TapBuffers,
        capacity_frames: u32,
        channels: usize,
        first_frame: u64,
        count: usize,
        out: &mut [f32],
    ) -> usize {
        let capacity = capacity_frames as u64;
        let mut available = 0;
        for index in 0..count {
            let coordinate = first_frame + index as u64;
            if coordinate < buffers.valid_from || coordinate >= buffers.write_head {
                continue;
            }
            let slot = (coordinate % capacity) as usize * channels;
            out[index * channels..(index + 1) * channels]
                .copy_from_slice(&ring[slot..slot + channels]);
            available += 1;
        }
        available
    }
}

#[cfg(test)]
#[path = "pcm_tap_tests.rs"]
mod tests;
