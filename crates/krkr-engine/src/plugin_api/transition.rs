//! Transition handler providers for plugins — `TVPAddTransHandlerProvider`.
//!
//! A KRKR transition is not a TJS object: it is a C++ **provider** registered
//! under a name, plus a per-playback **handler** the engine drives each frame.
//!
//! * `iTVPTransHandlerProvider` (`krkrz/src/core/visual/transhandler.h:306-332`)
//!   owns a name (`GetName`, `:313`) and a factory (`StartTransition`,
//!   `:317-332`) that builds an `iTVPBaseTransHandler` for one playback.
//! * `iTVPDivisibleTransHandler` (`:242-276`) is that handler: `StartProcess`/
//!   `EndProcess` bracket one frame (`LayerIntf.cpp:5056-5108`), and `Process`
//!   (`transhandler.h:255`) receives the destination rectangle plus the
//!   writable `Dest` bitmap and the read-only `Src1` (the destination layer's
//!   own image, `LayerIntf.cpp:6592`) and `Src2` (the transition source
//!   layer's image, `:6611`).
//! * `TVPAddTransHandlerProvider`/`TVPRemoveTransHandlerProvider`
//!   (`TransIntf.cpp:307-338`) insert and delete it in a name-keyed table; a
//!   duplicate name throws `TVPTransAlreadyRegistered` ("Transition %1 already
//!   registerd", `vc2012/string_table_en.rc`, the reference's spelling), and
//!   `TVPFindTransHandlerProvider` (`:341-359`) is the exact, case-sensitive
//!   find `Layer.beginTransition` runs **before** it reads any option
//!   (`LayerIntf.cpp:6206`); a miss throws `TVPCannotFindTransHander`.
//!
//! [`TransitionHandlerProvider`] and [`TransitionHandler`] are those two
//! interfaces, and [`register_transition_provider`] /
//! [`unregister_transition_provider`] are the two registry calls.  A plugin
//! calls them from [`crate::KrkrPlugin::register`] / `unregister` — the
//! reference's `V2Link` / `V2Unlink` — so a provider answers exactly while its
//! module is linked and its names fall back to the official unknown-name error
//! once it is not.  This replaces the engine-side projection that answered a
//! linked plugin shim's names with a crossfade: GlitchEffect's three providers
//! (`glitch`, `fadeglitch`, `loopglitch`) and extNagano's twelve
//! (`3duniversal`, `blurfade`, `imagewipe`, `morphing`, `multiripple`, `book`,
//! `honeyturn`, `flutter`, `rgbfade`, `scanline`, `spin`, `zoomfade`) are
//! exactly the names a plugin now registers instead of the engine carrying a
//! table (`docs/plugins/transitions.md` §5.3).
//!
//! # What a registered provider owns
//!
//! A name and a factory.  Registration stores the provider in
//! [`KrkrHost`]'s registry under `name()`; the name is matched verbatim, so
//! `"Wave"` never answers for `"wave"` (`TransIntf.cpp:351`).  A name the
//! engine's own kernels already answer to (`crossfade`, `universal`, `scroll`
//! and extrans' seven — `krkr_core::TRANSITION_PROVIDER_NAMES`) cannot be
//! registered: those are the reference's default providers
//! (`TVPRegisterDefaultTransHandlerProvider`, `TransIntf.cpp:1196-1213`) and
//! the extrans plugin's registrations, which are always present in this build.
//! Registering an already-taken name fails the way `TVPAddTransHandlerProvider`
//! does (`:320-321`); re-registering the *same* provider object is a no-op,
//! because this engine runs `KrkrPlugin::register` twice (boot and the first
//! `Plugins.link`), unlike the reference's single `V2Link`.
//!
//! A running transition holds its own started handler; removing the provider
//! from the registry (`unregister_transition_provider`, the plugin's
//! `V2Unlink`) neither stops nor invalidates it — the reference keeps the
//! handler alive through its own ref-count (`pro->Release()` after
//! `StartTransition`, `LayerIntf.cpp:6348`, releases the provider while the
//! layer owns the handler).
//!
//! # The channel: a CPU composite over the two layer bitmaps
//!
//! The engine hands the handler the two layers' **own** bitmaps — `Src1` is
//! the destination layer's image captured when the transition started and
//! stays fixed, `Src2` is the source layer's image re-read every pass
//! (`TransSrc->Complete(destrect)`, `LayerIntf.cpp:6604`) — and calls
//! [`TransitionHandler::process`] once per engine tick.  The pass writes into
//! a destination buffer that starts as a copy of `Src1`; the engine then
//! replaces the destination layer's image with it, which is what the
//! reference's `tTransDrawable::DrawCompleted` (`:6575`) does to the layer's
//! target bitmap.  No kernel, no WGSL — a plugin implements pixel work in Rust,
//! which is what GlitchEffect's handlers are (`docs/plugins/transitions.md`
//! §4.2: a layer-to-layer pixel pass).
//!
//! **Byte order**: the faces are the engine's own store — R, G, B, A per
//! pixel, top-down, tightly packed — not the reference's memory-order BGRA
//! (`docs/plugins/layer-ex-family.md` §1); a ported algorithm that indexes
//! byte 0 as blue must index byte 2 instead (`plugin_api::layer` §B.3.4).
//! Both faces are presented in their own layer-local coordinates, the way the
//! reference hands `Src1`/`Src2` the same processing rectangle.  A pass's
//! output buffer is `dest_size.0 * dest_size.1 * 4` bytes — the destination
//! layer's own image, and what the engine replaces each pass.
//!
//! **The engine checks none of the built-in family's options for a provider
//! name.**  The crossfade family's `Specify option time`/`Specify option rule`
//! and its same-size requirement (`TransIntf.cpp:508-529`, `:777`) are that
//! family's own `StartTransition` rules, so a provider sees the raw request —
//! unsized, unclamped, every member — and enforces its own (extrans fails on
//! `src1 != src2` inside each provider, `wave.cpp:313-320`).  The one rule the
//! engine does apply is the caller's clock: an absent `time` leaves no clock,
//! so the call takes the engine's immediate projection — the factory still
//! runs (exactly as `pro->StartTransition` does), and the handler it returns
//! is dropped without a pass.
//!
//! # What this channel does not model
//!
//! * **The KAG `[trans]` page projection.**  The tag's transition is the
//!   engine's own whole-tree projection (`Host::begin_kag_transition`), which
//!   has no destination/source layer pair to hand a CPU handler; a registered
//!   provider's name keeps the projection's crossfade composite there.
//! * **Child subtrees.**  The reference also transitions each child's draw
//!   through `tTransDrawable::DrawCompleted` per region; this channel composes
//!   the destination layer's own image only, so a `withchildren` call whose
//!   destination has children leaves those children drawing live.
//! * **`tutGiveUpdate` / `ttSimple`.**  The script path is the reference's
//!   `ttExchange` with a source layer; `iTVPGiveUpdateTransHandler` is refused
//!   by the reference itself (`LayerIntf.cpp:6264-6266`) and `ttSimple` has no
//!   source layer to hand over.  Both remain unimplemented here.
//! * **`MakeFinalImage`.**  Declared in the interface but never called by the
//!   reference core (`LayerIntf.cpp` has no call site); the destination keeps
//!   the handler's last pass, and the stop's `Exchange`/`Swap` moves content
//!   between the two layer objects as it does for every other method.
//!
//! **Panics**: a panic inside `process` unwinds through the engine tick like
//! any host callback; the engine does not catch-unwind, and the handler's lock
//! is taken again by the next pass (the callers use a poison-tolerant lock).
//!
//! **Threading**: `process` runs on the engine's frame thread and takes
//! `&mut self`, so a handler needs no internal synchronization.

use std::{fmt, sync::Arc, time::Duration};

use krkr_tjs2::runtime::{ObjectHandle, Runtime, Variant};

use crate::host::KrkrHost;

/// `iTVPTransHandlerProvider` (`transhandler.h:306-332`): a plugin's transition
/// provider — one exact name plus the factory that builds a handler for one
/// playback.
///
/// Implement it in the plugin crate and register it through
/// [`register_transition_provider`] in
/// [`KrkrPlugin::register`](crate::KrkrPlugin::register); the handler factory
/// runs when the script's [`Layer.beginTransition`] starts a transition under
/// this name.  (The KAG `[trans]` page projection does not reach a handler;
/// see the module docs.)
pub trait TransitionHandlerProvider: Send + Sync {
    /// `GetName` (`transhandler.h:313`): the exact, case-sensitive name this
    /// provider answers to.  `TVPFindTransHandlerProvider`
    /// (`TransIntf.cpp:341-359`) hashes the caller's spelling verbatim, so
    /// `"Wave"` is not `"wave"`.
    fn name(&self) -> &str;

    /// `StartTransition` (`transhandler.h:317-332`): build the handler this
    /// playback drives.
    ///
    /// Returning `Err` aborts the transition with the reference's
    /// `TVPTransHandlerError` text ("Transition handler error
    /// iTVPTransHandlerProvider::StartTransition failed", `LayerIntf.cpp:6246`
    /// and `IDS_TVP_TRANS_HANDLER_ERROR`); the provider's own
    /// [`TransitionHandlerError::message`] is logged, not shown to the script,
    /// exactly as the reference swallows the returned `tjs_error`.
    fn start_transition(
        &self,
        request: &TransitionRequest,
    ) -> Result<Box<dyn TransitionHandler>, TransitionHandlerError>;
}

/// `iTVPDivisibleTransHandler` (`transhandler.h:242-276`): the per-playback
/// handler a provider's factory builds.
///
/// The engine calls [`process`](Self::process) once per frame while the
/// transition runs; the last pass stays on screen when the transition stops
/// (or, for `selfupdate`, until `Layer.update()` asks for the next one).
pub trait TransitionHandler: Send {
    /// `Process` (`transhandler.h:255`): compose one pass into `dest`.
    ///
    /// `frame.dest_before` is `Src1`, `frame.source` is `Src2`, and `dest` is
    /// the writable `Dest` (`tTVPDivisibleData::Dest`, `LayerIntf.cpp:6532`).
    /// `dest` is `dest_before`'s length and starts as a copy of `Src1`, so a
    /// pass that writes nothing keeps the previous frame (`Phase == 0` hands
    /// `Dest` back as `Src1`, `TransIntf.cpp:598-603`).
    fn process(&mut self, frame: TransitionFrame<'_>, dest: &mut [u8]);
}

/// Everything `iTVPTransHandlerProvider::StartTransition` receives
/// (`transhandler.h:317-332`), captured when the transition starts.
#[derive(Clone, Debug)]
pub struct TransitionRequest {
    /// The `options` argument's members, captured once.  The reference wraps
    /// the script object in `tTVPSimpleOptionProvider` (`TransIntf.cpp:26-114`)
    /// and reads through it; a Rust handler gets a snapshot because its
    /// `process` pass runs on the frame tick, outside the script call.  Every
    /// reference provider reads its options at `StartTransition` time too
    /// (`wave.cpp:324-347`).
    pub options: TransitionOptions,
    /// The destination layer's `DisplayType` (`layertype` in the reference):
    /// the TJS `type` member (`ltOpaque` 1, `ltAlpha` 2, ...).
    pub dest_layer_type: i32,
    /// `src1w`/`src1h`: the destination layer's own bitmap size.
    pub dest_size: (u32, u32),
    /// `src2w`/`src2h`: the transition source layer's bitmap size.  `None` when
    /// the call has no source layer (the reference passes `0, 0`).
    pub source_size: Option<(u32, u32)>,
}

/// One [`TransitionHandler::process`] pass's input.
pub struct TransitionFrame<'a> {
    /// The handler clock: milliseconds since the transition started
    /// (`StartProcess(tick)`, `LayerIntf.cpp:5063`).
    pub tick: Duration,
    /// The phase the kernels call `Phase`: `tick / time`, clamped to
    /// `0.0..=1.0`.  A transition whose call supplied no `time` never reaches
    /// a handler (see the module docs), so a running handler's first pass is
    /// `0.0` and the last is just below `1.0`.
    pub progress: f32,
    /// The same options snapshot [`TransitionHandlerProvider::start_transition`]
    /// received.
    pub options: &'a TransitionOptions,
    /// `Src1` (`LayerIntf.cpp:6592`): the destination layer's own bitmap when
    /// the transition started.  Fixed for the whole playback.
    pub dest_before: TransitionFace<'a>,
    /// `Src2` (`LayerIntf.cpp:6611`): the transition source layer's own bitmap,
    /// re-read every pass.  `None` when the layer lost its image.
    pub source: Option<TransitionFace<'a>>,
}

/// One face's pixels: R, G, B, A per pixel, top-down, tightly packed.
#[derive(Clone, Copy, Debug)]
pub struct TransitionFace<'a> {
    pub pixels: &'a [u8],
    pub width: u32,
    pub height: u32,
}

/// The `options` argument of a transition call, captured once.
///
/// `tTVPSimpleOptionProvider` (`TransIntf.cpp:26-114`) resolves each name
/// through the script object at read time; a snapshot is the Rust-side
/// equivalent, taken when `StartTransition` runs.  A member the object does
/// not have reads back as `None`, which is the reference's
/// `TJS_E_MEMBERNOTFOUND` from `GetAsNumber` (`:57-76`).
#[derive(Clone, Debug, Default)]
pub struct TransitionOptions {
    entries: Vec<(String, Variant)>,
}

impl TransitionOptions {
    /// Builds a snapshot from `(name, value)` pairs, for a provider's own
    /// tests.
    pub fn new(entries: impl IntoIterator<Item = (String, Variant)>) -> Self {
        Self {
            entries: entries.into_iter().collect(),
        }
    }

    /// The raw member `name` carries, or `None` when absent.
    pub fn value(&self, name: &str) -> Option<&Variant> {
        self.entries
            .iter()
            .find(|(entry, _)| entry == name)
            .map(|(_, value)| value)
    }

    /// `iTVPSimpleOptionProvider::GetAsNumber` (`transhandler.h:118`): `name`
    /// converted to an integer, or `None` when absent or not convertible.
    pub fn integer(&self, name: &str) -> Option<i64> {
        self.value(name)?.to_integer().ok()
    }

    /// `GetAsNumber`'s real form: `name` as a double.
    pub fn number(&self, name: &str) -> Option<f64> {
        self.value(name)?.to_real().ok()
    }

    /// `iTVPSimpleOptionProvider::GetAsString` (`transhandler.h:122`): `name`
    /// converted to a string, or `None` when absent or not convertible.
    pub fn string(&self, name: &str) -> Option<String> {
        self.value(name)?.to_tjs_string().ok()
    }

    /// TJS truthiness of `name`; an absent member is `false`.
    pub fn flag(&self, name: &str) -> bool {
        self.value(name).is_some_and(Variant::is_truthy)
    }

    /// Every member name the snapshot carries, in object order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(name, _)| name.as_str())
    }

    /// Everything the object's members held when the transition started.
    pub(crate) fn snapshot(runtime: &Runtime<KrkrHost>, options: Option<ObjectHandle>) -> Self {
        let Some(options) = options else {
            return Self::default();
        };
        Self {
            entries: runtime.object_members(options),
        }
    }
}

/// A provider's `StartTransition` failure.
///
/// The script sees the reference's message-only `TVPTransHandlerError`
/// ("Transition handler error iTVPTransHandlerProvider::StartTransition
/// failed"); `message` is the provider's own reason, which the engine logs so
/// the host diagnostics keep the cause the reference discards.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransitionHandlerError {
    message: String,
}

impl TransitionHandlerError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// The provider's own failure text.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for TransitionHandlerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TransitionHandlerError {}

/// Registers `provider` under its [`name`](TransitionHandlerProvider::name) —
/// `TVPAddTransHandlerProvider` (`TransIntf.cpp:307-324`).
///
/// Call it from [`KrkrPlugin::register`](crate::KrkrPlugin::register), keeping
/// the provider in a `OnceLock` and handing back the same `Arc` on every call:
/// the engine runs `register` at boot *and* when the first `Plugins.link`
/// installs the module, and a re-registration of the identical `Arc` is a
/// no-op.  A *different* provider under a name that is already taken fails
/// with the reference's `TVPTransAlreadyRegistered` text, as does a name the
/// engine's own kernels answer to.
pub fn register_transition_provider(
    runtime: &mut Runtime<KrkrHost>,
    provider: Arc<dyn TransitionHandlerProvider>,
) -> krkr_tjs2::Result<()> {
    runtime.host_mut().register_transition_provider(provider)
}

/// Removes the provider registered under `name` —
/// `TVPRemoveTransHandlerProvider` (`TransIntf.cpp:326-338`) — and reports
/// whether one was there.
///
/// Call it from [`KrkrPlugin::unregister`](crate::KrkrPlugin::unregister), the
/// reference's `V2Unlink`.  A transition already running keeps its started
/// handler; only new lookups see the miss, which is the reference's behaviour
/// (the removed table entry does not touch the layer's handler reference).
pub fn unregister_transition_provider(runtime: &mut Runtime<KrkrHost>, name: &str) -> bool {
    runtime.host_mut().unregister_transition_provider(name)
}

/// The names the host currently answers for, sorted.  The reference has no
/// enumerator; this exists for host diagnostics and tests.
pub fn transition_provider_names(runtime: &Runtime<KrkrHost>) -> Vec<String> {
    runtime.host().transition_provider_names()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, OnceLock};

    use krkr_core::{FrameInput, Size};

    use crate::{EngineConfig, EngineInput, KrkrEngine, KrkrPlugin};

    use super::*;

    /// What the mock provider saw, so the test can assert the faces, options
    /// and clock the engine handed it.
    #[derive(Default)]
    struct Recording {
        starts: Vec<StartRecord>,
        passes: Vec<PassRecord>,
    }

    #[derive(Debug, PartialEq)]
    struct StartRecord {
        dest_size: (u32, u32),
        source_size: Option<(u32, u32)>,
        time: Option<i64>,
        dest_layer_type: i32,
        options: Vec<String>,
    }

    #[derive(Debug, PartialEq)]
    struct PassRecord {
        tick_millis: u64,
        progress: f32,
        dest_before: Option<[u8; 4]>,
        source: Option<[u8; 4]>,
    }

    /// A provider in the shape a plugin would register: one exact name and a
    /// factory for the per-playback handler.
    struct MockProvider {
        name: &'static str,
        fail: bool,
        log: Arc<Mutex<Recording>>,
    }

    impl MockProvider {
        fn new(name: &'static str, log: Arc<Mutex<Recording>>) -> Self {
            Self {
                name,
                fail: false,
                log,
            }
        }

        fn failing(name: &'static str, log: Arc<Mutex<Recording>>) -> Self {
            Self {
                name,
                fail: true,
                log,
            }
        }
    }

    impl TransitionHandlerProvider for MockProvider {
        fn name(&self) -> &str {
            self.name
        }

        fn start_transition(
            &self,
            request: &TransitionRequest,
        ) -> std::result::Result<Box<dyn TransitionHandler>, TransitionHandlerError> {
            self.log
                .lock()
                .expect("recording")
                .starts
                .push(StartRecord {
                    dest_size: request.dest_size,
                    source_size: request.source_size,
                    time: request.options.integer("time"),
                    dest_layer_type: request.dest_layer_type,
                    options: request.options.names().map(str::to_string).collect(),
                });
            if self.fail {
                return Err(TransitionHandlerError::new("mock start failure"));
            }
            Ok(Box::new(MockHandler {
                log: Arc::clone(&self.log),
            }))
        }
    }

    /// Copies `Src2` over the destination and stamps a marker pixel, so the
    /// test can tell a pass ran even between two equal faces.
    struct MockHandler {
        log: Arc<Mutex<Recording>>,
    }

    impl TransitionHandler for MockHandler {
        fn process(&mut self, frame: TransitionFrame<'_>, dest: &mut [u8]) {
            self.log.lock().expect("recording").passes.push(PassRecord {
                tick_millis: frame.tick.as_millis() as u64,
                progress: frame.progress,
                dest_before: first_pixel(frame.dest_before),
                source: frame.source.and_then(first_pixel),
            });
            if let Some(source) = frame.source
                && source.pixels.len() == dest.len()
            {
                dest.copy_from_slice(source.pixels);
            }
            if dest.len() >= 4 {
                dest[0..4].copy_from_slice(&[0xaa, 0xbb, 0xcc, 0xff]);
            }
        }
    }

    fn first_pixel(face: TransitionFace<'_>) -> Option<[u8; 4]> {
        (face.pixels.len() >= 4).then(|| {
            [
                face.pixels[0],
                face.pixels[1],
                face.pixels[2],
                face.pixels[3],
            ]
        })
    }

    /// A visible 4×4 layer in one colour: destination red, source blue.
    const LAYER_PAIR: &str = r#"
        global.dest = new Layer();
        dest.setImageSize(4, 4);
        dest.fillRect(0, 0, 4, 4, 0xffff0000);
        dest.visible = true;
        global.source = new Layer();
        source.setImageSize(4, 4);
        source.fillRect(0, 0, 4, 4, 0xff0000ff);
        source.visible = true;
    "#;

    fn engine() -> KrkrEngine {
        KrkrEngine::new(EngineConfig::default()).expect("engine")
    }

    fn input() -> EngineInput {
        EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new())
    }

    fn pixel(engine: &mut KrkrEngine, expression: &str) -> Variant {
        engine
            .execute_expression("pixel.tjs", expression)
            .expect("expression")
    }

    /// The end-to-end path a plugin's provider takes: register, resolve the
    /// name through the script `Layer.beginTransition`, let the handler
    /// compose the destination layer's bitmap each tick, and stop.
    #[test]
    fn a_registered_provider_runs_its_handler_through_the_script_path() {
        let mut engine = engine();
        let log = Arc::new(Mutex::new(Recording::default()));
        register_transition_provider(
            engine.tjs_runtime_mut(),
            Arc::new(MockProvider::new("mockfade", Arc::clone(&log))),
        )
        .expect("register");

        engine
            .execute_script(
                "inline.tjs",
                &format!(
                    r#"
                    {LAYER_PAIR}
                    global.completed = 0;
                    dest.onTransitionCompleted = function(d, s) {{ global.completed = 1; }};
                    dest.beginTransition("mockfade", true, source, %[time: 10]);
                    "#
                ),
            )
            .expect("begin transition");

        {
            let log = log.lock().expect("recording");
            assert_eq!(
                log.starts,
                vec![StartRecord {
                    dest_size: (4, 4),
                    source_size: Some((4, 4)),
                    time: Some(10),
                    dest_layer_type: 2,
                    options: vec!["time".to_string()],
                }]
            );
            // `StartTransition` ends with `Update(true)` (`LayerIntf.cpp:6344`),
            // so the first pass of the clock is already composed.
            assert_eq!(
                log.passes.first().expect("first pass"),
                &PassRecord {
                    tick_millis: 0,
                    progress: 0.0,
                    dest_before: Some([255, 0, 0, 255]),
                    source: Some([0, 0, 255, 255]),
                }
            );
        }

        let frame = engine.update(input(), Duration::ZERO).expect("frame");
        assert!(
            frame.output.transitions.is_empty(),
            "a provider transition is composed CPU-side, not by a kernel"
        );
        assert!(
            frame
                .output
                .image_uploads
                .iter()
                .any(|upload| upload.rgba.len() >= 4
                    && upload.rgba[0..4] == [0xaa, 0xbb, 0xcc, 0xff]),
            "the composite reaches the renderer as the destination layer's upload"
        );
        assert_eq!(
            pixel(&mut engine, "dest.getMainPixel(0, 0)"),
            Variant::Integer(0xaabbcc),
            "the handler's output is the destination layer's image"
        );
        assert_eq!(
            pixel(&mut engine, "dest.getMainPixel(3, 3)"),
            Variant::Integer(0x0000ff),
            "the rest of the buffer is the handler's Src2 copy"
        );
        assert_eq!(pixel(&mut engine, "completed"), Variant::Integer(0));
        assert!(engine.host().has_active_transition());

        // The clock reaches `time`, the transition stops, and the stop fires
        // `onTransitionCompleted` with the usual exchange.
        engine
            .update(input(), Duration::from_millis(10))
            .expect("frame");
        assert!(!engine.host().has_active_transition());
        assert_eq!(pixel(&mut engine, "completed"), Variant::Integer(1));
        assert!(
            log.lock().expect("recording").passes.len() >= 2,
            "every engine tick composes one pass"
        );
    }

    /// A `selfupdate` provider transition is driven by `Layer.update()` alone
    /// (`TransSelfUpdate`, `LayerIntf.cpp:6211`): the idle tick only piles the
    /// frame time up, and the pass runs when the script asks for it.
    #[test]
    fn a_self_updated_provider_transition_composes_when_the_script_asks() {
        let mut engine = engine();
        let log = Arc::new(Mutex::new(Recording::default()));
        register_transition_provider(
            engine.tjs_runtime_mut(),
            Arc::new(MockProvider::new("mockfade", Arc::clone(&log))),
        )
        .expect("register");
        engine
            .execute_script(
                "inline.tjs",
                &format!(
                    r#"
                    {LAYER_PAIR}
                    dest.beginTransition("mockfade", true, source, %[time: 100, selfupdate: 1]);
                    "#
                ),
            )
            .expect("begin transition");
        assert_eq!(
            log.lock().expect("recording").passes.len(),
            1,
            "the start pass runs at tick zero"
        );

        engine
            .update(input(), Duration::from_millis(50))
            .expect("frame");
        assert!(
            engine.host().has_active_transition(),
            "a self-updated transition waits for the script"
        );
        assert_eq!(
            log.lock().expect("recording").passes.len(),
            1,
            "the idle tick does not compose a self-updated transition"
        );

        engine
            .execute_script("update.tjs", "dest.update();")
            .expect("script-driven pass");
        let log = log.lock().expect("recording");
        let last = log.passes.last().expect("pass after update");
        assert_eq!(last.tick_millis, 50);
        assert!((last.progress - 0.5).abs() < 0.001);
    }

    /// Unregistering is `V2Unlink`: the name is the official unknown-name
    /// error again, and a later re-registration answers it once more.
    #[test]
    fn an_unregistered_name_is_the_official_miss_again() {
        let mut engine = engine();
        let log = Arc::new(Mutex::new(Recording::default()));
        register_transition_provider(
            engine.tjs_runtime_mut(),
            Arc::new(MockProvider::new("mockfade", Arc::clone(&log))),
        )
        .expect("register");
        assert_eq!(
            transition_provider_names(engine.tjs_runtime()),
            vec!["mockfade".to_string()]
        );
        assert!(unregister_transition_provider(
            engine.tjs_runtime_mut(),
            "mockfade"
        ));
        assert!(!unregister_transition_provider(
            engine.tjs_runtime_mut(),
            "mockfade"
        ));
        assert!(transition_provider_names(engine.tjs_runtime()).is_empty());

        engine
            .execute_script("inline.tjs", LAYER_PAIR)
            .expect("layers");
        let message = engine
            .execute_script(
                "inline.tjs",
                r#"
                var message = "";
                try { dest.beginTransition("mockfade", true, source, %[time: 10]); }
                catch (e) { message = e.message; }
                return message;
                "#,
            )
            .expect("script");
        assert_eq!(
            message,
            Variant::String("Cannot find transition handler mockfade".to_string())
        );

        register_transition_provider(
            engine.tjs_runtime_mut(),
            Arc::new(MockProvider::new("mockfade", Arc::clone(&log))),
        )
        .expect("re-register");
        engine
            .execute_script(
                "inline.tjs",
                r#"dest.beginTransition("mockfade", true, source, %[time: 10]);"#,
            )
            .expect("the name answers again");
    }

    /// A failed factory is the reference's message-only `TVPTransHandlerError`
    /// (`LayerIntf.cpp:6246`), not a provider-specific script error.
    #[test]
    fn a_failed_start_reports_the_official_handler_error() {
        let mut engine = engine();
        let log = Arc::new(Mutex::new(Recording::default()));
        register_transition_provider(
            engine.tjs_runtime_mut(),
            Arc::new(MockProvider::failing("failfade", log)),
        )
        .expect("register");
        engine
            .execute_script("inline.tjs", LAYER_PAIR)
            .expect("layers");
        let message = engine
            .execute_script(
                "inline.tjs",
                r#"
                var message = "";
                try { dest.beginTransition("failfade", true, source, %[time: 10]); }
                catch (e) { message = e.message; }
                return message;
                "#,
            )
            .expect("script");
        assert_eq!(
            message,
            Variant::String(
                "Transition handler error iTVPTransHandlerProvider::StartTransition failed"
                    .to_string()
            )
        );
        assert!(!engine.host().has_active_transition());
    }

    /// `TVPAddTransHandlerProvider` refuses a taken name (`TransIntf.cpp:320`);
    /// the engine's own kernel names are always registered first, and the same
    /// provider object may be handed in twice because this engine runs
    /// `KrkrPlugin::register` at boot and at the first `Plugins.link`.
    #[test]
    fn registration_refuses_taken_and_engine_names() {
        let mut engine = engine();
        let log = Arc::new(Mutex::new(Recording::default()));
        let provider: Arc<dyn TransitionHandlerProvider> =
            Arc::new(MockProvider::new("mockfade", Arc::clone(&log)));
        register_transition_provider(engine.tjs_runtime_mut(), Arc::clone(&provider))
            .expect("register");
        register_transition_provider(engine.tjs_runtime_mut(), Arc::clone(&provider))
            .expect("the same provider object is a no-op");

        let error = register_transition_provider(
            engine.tjs_runtime_mut(),
            Arc::new(MockProvider::new("mockfade", Arc::clone(&log))),
        )
        .expect_err("a different provider under the name fails");
        assert_eq!(error.message, "Transition mockfade already registerd");

        let error = register_transition_provider(
            engine.tjs_runtime_mut(),
            Arc::new(MockProvider::new("wave", Arc::clone(&log))),
        )
        .expect_err("an engine kernel name is already registered");
        assert_eq!(error.message, "Transition wave already registerd");

        // The lookup is exact and case-sensitive (`TransIntf.cpp:351`), so a
        // differently spelled name is free.
        register_transition_provider(
            engine.tjs_runtime_mut(),
            Arc::new(MockProvider::new("Wave", Arc::clone(&log))),
        )
        .expect("`Wave` is not `wave`");
    }

    /// The engine's own providers — the three built-ins and extrans' seven
    /// kernels — still resolve to their kernels with a registry in place.
    #[test]
    fn engine_and_extrans_names_still_resolve_to_their_kernels() {
        let mut engine = engine();
        let names = [
            "crossfade",
            "scroll",
            "wave",
            "mosaic",
            "turn",
            "rotatezoom",
            "rotatevanish",
            "rotateswap",
            "ripple",
        ];
        let mut script = String::new();
        for (index, name) in names.iter().enumerate() {
            script.push_str(&format!(
                r#"
                var dest{index} = new Layer();
                dest{index}.setImageSize(4, 4);
                dest{index}.fillRect(0, 0, 4, 4, 0xffff0000);
                dest{index}.visible = true;
                var source{index} = new Layer();
                source{index}.setImageSize(4, 4);
                source{index}.fillRect(0, 0, 4, 4, 0xff0000ff);
                source{index}.visible = true;
                dest{index}.beginTransition("{name}", false, source{index}, %[time: 10]);
                "#
            ));
        }
        engine
            .execute_script("inline.tjs", &script)
            .expect("kernel transitions");
        let frame = engine.update(input(), Duration::ZERO).expect("frame");
        let started = frame
            .output
            .transitions
            .iter()
            .map(|transition| transition.method.as_str())
            .collect::<Vec<_>>();
        assert_eq!(started.len(), names.len());
        for name in names {
            assert!(started.contains(&name), "`{name}` resolved to its kernel");
        }

        // `universal` stops in the kernel path with its own option error, which
        // proves the name resolved there and not to the registry's miss.  A
        // fresh pair is used: `dest0` already runs its own transition.
        let message = engine
            .execute_script(
                "inline.tjs",
                r#"
                var message = "";
                var dest9 = new Layer();
                dest9.setImageSize(4, 4);
                dest9.visible = true;
                var source9 = new Layer();
                source9.setImageSize(4, 4);
                source9.visible = true;
                try { dest9.beginTransition("universal", true, source9, %[time: 10]); }
                catch (e) { message = e.message; }
                return message;
                "#,
            )
            .expect("script");
        assert_eq!(message, Variant::String("Specify option rule".to_string()));
    }

    /// The registry is consulted before the engine's interim linked-plugin
    /// projection (M54's `PLUGIN_TRANSITION_NAMES`): once a provider answers a
    /// name, that name runs the handler instead of the crossfade degrade.
    #[test]
    fn a_registered_provider_wins_over_the_linked_plugin_projection() {
        struct ExtNaganoShim;

        impl KrkrPlugin for ExtNaganoShim {
            fn name(&self) -> &str {
                "extNagano.dll"
            }

            fn register(&self, _runtime: &mut Runtime<KrkrHost>) -> krkr_tjs2::Result<()> {
                Ok(())
            }
        }

        let mut engine = engine();
        engine.register_plugin(ExtNaganoShim).expect("shim plugin");
        engine
            .execute_script("inline.tjs", LAYER_PAIR)
            .expect("layers");
        engine
            .execute_script(
                "inline.tjs",
                r#"dest.beginTransition("blurfade", false, source, %[time: 10]);"#,
            )
            .expect("the linked shim answers the name");
        let frame = engine.update(input(), Duration::ZERO).expect("frame");
        assert_eq!(
            frame.output.transitions.first().expect("transition").method,
            "crossfade",
            "without a provider the shim's name stays the projection"
        );

        let log = Arc::new(Mutex::new(Recording::default()));
        register_transition_provider(
            engine.tjs_runtime_mut(),
            Arc::new(MockProvider::new("blurfade", Arc::clone(&log))),
        )
        .expect("register");
        engine
            .execute_script(
                "inline.tjs",
                r#"
                dest.stopTransition();
                var dest2 = new Layer();
                dest2.setImageSize(4, 4);
                dest2.fillRect(0, 0, 4, 4, 0xffff0000);
                dest2.visible = true;
                dest2.beginTransition("blurfade", false, source, %[time: 10]);
                "#,
            )
            .expect("the provider answers the name");
        let frame = engine.update(input(), Duration::ZERO).expect("frame");
        assert!(
            frame.output.transitions.is_empty(),
            "the provider's handler composes, the projection does not"
        );
        assert_eq!(
            log.lock().expect("recording").starts.len(),
            1,
            "the provider's factory ran"
        );
    }

    /// The plugin lifecycle the engine actually runs: `register_plugin` calls
    /// `register` at boot, `Plugins.link` calls it again (the same provider
    /// object, a no-op), and `Plugins.unlink` runs `unregister` — the
    /// reference's `V2Link`/`V2Unlink`.
    #[test]
    fn the_plugin_lifecycle_links_and_unlinks_its_names() {
        struct MockTransitionPlugin {
            log: Arc<Mutex<Recording>>,
            provider: OnceLock<Arc<MockProvider>>,
        }

        impl KrkrPlugin for MockTransitionPlugin {
            fn name(&self) -> &str {
                "MockTransitions.dll"
            }

            fn register(&self, runtime: &mut Runtime<KrkrHost>) -> krkr_tjs2::Result<()> {
                let provider = Arc::clone(self.provider.get_or_init(|| {
                    Arc::new(MockProvider::new("mockfade", Arc::clone(&self.log)))
                }));
                register_transition_provider(runtime, provider)
            }

            fn unregister(&self, runtime: &mut Runtime<KrkrHost>) -> krkr_tjs2::Result<()> {
                unregister_transition_provider(runtime, "mockfade");
                Ok(())
            }
        }

        let mut engine = engine();
        engine
            .register_plugin(MockTransitionPlugin {
                log: Arc::new(Mutex::new(Recording::default())),
                provider: OnceLock::new(),
            })
            .expect("register plugin");
        assert_eq!(
            transition_provider_names(engine.tjs_runtime()),
            vec!["mockfade".to_string()]
        );
        engine
            .execute_script("link.tjs", r#"Plugins.link("MockTransitions.dll");"#)
            .expect("link");
        assert_eq!(
            transition_provider_names(engine.tjs_runtime()),
            vec!["mockfade".to_string()],
            "the second `register` hands back the same provider"
        );

        engine
            .execute_script("link.tjs", r#"Plugins.unlink("MockTransitions.dll");"#)
            .expect("unlink");
        assert!(transition_provider_names(engine.tjs_runtime()).is_empty());
        engine
            .execute_script("inline.tjs", LAYER_PAIR)
            .expect("layers");
        let message = engine
            .execute_script(
                "inline.tjs",
                r#"
                var message = "";
                try { dest.beginTransition("mockfade", true, source, %[time: 10]); }
                catch (e) { message = e.message; }
                return message;
                "#,
            )
            .expect("script");
        assert_eq!(
            message,
            Variant::String("Cannot find transition handler mockfade".to_string())
        );

        engine
            .execute_script("link.tjs", r#"Plugins.link("MockTransitions.dll");"#)
            .expect("re-link");
        assert_eq!(
            transition_provider_names(engine.tjs_runtime()),
            vec!["mockfade".to_string()]
        );
    }
}
