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
//! carrying a `WaveSoundBuffer` basename in its chain: `StkFreeVerb`,
//! `GraphicEqualizer` and `DelayEffect` (the C++ class is `WaveDelay`, the TJS
//! class name is `DelayEffect` — wide strings at `0x10029a24`, `0x100299c0`,
//! `0x1002999c`), each with an `interface` property. `SimpleWaveFilter<…>`
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
//! **Not reachable from the audio path yet**: the engine's `WaveSoundBuffer`
//! owns a per-instance `filters` array (read-only member — the reference's
//! `TJSCreateArrayObject` at `sound/WaveIntf.cpp:815` with a denied setter at
//! `:1552`) but neither reads it nor carries a filter chain through
//! `AudioCommand` (`crates/krkr-core/src/lib.rs:665` has no filter payload),
//! so nothing calls [`FreeVerb::process`] while a buffer plays. The filters'
//! `interface` property returns a **sentinel** integer instead of the
//! reference's raw `iTVPBasicWaveFilter*`: the missing engine seam is filed as
//! a finding, and [`GraphicEqualizer::process`] / [`FreeVerb::process`] /
//! [`DelayEffect::process`] are the entry points a future chain would call.

// The DSP types and their `process`/`reset` entry points are the module's
// processing contract: nothing in the engine calls them until the
// `WaveSoundBuffer` filter chain exists (see the last paragraph of the module
// docs), and the numeric tests drive them directly. The unused-item lint is
// silenced for the module so the ported coefficients and their API stay
// together instead of being trimmed to the script surface. `result_large_err`
// is the crate-wide `TjsError` size lint every native callback carries.
#![allow(dead_code)]
#![allow(clippy::result_large_err)]

use std::cell::RefCell;
use std::collections::BTreeMap;

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativePropertyAccess, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "GraphicEqualizer / StkFreeVerb / DelayEffect filters on WaveSoundBuffer",
    notes: "Real DSP (10-band peaking EQ, FreeVerb, damped feedback delay) with the recovered \
            member surface and parameter ranges, processing interleaved f32 PCM; the classes \
            install on the global object like the reference. The engine has no per-buffer filter \
            chain yet (`WaveSoundBuffer.filters` is the buffer's own read-only array and nothing \
            consumes it, and `AudioCommand` carries no filter payload), so the filters run only \
            through their Rust `process` entry points until that seam exists — `interface` answers \
            a sentinel integer instead of a raw pointer. See the module docs for the re-derived \
            constants and the parts that are inferred.",
    install: |engine| engine.register_plugin(WfBasicEffectPlugin),
};

pub struct WfBasicEffectPlugin;

impl KrkrPlugin for WfBasicEffectPlugin {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_wf_basic_effect(runtime);
        runtime
            .host_mut()
            .log("wfBasicEffect.dll registered: GraphicEqualizer / StkFreeVerb / DelayEffect (DSP local; no engine filter chain yet)");
        Ok(())
    }
}

/// The canonical DLL name: what `Plugins.link` matches.
const PLUGIN_NAME: &str = "wfBasicEffect.dll";

/// The sentinel the `interface` property answers with.
///
/// The reference hands the engine a raw `iTVPBasicWaveFilter*` cast to an
/// integer (`PhaseVocoderFilter.cpp:54`); a Rust engine cannot publish a
/// pointer to a script-visible value, so the dossier's porting outline maps
/// object identity to the filter instead and this is the non-null marker the
/// script-side property answers. Each class gets its own value so a future
/// chain can tell them apart before it resolves the object.
const INTERFACE_SENTINEL_EQ: i64 = 0x5746_0001; // "WF" + GraphicEqualizer
const INTERFACE_SENTINEL_FV: i64 = 0x5746_0002; // "WF" + StkFreeVerb
const INTERFACE_SENTINEL_DL: i64 = 0x5746_0003; // "WF" + DelayEffect

// ---------------------------------------------------------------------------
// Per-instance state
// ---------------------------------------------------------------------------

/// One live filter instance. The TJS object owns the parameters; this is the
/// processing state a future engine filter chain would look up by identity.
enum EffectState {
    Equalizer(Box<GraphicEqualizer>),
    FreeVerb(FreeVerb),
    Delay(DelayEffect),
}

thread_local! {
    /// Filter instances by object handle. Entries are dropped by `finalize`,
    /// the convention the other plugin modules in this crate use.
    static FILTERS: RefCell<BTreeMap<ObjectHandle, EffectState>> =
        const { RefCell::new(BTreeMap::new()) };
}

fn with_state<R>(handle: ObjectHandle, f: impl FnOnce(&mut EffectState) -> R) -> Option<R> {
    FILTERS.with(|filters| filters.borrow_mut().get_mut(&handle).map(f))
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

    /// `setMode`: freeze the tail.
    pub fn set_mode(&mut self, frozen: bool) {
        self.frozen = frozen;
        self.apply_controls();
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

/// `GraphicEqualizer`: `new GraphicEqualizer()`, `setGain(band, gain)`,
/// `getGain(band)` and the read-only `interface` property.
fn install_equalizer_class(runtime: &mut Runtime<KrkrHost>) {
    let class = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, _this: Option<ObjectHandle>, _args: Vec<Variant>| {
            let instance = new_plugin_instance(runtime, "GraphicEqualizer");
            let sample_rate = native_sample_rate(runtime);
            FILTERS.with(|filters| {
                filters.borrow_mut().insert(
                    instance,
                    EffectState::Equalizer(Box::new(GraphicEqualizer::new(sample_rate))),
                );
            });
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(class, "GraphicEqualizer");
    install_equalizer_members(runtime, class);
    runtime.set_global_member("GraphicEqualizer", Variant::Object(class));
}

fn install_equalizer_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "setGain", equalizer_set_gain);
    runtime.register_object_native(handle, "getGain", equalizer_get_gain);
    runtime.register_object_native(handle, "finalize", plugin_finalize);
    runtime.register_object_native_property_with_access(
        handle,
        "interface",
        NativePropertyAccess::ReadOnly,
        |_runtime, _this| Ok(Variant::Integer(INTERFACE_SENTINEL_EQ)),
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
        |runtime: &mut Runtime<KrkrHost>, _this: Option<ObjectHandle>, _args: Vec<Variant>| {
            let instance = new_plugin_instance(runtime, "StkFreeVerb");
            let sample_rate = native_sample_rate(runtime);
            FILTERS.with(|filters| {
                filters
                    .borrow_mut()
                    .insert(instance, EffectState::FreeVerb(FreeVerb::new(sample_rate)));
            });
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(class, "StkFreeVerb");
    install_free_verb_members(runtime, class);
    runtime.set_global_member("StkFreeVerb", Variant::Object(class));
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
    // its return value was not recovered, so it answers the same sentinel as
    // `interface` here.
    runtime.register_object_native_property_with_access(
        handle,
        "extend",
        NativePropertyAccess::ReadOnly,
        |_runtime, _this| Ok(Variant::Integer(INTERFACE_SENTINEL_FV)),
        |_runtime, _this, _value| Err(TjsError::access_denied()),
    );
    runtime.register_object_native_property_with_access(
        handle,
        "interface",
        NativePropertyAccess::ReadOnly,
        |_runtime, _this| Ok(Variant::Integer(INTERFACE_SENTINEL_FV)),
        |_runtime, _this, _value| Err(TjsError::access_denied()),
    );
}

/// `DelayEffect`: the variadic `init(delay, damping, feedback, maxDelay?)` and
/// the read-only `interface` property.
fn install_delay_class(runtime: &mut Runtime<KrkrHost>) {
    let class = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, _this: Option<ObjectHandle>, _args: Vec<Variant>| {
            let instance = new_plugin_instance(runtime, "DelayEffect");
            let sample_rate = native_sample_rate(runtime);
            FILTERS.with(|filters| {
                filters
                    .borrow_mut()
                    .insert(instance, EffectState::Delay(DelayEffect::new(sample_rate)));
            });
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(class, "DelayEffect");
    install_delay_members(runtime, class);
    runtime.set_global_member("DelayEffect", Variant::Object(class));
}

fn install_delay_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", plugin_finalize);
    runtime.register_object_native(handle, "init", delay_init);
    runtime.register_object_native_property_with_access(
        handle,
        "interface",
        NativePropertyAccess::ReadOnly,
        |_runtime, _this| Ok(Variant::Integer(INTERFACE_SENTINEL_DL)),
        |_runtime, _this, _value| Err(TjsError::access_denied()),
    );
}

/// `init(p0, p1, p2, p3?)`: the recovered argument handling of
/// `WaveDelay::init` (`0x10002360`) — fewer than three arguments is a no-op,
/// the fourth is optional.
fn delay_init(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = plugin_this(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    if args.len() < 3 {
        return Ok(Variant::Void);
    }
    let delay = args[0].to_integer()? as f32;
    let damping = args[1].to_real()? as f32;
    let feedback = args[2].to_real()? as f32;
    let max_delay = match args.get(3) {
        Some(value) if !matches!(value, Variant::Void) => Some(value.to_integer()? as f32),
        _ => None,
    };
    let warnings = with_state(this, |state| {
        let EffectState::Delay(delay_effect) = state else {
            return Vec::new();
        };
        let mut warnings = Vec::new();
        if let Some(warning) = delay_effect.set_damping(damping) {
            warnings.push(warning);
        }
        delay_effect.set_feedback(feedback);
        if let Some(max_delay) = max_delay {
            delay_effect.set_max_delay_millis(max_delay);
        }
        if let Some(warning) = delay_effect.set_delay_millis(delay) {
            warnings.push(warning);
        }
        warnings
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

/// A fresh instance object of a plugin class: class info, `__className` (the
/// value a future engine-side filter chain resolves by identity), and the
/// superclass link to the class object.
fn new_plugin_instance(runtime: &mut Runtime<KrkrHost>, class_name: &'static str) -> ObjectHandle {
    let instance = runtime.alloc_ordinary_object();
    runtime.add_object_class_info(instance, class_name);
    runtime.set_object_member(
        instance,
        "__className",
        Variant::String(class_name.to_string()),
    );
    if let Variant::Object(class) = runtime.global_member(class_name) {
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
        FILTERS.with(|filters| filters.borrow_mut().remove(&this));
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

    /// The recovered member set of every class (module docs, binder chain
    /// `0x10009390`): the class must expose each one.
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
                &format!("(function() {{ return typeof {class}.{member}; }})()"),
            );
            assert_ne!(probe, "undefined", "{class}.{member} is not installed");
        }
    }

    /// The defaults on a fresh instance: the ten `1.0` band gains, the
    /// recovered FreeVerb constants, and the DelayEffect slots.
    #[test]
    fn instance_defaults_match_the_recovered_constants() {
        let mut engine = engine();
        let gains = string(
            &mut engine,
            "(function() {\n\
                 var eq = new GraphicEqualizer();\n\
                 var out = [];\n\
                 for (var band = 0; band < 10; band = band + 1) out.push(eq.getGain(band));\n\
                 return out.join(\",\");\n\
             })()",
        );
        assert_eq!(gains, "1,1,1,1,1,1,1,1,1,1");

        assert_eq!(
            real(
                &mut engine,
                "(function() { var reverb = new StkFreeVerb(); return reverb.roomSize; })()"
            ),
            f64::from(FREEVERB_DEFAULT_ROOM_SIZE)
        );
        assert_eq!(
            real(
                &mut engine,
                "(function() { var reverb = new StkFreeVerb(); return reverb.damping; })()"
            ),
            f64::from(FREEVERB_DEFAULT_DAMPING)
        );
        assert_eq!(
            real(
                &mut engine,
                "(function() { var reverb = new StkFreeVerb(); return reverb.width; })()"
            ),
            f64::from(FREEVERB_DEFAULT_WIDTH)
        );
        assert_eq!(
            real(
                &mut engine,
                "(function() { var reverb = new StkFreeVerb(); return reverb.effectMix; })()"
            ),
            f64::from(FREEVERB_DEFAULT_EFFECT_MIX)
        );
        assert_eq!(
            run(
                &mut engine,
                "(function() { var reverb = new StkFreeVerb(); return reverb.mode; })()"
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
                &format!("(function() {{ var eq = new GraphicEqualizer(); {body} }})()"),
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

    /// Every class answers `interface` (and StkFreeVerb its `extend`) with the
    /// non-null sentinel the port documents, and the properties are read-only.
    #[test]
    fn interface_is_a_read_only_sentinel() {
        let mut engine = engine();
        let values = string(
            &mut engine,
            "(function() {\n\
                 var eq = new GraphicEqualizer();\n\
                 var reverb = new StkFreeVerb();\n\
                 var delay = new DelayEffect();\n\
                 return eq.interface + \":\" + reverb.interface + \":\" + reverb.extend + \":\" + delay.interface;\n\
             })()",
        );
        assert_eq!(
            values,
            format!(
                "{INTERFACE_SENTINEL_EQ}:{INTERFACE_SENTINEL_FV}:{INTERFACE_SENTINEL_FV}:{INTERFACE_SENTINEL_DL}"
            )
        );
        let error = try_run(
            &mut engine,
            "(function() { var eq = new GraphicEqualizer(); eq.interface = 0; return 0; })()",
        )
        .expect_err("interface is read-only");
        assert_eq!(
            error.message,
            "Invalid operation for Read-only or Write-only property"
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
                &format!("(function() {{ var reverb = new StkFreeVerb(); {body} }})()"),
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
            "(function() {\n\
                 var delay = new DelayEffect();\n\
                 delay.init(100, 0.5);\n\
                 var early = \"ok\";\n\
                 delay.init(100, 0.5, 0.25, 250);\n\
                 delay.init(25, 2.0, 0.5);\n\
                 return early;\n\
             })()",
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
            "(function() {\n\
                 var buffer = new WaveSoundBuffer();\n\
                 buffer.filters.clear();\n\
                 buffer.filters.add(new GraphicEqualizer());\n\
                 buffer.filters.add(new StkFreeVerb());\n\
                 global.attached = buffer.filters;\n\
                 return buffer.filters.count;\n\
             })()",
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
