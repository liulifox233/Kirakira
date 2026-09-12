//! `layerExMovie.dll`: `Layer.openMovie` / `startMovie` / `stopMovie` /
//! `isPlayingMovie` — the family's "movie drawn into a layer image".
//!
//! Real plugin: `krkrz/src/plugins/win32/layerExMovie/`, upstream
//! <https://github.com/krkrz/krkrz/tree/last_hodgepodge_repository/src/plugins/win32/layerExMovie>
//! (`docs/plugins/layer-ex-family.md` §2.7). The reference attaches a class
//! with a per-layer native instance to the global `Layer` class
//! (`main.cpp:26-30`): `openMovie(filename, alpha)` copies the storage file to
//! a temporary file and opens it through DirectShow
//! (`layerExMovie.cpp:148-221`), then forces the video surface to 32bpp and
//! pushes the movie size onto the layer's `imageWidth`/`imageHeight`/`type`
//! (`:224-249`). `startMovie(loop)` runs the stream and fires
//! `onStartMovie` (`:229-249`); frames are pulled from the engine's
//! continuous-event callback (`:350-421`) and copied straight into the
//! layer's bitmap (with `alpha=true` the movie is double-width: RGB in the
//! left half, alpha in the right half's byte 0), then `onUpdateMovie` fires —
//! deliberately without `redraw()`, the script calls `Layer.update()` itself
//! (`:390`). `stopMovie()` stops the stream and fires `onStopMovie` when it
//! was playing (`:256-267`); `isPlayingMovie()` reports the latch (`:269-272`).
//!
//! # Why this module is a [`PluginStatus::Shim`]
//!
//! Every link of the reference's chain between "script calls `openMovie`" and
//! "pixels appear in the layer bitmap" is missing on this engine, and none of
//! them can be faked from a plugin module:
//!
//! * **No plugin-facing decoder.** [`krkr_engine::plugin_api`] exposes
//!   `layer` and `storage` only. The engine's own decode path
//!   (`native/video.rs` over `krkr-video`) is reached through the TJS
//!   `VideoOverlay` object; `krkr-video`'s `VideoSource`/`VideoPort` are not
//!   re-exported by `krkr-engine` and this crate's production dependencies
//!   are `krkr-engine` and `krkr-tjs2` by policy, so a plugin cannot open a
//!   movie even if it wanted to decode one frame at a time.
//! * **No per-tick plugin hook.** [`krkr_engine::KrkrPlugin`] has
//!   `name`/`register`/`unregister` only; the reference's whole movie pump is
//!   `OnContinuousCallback` (`layerExMovie.cpp:350-421`), which has no Rust
//!   counterpart. A plugin cannot even ask the engine to call it again.
//! * **No script-thread event posting.** The scheduler's posting entry points
//!   are `pub(crate)`, so `onUpdateMovie`/`onStartMovie`/`onStopMovie` could
//!   not be delivered from a decoder thread either.
//! * **No size to push.** The reference resizes the layer to the movie's
//!   decoded size (`:243-249`); without decoding, the size is unknown, so not
//!   even that observable side effect is honest to reproduce.
//!
//! What is left is the surface and its honest behaviour: the four members
//! exist and behave exactly like the reference on a machine whose DirectShow
//! open failed — `openMovie` checks the storage and returns, `startMovie`
//! does nothing (`layerExMovie.cpp:231` guards on `pSample`), nothing plays,
//! no event fires, `isPlayingMovie()` answers `false`. The difference is that
//! the failure is *diagnosed*: `openMovie` logs which link is missing instead
//! of leaving the script to wonder. A later engine mission that adds a
//! `plugin_api::video` facility (open a storage movie, pull frames) plus a
//! plugin tick or post hook would make the real port possible; the frame copy
//! then follows `layerExMovie.cpp:355-390`, and the Kirikiroid2 port
//! (`Kirikiroid2/src/plugins/layerExMovie.cpp`) is the model for presenting
//! through the engine's own video abstraction instead of DirectShow.

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result,
    runtime::{NativeArgCount, NativeFunction, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "Layer.openMovie/startMovie/stopMovie/isPlayingMovie (movie drawn into a layer image)",
    notes: "Surface only, and honest about it: the engine has no plugin-facing video decoder, no per-tick plugin hook and no script-thread event posting, so the DirectShow-backed pump of layerExMovie.cpp:350-421 cannot be reproduced and no frame ever reaches the layer bitmap. openMovie validates the storage and logs the missing link (the reference logs and returns when its own open fails, layerExMovie.cpp:148-167); startMovie/stopMovie are silent no-ops like the reference's pSample==null paths; isPlayingMovie is false; the reference's imageWidth/imageHeight/type push (layerExMovie.cpp:243-249) is not reproduced because the movie size is unknowable without a decoder. A plugin_api::video facility plus a tick/post hook would enable the real port.",
    install: |engine| engine.register_plugin(LayerExMoviePlugin),
};

pub struct LayerExMoviePlugin;

impl KrkrPlugin for LayerExMoviePlugin {
    fn name(&self) -> &str {
        "layerExMovie.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        let Variant::Object(layer) = runtime.global_member("Layer") else {
            return Ok(());
        };
        // `NCB_ATTACH_CLASS_WITH_HOOK(layerExMovie, Layer)` with
        // `NCB_METHOD(openMovie)`, `startMovie`, `stopMovie`,
        // `isPlayingMovie` (`main.cpp:26-30`). The reference declares
        // `openMovie(const tjs_char*, bool)`, `startMovie(bool)`,
        // `stopMovie()`, `isPlayingMovie()`, so a shorter call is
        // `TJS_E_BADPARAMCOUNT`.
        register_unless_closure(
            runtime,
            layer,
            "openMovie",
            NativeArgCount::AtLeast(2),
            layer_open_movie,
        );
        register_unless_closure(
            runtime,
            layer,
            "startMovie",
            NativeArgCount::AtLeast(1),
            layer_start_movie,
        );
        register_unless_closure(
            runtime,
            layer,
            "stopMovie",
            NativeArgCount::Any,
            layer_stop_movie,
        );
        register_unless_closure(
            runtime,
            layer,
            "isPlayingMovie",
            NativeArgCount::Any,
            layer_is_playing_movie,
        );
        Ok(())
    }
}

/// `layerExMovie::openMovie` (`layerExMovie.cpp:143-252`), reduced to the
/// storage check.
///
/// The reference returns silently after logging `<filename>:ファイルが開けません`
/// when its IStream cannot be opened (`:148-153`); on this engine the open
/// always fails, so the log says why. The `alpha` argument is accepted (the
/// reference reads it into `this->alpha`, `:146`) but reaches no behaviour:
/// the double-width layout it selects is a frame-copy concern
/// (`:364-377`) that cannot happen without frames.
fn layer_open_movie(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let filename = args.first().cloned().unwrap_or_default().to_tjs_string()?;
    let alpha = args.get(1).is_some_and(Variant::is_truthy);
    if runtime.host().storage_exists(&filename) {
        runtime.host_mut().log(&format!(
            "WARN layerExMovie.dll: openMovie({filename:?}, alpha={alpha}) — this engine has no \
             plugin-facing video decode facility and no per-tick plugin hook, so no decoder can \
             feed the layer bitmap; playback stays unavailable (see src/layer_ex_movie.rs)"
        ));
    } else {
        runtime.host_mut().log(&format!(
            "layerExMovie.dll: openMovie: cannot open {filename:?} (the storage does not exist)"
        ));
    }
    // `openMovie` works on the layer's `imageWidth`/`imageHeight`/`type`
    // properties (`:247-249`) only after a successful decode; nothing to set.
    Ok(Variant::Void)
}

/// `layerExMovie::startMovie` (`layerExMovie.cpp:226-249`).
///
/// The reference runs only when `pSample` exists (`:231`), marks the plug-in
/// playing through the continuous-event hook (`start()`, `:274-280`) and fires
/// `onStartMovie`. No decoder exists here, so this is the `pSample == null`
/// path: nothing runs, nothing fires.
fn layer_start_movie(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

/// `layerExMovie::stopMovie` (`layerExMovie.cpp:254-267`).
///
/// The reference removes the continuous-event hook and releases the media
/// objects, then fires `onStopMovie` only when it was playing (`:258-265`).
/// Nothing here can be playing, so only the no-op path remains.
fn layer_stop_movie(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Void)
}

/// `layerExMovie::isPlayingMovie` (`layerExMovie.cpp:269-272`).
///
/// `playing` is set by `start()` (`:276`) and cleared by `stop()` (`:285`),
/// both reachable only from a real open; without one it is `false`, which is
/// exactly what the reference answers after a failed `openMovie`.
fn layer_is_playing_movie(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    Ok(Variant::Integer(0))
}

/// Registers `function` unless a script already owns the member, the way
/// `layer_ex_draw.rs` attaches the rest of the family's surface.
fn register_unless_closure(
    runtime: &mut Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &'static str,
    arg_count: NativeArgCount,
    function: impl NativeFunction<KrkrHost> + 'static,
) {
    if matches!(runtime.object_member(object, name), Variant::Closure(_)) {
        return;
    }
    runtime.register_object_native_with_arg_count(object, name, arg_count, function);
}

#[cfg(test)]
mod tests {
    use krkr_engine::{EngineConfig, KrkrEngine};
    use krkr_tjs2::runtime::{ObjectHandle, Variant};

    use super::LayerExMoviePlugin;

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine.register_plugin(LayerExMoviePlugin).expect("plugin");
        engine
    }

    fn layer_class(engine: &KrkrEngine) -> ObjectHandle {
        engine
            .tjs_runtime()
            .global_member("Layer")
            .object_handle()
            .expect("Layer class")
    }

    /// The four members plus the three script hooks the reference reads off
    /// the layer object (`layerExMovie.cpp:56-60`) exist as callable members.
    #[test]
    fn the_layer_surface_is_registered() {
        let engine = engine();
        let layer = layer_class(&engine);
        for name in ["openMovie", "startMovie", "stopMovie", "isPlayingMovie"] {
            assert!(
                is_callable_member(&engine, layer, name),
                "Layer.{name} is registered"
            );
        }
    }

    /// Registered natives are stored as function objects and script functions
    /// as closures; both are callable members.
    fn is_callable_member(engine: &KrkrEngine, object: ObjectHandle, name: &str) -> bool {
        match engine.tjs_runtime().object_member(object, name) {
            Variant::Closure(_) => true,
            Variant::Object(handle) => engine.tjs_runtime().object_is_callable(handle),
            _ => false,
        }
    }

    /// The reference declares `openMovie(filename, alpha)` and
    /// `startMovie(loop)`; a shorter call is `TJS_E_BADPARAMCOUNT`.
    #[test]
    fn short_argument_lists_are_rejected() {
        let mut engine = engine();
        engine
            .execute_script("inline.tjs", "global.layer = new Layer();")
            .expect("layer");
        let error = engine
            .execute_script("bad.tjs", "layer.openMovie(\"movie.mp4\");")
            .expect_err("one argument");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::BadParamCount);
        let error = engine
            .execute_script("bad.tjs", "layer.startMovie();")
            .expect_err("no arguments");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::BadParamCount);
        // `stopMovie()`/`isPlayingMovie()` take none.
        engine
            .execute_script("ok.tjs", "layer.stopMovie(); layer.isPlayingMovie();")
            .expect("no-argument calls");
    }

    /// A layer with the reference's three script hooks counted from TJS
    /// (`layerExMovie.cpp:56-60`), plus a distinctive bitmap.
    const HOOKED_LAYER: &str = r#"
        global.layer = new Layer();
        layer.setImageSize(3, 1);
        layer.fillRect(0, 0, 3, 1, 0xff00ff00);
        global.started = 0;
        global.updated = 0;
        global.stopped = 0;
        layer.onStartMovie = function() { global.started++; };
        layer.onUpdateMovie = function() { global.updated++; };
        layer.onStopMovie = function() { global.stopped++; };
    "#;

    /// The honest no-op: with the storage missing — and equally with it
    /// present — nothing plays, no event fires and the layer keeps its own
    /// bitmap. The storage check is the one real behaviour `openMovie` has.
    #[test]
    fn open_movie_never_plays_and_diagnoses_the_missing_link() {
        // No project storage at all: the file cannot be opened, the same
        // silent return the reference takes when its own open fails
        // (`layerExMovie.cpp:148-153`), with the reason logged.
        let mut engine = engine();
        engine
            .execute_script("inline.tjs", HOOKED_LAYER)
            .expect("layer");
        engine
            .execute_script("play.tjs", "layer.openMovie(\"missing.mp4\", false);")
            .expect("openMovie on a missing file");
        let logs = engine.host().logs().join("\n");
        assert!(
            logs.contains("cannot open \"missing.mp4\""),
            "the missing file is diagnosed: {logs}"
        );
        engine
            .execute_script("play.tjs", "layer.startMovie(true);")
            .expect("startMovie");
        assert_eq!(playing(&mut engine), 0, "nothing plays");
        engine
            .execute_script("play.tjs", "layer.stopMovie();")
            .expect("stopMovie");
        assert_eq!(playing(&mut engine), 0);
        assert_eq!(hook(&mut engine, "started"), 0, "onStartMovie never fires");
        assert_eq!(hook(&mut engine, "updated"), 0, "onUpdateMovie never fires");
        assert_eq!(hook(&mut engine, "stopped"), 0, "onStopMovie never fires");

        // A readable file is the interesting case: the storage check passes
        // and the log names the engine link that is actually missing.
        let mut engine = engine_with_file();
        engine
            .execute_script("inline.tjs", HOOKED_LAYER)
            .expect("layer");
        engine
            .execute_script("play.tjs", "layer.openMovie(\"movie.mp4\", false);")
            .expect("openMovie");
        assert_eq!(
            playing(&mut engine),
            0,
            "an existing file still cannot play"
        );
        let logs = engine.host().logs().join("\n");
        assert!(
            logs.contains("no plugin-facing video decode facility"),
            "the missing engine link is named: {logs}"
        );
        engine
            .execute_script(
                "play.tjs",
                "layer.startMovie(false); layer.startMovie(true);",
            )
            .expect("startMovie");
        engine
            .execute_script("play.tjs", "layer.stopMovie();")
            .expect("stopMovie");
        assert_eq!(playing(&mut engine), 0);
        assert_eq!(hook(&mut engine, "started"), 0, "onStartMovie never fires");
        assert_eq!(hook(&mut engine, "updated"), 0, "onUpdateMovie never fires");
        assert_eq!(hook(&mut engine, "stopped"), 0, "onStopMovie never fires");

        // The layer bitmap is untouched: the reference only overwrites it
        // from decoded frames. (`getMainPixel` is the `0xRRGGBB` half.)
        assert_eq!(pixel(&mut engine, 0, 0), 0x00ff00);
        assert_eq!(
            integer(&mut engine, "layer.imageWidth"),
            3,
            "the reference's size push (layerExMovie.cpp:243-249) needs the decoded size"
        );
        assert_eq!(integer(&mut engine, "layer.imageHeight"), 1);
    }

    /// A second registration (the engine re-installs a module at `Plugins.link`)
    /// keeps the surface, and a script-owned member is left alone.
    #[test]
    fn registration_is_idempotent_and_respects_script_members() {
        let mut re_registered = engine();
        re_registered
            .register_plugin(LayerExMoviePlugin)
            .expect("second");
        re_registered
            .execute_script("inline.tjs", "global.layer = new Layer();")
            .expect("layer");
        assert_eq!(playing(&mut re_registered), 0);

        let mut patched = engine();
        patched
            .execute_script(
                "inline.tjs",
                "Layer.openMovie = function(filename, alpha) { global.patched = 1; };",
            )
            .expect("patch");
        patched
            .register_plugin(LayerExMoviePlugin)
            .expect("register");
        patched
            .execute_script("patch.tjs", "Layer.openMovie(\"x\", 0);")
            .expect("patched member");
        assert_eq!(
            patched
                .execute_expression("check.tjs", "global.patched")
                .expect("patched")
                .to_integer()
                .expect("integer"),
            1,
            "a script-owned member is not overwritten"
        );
    }

    /// Engine with a readable `movie.mp4` in memory storage, so `openMovie`
    /// takes the "file exists" branch.
    fn engine_with_file() -> KrkrEngine {
        let storage = krkr_assets::ProjectStorage::from_memory([(
            "movie.mp4",
            b"not really a movie".to_vec(),
        )]);
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(std::sync::Arc::new(storage)),
            ..EngineConfig::default()
        })
        .expect("engine");
        engine.register_plugin(LayerExMoviePlugin).expect("plugin");
        engine
    }

    fn playing(engine: &mut KrkrEngine) -> i64 {
        engine
            .execute_expression("read.tjs", "layer.isPlayingMovie()")
            .expect("isPlayingMovie")
            .to_integer()
            .expect("integer")
    }

    fn hook(engine: &mut KrkrEngine, name: &str) -> i64 {
        engine
            .execute_expression("read.tjs", &format!("global.{name}"))
            .expect("hook counter")
            .to_integer()
            .expect("integer")
    }

    fn integer(engine: &mut KrkrEngine, expression: &str) -> i64 {
        engine
            .execute_expression("read.tjs", expression)
            .expect("integer expression")
            .to_integer()
            .expect("integer")
    }

    fn pixel(engine: &mut KrkrEngine, x: i64, y: i64) -> i64 {
        engine
            .execute_expression("read.tjs", &format!("layer.getMainPixel({x}, {y})"))
            .expect("pixel")
            .to_integer()
            .expect("integer")
    }
}
