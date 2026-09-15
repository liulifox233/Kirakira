//! `wfBasicEffect.dll` — the `GraphicEqualizer`, `StkFreeVerb` and
//! `DelayEffect` WaveSoundBuffer filters.
//!
//! The DLL ships without source, so every claim below is read out of the
//! shipped image (`/home/ruri/games/PARQUET/PARQUET/plugin/wfBasicEffect.dll`,
//! PE32 i386, 226 816 B; the dossier's `/Users/ruri/...` path is the macOS
//! checkout of the same file). The addresses in the comments are that image's
//! virtual addresses, recovered with `objdump -d -M intel` (Ghidra and Java
//! are not installed on this machine, so the dossier's headless recipe cannot
//! run; the PE's RTTI was walked with a small scratch script instead). The
//! dossier is `docs/plugins/wfBasicEffect.md`.
//!
//! # What the DLL registers
//!
//! Three `SimpleBinder` classes (`krkr2 …/plugins/win32/00_simplebinder`), each
//! anchored on `WaveSoundBuffer`: `StkFreeVerb`, `GraphicEqualizer` and
//! `DelayEffect` (the C++ class is `WaveDelay`, the TJS class name is
//! `DelayEffect` — wide strings at `0x10029a24`, `0x100299c0`, `0x1002999c`),
//! each with an `interface` property. The anchor matters: the binder's
//! `BindUtil(base, link).Class(name, …)` resolves `base` through
//! `StoreUtil::GetObject` and then stores the created class object *into* that
//! object (`simplebinder.hpp`'s `ClassStore::Link` ends in
//! `obj->PropSet(flag, key, …, obj)`), and all three chains construct the
//! `WaveSoundBuffer` wide string `0x10029a04` (`push`es at `0x1000962c`,
//! `0x100099d5` and `0x10009b8a`, immediately after each class name). The
//! classes are therefore reachable as `WaveSoundBuffer.StkFreeVerb`,
//! `WaveSoundBuffer.GraphicEqualizer` and `WaveSoundBuffer.DelayEffect` and as
//! nothing else — the DLL creates no global of these names. That is the shape
//! the games use: PARQUET's `data.xp3>sysscn/voiceeffect.tjs` objects 48-50
//! load `WaveSoundBuffer.<name>` and `new` it. `SimpleWaveFilter<…>`
//! RTTI names (`0x10034004`, `0x1003408c`, `0x10034170`) show the shared audio
//! adapter, and `stk::FreeVerb` / `stk::Delay` / `stk::OnePole` / `stk::Effect`
//! RTTI names show the Synthesis ToolKit classes underneath.
//!
//! Recovered member sets (binder chain `0x10009390`, member names pushed in
//! chain order, wide strings `0x1002999c`-`0x10029aa0`):
//!
//! | class | members |
//! |---|---|
//! | `StkFreeVerb` | `extend`, `mode`, `width`, `damping`, `roomSize`, `effectMix`, `interface` |
//! | `GraphicEqualizer` | `setGain`, `getGain`, `interface` |
//! | `DelayEffect` | `init`, `interface` |
//!
//! `GraphicEqualizer.setGain(band, gain)` / `getGain(band)` are the two
//! `FunctionCallback` instantiations in the RTTI (one-argument and
//! two-argument); `DelayEffect.init` is the variadic `WaveDelay` callback
//! (`…AEHPAVtTJSVariant@@HPAPAV2@@Z`). Every other member is a property
//! (`GetterCallback`/`SetterCallback` RTTI); `StkFreeVerb` is the only class
//! with a non-const getter (`…AEH…`), which is why its first member is
//! modelled as a read-only property rather than a function.
//!
//! # Recovered parameters
//!
//! * **GraphicEqualizer** — ten bands with centre frequencies 31.5, 63, 125,
//!   250, 500, 1 000, 2 000, 4 000, 8 000 and 16 000 Hz (`.rdata` float table
//!   `0x10029278`-`0x1002929c`, pointed at by `0x100331cc` and loaded by the
//!   constructor at `0x10007ac8`). The per-band gains are ten floats at
//!   `+0xc4`-`+0xe8`, all initialised to `1.0` (`fld1; fst …`, `0x10007ad3`).
//!   `setGain` (`0x10002250`) converts the second argument through the host
//!   `AsReal`, the first through `AsInteger`, and stores into
//!   `gain[band]` **only when `band <= 9`** (`cmp eax,0x9; ja` — an
//!   out-of-range band is silently ignored). `getGain` (`0x100022f0`) returns
//!   the stored float, and **`0.0` for any band outside 0-9** (`fldz`).
//!   The gains are linear multipliers, not decibels (the default is `1.0`).
//! * **StkFreeVerb** — `Effect::setEffectMix` (`0x1000b6a0`) clamps the mix to
//!   `[0, 1]` with the two recovered warnings (`0x1002a1b8`, `0x1002a208`);
//!   the FreeVerb constructor (`0x1000b710`) stores the default comb feedback
//!   `0.91` (`0x1000b84f`) and damp coefficient `0.1` (`0x1000b867`) directly,
//!   i.e. the scaled forms of `roomSize 0.75` and `damping 0.25`
//!   (`0.28 * r + 0.7 = 0.91`, `0.4 * d = 0.1`; the scale constants are the
//!   FreeVerb `scaleroom 0.28` / `offsetroom 0.7` / `scaledamp 0.4` at
//!   `0x1002a110`-`0x1002a118`), sets `width` to `1.0` (`fld1`, `0x1000b876`),
//!   `effectMix` to `0.75` (`0x10029330`, passed to `setEffectMix` at
//!   `0x1000b857`) and `fixedGain` to `0.015` (`0x1002a0f0`, `0x1000ae30`).
//!   The comb tunings `{1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617}`
//!   (`.data` `0x1003426c`) and allpass tunings `{556, 441, 341, 225}`
//!   (`0x1003428c`) are scaled by `Stk::sampleRate() / 44100` (`0x1000b892`,
//!   the two `44100.0` doubles at `0x10034350` / `0x1002a258`).
//! * **DelayEffect** — the constructor (`0x10007780`) stores the double `0.75`
//!   (`0x10029330`) as two floats at `+0xc0`/`+0xc4` (`0.0` and `1.8125`) and
//!   the integer `1000` at `+0xc8` (`0x100077ff`). `WaveDelay::init`
//!   (`0x10002360`) requires at least three arguments (`cmp ebp,0x2; jle`):
//!   `p0` (integer) lands at `+0xcc`, `p1` (real) at `+0xd4`, `p2` (real) at
//!   `+0xd8`, and the optional `p3` (integer) at `+0xc0`. The STK classes the
//!   DLL links fix the error semantics the port keeps: `stk::Delay::setDelay`
//!   refuses a delay past the maximum (`Delay::setDelay: argument (… greater
//!   than maximum!`, `0x1002a47c`) and `stk::OnePole::setPole` /
//!   `setCoefficients` refuse `|pole| >= 1` (`OnePole::setPole: argument (…`,
//!   `0x1002a50c`, `0x1002a548`).
//!
//! # The sample format
//!
//! The DLL's float adapter (`SimpleFloatSource`, `0x10004e50`) is the piece
//! that sees the wave format: it copies ten dwords of `tTVPWaveFormat` into the
//! filter object (`rep movs`, `0x10004e8c`), rejects a format wider than 32
//! bits or with more than four channels (`HiRes format not supported.`,
//! `0x1002975c`, at `0x10004e98`), and publishes a 32-bit float output format
//! of its own (`[+0x50] = 0x20`, `[+0x54] = 4`, `[+0x6c] = 1` at
//! `0x10004ed5`). Connecting a second upstream source throws
//! `Cannot connect multiple wave sound buffer at once.` (`0x10029798`,
//! `0x10004e75`). The filter therefore processes **interleaved 32-bit float
//! PCM**, one or two channels — the same representation this engine feeds its
//! audio backend (`krkr_core::PcmStreamSource`, `kira::Frame`), which is why
//! the DSP below runs on `&mut [f32]` plus a channel count.
//!
//! # What this module implements
//!
//! **Real**: the three classes and their members, and the three filters as
//! real DSP over interleaved `f32` PCM — a ten-band peaking-EQ bank, Jezar's
//! FreeVerb (the algorithm the DLL's tunings, scales and fixed gain belong to)
//! and a feedback delay line with a one-pole damping stage. The numeric tests
//! at the bottom drive impulse and step responses with expectations derived
//! from these coefficients by hand.
//!
//! **Inferred, not recovered** (documented where the code uses it): the
//! `StkFreeVerb` raw defaults `roomSize 0.75` / `damping 0.25` are derived
//! from the stored scaled constants rather than read as raw stores; the
//! dry/wet crossfade follows the classic FreeVerb/STK `Effect` shape
//! (`dry * (1 - mix) + wet * mix`) where the DLL's own `update()` mixes them
//! with a formula that could not be reconstructed with objdump; the
//! `GraphicEqualizer` band filters are RBJ peaking biquads at one-octave `Q`
//! (the DLL builds per-band coefficient vectors through a templated filter
//! class, `0x100089d0`/`0x10008640`, whose coefficient math was not recovered);
//! `DelayEffect.init`'s four parameters are recovered by slot and type but
//! their names here (delay milliseconds, damping pole, feedback, maximum
//! delay) are inferred from the STK classes the DLL links; and `extend`'s
//! return value was not recovered (this port returns the same sentinel as
//! `interface`).
//!
//! **How a filter reaches the audio path.**  The engine's `WaveSoundBuffer`
//! owns a per-instance `filters` array (read-only member — the reference's
//! `TJSCreateArrayObject` at `sound/WaveIntf.cpp:815` with a denied setter at
//! `:1552`); at `open` the engine reads each element's `interface` and
//! publishes the values in array order (`AudioCommand::SetFilters`), and the
//! audio backend resolves each value back to the DSP object
//! (`krkr_audio::register_wave_filter`, which the three constructors call) and
//! drives it from inside the sample path — `recreate` at connect time,
//! `reset` when playback starts, `process` per decoded unit
//! (`iTVPBasicWaveFilter`, `sound/WaveIntf.h:130-137`).  The
//! `interface` property therefore answers a **per-instance registration id**
//! instead of the reference's raw `iTVPBasicWaveFilter*`: the value is stable
//! for an instance and meaningless outside the registration, and a stale one
//! (a filter the script `finalize`d) stops resolving instead of being cast.
//!
//! The three DSP types stay directly callable (`process`/`reset`), which is
//! what the numeric tests below drive; [`TapPcmSource`] in this module is the
//! other half of the same boundary — the decoded-PCM readback the engine's
//! `getVisBuffer` and the sample-reading plugins use.

// The DSP types and their `process`/`reset` entry points are the module's
// processing contract: the engine's chain calls them through `SharedFilter`
// (see the module docs) and the numeric tests drive them directly, so the
// ported coefficients and their API stay together. The unused-item lint is
// silenced for the module because parts of that surface are exercised only by
// tests. `result_large_err` is the crate-wide `TjsError` size lint every native
// callback carries.
#![allow(dead_code)]
#![allow(clippy::result_large_err)]

use std::{
    cell::RefCell,
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use krkr_engine::{KrkrHost, KrkrPlugin, plugin_api};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativePropertyAccess, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "GraphicEqualizer / StkFreeVerb / DelayEffect filters on WaveSoundBuffer",
    notes: "Real DSP (10-band peaking EQ, FreeVerb, damped feedback delay) with the recovered \
            member surface and parameter ranges, processing interleaved f32 PCM; the classes \
            install on the `WaveSoundBuffer` class object like the reference \
            (`WaveSoundBuffer.StkFreeVerb` / `.GraphicEqualizer` / `.DelayEffect`, the binder's \
            base — PARQUET's voiceeffect.tjs reads them there), the constructors consume the ini \
            arguments the DLL's class entries read, and each live filter registers its DSP object \
            with the audio backend so the engine's `WaveSoundBuffer.filters` chain drives it \
            while the buffer plays. `interface` answers that registration id (the port's stand-in \
            for the raw `iTVPBasicWaveFilter*`). See the module docs for the re-derived constants \
            and the parts that are inferred.",
    install: |engine| engine.register_plugin(WfBasicEffectPlugin),
};

pub struct WfBasicEffectPlugin;

impl KrkrPlugin for WfBasicEffectPlugin {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_wf_basic_effect(runtime);
        // The engine reads `WaveSoundBuffer.filters` and publishes the ids these
        // classes hand out through `interface`; the chain the audio backend
        // builds from them calls back into these very objects.
        install_engine_audio_bridge();
        runtime.host_mut().log(
            "wfBasicEffect.dll registered: GraphicEqualizer / StkFreeVerb / DelayEffect \
             (DSP driven by WaveSoundBuffer.filters)",
        );
        Ok(())
    }
}

/// The engine's [`plugin_api::audio::WavePcmSource`] over the audio backend's
/// decoded-PCM tap.
///
/// The engine cannot name `krkr-audio` (or the `AudioSystem` its shell owns),
/// so a plugin that can installs this adapter: `krkr_audio::active_pcm_tap()`
/// is the live tap, and one read here is one reference `GetVisBuffer`: the
/// samples of `buffer` starting `aheadsamples` frames past the play position,
/// `numsamples` long (`sound/win32/WaveImpl.cpp:3274-3330`).
struct TapPcmSource;

impl plugin_api::audio::WavePcmSource for TapPcmSource {
    fn read_window(
        &self,
        id: plugin_api::audio::AudioInstanceId,
        request: plugin_api::audio::WavePcmRequest,
    ) -> Option<plugin_api::audio::WavePcmWindow> {
        let tap = krkr_audio::active_pcm_tap()?;
        let channels = tap.spec(id)?.channels.max(1) as usize;
        let snapshot = tap.read(
            id,
            krkr_audio::PcmTapWindow {
                back_frames: 0,
                ahead_frames: request.ahead_frames.saturating_add(request.frames),
            },
        )?;
        // The reference starts its copy `aheadsamples` frames past the play
        // position; the tap window starts at it, so the leading frames are the
        // requested lead.
        let skip = (request.ahead_frames as usize * channels).min(snapshot.frames.len());
        Some(plugin_api::audio::WavePcmWindow {
            spec: snapshot.spec,
            state: match snapshot.state {
                krkr_audio::PcmTapState::Playing => plugin_api::audio::WavePcmState::Playing,
                krkr_audio::PcmTapState::Paused => plugin_api::audio::WavePcmState::Paused,
                krkr_audio::PcmTapState::Stopped => plugin_api::audio::WavePcmState::Stopped,
            },
            samples: snapshot.frames[skip..].to_vec(),
            available_frames: snapshot
                .available_frames
                .saturating_sub(request.ahead_frames),
        })
    }
}

/// Serialises the tests that install the process-wide PCM source
/// (`plugin_api::audio`'s slot): the plugins' tests run in one process, and a
/// source another test installed would otherwise answer their reads.
#[cfg(test)]
pub(crate) fn lock_pcm_source() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Installs the tap-backed PCM source into the engine's plugin API, replacing
/// an equivalent one.
///
/// Every plugin that reads samples (`getSample.dll`, `fftgraph.dll`) installs
/// it, so whichever registers first wins with the same adapter and a later
/// registration is a no-op (`install_wave_pcm_source`); with no audio system
/// alive the source answers nothing and the engine's `getVisBuffer` keeps the
/// reference's not-playing `0`.
pub(crate) fn install_engine_audio_bridge() {
    plugin_api::audio::install_wave_pcm_source(Arc::new(TapPcmSource));
}

/// The canonical DLL name: what `Plugins.link` matches.
const PLUGIN_NAME: &str = "wfBasicEffect.dll";

// ---------------------------------------------------------------------------
// Per-instance state
// ---------------------------------------------------------------------------

/// One live filter instance. The TJS object owns the parameters; this is the
/// processing state the engine's chain drives.
enum EffectState {
    Equalizer(Box<GraphicEqualizer>),
    FreeVerb(FreeVerb),
    Delay(DelayEffect),
}

impl EffectState {
    fn process(&mut self, frames: &mut [f32], channels: u32) {
        match self {
            EffectState::Equalizer(equalizer) => equalizer.process(frames, channels as usize),
            EffectState::FreeVerb(reverb) => reverb.process(frames, channels as usize),
            EffectState::Delay(delay) => delay.process(frames, channels as usize),
        }
    }

    fn reset(&mut self) {
        match self {
            EffectState::Equalizer(equalizer) => equalizer.reset(),
            EffectState::FreeVerb(reverb) => reverb.reset(),
            EffectState::Delay(delay) => delay.reset(),
        }
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        match self {
            EffectState::Equalizer(equalizer) => equalizer.set_sample_rate(sample_rate),
            EffectState::FreeVerb(reverb) => reverb.set_sample_rate(sample_rate),
            EffectState::Delay(delay) => delay.set_sample_rate(sample_rate),
        }
    }

    fn sample_rate(&self) -> f32 {
        match self {
            EffectState::Equalizer(equalizer) => equalizer.sample_rate(),
            EffectState::FreeVerb(reverb) => reverb.sample_rate(),
            EffectState::Delay(delay) => delay.sample_rate(),
        }
    }
}

/// One filter as the audio backend sees it: the DSP state behind a lock, plus
/// the format the chain connected it with.
///
/// The reference hands the engine the filter object's address
/// (`iTVPBasicWaveFilter*`, `sound/WaveIntf.h:130`); this is the Rust stand-in
/// — an `Arc` the engine's chain holds through the id the class publishes from
/// `interface`, shared with the script-side setters that mutate the same DSP
/// state under the same lock.  A filter therefore has exactly the reference's
/// lifetime: registered when the TJS class constructs it, unregistered by
/// `finalize`.
struct SharedFilter {
    state: Mutex<FilterState>,
    /// Whether a live chain holds this filter.  The shipped DLL's source
    /// adapter refuses a second upstream source
    /// ([`MULTIPLE_BUFFER_ERROR`]), and a chain releases its filters when it
    /// drops (`krkr_audio::WaveFilterChain`'s `Drop`, the reference's
    /// `Clear`), so the same filter may join the next chain but not two at
    /// once.
    connected: AtomicBool,
}

struct FilterState {
    effect: EffectState,
    /// Format [`krkr_audio::WaveFilter::recreate`] was connected with: the
    /// reference's `SimpleFloatSource` copies the format the same way
    /// (`0x10004e8c`), and `process` reads its channel count from here.
    channels: u32,
}

impl krkr_audio::WaveFilter for SharedFilter {
    fn recreate(
        &self,
        spec: krkr_audio::PcmAudioSpec,
    ) -> std::result::Result<krkr_audio::PcmAudioSpec, String> {
        // The DLL's source adapter rejects a format wider than 32 bits or with
        // more than four channels (`HiRes format not supported.`, `0x10004e98`)
        // and every filter asks for one or two channels
        // (`check_filter_channels`, `invalid channels.`).
        // `Cannot connect multiple wave sound buffer at once.` (`0x10029798`,
        // thrown from `0x10004e75` when a second source is connected while one
        // is live).  The swap leaves the previous owner's claim intact: only the
        // chain that got `false` may release it.
        if self.connected.swap(true, Ordering::SeqCst) {
            return Err(MULTIPLE_BUFFER_ERROR.to_string());
        }
        if let Err(reason) = check_filter_channels(spec.channels) {
            self.connected.store(false, Ordering::SeqCst);
            return Err(reason.to_string());
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.effect.sample_rate() != spec.sample_rate as f32 {
            state.effect.set_sample_rate(spec.sample_rate as f32);
        }
        state.channels = spec.channels.max(1);
        Ok(spec)
    }

    fn clear(&self) {
        // The reference's `Clear` releases the source the filter was connected
        // to — the chain's hold on it, in this port's terms.  The DSP state is
        // the instance's and stays.
        self.connected.store(false, Ordering::SeqCst);
    }

    fn update(&self) {
        // The DLL's setters store their value and `update()` recomputes the
        // coefficients; this port recomputes them in the setters eagerly, so
        // the per-unit `Update` has nothing left to apply.
    }

    fn reset(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.effect.reset();
    }

    fn process(&self, frames: &mut [f32]) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let channels = state.channels.max(1);
        state.effect.process(frames, channels);
    }
}

/// The registration plus the live state of one filter instance, by object.
struct FilterSlot {
    filter: Arc<SharedFilter>,
    id: krkr_audio::WaveFilterId,
}

thread_local! {
    /// Filter instances by object handle. Entries are dropped by `finalize`,
    /// which also unregisters the filter from the audio backend, the
    /// convention the other plugin modules in this crate use.
    static FILTERS: RefCell<BTreeMap<ObjectHandle, FilterSlot>> =
        const { RefCell::new(BTreeMap::new()) };
}

/// Registers `state` with the audio backend and pairs it with its id.
fn new_filter_slot(state: EffectState) -> FilterSlot {
    let filter = Arc::new(SharedFilter {
        state: Mutex::new(FilterState {
            effect: state,
            channels: 2,
        }),
        connected: AtomicBool::new(false),
    });
    let id =
        krkr_audio::register_wave_filter(Arc::clone(&filter) as Arc<dyn krkr_audio::WaveFilter>);
    FilterSlot { filter, id }
}

/// The id the instance's `interface` property publishes: what the engine reads
/// out of the `filters` array and what the backend resolves back to this DSP
/// object.
fn filter_interface(handle: ObjectHandle) -> Option<i64> {
    FILTERS.with(|filters| filters.borrow().get(&handle).map(|slot| slot.id.raw()))
}

fn with_state<R>(handle: ObjectHandle, f: impl FnOnce(&mut EffectState) -> R) -> Option<R> {
    let filter = FILTERS.with(|filters| {
        filters
            .borrow()
            .get(&handle)
            .map(|slot| Arc::clone(&slot.filter))
    })?;
    let mut state = filter
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Some(f(&mut state.effect))
}

// ---------------------------------------------------------------------------
// GraphicEqualizer
// ---------------------------------------------------------------------------

/// The ten band centre frequencies, re-derived from the DLL's float table at
/// `.rdata 0x10029278` (loaded by the constructor at `0x10007ac8`).
pub const EQ_BAND_FREQUENCIES: [f32; 10] = [
    31.5, 63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];

/// The default gain of every band: ten `1.0` floats (`fld1; fst …`,
/// `0x10007ad3`-`0x10007b17`). Linear, so unity is flat.
pub const EQ_DEFAULT_GAIN: f32 = 1.0;

/// The one-octave peaking `Q`. Inferred: the DLL stores a per-band coefficient
/// vector built by an unrecovered templated filter, so this port uses the
/// standard octave-band peaking design (`Q = sqrt(2) ≈ 1.414`) for the
/// recovered band spacing.
pub const EQ_BAND_Q: f32 = std::f32::consts::SQRT_2;

/// One direct-form-II-transposed biquad.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    state1: f32,
    state2: f32,
}

impl Biquad {
    const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
        state1: 0.0,
        state2: 0.0,
    };

    /// The RBJ cookbook peak/notch coefficients. The recovered gains are
    /// linear multipliers (the constructor's `fld1` default of `1.0`), and
    /// RBJ's `A` is the square root of the linear gain (`|H(f0)| = A²`), so the
    /// square root is taken here and the band centre answers exactly the
    /// stored gain. `A = 1` collapses the section to the identity
    /// (`b0 == 1`, `b1 == a1`, `b2 == a2`).
    fn peaking(frequency: f32, sample_rate: f32, q: f32, linear_gain: f32) -> Self {
        let a = linear_gain.max(0.0).sqrt().max(1e-3);
        let w0 = 2.0 * std::f32::consts::PI * frequency / sample_rate;
        let (sin_w0, cos_w0) = (w0.sin(), w0.cos());
        let alpha = sin_w0 / (2.0 * q);
        let inv_a0 = 1.0 / (1.0 + alpha / a);
        Self {
            b0: (1.0 + alpha * a) * inv_a0,
            b1: (-2.0 * cos_w0) * inv_a0,
            b2: (1.0 - alpha * a) * inv_a0,
            a1: (-2.0 * cos_w0) * inv_a0,
            a2: (1.0 - alpha / a) * inv_a0,
            state1: 0.0,
            state2: 0.0,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let output = self.b0 * input + self.state1;
        self.state1 = flush_denormal(self.b1 * input - self.a1 * output + self.state2);
        self.state2 = flush_denormal(self.b2 * input - self.a2 * output);
        output
    }

    fn reset(&mut self) {
        self.state1 = 0.0;
        self.state2 = 0.0;
    }
}

/// The ten-band graphic equalizer: one peaking biquad per recovered band
/// centre, driven by the per-band linear gains.
///
/// `setGain`/`getGain` reproduce the recovered band handling exactly: band
/// indices outside `0..=9` are silently ignored by the setter and answer `0.0`
/// from the getter (`0x10002250`, `0x100022f0`).
#[derive(Debug)]
pub struct GraphicEqualizer {
    sample_rate: f32,
    gains: [f32; 10],
    bands: [Biquad; 10],
}

impl GraphicEqualizer {
    pub fn new(sample_rate: f32) -> Self {
        let mut equalizer = Self {
            sample_rate,
            gains: [EQ_DEFAULT_GAIN; 10],
            bands: [Biquad::IDENTITY; 10],
        };
        equalizer.rebuild();
        equalizer
    }

    /// The rate the biquads were designed for. The DLL's source adapter sets
    /// the format when the filter is connected (`Recreate`), so this is the
    /// chain's format, not the rate the object was constructed with.
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Re-designs every band for `sample_rate` (`0x10007ac8`'s constructor
    /// design, redone for the format the chain connected with).
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        if self.sample_rate == sample_rate {
            return;
        }
        self.sample_rate = sample_rate;
        self.rebuild();
    }

    /// The band count (always 10; the constructor allocates ten gains).
    pub const fn band_count(&self) -> usize {
        EQ_BAND_FREQUENCIES.len()
    }

    /// The stored gain of `band`, or `0.0` for an out-of-range band
    /// (`getGain`, `0x100022f0`: `cmp eax,0x9; ja` → `fldz`).
    pub fn gain(&self, band: i64) -> f32 {
        usize::try_from(band)
            .ok()
            .and_then(|band| self.gains.get(band).copied())
            .unwrap_or(0.0)
    }

    /// Stores a gain, ignoring an out-of-range band (`setGain`, `0x10002250`).
    /// The reference stores the value unchanged (no clamp).
    pub fn set_gain(&mut self, band: i64, gain: f32) {
        let Ok(index) = usize::try_from(band) else {
            return;
        };
        if index >= self.gains.len() {
            return;
        }
        self.gains[index] = gain;
        self.rebuild();
    }

    fn rebuild(&mut self) {
        for (index, band) in self.bands.iter_mut().enumerate() {
            band.reset();
            *band = Biquad::peaking(
                EQ_BAND_FREQUENCIES[index],
                self.sample_rate,
                EQ_BAND_Q,
                self.gains[index],
            );
        }
    }

    pub fn reset(&mut self) {
        for band in &mut self.bands {
            band.reset();
        }
    }

    /// Processes interleaved `f32` frames in place. Mono and stereo are the
    /// channel counts the recovered adapter asks for (`SimpleWaveFilter`
    /// requests one or two channels, `0x10004da0`: anything else is
    /// `invalid channels.`); a wider frame is processed channel-by-channel.
    pub fn process(&mut self, frames: &mut [f32], channels: usize) {
        if channels == 0 {
            return;
        }
        for frame in frames.chunks_mut(channels) {
            for value in frame.iter_mut() {
                let mut sample = *value;
                for band in &mut self.bands {
                    sample = band.process(sample);
                }
                *value = sample;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// StkFreeVerb
// ---------------------------------------------------------------------------

/// The comb tunings of the DLL's FreeVerb (`.data` `0x1003426c`, listed there
/// descending as `1617…1116`), scaled by `sample_rate / 44100` at construction
/// (`0x1000b892`).
const FREEVERB_COMB_TUNINGS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
/// The allpass tunings (`.data` `0x1003428c`, listed there as
/// `225, 556, 441, 341`; the bank is parallel, so the order carries no
/// behaviour).
const FREEVERB_ALLPASS_TUNINGS: [usize; 4] = [556, 441, 341, 225];
/// The classic stereo spread added to every right-channel delay.
const FREEVERB_STEREO_SPREAD: usize = 23;
/// `fixedGain` (`0x1002a0f0`), the input attenuation of every reverb write.
const FREEVERB_FIXED_GAIN: f32 = 0.015;
/// `scaleroom` (`0x1002a110`).
const FREEVERB_SCALE_ROOM: f32 = 0.28;
/// `offsetroom` (`0x1002a118`).
const FREEVERB_OFFSET_ROOM: f32 = 0.7;
/// `scaledamp` (`0x1002a108`).
const FREEVERB_SCALE_DAMP: f32 = 0.4;

/// The recovered `roomSize` default: the constructor stores the scaled comb
/// feedback `0.91` (`0x1000b84f`), which is `0.28 * 0.75 + 0.7`.
pub const FREEVERB_DEFAULT_ROOM_SIZE: f32 = 0.75;
/// The recovered `damping` default: the constructor stores the scaled damp
/// coefficient `0.1` (`0x1000b867`), which is `0.4 * 0.25`.
pub const FREEVERB_DEFAULT_DAMPING: f32 = 0.25;
/// The recovered `width` default: `fld1` at `0x1000b876`.
pub const FREEVERB_DEFAULT_WIDTH: f32 = 1.0;
/// The recovered `effectMix` default: `0.75` (`0x10029330`) passed to
/// `Effect::setEffectMix` at `0x1000b857`.
pub const FREEVERB_DEFAULT_EFFECT_MIX: f32 = 0.75;

/// The two recovered `Effect::setEffectMix` warnings (`0x1002a1b8`,
/// `0x1002a208`), returned so the TJS layer can log them.
pub const EFFECT_MIX_HIGH_WARNING: &str =
    "Effect::setEffectMix: mix parameter is greater than 1.0 ... setting to one!";
pub const EFFECT_MIX_LOW_WARNING: &str =
    "Effect::setEffectMix: mix parameter is less than zero ... setting to zero!";

#[derive(Debug)]
struct FreeVerbComb {
    buffer: Vec<f32>,
    index: usize,
    feedback: f32,
    damp1: f32,
    damp2: f32,
    store: f32,
}

impl FreeVerbComb {
    fn new(length: usize) -> Self {
        Self {
            buffer: vec![0.0; length.max(1)],
            index: 0,
            feedback: 0.0,
            damp1: 0.0,
            damp2: 1.0,
            store: 0.0,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let output = self.buffer[self.index];
        // Jezar's one-pole damping inside the loop, with the reference's
        // `undenormalise` flush applied to both state stores.
        self.store = flush_denormal(output * self.damp2 + self.store * self.damp1);
        self.buffer[self.index] = flush_denormal(input + self.store * self.feedback);
        self.index = (self.index + 1) % self.buffer.len();
        output
    }
}

#[derive(Debug)]
struct FreeVerbAllpass {
    buffer: Vec<f32>,
    index: usize,
}

impl FreeVerbAllpass {
    fn new(length: usize) -> Self {
        Self {
            buffer: vec![0.0; length.max(1)],
            index: 0,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let buffered = self.buffer[self.index];
        self.buffer[self.index] = flush_denormal(input + buffered * 0.5);
        self.index = (self.index + 1) % self.buffer.len();
        -input + buffered
    }
}

/// The FreeVerb reverberator with the DLL's constants and defaults.
///
/// Structure (Jezar's public-domain FreeVerb, whose constants the DLL uses):
/// eight parallel damped combs per channel into four series allpasses, with
/// `fixedGain` on the input, `roomSize` scaled to the comb feedback and
/// `damping` scaled to the one-pole coefficient. `mode` is the freeze switch
/// (feedback 1.0, damping 0, so the tail sustains).
#[derive(Debug)]
pub struct FreeVerb {
    sample_rate: f32,
    room_size: f32,
    damping: f32,
    width: f32,
    effect_mix: f32,
    frozen: bool,
    /// The integer behind the `extend` member: the DLL's class entry stores
    /// the constructor's sixth argument at `instance + 0xc8`
    /// (`0x10007f2f`) and `extend`'s getter reads exactly that field
    /// (`0x100021c0`: `mov esi,[ecx+0xc8]`) before it calls into the STK
    /// sub-object.  PARQUET's ini passes `1000` there
    /// (`Verb(0.75, 0.75, 0.25, 1.0, 0, 1000)`).
    extend: i64,
    combs_left: Vec<FreeVerbComb>,
    combs_right: Vec<FreeVerbComb>,
    allpasses_left: Vec<FreeVerbAllpass>,
    allpasses_right: Vec<FreeVerbAllpass>,
}

impl FreeVerb {
    pub fn new(sample_rate: f32) -> Self {
        let mut reverb = Self {
            sample_rate,
            room_size: FREEVERB_DEFAULT_ROOM_SIZE,
            damping: FREEVERB_DEFAULT_DAMPING,
            width: FREEVERB_DEFAULT_WIDTH,
            effect_mix: FREEVERB_DEFAULT_EFFECT_MIX,
            frozen: false,
            extend: 0,
            combs_left: Vec::new(),
            combs_right: Vec::new(),
            allpasses_left: Vec::new(),
            allpasses_right: Vec::new(),
        };
        reverb.rebuild();
        reverb
    }

    fn rebuild(&mut self) {
        let scale = self.sample_rate / 44100.0;
        self.combs_left = FREEVERB_COMB_TUNINGS
            .iter()
            .map(|tuning| FreeVerbComb::new(scaled_length(*tuning, scale)))
            .collect();
        self.combs_right = FREEVERB_COMB_TUNINGS
            .iter()
            .map(|tuning| FreeVerbComb::new(scaled_length(tuning + FREEVERB_STEREO_SPREAD, scale)))
            .collect();
        self.allpasses_left = FREEVERB_ALLPASS_TUNINGS
            .iter()
            .map(|tuning| FreeVerbAllpass::new(scaled_length(*tuning, scale)))
            .collect();
        self.allpasses_right = FREEVERB_ALLPASS_TUNINGS
            .iter()
            .map(|tuning| {
                FreeVerbAllpass::new(scaled_length(tuning + FREEVERB_STEREO_SPREAD, scale))
            })
            .collect();
        self.apply_controls();
    }

    fn apply_controls(&mut self) {
        let (feedback, damp1) = if self.frozen {
            (1.0, 0.0)
        } else {
            (
                self.room_size * FREEVERB_SCALE_ROOM + FREEVERB_OFFSET_ROOM,
                self.damping * FREEVERB_SCALE_DAMP,
            )
        };
        for comb in self
            .combs_left
            .iter_mut()
            .chain(self.combs_right.iter_mut())
        {
            comb.feedback = feedback;
            comb.damp1 = damp1;
            comb.damp2 = 1.0 - damp1;
        }
    }

    pub fn room_size(&self) -> f32 {
        self.room_size
    }

    pub fn damping(&self) -> f32 {
        self.damping
    }

    pub fn width(&self) -> f32 {
        self.width
    }

    pub fn effect_mix(&self) -> f32 {
        self.effect_mix
    }

    pub fn frozen(&self) -> bool {
        self.frozen
    }

    /// `setRoomSize`: the reference stores the value directly (the scaled comb
    /// feedback is derived in `update()`, `0x1000adb0`).
    pub fn set_room_size(&mut self, value: f32) {
        self.room_size = value;
        self.apply_controls();
    }

    /// `setDamping`.
    pub fn set_damping(&mut self, value: f32) {
        self.damping = value;
        self.apply_controls();
    }

    /// `setWidth`: the classic FreeVerb clamps the width to `[0, 1]`.
    pub fn set_width(&mut self, value: f32) {
        self.width = value.clamp(0.0, 1.0);
    }

    /// `Effect::setEffectMix` (`0x1000b6a0`): clamps to `[0, 1]` and answers
    /// the recovered warning when it had to.
    pub fn set_effect_mix(&mut self, mix: f32) -> Option<&'static str> {
        if mix < 0.0 {
            self.effect_mix = 0.0;
            return Some(EFFECT_MIX_LOW_WARNING);
        }
        if mix > 1.0 {
            self.effect_mix = 1.0;
            return Some(EFFECT_MIX_HIGH_WARNING);
        }
        self.effect_mix = mix;
        None
    }

    /// The rate the comb and allpass lengths were scaled from
    /// (`Stk::sampleRate() / 44100`, `0x1000b892`).
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Rescales every delay line for `sample_rate` (`0x1000b892`'s scaling,
    /// redone for the format the chain connected with) and re-applies the
    /// controls.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        if self.sample_rate == sample_rate {
            return;
        }
        self.sample_rate = sample_rate;
        self.rebuild();
    }

    /// `setMode`: freeze the tail.
    pub fn set_mode(&mut self, frozen: bool) {
        self.frozen = frozen;
        self.apply_controls();
    }

    /// The `extend` member's integer.  The DLL keeps it at `+0xc8` and its
    /// non-const getter (`0x100021c0`) hands it to the STK sub-object; what
    /// that call does to the reverb was not recovered, so this port stores the
    /// value and answers it back instead of inventing an effect.
    pub fn extend(&self) -> i64 {
        self.extend
    }

    pub fn set_extend(&mut self, value: i64) {
        self.extend = value;
    }

    pub fn reset(&mut self) {
        for comb in self
            .combs_left
            .iter_mut()
            .chain(self.combs_right.iter_mut())
        {
            comb.buffer.fill(0.0);
            comb.store = 0.0;
            comb.index = 0;
        }
        for allpass in self
            .allpasses_left
            .iter_mut()
            .chain(self.allpasses_right.iter_mut())
        {
            allpass.buffer.fill(0.0);
            allpass.index = 0;
        }
    }

    /// Processes interleaved `f32` frames in place.
    ///
    /// A mono frame feeds both reverb channels; a stereo frame feeds left and
    /// right. Channels past the second are passed through untouched (the
    /// recovered adapter only ever asks for one or two channels, and the
    /// DLL rejects a source wider than four with `HiRes format not supported.`).
    /// A buffer whose length is not a multiple of `channels` is tolerated like
    /// the sibling processors: the trailing partial frame is processed with
    /// its missing channels read as the first, so the reverb never indexes
    /// past the chunk it was handed.
    pub fn process(&mut self, frames: &mut [f32], channels: usize) {
        if channels == 0 {
            return;
        }
        for frame in frames.chunks_mut(channels) {
            let input_left = frame[0];
            let input_right = frame.get(1).copied().unwrap_or(input_left);
            let scaled_left = input_left * FREEVERB_FIXED_GAIN;
            let scaled_right = input_right * FREEVERB_FIXED_GAIN;

            let mut wet_left = 0.0;
            let mut wet_right = 0.0;
            for (left, right) in self.combs_left.iter_mut().zip(self.combs_right.iter_mut()) {
                wet_left += left.process(scaled_left);
                wet_right += right.process(scaled_right);
            }
            for (left, right) in self
                .allpasses_left
                .iter_mut()
                .zip(self.allpasses_right.iter_mut())
            {
                wet_left = left.process(wet_left);
                wet_right = right.process(wet_right);
            }

            let wet1 = self.width / 2.0 + 0.5;
            let wet2 = (1.0 - self.width) / 2.0;
            let mixed_left = wet_left * wet1 + wet_right * wet2;
            let mixed_right = wet_right * wet1 + wet_left * wet2;
            let dry = 1.0 - self.effect_mix;

            frame[0] = dry * input_left + self.effect_mix * mixed_left;
            if let Some(right) = frame.get_mut(1) {
                *right = dry * input_right + self.effect_mix * mixed_right;
            }
        }
    }
}

fn scaled_length(tuning: usize, scale: f32) -> usize {
    ((tuning as f32) * scale).round().max(1.0) as usize
}

// ---------------------------------------------------------------------------
// DelayEffect
// ---------------------------------------------------------------------------

/// The recovered maximum-delay default: the constructor stores the integer
/// `1000` at `+0xc8` (`0x100077ff`), read as milliseconds here (inferred).
pub const DELAY_DEFAULT_MAX_MILLIS: f32 = 1000.0;

/// The recovered `stk::OnePole::setPole` bound: `|pole| < 1`
/// (`0x1002a50c`, `0x1002a548`).
pub const DELAY_POLE_WARNING: &str = "OnePole::setPole: argument (…) should be less than 1.0!";

/// One one-pole damping stage (`stk::OnePole`: `y[n] = a1 * y[n-1] + b0 * x[n]`).
#[derive(Clone, Copy, Debug, Default)]
struct OnePole {
    a1: f32,
    b0: f32,
    last: f32,
}

impl OnePole {
    fn process(&mut self, input: f32) -> f32 {
        self.last = flush_denormal(self.b0 * input + self.a1 * self.last);
        self.last
    }
}

/// The delay effect: a feedback delay line whose tail is damped by a one-pole
/// filter, i.e. the `stk::Delay` + `stk::OnePole` pair the DLL links.
///
/// The four `init` parameters are recovered by slot and type
/// (`WaveDelay::init`, `0x10002360`: `p0` int → `+0xcc`, `p1` real → `+0xd4`,
/// `p2` real → `+0xd8`, optional `p3` int → `+0xc0`); the names this port uses
/// — delay milliseconds, damping pole, feedback, maximum delay — are inferred
/// from those STK classes, and the recovery of the *semantics* is listed as
/// open in the module docs.
#[derive(Debug)]
pub struct DelayEffect {
    sample_rate: f32,
    max_delay_millis: f32,
    delay_millis: f32,
    damping: f32,
    feedback: f32,
    /// `+0xc4`'s recovered default `1.8125` is stored for completeness; its
    /// role was not recovered and it does not drive the DSP.
    unrecovered_field: f32,
    lines: Vec<Vec<f32>>,
    positions: Vec<usize>,
    dampers: Vec<OnePole>,
}

impl DelayEffect {
    pub fn new(sample_rate: f32) -> Self {
        let mut effect = Self {
            sample_rate,
            max_delay_millis: DELAY_DEFAULT_MAX_MILLIS,
            delay_millis: 0.0,
            damping: 0.0,
            feedback: 0.0,
            unrecovered_field: 1.8125,
            lines: Vec::new(),
            positions: Vec::new(),
            dampers: Vec::new(),
        };
        effect.rebuild();
        effect
    }

    fn rebuild(&mut self) {
        let length = ((self.max_delay_millis / 1000.0) * self.sample_rate).ceil() as usize + 1;
        self.lines = (0..2).map(|_| vec![0.0; length.max(1)]).collect();
        self.positions = vec![0; 2];
        self.dampers = vec![OnePole::default(); 2];
        self.set_damping(self.damping);
    }

    /// The rate the delay lines were sized from.
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Resizes the delay lines for `sample_rate` (`0x10002360`'s maximum-delay
    /// sizing, redone for the format the chain connected with).
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        if self.sample_rate == sample_rate {
            return;
        }
        self.sample_rate = sample_rate;
        self.rebuild();
    }

    /// `Delay::setDelay` semantics (`0x1001019a`): a delay past the maximum is
    /// refused with the recovered warning, everything else is stored.
    pub fn set_delay_millis(&mut self, millis: f32) -> Option<&'static str> {
        if millis > self.max_delay_millis {
            return Some("Delay::setDelay: argument (…) greater than maximum!");
        }
        self.delay_millis = millis.max(0.0);
        None
    }

    /// `OnePole::setPole` semantics (`0x10010933`): `|pole| >= 1` is refused
    /// with the recovered warning and **without touching the live
    /// coefficients** — STK's setter returns before storing, so the previous
    /// pole keeps filtering and [`DelayEffect::damping`] keeps reporting it.
    pub fn set_damping(&mut self, pole: f32) -> Option<&'static str> {
        if !pole.is_finite() || pole.abs() >= 1.0 {
            return Some(DELAY_POLE_WARNING);
        }
        self.damping = pole;
        for damper in &mut self.dampers {
            damper.a1 = pole;
            damper.b0 = 1.0 - pole;
        }
        None
    }

    /// The optional fourth `init` argument: the maximum delay in milliseconds.
    pub fn set_max_delay_millis(&mut self, millis: f32) {
        if millis <= 0.0 {
            return;
        }
        self.max_delay_millis = millis;
        self.rebuild();
    }

    /// The recovered `p1`/`p2` pair: damping pole and feedback.
    pub fn damping(&self) -> f32 {
        self.damping
    }

    pub fn feedback(&self) -> f32 {
        self.feedback
    }

    pub fn set_feedback(&mut self, feedback: f32) {
        self.feedback = feedback;
    }

    pub fn delay_millis(&self) -> f32 {
        self.delay_millis
    }

    pub fn max_delay_millis(&self) -> f32 {
        self.max_delay_millis
    }

    /// The `+0xc4` slot the constructor fills with `1.8125` (`fstp [esi+0xc0]`
    /// of the `0.75` double at `0x10029330`, `0x100077f4`). Its role was not
    /// recovered and it drives nothing; kept so the layout claim is checkable.
    pub fn unrecovered_field(&self) -> f32 {
        self.unrecovered_field
    }

    pub fn reset(&mut self) {
        for line in &mut self.lines {
            line.fill(0.0);
        }
        for damper in &mut self.dampers {
            damper.last = 0.0;
        }
    }

    /// Processes interleaved `f32` frames in place: `out = dry + damped echo`,
    /// with the damped delayed sample written back through `feedback`.
    pub fn process(&mut self, frames: &mut [f32], channels: usize) {
        if channels == 0 || self.lines.is_empty() {
            return;
        }
        let delay_samples = ((self.delay_millis / 1000.0) * self.sample_rate) as usize;
        for frame in frames.chunks_mut(channels) {
            for (channel, value) in frame.iter_mut().enumerate() {
                let index = channel.min(self.lines.len() - 1);
                let line = &mut self.lines[index];
                let length = line.len();
                let position = self.positions[index].min(length - 1);
                let read = (position + length - delay_samples.min(length - 1)) % length;
                let delayed = line[read];
                let damped = self.dampers[index].process(delayed);
                line[position] = flush_denormal(*value + self.feedback * damped);
                self.positions[index] = (position + 1) % length;
                *value = flush_denormal(*value + damped);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Format checks (the recovered adapter's contract)
// ---------------------------------------------------------------------------

/// The recovered `SimpleFloatSource::SetSource` checks (`0x10004e50`): a
/// format wider than 32 bits or with more than four channels is refused.
pub fn check_source_format(
    bits_per_sample: u32,
    channels: u32,
) -> std::result::Result<(), &'static str> {
    if bits_per_sample > 32 || channels > 4 {
        return Err("HiRes format not supported.");
    }
    Ok(())
}

/// The recovered per-filter channel check (`0x10004da0`): a `SimpleWaveFilter`
/// asks its source adapter for **one or two** channels only.
pub fn check_filter_channels(channels: u32) -> std::result::Result<(), &'static str> {
    if !(1..=2).contains(&channels) {
        return Err("invalid channels.");
    }
    Ok(())
}

/// The recovered "one buffer per filter" error (`0x10029798`, thrown from
/// `0x10004e75` when a second source is connected).
pub const MULTIPLE_BUFFER_ERROR: &str = "Cannot connect multiple wave sound buffer at once.";

/// Flushes a subnormal value to zero, the reference's `undenormalise`
/// (`(bits & 0x7f800000) == 0`), keeping every filter state store exactly
/// zero once the tail has decayed instead of leaving denormals behind.
fn flush_denormal(value: f32) -> f32 {
    if value != 0.0 && value.abs() < f32::MIN_POSITIVE {
        0.0
    } else {
        value
    }
}

// ---------------------------------------------------------------------------
// TJS surface
// ---------------------------------------------------------------------------

/// Announces a recovered reference warning through the engine log. The
/// reference prints these through `stk::Stk::handleError`; the engine's log is
/// this port's `oStream_`.
fn log_warning(runtime: &mut Runtime<KrkrHost>, message: &str) {
    runtime.host_mut().log(message);
}

fn install_wf_basic_effect(runtime: &mut Runtime<KrkrHost>) {
    FILTERS.with(|filters| filters.borrow_mut().clear());
    install_equalizer_class(runtime);
    install_free_verb_class(runtime);
    install_delay_class(runtime);
}

/// `GraphicEqualizer`: `new WaveSoundBuffer.GraphicEqualizer()`,
/// `setGain(band, gain)`, `getGain(band)` and the read-only `interface`
/// property.
fn install_equalizer_class(runtime: &mut Runtime<KrkrHost>) {
    let class = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, _this: Option<ObjectHandle>, args: Vec<Variant>| {
            let instance = new_plugin_instance(runtime, "GraphicEqualizer");
            let sample_rate = native_sample_rate(runtime);
            let mut equalizer = GraphicEqualizer::new(sample_rate);
            // The DLL's class entry (`0x10007fa0`) reads the constructor
            // arguments as the ten band gains: `AsReal(param[i])` is stored
            // into `instance + 0xc4 + 4*i` — the same array `setGain`
            // (`0x10002250`) writes — for as long as `i <= 9`, and every
            // further argument is dropped (`cmp esi,0x9; ja`,
            // `0x1000804d`-`0x10008065`).  PARQUET's `EQ` wrapper
            // (`voiceeffect.tjs` object 49) passes the ini's ten values
            // through, so this is the only place they reach the filter.
            for (band, value) in args.iter().take(EQ_BAND_FREQUENCIES.len()).enumerate() {
                equalizer.set_gain(band as i64, value.to_real()? as f32);
            }
            FILTERS.with(|filters| {
                filters.borrow_mut().insert(
                    instance,
                    new_filter_slot(EffectState::Equalizer(Box::new(equalizer))),
                );
            });
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(class, "GraphicEqualizer");
    install_equalizer_members(runtime, class);
    publish_filter_class(runtime, "GraphicEqualizer", class);
}

fn install_equalizer_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "setGain", equalizer_set_gain);
    runtime.register_object_native(handle, "getGain", equalizer_get_gain);
    runtime.register_object_native(handle, "finalize", plugin_finalize);
    runtime.register_object_native_property_with_access(
        handle,
        "interface",
        NativePropertyAccess::ReadOnly,
        |_runtime, this| {
            Ok(Variant::Integer(
                this.and_then(filter_interface).unwrap_or(0),
            ))
        },
        |_runtime, _this, _value| Err(TjsError::access_denied()),
    );
}

fn equalizer_set_gain(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = plugin_this(runtime, this_obj) else {
        return Err(TjsError::bad_param_count());
    };
    let band = args
        .first()
        .map(Variant::to_integer)
        .transpose()?
        .ok_or_else(TjsError::bad_param_count)?;
    let gain = args
        .get(1)
        .map(Variant::to_real)
        .transpose()?
        .ok_or_else(TjsError::bad_param_count)?;
    with_state(this, |state| {
        if let EffectState::Equalizer(equalizer) = state {
            equalizer.set_gain(band, gain as f32);
        }
    });
    Ok(Variant::Void)
}

fn equalizer_get_gain(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = plugin_this(runtime, this_obj) else {
        return Err(TjsError::bad_param_count());
    };
    let band = args
        .first()
        .map(Variant::to_integer)
        .transpose()?
        .ok_or_else(TjsError::bad_param_count)?;
    let gain = with_state(this, |state| match state {
        EffectState::Equalizer(equalizer) => equalizer.gain(band),
        _ => 0.0,
    })
    .unwrap_or(0.0);
    Ok(Variant::Real(f64::from(gain)))
}

/// `StkFreeVerb`: the five recovered properties plus `extend` and `interface`.
fn install_free_verb_class(runtime: &mut Runtime<KrkrHost>) {
    let class = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, _this: Option<ObjectHandle>, args: Vec<Variant>| {
            let instance = new_plugin_instance(runtime, "StkFreeVerb");
            let sample_rate = native_sample_rate(runtime);
            let mut reverb = FreeVerb::new(sample_rate);
            // The DLL's class entry (`0x10007d90`) reads up to six arguments,
            // each one absent leaving the field at its constructor default —
            // the ini line `Verb(0.75, 0.75, 0.25, 1.0, 0, 1000)` spells every
            // one of them out:
            //
            // | arg | member | DLL |
            // | --- | ------ | --- |
            // | 0 | `effectMix` | the `effectMix` setter (`0x10001ea0`): stores the double at `+0xc0` and calls the STK virtual at `+0xd0+0xc`, i.e. `Effect::setEffectMix` (`0x1000b6a0`) |
            // | 1 | `roomSize` | `0x1000af90`: `0.28 * r + 0.7` into `+0x40` (the comb feedback `scaleroom`/`offsetroom` constants at `0x1002a110`/`0x1002a118`) |
            // | 2 | `damping` | `0x1000afb0`: `0.4 * d` into `+0x50` (`scaledamp`, `0x1002a108`) |
            // | 3 | `width` | `0x1000afd0`: the value into `+0x78` |
            // | 4 | `mode` | `0x1000afe0`: the integer's low byte into `+0x80` |
            // | 5 | `extend` | `0x10007f2f`: `AsInteger` into `+0xc8` |
            //
            // Every conversion is the DLL's (`AsReal` for 1-3, `AsInteger` for
            // 4-5) and the mode is the low byte of that integer, not "nonzero"
            // (`movzx ecx,al`, `0x10007ef4`).
            if let Some(value) = args.first() {
                if let Some(warning) = reverb.set_effect_mix(value.to_real()? as f32) {
                    runtime.host_mut().log(warning);
                }
            }
            if let Some(value) = args.get(1) {
                reverb.set_room_size(value.to_real()? as f32);
            }
            if let Some(value) = args.get(2) {
                reverb.set_damping(value.to_real()? as f32);
            }
            if let Some(value) = args.get(3) {
                reverb.set_width(value.to_real()? as f32);
            }
            if let Some(value) = args.get(4) {
                reverb.set_mode(value.to_integer()? as u8 != 0);
            }
            if let Some(value) = args.get(5) {
                reverb.set_extend(value.to_integer()?);
            }
            FILTERS.with(|filters| {
                filters
                    .borrow_mut()
                    .insert(instance, new_filter_slot(EffectState::FreeVerb(reverb)));
            });
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(class, "StkFreeVerb");
    install_free_verb_members(runtime, class);
    publish_filter_class(runtime, "StkFreeVerb", class);
}

fn install_free_verb_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", plugin_finalize);
    let real = |runtime: &mut Runtime<KrkrHost>,
                handle: ObjectHandle,
                name: &'static str,
                get: fn(&FreeVerb) -> f64,
                set: fn(&mut FreeVerb, f32) -> Option<&'static str>| {
        runtime.register_object_native_property_with_access(
            handle,
            name,
            NativePropertyAccess::ReadWrite,
            move |_runtime, this| {
                let value = this
                    .and_then(|this| with_state(this, |state| free_verb_ref(state).map(get)))
                    .flatten()
                    .unwrap_or(0.0);
                Ok(Variant::Real(value))
            },
            move |runtime, this, value| {
                let parsed = value.to_real()? as f32;
                let warning = this.and_then(|this| {
                    with_state(this, |state| {
                        free_verb_mut(state).and_then(|reverb| set(reverb, parsed))
                    })
                    .flatten()
                });
                if let Some(warning) = warning {
                    log_warning(runtime, warning);
                }
                Ok(())
            },
        );
    };
    real(
        runtime,
        handle,
        "roomSize",
        |reverb| f64::from(reverb.room_size()),
        |reverb, value| {
            reverb.set_room_size(value);
            None
        },
    );
    real(
        runtime,
        handle,
        "damping",
        |reverb| f64::from(reverb.damping()),
        |reverb, value| {
            reverb.set_damping(value);
            None
        },
    );
    real(
        runtime,
        handle,
        "width",
        |reverb| f64::from(reverb.width()),
        |reverb, value| {
            reverb.set_width(value);
            None
        },
    );
    real(
        runtime,
        handle,
        "effectMix",
        |reverb| f64::from(reverb.effect_mix()),
        |reverb, value| reverb.set_effect_mix(value),
    );
    runtime.register_object_native_property_with_access(
        handle,
        "mode",
        NativePropertyAccess::ReadWrite,
        |_runtime, this| {
            let frozen = this
                .and_then(|this| {
                    with_state(this, |state| free_verb_ref(state).map(FreeVerb::frozen))
                })
                .flatten()
                .unwrap_or(false);
            Ok(Variant::Integer(i64::from(frozen)))
        },
        |_runtime, this, value| {
            let frozen = value.is_truthy();
            if let Some(this) = this {
                with_state(this, |state| {
                    if let Some(reverb) = free_verb_mut(state) {
                        reverb.set_mode(frozen);
                    }
                });
            }
            Ok(())
        },
    );
    // `extend` is the one member of the StkFreeVerb group whose getter is
    // non-const (`GetterCallback<P8StkFreeVerb@@AEH…>`, RTTI at `0x10033b70`);
    // it reads the integer the constructor stored at `+0xc8` and hands it to
    // the STK sub-object (`0x100021c0`).  What that call changes was not
    // recovered, so the port stores and answers the value — the constructor
    // argument PARQUET's ini passes — instead of inventing an effect.
    runtime.register_object_native_property_with_access(
        handle,
        "extend",
        NativePropertyAccess::ReadWrite,
        |_runtime, this| {
            let extend = this
                .and_then(|this| {
                    with_state(this, |state| free_verb_ref(state).map(FreeVerb::extend))
                })
                .flatten()
                .unwrap_or(0);
            Ok(Variant::Integer(extend))
        },
        |_runtime, this, value| {
            let extend = value.to_integer()?;
            if let Some(this) = this {
                with_state(this, |state| {
                    if let Some(reverb) = free_verb_mut(state) {
                        reverb.set_extend(extend);
                    }
                });
            }
            Ok(())
        },
    );
    runtime.register_object_native_property_with_access(
        handle,
        "interface",
        NativePropertyAccess::ReadOnly,
        |_runtime, this| {
            Ok(Variant::Integer(
                this.and_then(filter_interface).unwrap_or(0),
            ))
        },
        |_runtime, _this, _value| Err(TjsError::access_denied()),
    );
}

/// `DelayEffect`: the variadic `init(delay, damping, feedback, maxDelay?)` and
/// the read-only `interface` property.
fn install_delay_class(runtime: &mut Runtime<KrkrHost>) {
    let class = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, _this: Option<ObjectHandle>, args: Vec<Variant>| {
            let instance = new_plugin_instance(runtime, "DelayEffect");
            let sample_rate = native_sample_rate(runtime);
            let mut delay = DelayEffect::new(sample_rate);
            // The DLL's class entry (`0x10009300`) calls the very function the
            // `init` member is (`0x10002360`, registered at `0x10009ae8`):
            // `if (numparams > 0) WaveDelay::init(instance, numparams, param)`
            // (`0x10009366`-`0x10009372`), and `init` itself ignores fewer than
            // three arguments (`cmp ebp,0x2; jle`).  The constructor's
            // arguments are therefore exactly `init`'s.
            if let Some(init) = delay_init_arguments(&args)? {
                for warning in apply_delay_init(&mut delay, &init) {
                    runtime.host_mut().log(warning);
                }
            }
            FILTERS.with(|filters| {
                filters
                    .borrow_mut()
                    .insert(instance, new_filter_slot(EffectState::Delay(delay)));
            });
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(class, "DelayEffect");
    install_delay_members(runtime, class);
    publish_filter_class(runtime, "DelayEffect", class);
}

fn install_delay_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", plugin_finalize);
    runtime.register_object_native(handle, "init", delay_init);
    runtime.register_object_native_property_with_access(
        handle,
        "interface",
        NativePropertyAccess::ReadOnly,
        |_runtime, this| {
            Ok(Variant::Integer(
                this.and_then(filter_interface).unwrap_or(0),
            ))
        },
        |_runtime, _this, _value| Err(TjsError::access_denied()),
    );
}

/// `init(p0, p1, p2, p3?)`: the recovered argument handling of
/// `WaveDelay::init` (`0x10002360`) — fewer than three arguments is a no-op,
/// the fourth is optional.
/// The `WaveDelay::init` parameters (`0x10002360`), shared by the member and
/// by the class entry (which calls the same function,
/// `sound`… `wfBasicEffect.dll:0x10009372`).
#[derive(Clone, Copy, Debug, PartialEq)]
struct DelayInit {
    /// `p0`, an integer: the delay in milliseconds (`+0xcc`).
    delay_millis: f32,
    /// `p1`, a real: the one-pole damping pole (`+0xd4`).
    damping: f32,
    /// `p2`, a real: the feedback (`+0xd8`).
    feedback: f32,
    /// The optional `p3`, an integer: the maximum delay (`+0xc0`).
    max_delay_millis: Option<f32>,
}

/// Reads the `init` arguments: fewer than three are ignored entirely
/// (`0x10002360`'s `cmp ebp,0x2; jle`), and the conversions are the DLL's
/// (`AsInteger` for `p0`/`p3`, `AsReal` for `p1`/`p2`).
fn delay_init_arguments(args: &[Variant]) -> Result<Option<DelayInit>> {
    if args.len() < 3 {
        return Ok(None);
    }
    let max_delay_millis = match args.get(3) {
        Some(value) if !matches!(value, Variant::Void) => Some(value.to_integer()? as f32),
        _ => None,
    };
    Ok(Some(DelayInit {
        delay_millis: args[0].to_integer()? as f32,
        damping: args[1].to_real()? as f32,
        feedback: args[2].to_real()? as f32,
        max_delay_millis,
    }))
}

/// Applies the parsed `init` parameters and answers the recovered warnings
/// (`Delay::setDelay` beyond the maximum, `OnePole::setPole` at `|pole| >= 1`).
fn apply_delay_init(delay_effect: &mut DelayEffect, init: &DelayInit) -> Vec<&'static str> {
    let mut warnings = Vec::new();
    if let Some(warning) = delay_effect.set_damping(init.damping) {
        warnings.push(warning);
    }
    delay_effect.set_feedback(init.feedback);
    if let Some(max_delay) = init.max_delay_millis {
        delay_effect.set_max_delay_millis(max_delay);
    }
    if let Some(warning) = delay_effect.set_delay_millis(init.delay_millis) {
        warnings.push(warning);
    }
    warnings
}

fn delay_init(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = plugin_this(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    let Some(init) = delay_init_arguments(&args)? else {
        return Ok(Variant::Void);
    };
    let warnings = with_state(this, |state| {
        let EffectState::Delay(delay_effect) = state else {
            return Vec::new();
        };
        apply_delay_init(delay_effect, &init)
    })
    .unwrap_or_default();
    for warning in warnings {
        log_warning(runtime, warning);
    }
    Ok(Variant::Void)
}

fn free_verb_ref(state: &EffectState) -> Option<&FreeVerb> {
    match state {
        EffectState::FreeVerb(reverb) => Some(reverb),
        _ => None,
    }
}

fn free_verb_mut(state: &mut EffectState) -> Option<&mut FreeVerb> {
    match state {
        EffectState::FreeVerb(reverb) => Some(reverb),
        _ => None,
    }
}

/// Publishes one filter class where the reference's binder puts it: as a
/// member of the `WaveSoundBuffer` class object.
///
/// The DLL's three `SimpleBinder` chains are all anchored on `WaveSoundBuffer`
/// (`BindUtil(TJS_W("WaveSoundBuffer"), link).Class(TJS_W("StkFreeVerb"), …)`;
/// see the module docs), so `WaveSoundBuffer.StkFreeVerb` is the class object's
/// only home — the DLL never registers a global of these names, and PARQUET's
/// `voiceeffect.tjs` reads them from there (objects 48-50).
fn publish_filter_class(
    runtime: &mut Runtime<KrkrHost>,
    class_name: &'static str,
    class: ObjectHandle,
) {
    if let Some(wave) = runtime.global_member("WaveSoundBuffer").object_handle() {
        runtime.set_object_member(wave, class_name, Variant::Object(class));
    }
}

/// The class object behind `WaveSoundBuffer.<name>`, the one place
/// [`publish_filter_class`] puts it.
fn filter_class(runtime: &Runtime<KrkrHost>, class_name: &str) -> Option<ObjectHandle> {
    let wave = runtime.global_member("WaveSoundBuffer").object_handle()?;
    runtime.object_member(wave, class_name).object_handle()
}

/// A fresh instance object of a plugin class: class info, `__className` (the
/// value a future engine-side filter chain resolves by identity), and the
/// superclass link to the class object under `WaveSoundBuffer`.
fn new_plugin_instance(runtime: &mut Runtime<KrkrHost>, class_name: &'static str) -> ObjectHandle {
    let instance = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(instance, class_name);
    runtime.set_object_member(
        instance,
        "__className",
        Variant::String(class_name.to_string()),
    );
    if let Some(class) = filter_class(runtime, class_name) {
        runtime.set_object_super_class(instance, class);
    }
    instance
}

fn plugin_this(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Option<ObjectHandle> {
    this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle))
}

fn plugin_finalize(
    _runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(this) = this_obj {
        let removed = FILTERS.with(|filters| filters.borrow_mut().remove(&this));
        // The engine's chain holds the same `Arc` until its own slot drops, so
        // unregistering here ends the *id's* life (a stale id in a script's
        // `filters` array no longer resolves), not the DSP object's.
        if let Some(slot) = removed {
            krkr_audio::unregister_wave_filter(slot.id);
        }
    }
    Ok(Variant::Void)
}

/// The sample rate a new filter is built for. The engine's audio layer owns
/// the real rate; the riff chain has no way to ask it yet, so instances are
/// built for the default 44 100 Hz — the rate the DLL's FreeVerb tunings are
/// scaled from (`Stk::sampleRate()` at construction).
fn native_sample_rate(_runtime: &Runtime<KrkrHost>) -> f32 {
    44100.0
}

// ---------------------------------------------------------------------------
// The plugin's public processing surface
// ---------------------------------------------------------------------------

/// The processing state of a live filter object, for a future engine filter
/// chain that resolves a `WaveSoundBuffer.filters` element by identity.
pub enum FilterHandle {
    Equalizer,
    FreeVerb,
    Delay,
}

/// Runs `process` on the filter behind `handle`, when one is alive.
pub fn process_filter(
    handle: ObjectHandle,
    frames: &mut [f32],
    channels: usize,
) -> Option<FilterHandle> {
    with_state(handle, |state| match state {
        EffectState::Equalizer(equalizer) => {
            equalizer.process(frames, channels);
            FilterHandle::Equalizer
        }
        EffectState::FreeVerb(reverb) => {
            reverb.process(frames, channels);
            FilterHandle::FreeVerb
        }
        EffectState::Delay(delay) => {
            delay.process(frames, channels);
            FilterHandle::Delay
        }
    })
}
#[cfg(test)]
mod tests {
    use krkr_engine::{EngineConfig, KrkrEngine};
    use krkr_tjs2::runtime::Variant;

    use super::*;

    // ------------------------------------------------------------- surface

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(WfBasicEffectPlugin).expect("plugin");
        engine
    }

    fn run(engine: &mut KrkrEngine, script: &str) -> Variant {
        engine
            .execute_expression("inline.tjs", script)
            .expect("script")
    }

    fn try_run(engine: &mut KrkrEngine, script: &str) -> krkr_tjs2::Result<Variant> {
        engine.execute_expression("inline.tjs", script)
    }

    fn string(engine: &mut KrkrEngine, script: &str) -> String {
        run(engine, script).to_tjs_string().expect("string")
    }

    fn real(engine: &mut KrkrEngine, script: &str) -> f64 {
        run(engine, script).to_real().expect("real")
    }

    /// Where the games reach the classes: the binder's base. PARQUET's
    /// `voiceeffect.tjs` objects 48-50 read `WaveSoundBuffer.<name>` and `new`
    /// it, so every script-level probe below spells the class that way.
    const EQ: &str = "WaveSoundBuffer.GraphicEqualizer";
    const FREE_VERB: &str = "WaveSoundBuffer.StkFreeVerb";
    const DELAY: &str = "WaveSoundBuffer.DelayEffect";

    /// The recovered member set of every class (module docs, binder chain
    /// `0x10009390`): the class must expose each one, reached the way a game
    /// reaches it.
    #[test]
    fn the_three_classes_carry_their_recovered_members() {
        let mut engine = engine();
        let members = [
            ("StkFreeVerb", "extend"),
            ("StkFreeVerb", "mode"),
            ("StkFreeVerb", "width"),
            ("StkFreeVerb", "damping"),
            ("StkFreeVerb", "roomSize"),
            ("StkFreeVerb", "effectMix"),
            ("StkFreeVerb", "interface"),
            ("GraphicEqualizer", "setGain"),
            ("GraphicEqualizer", "getGain"),
            ("GraphicEqualizer", "interface"),
            ("DelayEffect", "init"),
            ("DelayEffect", "interface"),
        ];
        for (class, member) in members {
            let probe = string(
                &mut engine,
                &format!("(function() {{ return typeof WaveSoundBuffer.{class}.{member}; }})()"),
            );
            assert_ne!(
                probe, "undefined",
                "WaveSoundBuffer.{class}.{member} is not installed"
            );
        }
    }

    /// The class-object path the games use, end to end. `voiceeffect.tjs`
    /// builds its filters through the guarded harness `VoiceEffectFactory`
    /// (`if (typeof VoiceEffectFactory[a0] == "Object") …`) and its `Verb` /
    /// `EQ` / `Delay` members resolve the class through the
    /// `WaveSoundBuffer` object before `new`ing it; the same guarded helper
    /// here (`typeof a0[a1] == "Object"`) has to find all three classes, and
    /// what it builds has to be a live filter the buffer's own `filters` array
    /// accepts. The bare names stay absent: the DLL anchors every class on
    /// `WaveSoundBuffer` and registers no global of them, so a script probing a
    /// global sees `void` (`typeof` of the miss answers "undefined").
    #[test]
    fn the_guarded_class_object_lookup_builds_live_filters() {
        let mut engine = engine();
        let value = string(
            &mut engine,
            "(function() {\n\
                 var l1 = function(a0, a1) {\n\
                     if (typeof a0[a1] == \"Object\") return new a0[a1]();\n\
                     return null;\n\
                 };\n\
                 var buffer = new WaveSoundBuffer();\n\
                 buffer.filters.clear();\n\
                 var eq = l1(WaveSoundBuffer, \"GraphicEqualizer\");\n\
                 var reverb = l1(WaveSoundBuffer, \"StkFreeVerb\");\n\
                 var delay = l1(WaveSoundBuffer, \"DelayEffect\");\n\
                 eq.setGain(9, 2);\n\
                 buffer.filters.add(eq);\n\
                 buffer.filters.add(reverb);\n\
                 buffer.filters.add(delay);\n\
                 return typeof WaveSoundBuffer.StkFreeVerb + \":\" + eq.getGain(9) + \":\" +\n\
                     buffer.filters.count + \":\" +\n\
                     (typeof global.StkFreeVerb) + \":\" + (typeof global.GraphicEqualizer) + \":\" +\n\
                     (typeof global.DelayEffect);\n\
             })()",
        );
        assert_eq!(value, "Object:2:3:undefined:undefined:undefined");
    }

    /// The defaults on a fresh instance: the ten `1.0` band gains, the
    /// recovered FreeVerb constants, and the DelayEffect slots.
    #[test]
    fn instance_defaults_match_the_recovered_constants() {
        let mut engine = engine();
        let gains = string(
            &mut engine,
            &format!(
                "(function() {{\n\
                     var eq = new {EQ}();\n\
                     var out = [];\n\
                     for (var band = 0; band < 10; band = band + 1) out.push(eq.getGain(band));\n\
                     return out.join(\",\");\n\
                 }})()"
            ),
        );
        assert_eq!(gains, "1,1,1,1,1,1,1,1,1,1");

        let reverb = |engine: &mut KrkrEngine, body: &str| {
            real(
                engine,
                &format!("(function() {{ var reverb = new {FREE_VERB}(); {body} }})()"),
            )
        };
        assert_eq!(
            reverb(&mut engine, "return reverb.roomSize;"),
            f64::from(FREEVERB_DEFAULT_ROOM_SIZE)
        );
        assert_eq!(
            reverb(&mut engine, "return reverb.damping;"),
            f64::from(FREEVERB_DEFAULT_DAMPING)
        );
        assert_eq!(
            reverb(&mut engine, "return reverb.width;"),
            f64::from(FREEVERB_DEFAULT_WIDTH)
        );
        assert_eq!(
            reverb(&mut engine, "return reverb.effectMix;"),
            f64::from(FREEVERB_DEFAULT_EFFECT_MIX)
        );
        assert_eq!(
            run(
                &mut engine,
                &format!(
                    "(function() {{ var reverb = new {FREE_VERB}(); return reverb.mode; }})()"
                )
            ),
            Variant::Integer(0)
        );
    }

    /// The recovered band handling (`0x10002250`, `0x100022f0`): the setter
    /// silently ignores a band outside 0-9, the getter answers `0.0` for one,
    /// and an in-range set round-trips.
    #[test]
    fn equalizer_band_handling_matches_the_recovered_checks() {
        let mut engine = engine();
        // The reference stores the value unchanged and ignores an out-of-range
        // band (`cmp eax,0x9; ja` at 0x100022a2), and answers `0.0` for one
        // (`fldz` at 0x10002323). The values are compared as numbers: the
        // engine renders a real zero as `+0.0`, so a string compare would test
        // the formatter rather than the filter.
        let probe = |engine: &mut KrkrEngine, body: &str| {
            real(
                engine,
                &format!("(function() {{ var eq = new {EQ}(); {body} }})()"),
            )
        };
        assert_eq!(
            probe(&mut engine, "eq.setGain(3, 1.5); return eq.getGain(3);"),
            1.5
        );
        assert_eq!(
            probe(&mut engine, "eq.setGain(10, 9); return eq.getGain(10);"),
            0.0
        );
        assert_eq!(
            probe(&mut engine, "eq.setGain(-1, 9); return eq.getGain(-1);"),
            0.0
        );
        assert_eq!(
            probe(&mut engine, "eq.setGain(10, 9); return eq.getGain(4);"),
            1.0
        );
        assert_eq!(
            probe(&mut engine, "eq.setGain(-1, 9); return eq.getGain(4);"),
            1.0
        );
    }

    /// Every class answers `interface` with the id its filter is registered
    /// under with the audio backend — the port's stand-in for the reference's
    /// `iTVPBasicWaveFilter*` (`sound/WaveIntf.h:130`): per instance (not an
    /// address, so two filters of one class answer differently), stable across
    /// reads, non-zero, and read-only.
    #[test]
    fn interface_is_a_read_only_instance_id() {
        let mut engine = engine();
        let values = string(
            &mut engine,
            &format!(
                "(function() {{\n\
                     var eq = new {EQ}();\n\
                     var other = new {EQ}();\n\
                     var reverb = new {FREE_VERB}();\n\
                     var delay = new {DELAY}();\n\
                     var stable = (eq.interface === eq.interface);\n\
                     var distinct = !(eq.interface === other.interface)\n\
                         && !(eq.interface === reverb.interface)\n\
                         && !(reverb.interface === delay.interface);\n\
                     var nonNull = eq.interface !== 0 && reverb.interface !== 0 && delay.interface !== 0;\n\
                     return stable + \":\" + distinct + \":\" + nonNull;\n\
                 }})()"
            ),
        );
        assert_eq!(
            values, "1:1:1",
            "interface is a stable, per-instance, non-zero id"
        );
        let error = try_run(
            &mut engine,
            &format!("(function() {{ var eq = new {EQ}(); eq.interface = 0; return 0; }})()"),
        )
        .expect_err("interface is read-only");
        assert_eq!(
            error.message,
            "Invalid operation for Read-only or Write-only property"
        );
    }

    /// The link rows 10/11 rest on: the value `interface` publishes is the id
    /// the audio backend registered the very DSP object under
    /// (`krkr_audio::register_wave_filter`), so the engine's chain — built from
    /// the `filters` array's `interface` values — drives the filter the script
    /// configured.  `finalize` ends that id's life, so a stale value in a
    /// script's array stops resolving instead of being cast.
    #[test]
    fn the_published_interface_drives_the_scripted_filter() {
        let mut engine = engine();
        // The recovered crossfade is `dry * (1 - mix) + wet * mix`
        // (`Effect::tick`), and the wet path is silent on the very first
        // sample, so `effectMix 0.5` halves an impulse: the chain's output
        // carries the mix this script configured.
        let id = string(
            &mut engine,
            &format!(
                "(function() {{ global.reverb = new {FREE_VERB}(0.5, 0.5, 0.5, 1.0, 0, 0); return reverb.interface; }})()"
            ),
        );
        let id: i64 = id.parse().expect("interface is an integer id");

        let spec = krkr_audio::PcmAudioSpec {
            sample_rate: 44_100,
            channels: 2,
        };
        let (chain, skipped) = krkr_audio::WaveFilterChain::build(&[id], spec);
        assert!(skipped.is_empty(), "the id resolves: {skipped:?}");
        assert_eq!(chain.len(), 1);

        let mut frames = vec![0.0_f32; 32];
        frames[0] = 0.5;
        frames[1] = 0.5;
        chain.process(&mut frames);
        assert!(
            (frames[0] - 0.25).abs() < 1e-3 && (frames[1] - 0.25).abs() < 1e-3,
            "the scripted filter is the one the chain drove, got {:?}",
            &frames[..2]
        );

        engine
            .execute_script("reverb_finalize.tjs", "invalidate reverb;")
            .expect("finalize");
        assert!(
            krkr_audio::resolve_wave_filter(id).is_none(),
            "a finalized filter's id must stop resolving"
        );
        let (dropped, skipped) = krkr_audio::WaveFilterChain::build(&[id], spec);
        assert!(dropped.is_empty());
        assert_eq!(skipped.len(), 1, "a stale id is reported, not invented");
    }

    /// The shipped DLL's one-buffer rule, behaviourally: its source adapter
    /// throws `Cannot connect multiple wave sound buffer at once.`
    /// (`0x10029798`) while a source is connected, and the chain releases its
    /// filters when it drops — the reference's `Clear` — so the same filter may
    /// join the next chain but not two live ones.
    #[test]
    fn a_filter_held_by_a_live_chain_refuses_a_second_connection() {
        let mut engine = engine();
        let id: i64 = string(
            &mut engine,
            &format!("(function() {{ var v = new {FREE_VERB}(); return v.interface; }})()"),
        )
        .parse()
        .expect("interface is an integer id");
        let spec = krkr_audio::PcmAudioSpec {
            sample_rate: 44_100,
            channels: 2,
        };

        let (first, skipped) = krkr_audio::WaveFilterChain::build(&[id], spec);
        assert!(skipped.is_empty(), "the first chain connects: {skipped:?}");
        assert_eq!(first.len(), 1);

        let (second, skipped) = krkr_audio::WaveFilterChain::build(&[id], spec);
        assert!(second.is_empty(), "a second live chain must not connect");
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].reason, MULTIPLE_BUFFER_ERROR);

        drop(first);
        let (third, skipped) = krkr_audio::WaveFilterChain::build(&[id], spec);
        assert!(
            skipped.is_empty(),
            "a dropped chain releases the filter: {skipped:?}"
        );
        assert_eq!(third.len(), 1);
    }

    /// The engine↔backend seam itself: `TapPcmSource` is the only code that
    /// maps a `krkr-audio` tap window into the engine's
    /// [`plugin_api::audio::WavePcmWindow`], so this drives a real
    /// [`krkr_audio::PcmTap`] (through the process-wide handle an
    /// `AudioSystem` publishes) and pins the mapping — the `aheadsamples`
    /// lead, the format, the state and the availability count.
    #[test]
    fn the_tap_adapter_maps_the_window_the_engine_reads() {
        use krkr_engine::plugin_api::audio::{WavePcmRequest, WavePcmSource, WavePcmState};

        let _guard = lock_pcm_source();
        let system = krkr_audio::AudioSystem::new();
        let tap = krkr_audio::active_pcm_tap().expect("the system published its tap");
        let id = krkr_audio::AudioInstanceId(7171);
        let spec = krkr_audio::PcmAudioSpec {
            sample_rate: 48_000,
            channels: 2,
        };
        let feed = tap.register(id, spec);
        // Arm the ring before publishing (an unread tap ignores publishes) and
        // publish 16 frames at coordinates 0..16, both channels equal so the
        // values are easy to read back.
        let _ = tap.read(id, krkr_audio::PcmTapWindow::ahead(0));
        let samples: Vec<f32> = (0..16)
            .flat_map(|index| [index as f32, index as f32])
            .collect();
        feed.push_at(0, &samples);
        feed.set_source_position(0);

        let source = TapPcmSource;
        // The reference's window without a lead: frames 0..4.
        let window = source
            .read_window(id, WavePcmRequest::new(0, 4))
            .expect("the tap has the frames");
        assert_eq!(window.spec, spec);
        assert_eq!(window.state, WavePcmState::Playing);
        assert_eq!(
            window.samples,
            vec![0.0, 0.0, 1.0, 1.0, 2.0, 2.0, 3.0, 3.0],
            "the window starts at the cursor"
        );
        assert_eq!(window.available_frames, 4);

        // `aheadsamples`: the window starts that many frames past the cursor,
        // which is the lead the reference's ring read applies
        // (`GetVisBuffer`, `sound/win32/WaveImpl.cpp:3296-3298`).
        let window = source
            .read_window(id, WavePcmRequest::new(2, 2))
            .expect("the tap has the frames");
        assert_eq!(
            window.samples,
            vec![2.0, 2.0, 3.0, 3.0],
            "the lead is skipped, not delivered"
        );
        assert_eq!(window.available_frames, 2);

        // Past the published frames: the tap answers, the window is empty.
        let window = source
            .read_window(id, WavePcmRequest::new(40, 2))
            .expect("the instance is tapped");
        assert_eq!(window.available_frames, 0);

        feed.stop();
        drop(system);
        krkr_engine::plugin_api::audio::clear_wave_pcm_source();
    }

    /// Row-26 pin, `GraphicEqualizer`: the class entry (`0x10007fa0`) stores
    /// `AsReal(param[i])` into band `i`'s gain — the array `setGain` writes —
    /// for as long as `i <= 9` and drops every further argument
    /// (`cmp esi,0x9; ja`, `0x1000804d`), so PARQUET's `EQ(band0..band9)`
    /// wrapper configures the ten bands and nothing else.
    #[test]
    fn the_equalizer_constructor_assigns_the_band_gains() {
        let mut engine = engine();
        let probes = [(0, 0.5), (1, 1.5), (2, 2.0), (3, 1.0), (9, 1.0)];
        for (band, expected) in probes {
            let value = real(
                &mut engine,
                &format!(
                    "(function() {{ var eq = new {EQ}(0.5, 1.5, 2.0); return eq.getGain({band}); }})()"
                ),
            );
            assert!(
                (value - expected).abs() < 1e-6,
                "band {band} should be {expected}, got {value}"
            );
        }

        // Ten values, then an eleventh the DLL drops.
        let capped = [(0, 0.0), (9, 9.0)];
        for (band, expected) in capped {
            let value = real(
                &mut engine,
                &format!(
                    "(function() {{ var eq = new {EQ}(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 123.5); return eq.getGain({band}); }})()"
                ),
            );
            assert!(
                (value - expected).abs() < 1e-6,
                "an eleventh argument is dropped; band {band} should be {expected}, got {value}"
            );
        }

        // The arguments are the same field `setGain` writes.
        let after_set_gain = real(
            &mut engine,
            &format!(
                "(function() {{ var eq = new {EQ}(0.5); eq.setGain(0, 3.0); return eq.getGain(0); }})()"
            ),
        );
        assert!((after_set_gain - 3.0).abs() < 1e-6);
    }

    /// Row-26 pin, `StkFreeVerb`: the class entry (`0x10007d90`) reads up to
    /// six arguments — `effectMix`, `roomSize`, `damping`, `width`, `mode`
    /// (the integer's low byte) and `extend` — which is exactly the ini line
    /// `Verb(0.75, 0.75, 0.25, 1.0, 0, 1000)` PARQUET passes
    /// (`voiceeffect.tjs`, object 48).
    #[test]
    fn the_free_verb_constructor_applies_the_ini_line() {
        let mut engine = engine();
        let two = [("effectMix", 0.6), ("roomSize", 0.4)];
        for (member, expected) in two {
            let value = real(
                &mut engine,
                &format!(
                    "(function() {{ var v = new {FREE_VERB}(0.6, 0.4); return v.{member}; }})()"
                ),
            );
            assert!(
                (value - expected).abs() < 1e-6,
                "{member} should be {expected}, got {value}"
            );
        }

        let full = [
            ("effectMix", 0.75),
            ("roomSize", 0.75),
            ("damping", 0.25),
            ("width", 1.0),
        ];
        for (member, expected) in full {
            let value = real(
                &mut engine,
                &format!(
                    "(function() {{ var v = new {FREE_VERB}(0.75, 0.75, 0.25, 1.0, 0, 1000); return v.{member}; }})()"
                ),
            );
            assert!(
                (value - expected).abs() < 1e-6,
                "{member} should be {expected}, got {value}"
            );
        }
        let rest = string(
            &mut engine,
            &format!(
                "(function() {{ var v = new {FREE_VERB}(0.75, 0.75, 0.25, 1.0, 0, 1000); return v.mode + \":\" + v.extend; }})()"
            ),
        );
        assert_eq!(rest, "0:1000");

        // The mode argument is the integer's low byte (`movzx ecx,al`,
        // `0x10007ef4`), not "nonzero".
        let mode = string(
            &mut engine,
            &format!(
                "(function() {{\n\
                     var frozen = new {FREE_VERB}(0.5, 0.5, 0.5, 0.5, 1);\n\
                     var wrapped = new {FREE_VERB}(0.5, 0.5, 0.5, 0.5, 256);\n\
                     return frozen.mode + \":\" + wrapped.mode;\n\
                 }})()"
            ),
        );
        assert_eq!(mode, "1:0");
    }

    /// Row-26 pin, `DelayEffect`: the class entry (`0x10009300`) calls the very
    /// function the `init` member is (`0x10002360`), so the constructor's
    /// arguments are `init`'s — and `init` ignores fewer than three
    /// (`cmp ebp,0x2; jle`).  The class exposes no property for them, so the
    /// probe is the filter state itself.
    #[test]
    fn the_delay_constructor_is_init() {
        let mut engine = engine();
        // The expression returns the self-bound instance (a closure whose
        // owner is the object), so unwrap it the way the engine's readers do.
        let object = run(
            &mut engine,
            &format!("(function() {{ return new {DELAY}(120, 0.5, 0.25, 250); }})()"),
        );
        let handle = object.object_handle().expect("an object");
        let handle = engine.tjs_runtime().bound_this(handle).unwrap_or(handle);
        let probed = with_state(handle, |state| match state {
            EffectState::Delay(delay) => (
                delay.delay_millis(),
                delay.damping(),
                delay.feedback(),
                delay.max_delay_millis(),
            ),
            _ => panic!("expected a delay"),
        })
        .expect("the instance is live");
        assert!((probed.0 - 120.0).abs() < 1e-3, "delay: {probed:?}");
        assert!((probed.1 - 0.5).abs() < 1e-3, "damping pole: {probed:?}");
        assert!((probed.2 - 0.25).abs() < 1e-3, "feedback: {probed:?}");
        assert!((probed.3 - 250.0).abs() < 1e-3, "max delay: {probed:?}");

        // Fewer than three arguments leaves the defaults, exactly like `init`.
        let short = run(
            &mut engine,
            &format!("(function() {{ return new {DELAY}(120, 0.5); }})()"),
        );
        let handle = short.object_handle().expect("an object");
        let handle = engine.tjs_runtime().bound_this(handle).unwrap_or(handle);
        let untouched = with_state(handle, |state| match state {
            EffectState::Delay(delay) => (delay.delay_millis(), delay.feedback()),
            _ => panic!("expected a delay"),
        })
        .expect("the instance is live");
        let default = DelayEffect::new(44100.0);
        assert_eq!(
            untouched,
            (default.delay_millis(), default.feedback()),
            "a short list is ignored"
        );
    }

    /// The out-of-range `effectMix` behaviour recovered from
    /// `Effect::setEffectMix` (`0x1000b6a0`): clamped to 0/1 with the DLL's own
    /// warning text in the engine log.
    #[test]
    fn effect_mix_clamps_with_the_recovered_warnings() {
        let mut engine = engine();
        let probe = |engine: &mut KrkrEngine, body: &str| {
            real(
                engine,
                &format!("(function() {{ var reverb = new {FREE_VERB}(); {body} }})()"),
            )
        };
        assert_eq!(
            probe(
                &mut engine,
                "reverb.effectMix = 2.5; return reverb.effectMix;"
            ),
            1.0,
            "a mix above one is clamped to one"
        );
        assert_eq!(
            probe(
                &mut engine,
                "reverb.effectMix = -3; return reverb.effectMix;"
            ),
            0.0,
            "a negative mix is clamped to zero"
        );
        assert_eq!(
            probe(
                &mut engine,
                "reverb.effectMix = 0.25; return reverb.effectMix;"
            ),
            0.25
        );
        let logs = engine.host().logs().join("\n");
        assert!(
            logs.contains(
                "Effect::setEffectMix: mix parameter is greater than 1.0 ... setting to one!"
            ),
            "missing the recovered high warning: {logs}"
        );
        assert!(
            logs.contains(
                "Effect::setEffectMix: mix parameter is less than zero ... setting to zero!"
            ),
            "missing the recovered low warning: {logs}"
        );
    }

    /// `DelayEffect.init` is the recovered variadic call (`0x10002360`):
    /// fewer than three arguments is a no-op, three set the effect, and the
    /// optional fourth overrides the maximum delay. A pole outside `(-1, 1)`
    /// is refused with the recovered `stk::OnePole` warning in the engine log.
    #[test]
    fn delay_init_ignores_a_short_list_and_warns_on_a_bad_pole() {
        let mut engine = engine();
        let value = string(
            &mut engine,
            &format!(
                "(function() {{\n\
                     var delay = new {DELAY}();\n\
                     delay.init(100, 0.5);\n\
                     var early = \"ok\";\n\
                     delay.init(100, 0.5, 0.25, 250);\n\
                     delay.init(25, 2.0, 0.5);\n\
                     return early;\n\
                 }})()"
            ),
        );
        assert_eq!(value, "ok");
        assert!(
            engine
                .host()
                .logs()
                .iter()
                .any(|line| line.contains("OnePole::setPole")),
            "the recovered one-pole warning must reach the log"
        );
    }

    /// A filter object added to a `WaveSoundBuffer.filters` array survives —
    /// the one script-visible half of the reference contract — and the Rust
    /// side can resolve it by identity for processing (the dossier's
    /// pointer-free mapping).  The member itself is read-only
    /// (`TJS_DENY_NATIVE_PROP_SETTER`, `WaveIntf.cpp:1552`), so the script
    /// fills the buffer's own array instead of replacing the member.
    #[test]
    fn filters_array_keeps_the_object_and_identity_resolves_it() {
        let mut engine = engine();
        let handle = run(
            &mut engine,
            &format!(
                "(function() {{\n\
                     var buffer = new WaveSoundBuffer();\n\
                     buffer.filters.clear();\n\
                     buffer.filters.add(new {EQ}());\n\
                     buffer.filters.add(new {FREE_VERB}());\n\
                     global.attached = buffer.filters;\n\
                     return buffer.filters.count;\n\
                 }})()"
            ),
        );
        assert_eq!(handle, Variant::Integer(2));
        let object = run(&mut engine, "global.attached[0]")
            .object_handle()
            .expect("object");
        let mut frames = [1.0_f32, -1.0, 0.5, -0.5];
        assert!(
            matches!(
                process_filter(object, &mut frames, 2),
                Some(FilterHandle::Equalizer)
            ),
            "the stored object resolves to its live filter state"
        );
    }

    // --------------------------------------------------------------- DSP

    /// An impulse response of `length` frames through `process`.
    fn impulse_response<F>(length: usize, channels: usize, mut process: F) -> Vec<f32>
    where
        F: FnMut(&mut [f32]),
    {
        let mut frames = vec![0.0_f32; length * channels];
        frames[0] = 1.0;
        process(&mut frames);
        frames
    }

    /// The magnitude of the transfer function at `frequency`, from an impulse
    /// response (a single-frequency DFT bin, `sum x[n] e^{-i 2 pi f n / fs}`).
    fn magnitude_at(response: &[f32], sample_rate: f32, frequency: f32) -> f32 {
        let step = -2.0 * std::f32::consts::PI * frequency / sample_rate;
        let (mut real, mut imaginary) = (0.0_f32, 0.0_f32);
        for (index, value) in response.iter().enumerate() {
            let phase = step * index as f32;
            real += value * phase.cos();
            imaginary += value * phase.sin();
        }
        (real * real + imaginary * imaginary).sqrt()
    }

    /// A flat equalizer is a passthrough up to the coefficient round-off:
    /// with `A = 1` the peaking section's numerator and denominator coincide
    /// exactly in `b1`/`b2` vs `a1`/`a2`, and `b0 = (1 + alpha) / (1 + alpha)`
    /// lands within one ULP of one — which is all f32 arithmetic allows.
    #[test]
    fn a_flat_equalizer_is_a_passthrough() {
        let mut equalizer = GraphicEqualizer::new(44100.0);
        let input: Vec<f32> = (0..256).map(|index| (index as f32 * 0.1).sin()).collect();
        let mut frames = input.clone();
        equalizer.process(&mut frames, 1);
        for (processed, original) in frames.iter().zip(input.iter()) {
            assert!(
                (processed - original).abs() <= original.abs() * 1e-5 + 1e-7,
                "flat band moved a sample: {processed} vs {original}"
            );
        }
    }

    /// Boosting one band by the linear factor 2.0 doubles the response at that
    /// band's centre frequency and leaves the far end of the spectrum alone
    /// (one-octave peaking biquads), which pins the recovered band table.
    #[test]
    fn an_equalizer_boost_scales_its_band_centre() {
        let mut equalizer = GraphicEqualizer::new(44100.0);
        equalizer.set_gain(4, 2.0); // band 4 is 1 kHz (0x1002928c in .rdata)
        let response = impulse_response(8192, 1, |frames| equalizer.process(frames, 1));
        let at_band = magnitude_at(&response, 44100.0, EQ_BAND_FREQUENCIES[4]);
        assert!(
            (at_band - 2.0).abs() < 0.05,
            "1 kHz response is {at_band}, expected the gain 2.0"
        );
        let at_dc = magnitude_at(&response, 44100.0, 0.0);
        let at_nyquist = magnitude_at(&response, 44100.0, 22050.0);
        assert!((at_dc - 1.0).abs() < 0.05, "DC response is {at_dc}");
        assert!(
            (at_nyquist - 1.0).abs() < 0.05,
            "Nyquist response is {at_nyquist}"
        );
    }

    /// A step through a boosted band stays bounded and settles — the state is
    /// a stable direct-form-II biquad bank, and the denormal flush keeps the
    /// state exactly zero once the step has passed.
    #[test]
    fn an_equalizer_step_response_settles_and_flushes_denormals() {
        let mut equalizer = GraphicEqualizer::new(44100.0);
        equalizer.set_gain(9, 3.0);
        let mut frames = vec![1.0_f32; 8192];
        equalizer.process(&mut frames, 1);
        assert!(
            frames.iter().all(|value| value.is_finite()),
            "the bank blew up"
        );
        let tail = &frames[8000..];
        assert!(
            tail.iter().all(|value| (*value - tail[0]).abs() < 1e-3),
            "a step through a stable peaking bank must settle"
        );
    }

    /// The recovered FreeVerb `effectMix` handling: zero is a dry passthrough,
    /// and the port's own crossfade follows `dry * (1 - mix) + wet * mix`.
    #[test]
    fn free_verb_effect_mix_zero_is_a_passthrough() {
        let mut reverb = FreeVerb::new(44100.0);
        assert_eq!(reverb.set_effect_mix(0.0), None);
        let input: Vec<f32> = (0..512).map(|index| (index as f32 * 0.05).cos()).collect();
        let mut frames = input.clone();
        reverb.process(&mut frames, 1);
        assert_eq!(frames, input);
    }

    /// A buffer whose length is not a multiple of the channel count is
    /// tolerated instead of panicking: the trailing partial frame is processed
    /// like the sibling processors do, and nothing indexes past the buffer.
    #[test]
    fn a_partial_trailing_frame_does_not_panic() {
        // Five samples at two channels: two whole frames plus one odd sample.
        // The wet path is silent for the first 1115 samples, so every output
        // sample is exactly the dry half — including the odd one, which pins
        // that the odd sample really is processed (as a mono frame) rather
        // than skipped or over-read.
        let mut reverb = FreeVerb::new(44100.0);
        reverb.set_effect_mix(0.5);
        let mut frames = [0.25_f32; 5];
        reverb.process(&mut frames, 2);
        for value in frames {
            assert!(
                (value - 0.125).abs() < 1e-6,
                "every sample is the dry half, got {value}"
            );
        }

        // The sibling processors tolerate the same short tail.
        let mut equalizer = GraphicEqualizer::new(44100.0);
        let mut delay = DelayEffect::new(44100.0);
        let mut tail = [0.75_f32, 0.5];
        equalizer.process(&mut tail, 3);
        delay.process(&mut tail, 3);
        assert!(tail.iter().all(|value| value.is_finite()));
    }

    /// The FreeVerb impulse response with the wet path only: silence until the
    /// shortest comb delay (the recovered tuning 1116 samples at 44.1 kHz
    /// scaled by `sampleRate / 44100`), then a live tail.
    #[test]
    fn free_verb_tail_starts_at_the_recovered_comb_delay() {
        let mut reverb = FreeVerb::new(44100.0);
        reverb.set_effect_mix(1.0);
        let response = impulse_response(8192, 1, |frames| reverb.process(frames, 1));
        let first_nonzero = response
            .iter()
            .position(|value| *value != 0.0)
            .expect("the reverb must produce a tail");
        assert_eq!(
            first_nonzero, FREEVERB_COMB_TUNINGS[0],
            "the first wet sample must leave the 1116-sample comb (0x1003426c)"
        );
        assert!(
            response[FREEVERB_COMB_TUNINGS[0]..]
                .iter()
                .any(|value| value.abs() > 1e-6),
            "the tail must carry energy"
        );
    }

    /// The comb scaling recovered from the DLL: feedback is `0.28 * roomSize +
    /// 0.7` and the damping coefficient is `0.4 * damping` (the constants at
    /// `0x1002a110`/`0x1002a118`/`0x1002a108`), and the one-pole damping store
    /// is `output * damp2 + store * damp1` with `damp2 = 1 - damp1`.
    #[test]
    fn free_verb_comb_feedback_and_damping_follow_the_recovered_scaling() {
        let mut reverb = FreeVerb::new(44100.0);
        reverb.set_room_size(0.5);
        reverb.set_damping(0.1);
        reverb.set_effect_mix(1.0);
        let comb = &reverb.combs_left[0];
        let expected_feedback = 0.5 * FREEVERB_SCALE_ROOM + FREEVERB_OFFSET_ROOM;
        assert!((comb.feedback - expected_feedback).abs() < 1e-6);
        assert!((comb.damp1 - 0.1 * FREEVERB_SCALE_DAMP).abs() < 1e-6);
        assert!((comb.damp2 - (1.0 - comb.damp1)).abs() < 1e-6);

        // A four-sample comb given an impulse echoes it after four samples and
        // writes `output * feedback` back, so the third echo carries the
        // feedback twice.
        let mut comb = FreeVerbComb::new(4);
        comb.feedback = 0.5;
        comb.damp1 = 0.0;
        comb.damp2 = 1.0;
        let mut output = [0.0_f32; 12];
        output[0] = comb.process(1.0);
        for value in output.iter_mut().skip(1) {
            *value = comb.process(0.0);
        }
        assert_eq!(output[0..4], [0.0; 4], "the delay is four samples");
        assert_eq!(output[4], 1.0);
        assert_eq!(output[8], 0.5);
    }

    /// The first wet sample of the whole reverb is the input impulse scaled by
    /// the recovered `fixedGain` `0.015` (`0x1002a0f0`), passed through the
    /// four allpasses' direct terms (four sign flips, so it comes out
    /// positive).
    #[test]
    fn free_verb_first_wet_sample_carries_the_fixed_gain() {
        let mut reverb = FreeVerb::new(44100.0);
        reverb.set_effect_mix(1.0);
        let response = impulse_response(8192, 1, |frames| reverb.process(frames, 1));
        let first = response[FREEVERB_COMB_TUNINGS[0]];
        assert!(
            (first - FREEVERB_FIXED_GAIN).abs() < 1e-6,
            "the first wet sample is {first}, expected fixedGain {}",
            FREEVERB_FIXED_GAIN
        );
    }

    /// The tail decays to exact zero (the reference's `undenormalise` flush)
    /// instead of leaving subnormal state behind. The longest comb (1617
    /// samples) decays by the 0.7 feedback per round trip, so the tail needs
    /// several seconds of samples before it can flush.
    #[test]
    fn free_verb_tail_flushes_to_exact_zero() {
        let mut reverb = FreeVerb::new(44100.0);
        reverb.set_effect_mix(1.0);
        reverb.set_room_size(0.0); // comb feedback 0.7, the shortest decay
        reverb.set_damping(0.5);
        let mut frames = vec![0.0_f32; 44100 * 20];
        frames[0] = 1.0;
        reverb.process(&mut frames, 1);
        let tail = &frames[44100 * 19..];
        assert!(
            tail.iter().all(|value| *value == 0.0),
            "the reverb tail must flush to exact zero, last sample {}",
            tail.last().copied().unwrap_or(1.0)
        );
    }

    /// The float-source checks of the recovered adapter (`0x10004e98`,
    /// `0x10004da0`) and its single-buffer error (`0x10004e75`).
    #[test]
    fn the_recovered_format_checks_reject_the_same_inputs() {
        assert_eq!(check_source_format(32, 4), Ok(()));
        assert_eq!(check_source_format(16, 2), Ok(()));
        assert_eq!(
            check_source_format(32, 5),
            Err("HiRes format not supported.")
        );
        assert_eq!(
            check_source_format(64, 2),
            Err("HiRes format not supported.")
        );
        assert_eq!(check_filter_channels(1), Ok(()));
        assert_eq!(check_filter_channels(2), Ok(()));
        assert_eq!(check_filter_channels(0), Err("invalid channels."));
        assert_eq!(check_filter_channels(3), Err("invalid channels."));
        assert_eq!(
            MULTIPLE_BUFFER_ERROR,
            "Cannot connect multiple wave sound buffer at once."
        );

        // The DSP itself only accepts the channel counts the adapter asks for.
        let mut equalizer = GraphicEqualizer::new(44100.0);
        let mut stereo = [1.0_f32, -1.0];
        equalizer.process(&mut stereo, 2);
        assert!(stereo.iter().all(|value| value.is_finite()));
    }

    /// The one-pole damping stage of the delay effect is a real filter: its
    /// pole is clamped below 1.0 (`stk::OnePole::setPole`), and the echo
    /// decays by the feedback factor on each round trip. With the damping
    /// bypassed (pole 0, `b0 = 1`) the echoes are exact multiples of the
    /// impulse and the feedback.
    #[test]
    fn delay_echo_decays_by_the_feedback_factor() {
        let mut delay = DelayEffect::new(44100.0);
        assert!(delay.set_damping(1.0).is_some(), "a pole of 1.0 is refused");
        assert_eq!(delay.set_damping(0.0), None);
        delay.set_feedback(0.5);
        assert_eq!(delay.set_delay_millis(100.0), None);
        assert_eq!(delay.delay_millis(), 100.0);

        let echo_samples = 4410; // 100 ms at 44.1 kHz
        let response = impulse_response(echo_samples * 3, 1, |frames| delay.process(frames, 1));
        assert_eq!(response[0], 1.0, "the dry impulse passes");
        assert!(
            (response[echo_samples] - 1.0).abs() < 1e-6,
            "the first echo is the delayed impulse, got {}",
            response[echo_samples]
        );
        assert!(
            (response[echo_samples * 2] - 0.5).abs() < 1e-6,
            "the second echo carries the feedback factor, got {}",
            response[echo_samples * 2]
        );
        assert!(
            response[1..echo_samples].iter().all(|value| *value == 0.0),
            "nothing between the impulse and its first echo"
        );
    }

    /// The one-pole damping stage scales the first echo by `b0 = 1 - pole`
    /// (`stk::OnePole`, one pole at `pole`): a pole of 0.5 halves it.
    #[test]
    fn the_damping_pole_shapes_the_echo() {
        let mut delay = DelayEffect::new(44100.0);
        assert_eq!(delay.set_damping(0.5), None);
        delay.set_feedback(0.5);
        assert_eq!(delay.set_delay_millis(100.0), None);
        let echo_samples = 4410;
        let response = impulse_response(echo_samples * 2, 1, |frames| delay.process(frames, 1));
        assert!(
            (response[echo_samples] - 0.5).abs() < 1e-6,
            "a 0.5 pole scales the echo by b0 = 0.5, got {}",
            response[echo_samples]
        );
    }

    /// A refused damping pole is inert, as STK's `OnePole::setPole` is: the
    /// warning is reported but neither the live coefficients nor the value the
    /// getter answers change.
    #[test]
    fn a_refused_damping_pole_leaves_the_state_alone() {
        let mut delay = DelayEffect::new(44100.0);
        assert_eq!(delay.set_damping(0.5), None);
        let warning = delay.set_damping(2.0);
        assert!(warning.is_some_and(|text| text.contains("OnePole::setPole")));
        assert_eq!(
            delay.damping(),
            0.5,
            "the refusal keeps the previous pole, not the refused one"
        );

        // The audio still carries the 0.5 pole's shaping: b0 = 0.5 scales the
        // echo by half.
        delay.set_feedback(0.5);
        assert_eq!(delay.set_delay_millis(100.0), None);
        let echo_samples = 4410;
        let response = impulse_response(echo_samples * 2, 1, |frames| delay.process(frames, 1));
        assert!(
            (response[echo_samples] - 0.5).abs() < 1e-6,
            "the refused call must not change the running filter, got {}",
            response[echo_samples]
        );
    }

    /// A delay past the recovered maximum is refused with the DLL's
    /// `stk::Delay::setDelay` warning (`0x1002a47c`).
    #[test]
    fn a_delay_past_the_maximum_is_refused() {
        let mut delay = DelayEffect::new(44100.0);
        let warning = delay.set_delay_millis(DELAY_DEFAULT_MAX_MILLIS + 1.0);
        assert!(warning.is_some_and(|text| text.contains("Delay::setDelay")));
        assert_eq!(delay.delay_millis(), 0.0, "the refused delay is not stored");
    }

    /// The engine's default sample rate is what instances are built for, and
    /// the recovered `+0xc4` slot is kept for the layout claim.
    #[test]
    fn instance_sample_rate_and_the_unrecovered_slot_are_kept() {
        let delay = DelayEffect::new(44100.0);
        assert_eq!(delay.max_delay_millis(), DELAY_DEFAULT_MAX_MILLIS);
        assert!((delay.unrecovered_field() - 1.8125).abs() < f32::EPSILON);
    }
}
