//! `WaveSoundBuffer.filters`: the per-sample DSP chain of a playing buffer.
//!
//! # The reference contract
//!
//! `tTJSNI_BaseWaveSoundBuffer` owns a TJS array (`Filters`, created in the
//! constructor, `krkrz/src/core/sound/WaveIntf.cpp:815`) and exposes it through
//! the getter-only `filters` property (`:1540-1554`; the setter is
//! `TJS_DENY_NATIVE_PROP_SETTER`, so scripts mutate the array, not the
//! property).  The chain over that array is built by `RebuildFilterChain`
//! (`:865-905`), called from `Open` right after the loop manager exists
//! (`sound/win32/WaveImpl.cpp:2935`): for each element it reads the element's
//! `interface` property, casts that value to `iTVPBasicWaveFilter*`, and folds
//! the filters over the decoder in array order —
//! `FilterOutput = filter->Recreate(FilterOutput)`, so element 0 is the
//! innermost and processes first — then reads `InputFormat` from the chain's
//! output (`:2938`).  `iTVPBasicWaveFilter` (`sound/WaveIntf.h:130-137`) is
//! `Recreate(tTVPSampleAndLabelSource*)` / `Clear()` / `Update()` / `Reset()`.
//!
//! The rest of the schedule is in `sound/win32/WaveImpl.cpp`:
//!
//! | call | where |
//! | ---- | ----- |
//! | `Clear()` per filter | `ClearFilterChain` (`:907-923`), from `Clear` (`:2336`), i.e. at `Open` (`:2920`) and at unload |
//! | `Reset()` per filter | `ResetFilterChain` (`:925-931`), from `StartPlay` (`:2813`) |
//! | `Update()` per filter | `UpdateFilterChain` (`:933-947`), from `FillL2Buffer` (`:2396`), before each decoded unit (~125 ms) |
//!
//! The chain's output is what `Decode` pulls into the L2 buffer (`:2348-2364`)
//! and therefore what the buffer plays *and* what `getVisBuffer` reads back
//! (`:2507-2510` copies the L2 unit into the visualization ring), so a filter
//! is audible and visible at the same point of the pipeline.
//!
//! # What this port represents, and what it cannot
//!
//! The reference's `interface` is a raw pointer into the filter object; Rust
//! cannot publish a pointer as a script value, so [`WaveFilterId`] is the
//! honest stand-in: an opaque, process-unique integer a filter object
//! publishes through its own `interface` property and that
//! [`register_wave_filter`] binds to a live [`WaveFilter`].  What matches: the
//! value is per instance (not per class), it is what the engine reads out of
//! the `filters` array, it identifies exactly one live filter, and it stops
//! resolving once the filter unregisters (`finalize` in the filter's TJS
//! class, the counterpart of the reference's destructor).  What cannot match:
//! nothing can cast the value back to a C++ object, it is not an address, and
//! a script that fabricates one gets whatever — if anything — is registered
//! under it; [`WaveFilterChain::build`] reports such an element as skipped
//! instead of pretending a chain exists.
//!
//! The other representational gap is the pull model.  `Recreate` returns a new
//! *source* the engine then decodes from; here the audio worker owns the
//! sample buffers, so a filter instead processes them in place and
//! [`WaveFilter::recreate`] answers the format it accepts.  The call order,
//! the per-unit `update` and the state lifetime are the reference's.
//!
//! # Which of this engine's playback paths carry a chain
//!
//! | path | chain | anchor |
//! | ---- | ----- | ------ |
//! | a storage buffer (`WaveSoundBuffer.open`) | yes | the reference decodes every buffer through the chain on its own thread (`sound/win32/WaveImpl.cpp:2348-2364`), so a filtered instance is loaded whole and played through [`FilteredPcmDecoder`], the closest of this engine's paths |
//! | a live PCM stream (movie soundtracks, `AudioCommand::PlayPcmStream`) | no | a movie's audio goes through the movie graph (`VideoOverlay`), which has no `filters` property in the reference either; the engine never attaches a chain to a video instance's audio id |
//! | kira's own streaming file decoder | no, by design | its frames are private, so a filtered instance is routed to the static path instead (`dispatch_play_load`) rather than silently playing unfiltered; the same opacity is why an *unfiltered* streaming sound — a looping BGM by default — registers no PCM tap either (see `register_sound_tap`) |
//! | a paused / armed buffer | yes, silent | the chain is built and reset at a play whose stream stays paused (M207's armed play, M213's `open`-clears-the-pause); pausing does not touch it |
//!
//! The chain's lifetime follows the reference's, not the stream's: the list an
//! instance was opened with survives `Stop` (`ClearFilterChain` runs from
//! `Clear`, i.e. at `Open` and unload, `sound/win32/WaveImpl.cpp:2336`,
//! `:2920`), `StartPlay` only `Reset`s the filters (`:2813`), and a new list
//! replaces the old one by clearing the filters it dropped
//! (`ClearFilterChain`, `sound/WaveIntf.cpp:907-923`).

use std::{
    collections::BTreeMap,
    fmt,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicI64, Ordering},
    },
};

use krkr_core::PcmAudioSpec;

/// One filter of a `WaveSoundBuffer.filters` chain — the reference's
/// `iTVPBasicWaveFilter` (`krkrz/src/core/sound/WaveIntf.h:130-137`), with its
/// four methods and the processing entry point that replaces the reference's
/// `tTVPSampleAndLabelSource::Decode` pull.
///
/// Implementations are shared between the script thread (parameter setters run
/// on the engine thread) and the audio worker's decode thread, hence
/// `Send + Sync` and `&self` methods: a filter keeps its own interior
/// mutability.
pub trait WaveFilter: Send + Sync {
    /// Attaches the filter to a source of `spec` samples — the reference's
    /// `Recreate`, which answers the format the next stage sees.
    ///
    /// The returned format is folded into the chain, and it has to be the one
    /// the filter was connected with: this port's render path is fixed (stereo
    /// at the sound's own rate — the filters re-design themselves at connect
    /// time instead of resampling), so a filter that answers a different
    /// sample rate or channel count is refused by [`WaveFilterChain::build`],
    /// released, and reported (the reference would hand the narrower format
    /// on).  An `Err` is the reference's refusal to connect —
    /// the shipped `wfBasicEffect.dll` throws `HiRes format not supported.`
    /// for a format wider than 32 bits or with more than four channels, and
    /// `Cannot connect multiple wave sound buffer at once.` when a second
    /// upstream source is connected while one is live — and the element is
    /// left out of the chain with the reason reported.  A filter that is
    /// already held by a live chain must refuse the reconnect: the chain
    /// releases its filters when it drops ([`WaveFilterChain`]'s `Drop`), which
    /// is the reference's `Clear`.
    fn recreate(&self, spec: PcmAudioSpec) -> Result<PcmAudioSpec, String>;

    /// Releases what [`WaveFilter::recreate`] built — `Clear`, called when the
    /// buffer clears its chain (`ClearFilterChain`, `sound/WaveIntf.cpp:907`).
    fn clear(&self);

    /// Picks up parameter changes — `Update`, called before each decoded unit
    /// (`UpdateFilterChain`, `sound/WaveIntf.cpp:933`).
    fn update(&self);

    /// Clears the processing state — `Reset`, called when playback starts at
    /// the first sample (`ResetFilterChain`, `:925`).  The filter objects stay
    /// attached: a `WaveSoundBuffer` that stops and plays again resets the
    /// same filters instead of rebuilding them.
    fn reset(&self);

    /// Processes one unit of interleaved `f32` samples in place, with
    /// `spec.channels` values per frame — the reference's decode step of the
    /// chain.
    fn process(&self, frames: &mut [f32]);
}

/// The value a filter object's `interface` property publishes.
///
/// The reference's value is the object's address cast to an integer; this is
/// the port's stand-in for it (see the module docs): opaque, per instance, and
/// meaningless outside the registration that created it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct WaveFilterId(i64);

impl WaveFilterId {
    /// The value scripts and the engine see: `interface` publishes exactly
    /// this, and the engine passes it back verbatim.
    pub const fn raw(self) -> i64 {
        self.0
    }
}

impl fmt::Display for WaveFilterId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "wave-filter#{}", self.0)
    }
}

/// One element [`WaveFilterChain::build`] could not take.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WaveFilterSkip {
    /// The `interface` value of the element that was skipped.
    pub id: i64,
    /// Why it could not join the chain: nothing is registered under the value,
    /// or the filter refused the format.
    pub reason: String,
}

/// Registers a live filter and returns the id its `interface` property must
/// publish.
///
/// The registry replaces the reference's `reinterpret_cast<iTVPBasicWaveFilter*>`
/// as the link between a `filters` array element and the processing object:
/// the engine reads a script-visible integer, the worker resolves it here.
pub fn register_wave_filter(filter: Arc<dyn WaveFilter>) -> WaveFilterId {
    let id = NEXT_FILTER_ID.fetch_add(1, Ordering::Relaxed);
    lock_registry().insert(id, filter);
    WaveFilterId(id)
}

/// Unregisters a filter (`finalize` on its TJS class).  A chain that already
/// holds the filter keeps working — it holds its own handle.
pub fn unregister_wave_filter(id: WaveFilterId) -> bool {
    lock_registry().remove(&id.0).is_some()
}

/// The filter an `interface` value names, when one is registered.
pub fn resolve_wave_filter(id: i64) -> Option<Arc<dyn WaveFilter>> {
    lock_registry().get(&id).cloned()
}

static NEXT_FILTER_ID: AtomicI64 = AtomicI64::new(1);

fn lock_registry() -> std::sync::MutexGuard<'static, BTreeMap<i64, Arc<dyn WaveFilter>>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<i64, Arc<dyn WaveFilter>>>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The chain of one playing instance.
///
/// Built by [`WaveFilterChain::build`] from the `interface` values the engine
/// read out of the instance's `filters` array, exactly the way
/// `RebuildFilterChain` folds the array over the decoder
/// (`sound/WaveIntf.cpp:898-905`).  The audio worker owns it for as long as
/// the playback it was built for: it is rebuilt at every playback start (the
/// buffer's `open` re-reads the array, the reference rebuilds in `Open`) and
/// is dropped when the sound stops playing.  The filters themselves live in
/// the registry, so rebuilding shares the same objects — that is what makes a
/// script's `setGain` reach the next playback, and the `reset` at the start of
/// each playback is the reference's `ResetFilterChain`.
pub struct WaveFilterChain {
    filters: Vec<Arc<dyn WaveFilter>>,
    spec: PcmAudioSpec,
}

impl WaveFilterChain {
    /// Resolves `ids` — the `interface` values of a `filters` array, in array
    /// order — into a chain over `spec`.
    ///
    /// Elements that resolve to nothing and elements whose `recreate` refuses
    /// the format are left out and reported; the rest keep their relative
    /// order, so filter *n* processes after filter *n-1* over the same buffer.
    pub fn build(ids: &[i64], spec: PcmAudioSpec) -> (Self, Vec<WaveFilterSkip>) {
        let mut filters: Vec<Arc<dyn WaveFilter>> = Vec::new();
        let mut skipped = Vec::new();
        let mut current = spec;
        for id in ids {
            let Some(filter) = resolve_wave_filter(*id) else {
                skipped.push(WaveFilterSkip {
                    id: *id,
                    reason: "no filter is registered under this interface value".to_string(),
                });
                continue;
            };
            match filter.recreate(current) {
                Ok(next) if next == current => {
                    current = next;
                    filters.push(filter);
                }
                // The render path is fixed: two channels (kira's `Frame` is a
                // stereo pair and the decoder publishes stereo units) at the
                // sound's own rate.  The reference would resample the format
                // through its own buffer and hand the narrower one on; here a
                // filter that answers a different format is refused, and the
                // refusal names which half changed.
                Ok(next) => {
                    // `recreate` connected the filter and may hold what that
                    // claims (the one-source rule): a filter the chain leaves
                    // out is released, or the claim leaks and the element can
                    // never join a chain again — and the next failure would
                    // blame the one-source rule instead of the format.
                    filter.clear();
                    skipped.push(WaveFilterSkip {
                        id: *id,
                        reason: format!(
                            "cannot change the connected format \
                             ({} Hz {}ch -> {} Hz {}ch)",
                            current.sample_rate, current.channels, next.sample_rate, next.channels
                        ),
                    });
                }
                Err(reason) => {
                    // Same release: a filter that refuses may still hold what
                    // its `recreate` claimed.
                    filter.clear();
                    skipped.push(WaveFilterSkip { id: *id, reason });
                }
            }
        }
        (
            Self {
                filters,
                spec: current,
            },
            skipped,
        )
    }

    /// The format the chain connected with — the reference reads
    /// `FilterOutput->GetFormat()` here (`sound/win32/WaveImpl.cpp:2938`).
    ///
    /// This port's render path is fixed (stereo at the sound's own rate: the
    /// decoder hands the buffer stereo units and kira resamples for the
    /// device), and a filter cannot resample inside the chain, so the value is
    /// always the sound's format: a filter that answers a different one is
    /// refused at build time — [`WaveFilterChain::build`] releases it and
    /// reports the refusal in its skips — where the reference would accept the
    /// narrower format and resample.
    pub const fn spec(&self) -> PcmAudioSpec {
        self.spec
    }

    pub fn len(&self) -> usize {
        self.filters.len()
    }

    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }

    /// `UpdateFilterChain` (`sound/WaveIntf.cpp:933`), before each unit.
    pub fn update(&self) {
        for filter in &self.filters {
            filter.update();
        }
    }

    /// `ResetFilterChain` (`:925`), when playback starts.
    pub fn reset(&self) {
        for filter in &self.filters {
            filter.reset();
        }
    }

    /// `ClearFilterChain`'s per-filter `Clear()` (`:914-916`).
    pub fn clear(&self) {
        for filter in &self.filters {
            filter.clear();
        }
    }

    /// Runs one unit through the chain, innermost first.
    pub fn process(&self, frames: &mut [f32]) {
        for filter in &self.filters {
            filter.process(frames);
        }
    }
}

impl Drop for WaveFilterChain {
    /// Dropping the chain releases what it connected: the reference's
    /// `ClearFilterChain` per-filter `Clear()` (`sound/WaveIntf.cpp:907-923`)
    /// runs when the buffer clears its chain (at `open` and unload), and this
    /// chain lives exactly as long as one playback — so a filter is free to
    /// join the next chain, and one that is still held by a live chain refuses
    /// the reconnect (`WaveFilter::recreate`'s one-buffer rule, the shipped
    /// DLL's `Cannot connect multiple wave sound buffer at once.`).
    fn drop(&mut self) {
        self.clear();
    }
}

impl fmt::Debug for WaveFilterChain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WaveFilterChain")
            .field("filters", &self.filters.len())
            .field("spec", &self.spec)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A filter that records what it was asked to do, so the chain's call
    /// order and its per-unit schedule are observable.
    #[derive(Default)]
    struct RecordingFilter {
        name: &'static str,
        log: Arc<Mutex<Vec<String>>>,
        spec: Mutex<Option<PcmAudioSpec>>,
        reject: Option<&'static str>,
        gain: Mutex<f32>,
    }

    impl RecordingFilter {
        fn new(name: &'static str, log: &Arc<Mutex<Vec<String>>>) -> Arc<Self> {
            Arc::new(Self {
                name,
                log: Arc::clone(log),
                spec: Mutex::new(None),
                reject: None,
                gain: Mutex::new(1.0),
            })
        }

        fn with_gain(self: &Arc<Self>, gain: f32) -> Arc<Self> {
            *self.gain.lock().expect("gain") = gain;
            Arc::clone(self)
        }

        fn record(&self, entry: String) {
            self.log.lock().expect("log").push(entry);
        }
    }

    impl WaveFilter for RecordingFilter {
        fn recreate(&self, spec: PcmAudioSpec) -> Result<PcmAudioSpec, String> {
            self.record(format!("recreate:{}:{}", self.name, spec.channels));
            if let Some(reason) = self.reject {
                return Err(reason.to_string());
            }
            *self.spec.lock().expect("spec") = Some(spec);
            Ok(spec)
        }

        fn clear(&self) {
            self.record(format!("clear:{}", self.name));
        }

        fn update(&self) {
            self.record(format!("update:{}", self.name));
        }

        fn reset(&self) {
            self.record(format!("reset:{}", self.name));
        }

        fn process(&self, frames: &mut [f32]) {
            self.record(format!("process:{}", self.name));
            let gain = *self.gain.lock().expect("gain");
            for sample in frames.iter_mut() {
                *sample *= gain;
            }
        }
    }

    fn spec() -> PcmAudioSpec {
        PcmAudioSpec {
            sample_rate: 44_100,
            channels: 2,
        }
    }

    fn log() -> Arc<Mutex<Vec<String>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn entries(log: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        log.lock().expect("log").clone()
    }

    /// The chain is a fold in array order: element 0 is the innermost filter,
    /// so it processes first — `RebuildFilterChain`'s
    /// `FilterOutput = i->Interface->Recreate(FilterOutput)`
    /// (`sound/WaveIntf.cpp:898-905`).
    #[test]
    fn the_chain_folds_in_array_order() {
        let log = log();
        let first = RecordingFilter::new("first", &log).with_gain(2.0);
        let second = RecordingFilter::new("second", &log).with_gain(0.5);
        let first_id = register_wave_filter(first);
        let second_id = register_wave_filter(second);

        let (chain, skipped) = WaveFilterChain::build(&[first_id.raw(), second_id.raw()], spec());
        assert!(skipped.is_empty(), "both filters resolve, got {skipped:?}");
        assert_eq!(chain.len(), 2);
        assert_eq!(
            entries(&log),
            vec![
                "recreate:first:2".to_string(),
                "recreate:second:2".to_string()
            ],
            "recreate runs in array order"
        );

        let mut samples = [1.0f32, -1.0, 0.25, 0.5];
        chain.process(&mut samples);
        assert_eq!(
            entries(&log),
            vec![
                "recreate:first:2".to_string(),
                "recreate:second:2".to_string(),
                "process:first".to_string(),
                "process:second".to_string(),
            ],
            "element 0 processes first and element 1 sees its output"
        );
        // 2.0 then 0.5: the filters compose over the same buffer.
        assert!((samples[0] - 1.0).abs() < 1e-6, "got {samples:?}");

        unregister_wave_filter(first_id);
        unregister_wave_filter(second_id);
    }

    /// `Update()` runs before each decoded unit and `Reset()` when playback
    /// starts (`UpdateFilterChain`/`ResetFilterChain`), and neither rebuilds
    /// the chain: the calls reach the same filters.
    #[test]
    fn update_and_reset_reach_every_filter_without_rebuilding() {
        let log = log();
        let filter = RecordingFilter::new("only", &log);
        let id = register_wave_filter(Arc::clone(&filter) as Arc<dyn WaveFilter>);
        let (chain, _) = WaveFilterChain::build(&[id.raw()], spec());

        chain.reset();
        chain.update();
        let mut samples = [0.5f32, 0.5];
        chain.process(&mut samples);
        chain.update();
        chain.clear();

        assert_eq!(
            entries(&log),
            vec![
                "recreate:only:2".to_string(),
                "reset:only".to_string(),
                "update:only".to_string(),
                "process:only".to_string(),
                "update:only".to_string(),
                "clear:only".to_string(),
            ]
        );
        unregister_wave_filter(id);
    }

    /// A filter refusing the format is the reference's failed `Recreate`
    /// (the shipped DLL throws for a wider-than-32-bit or >4-channel format):
    /// the element is left out of the chain and reported, and the remaining
    /// elements still form a chain.
    #[test]
    fn a_filter_that_refuses_the_format_is_skipped_and_reported() {
        let log = log();
        let rejecting = Arc::new(RecordingFilter {
            name: "rejecting",
            log: Arc::clone(&log),
            spec: Mutex::new(None),
            reject: Some("HiRes format not supported."),
            gain: Mutex::new(1.0),
        });
        let accepting = RecordingFilter::new("accepting", &log);
        let rejecting_id = register_wave_filter(rejecting);
        let accepting_id = register_wave_filter(accepting);

        let (chain, skipped) =
            WaveFilterChain::build(&[rejecting_id.raw(), accepting_id.raw()], spec());
        assert_eq!(chain.len(), 1, "only the accepting filter joins");
        assert_eq!(
            skipped,
            vec![WaveFilterSkip {
                id: rejecting_id.raw(),
                reason: "HiRes format not supported.".to_string(),
            }]
        );

        let mut samples = [0.0f32, 0.0];
        chain.process(&mut samples);
        assert!(
            !entries(&log)
                .iter()
                .any(|entry| entry == "process:rejecting"),
            "a refused filter must not process, got {:?}",
            entries(&log)
        );

        unregister_wave_filter(rejecting_id);
        unregister_wave_filter(accepting_id);
    }

    /// A filter that answers a different format than it was connected with is
    /// refused: the render path is fixed (stereo at the sound's rate), so
    /// [`WaveFilterChain::spec`] always reports the sound's format and the
    /// refusal is reported rather than silently ignored — for either half of
    /// the format.
    #[test]
    fn a_filter_changing_the_format_is_refused_and_reported() {
        struct ToMono;

        impl WaveFilter for ToMono {
            fn recreate(&self, spec: PcmAudioSpec) -> std::result::Result<PcmAudioSpec, String> {
                Ok(PcmAudioSpec {
                    sample_rate: spec.sample_rate,
                    channels: 1,
                })
            }
            fn clear(&self) {}
            fn update(&self) {}
            fn reset(&self) {}
            fn process(&self, frames: &mut [f32]) {
                for sample in frames.iter_mut() {
                    *sample *= 3.0;
                }
            }
        }

        struct HalfRate;

        impl WaveFilter for HalfRate {
            fn recreate(&self, spec: PcmAudioSpec) -> std::result::Result<PcmAudioSpec, String> {
                Ok(PcmAudioSpec {
                    sample_rate: spec.sample_rate / 2,
                    channels: spec.channels,
                })
            }
            fn clear(&self) {}
            fn update(&self) {}
            fn reset(&self) {}
            fn process(&self, _frames: &mut [f32]) {}
        }

        let to_mono = register_wave_filter(Arc::new(ToMono));
        let half_rate = register_wave_filter(Arc::new(HalfRate));
        for (id, changed) in [(to_mono, "ch"), (half_rate, "Hz")] {
            let (chain, skipped) = WaveFilterChain::build(&[id.raw()], spec());
            assert!(chain.is_empty(), "the changing filter must not join");
            assert_eq!(skipped.len(), 1);
            assert_eq!(skipped[0].id, id.raw());
            assert!(
                skipped[0].reason.contains(changed),
                "the reason names what changed ({changed}): {:?}",
                skipped[0].reason
            );
            unregister_wave_filter(id);
        }
    }

    /// The chain releases a filter it refuses.  A filter takes its one-source
    /// claim inside `recreate` and releases it in `clear`; without the release
    /// a refused filter would stay claimed for the rest of the process — it
    /// could never join a chain again, and the next attempt would be reported
    /// as the one-source rule instead of the format.
    #[test]
    fn a_filter_the_chain_refuses_is_released() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct ClaimingMono {
            connected: AtomicBool,
        }

        impl WaveFilter for ClaimingMono {
            fn recreate(&self, spec: PcmAudioSpec) -> std::result::Result<PcmAudioSpec, String> {
                if self.connected.swap(true, Ordering::SeqCst) {
                    return Err("Cannot connect multiple wave sound buffer at once.".to_string());
                }
                Ok(PcmAudioSpec {
                    sample_rate: spec.sample_rate,
                    channels: 1,
                })
            }
            fn clear(&self) {
                self.connected.store(false, Ordering::SeqCst);
            }
            fn update(&self) {}
            fn reset(&self) {}
            fn process(&self, _frames: &mut [f32]) {}
        }

        let filter = Arc::new(ClaimingMono {
            connected: AtomicBool::new(false),
        });
        let id = register_wave_filter(Arc::clone(&filter) as Arc<dyn WaveFilter>);

        let (chain, skipped) = WaveFilterChain::build(&[id.raw()], spec());
        assert!(chain.is_empty());
        assert!(
            skipped[0].reason.contains("format"),
            "the first refusal is the format: {skipped:?}"
        );
        assert!(
            !filter.connected.load(Ordering::SeqCst),
            "the refused filter must be released"
        );

        // The second attempt fails for the same reason — a leaked claim would
        // answer the one-source rule here.
        let (chain, skipped) = WaveFilterChain::build(&[id.raw()], spec());
        assert!(chain.is_empty());
        assert!(
            skipped[0].reason.contains("format"),
            "a leaked claim would blame the one-source rule: {skipped:?}"
        );
        unregister_wave_filter(id);
    }

    /// An `interface` value nobody registered (a script-fabricated one) is
    /// skipped and named — never silently treated as a filter.
    #[test]
    fn an_unregistered_interface_value_is_reported_not_invented() {
        let (chain, skipped) = WaveFilterChain::build(&[0x5746_dead], spec());
        assert!(chain.is_empty());
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].id, 0x5746_dead);
        assert!(
            skipped[0].reason.contains("no filter is registered"),
            "got {:?}",
            skipped[0]
        );
    }

    /// The registry hands every registration a distinct id and stops resolving
    /// it at `unregister` — the lifetime the shipped filter classes get from
    /// construction and `finalize`.
    #[test]
    fn registration_ids_are_unique_and_stop_resolving_on_unregister() {
        let log = log();
        let first = register_wave_filter(RecordingFilter::new("a", &log));
        let second = register_wave_filter(RecordingFilter::new("b", &log));
        assert_ne!(first.raw(), second.raw());
        assert!(resolve_wave_filter(first.raw()).is_some());
        assert!(unregister_wave_filter(first));
        assert!(resolve_wave_filter(first.raw()).is_none());
        assert!(resolve_wave_filter(second.raw()).is_some());
        assert!(
            !unregister_wave_filter(first),
            "unregister is not idempotent"
        );
        unregister_wave_filter(second);
    }
}
