//! `getSample.dll` — WaveSoundBuffer sample accessors for lip-sync animation.
//!
//! Reference: <https://github.com/wtnbgo/getSample>, `main.cpp` (159 lines,
//! byte-identical in the krkr2 trunk). The DLL attaches to the engine's
//! `WaveSoundBuffer` class (`main.cpp:40, 152-158`):
//!
//! ```text
//! getSample([n = 100])                     -> int   average of non-negative samples
//! sampleValue                              -> real  max of (sample/32768)^2 (read-only)
//! sampleCount / sampleAhead                -> int   window of sampleValue (read/write)
//! WaveSoundBuffer.setDefaultCounts(cnt)    -> void  static defaults (100 / 0)
//! WaveSoundBuffer.setDefaultAheads(ahd)    -> void
//! ```
//!
//! `getSample(n)` fetches `n` mono int16 samples at the play cursor through the
//! object's own `getVisBuffer(ptr, n, 1)` (`main.cpp:24`), drops the negative
//! samples and returns `sum / count`, or 0 when none qualified (`:27-32`). A
//! failed fetch is the call's own error (`:24, 37`). `sampleValue` fetches
//! `sampleCount` samples with `sampleAhead` frames of lead through
//! `getVisBuffer(ptr, count, 1, ahead)` (`:92`), clamps the reported count to
//! the buffer (`:96-97`) and returns the largest `(sample/32768)^2`, 0 when the
//! fetch reported nothing (`:99-106`); a failed fetch only logs (`:94`).
//! Reading any of the three properties creates the per-object add-on state once
//! with the current statics as its defaults (`:56-78, 139-149`).
//!
//! # What is real
//!
//! The surface, the statics, the per-object state, both algorithms and the
//! sample source are the reference's.  The fetch asks the buffer's own
//! `getVisBuffer` for the decoded PCM at the play cursor, the engine answers
//! with the samples the audio backend is rendering
//! (`crates/krkr-audio/src/pcm_tap.rs` through
//! `krkr_engine::plugin_api::audio`, which `wf_basic_effect`'s
//! `install_engine_audio_bridge` fills from the live tap), and a buffer that is
//! not playing answers the reference's "no samples written", `0`.
//!
//! The one representational difference is the destination: the reference lends
//! the engine its own `short*` and reads it back through the pointer
//! (`main.cpp:20-24`, `:70-77`), and this engine has no pointer channel, so the
//! destination is a TJS array the engine fills with the same samples.  The call
//! still goes through the object's `getVisBuffer` member, so a script that
//! replaced it is honoured exactly as in the reference.
//!
//! # Two owners of the same property names
//!
//! The engine also pre-implements `sampleValue`, `sampleCount` and
//! `sampleAhead` — as *class-level* members, which is where this module's
//! reference attach puts them too (`getSample/main.cpp:152-156`), so a linked
//! `getSample.dll` replaces the engine's members instead of shadowing them, and
//! an instance read resolves through the class either way
//! (`crates/krkr-engine/src/native/classes.rs`,
//! `PLUGIN_OWNED_WAVE_PROPERTIES`).  `setDefaultCounts`/`setDefaultAheads` keep
//! the engine's statics in step by also writing its global default member
//! (`wave_static_property_backing_key`), which the engine's class-level getters
//! read.
//!
//! The reference's add-on constructor also sets `useVisBuffer = 1` on the
//! object (`main.cpp:64-67`).  This engine's `useVisBuffer` stays the
//! class-level property it was and has no consumer: `getVisBuffer` answers
//! whenever the instance is playing and a sample source is installed, instead
//! of gating on the flag (`sound/win32/WaveImpl.cpp:3278`).  The port therefore
//! leaves the flag alone rather than writing a property nothing reads.

use std::cell::RefCell;
use std::collections::BTreeMap;

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result,
    runtime::{NativeArgCount, NativePropertyAccess, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "WaveSoundBuffer.getSample / sampleValue / sampleCount / sampleAhead / setDefaultCounts / setDefaultAheads",
    notes: "Surface, statics, per-object state, both algorithms and the sample source are the \
            reference's: the fetch runs through the buffer's own `getVisBuffer`, which the engine \
            answers from the audio backend's decoded-PCM tap (krkr-engine's plugin_api::audio, \
            filled by wfBasicEffect's tap bridge), so `getSample` and `sampleValue` read the PCM a \
            playing buffer is rendering and answer 0 when it is not playing. The reference's raw \
            `short*` destination is a TJS array here (no pointer channel); `useVisBuffer` stays an \
            engine-side flag with no consumer. Tested with a scripted source and with the engine's \
            own getVisBuffer.",
    install: |engine| engine.register_plugin(GetSamplePlugin),
};

/// Canonical DLL name: what [`KrkrPlugin::name`] reports and what a game's
/// `Plugins.link("getSample.dll")` resolves through [`crate::catalog`].
pub(crate) const NAME: &str = "getSample.dll";

/// The add-on's static defaults (`main.cpp:135-136`).
const DEFAULT_SAMPLE_COUNT: i64 = 100;
const DEFAULT_SAMPLE_AHEAD: i64 = 0;

/// The reference's i16 normalisation (`main.cpp:102`).
const SAMPLE_FULL_SCALE: f64 = 32768.0;

pub struct GetSamplePlugin;

impl KrkrPlugin for GetSamplePlugin {
    fn name(&self) -> &str {
        NAME
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_get_sample(runtime);
        Ok(())
    }
}

/// Per-object add-on state, the reference's `WaveSoundBufferAdd` instance
/// (`main.cpp:46-133`): the buffer window parameters, captured from the
/// statics when the state is created (`:56-63`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct AdapterState {
    counts: i64,
    aheads: i64,
}

thread_local! {
    /// The add-on state of every object the accessors were used on, keyed by
    /// the TJS object (the reference stores it on the object itself).
    static ADAPTERS: RefCell<BTreeMap<ObjectHandle, AdapterState>> =
        const { RefCell::new(BTreeMap::new()) };
    /// `WaveSoundBufferAdd::defaultCounts` / `defaultAheads` (`main.cpp:135-136`).
    static DEFAULTS: RefCell<(i64, i64)> =
        const { RefCell::new((DEFAULT_SAMPLE_COUNT, DEFAULT_SAMPLE_AHEAD)) };
}

/// The engine's global fallback default for an instance property without an
/// own value (`wave_static_property_backing_key`, `classes.rs:3150-3152`,
/// read at `:3009-3014` and written by the engine's own statics at
/// `:3464-3490`). Writing the same member keeps the engine's instance getters
/// honouring this module's statics.
fn engine_default_key(name: &str) -> String {
    format!("__nativeWaveStaticProperty${name}")
}

/// The current statics, i.e. the defaults a newly created state captures.
fn default_sample_settings() -> (i64, i64) {
    DEFAULTS.with(|defaults| *defaults.borrow())
}

/// The add-on state of `object`, created on first use with the statics as its
/// defaults (`main.cpp:56-78, 139-149`).
fn adapter_state(object: ObjectHandle) -> AdapterState {
    if let Some(state) = ADAPTERS.with(|adapters| adapters.borrow().get(&object).copied()) {
        return state;
    }
    let (counts, aheads) = default_sample_settings();
    let state = AdapterState { counts, aheads };
    ADAPTERS.with(|adapters| adapters.borrow_mut().insert(object, state));
    state
}

/// Mutates the add-on state of `object`, creating it first when needed.
fn with_adapter_state<R>(object: ObjectHandle, update: impl FnOnce(&mut AdapterState) -> R) -> R {
    let _ = adapter_state(object);
    ADAPTERS.with(|adapters| {
        let mut adapters = adapters.borrow_mut();
        let state = adapters
            .get_mut(&object)
            .expect("the adapter state was just created");
        update(state)
    })
}

/// One `getVisBuffer` fetch as this plugin sees it: `count` mono samples
/// starting `ahead` frames past the play cursor, and the count the engine
/// reported (`GetVisBuffer`'s return value,
/// `sound/win32/WaveImpl.cpp:3327`).
///
/// The reference lends the engine its own `short*` and reads it back through
/// the pointer; this engine has no pointer channel, so the destination is a
/// TJS array the engine fills with the same samples — the reference's fetch
/// with the pointer replaced by an array.  The call still goes through the
/// object's own `getVisBuffer` member, so a script that replaced it is
/// honoured exactly as in the reference.
fn vis_samples(
    runtime: &mut Runtime<KrkrHost>,
    buffer: ObjectHandle,
    count: i64,
    ahead: i64,
) -> Result<Vec<i16>> {
    if count <= 0 {
        return Ok(Vec::new());
    }
    let destination = runtime.alloc_array_object(Vec::new());
    // `main.cpp:24`: FuncCall(getVisBuffer, {buffer, n, 1}).
    let written = runtime
        .call_object_method(
            buffer,
            "getVisBuffer",
            vec![
                Variant::Object(destination),
                Variant::Integer(count),
                Variant::Integer(1),
                Variant::Integer(ahead),
            ],
        )?
        .to_integer()
        .unwrap_or(0);
    let written = written.clamp(0, count) as usize;
    let elements = runtime.array_elements(destination).unwrap_or_default();
    Ok(elements
        .iter()
        .take(written)
        .map(|value| value.to_integer().unwrap_or(0) as i16)
        .collect())
}

fn install_get_sample(runtime: &mut Runtime<KrkrHost>) {
    // The readback this module's fetch uses: the audio backend's decoded-PCM
    // tap, installed through the engine's plugin API (see
    // `wf_basic_effect`'s `install_engine_audio_bridge`).
    crate::wf_basic_effect::install_engine_audio_bridge();
    let class = match runtime.global_member("WaveSoundBuffer") {
        Variant::Object(handle) => handle,
        _ => {
            let handle = runtime.alloc_ordinary_object();
            runtime.add_object_class_info(handle, "WaveSoundBuffer");
            runtime.set_global_member("WaveSoundBuffer", Variant::Object(handle));
            handle
        }
    };

    // Script-provided members (closures) win over the plugin's, the same
    // ownership rule the engine applies to its own natives
    // (`register_native_method_preserving_script`, `classes.rs:3166-3196`).
    if !is_script_member(runtime, class, "getSample") {
        runtime.register_object_native_with_arg_count(
            class,
            "getSample",
            NativeArgCount::Any,
            get_sample,
        );
    }
    if !is_script_member(runtime, class, "setDefaultCounts") {
        runtime.register_object_native_with_arg_count(
            class,
            "setDefaultCounts",
            NativeArgCount::Any,
            set_default_counts,
        );
    }
    if !is_script_member(runtime, class, "setDefaultAheads") {
        runtime.register_object_native_with_arg_count(
            class,
            "setDefaultAheads",
            NativeArgCount::Any,
            set_default_aheads,
        );
    }
    if !is_script_member(runtime, class, "sampleValue") {
        // `Property(..., (int)0)` (`main.cpp:153`) is a getter with no setter.
        runtime.register_object_native_property_with_access(
            class,
            "sampleValue",
            NativePropertyAccess::ReadOnly,
            move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
                sample_value(runtime, this_obj, class)
            },
            |_runtime: &mut Runtime<KrkrHost>, _this_obj: Option<ObjectHandle>, _value: Variant| {
                Ok(())
            },
        );
    }
    if !is_script_member(runtime, class, "sampleCount") {
        register_window_property(runtime, class, "sampleCount", true);
    }
    if !is_script_member(runtime, class, "sampleAhead") {
        register_window_property(runtime, class, "sampleAhead", false);
    }
}

/// True when `name` was provided by game scripts (a closure) rather than by
/// native code; script overrides must not be replaced.
fn is_script_member(runtime: &Runtime<KrkrHost>, class: ObjectHandle, name: &str) -> bool {
    matches!(runtime.object_member(class, name), Variant::Closure(_))
}

/// `sampleCount` / `sampleAhead` (`main.cpp:113-124, 154-155`): per-object
/// integers created lazily from the statics.
fn register_window_property(
    runtime: &mut Runtime<KrkrHost>,
    class: ObjectHandle,
    name: &'static str,
    is_count: bool,
) {
    runtime.register_object_native_property(
        class,
        name,
        move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            let object = receiver(runtime, this_obj, class);
            let state = adapter_state(object);
            Ok(Variant::Integer(if is_count {
                state.counts
            } else {
                state.aheads
            }))
        },
        move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, value: Variant| {
            let object = receiver(runtime, this_obj, class);
            // The reference stores the value and reallocates its buffer
            // (`main.cpp:114-120`); a negative window is out of contract there
            // (`new short[counts]`), clamped here and by the engine's own
            // setter (`classes.rs:3064-3066`).
            let value = value.to_integer()?.max(0);
            with_adapter_state(object, |state| {
                if is_count {
                    state.counts = value;
                } else {
                    state.aheads = value;
                }
            });
            Ok(())
        },
    );
}

/// The object a property read/write talks about: the bound instance when
/// there is one, else the class object itself (the reference's instance hook
/// attaches its add-on state to whatever object first uses the property).
fn receiver(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    class: ObjectHandle,
) -> ObjectHandle {
    this_obj
        .map(|handle| runtime.bound_this(handle).unwrap_or(handle))
        .filter(|handle| *handle != runtime.global_handle())
        .unwrap_or(class)
}

/// `getSample(n)` (`main.cpp:12-38`): fetch `n` mono samples at the play
/// cursor, average the non-negative ones.
fn get_sample(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // `tjs_int n = numparams > 0 ? (tjs_int)*param[0] : 100` (`main.cpp:16`).
    let n = optional_integer(&args, 0)?.unwrap_or(DEFAULT_SAMPLE_COUNT);
    if n <= 0 {
        // `main.cpp:17`: n <= 0 returns with no result — script sees void.
        return Ok(Variant::Void);
    }
    let Some(this) = this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle)) else {
        return Ok(Variant::Void);
    };

    // `main.cpp:24`: FuncCall(getVisBuffer, {buffer, n, 1}) — the engine fills
    // the plugin's window through the pointer.  This engine has no pointer
    // channel, so the same fetch is `plugin_api::audio`'s Rust window: `n` mono
    // samples at the play cursor, empty when the buffer is not playing (the
    // reference's "no samples written").
    let samples = vis_samples(runtime, this, n, 0)?;
    Ok(Variant::Integer(average_non_negative(&samples)))
}

/// `getSampleValue` (`main.cpp:89-107`): fetch `counts` samples `aheads`
/// frames ahead and return the largest `(sample/32768)^2`.
fn sample_value(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    class: ObjectHandle,
) -> Result<Variant> {
    let object = receiver(runtime, this_obj, class);
    let state = adapter_state(object);
    let counts = state.counts.max(0);

    // `memset(buf, 0, counts)` then `getVisBuffer(buf, counts, 1, aheads)`
    // (`main.cpp:90-92`), read as the same Rust window `getSample` uses.
    let mut samples = match vis_samples(runtime, object, counts, state.aheads) {
        Ok(samples) => samples,
        Err(error) => {
            // `main.cpp:94`: a failed fetch only logs here.
            runtime.host_mut().log(&format!(
                "getSample: getVisBuffer failed: {}",
                error.message
            ));
            Vec::new()
        }
    };

    // `main.cpp:96-97`: a count outside the buffer reads as the buffer size.
    let mut count = samples.len() as i64;
    if count > counts || count < 0 {
        count = counts;
    }
    let count = count.max(0) as usize;
    samples.truncate(count.min(samples.len()));
    Ok(Variant::Real(max_squared(&samples)))
}

/// `setDefaultCounts` (`main.cpp:131`): the default `sampleCount` of states
/// created afterwards, and of the engine's instance fallback.
fn set_default_counts(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let count = optional_integer(&args, 0)?
        .unwrap_or(DEFAULT_SAMPLE_COUNT)
        .max(0);
    DEFAULTS.with(|defaults| defaults.borrow_mut().0 = count);
    let global = runtime.global_handle();
    runtime.set_object_member(
        global,
        engine_default_key("sampleCount"),
        Variant::Integer(count),
    );
    Ok(Variant::Void)
}

/// `setDefaultAheads` (`main.cpp:132`): the default `sampleAhead`.
fn set_default_aheads(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let ahead = optional_integer(&args, 0)?
        .unwrap_or(DEFAULT_SAMPLE_AHEAD)
        .max(0);
    DEFAULTS.with(|defaults| defaults.borrow_mut().1 = ahead);
    let global = runtime.global_handle();
    runtime.set_object_member(
        global,
        engine_default_key("sampleAhead"),
        Variant::Integer(ahead),
    );
    Ok(Variant::Void)
}

/// `for (i = 0; i < n; i++) if (buf[i] >= 0) { sum += buf[i]; c++; }` and the
/// integer `sum / c` (`main.cpp:27-32`), 0 when no sample qualified.
fn average_non_negative(samples: &[i16]) -> i64 {
    let mut sum: i64 = 0;
    let mut count: i64 = 0;
    for &sample in samples {
        if sample >= 0 {
            sum += i64::from(sample);
            count += 1;
        }
    }
    if count > 0 { sum / count } else { 0 }
}

/// `s = ((double)buf[i]) / 32768.0; s *= s;` and the maximum over the scanned
/// window (`main.cpp:100-106`).
fn max_squared(samples: &[i16]) -> f64 {
    let mut max = 0.0f64;
    for &sample in samples {
        let value = f64::from(sample) / SAMPLE_FULL_SCALE;
        let squared = value * value;
        if max < squared {
            max = squared;
        }
    }
    max
}

/// An omitted or `void` argument reads as `None` (the engine's own
/// `optional_integer`, `classes.rs:8987-8992`).
fn optional_integer(args: &[Variant], index: usize) -> Result<Option<i64>> {
    args.get(index)
        .filter(|value| !matches!(value, Variant::Void))
        .map(Variant::to_integer)
        .transpose()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use krkr_engine::{EngineConfig, KrkrEngine};
    use krkr_tjs2::runtime::Variant;

    use super::*;
    use crate::catalog;

    fn engine_with_plugin() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(GetSamplePlugin).expect("plugin");
        engine
    }

    fn integer(engine: &mut KrkrEngine, expression: &str) -> i64 {
        engine
            .execute_expression("probe.tjs", expression)
            .expect("expression")
            .to_integer()
            .expect("integer")
    }

    fn real(engine: &mut KrkrEngine, expression: &str) -> f64 {
        engine
            .execute_expression("probe.tjs", expression)
            .expect("expression")
            .to_real()
            .expect("real")
    }

    #[test]
    fn registers_the_surface_and_resolves_the_alias() {
        let engine = engine_with_plugin();
        assert_eq!(catalog::canonical_name(NAME), Some(NAME));
        assert_eq!(catalog::canonical_name("GetSample.dll"), Some(NAME));
        assert_eq!(catalog::canonical_name("getsample.dll"), Some(NAME));
        assert!(catalog::is_same_plugin("getSample.dll", "GetSample.dll"));

        let runtime = engine.tjs_runtime();
        let class = runtime
            .global_member("WaveSoundBuffer")
            .object_handle()
            .expect("WaveSoundBuffer class");
        for name in [
            "getSample",
            "setDefaultCounts",
            "setDefaultAheads",
            "sampleValue",
            "sampleCount",
            "sampleAhead",
        ] {
            assert!(
                !matches!(runtime.object_member(class, name), Variant::Void),
                "member `{name}` is not registered on WaveSoundBuffer",
            );
        }
    }

    /// The reference's average (`main.cpp:27-32`), on synthetic windows: the
    /// negatives are dropped from both the sum and the count, an all-negative
    /// window is 0, and the division is the integer division the C++ performs.
    /// Row-24 pin: the fetch is the *playing* buffer's decoded PCM, so
    /// `getSample` stops answering the not-playing zero.  The source is the
    /// engine's plugin-API slot (`plugin_api::audio`), filled here by a
    /// scripted window in place of the audio backend's tap.
    #[test]
    fn get_sample_reads_the_playing_pcm() {
        use krkr_engine::plugin_api::audio::{
            PcmAudioSpec, WavePcmRequest, WavePcmSource, WavePcmState, WavePcmWindow,
            clear_wave_pcm_source, set_wave_pcm_source,
        };

        struct HalfScale;

        impl WavePcmSource for HalfScale {
            fn read_window(
                &self,
                _id: krkr_engine::plugin_api::audio::AudioInstanceId,
                request: WavePcmRequest,
            ) -> Option<WavePcmWindow> {
                Some(WavePcmWindow {
                    spec: PcmAudioSpec {
                        sample_rate: 44_100,
                        channels: 2,
                    },
                    state: WavePcmState::Playing,
                    samples: (0..request.frames).flat_map(|_| [0.5_f32, 0.5]).collect(),
                    available_frames: request.frames,
                })
            }
        }

        let _guard = crate::wf_basic_effect::lock_pcm_source();
        let mut engine = engine_with_plugin();
        set_wave_pcm_source(Arc::new(HalfScale));
        let value = engine
            .execute_expression(
                "pcm.tjs",
                "(function() { var buffer = new WaveSoundBuffer(); \
                     buffer.open(\"tone.ogg\"); buffer.play(); \
                     var samples = []; \
                     var written = buffer.getVisBuffer(samples, 4, 1, 0); \
                     return buffer.status + \":\" + written + \":\" + samples.count + \":\" + \
                         samples[0] + \":\" + buffer.getSample(4); })()",
            )
            .expect("script")
            .to_tjs_string()
            .expect("string");
        clear_wave_pcm_source();
        assert_eq!(
            value, "play:4:4:16384:16384",
            "getSample must read the playing PCM (status:written:count:first:average)"
        );
    }

    #[test]
    fn get_sample_averages_only_the_non_negative_samples() {
        assert_eq!(average_non_negative(&[]), 0);
        assert_eq!(average_non_negative(&[-1, -32768, -5]), 0);
        assert_eq!(average_non_negative(&[100, -500, 300, 0]), 133);
        assert_eq!(average_non_negative(&[32767]), 32767);
        assert_eq!(average_non_negative(&[1, 2, -3, 3]), 2);
    }

    /// `sampleValue`'s measure (`main.cpp:100-106`): the largest square of
    /// `sample/32768`, including negative samples (squares are positive).
    #[test]
    fn sample_value_is_the_largest_squared_amplitude() {
        assert_eq!(max_squared(&[]), 0.0);
        assert_eq!(max_squared(&[0, 0, 0]), 0.0);
        assert_eq!(max_squared(&[-32768, 100, 16384]), 1.0);
        let quarter = max_squared(&[-16384, 1]);
        assert!(
            (quarter - 0.25).abs() < 1e-12,
            "(-16384/32768)^2 = {quarter}"
        );
    }

    /// `setDefaultCounts`/`setDefaultAheads` (`main.cpp:131-136`) drive the
    /// values a new buffer reads. The reads go through the engine's own
    /// instance natives, which is exactly the coupling the statics keep in
    /// step (the engine's instance properties shadow this module's).
    #[test]
    fn the_statics_drive_the_defaults_the_instances_read() {
        let mut engine = engine_with_plugin();
        engine
            .execute_script(
                "defaults.tjs",
                r#"
                global.b = new WaveSoundBuffer();
                global.before_count = b.sampleCount;
                global.before_ahead = b.sampleAhead;
                WaveSoundBuffer.setDefaultCounts(37);
                WaveSoundBuffer.setDefaultAheads(9);
                global.c = new WaveSoundBuffer();
                global.after_count = c.sampleCount;
                global.after_ahead = c.sampleAhead;
                c.sampleCount = 12;
                c.sampleAhead = 3;
                global.own_count = c.sampleCount;
                global.own_ahead = c.sampleAhead;
                global.fresh = new WaveSoundBuffer();
                global.fresh_count = fresh.sampleCount;
                "#,
            )
            .expect("defaults script");
        assert_eq!(integer(&mut engine, "before_count"), 100);
        assert_eq!(integer(&mut engine, "before_ahead"), 0);
        assert_eq!(integer(&mut engine, "after_count"), 37);
        assert_eq!(integer(&mut engine, "after_ahead"), 9);
        assert_eq!(integer(&mut engine, "own_count"), 12);
        assert_eq!(integer(&mut engine, "own_ahead"), 3);
        assert_eq!(integer(&mut engine, "fresh_count"), 37);
    }

    /// The class-level properties this module registers (`WaveSoundBuffer.
    /// sampleCount` and friends, `main.cpp:152-156`): defaults from the
    /// statics, writes stick, `sampleValue` answers the reference's
    /// fetch-nothing value, and `sampleValue` is read-only.
    #[test]
    fn the_class_level_properties_are_the_references() {
        let mut engine = engine_with_plugin();
        engine
            .execute_script(
                "class.tjs",
                r#"
                global.default_count = WaveSoundBuffer.sampleCount;
                global.default_ahead = WaveSoundBuffer.sampleAhead;
                WaveSoundBuffer.sampleCount = 8;
                WaveSoundBuffer.sampleAhead = 2;
                global.written_count = WaveSoundBuffer.sampleCount;
                global.written_ahead = WaveSoundBuffer.sampleAhead;
                global.value = WaveSoundBuffer.sampleValue;
                global.value_write = "accepted";
                try { WaveSoundBuffer.sampleValue = 1; }
                catch (e) { global.value_write = "denied"; }
                "#,
            )
            .expect("class script");
        // The class object captures the statics when its state is created, so
        // the values below are the initial ones (100 / 0) and the writes
        // stick on the same object.
        assert_eq!(integer(&mut engine, "default_count"), 100);
        assert_eq!(integer(&mut engine, "default_ahead"), 0);
        assert_eq!(integer(&mut engine, "written_count"), 8);
        assert_eq!(integer(&mut engine, "written_ahead"), 2);
        assert_eq!(real(&mut engine, "value"), 0.0);
        assert_eq!(
            engine
                .execute_expression("probe.tjs", "value_write")
                .expect("write probe")
                .to_tjs_string()
                .expect("string"),
            "denied",
        );
    }

    /// The reference's fetch is the object's own `getVisBuffer(ptr, n, 1)`
    /// (`main.cpp:24`); a script override can observe the call, and its
    /// reported count is what `sampleValue` clamps against. With no samples
    /// in the plugin's window both accessors answer 0.
    #[test]
    fn the_fetch_is_the_objects_own_get_vis_buffer() {
        let mut engine = engine_with_plugin();
        engine
            .execute_script(
                "fetch.tjs",
                r#"
                global.b = new WaveSoundBuffer();
                global.vis_calls = "";
                b.getVisBuffer = function(dest, samples, channels, ahead) {
                    global.vis_calls = global.vis_calls + "(" + (typeof dest) + "," + samples + "," + channels + ")";
                    return samples;
                };
                global.avg_default = b.getSample();
                global.avg_four = b.getSample(4);
                global.avg_none = b.getSample(0);
                "#,
            )
            .expect("fetch script");
        assert_eq!(
            engine
                .execute_expression("probe.tjs", "vis_calls")
                .expect("calls")
                .to_tjs_string()
                .expect("string"),
            "(Object,100,1)(Object,4,1)",
            "getSample fetches (dest, n, 1) with an array destination (the reference \
             lends a `short*`; this engine has no pointer channel); n <= 0 returns \
             before the fetch",
        );
        assert_eq!(integer(&mut engine, "avg_default"), 0);
        assert_eq!(integer(&mut engine, "avg_four"), 0);
        assert!(
            matches!(
                engine.execute_expression("probe.tjs", "avg_none"),
                Ok(Variant::Void)
            ),
            "getSample(0) has no result (`main.cpp:17` returns with none)",
        );
    }

    /// `main.cpp:37`: a failed `getVisBuffer` is `getSample`'s own error.
    #[test]
    fn a_failed_fetch_propagates_from_get_sample() {
        let mut engine = engine_with_plugin();
        let error = engine
            .execute_script(
                "fail.tjs",
                r#"
                var b = new WaveSoundBuffer();
                b.getVisBuffer = function(ptr, samples, channels) { throw "no samples"; };
                b.getSample(2);
                "#,
            )
            .expect_err("a failed fetch must propagate");
        let mentions_the_throw = error.message.contains("no samples")
            || error
                .exception_message
                .as_deref()
                .is_some_and(|message| message.contains("no samples"));
        assert!(
            mentions_the_throw,
            "the thrown error is the fetch's: message={:?} exception={:?}",
            error.message, error.exception_message,
        );
    }

    /// `main.cpp:94`: `sampleValue` only logs a failed fetch and answers 0.
    #[test]
    fn a_failed_fetch_only_logs_in_sample_value() {
        let mut engine = engine_with_plugin();
        engine
            .execute_script(
                "log.tjs",
                r#"
                WaveSoundBuffer.getVisBuffer = function(ptr, samples, channels, ahead) {
                    throw "no samples";
                };
                "#,
            )
            .expect("override script");
        assert_eq!(real(&mut engine, "WaveSoundBuffer.sampleValue"), 0.0);
        assert!(
            engine
                .host()
                .logs()
                .iter()
                .any(|line| line.contains("getSample: getVisBuffer failed")),
            "the failed fetch must be logged: {:?}",
            engine.host().logs(),
        );
    }

    /// The engine's ownership rule: a member a script defined before the
    /// plugin registers is not replaced.
    #[test]
    fn a_script_provided_get_sample_wins() {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "pre.tjs",
                r#"
                WaveSoundBuffer.getSample = function(n) { return 7; };
                "#,
            )
            .expect("pre script");
        engine.register_plugin(GetSamplePlugin).expect("plugin");
        engine
            .execute_script(
                "call.tjs",
                r#"
                global.b = new WaveSoundBuffer();
                global.answer = b.getSample(3);
                "#,
            )
            .expect("call script");
        assert_eq!(integer(&mut engine, "answer"), 7);
    }
}
