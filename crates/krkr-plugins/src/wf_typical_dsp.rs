//! `wfTypicalDSP.dll` — the `WaveDSPFilter` WaveSoundBuffer filter.
//!
//! The DLL ships without source (`/home/ruri/games/PARQUET/PARQUET/plugin/
//! wfTypicalDSP.dll`, PE32 i386, 1 183 232 B); the dossier is
//! `docs/plugins/wfTypicalDSP.md`. Ghidra and Java are not installed on this
//! machine, so the dossier's headless recipe cannot run: the recovered names
//! below were re-checked with `objdump`/`strings` against the shipped image,
//! and the deep structure claims stay the dossier's.
//!
//! # The recovered surface
//!
//! * One native class, `WaveDSPFilter` (RTTI `tTJSNC_WaveDSPFilter` /
//!   `tTJSNI_WaveDSPFilter`), registered against a `WaveSoundBuffer` with the
//!   same single-buffer invariant as `wfBasicEffect`
//!   (`TVPCannotConnectMultipleWaveSoundBufferAtOnce`). "Against" is literal:
//!   the binder chain constructs the `WaveSoundBuffer` wide string (VA
//!   `0x100d76a8`, at `0x100a557c`) right after the class name
//!   (`0x100cbcc0`, at `0x100a554d`) and links the created class object into
//!   *that* object, exactly like `wfBasicEffect`'s three chains — so the class
//!   lives at `WaveSoundBuffer.WaveDSPFilter` and no global `WaveDSPFilter`
//!   exists. PARQUET reads it there: `voiceeffect.tjs` object 2 (`DSP`) does
//!   `gpd WaveSoundBuffer`, `gpd .WaveDSPFilter`, `new`.
//! * Members referenced by the registration: `name`, `label`, `getParamInfo`,
//!   `setParams`, `currentValue`, `defaultValue`, `interface`, `finalize`.
//! * The DLL's own parameter vocabulary. Its identifier strings are the
//!   **space-less** UTF-16 names (`strings -el`): the responses `LowPass`,
//!   `HighPass`, `BandPass`, `BandStop`, `LowShelf`, `HighShelf`, `BandShelf`
//!   (plus `AllPass`, `BandPass1`, `BandPass2`, which this port recognises but
//!   does not model) and the designs `Bessel`, `Butterworth`, `ChebyshevI`,
//!   `ChebyshevII`, `Chebyshev1`, `Chebyshev2`, `Elliptic`, `Legendre`, `RBJ`,
//!   `Custom`, `OnePole`, `TwoPole`. The *spaced* spellings
//!   (`Low Pass`, `Chebyshev I`, …) occur in the binary in neither encoding —
//!   they are this port's readable canonical values (the dossier's
//!   transcription), and the space-less identifiers are accepted as aliases of
//!   them. The only spaced strings in the image are the parameter **labels**
//!   (`Custom One-Pole`, `Custom Two-Pole`, `Center Frequency`,
//!   `Cutoff Frequency`, `Bandwidth (Hz)`, `Bandwidth (Octaves)`, `Order`,
//!   `Pole Angle`, `Pole Distance`, `Pole Real`), which this port reproduces
//!   in its parameter table. The DLL's template instances
//!   (`Dsp::FilterDesign<Dsp::LowPass … Design::{Bessel,Butterworth,
//!   ChebyshevI,ChebyshevII,Elliptic,Legendre,RBJ}, DirectFormI/II,
//!   TransposedDirectFormI/II>`) are Vinnie Falco's DSPFilters library, which
//!   is what the response/design matrix above is.
//! * Error strings: `invalid usage of ParamInfo`, `attempt to process/reset
//!   empty ChannelState`, `ClassID mismatched:`.
//! * Processing is float PCM (`TVPConvertPCMToFloat`), i.e. the same
//!   interleaved `f32` the engine feeds its audio backend.
//!
//! # The parameter model
//!
//! The reference exposes parameters as `ParamInfo` records; the dossier
//! records that the DLL builds them with `TJSCreateDictionaryObject` and that
//! the *field* names are an open question. This port uses the class's own
//! member names as the record fields — `name`, `label`, `currentValue`,
//! `defaultValue` — which is the reading `getParamInfo`/`currentValue`/
//! `defaultValue` support, and picks parameter names (`type`, `design`,
//! `cutoff`, …) for the labels the DLL's strings carry. The parameter *names*
//! are this port's choice; the labels and the design/response vocabularies are
//! recovered. `getParamInfo` accepts an index or a name and answers the record
//! dictionary; `setParams` accepts the reference's positional form plus a
//! dictionary or name/value pairs (the port's own extension, checked first,
//! see below), and an unknown name or a malformed argument raises the
//! recovered `invalid usage of ParamInfo` message.
//!
//! # `setParams`: the reference's positional/void-slot contract
//!
//! The reference's `setParams` is **positional**, not name-based. The member
//! wrapper (VA `0x10001e60`, registered at `0x10001d32` with
//! `TJSNativeClassRegisterNCM`) resolves the native instance and calls
//! `0x10002a30`, which writes the arguments into eight doubles at
//! `instance+0x80` (`fstp QWORD PTR [edi+esi*8+0x80]`, `0x10002b15`). That
//! array is DSPFilters' `Params` (`double value[8]`, `maxParameters = 8`,
//! `shared/DSPFilters/include/DspFilters/Params.h`), the parameter vector of
//! the live filter object:
//!
//! * Slot `i` is the `i`-th parameter of the live filter object; the slot
//!   count is that object's `getNumParams()` (virtual call through
//!   `[vtable+0xc]`, `0x10002a86`). Slots at or past the count are zeroed and
//!   their arguments ignored (`fldz` at `0x10002aae`).
//! * A **missing or `void` argument restores the slot's default**: before the
//!   loop the function fills a local eight-double buffer from the filter's
//!   `getParamInfo(i)` through helper `0x100a5f30`, which loads the
//!   `ParamInfo` field at `+0x20`. In DSPFilters' `ParamInfo` that field is
//!   `m_defaultNativeValue` (layout `{m_id, m_szLabel, m_szName, m_arg1,
//!   m_arg2, m_defaultNativeValue, m_toControlValue, m_toNativeValue,
//!   m_toString}` — the same layout the DLL's own inlined `ParamInfo`
//!   constructors write: `0x100015f0` sample rate 11025/192000/44100,
//!   `0x10001640` cutoff and `0x10001690` center 10/22040/2000, `0x100016e0`
//!   ripple 0.001/12/0.01, `0x10001730` bandwidth Hz 10/22040/1720,
//!   `0x10001780` stopband 3/60/48, `0x10001870` gain −24/24/−6, `0x100017d0`
//!   pole real −1/1/0.25, `0x100018c0` pole distance 0/1/0.5, `0x10001910`
//!   pole angle 0/π/π⁄2, `0x10001a50` resonance −4/4/1, `0x10001aa0` slope
//!   −2/2/1, `0x10001af0` octave bandwidth −4/4/1 — the parameter table's
//!   defaults carry these recovered values). So a `void` slot is not "keep the
//!   previous value": it is "back to the design's default".
//! * Any other argument is stored through `AsReal()` (`0x10002b15`), whatever
//!   its type; arguments past the eighth slot are ignored (the loop runs
//!   `i < 8`, `0x10002aa0`).
//! * The callback stores the parameter count into the call's result
//!   (`0x10001ec8`, `tTJSVariant::operator=(tjs_int32)`, resolved from
//!   `im::tjsVariant.h`), i.e. `setParams` returns the slot count.
//!
//! Slot 0 is the **sample rate** for every design: DSPFilters' design classes
//! hand `params[0]` to their `setup` as the sample rate (`RBJ::TypeI::setParams`
//! and `Butterworth::Design::TypeI::setParams` in the library headers). That is
//! why PARQUET's `setParams(void, v0..v4)` leaves slot 0 `void`: it keeps the
//! default 44100 Hz. The orders of the other slots come from the design classes
//! of the DSPFilters library the DLL embeds; this port maps them onto its own
//! parameter names in [`positional_slots`] (slot 1 and up, sample rate aside):
//!
//! * `RBJ` — `[cutoff, Q]` for Low/High Pass, `[center, bandwidth]` for
//!   Band Pass/Stop, `[cutoff, gain, slope]` for Low/High Shelf and
//!   `[center, gain, bandwidth]` for Band Shelf.
//! * `Butterworth`/`Bessel`/`Legendre`/`Chebyshev I`/`Chebyshev II`/
//!   `Elliptic` — the library's `OrderBase` puts the `Order` at slot 1, then
//!   the response's frequencies (`cutoff`, or `center` + `bandwidthHz`), then
//!   `gain` for the shelves, then the design's extras (`ripple` for
//!   Chebyshev I/Elliptic, `stopAttenuation` for Chebyshev II/Elliptic).
//! * `Custom One-Pole`/`Two-Pole` — the port's placement: frequency, then
//!   `poleAngle`, `poleDistance`, `poleReal` (the DLL's Custom layouts are not
//!   recovered).
//!
//! **Unit conversions.** The library feeds the value after the frequency into
//! `sin/(2·x)` for two of the RBJ responses — TypeI's `x` is the `Resonance`
//! (Q), TypeII's the octave bandwidth — while this port's `quality()` computes
//! `sin/(2·Q)` from its `bandwidthOctaves` (`Q = 1/(2·sinh(ln2/2·BW))`). Those
//! two slots are therefore stored as the octave bandwidth that reproduces the
//! library's `x` exactly (the conversion is the exact inverse of `quality()`,
//! so the chain matches), and two consequences are visible and deliberate: a
//! `void` Q or bandwidth slot stores the converted default (`Q = 1` → ≈1.3886
//! octaves), and `currentValue("bandwidthOctaves")` reports the stored octave
//! value, not the script's `x`. TypeIII's slot is the library's shelf `Slope`,
//! stored verbatim: the port's shelf branch reads it with the gain exactly as
//! the library's `AL = sn/2·sqrt((A + 1/A)(1/S − 1) + 2)` does, so a later gain
//! change re-derives the shelf the way the reference would. TypeIV's bandwidth
//! goes through the library's `sinh(ln2/2·BW·w0/sn)` octave form — the port's
//! band shelf is the library's (`RBJ.cpp` `BandShelf::setup`), wedge included —
//! and stays a raw octave value. A value whose conversion has no positive
//! radicand — a non-positive `x`, or a shelf slope whose
//! `(A + 1/A)(1/S − 1) + 2` is not positive, e.g. slope `2` at `−24 dB` —
//! answers an error rather than the reference's nonsense filter: the
//! conversion refuses with the port's `invalid usage of ParamInfo`, and the
//! designer refuses a slope or bandwidth it cannot use with its own message
//! (the parameter is rolled back, so the filter keeps a working chain).
//!
//! **Which bandwidth field carries the Q.** The library's parameter lists
//! decide: the RBJ designs declare the octave bandwidth, so `quality()` reads
//! the octave field for them (with the Hz form as this port's fallback), while
//! the prototype designs declare only `Bandwidth (Hz)`
//! (`Butterworth.h`/`ChebyshevI.h`… `TypeIIBase` → `defaultBandwidthHzParam`),
//! so `quality()` reads the Hz field for them. Before this, the octave field's
//! default (1.0) shadowed the Hz slot value, and a `setParams(void, order,
//! center, bandwidthHz)` on a prototype band designed the wrong width.
//!
//! The dictionary/name-value pair forms are this port's extension (the
//! reference would read a dictionary as slot 0's value). They are checked
//! first — an object first argument is the dictionary form, a string first
//! argument is the pairs form, everything else (including no arguments and the
//! game's leading `void`) is the reference's positional form.
//!
//! # Construction
//!
//! The class is created through `TJSCreateNativeClassForPlugin(name,
//! factory)` (`0x100012e1`; the factory `0x10003bc0` news the 0xd8-byte
//! instance), and TJS2's `CreateNew` then calls the member named after the
//! class with the constructor arguments (`tjsNative.cpp:389`,
//! `FuncCall(0, ClassName, …, numparams, param, dsp)`). The DLL registers that
//! member (`0x10001ce7`, callback `0x10001e00`) and forwards the arguments to
//! the instance's first virtual method (`0x100020f0`), which consumes up to
//! four of them: slot 0 the response name, slot 1 the design name, slot 2 the
//! direct-form name and slot 3 an integer stored at `instance+0xd0`. Names are
//! mapped through the DLL's tables (`0x100cbd38` responses, `0x100cbda0`
//! designs, `0x100cbdf8` forms, each `{value, name}` with a negative
//! terminator; a miss leaves the field unchanged). PARQUET builds every filter
//! as `new WaveSoundBuffer.WaveDSPFilter(args[0], args[1])`
//! (`voiceeffect.tjs` object 2), so the port's constructor applies the first
//! two arguments as its `type` and `design`; the form and the trailing integer
//! are accepted and have no effect here (the port's chain is one direct form,
//! and the integer's role in the DLL was not recovered). The three response
//! names the reference's table carries but this port cannot model (`AllPass`,
//! `BandPass1`, `BandPass2`) are named in the port's `does not model` error —
//! the game's factory list includes an `AllPass_RBJ` wrapper — and a `new`
//! that names an unbuildable design fails before the filter is registered.
//!
//! A no-argument `new` is the one place the port's defaults are its own: the
//! reference's instance starts at response 8 (`0x10001fb0`'s `+0xc4`, its
//! table's last response), design 0 (`RBJ`), form `DirectFormII` and
//! `instance+0xd0 = 0x400`, while this port starts at `Low Pass`/`Butterworth`.
//! The game always passes both names, so the divergence is latent.
//!
//! # What this module implements
//!
//! **Real**: `WaveDSPFilter`'s member surface, the parameter table with its
//! defaults, and a real IIR designer/processor behind them — a biquad chain
//! built from the selected design and response:
//!
//! * `RBJ` — the cookbook coefficients for every response the class names;
//!   `Band Shelf` uses the library's own band-shelf form (`AL =
//!   sn·sinh(ln2/2·BW·w0/sn)`, `RBJ.cpp` `BandShelf::setup`), and the shelves
//!   their `sqrt((A + 1/A)(1/S − 1) + 2)` form.
//! * `Butterworth`, `Chebyshev I`, `Chebyshev II`, `Bessel`, `Legendre` —
//!   an analog prototype (computed here: closed forms for Butterworth and the
//!   two Chebyshev families, the roots of the reverse Bessel polynomial and of
//!   the Legendre polynomial for the other two), frequency-transformed for the
//!   selected response and mapped through the bilinear transform into biquad
//!   sections, peak-normalised to unity.
//! * `Custom One-Pole` / `Custom Two-Pole` — pole placement from the recovered
//!   `Pole Angle` / `Pole Distance` parameters.
//!
//! **Not implemented** (documented, not silently substituted): the
//! `Elliptic` design and the shelf responses for the non-RBJ designs need the
//! elliptic function machinery / polynomial shelf algebra of the reference
//! matrix, and the DLL's `AllPass`, `BandPass1` and `BandPass2` response
//! identifiers are recognised but unmodelled (this port names them in its
//! refusal instead of treating them as typos). All of them answer a clear error
//! rather than a wrong filter. The DLL's own Bessel/Legendre pole tables were not extracted
//! (that is a second RE pass on the 1.1 MB image), so those two prototypes are
//! computed numerically here.
//!
//! **Not reachable from the audio path yet**: like `wfBasicEffect`, nothing in
//! the engine consumes `WaveSoundBuffer.filters` or carries a filter chain
//! through `AudioCommand`, so the filters run through their Rust `process`
//! entry points only, and `interface` answers a sentinel integer rather than
//! the reference's raw pointer. The missing seam is filed as a finding.

// The designer and its processing entry points are the module's contract:
// nothing calls them until the `WaveSoundBuffer` filter chain exists, and the
// numeric tests drive them directly. `result_large_err` is the crate-wide
// `TjsError` size lint every native callback carries.
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
    feature: "WaveDSPFilter (tTJSNC_WaveDSPFilter / tTJSNI_WaveDSPFilter) on WaveSoundBuffer",
    notes: "Real IIR designer and processor behind the recovered member surface: RBJ cookbook \
            (the band shelf in the library's own sinh-octave form), Butterworth / Chebyshev I / \
            Chebyshev II / Bessel / Legendre analog prototypes through the bilinear transform, \
            and custom pole placement; the parameter table carries the recovered labels and both \
            the DLL's own identifiers and the readable spellings for design/response values. The \
            Elliptic design, the DLL's AllPass/BandPass1/BandPass2 identifiers and the non-RBJ \
            shelf responses answer a clear error instead of a wrong filter (see the module docs). \
            The class installs on the `WaveSoundBuffer` class object like the reference \
            (`WaveSoundBuffer.WaveDSPFilter`, the binder's base — PARQUET's voiceeffect.tjs reads \
            it there). The engine has no per-buffer filter chain yet, so the filter runs through \
            its Rust `process` entry point only and `interface` answers a sentinel integer.",
    install: |engine| engine.register_plugin(WfTypicalDspPlugin),
};

pub struct WfTypicalDspPlugin;

impl KrkrPlugin for WfTypicalDspPlugin {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_wf_typical_dsp(runtime);
        runtime
            .host_mut()
            .log("wfTypicalDSP.dll registered: WaveDSPFilter (designer local; no engine filter chain yet)");
        Ok(())
    }
}

const PLUGIN_NAME: &str = "wfTypicalDSP.dll";

/// The sentinel `interface` answers with (see the `wfBasicEffect` module: a
/// Rust engine resolves the filter by object identity instead of publishing a
/// raw `iTVPBasicWaveFilter*`).
const INTERFACE_SENTINEL: i64 = 0x5746_0101;

/// The sample rate behind slot 0 of the positional `setParams` form. The
/// reference's default is `ParamInfo::defaultSampleRateParam()`'s
/// `m_defaultNativeValue` (the DLL loads `44100.0` for the `Sample Rate`
/// descriptor, VA `0x100015f0`'s `ds:0x100d7d20`); a `void` slot 0 restores it.
const SAMPLE_RATE_DEFAULT: f64 = 44100.0;

/// The reference's slot vector has eight entries (`DSPFilters`'
/// `maxParameters`); its `setParams` loop runs `i < 8` and ignores arguments
/// past the eighth (`0x10002aa0`).
const MAX_POSITIONAL_SLOTS: usize = 8;

// ---------------------------------------------------------------------------
// The recovered vocabularies
// ---------------------------------------------------------------------------

/// This port's canonical response values (the dossier's spaced spellings; the
/// DLL itself only has the space-less identifiers below).
pub const RESPONSE_NAMES: [&str; 7] = [
    "Low Pass",
    "High Pass",
    "Band Pass",
    "Band Stop",
    "Low Shelf",
    "High Shelf",
    "Band Shelf",
];

/// This port's canonical design values.
pub const DESIGN_NAMES: [&str; 7] = [
    "Bessel",
    "Butterworth",
    "Chebyshev I",
    "Chebyshev II",
    "Elliptic",
    "Legendre",
    "RBJ",
];

/// The two pole-placement designs (the DLL's `Custom` + `OnePole`/`TwoPole`
/// identifiers, labelled `Custom One-Pole` / `Custom Two-Pole`).
pub const CUSTOM_DESIGN_NAMES: [&str; 2] = ["Custom One-Pole", "Custom Two-Pole"];

/// The DLL's own space-less response identifiers (`strings -el`), each mapped
/// to the canonical value it aliases.
pub const RESPONSE_IDENTIFIERS: [(&str, &str); 7] = [
    ("LowPass", "Low Pass"),
    ("HighPass", "High Pass"),
    ("BandPass", "Band Pass"),
    ("BandStop", "Band Stop"),
    ("LowShelf", "Low Shelf"),
    ("HighShelf", "High Shelf"),
    ("BandShelf", "Band Shelf"),
];

/// The DLL's design identifiers, including its `Chebyshev1`/`Chebyshev2`
/// short forms and the `OnePole`/`TwoPole` halves of the custom designs,
/// each mapped to the canonical value it aliases.
pub const DESIGN_IDENTIFIERS: [(&str, &str); 13] = [
    ("Bessel", "Bessel"),
    ("Butterworth", "Butterworth"),
    ("ChebyshevI", "Chebyshev I"),
    ("Chebyshev1", "Chebyshev I"),
    ("ChebyshevII", "Chebyshev II"),
    ("Chebyshev2", "Chebyshev II"),
    ("Elliptic", "Elliptic"),
    ("Legendre", "Legendre"),
    ("RBJ", "RBJ"),
    ("CustomOnePole", "Custom One-Pole"),
    ("OnePole", "Custom One-Pole"),
    ("CustomTwoPole", "Custom Two-Pole"),
    ("TwoPole", "Custom Two-Pole"),
];

/// The DLL's identifiers this port recognises but does not model: they get a
/// named refusal rather than the generic unknown-value message.
pub const UNMODELLED_RESPONSE_IDENTIFIERS: [&str; 3] = ["AllPass", "BandPass1", "BandPass2"];

/// Resolves a response name or identifier to the canonical value.
pub fn canonical_response(name: &str) -> Option<&'static str> {
    if let Some(canonical) = RESPONSE_NAMES.iter().find(|candidate| **candidate == name) {
        return Some(canonical);
    }
    RESPONSE_IDENTIFIERS
        .iter()
        .find(|(identifier, _)| *identifier == name)
        .map(|(_, canonical)| *canonical)
}

/// Resolves a design name or identifier to the canonical value.
pub fn canonical_design(name: &str) -> Option<&'static str> {
    if let Some(canonical) = design_names().find(|candidate| *candidate == name) {
        return Some(canonical);
    }
    DESIGN_IDENTIFIERS
        .iter()
        .find(|(identifier, _)| *identifier == name)
        .map(|(_, canonical)| *canonical)
}

/// Every design name `setParams` accepts.
pub fn design_names() -> impl Iterator<Item = &'static str> {
    DESIGN_NAMES.into_iter().chain(CUSTOM_DESIGN_NAMES)
}

// ---------------------------------------------------------------------------
// Complex numbers (the designer's only arithmetic dependency)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Complex {
    pub re: f64,
    pub im: f64,
}

impl Complex {
    pub const fn new(re: f64, im: f64) -> Self {
        Self { re, im }
    }

    pub fn polar(radius: f64, angle: f64) -> Self {
        Self::new(radius * angle.cos(), radius * angle.sin())
    }

    fn add(self, other: Self) -> Self {
        Self::new(self.re + other.re, self.im + other.im)
    }

    fn sub(self, other: Self) -> Self {
        Self::new(self.re - other.re, self.im - other.im)
    }

    fn mul(self, other: Self) -> Self {
        Self::new(
            self.re * other.re - self.im * other.im,
            self.re * other.im + self.im * other.re,
        )
    }

    fn div(self, other: Self) -> Self {
        let denominator = other.re * other.re + other.im * other.im;
        Self::new(
            (self.re * other.re + self.im * other.im) / denominator,
            (self.im * other.re - self.re * other.im) / denominator,
        )
    }

    fn scale(self, factor: f64) -> Self {
        Self::new(self.re * factor, self.im * factor)
    }

    pub fn abs(self) -> f64 {
        self.re.hypot(self.im)
    }

    fn sqrt(self) -> Self {
        // Principal square root.
        let radius = self.abs();
        let re = ((radius + self.re) / 2.0).sqrt();
        let im = ((radius - self.re) / 2.0).sqrt().copysign(self.im);
        Self::new(re, im)
    }
}

// ---------------------------------------------------------------------------
// The response and design vocabulary
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResponseType {
    LowPass,
    HighPass,
    BandPass,
    BandStop,
    LowShelf,
    HighShelf,
    BandShelf,
}

impl ResponseType {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match canonical_response(name)? {
            "Low Pass" => Self::LowPass,
            "High Pass" => Self::HighPass,
            "Band Pass" => Self::BandPass,
            "Band Stop" => Self::BandStop,
            "Low Shelf" => Self::LowShelf,
            "High Shelf" => Self::HighShelf,
            "Band Shelf" => Self::BandShelf,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::LowPass => "Low Pass",
            Self::HighPass => "High Pass",
            Self::BandPass => "Band Pass",
            Self::BandStop => "Band Stop",
            Self::LowShelf => "Low Shelf",
            Self::HighShelf => "High Shelf",
            Self::BandShelf => "Band Shelf",
        }
    }

    fn is_shelf(self) -> bool {
        matches!(self, Self::LowShelf | Self::HighShelf | Self::BandShelf)
    }

    fn is_band(self) -> bool {
        matches!(self, Self::BandPass | Self::BandStop | Self::BandShelf)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesignKind {
    Bessel,
    Butterworth,
    ChebyshevI,
    ChebyshevII,
    Elliptic,
    Legendre,
    Rbj,
    CustomOnePole,
    CustomTwoPole,
}

impl DesignKind {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match canonical_design(name)? {
            "Bessel" => Self::Bessel,
            "Butterworth" => Self::Butterworth,
            "Chebyshev I" => Self::ChebyshevI,
            "Chebyshev II" => Self::ChebyshevII,
            "Elliptic" => Self::Elliptic,
            "Legendre" => Self::Legendre,
            "RBJ" => Self::Rbj,
            "Custom One-Pole" => Self::CustomOnePole,
            "Custom Two-Pole" => Self::CustomTwoPole,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Bessel => "Bessel",
            Self::Butterworth => "Butterworth",
            Self::ChebyshevI => "Chebyshev I",
            Self::ChebyshevII => "Chebyshev II",
            Self::Elliptic => "Elliptic",
            Self::Legendre => "Legendre",
            Self::Rbj => "RBJ",
            Self::CustomOnePole => "Custom One-Pole",
            Self::CustomTwoPole => "Custom Two-Pole",
        }
    }
}

/// A design the port does not implement is an error, never a silent substitute.
#[derive(Clone, Debug, PartialEq)]
pub enum DesignError {
    Unsupported(String),
    BadParameter(String),
}

impl std::fmt::Display for DesignError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(message) | Self::BadParameter(message) => {
                formatter.write_str(message)
            }
        }
    }
}

/// Everything the designer needs, in the units the parameter table carries.
#[derive(Clone, Copy, Debug)]
pub struct FilterSpec {
    pub response: ResponseType,
    pub design: DesignKind,
    pub sample_rate: f32,
    pub cutoff: f32,
    pub center: f32,
    pub bandwidth_hz: f32,
    pub bandwidth_octaves: f32,
    pub order: u32,
    pub gain_db: f32,
    pub ripple: f64,
    pub stop_attenuation: f64,
    pub pole_angle: f32,
    pub pole_distance: f32,
    pub pole_real: f32,
}

impl FilterSpec {
    /// The design centre, in Hz: the cutoff for the single-ended responses,
    /// the centre frequency for the band ones.
    fn design_frequency(&self) -> f32 {
        if self.response.is_band() {
            self.center
        } else {
            self.cutoff
        }
    }

    /// The resonant `Q` the RBJ designs and the band transforms use. Which
    /// field carries it follows the library's parameter lists: the RBJ designs
    /// declare an octave bandwidth (`RBJ.h` `TypeIIBase` → `Bandwidth
    /// (Octaves)`), so their octave field wins when it is set, with the Hz form
    /// as the port's fallback; the prototype designs declare only `Bandwidth
    /// (Hz)` (`Butterworth.h` `TypeIIBase` → `defaultBandwidthHzParam`), so
    /// they always read the Hz field — otherwise the octave field's default
    /// (1.0) would shadow the reference's Hz slot value.
    fn quality(&self) -> f64 {
        let frequency = f64::from(self.design_frequency().max(1.0));
        if self.design != DesignKind::Rbj {
            return (frequency / f64::from(self.bandwidth_hz.max(1.0))).max(0.001);
        }
        if self.bandwidth_octaves > 0.0 {
            let bandwidth = f64::from(self.bandwidth_octaves);
            let sinh = ((std::f64::consts::LN_2 / 2.0) * bandwidth).sinh();
            if sinh > 0.0 {
                return 1.0 / (2.0 * sinh);
            }
        }
        (frequency / f64::from(self.bandwidth_hz.max(1.0))).max(0.001)
    }

    /// The band edge in radians, clamped below Nyquist so the bilinear
    /// transform always lands inside the unit circle.
    fn digital_omega(&self) -> f64 {
        let nyquist = f64::from(self.sample_rate) / 2.0;
        let frequency = f64::from(self.design_frequency()).clamp(1.0, nyquist * 0.999);
        2.0 * std::f64::consts::PI * frequency / f64::from(self.sample_rate)
    }
}

// ---------------------------------------------------------------------------
// Biquads
// ---------------------------------------------------------------------------

/// One transposed-direct-form-II biquad.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Biquad {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
    state1: f32,
    state2: f32,
}

impl Biquad {
    pub const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
        state1: 0.0,
        state2: 0.0,
    };

    pub fn process(&mut self, input: f32) -> f32 {
        let output = self.b0 * input + self.state1;
        self.state1 = flush_denormal(self.b1 * input - self.a1 * output + self.state2);
        self.state2 = flush_denormal(self.b2 * input - self.a2 * output);
        output
    }

    pub fn reset(&mut self) {
        self.state1 = 0.0;
        self.state2 = 0.0;
    }

    /// The magnitude of `H(e^{jω})` for the section alone, evaluated directly
    /// on the unit circle.
    fn magnitude_at(&self, omega: f64) -> f64 {
        let (sin1, cos1) = omega.sin_cos();
        let (sin2, cos2) = (2.0 * omega).sin_cos();
        let (b0, b1, b2) = (f64::from(self.b0), f64::from(self.b1), f64::from(self.b2));
        let (a1, a2) = (f64::from(self.a1), f64::from(self.a2));
        let numerator_re = b0 + b1 * cos1 + b2 * cos2;
        let numerator_im = -(b1 * sin1 + b2 * sin2);
        let denominator_re = 1.0 + a1 * cos1 + a2 * cos2;
        let denominator_im = -(a1 * sin1 + a2 * sin2);
        numerator_re.hypot(numerator_im) / denominator_re.hypot(denominator_im)
    }
}

/// Flushes a subnormal to zero (the reference's `undenormalise`), keeping a
/// decaying IIR's state exactly zero.
fn flush_denormal(value: f32) -> f32 {
    if value != 0.0 && value.abs() < f32::MIN_POSITIVE {
        0.0
    } else {
        value
    }
}

// ---------------------------------------------------------------------------
// Analog prototypes
// ---------------------------------------------------------------------------

/// The analog low-pass prototype: poles and zeros in the `s` plane at a unit
/// cutoff, plus its DC gain.
struct Prototype {
    poles: Vec<Complex>,
    zeros: Vec<Complex>,
    dc_gain: f64,
}

impl Prototype {
    /// Evaluates `H(σ + jω)` of the prototype as given.
    fn response(&self, point: Complex) -> Complex {
        let mut value = Complex::new(self.dc_gain, 0.0);
        for zero in &self.zeros {
            value = value.mul(point.sub(*zero));
        }
        for pole in &self.poles {
            value = value.div(point.sub(*pole));
        }
        value
    }
}

/// Butterworth: `n` poles on the unit circle, no zeros.
fn butterworth_prototype(order: u32) -> Prototype {
    let n = order.max(1);
    let poles = (0..n)
        .map(|index| {
            let angle = std::f64::consts::PI * (2.0 * f64::from(index) + f64::from(n) + 1.0)
                / (2.0 * f64::from(n));
            Complex::polar(1.0, angle)
        })
        .collect();
    Prototype {
        poles,
        zeros: Vec::new(),
        dc_gain: 1.0,
    }
}

/// Chebyshev type I: equiripple passband with `ripple_db` of ripple. The DC
/// gain is `1/sqrt(1 + ε²)` for even orders and `1` for odd ones, which is the
/// classical normalisation (and leaves the DC gain below 1 for even orders,
/// exactly as the reference's design does).
fn chebyshev_i_prototype(order: u32, ripple_db: f64) -> Prototype {
    let n = order.max(1);
    let epsilon = (10.0_f64.powf(ripple_db.max(0.0) / 10.0) - 1.0)
        .max(1e-12)
        .sqrt();
    let mu = (1.0 / epsilon).asinh() / f64::from(n);
    let poles = (0..n)
        .map(|index| {
            let angle =
                std::f64::consts::PI * (2.0 * f64::from(index) + 1.0) / (2.0 * f64::from(n));
            Complex::new(-mu.sinh() * angle.sin(), mu.cosh() * angle.cos())
        })
        .collect();
    let dc_gain = if n.is_multiple_of(2) {
        1.0 / (1.0 + epsilon * epsilon).sqrt()
    } else {
        1.0
    };
    Prototype {
        poles,
        zeros: Vec::new(),
        dc_gain,
    }
}

/// Chebyshev type II: equiripple stopband with `stop_db` of attenuation.
fn chebyshev_ii_prototype(order: u32, stop_db: f64) -> Prototype {
    let n = order.max(1);
    let ripple = stop_db.max(0.1);
    let epsilon = 1.0 / (10.0_f64.powf(ripple / 10.0) - 1.0).max(1e-12).sqrt();
    let mu = (1.0 / epsilon).asinh() / f64::from(n);
    let mut poles = Vec::new();
    let mut zeros = Vec::new();
    for index in 0..n {
        let angle = std::f64::consts::PI * (2.0 * f64::from(index) + 1.0) / (2.0 * f64::from(n));
        let (sin, cos) = (angle.sin(), angle.cos());
        let cosh = mu.cosh();
        let sinh = mu.sinh();
        let pole_re = -sinh * sin;
        let pole_im = cosh * cos;
        let magnitude = pole_re * pole_re + pole_im * pole_im;
        // The type-II poles are the reciprocal of the type-I poles.
        poles.push(Complex::new(
            -sinh * sin / magnitude,
            cosh * cos / magnitude,
        ));
        let zero_magnitude = 1.0 / cos;
        zeros.push(Complex::new(0.0, zero_magnitude));
    }
    Prototype {
        poles,
        zeros,
        dc_gain: 1.0,
    }
}

/// The roots of the reverse Bessel polynomial `θ_n(s)` (the Bessel filter's
/// prototype poles), computed with the Durand-Kerner method — deterministic
/// and independent of platform libraries. The prototype is then scaled so
/// `|H(j·1)| = 1/√2` (the −3 dB definition of the Bessel cutoff).
fn bessel_prototype(order: u32) -> Prototype {
    let n = order.max(1);
    // θ_n(s) = sum_{k=0..n} (2n-k)! / ((n-k)! k! 2^(n-k)) s^k.
    let factorial = |value: u32| -> f64 { (1..=value).map(f64::from).product::<f64>() };
    let mut coefficients = Vec::with_capacity(n as usize + 1);
    for k in 0..=n {
        let numerator = factorial(2 * n - k);
        let denominator = factorial(n - k) * factorial(k) * 2.0_f64.powi((n - k) as i32);
        coefficients.push(numerator / denominator);
    }
    let mut prototype = Prototype {
        poles: polynomial_roots(&coefficients),
        zeros: Vec::new(),
        dc_gain: 1.0,
    };
    scale_to_minus_three_db(&mut prototype);
    prototype
}

/// Legendre: the classical construction from the Legendre polynomial, using
/// `|H(jω)|² = 1 / (1 + P_n(ω)²)` with the polynomial in `ω`. The poles are
/// the left-half-plane roots of `1 + P_n(s/j)²`, so the −3 dB point sits at
/// `ω = 1` exactly (`P_n(1) = 1`). The DLL's own table was not extracted.
fn legendre_prototype(order: u32) -> Prototype {
    let n = order.max(1);
    let polynomial = legendre_polynomial(n);
    // Substitute x → -j s: coefficient k becomes c_k (-j)^k.
    let mut substituted = Vec::with_capacity(polynomial.len());
    let mut power = Complex::new(1.0, 0.0);
    let minus_j = Complex::new(0.0, -1.0);
    for coefficient in &polynomial {
        substituted.push(power.scale(*coefficient));
        power = power.mul(minus_j);
    }
    // Square it and add one: Q(s) = 1 + P_n(s/j)².
    let mut squared = complex_polynomial_multiply(&substituted, &substituted);
    squared[0] = squared[0].add(Complex::new(1.0, 0.0));
    let poles = polynomial_roots_complex(&squared)
        .into_iter()
        .filter(|root| root.re < 0.0)
        .collect();
    Prototype {
        poles,
        zeros: Vec::new(),
        dc_gain: legendre_dc_gain(n),
    }
}

/// `P_n(x)` by the recurrence `P_0 = 1`, `P_1 = x`,
/// `(k+1) P_{k+1} = (2k+1) x P_k - k P_{k-1}`, in ascending coefficients.
fn legendre_polynomial(order: u32) -> Vec<f64> {
    let n = order.max(1);
    let mut previous = vec![1.0_f64];
    let mut current = vec![0.0_f64, 1.0];
    for k in 1..n {
        let k_f = f64::from(k);
        let mut next = vec![0.0_f64; current.len() + 1];
        for (index, coefficient) in current.iter().enumerate() {
            next[index + 1] += (2.0 * k_f + 1.0) * coefficient;
        }
        for (index, coefficient) in previous.iter().enumerate() {
            next[index] -= k_f * coefficient;
        }
        for coefficient in next.iter_mut() {
            *coefficient /= k_f + 1.0;
        }
        previous = current;
        current = next;
    }
    if n == 0 { vec![1.0] } else { current }
}

/// `|H(0)| = 1 / sqrt(1 + P_n(0)²)`.
fn legendre_dc_gain(order: u32) -> f64 {
    let polynomial = legendre_polynomial(order);
    1.0 / (1.0 + polynomial[0] * polynomial[0]).sqrt()
}

/// Ascending-coefficient polynomial product over the complexes.
fn complex_polynomial_multiply(left: &[Complex], right: &[Complex]) -> Vec<Complex> {
    let mut product = vec![Complex::new(0.0, 0.0); left.len() + right.len() - 1];
    for (left_index, left_value) in left.iter().enumerate() {
        for (right_index, right_value) in right.iter().enumerate() {
            product[left_index + right_index] =
                product[left_index + right_index].add(left_value.mul(*right_value));
        }
    }
    product
}

/// Scales a prototype's poles so its response crosses −3 dB at ω = 1.
///
/// The ratio is relative to the prototype's own DC gain: the digital design
/// re-normalises the chain to unity peak, so an absolute −3 dB at ω = 1 would
/// be undone by that step (the unscaled Bessel prototype's DC gain is far from
/// one). For these prototypes (no finite zeros) the ratio at a fixed ω grows
/// with the pole scale, so the search brackets the crossing geometrically and
/// bisects it; a prototype that does not cross the target is left unscaled.
fn scale_to_minus_three_db(prototype: &mut Prototype) {
    let target = std::f64::consts::FRAC_1_SQRT_2;
    let response = |scale: f64, prototype: &Prototype, point: Complex| {
        let scaled = Prototype {
            poles: prototype
                .poles
                .iter()
                .map(|pole| pole.scale(scale))
                .collect(),
            zeros: prototype.zeros.clone(),
            dc_gain: prototype.dc_gain,
        };
        scaled.response(point).abs()
    };
    let ratio = |scale: f64, prototype: &Prototype| {
        let dc = response(scale, prototype, Complex::new(0.0, 0.0));
        if dc <= 0.0 {
            return 1.0;
        }
        response(scale, prototype, Complex::new(0.0, 1.0)) / dc
    };
    let (mut low, mut high) = (1e-6_f64, 1e6_f64);
    if !(ratio(low, prototype) <= target && ratio(high, prototype) >= target) {
        return;
    }
    for _ in 0..200 {
        let middle = (low * high).sqrt();
        if ratio(middle, prototype) < target {
            low = middle;
        } else {
            high = middle;
        }
    }
    let scale = (low * high).sqrt();
    for pole in &mut prototype.poles {
        *pole = pole.scale(scale);
    }
}

/// Roots of a polynomial given by ascending real coefficients, Durand-Kerner.
fn polynomial_roots(coefficients: &[f64]) -> Vec<Complex> {
    let complex: Vec<Complex> = coefficients
        .iter()
        .map(|value| Complex::new(*value, 0.0))
        .collect();
    polynomial_roots_complex(&complex)
}

/// Roots of a polynomial given by ascending complex coefficients,
/// Durand-Kerner: deterministic, allocation-light and library-free.
fn polynomial_roots_complex(coefficients: &[Complex]) -> Vec<Complex> {
    let degree = coefficients
        .iter()
        .rposition(|value| value.abs() > 1e-14)
        .unwrap_or(0);
    if degree == 0 {
        return Vec::new();
    }
    let leading = coefficients[degree];
    let normalized: Vec<Complex> = coefficients[..=degree]
        .iter()
        .map(|value| value.div(leading))
        .collect();
    let evaluate = |point: Complex| -> Complex {
        let mut value = Complex::new(0.0, 0.0);
        for coefficient in normalized.iter().rev() {
            value = value.mul(point).add(*coefficient);
        }
        value
    };
    // The classic seed: powers of a complex number just inside the unit
    // circle, which keeps the iterates apart for these polynomials.
    let seed = Complex::polar(0.9, 0.7);
    let mut roots: Vec<Complex> = (0..degree)
        .map(|index| {
            let mut value = Complex::new(1.0, 0.0);
            for _ in 0..=index {
                value = value.mul(seed);
            }
            value
        })
        .collect();
    for _ in 0..600 {
        let mut shift = 0.0_f64;
        for index in 0..degree {
            let mut denominator = Complex::new(1.0, 0.0);
            for other in 0..degree {
                if other != index {
                    denominator = denominator.mul(roots[index].sub(roots[other]));
                }
            }
            if denominator.abs() < 1e-300 {
                continue;
            }
            let delta = evaluate(roots[index]).div(denominator);
            roots[index] = roots[index].sub(delta);
            shift = shift.max(delta.abs());
        }
        if shift < 1e-14 {
            break;
        }
    }
    roots
}

// ---------------------------------------------------------------------------
// The designer
// ---------------------------------------------------------------------------

/// A designed chain: the cascade of sections that realises the response.
#[derive(Clone, Debug, Default)]
pub struct DesignedFilter {
    pub sections: Vec<Biquad>,
}

impl DesignedFilter {
    /// Builds the chain for `spec`, or answers why the combination is not
    /// implemented (never a silent substitute).
    pub fn design(spec: &FilterSpec) -> std::result::Result<Self, DesignError> {
        let mut sections = match spec.design {
            DesignKind::Rbj => rbj_sections(spec)?,
            DesignKind::CustomOnePole => vec![custom_one_pole(spec)],
            DesignKind::CustomTwoPole => vec![custom_two_pole(spec)],
            DesignKind::Elliptic => {
                return Err(DesignError::Unsupported(
                    "the Elliptic filter design is not implemented in this port".to_string(),
                ));
            }
            _ => prototype_sections(spec)?,
        };
        if matches!(
            spec.design,
            DesignKind::Bessel
                | DesignKind::Butterworth
                | DesignKind::ChebyshevI
                | DesignKind::ChebyshevII
                | DesignKind::Legendre
        ) {
            normalize_peak(&mut sections, spec);
        }
        Ok(Self { sections })
    }

    pub fn reset(&mut self) {
        for section in &mut self.sections {
            section.reset();
        }
    }

    /// Processes interleaved `f32` frames in place.
    pub fn process(&mut self, frames: &mut [f32], channels: usize) {
        if channels == 0 {
            return;
        }
        for frame in frames.chunks_mut(channels) {
            for value in frame.iter_mut() {
                let mut sample = *value;
                for section in &mut self.sections {
                    sample = section.process(sample);
                }
                *value = sample;
            }
        }
    }

    /// The chain's magnitude response at `frequency`, for verification and for
    /// a future `getParamInfo`-style inspection.
    pub fn magnitude_at_hz(&self, frequency: f64, sample_rate: f32) -> f64 {
        let omega = 2.0 * std::f64::consts::PI * frequency / f64::from(sample_rate);
        chain_magnitude(&self.sections, omega)
    }
}

fn chain_magnitude(sections: &[Biquad], omega: f64) -> f64 {
    sections
        .iter()
        .map(|section| section.magnitude_at(omega))
        .product()
}

/// Scales the chain so its peak magnitude over the audio band is 1.0. This is
/// the normalisation the classical prototypes expect (Butterworth and the
/// Bessel/Legendre prototypes are 1 at DC, the Chebyshev families ripple up to
/// 1), applied uniformly here.
fn normalize_peak(sections: &mut [Biquad], spec: &FilterSpec) {
    if sections.is_empty() {
        return;
    }
    let nyquist = f64::from(spec.sample_rate) / 2.0;
    let mut peak = 0.0_f64;
    // A logarithmic sweep plus DC and Nyquist: a narrow resonance at a low
    // frequency still meets the grid.
    let steps = 2048;
    for index in 0..=steps {
        let fraction = index as f64 / steps as f64;
        let frequency = if index == 0 {
            0.0
        } else {
            1.0 * (nyquist / 1.0).powf(fraction)
        }
        .min(nyquist);
        let omega = 2.0 * std::f64::consts::PI * frequency / f64::from(spec.sample_rate);
        peak = peak.max(chain_magnitude(sections, omega));
    }
    if peak > 1e-12 {
        // The chain's gain is the product of its sections, so scaling one
        // section's numerator scales the whole response exactly once.
        let scale = (1.0 / peak) as f32;
        if let Some(first) = sections.first_mut() {
            first.b0 *= scale;
            first.b1 *= scale;
            first.b2 *= scale;
        }
    }
}

/// The RBJ cookbook coefficients for every response the class names.
fn rbj_sections(spec: &FilterSpec) -> std::result::Result<Vec<Biquad>, DesignError> {
    let omega = spec.digital_omega();
    let (sin, cos) = (omega.sin(), omega.cos());
    let q = spec.quality().max(0.001);
    let alpha = sin / (2.0 * q);
    let a = 10.0_f64.powf(f64::from(spec.gain_db) / 40.0);
    let section = |b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64| Biquad {
        b0: (b0 / a0) as f32,
        b1: (b1 / a0) as f32,
        b2: (b2 / a0) as f32,
        a1: (a1 / a0) as f32,
        a2: (a2 / a0) as f32,
        state1: 0.0,
        state2: 0.0,
    };
    let coefficients = match spec.response {
        ResponseType::LowPass => section(
            (1.0 - cos) / 2.0,
            1.0 - cos,
            (1.0 - cos) / 2.0,
            1.0 + alpha,
            -2.0 * cos,
            1.0 - alpha,
        ),
        ResponseType::HighPass => section(
            (1.0 + cos) / 2.0,
            -(1.0 + cos),
            (1.0 + cos) / 2.0,
            1.0 + alpha,
            -2.0 * cos,
            1.0 - alpha,
        ),
        ResponseType::BandPass => section(alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
        ResponseType::BandStop => {
            section(1.0, -2.0 * cos, 1.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
        }
        ResponseType::LowShelf => {
            let beta = shelf_sq(spec, a, sin)?;
            section(
                a * ((a + 1.0) - (a - 1.0) * cos + beta),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - beta),
                (a + 1.0) + (a - 1.0) * cos + beta,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                (a + 1.0) + (a - 1.0) * cos - beta,
            )
        }
        ResponseType::HighShelf => {
            let beta = shelf_sq(spec, a, sin)?;
            section(
                a * ((a + 1.0) + (a - 1.0) * cos + beta),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - beta),
                (a + 1.0) - (a - 1.0) * cos + beta,
                2.0 * ((a - 1.0) - (a + 1.0) * cos),
                (a + 1.0) - (a - 1.0) * cos - beta,
            )
        }
        // The library's band shelf (`RBJ.cpp` `BandShelf::setup`), not the
        // cookbook's peaking section: `AL = sn·sinh(ln2/2·BW·w0/sn)` — the
        // `w0/sn` wedge is what makes the octave bandwidth mean an octave away
        // from DC — with `b0 = 1 + AL·A`, `b2 = 1 − AL·A`, `a0 = 1 + AL/A`,
        // `a2 = 1 − AL/A`.
        ResponseType::BandShelf => {
            let bandwidth = f64::from(spec.bandwidth_octaves);
            let al = sin * ((std::f64::consts::LN_2 / 2.0) * bandwidth * omega / sin).sinh();
            if !al.is_finite() || al <= 0.0 {
                return Err(DesignError::BadParameter(format!(
                    "Band Shelf bandwidth {bandwidth} has no finite form at {} Hz",
                    spec.center
                )));
            }
            section(
                1.0 + al * a,
                -2.0 * cos,
                1.0 - al * a,
                1.0 + al / a,
                -2.0 * cos,
                1.0 - al / a,
            )
        }
    };
    Ok(vec![coefficients])
}

/// The shelf's `sq` term: the library's `2·sqrt(A)·AL` with `AL =
/// sn/2·sqrt((A + 1/A)(1/S − 1) + 2)` (`RBJ.cpp` `LowShelf::setup` /
/// `HighShelf::setup`), where `S` is the stored `Slope` value. `S` doubles as
/// the port's octave field, and must leave the radicand positive.
fn shelf_sq(spec: &FilterSpec, a: f64, sin: f64) -> std::result::Result<f64, DesignError> {
    let slope = f64::from(spec.bandwidth_octaves);
    let radicand = (a + 1.0 / a) * (1.0 / slope - 1.0) + 2.0;
    if !radicand.is_finite() || radicand <= 0.0 {
        return Err(DesignError::BadParameter(format!(
            "the RBJ shelf slope {slope} has no finite form at {} dB",
            spec.gain_db
        )));
    }
    Ok(a.sqrt() * sin * radicand.sqrt())
}

fn custom_one_pole(spec: &FilterSpec) -> Biquad {
    let pole = f64::from(spec.pole_distance).clamp(0.0, 0.999_999);
    Biquad {
        b0: (1.0 - pole) as f32,
        b1: 0.0,
        b2: 0.0,
        a1: (-pole) as f32,
        a2: 0.0,
        state1: 0.0,
        state2: 0.0,
    }
}

fn custom_two_pole(spec: &FilterSpec) -> Biquad {
    let distance = f64::from(spec.pole_distance).clamp(0.0, 0.999_999);
    let angle = f64::from(spec.pole_angle);
    let a1 = -2.0 * distance * angle.cos();
    let a2 = distance * distance;
    // Unity gain at DC, the classical low-pass normalisation.
    let b0 = 1.0 + a1 + a2;
    Biquad {
        b0: b0 as f32,
        b1: 0.0,
        b2: 0.0,
        a1: a1 as f32,
        a2: a2 as f32,
        state1: 0.0,
        state2: 0.0,
    }
}

/// The analog prototype for the selected design, with the recovered
/// `ripple` / `stopAttenuation` parameters where the family needs them.
fn prototype_for(spec: &FilterSpec) -> Prototype {
    match spec.design {
        DesignKind::Butterworth => butterworth_prototype(spec.order),
        DesignKind::ChebyshevI => chebyshev_i_prototype(spec.order, spec.ripple),
        DesignKind::ChebyshevII => chebyshev_ii_prototype(spec.order, spec.stop_attenuation),
        DesignKind::Bessel => bessel_prototype(spec.order),
        DesignKind::Legendre => legendre_prototype(spec.order),
        // Handled before this point; the RBJ and custom designs never need a
        // prototype and Elliptic is refused.
        _ => butterworth_prototype(spec.order),
    }
}

/// The prototype → response → bilinear pipeline for the analog designs.
fn prototype_sections(spec: &FilterSpec) -> std::result::Result<Vec<Biquad>, DesignError> {
    if spec.response.is_shelf() {
        return Err(DesignError::Unsupported(format!(
            "the {} shelf responses are only implemented for the RBJ design",
            spec.design.name()
        )));
    }
    if spec.order == 0 || spec.order > 24 {
        return Err(DesignError::BadParameter(format!(
            "Order must be between 1 and 24, got {}",
            spec.order
        )));
    }
    let prototype = prototype_for(spec);
    let omega = spec.digital_omega();
    // The analog band parameters, pre-warped so the digital edge lands on the
    // requested frequency (the standard bilinear frequency warping).
    let prewarp = |value: f64| 2.0 * f64::from(spec.sample_rate) * (value / 2.0).tan();
    let omega_cutoff = prewarp(omega);
    let omega_center = prewarp(spec.digital_omega());

    let (analog_poles, analog_zeros, zeros_at_infinity, zeros_at_origin) = match spec.response {
        ResponseType::LowPass => (
            prototype
                .poles
                .iter()
                .map(|pole| pole.scale(omega_cutoff))
                .collect::<Vec<_>>(),
            prototype
                .zeros
                .iter()
                .map(|zero| zero.scale(omega_cutoff))
                .collect::<Vec<_>>(),
            prototype.poles.len().saturating_sub(prototype.zeros.len()),
            0usize,
        ),
        ResponseType::HighPass => (
            prototype
                .poles
                .iter()
                .map(|pole| Complex::new(omega_cutoff, 0.0).div(*pole))
                .collect::<Vec<_>>(),
            prototype
                .zeros
                .iter()
                .map(|zero| Complex::new(omega_cutoff, 0.0).div(*zero))
                .collect::<Vec<_>>(),
            0,
            prototype.poles.len().saturating_sub(prototype.zeros.len()),
        ),
        ResponseType::BandPass => {
            let bandwidth = omega_center / spec.quality().max(0.001);
            let mut poles = Vec::new();
            let mut zeros = Vec::new();
            for pole in &prototype.poles {
                poles.extend(solve_band_pair(*pole, bandwidth, omega_center));
            }
            for zero in &prototype.zeros {
                zeros.extend(solve_band_pair(*zero, bandwidth, omega_center));
            }
            let excess = prototype.poles.len().saturating_sub(prototype.zeros.len());
            (poles, zeros, excess, excess)
        }
        ResponseType::BandStop => {
            let bandwidth = omega_center / spec.quality().max(0.001);
            let mut poles = Vec::new();
            let mut zeros = Vec::new();
            for pole in &prototype.poles {
                poles.extend(solve_band_stop_pair(*pole, bandwidth, omega_center));
            }
            for zero in &prototype.zeros {
                zeros.extend(solve_band_stop_pair(*zero, bandwidth, omega_center));
            }
            let excess = prototype.poles.len().saturating_sub(prototype.zeros.len());
            // Butterworth's zeros at infinity become the ±ω0 notch pair.
            for _ in 0..excess {
                zeros.push(Complex::new(0.0, omega_center));
                zeros.push(Complex::new(0.0, -omega_center));
            }
            (poles, zeros, 0, 0)
        }
        _ => return Err(DesignError::Unsupported("unreachable response".to_string())),
    };

    // Bilinear transform with the frequency pre-warp already applied.
    let two_fs = 2.0 * f64::from(spec.sample_rate);
    let to_digital = |point: Complex| -> Complex {
        Complex::new(two_fs, 0.0)
            .add(point)
            .div(Complex::new(two_fs, 0.0).sub(point))
    };
    let mut digital_poles: Vec<Complex> =
        analog_poles.iter().map(|pole| to_digital(*pole)).collect();
    let mut digital_zeros: Vec<Complex> =
        analog_zeros.iter().map(|zero| to_digital(*zero)).collect();
    for _ in 0..zeros_at_infinity {
        digital_zeros.push(Complex::new(-1.0, 0.0));
    }
    for _ in 0..zeros_at_origin {
        digital_zeros.push(Complex::new(1.0, 0.0));
    }

    // Pair the roots section by section: a conjugate pole pair takes two
    // zeros (a conjugate pair or two real ones) into one biquad, a left-over
    // real pole takes one real zero into a first-order section. Consuming as
    // many zeros as the section has poles keeps every root in the product.
    digital_poles.sort_by(|left, right| left.im.abs().total_cmp(&right.im.abs()));
    digital_zeros.sort_by(|left, right| left.im.abs().total_cmp(&right.im.abs()));
    let mut sections = Vec::new();
    let mut zero_index = 0;
    let mut pole_index = 0;
    while pole_index < digital_poles.len() {
        let pole = digital_poles[pole_index];
        let has_conjugate = pole.im.abs() > 1e-9 && pole_index + 1 < digital_poles.len();
        if has_conjugate {
            let second = digital_poles[pole_index + 1];
            pole_index += 2;
            // A conjugate zero pair (the sort keeps conjugates adjacent), or
            // two real zeros.
            let (first_zero, second_zero) = if zero_index + 1 < digital_zeros.len() {
                let pair = (digital_zeros[zero_index], digital_zeros[zero_index + 1]);
                zero_index += 2;
                pair
            } else if zero_index < digital_zeros.len() {
                let zero = digital_zeros[zero_index];
                zero_index += 1;
                (zero, Complex::new(-1.0, 0.0))
            } else {
                (Complex::new(-1.0, 0.0), Complex::new(-1.0, 0.0))
            };
            let numerator_1 = -(first_zero.re + second_zero.re);
            let numerator_2 = first_zero.re * second_zero.re - first_zero.im * second_zero.im;
            let denominator_1 = -(pole.re + second.re);
            let denominator_2 = pole.re * second.re - pole.im * second.im;
            sections.push(Biquad {
                b0: 1.0,
                b1: numerator_1 as f32,
                b2: numerator_2 as f32,
                a1: denominator_1 as f32,
                a2: denominator_2 as f32,
                state1: 0.0,
                state2: 0.0,
            });
        } else {
            pole_index += 1;
            let zero = digital_zeros
                .get(zero_index)
                .copied()
                .unwrap_or(Complex::new(-1.0, 0.0));
            zero_index += 1;
            sections.push(Biquad {
                b0: 1.0,
                b1: (-zero.re) as f32,
                b2: 0.0,
                a1: (-pole.re) as f32,
                a2: 0.0,
                state1: 0.0,
                state2: 0.0,
            });
        }
    }
    Ok(sections)
}

/// The two `s`-plane roots of `s² - b p s + w0² = 0` (the LP→BP substitution
/// `ŝ = (s² + w0²)/(b s)`).
fn solve_band_pair(point: Complex, bandwidth: f64, center: f64) -> [Complex; 2] {
    let b = Complex::new(bandwidth * point.re, bandwidth * point.im);
    let discriminant = b
        .mul(b)
        .sub(Complex::new(4.0 * center * center, 0.0))
        .sqrt();
    [
        b.add(discriminant).scale(0.5),
        b.sub(discriminant).scale(0.5),
    ]
}

/// The two `s`-plane roots of `ŝ s² - b s + ŝ w0² = 0` (the LP→BS substitution
/// `ŝ = b s/(s² + w0²)`).
fn solve_band_stop_pair(point: Complex, bandwidth: f64, center: f64) -> [Complex; 2] {
    let numerator = Complex::new(bandwidth, 0.0);
    let discriminant = Complex::new(bandwidth * bandwidth, 0.0)
        .sub(point.mul(point).scale(4.0 * center * center))
        .sqrt();
    [
        numerator.add(discriminant).div(point.scale(2.0)),
        numerator.sub(discriminant).div(point.scale(2.0)),
    ]
}

// ---------------------------------------------------------------------------
// The parameter table
// ---------------------------------------------------------------------------

/// A parameter's current or default value.
#[derive(Clone, Debug, PartialEq)]
pub enum ParamValue {
    Real(f64),
    Text(&'static str),
}

impl ParamValue {
    fn as_real(&self) -> Option<f64> {
        match self {
            Self::Real(value) => Some(*value),
            Self::Text(_) => None,
        }
    }

    fn as_text(&self) -> Option<&'static str> {
        match self {
            Self::Text(value) => Some(value),
            Self::Real(_) => None,
        }
    }

    fn same_kind(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Self::Real(_), Self::Real(_)) | (Self::Text(_), Self::Text(_))
        )
    }
}

/// One row of the parameter table: the recovered label, the port's parameter
/// name, and the default.
pub struct ParamInfo {
    pub name: &'static str,
    pub label: &'static str,
    pub default: ParamValue,
}

/// The parameter table. The labels and the design/response vocabularies are
/// recovered from the DLL's strings; the numeric defaults are the DLL's own
/// `ParamInfo` defaults (its inlined constructors — see the module docs for the
/// builder addresses), and the parameter names are this port's (documented in
/// the module docs). The five labels marked `(port)` were not in the recovered
/// string list — they name parameters the designs need.
pub const PARAMETER_TABLE: [ParamInfo; 13] = [
    ParamInfo {
        name: "type",
        label: "Filter Type (port)",
        default: ParamValue::Text("Low Pass"),
    },
    ParamInfo {
        name: "design",
        label: "Filter Design (port)",
        default: ParamValue::Text("Butterworth"),
    },
    ParamInfo {
        name: "cutoff",
        label: "Cutoff Frequency",
        default: ParamValue::Real(2000.0),
    },
    ParamInfo {
        name: "center",
        label: "Center Frequency",
        default: ParamValue::Real(2000.0),
    },
    ParamInfo {
        name: "bandwidthHz",
        label: "Bandwidth (Hz)",
        default: ParamValue::Real(1720.0),
    },
    ParamInfo {
        name: "bandwidthOctaves",
        label: "Bandwidth (Octaves)",
        default: ParamValue::Real(1.0),
    },
    ParamInfo {
        name: "order",
        label: "Order",
        default: ParamValue::Real(2.0),
    },
    ParamInfo {
        name: "gain",
        label: "Gain (dB) (port)",
        default: ParamValue::Real(-6.0),
    },
    ParamInfo {
        name: "ripple",
        label: "Passband Ripple (dB) (port)",
        default: ParamValue::Real(0.01),
    },
    ParamInfo {
        name: "stopAttenuation",
        label: "Stopband Attenuation (dB) (port)",
        default: ParamValue::Real(48.0),
    },
    ParamInfo {
        name: "poleAngle",
        label: "Pole Angle",
        default: ParamValue::Real(std::f64::consts::FRAC_PI_2),
    },
    ParamInfo {
        name: "poleDistance",
        label: "Pole Distance",
        default: ParamValue::Real(0.5),
    },
    ParamInfo {
        name: "poleReal",
        label: "Pole Real",
        default: ParamValue::Real(0.25),
    },
];

fn parameter(name: &str) -> Option<&'static ParamInfo> {
    PARAMETER_TABLE.iter().find(|entry| entry.name == name)
}

fn parameter_at(index: i64) -> Option<&'static ParamInfo> {
    usize::try_from(index)
        .ok()
        .and_then(|index| PARAMETER_TABLE.get(index))
}

/// A slot's default in the port's units: the table value, which carries the
/// reference's recovered default.
fn slot_default(name: &str) -> f64 {
    parameter(name)
        .and_then(|entry| entry.default.as_real())
        .unwrap_or(0.0)
}

/// The default of the reference's `Resonance` (Q, builder `0x10001a50`) and of
/// its octave bandwidth (builder `0x10001af0`): both are `1` (the DLL loads
/// `fld1` for each).
const QUALITY_DEFAULT: f64 = 1.0;

/// The default of the reference's shelf `Slope` (builder `0x10001aa0`, `fld1`).
const SLOPE_DEFAULT: f64 = 1.0;

/// The octave bandwidth this port's `quality()` turns back into `q`
/// (`q = 1/(2·sinh(ln2/2·BW))`, inverted). Exact round trip, so a filter chain
/// built from the result reproduces `q`.
fn octaves_for_quality(q: f64) -> std::result::Result<f64, String> {
    if !(q.is_finite() && q > 0.0) {
        return Err(format!(
            "invalid usage of ParamInfo: the reference's `{q}` has no positive bandwidth equivalent"
        ));
    }
    Ok((2.0 / std::f64::consts::LN_2) * (1.0 / (2.0 * q)).asinh())
}

/// What one slot of the reference's positional `setParams` vector addresses.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Slot {
    /// Slot 0 of every design: the filter's sample rate (`Params[0]`, which the
    /// DSPFilters design classes hand to their `setup` as the rate).
    SampleRate,
    /// One of this port's parameters, whose units are the reference's already;
    /// its default is the table's recovered value.
    Parameter(&'static str),
    /// The reference's `Resonance` (Q) — RBJ TypeI. Stored as the octave
    /// bandwidth that reproduces `Q`.
    Resonance,
    /// The reference's octave bandwidth for the band responses — RBJ TypeII,
    /// which the library feeds into the same `sn/(2·x)` position as the Q.
    /// Stored the same way, so the chain matches the library's `AL`.
    BandwidthAsQuality,
    /// The reference's shelf slope — RBJ TypeIII, whose `AL` is the library's
    /// `sqrt((A + 1/A)(1/S − 1) + 2)` form. Stored verbatim; the port's shelf
    /// designer reads it together with the gain.
    ShelfSlope,
}

/// The port's names for the reference's slot vector, slot 0 — the sample rate —
/// first, then the current response/design's parameters in the order the
/// DSPFilters design classes declare them (see the module docs for the DLL
/// addresses and the library headers behind each order).
pub fn positional_slots(response: ResponseType, design: DesignKind) -> Vec<Slot> {
    let mut slots = vec![Slot::SampleRate];
    let band = response.is_band();
    let mut push = |slot: Slot| slots.push(slot);
    match design {
        // The RBJ responses carry no order. The library feeds the value after
        // the frequency (and the shelf gain) into `sin/(2·x)`: the Q for
        // TypeI, the octave bandwidth for TypeII and the shelf slope for
        // TypeIII. TypeIV's bandwidth goes through the library's octave
        // `sinh` form and stays a raw octave value.
        DesignKind::Rbj => {
            if band {
                push(Slot::Parameter("center"));
            } else {
                push(Slot::Parameter("cutoff"));
            }
            if response.is_shelf() {
                push(Slot::Parameter("gain"));
            }
            match response {
                ResponseType::BandShelf => push(Slot::Parameter("bandwidthOctaves")),
                ResponseType::LowShelf | ResponseType::HighShelf => push(Slot::ShelfSlope),
                ResponseType::BandPass | ResponseType::BandStop => push(Slot::BandwidthAsQuality),
                ResponseType::LowPass | ResponseType::HighPass => push(Slot::Resonance),
            }
        }
        // The DLL's Custom layouts were not recovered; this is the port's
        // placement.
        DesignKind::CustomOnePole | DesignKind::CustomTwoPole => {
            if band {
                push(Slot::Parameter("center"));
            } else {
                push(Slot::Parameter("cutoff"));
            }
            push(Slot::Parameter("poleAngle"));
            push(Slot::Parameter("poleDistance"));
            push(Slot::Parameter("poleReal"));
        }
        // The prototype designs share the library's `OrderBase`: `Order` first,
        // then the response's frequencies, then `gain` for the shelves, then
        // the design's extras.
        _ => {
            push(Slot::Parameter("order"));
            if band {
                push(Slot::Parameter("center"));
                push(Slot::Parameter("bandwidthHz"));
            } else {
                push(Slot::Parameter("cutoff"));
            }
            if response.is_shelf() {
                push(Slot::Parameter("gain"));
            }
            match design {
                DesignKind::ChebyshevI | DesignKind::Elliptic => push(Slot::Parameter("ripple")),
                DesignKind::ChebyshevII => push(Slot::Parameter("stopAttenuation")),
                _ => {}
            }
            if matches!(design, DesignKind::Elliptic) {
                push(Slot::Parameter("stopAttenuation"));
            }
        }
    }
    slots
}

/// The live parameter set of one filter: the table defaults with the script's
/// overrides applied, plus the designed chain.
struct WaveDspState {
    values: BTreeMap<&'static str, ParamValue>,
    chain: DesignedFilter,
    sample_rate: f32,
}

impl WaveDspState {
    fn new(sample_rate: f32) -> std::result::Result<Self, DesignError> {
        let values = PARAMETER_TABLE
            .iter()
            .map(|entry| (entry.name, entry.default.clone()))
            .collect();
        let mut state = Self {
            values,
            chain: DesignedFilter::default(),
            sample_rate,
        };
        state.chain = DesignedFilter::design(&state.spec())?;
        Ok(state)
    }

    fn value(&self, name: &str) -> Option<&ParamValue> {
        self.values.get(name)
    }

    fn real(&self, name: &str) -> f64 {
        self.value(name)
            .and_then(ParamValue::as_real)
            .unwrap_or(0.0)
    }

    fn text(&self, name: &str) -> &'static str {
        self.value(name).and_then(ParamValue::as_text).unwrap_or("")
    }

    /// The response the live chain was designed for.
    fn response(&self) -> ResponseType {
        ResponseType::from_name(self.text("type")).unwrap_or(ResponseType::LowPass)
    }

    /// The design the live chain was designed for.
    fn design(&self) -> DesignKind {
        DesignKind::from_name(self.text("design")).unwrap_or(DesignKind::Butterworth)
    }

    fn spec(&self) -> FilterSpec {
        FilterSpec {
            response: self.response(),
            design: self.design(),
            sample_rate: self.sample_rate,
            cutoff: self.real("cutoff") as f32,
            center: self.real("center") as f32,
            bandwidth_hz: self.real("bandwidthHz") as f32,
            bandwidth_octaves: self.real("bandwidthOctaves") as f32,
            order: self.real("order").max(1.0) as u32,
            gain_db: self.real("gain") as f32,
            ripple: self.real("ripple"),
            stop_attenuation: self.real("stopAttenuation"),
            pole_angle: self.real("poleAngle") as f32,
            pole_distance: self.real("poleDistance") as f32,
            pole_real: self.real("poleReal") as f32,
        }
    }

    /// Rebuilds the chain from the current values, used when several
    /// parameters change at once — the constructor's response/design pair
    /// cannot be applied one at a time, because the intermediate combination
    /// may be one the designer refuses.
    fn redesign(&mut self) -> std::result::Result<(), String> {
        match DesignedFilter::design(&self.spec()) {
            Ok(chain) => {
                self.chain = chain;
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        }
    }

    /// Slot 0 of the positional form: the rate the chain is designed against.
    /// A refused rate leaves the previous one (and the working chain) alone.
    fn set_sample_rate(&mut self, rate: f64) -> std::result::Result<(), String> {
        if !rate.is_finite() || rate <= 0.0 {
            return Err(format!(
                "invalid usage of ParamInfo: `{rate}` is not a sample rate"
            ));
        }
        let previous = self.sample_rate;
        self.sample_rate = rate as f32;
        match DesignedFilter::design(&self.spec()) {
            Ok(chain) => {
                self.chain = chain;
                Ok(())
            }
            Err(error) => {
                self.sample_rate = previous;
                Err(error.to_string())
            }
        }
    }

    /// Stores one parameter. A value of the wrong kind, an unknown name or an
    /// unknown design/response is the recovered `invalid usage of ParamInfo`
    /// error; a design the port does not implement reports its own message.
    fn set(&mut self, name: &str, value: ParamValue) -> std::result::Result<(), String> {
        let Some(entry) = parameter(name) else {
            return Err(format!(
                "invalid usage of ParamInfo: unknown parameter `{name}`"
            ));
        };
        if !entry.default.same_kind(&value) {
            return Err(format!(
                "invalid usage of ParamInfo: parameter `{name}` expects {}",
                match entry.default {
                    ParamValue::Real(_) => "a number",
                    ParamValue::Text(_) => "a string",
                }
            ));
        }
        if let ParamValue::Text(text) = value {
            let valid = match name {
                "type" => ResponseType::from_name(text).is_some(),
                "design" => DesignKind::from_name(text).is_some(),
                _ => true,
            };
            if !valid {
                return Err(format!(
                    "invalid usage of ParamInfo: unknown value `{text}` for `{name}`"
                ));
            }
        }
        let previous = self.values.insert(entry.name, value);
        match DesignedFilter::design(&self.spec()) {
            Ok(chain) => {
                self.chain = chain;
                Ok(())
            }
            Err(error) => {
                // Roll the parameter back so the object keeps a working chain.
                if let Some(previous) = previous {
                    self.values.insert(entry.name, previous);
                }
                Err(error.to_string())
            }
        }
    }
}

thread_local! {
    /// Live filters by object handle, dropped by `finalize`.
    static FILTERS: RefCell<BTreeMap<ObjectHandle, WaveDspState>> =
        const { RefCell::new(BTreeMap::new()) };
}

// ---------------------------------------------------------------------------
// TJS surface
// ---------------------------------------------------------------------------

fn install_wf_typical_dsp(runtime: &mut Runtime<KrkrHost>) {
    FILTERS.with(|filters| filters.borrow_mut().clear());
    let class = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<KrkrHost>, _this: Option<ObjectHandle>, args: Vec<Variant>| {
            let instance = runtime.alloc_ordinary_object();
            runtime.add_object_class_info(instance, "WaveDSPFilter");
            runtime.set_object_member(
                instance,
                "__className",
                Variant::String("WaveDSPFilter".to_string()),
            );
            if let Some(class) = filter_class(runtime) {
                runtime.set_object_super_class(instance, class);
            }
            match WaveDspState::new(SAMPLE_RATE_DEFAULT as f32) {
                Ok(mut state) => {
                    // The reference reads the constructor arguments before the
                    // instance is usable; a refused `new` must not leave a live
                    // filter behind.
                    apply_constructor_arguments(&mut state, &args)?;
                    FILTERS.with(|filters| filters.borrow_mut().insert(instance, state));
                }
                Err(error) => {
                    return Err(TjsError::runtime(format!(
                        "WaveDSPFilter cannot build its default filter: {error}"
                    )));
                }
            }
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(class, "WaveDSPFilter");
    install_members(runtime, class);
    publish_filter_class(runtime, class);
}

/// The reference's constructor arguments: the class-name NCM (`0x10001e00`)
/// forwards them to the instance's first virtual method (`0x100020f0`), which
/// reads up to four — response name, design name, direct-form name and an
/// integer it stores at `instance+0xd0`. Names go through the DLL's lookup
/// tables (`0x100cbd38` responses, `0x100cbda0` designs, `0x100cbdf8` forms),
/// whose miss path leaves the field unchanged. This port applies the first two
/// as its `type` and `design`; the form and the integer are accepted without
/// effect — the port's chain is a single direct form, and the integer's role in
/// the DLL was not recovered. PARQUET builds every filter as
/// `new WaveSoundBuffer.WaveDSPFilter(args[0], args[1])`, so the two names are
/// what a real game passes.
///
/// The reference's table carries three response names this port cannot model
/// (`AllPass`, `BandPass1`, `BandPass2`); naming one is the port's
/// `does not model` error, like the dictionary/pairs path, not a silent keep.
fn apply_constructor_arguments(state: &mut WaveDspState, args: &[Variant]) -> Result<()> {
    let mut named = false;
    for (index, name) in [(0usize, "type"), (1, "design")] {
        let Some(argument) = args.get(index) else {
            continue;
        };
        if matches!(argument, Variant::Void) {
            continue;
        }
        let text = argument.to_tjs_string()?;
        if name == "type" && UNMODELLED_RESPONSE_IDENTIFIERS.contains(&text.as_str()) {
            return Err(TjsError::runtime(format!(
                "invalid usage of ParamInfo: `{text}` is a WaveDSPFilter identifier this port does not model"
            )));
        }
        let canonical = if name == "type" {
            canonical_response(&text)
        } else {
            canonical_design(&text)
        };
        // A name the table does not carry leaves the field unchanged; a name
        // that resolves is applied, and a design this port cannot build
        // reports itself instead of silently keeping the previous one.
        let Some(canonical) = canonical else {
            continue;
        };
        state.values.insert(name, ParamValue::Text(canonical));
        named = true;
    }
    // Both names land before the chain is rebuilt: an intermediate pair (a
    // shelf response with the previous design, say) may be one the designer
    // refuses even though the final pair is fine.
    if named {
        state.redesign().map_err(TjsError::runtime)?;
    }
    Ok(())
}

/// Publishes the class where the reference's binder puts it: as a member of the
/// `WaveSoundBuffer` class object.
///
/// The DLL's chain is `BindUtil(TJS_W("WaveSoundBuffer"), link)
/// .Class(TJS_W("WaveDSPFilter"), …)` — the image constructs both strings
/// (`0x100a554d`, `0x100a557c`) before linking the class object into the
/// `WaveSoundBuffer` object, and never registers a global of the class name.
/// PARQUET's `voiceeffect.tjs` object 2 reads `WaveSoundBuffer.WaveDSPFilter`
/// and `new`s it.
fn publish_filter_class(runtime: &mut Runtime<KrkrHost>, class: ObjectHandle) {
    if let Some(wave) = runtime.global_member("WaveSoundBuffer").object_handle() {
        runtime.set_object_member(wave, "WaveDSPFilter", Variant::Object(class));
    }
}

/// The class object behind `WaveSoundBuffer.WaveDSPFilter`, the one place
/// [`publish_filter_class`] puts it.
fn filter_class(runtime: &Runtime<KrkrHost>) -> Option<ObjectHandle> {
    let wave = runtime.global_member("WaveSoundBuffer").object_handle()?;
    runtime.object_member(wave, "WaveDSPFilter").object_handle()
}

fn install_members(runtime: &mut Runtime<KrkrHost>, handle: ObjectHandle) {
    runtime.register_object_native(handle, "finalize", filter_finalize);
    runtime.register_object_native(handle, "getParamInfo", filter_get_param_info);
    runtime.register_object_native(handle, "setParams", filter_set_params);
    runtime.register_object_native(handle, "currentValue", filter_current_value);
    runtime.register_object_native(handle, "defaultValue", filter_default_value);
    runtime.register_object_native_property_with_access(
        handle,
        "name",
        NativePropertyAccess::ReadOnly,
        |_runtime, this| {
            let name = this
                .and_then(|this| with_state(this, |state| state.text("type")))
                .unwrap_or_default();
            Ok(Variant::String(name.to_string()))
        },
        |_runtime, _this, _value| Err(TjsError::access_denied()),
    );
    runtime.register_object_native_property_with_access(
        handle,
        "label",
        NativePropertyAccess::ReadOnly,
        |_runtime, this| {
            let label = this
                .and_then(|this| with_state(this, |state| state.text("design")))
                .unwrap_or_default();
            Ok(Variant::String(label.to_string()))
        },
        |_runtime, _this, _value| Err(TjsError::access_denied()),
    );
    runtime.register_object_native_property_with_access(
        handle,
        "interface",
        NativePropertyAccess::ReadOnly,
        |_runtime, _this| Ok(Variant::Integer(INTERFACE_SENTINEL)),
        |_runtime, _this, _value| Err(TjsError::access_denied()),
    );
}

fn plugin_this(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Option<ObjectHandle> {
    this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle))
}

/// Looks a filter up by identity, raising the recovered `ClassID mismatched:`
/// error when the member runs on an object that is not a live filter.
fn live_state(this: ObjectHandle) -> bool {
    FILTERS.with(|filters| filters.borrow().contains_key(&this))
}

fn with_state<R>(handle: ObjectHandle, f: impl FnOnce(&mut WaveDspState) -> R) -> Option<R> {
    FILTERS.with(|filters| filters.borrow_mut().get_mut(&handle).map(f))
}

fn filter_finalize(
    _runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(this) = this_obj {
        FILTERS.with(|filters| filters.borrow_mut().remove(&this));
    }
    Ok(Variant::Void)
}

/// The `index/name` argument the parameter readers accept.
fn parameter_argument(_runtime: &Runtime<KrkrHost>, value: &Variant) -> Result<&'static ParamInfo> {
    let found = match value {
        Variant::Integer(index) => parameter_at(*index),
        Variant::Real(index) => parameter_at(*index as i64),
        value => {
            let name = value.to_tjs_string()?;
            parameter(&name)
        }
    };
    found.ok_or_else(|| {
        TjsError::runtime("invalid usage of ParamInfo: no such parameter".to_string())
    })
}

fn param_value_variant(value: &ParamValue) -> Variant {
    match value {
        ParamValue::Real(value) => Variant::Real(*value),
        ParamValue::Text(value) => Variant::String((*value).to_string()),
    }
}

fn filter_get_param_info(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = plugin_this(runtime, this_obj).ok_or_else(|| {
        TjsError::runtime("invalid usage of ParamInfo: getParamInfo requires this".to_string())
    })?;
    if !FILTERS.with(|filters| filters.borrow().contains_key(&this)) {
        return Err(TjsError::runtime(
            "ClassID mismatched: WaveDSPFilter state is gone".to_string(),
        ));
    }
    let argument = args.first().ok_or_else(|| {
        TjsError::runtime(
            "invalid usage of ParamInfo: getParamInfo needs an index or name".to_string(),
        )
    })?;
    let entry = parameter_argument(runtime, argument)?;
    let current = with_state(this, |state| {
        state
            .value(entry.name)
            .cloned()
            .unwrap_or_else(|| entry.default.clone())
    })
    .unwrap_or_else(|| entry.default.clone());
    let record = runtime.alloc_dictionary_object();
    runtime.set_object_member(record, "name", Variant::String(entry.name.to_string()));
    runtime.set_object_member(record, "label", Variant::String(entry.label.to_string()));
    runtime.set_object_member(record, "currentValue", param_value_variant(&current));
    runtime.set_object_member(record, "defaultValue", param_value_variant(&entry.default));
    Ok(Variant::Object(record))
}

fn filter_current_value(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = plugin_this(runtime, this_obj).ok_or_else(|| {
        TjsError::runtime("invalid usage of ParamInfo: currentValue requires this".to_string())
    })?;
    let argument = args.first().ok_or_else(|| {
        TjsError::runtime(
            "invalid usage of ParamInfo: currentValue needs an index or name".to_string(),
        )
    })?;
    let entry = parameter_argument(runtime, argument)?;
    let value = with_state(this, |state| {
        state
            .value(entry.name)
            .cloned()
            .unwrap_or_else(|| entry.default.clone())
    })
    .unwrap_or_else(|| entry.default.clone());
    Ok(param_value_variant(&value))
}

fn filter_default_value(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let argument = args.first().ok_or_else(|| {
        TjsError::runtime(
            "invalid usage of ParamInfo: defaultValue needs an index or name".to_string(),
        )
    })?;
    let entry = parameter_argument(runtime, argument)?;
    Ok(param_value_variant(&entry.default))
}

/// `setParams(...)`: the reference's positional form, or this port's dictionary
/// / name-value pair extension. A rejected value leaves the parameter (and the
/// chain) untouched and reports the recovered error message. The call answers
/// the slot count either way, which is the reference's return value: its
/// callback stores `getNumParams()` into the result variant (`0x10001ec8`).
fn filter_set_params(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let this = plugin_this(runtime, this_obj).ok_or_else(|| {
        TjsError::runtime("invalid usage of ParamInfo: setParams requires this".to_string())
    })?;
    if let Some(Variant::Object(dictionary)) = args.first() {
        let mut assignments: Vec<(String, ParamValue)> = Vec::new();
        for (name, value) in runtime.object_members(*dictionary) {
            assignments.push((name, param_value_from_variant(&value)?));
        }
        apply_assignments(this, assignments)?;
        return Ok(Variant::Integer(slot_count(this)?));
    }
    if matches!(args.first(), Some(Variant::String(_))) {
        if !args.len().is_multiple_of(2) {
            return Err(TjsError::runtime(
                "invalid usage of ParamInfo: setParams needs name/value pairs or a dictionary"
                    .to_string(),
            ));
        }
        let mut assignments: Vec<(String, ParamValue)> = Vec::new();
        let mut index = 0;
        while index + 1 < args.len() {
            let name = args[index].to_tjs_string()?;
            assignments.push((name, param_value_from_variant(&args[index + 1])?));
            index += 2;
        }
        apply_assignments(this, assignments)?;
        return Ok(Variant::Integer(slot_count(this)?));
    }
    set_positional_params(this, &args)
}

fn apply_assignments(this: ObjectHandle, assignments: Vec<(String, ParamValue)>) -> Result<()> {
    for (name, value) in assignments {
        if let Err(message) = with_state(this, |state| state.set(&name, value)).unwrap_or(Ok(())) {
            return Err(TjsError::runtime(message));
        }
    }
    Ok(())
}

/// The count the reference's `setParams` answers with: the live filter object's
/// parameter count, which is this port's slot count for the current
/// response/design.
fn slot_count(this: ObjectHandle) -> Result<i64> {
    let count = with_state(this, |state| {
        positional_slots(state.response(), state.design()).len()
    })
    .ok_or_else(|| {
        TjsError::runtime("ClassID mismatched: WaveDSPFilter state is gone".to_string())
    })?;
    Ok(count as i64)
}

/// The reference's positional form
/// (`0x10002a30` — see the module docs): slot 0 is the sample rate, then the
/// current response/design's parameters. A `void` or missing argument restores
/// the slot's default, arguments past the eighth slot are ignored, and the
/// answer is the slot count.
fn set_positional_params(this: ObjectHandle, args: &[Variant]) -> Result<Variant> {
    let (response, design) = with_state(this, |state| (state.response(), state.design()))
        .ok_or_else(|| {
            TjsError::runtime("ClassID mismatched: WaveDSPFilter state is gone".to_string())
        })?;
    let slots = positional_slots(response, design);
    for (index, slot) in slots.iter().enumerate().take(MAX_POSITIONAL_SLOTS) {
        let supplied = match args.get(index) {
            None | Some(Variant::Void) => None,
            Some(argument) => Some(argument.to_real()?),
        };
        let result = match slot {
            Slot::SampleRate => {
                let rate = supplied.unwrap_or(SAMPLE_RATE_DEFAULT);
                with_state(this, |state| state.set_sample_rate(rate)).unwrap_or(Ok(()))
            }
            // The reference's units here are this port's already; its default
            // is the table's recovered value.
            Slot::Parameter(name) => {
                let value = supplied.unwrap_or_else(|| slot_default(name));
                with_state(this, |state| state.set(name, ParamValue::Real(value))).unwrap_or(Ok(()))
            }
            // The library feeds the value into `sn/(2·x)`, where this port's
            // `quality()` goes: store the octave bandwidth that reproduces it.
            Slot::Resonance | Slot::BandwidthAsQuality => {
                match octaves_for_quality(supplied.unwrap_or(QUALITY_DEFAULT)) {
                    Ok(octaves) => with_state(this, |state| {
                        state.set("bandwidthOctaves", ParamValue::Real(octaves))
                    })
                    .unwrap_or(Ok(())),
                    Err(message) => Err(message),
                }
            }
            // The shelf slope is stored verbatim: the port's shelf branch reads
            // it with the gain the way the library's `AL` does, so a later gain
            // change re-derives the shelf as the reference would.
            Slot::ShelfSlope => {
                let slope = supplied.unwrap_or(SLOPE_DEFAULT);
                with_state(this, |state| {
                    state.set("bandwidthOctaves", ParamValue::Real(slope))
                })
                .unwrap_or(Ok(()))
            }
        };
        if let Err(message) = result {
            return Err(TjsError::runtime(message));
        }
    }
    Ok(Variant::Integer(slots.len() as i64))
}

fn param_value_from_variant(value: &Variant) -> Result<ParamValue> {
    match value {
        Variant::String(text) => {
            // Both text parameters accept the canonical spellings and the
            // DLL's own space-less identifiers; anything else is the recovered
            // ParamInfo error (with the DLL's unmodelled identifiers named).
            if let Some(canonical) = canonical_response(text).or_else(|| canonical_design(text)) {
                return Ok(ParamValue::Text(canonical));
            }
            if UNMODELLED_RESPONSE_IDENTIFIERS
                .iter()
                .any(|identifier| identifier == text)
            {
                return Err(TjsError::runtime(format!(
                    "invalid usage of ParamInfo: `{text}` is a WaveDSPFilter identifier this port does not model"
                )));
            }
            Err(TjsError::runtime(format!(
                "invalid usage of ParamInfo: unknown value `{text}`"
            )))
        }
        Variant::Void | Variant::Null => Err(TjsError::runtime(
            "invalid usage of ParamInfo: a parameter value is required".to_string(),
        )),
        value => Ok(ParamValue::Real(value.to_real()?)),
    }
}

// ---------------------------------------------------------------------------
// The plugin's public processing surface
// ---------------------------------------------------------------------------

/// Runs the designed chain behind a live `WaveDSPFilter` object.
pub fn process_filter(handle: ObjectHandle, frames: &mut [f32], channels: usize) -> bool {
    with_state(handle, |state| {
        state.chain.process(frames, channels);
        true
    })
    .unwrap_or(false)
}

/// The chain's magnitude at a frequency, for a future engine-side inspection
/// (and for tests).
pub fn filter_magnitude_at_hz(handle: ObjectHandle, frequency: f64) -> Option<f64> {
    with_state(handle, |state| {
        state.chain.magnitude_at_hz(frequency, state.sample_rate)
    })
}

#[cfg(test)]
mod tests {
    use krkr_engine::{EngineConfig, KrkrEngine};
    use krkr_tjs2::runtime::Variant;

    use super::*;

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(WfTypicalDspPlugin).expect("plugin");
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

    /// A spec with the given design/response at 48 kHz, everything else at the
    /// table defaults.
    fn spec(design: DesignKind, response: ResponseType) -> FilterSpec {
        FilterSpec {
            response,
            design,
            sample_rate: 48000.0,
            cutoff: 1000.0,
            center: 1000.0,
            bandwidth_hz: 200.0,
            bandwidth_octaves: 0.0,
            order: 2,
            gain_db: 0.0,
            ripple: 1.0,
            stop_attenuation: 40.0,
            pole_angle: 0.0,
            pole_distance: 0.5,
            pole_real: -0.5,
        }
    }

    fn response_of(spec: &FilterSpec, frequency: f64) -> f64 {
        DesignedFilter::design(spec)
            .expect("design")
            .magnitude_at_hz(frequency, spec.sample_rate)
    }

    fn impulse_response(spec: &FilterSpec, length: usize) -> Vec<f32> {
        let mut filter = DesignedFilter::design(spec).expect("design");
        let mut frames = vec![0.0_f32; length];
        frames[0] = 1.0;
        filter.process(&mut frames, 1);
        frames
    }

    /// A `Q = 1/sqrt(2)` band is `bandwidthHz = f0 * sqrt(2)` with the octaves
    /// form off: `quality()` is `frequency / bandwidth_hz`.
    fn butterworth_q(spec: &mut FilterSpec) {
        spec.bandwidth_hz = spec.cutoff * std::f32::consts::SQRT_2;
        spec.bandwidth_octaves = 0.0;
    }

    const SQRT_HALF: f64 = std::f64::consts::FRAC_1_SQRT_2;

    // ------------------------------------------------------------- surface

    /// The class object a game reaches: the binder anchors `WaveDSPFilter` on
    /// `WaveSoundBuffer` (`voiceeffect.tjs` object 2 does `gpd
    /// WaveSoundBuffer`, `gpd .WaveDSPFilter`, `new`), so the probes below
    /// spell it that way.
    #[test]
    fn the_class_carries_every_recovered_member() {
        let mut engine = engine();
        for member in [
            "name",
            "label",
            "getParamInfo",
            "setParams",
            "currentValue",
            "defaultValue",
            "interface",
            "finalize",
        ] {
            let probe = string(
                &mut engine,
                &format!(
                    "(function() {{ return typeof WaveSoundBuffer.WaveDSPFilter.{member}; }})()"
                ),
            );
            assert_ne!(
                probe, "undefined",
                "WaveSoundBuffer.WaveDSPFilter.{member} is not installed"
            );
        }
    }

    /// The class-object path PARQUET's `voiceeffect.tjs` uses: its `DSP` helper
    /// resolves `global.WaveSoundBuffer` → `.WaveDSPFilter` and `new`s it with
    /// the response/design arguments. The member has to be there — the game's
    /// own guards (`typeof` before the lookup) turn a missing one into a silent
    /// no-op — and it is the class object's only home: the bare global stays
    /// absent, like the reference's.
    #[test]
    fn the_class_object_path_builds_a_filter_the_game_way() {
        let mut engine = engine();
        let value = string(
            &mut engine,
            "(function() {\n\
                 var dsp = function(a0, a1) {\n\
                     var t2 = global.WaveSoundBuffer;\n\
                     var t3 = t2.WaveDSPFilter;\n\
                     return new t3(a0, a1);\n\
                 };\n\
                 var filter = dsp(\"LowPass\", \"Butterworth\");\n\
                 return typeof WaveSoundBuffer.WaveDSPFilter + \":\" + filter.name + \":\" +\n\
                     (typeof global.WaveDSPFilter);\n\
             })()",
        );
        assert_eq!(value, "Object:Low Pass:undefined");
    }

    /// `getParamInfo` answers the record shape (name/label/currentValue/
    /// defaultValue) by index and by name, with the table's defaults.
    #[test]
    fn get_param_info_answers_records_by_index_and_name() {
        let mut engine = engine();
        let value = string(
            &mut engine,
            "(function() {\n\
                 var filter = new WaveSoundBuffer.WaveDSPFilter();\n\
                 var by_index = filter.getParamInfo(0);\n\
                 var by_name = filter.getParamInfo(\"cutoff\");\n\
                 return by_index.name + \"/\" + by_index.label + \"/\" + by_index.currentValue + \"/\" + by_index.defaultValue +\n\
                     \"|cutoff:\" + by_name.label + \"/\" + by_name.currentValue +\n\
                     \"|order:\" + filter.currentValue(\"order\") + \"/\" + filter.defaultValue(\"order\");\n\
             })()",
        );
        assert_eq!(
            value,
            "type/Filter Type (port)/Low Pass/Low Pass|cutoff:Cutoff Frequency/2000|order:2/2"
        );
        assert_eq!(
            string(
                &mut engine,
                "(function() { var f = new WaveSoundBuffer.WaveDSPFilter(); return f.name + \"/\" + f.label; })()"
            ),
            "Low Pass/Butterworth"
        );
    }

    /// `setParams` takes a dictionary or flat pairs and the read-back follows;
    /// the recovered `invalid usage of ParamInfo` message covers unknown names,
    /// wrong value kinds and odd argument lists.
    #[test]
    fn set_params_accepts_dictionaries_and_pairs_and_rejects_bad_usage() {
        let mut engine = engine();
        let value = string(
            &mut engine,
            "(function() {\n\
                 var filter = new WaveSoundBuffer.WaveDSPFilter();\n\
                 filter.setParams(%[type: \"High Pass\", cutoff: 250]);\n\
                 filter.setParams(\"design\", \"Chebyshev I\", \"order\", 4);\n\
                 return filter.currentValue(\"type\") + \"/\" + filter.currentValue(\"cutoff\") +\n\
                     \"/\" + filter.currentValue(\"design\") + \"/\" + filter.name + \"/\" + filter.label;\n\
             })()",
        );
        assert_eq!(value, "High Pass/250/Chebyshev I/High Pass/Chebyshev I");

        for call in [
            "filter.setParams(\"nope\", 1)",
            "filter.setParams(\"cutoff\", \"high\")",
            "filter.setParams(\"type\", \"Nonsense\")",
            "filter.setParams(\"cutoff\")",
        ] {
            let error = try_run(
                &mut engine,
                &format!(
                    "(function() {{ var filter = new WaveSoundBuffer.WaveDSPFilter(); {call}; return 0; }})()"
                ),
            )
            .expect_err("bad ParamInfo usage must fail");
            assert!(
                error.message.contains("invalid usage of ParamInfo"),
                "{call} reported {:?}",
                error.message
            );
        }
    }

    /// The reference's positional contract, and PARQUET's own call shape:
    /// `new WaveSoundBuffer.WaveDSPFilter(type, design)` followed by
    /// `setParams(void, v0..v4)`. Slot 0 is the sample rate (why the game's
    /// first argument is `void`), a `void` or missing argument restores the
    /// slot's default, arguments past the eighth slot are ignored, and the
    /// answer is the slot count.
    #[test]
    fn the_positional_form_takes_the_reference_slot_contract() {
        let mut engine = engine();
        // `DSP("LowPass", "RBJ")` then `setParams(void, cutoff, q)`.
        assert_eq!(
            string(
                &mut engine,
                "(function() {\n\
                     var f = new WaveSoundBuffer.WaveDSPFilter(\"LowPass\", \"RBJ\");\n\
                     var count = f.setParams(void, 2000, 0.7);\n\
                     return count + \"/\" + f.name + \"/\" + f.label;\n\
                 })()",
            ),
            "3/Low Pass/RBJ"
        );
        // The Q slot is stored as the octave bandwidth that reproduces 0.7, so
        // the chain's Q comes back as the script's value.
        let (cutoff, quality) = positional_probe(&mut engine, "f.setParams(void, 2000, 0.7);");
        assert_eq!(cutoff, 2000.0);
        assert!((quality - 0.7).abs() < 1e-6, "Q came back as {quality}");

        // A `void` slot restores the reference's default, not the previous
        // value, and missing trailing arguments behave the same way: the cutoff
        // returns to the recovered 2000 Hz and the Q to the library's
        // `Resonance` default of 1.
        let (cutoff, quality) = positional_probe(
            &mut engine,
            "f.setParams(void, 2000, 0.7); f.setParams(void, void, void);",
        );
        assert_eq!(cutoff, 2000.0);
        assert!(
            (quality - 1.0).abs() < 1e-6,
            "the void Q slot gave {quality}"
        );

        // Arguments past the slot count (and past the eighth slot) are ignored;
        // the game's own six-argument call — `setParams(void, v0..v4)` with the
        // trailing ones void registers — is the same case.
        for call in [
            "f.setParams(void, 2000, 0.7, void, void, void)",
            "f.setParams(void, 2000, 0.7, 5, 6, 7, 8, 9, 10, 11)",
        ] {
            assert_eq!(
                string(
                    &mut engine,
                    &format!(
                        "(function() {{\n\
                             var f = new WaveSoundBuffer.WaveDSPFilter(\"LowPass\", \"RBJ\");\n\
                             var count = {call};\n\
                             return count + \"/\" + f.currentValue(\"cutoff\");\n\
                         }})()"
                    ),
                ),
                "3/2000",
                "{call}"
            );
            let (_, quality) = positional_probe(&mut engine, &format!("{call};"));
            assert!((quality - 0.7).abs() < 1e-6, "{call} gave Q {quality}");
        }
    }

    /// Runs `statements` against a fresh script-built `Low Pass`/`RBJ` filter
    /// and answers its cutoff and the Q its chain was designed with.
    fn positional_probe(engine: &mut KrkrEngine, statements: &str) -> (f64, f64) {
        let handle = match try_run(
            engine,
            &format!(
                "(function() {{\n\
                     var f = new WaveSoundBuffer.WaveDSPFilter(\"LowPass\", \"RBJ\");\n\
                     {statements}\n\
                     return f;\n\
                 }})()"
            ),
        )
        .expect("the positional call must be accepted")
        {
            Variant::Object(handle) => handle,
            Variant::Closure(closure) => closure.object,
            other => panic!("the script returned {other:?}"),
        };
        with_state(handle, |state| {
            (state.real("cutoff"), state.spec().quality())
        })
        .expect("the filter is live")
    }

    /// Slot 0 is the sample rate the chain is designed against, and slot 1 of a
    /// Butterworth low pass is the order (`Butterworth.h`: `OrderBase` puts
    /// `Order` at `getParamInfo_1`, `TypeI` the cutoff at `getParamInfo_2`).
    #[test]
    fn the_positional_sample_rate_and_order_slots_land() {
        let mut engine = engine();
        let handle = match try_run(
            &mut engine,
            "(function() {\n\
                 var f = new WaveSoundBuffer.WaveDSPFilter(\"LowPass\", \"Butterworth\");\n\
                 f.setParams(22050, 4, 800);\n\
                 return f;\n\
             })()",
        )
        .expect("the positional call must be accepted")
        {
            Variant::Object(handle) => handle,
            Variant::Closure(closure) => closure.object,
            other => panic!("the script returned {other:?}"),
        };
        let (rate, order, cutoff) = with_state(handle, |state| {
            (state.sample_rate, state.real("order"), state.real("cutoff"))
        })
        .expect("the filter is live");
        assert_eq!(rate, 22050.0);
        assert_eq!(order, 4.0);
        assert_eq!(cutoff, 800.0);
    }

    /// The port's mapping of the slot vector: slot 0 is the sample rate, then
    /// the parameters in the order the DSPFilters design classes declare them,
    /// with the three `sin/(2·x)` slots marked for conversion.
    #[test]
    fn the_positional_slot_order_follows_the_design_classes() {
        // RBJ's `TypeI`: sample rate, cutoff, Q.
        assert_eq!(
            positional_slots(ResponseType::LowPass, DesignKind::Rbj),
            vec![Slot::SampleRate, Slot::Parameter("cutoff"), Slot::Resonance,]
        );
        // RBJ's `TypeII`: the octave bandwidth goes through the same
        // `sin/(2·x)` position as the Q.
        assert_eq!(
            positional_slots(ResponseType::BandPass, DesignKind::Rbj),
            vec![
                Slot::SampleRate,
                Slot::Parameter("center"),
                Slot::BandwidthAsQuality,
            ]
        );
        // RBJ's `TypeIII` (Low/High Shelf): cutoff, gain, slope.
        assert_eq!(
            positional_slots(ResponseType::HighShelf, DesignKind::Rbj),
            vec![
                Slot::SampleRate,
                Slot::Parameter("cutoff"),
                Slot::Parameter("gain"),
                Slot::ShelfSlope,
            ]
        );
        // RBJ's `TypeIV` (Band Shelf): center, gain, bandwidth — the library
        // uses this one in its `sinh` octave form, so it stays raw.
        assert_eq!(
            positional_slots(ResponseType::BandShelf, DesignKind::Rbj),
            vec![
                Slot::SampleRate,
                Slot::Parameter("center"),
                Slot::Parameter("gain"),
                Slot::Parameter("bandwidthOctaves"),
            ]
        );
        // The prototypes share `OrderBase` (Order first) and take the library's
        // Hz bandwidth for the band responses.
        assert_eq!(
            positional_slots(ResponseType::BandPass, DesignKind::Butterworth),
            vec![
                Slot::SampleRate,
                Slot::Parameter("order"),
                Slot::Parameter("center"),
                Slot::Parameter("bandwidthHz"),
            ]
        );
        assert_eq!(
            positional_slots(ResponseType::LowShelf, DesignKind::Butterworth),
            vec![
                Slot::SampleRate,
                Slot::Parameter("order"),
                Slot::Parameter("cutoff"),
                Slot::Parameter("gain"),
            ]
        );
        // Chebyshev I's design carries its ripple as the trailing parameter —
        // after `gain` on the shelves, as the library's TypeIII/TypeIV declare
        // it.
        assert_eq!(
            positional_slots(ResponseType::LowPass, DesignKind::ChebyshevI),
            vec![
                Slot::SampleRate,
                Slot::Parameter("order"),
                Slot::Parameter("cutoff"),
                Slot::Parameter("ripple"),
            ]
        );
        assert_eq!(
            positional_slots(ResponseType::LowShelf, DesignKind::ChebyshevI),
            vec![
                Slot::SampleRate,
                Slot::Parameter("order"),
                Slot::Parameter("cutoff"),
                Slot::Parameter("gain"),
                Slot::Parameter("ripple"),
            ]
        );
    }

    /// The constructor consumes its arguments: the DLL's class-name NCM
    /// (`0x100020f0`) reads a response name and a design name, which the game's
    /// factories pass. A name the tables do not carry leaves the field alone,
    /// and the form/integer arguments are accepted.
    #[test]
    fn the_constructor_consumes_the_response_and_design_arguments() {
        let mut engine = engine();
        assert_eq!(
            string(
                &mut engine,
                "(function() {\n\
                     var f = new WaveSoundBuffer.WaveDSPFilter(\"HighPass\", \"ChebyshevII\");\n\
                     return f.name + \"/\" + f.label;\n\
                 })()",
            ),
            "High Pass/Chebyshev II"
        );
        assert_eq!(
            string(
                &mut engine,
                "(function() {\n\
                     var f = new WaveSoundBuffer.WaveDSPFilter(\"Nonsense\", \"Butterworth\", \"DirectFormI\", 4);\n\
                     return f.name + \"/\" + f.label;\n\
                 })()",
            ),
            "Low Pass/Butterworth"
        );

        // The reference's table carries three response names this port cannot
        // model; naming one is the port's `does not model` error, not a silent
        // keep (the game's factory list has an `AllPass_RBJ` wrapper).
        let error = try_run(
            &mut engine,
            "(function() { return new WaveSoundBuffer.WaveDSPFilter(\"AllPass\", \"RBJ\"); })()",
        )
        .expect_err("AllPass is not modelled");
        assert!(
            error.message.contains("AllPass") && error.message.contains("does not model"),
            "unexpected message: {}",
            error.message
        );

        // A `new` naming an unbuildable design fails before the filter is
        // registered, and the class keeps working afterwards.
        let error = try_run(
            &mut engine,
            "(function() { return new WaveSoundBuffer.WaveDSPFilter(\"LowPass\", \"Elliptic\"); })()",
        )
        .expect_err("Elliptic must be refused at construction");
        assert!(error.message.contains("Elliptic"), "{}", error.message);
        assert_eq!(
            string(
                &mut engine,
                "(function() { var f = new WaveSoundBuffer.WaveDSPFilter(\"LowPass\", \"RBJ\"); return f.label; })()",
            ),
            "RBJ"
        );
    }

    /// The unimplemented combinations answer a clear error instead of a wrong
    /// filter: the Elliptic design and the non-RBJ shelf responses.
    #[test]
    fn unimplemented_combinations_report_instead_of_degrading() {
        let mut engine = engine();
        let error = try_run(
            &mut engine,
            "(function() { var f = new WaveSoundBuffer.WaveDSPFilter(); f.setParams(\"design\", \"Elliptic\"); return 0; })()",
        )
        .expect_err("Elliptic must be refused");
        assert!(error.message.contains("Elliptic"), "{}", error.message);

        let error = try_run(
            &mut engine,
            "(function() { var f = new WaveSoundBuffer.WaveDSPFilter(); f.setParams(\"type\", \"Low Shelf\"); return 0; })()",
        )
        .expect_err("a Butterworth shelf must be refused");
        assert!(error.message.contains("shelf"), "{}", error.message);

        // The refused parameter was rolled back: the filter still works.
        let value = string(
            &mut engine,
            "(function() { var f = new WaveSoundBuffer.WaveDSPFilter(); try { f.setParams(\"design\", \"Elliptic\"); } catch (e) {} return f.label; })()",
        );
        assert_eq!(value, "Butterworth");
    }

    /// The DLL's own space-less identifiers are accepted alongside the
    /// readable spellings, and the stored value is the canonical one. The
    /// identifiers were read from the shipped image (`strings -el`):
    /// `LowPass`, `HighPass`, `BandPass`, `BandStop`, `LowShelf`, `HighShelf`,
    /// `BandShelf`, `Bessel`, `Butterworth`, `ChebyshevI`, `ChebyshevII`,
    /// `Chebyshev1`, `Chebyshev2`, `Elliptic`, `Legendre`, `RBJ`, `Custom`,
    /// `OnePole`, `TwoPole` (+ the unmodelled `AllPass`, `BandPass1`,
    /// `BandPass2`).
    #[test]
    fn the_dll_identifier_spellings_are_accepted() {
        let mut engine = engine();
        let value = string(
            &mut engine,
            "(function() {\n\
                 var f = new WaveSoundBuffer.WaveDSPFilter();\n\
                 f.setParams(\"type\", \"LowPass\", \"design\", \"ChebyshevI\");\n\
                 var first = f.currentValue(\"type\") + \"/\" + f.currentValue(\"design\");\n\
                 f.setParams(%[type: \"HighPass\", design: \"Chebyshev1\"]);\n\
                 var second = f.currentValue(\"type\") + \"/\" + f.currentValue(\"design\");\n\
                 f.setParams(\"design\", \"OnePole\", \"poleDistance\", 0.5);\n\
                 var third = f.currentValue(\"design\");\n\
                 f.setParams(\"design\", \"RBJ\", \"type\", \"HighShelf\");\n\
                 var fourth = f.currentValue(\"type\") + \"/\" + f.currentValue(\"design\");\n\
                 return first + \"|\" + second + \"|\" + third + \"|\" + fourth;\n\
             })()",
        );
        assert_eq!(
            value,
            "Low Pass/Chebyshev I|High Pass/Chebyshev I|Custom One-Pole|High Shelf/RBJ"
        );

        // A recognised-but-unmodelled identifier is named, not treated as a
        // typo.
        let error = try_run(
            &mut engine,
            "(function() { var f = new WaveSoundBuffer.WaveDSPFilter(); f.setParams(\"type\", \"AllPass\"); return 0; })()",
        )
        .expect_err("AllPass is not modelled");
        assert!(
            error.message.contains("AllPass") && error.message.contains("does not model"),
            "unexpected message: {}",
            error.message
        );
    }

    /// Every identifier the module claims to accept really resolves, and every
    /// alias maps onto a canonical name the enum understands.
    #[test]
    fn every_identifier_alias_resolves_to_a_known_value() {
        for (identifier, canonical) in RESPONSE_IDENTIFIERS {
            assert_eq!(
                canonical_response(identifier),
                Some(canonical),
                "{identifier} must resolve to {canonical}"
            );
            assert!(
                ResponseType::from_name(identifier).is_some(),
                "{identifier} must build a response"
            );
        }
        for (identifier, canonical) in DESIGN_IDENTIFIERS {
            assert_eq!(
                canonical_design(identifier),
                Some(canonical),
                "{identifier} must resolve to {canonical}"
            );
            assert!(
                DesignKind::from_name(identifier).is_some(),
                "{identifier} must build a design"
            );
        }
        for canonical in RESPONSE_NAMES {
            assert_eq!(canonical_response(canonical), Some(canonical));
        }
        for canonical in design_names() {
            assert_eq!(canonical_design(canonical), Some(canonical));
        }
        for identifier in UNMODELLED_RESPONSE_IDENTIFIERS {
            assert_eq!(canonical_response(identifier), None);
        }
    }

    #[test]
    fn interface_is_a_read_only_sentinel() {
        let mut engine = engine();
        assert_eq!(
            run(
                &mut engine,
                "(function() { var f = new WaveSoundBuffer.WaveDSPFilter(); return f.interface; })()"
            ),
            Variant::Integer(INTERFACE_SENTINEL)
        );
        let error = try_run(
            &mut engine,
            "(function() { var f = new WaveSoundBuffer.WaveDSPFilter(); f.interface = 0; return 0; })()",
        )
        .expect_err("interface is read-only");
        assert_eq!(
            error.message,
            "Invalid operation for Read-only or Write-only property"
        );
    }

    // -------------------------------------------------------------- RBJ

    #[test]
    fn rbj_low_pass_has_unity_dc_and_the_q_cutoff() {
        let mut spec = spec(DesignKind::Rbj, ResponseType::LowPass);
        butterworth_q(&mut spec);
        // f32 coefficient round-off leaves the DC gain a few 1e-3 off unity at
        // this cutoff; the shape is what this test pins.
        assert!(
            (response_of(&spec, 0.0) - 1.0).abs() < 0.01,
            "DC gain is {}",
            response_of(&spec, 0.0)
        );
        assert!(
            (response_of(&spec, 1000.0) - SQRT_HALF).abs() < 1e-3,
            "an RBJ low pass with Q = 1/sqrt(2) crosses -3 dB at its cutoff"
        );
        assert!(
            response_of(&spec, 20000.0) < 0.01,
            "the stopband must fall away"
        );
    }

    #[test]
    fn rbj_high_pass_mirrors_the_low_pass() {
        let mut spec = spec(DesignKind::Rbj, ResponseType::HighPass);
        butterworth_q(&mut spec);
        assert!(
            (response_of(&spec, 24000.0) - 1.0).abs() < 1e-3,
            "Nyquist gain"
        );
        assert!(
            (response_of(&spec, 1000.0) - SQRT_HALF).abs() < 1e-3,
            "cutoff gain"
        );
        assert!(
            response_of(&spec, 10.0) < 0.01,
            "the stopband must fall away"
        );
    }

    #[test]
    fn rbj_band_pass_peaks_at_its_center() {
        let mut spec = spec(DesignKind::Rbj, ResponseType::BandPass);
        spec.bandwidth_hz = 100.0; // Q = 10
        assert!(
            (response_of(&spec, 1000.0) - 1.0).abs() < 1e-3,
            "peak gain is {}",
            response_of(&spec, 1000.0)
        );
        assert!(response_of(&spec, 20.0) < 0.05);
        assert!(response_of(&spec, 24000.0) < 0.05);
    }

    #[test]
    fn rbj_band_stop_notches_at_its_center() {
        let mut spec = spec(DesignKind::Rbj, ResponseType::BandStop);
        spec.bandwidth_hz = 100.0;
        assert!(
            response_of(&spec, 1000.0) < 1e-3,
            "the notch is {}",
            response_of(&spec, 1000.0)
        );
        assert!((response_of(&spec, 10.0) - 1.0).abs() < 0.02);
        assert!((response_of(&spec, 24000.0) - 1.0).abs() < 0.02);
    }

    #[test]
    fn rbj_shelves_move_the_named_end_by_the_gain() {
        let gain = 10.0_f64.powf(6.0 / 20.0);
        let mut low = spec(DesignKind::Rbj, ResponseType::LowShelf);
        low.gain_db = 6.0;
        // The shelf slope: the library's `Slope` parameter, default 1.
        low.bandwidth_octaves = 1.0;
        assert!((response_of(&low, 0.0) - gain).abs() < 1e-3, "low shelf DC");
        assert!(
            (response_of(&low, 24000.0) - 1.0).abs() < 0.02,
            "low shelf top"
        );

        let mut high = spec(DesignKind::Rbj, ResponseType::HighShelf);
        high.gain_db = 6.0;
        high.bandwidth_octaves = 1.0;
        assert!(
            (response_of(&high, 24000.0) - gain).abs() < 0.02,
            "high shelf Nyquist"
        );
        assert!(
            (response_of(&high, 0.0) - 1.0).abs() < 1e-3,
            "high shelf DC"
        );
    }

    /// `|H(e^{jω})|` of a biquad at `frequency`, so a test can compare a
    /// designed chain against coefficients written out from the reference's
    /// source (the denominator's `a0` normalises both rows).
    fn biquad_magnitude(
        frequency: f64,
        sample_rate: f32,
        [b0, b1, b2]: [f64; 3],
        [a0, a1, a2]: [f64; 3],
    ) -> f64 {
        let omega = 2.0 * std::f64::consts::PI * frequency / f64::from(sample_rate);
        let (b0, b1, b2) = (b0 / a0, b1 / a0, b2 / a0);
        let (a1, a2) = (a1 / a0, a2 / a0);
        let re_b = b0 + b1 * (-omega).cos() + b2 * (-2.0 * omega).cos();
        let im_b = b1 * (-omega).sin() + b2 * (-2.0 * omega).sin();
        let re_a = 1.0 + a1 * (-omega).cos() + a2 * (-2.0 * omega).cos();
        let im_a = a1 * (-omega).sin() + a2 * (-2.0 * omega).sin();
        re_b.hypot(im_b) / re_a.hypot(im_a)
    }

    /// The shelf slot is the library's `Slope`, stored verbatim; the port's
    /// shelf branch computes the library's `AL` — the `sn/2·sqrt((A + 1/A)(1/S
    /// − 1) + 2)` form — and `sq = 2·sqrt(A)·AL` from it and the gain, so the
    /// chain reproduces `RBJ.cpp` `HighShelf::setup`. Unlike a stored
    /// conversion, a later gain change re-derives it.
    #[test]
    fn the_rbj_slope_slot_reproduces_the_library() {
        let mut engine = engine();
        let handle = match try_run(
            &mut engine,
            "(function() {\n\
                 var f = new WaveSoundBuffer.WaveDSPFilter(\"HighShelf\", \"RBJ\");\n\
                 f.setParams(void, 2000, 6, 0.5);\n\
                 return f;\n\
             })()",
        )
        .expect("the shelf's positional call must be accepted")
        {
            Variant::Object(handle) => handle,
            Variant::Closure(closure) => closure.object,
            other => panic!("the script returned {other:?}"),
        };
        let (gain, slope, rate, designed) = with_state(handle, |state| {
            let rate = state.sample_rate;
            (
                state.real("gain"),
                state.real("bandwidthOctaves"),
                rate,
                [500.0, 2000.0, 8000.0]
                    .map(|frequency| state.chain.magnitude_at_hz(frequency, rate)),
            )
        })
        .expect("the filter is live");
        assert_eq!(gain, 6.0);
        assert_eq!(slope, 0.5, "the slope is stored verbatim");

        // `RBJ.cpp` `HighShelf::setup` at the live cutoff and sample rate.
        let a = 10.0_f64.powf(6.0 / 40.0);
        let omega = 2.0 * std::f64::consts::PI * 2000.0 / f64::from(rate);
        let (sin, cos) = (omega.sin(), omega.cos());
        let al = sin / 2.0 * ((a + 1.0 / a) * (1.0 / slope - 1.0) + 2.0).sqrt();
        let sq = 2.0 * a.sqrt() * al;
        let b = [
            a * ((a + 1.0) + (a - 1.0) * cos + sq),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
            a * ((a + 1.0) + (a - 1.0) * cos - sq),
        ];
        let denominator = [
            (a + 1.0) - (a - 1.0) * cos + sq,
            2.0 * ((a - 1.0) - (a + 1.0) * cos),
            (a + 1.0) - (a - 1.0) * cos - sq,
        ];
        for (index, frequency) in [500.0, 2000.0, 8000.0].into_iter().enumerate() {
            let expected = biquad_magnitude(frequency, rate, b, denominator);
            assert!(
                (designed[index] - expected).abs() < 1e-5,
                "at {frequency} Hz: {} vs {}",
                designed[index],
                expected
            );
        }
    }

    /// The port's band shelf is the library's `BandShelf::setup`, including the
    /// `w0/sn` wedge in `AL = sn·sinh(ln2/2·BW·w0/sn)`: at 10 kHz/44.1 kHz the
    /// wedge moves the sinh argument by about 47 %, so its absence is visible
    /// away from the center (at the center this coefficient family's gain is
    /// `A²` whatever `AL` is).
    #[test]
    fn the_rbj_band_shelf_uses_the_librarys_octave_wedge() {
        let mut spec = spec(DesignKind::Rbj, ResponseType::BandShelf);
        spec.center = 10_000.0;
        spec.bandwidth_octaves = 1.0;
        spec.gain_db = 6.0;

        // The library's arithmetic, written out from `RBJ.cpp`: the coefficients
        // come from the design center, the response is sampled at `frequency`.
        let a = 10.0_f64.powf(6.0 / 40.0);
        let omega0 = 2.0 * std::f64::consts::PI * 10_000.0 / f64::from(spec.sample_rate);
        let magnitude = |frequency: f64, al: f64| {
            let omega = 2.0 * std::f64::consts::PI * frequency / f64::from(spec.sample_rate);
            let cos0 = omega0.cos();
            let (b0, b1, b2) = (1.0 + al * a, -2.0 * cos0, 1.0 - al * a);
            let (a0, a1, a2) = (1.0 + al / a, -2.0 * cos0, 1.0 - al / a);
            let (b0, b1, b2) = (b0 / a0, b1 / a0, b2 / a0);
            let (a1, a2) = (a1 / a0, a2 / a0);
            let re_b = b0 + b1 * (-omega).cos() + b2 * (-2.0 * omega).cos();
            let im_b = b1 * (-omega).sin() + b2 * (-2.0 * omega).sin();
            let re_a = 1.0 + a1 * (-omega).cos() + a2 * (-2.0 * omega).cos();
            let im_a = a1 * (-omega).sin() + a2 * (-2.0 * omega).sin();
            re_b.hypot(im_b) / re_a.hypot(im_a)
        };
        let arg = std::f64::consts::LN_2 / 2.0;
        let wedged = |frequency: f64| {
            magnitude(
                frequency,
                omega0.sin() * (arg * omega0 / omega0.sin()).sinh(),
            )
        };
        let unwarped = |frequency: f64| magnitude(frequency, omega0.sin() * arg.sinh());

        assert!(
            (response_of(&spec, 10_000.0) - wedged(10_000.0)).abs() < 1e-5,
            "the port's band shelf is {}, the library's {}",
            response_of(&spec, 10_000.0),
            wedged(10_000.0)
        );
        assert!(
            (response_of(&spec, 7_000.0) - wedged(7_000.0)).abs() < 1e-5,
            "the port's band shelf at 7 kHz is {}, the library's {}",
            response_of(&spec, 7_000.0),
            wedged(7_000.0)
        );
        assert!(
            (wedged(7_000.0) - unwarped(7_000.0)).abs() > 0.05,
            "the wedge must matter at 10 kHz: {} vs {} unwarped",
            wedged(7_000.0),
            unwarped(7_000.0)
        );
    }

    /// The prototype designs' band slots take their width from `bandwidthHz`
    /// (their `TypeIIBase` declares `defaultBandwidthHzParam` and no octave
    /// parameter), so the octave field's default must not shadow it: a
    /// `setParams(void, order, center, bandwidthHz)` designs `center /
    /// bandwidthHz` as the Q.
    #[test]
    fn the_prototype_band_slot_uses_its_hz_bandwidth() {
        let mut engine = engine();
        let handle = match try_run(
            &mut engine,
            "(function() {\n\
                 var f = new WaveSoundBuffer.WaveDSPFilter(\"BandPass\", \"Butterworth\");\n\
                 f.setParams(void, 4, 1000, 500);\n\
                 return f;\n\
             })()",
        )
        .expect("the positional call must be accepted")
        {
            Variant::Object(handle) => handle,
            Variant::Closure(closure) => closure.object,
            other => panic!("the script returned {other:?}"),
        };
        let (center, bandwidth, quality) = with_state(handle, |state| {
            (
                state.real("center"),
                state.real("bandwidthHz"),
                state.spec().quality(),
            )
        })
        .expect("the filter is live");
        assert_eq!(center, 1000.0);
        assert_eq!(bandwidth, 500.0);
        assert!((quality - 2.0).abs() < 1e-6, "Q came out as {quality}");
    }

    // ------------------------------------------- analog prototype designs

    /// Butterworth: unity DC, `-3 dB` exactly at the cutoff, and a steeper
    /// rolloff for a higher order (`|H(2fc)| = 1/sqrt(1 + 2^(2n))`).
    #[test]
    fn butterworth_crosses_minus_three_db_at_the_cutoff() {
        for order in [2_u32, 4] {
            let mut spec = spec(DesignKind::Butterworth, ResponseType::LowPass);
            spec.order = order;
            assert!(
                (response_of(&spec, 0.0) - 1.0).abs() < 1e-3,
                "order {order} DC gain"
            );
            assert!(
                (response_of(&spec, 1000.0) - SQRT_HALF).abs() < 5e-3,
                "order {order} cutoff gain is {}",
                response_of(&spec, 1000.0)
            );
            let expected = 1.0 / (1.0 + 2.0_f64.powi(2 * order as i32)).sqrt();
            assert!(
                (response_of(&spec, 2000.0) - expected).abs() < 5e-3,
                "order {order} gain at 2fc is {}, expected {expected}",
                response_of(&spec, 2000.0)
            );
        }
    }

    /// Chebyshev I: the passband ripples between `-ripple dB` and unity, and
    /// the stopband is monotone below it.
    #[test]
    fn chebyshev_i_ripples_inside_its_passband() {
        let mut spec = spec(DesignKind::ChebyshevI, ResponseType::LowPass);
        spec.order = 4;
        spec.ripple = 1.0;
        let floor = 10.0_f64.powf(-1.0 / 20.0);
        let mut minimum = 1.0_f64;
        for step in 0..=200 {
            let frequency = 1000.0 * step as f64 / 200.0;
            let magnitude = response_of(&spec, frequency);
            assert!(magnitude <= 1.0 + 1e-6, "the passband peaks at unity");
            minimum = minimum.min(magnitude);
        }
        assert!(
            (minimum - floor).abs() < 5e-3,
            "the ripple floor is {minimum}, expected {floor}"
        );
        assert!(response_of(&spec, 4000.0) < floor);
    }

    /// Chebyshev II: unity at DC and the stopband attenuation at the edge.
    #[test]
    fn chebyshev_ii_holds_its_stopband_attenuation() {
        let mut spec = spec(DesignKind::ChebyshevII, ResponseType::LowPass);
        spec.order = 4;
        spec.stop_attenuation = 40.0;
        assert!((response_of(&spec, 0.0) - 1.0).abs() < 1e-3);
        let stop = 10.0_f64.powf(-40.0 / 20.0);
        assert!(
            response_of(&spec, 2000.0) <= stop * 1.05,
            "stopband gain is {}",
            response_of(&spec, 2000.0)
        );
    }

    /// Bessel and Legendre prototypes cross `-3 dB` (relative to the peak) at
    /// the cutoff and fall monotonically once past the passband. Their DC
    /// gains differ by construction: Bessel's prototype is unity at DC, while
    /// Legendre's natural DC gain is `1/sqrt(1 + P_n(0)²)` (`≈ 0.936` for
    /// order 4) because its passband peak sits where the Legendre polynomial
    /// first vanishes, not at DC.
    #[test]
    fn bessel_and_legendre_cross_minus_three_db_at_the_cutoff() {
        for design in [DesignKind::Bessel, DesignKind::Legendre] {
            let mut spec = spec(design, ResponseType::LowPass);
            spec.order = 4;
            let dc = response_of(&spec, 0.0);
            assert!((0.9..=1.05).contains(&dc), "{design:?} DC gain {dc}");
            let cutoff = response_of(&spec, 1000.0);
            assert!(
                (cutoff - SQRT_HALF).abs() < 0.08,
                "{design:?} cutoff gain {cutoff}"
            );
            // Compare only above the passband peak: Legendre lifts from its DC
            // gain to unity around its first polynomial zero (≈ 340 Hz here).
            let mut previous = f64::INFINITY;
            for step in 2..=23 {
                let frequency = 1000.0 * step as f64;
                let magnitude = response_of(&spec, frequency);
                assert!(
                    magnitude <= previous + 1e-6,
                    "{design:?} response rises at {frequency} Hz"
                );
                previous = magnitude;
            }
        }
    }

    /// The pole-placement designs: one and two poles at the recovered
    /// `Pole Distance` / `Pole Angle`, unity at DC and stable.
    #[test]
    fn custom_pole_designs_are_stable_with_unity_dc() {
        let mut one = spec(DesignKind::CustomOnePole, ResponseType::LowPass);
        one.pole_distance = 0.5;
        let filter = DesignedFilter::design(&one).expect("design");
        assert!((filter.magnitude_at_hz(0.0, one.sample_rate) - 1.0).abs() < 1e-6);
        assert!(
            filter.magnitude_at_hz(20000.0, one.sample_rate) < 0.5,
            "a one-pole low pass must roll off by Nyquist"
        );

        let mut two = spec(DesignKind::CustomTwoPole, ResponseType::LowPass);
        two.pole_distance = 0.6;
        two.pole_angle = 0.4;
        let filter = DesignedFilter::design(&two).expect("design");
        assert!((filter.magnitude_at_hz(0.0, two.sample_rate) - 1.0).abs() < 1e-6);
        let response = impulse_response(&two, 8192);
        assert!(response.iter().all(|value| value.is_finite()));
        assert!(
            response[4000..].iter().all(|value| value.abs() < 1e-3),
            "a pair inside the unit circle must decay"
        );
    }

    // -------------------------------------------------- numerics and shape

    /// A low-pass step settles at its DC gain, a high-pass step at zero, and a
    /// band-pass impulse response integrates to zero — the standard sanity
    /// checks of the three response shapes.
    #[test]
    fn step_and_impulse_responses_have_the_expected_shape() {
        let low = spec(DesignKind::Butterworth, ResponseType::LowPass);
        let mut filter = DesignedFilter::design(&low).expect("design");
        let mut frames = vec![1.0_f32; 8192];
        filter.process(&mut frames, 1);
        assert!(
            (frames[8191] - 1.0).abs() < 1e-3,
            "a low-pass step settles at unity, got {}",
            frames[8191]
        );

        let high = spec(DesignKind::Rbj, ResponseType::HighPass);
        let mut filter = DesignedFilter::design(&high).expect("design");
        let mut frames = vec![1.0_f32; 8192];
        filter.process(&mut frames, 1);
        assert!(
            frames[8191].abs() < 1e-3,
            "a high-pass step settles at zero, got {}",
            frames[8191]
        );

        let band = spec(DesignKind::Butterworth, ResponseType::BandPass);
        let response = impulse_response(&band, 16384);
        let sum: f64 = response.iter().map(|value| f64::from(*value)).sum();
        assert!(sum.abs() < 1e-3, "a band pass rejects DC, sum {sum}");
    }

    /// The band transforms on the analog prototypes: a Butterworth band stop
    /// is unity at DC and Nyquist with its notch at the centre, and a
    /// Butterworth high pass is unity at Nyquist with zero at DC.
    #[test]
    fn prototype_band_stop_and_high_pass_hold_their_shapes() {
        let mut stop = spec(DesignKind::Butterworth, ResponseType::BandStop);
        stop.bandwidth_hz = stop.center * std::f32::consts::SQRT_2;
        assert!(
            (response_of(&stop, 0.0) - 1.0).abs() < 0.02,
            "band stop DC gain is {}",
            response_of(&stop, 0.0)
        );
        assert!(
            response_of(&stop, 1000.0) < 1e-3,
            "the notch is {}",
            response_of(&stop, 1000.0)
        );
        assert!(
            (response_of(&stop, 24000.0) - 1.0).abs() < 0.02,
            "band stop Nyquist gain is {}",
            response_of(&stop, 24000.0)
        );

        let high = spec(DesignKind::Butterworth, ResponseType::HighPass);
        assert!(response_of(&high, 0.0) < 1e-3, "high pass DC gain");
        assert!(
            (response_of(&high, 24000.0) - 1.0).abs() < 1e-3,
            "high pass Nyquist gain is {}",
            response_of(&high, 24000.0)
        );
        assert!(
            (response_of(&high, 1000.0) - SQRT_HALF).abs() < 5e-3,
            "high pass cutoff gain is {}",
            response_of(&high, 1000.0)
        );
    }

    /// Every implemented design answers a low-pass step that settles at unity
    /// — a stable, finite per-class check (order 3, so the Chebyshev I DC gain
    /// is 1 rather than its even-order ripple floor).
    #[test]
    fn every_design_settles_a_low_pass_step() {
        for design in [
            DesignKind::Bessel,
            DesignKind::Butterworth,
            DesignKind::ChebyshevI,
            DesignKind::ChebyshevII,
            DesignKind::Legendre,
            DesignKind::Rbj,
        ] {
            let mut spec = spec(design, ResponseType::LowPass);
            spec.order = 3;
            let mut filter = DesignedFilter::design(&spec).expect("design");
            let mut frames = vec![1.0_f32; 4800];
            filter.process(&mut frames, 1);
            assert!(
                frames.iter().all(|value| value.is_finite()),
                "{design:?} produced a non-finite step response"
            );
            let settled = frames[4799];
            assert!(
                (settled - 1.0).abs() < 0.05,
                "{design:?} settles at {settled}"
            );
        }
    }

    /// The sections are stable and their state flushes to exact zero (the
    /// reference's `undenormalise`), so a decaying tail cannot leave
    /// subnormals behind.
    #[test]
    fn a_decaying_tail_flushes_to_exact_zero() {
        let mut spec = spec(DesignKind::Butterworth, ResponseType::LowPass);
        spec.order = 8;
        let mut filter = DesignedFilter::design(&spec).expect("design");
        let mut frames = vec![0.0_f32; 48000 * 4];
        frames[0] = 1.0;
        filter.process(&mut frames, 1);
        // f32 biquad arithmetic leaves a quantisation limit cycle far below
        // the audible floor rather than exact zero (the reference's
        // `undenormalise` only flushes subnormals); the tail must be silent by
        // any measure.
        let tail = &frames[48000 * 3..];
        let loudest = tail
            .iter()
            .fold(0.0_f32, |peak, value| peak.max(value.abs()));
        assert!(
            loudest < 1e-15,
            "the tail must be silent, loudest sample {loudest}"
        );
    }

    /// The engine-level identity path: a filter object built from TJS resolves
    /// to its chain, processes frames and reports its magnitude.
    #[test]
    fn a_script_built_filter_processes_through_its_identity() {
        let mut engine = engine();
        let handle = run(
            &mut engine,
            "(function() { return new WaveSoundBuffer.WaveDSPFilter(); })()",
        )
        .object_handle()
        .expect("object");
        assert!(process_filter(handle, &mut [1.0_f32, 0.0, 0.0, 0.0], 1));
        let magnitude = filter_magnitude_at_hz(handle, 0.0).expect("magnitude");
        assert!(
            (magnitude - 1.0).abs() < 1e-3,
            "default Butterworth LP DC gain"
        );
        assert_eq!(
            run(
                &mut engine,
                "(function() { var f = new WaveSoundBuffer.WaveDSPFilter(); return f.name; })()"
            ),
            Variant::String("Low Pass".to_string())
        );
    }
}
