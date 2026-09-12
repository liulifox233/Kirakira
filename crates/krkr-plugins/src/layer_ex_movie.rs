//! `layerExMovie.dll`: `Layer.openMovie` / `startMovie` / `stopMovie` /
//! `isPlayingMovie` — the family's "movie drawn into a layer image".
//!
//! Real plugin: `krkrz/src/plugins/win32/layerExMovie/`, upstream
//! <https://github.com/krkrz/krkrz/tree/last_hodgepodge_repository/src/plugins/win32/layerExMovie>
//! (`docs/plugins/layer-ex-family.md` §2.7). The reference attaches a class
//! with a per-layer native instance to the global `Layer` class
//! (`main.cpp:26-30`): `openMovie(filename, alpha)` copies the storage file to
//! a temporary file (`:148-167`) and opens it through DirectShow
//! (`:175-287`), then forces the video surface to 32bpp and pushes the movie
//! size onto the layer's `imageWidth`/`imageHeight`/`type` (`:224-249`).
//! `startMovie(loop)` runs the stream and fires `onStartMovie` (`:293-308`,
//! the call at `:301-303`); frames are pulled from the engine's
//! continuous-event callback (`:350-421`) and copied straight into the
//! layer's bitmap (with `alpha=true` the movie is double-width: RGB in the
//! left half, alpha in the right half's byte 0), then `onUpdateMovie` fires —
//! deliberately without `redraw()`, the script calls `Layer.update()` itself
//! (`:390`). `stopMovie()` stops the stream and fires `onStopMovie` when it
//! was playing (`:313-324`, the call at `:319-323`); `isPlayingMovie()`
//! reports the latch (`:326-330`).
//!
//! # What replaces DirectShow
//!
//! [`krkr_engine::plugin_api::video`] replaces the DirectShow graph: the
//! storage is read by `open_movie`, which hands the host's decoder factory the
//! bytes as `VideoSource::Bytes` — the same flow `VideoOverlay` uses, and the
//! counterpart of the reference's `TVPCreateIStream` copy (`:142-167`). The
//! per-frame callback becomes a `System.addContinuousHandler` handler this
//! module allocates with `Runtime::alloc_native_function`: one handler per
//! playing movie, the reference's one hook per instance
//! (`TVPAddContinuousEventHook(this)`, `:332-338`, the call at `:336`).
//! Decoder, latch and the pending frame live in a per-layer slot on the host
//! (`KrkrHost::layer_extension*`), so invalidating the layer drops the movie
//! with it — and the pump retires its own handler when it finds no session.
//!
//! Where the reference's sample update is paced by the DirectShow media clock,
//! the pump here paces against the tick the callback is handed: a frame is
//! copied when its `pts_ms` has passed the timeline `startMovie` anchored, so
//! playback follows the movie's own timeline rather than the engine's frame
//! rate. The end of a stream either rewinds (the loop's `Seek(0)`, `:402`,
//! branch `:400-409`) or stops the movie and fires `onStopMovie` (`:411`), and
//! a decode failure stops it the way the reference's error path does
//! (`:415-417`).
//!
//! # Mapped, with the reason
//!
//! * **The `ltOpaque`/`ltAlpha` push** (`:249`) is not reproduced: a plugin
//!   cannot write a layer's native `type` property (the engine's accessor also
//!   re-derives `neutralColor` and the image's ability to exist, so the raw
//!   member write `Runtime::set_object_member` offers would desync the script
//!   value from the render node). The type a movie needs is the one a layer
//!   starts with: `ltAlpha` blends the copied alpha, and a plain movie's own
//!   frames carry their alpha, which the copy keeps.
//! * **The frame copy is clamped to both bitmaps.** The reference reads
//!   `_width` pixels per row from a surface that is only `movieWidth` wide
//!   (`:370` sets `w = movieWidth * 4`, the x loop at `:375` runs to `_width`)
//!   and can therefore run past its own surface once the script has enlarged
//!   the layer; here the copy stops at the smaller of the layer's bitmap and
//!   the decoded frame.
//! * **The layer is repainted after each frame** (`layer_update`), where the
//!   reference leaves `redraw()` commented out (`:390`) and expects the script
//!   to call `update()` from `onUpdateMovie` (`manual.tjs` §注意点). That is
//!   the family's contract in this crate (`layerExBTOA`, `layerExRaster` and
//!   friends all mutate then update), and a script that also updates is
//!   idempotent.
//! * **A hook that throws ends the playback.** The engine retires a continuous
//!   callback whose call returned an error and reports the exception itself,
//!   where the reference ignores its `FuncCall` result and keeps the movie
//!   running. `stopMovie()` still clears the latch and fires `onStopMovie`
//!   from there, so the layer does not stay stuck with a script that cares.
//! * **Decoding runs on the script thread**, one frame per callback, which is
//!   where the reference's synchronous `pSample->Update` runs too. A port that
//!   wanted decode-ahead can keep the decoder on its own thread — a `VideoPort`
//!   is `Send` — but the layer can only be touched from the callback the pump
//!   is already on.
//!
//! Reference line numbers refer to the krkrz checkout at
//! `/Users/ruri/repo/krkrz` (`last_hodgepodge_repository`, Shift-JIS sources
//! converted with `iconv -f CP932`), re-read one by one on 2026-09-12:
//! `layerExMovie.cpp` is 421 lines, `main.cpp` 61.

use std::sync::{Arc, Mutex, MutexGuard};

use krkr_engine::{
    KrkrHost, KrkrPlugin,
    plugin_api::{
        layer::{LayerBitmapViewMut, layer_bitmap_write, layer_update},
        video::{VideoFrame, VideoPort, open_movie},
    },
};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeArgCount, NativeFunction, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Implemented,
    feature: "Layer.openMovie/startMovie/stopMovie/isPlayingMovie (movie drawn into a layer image)",
    notes: "A port of layerExMovie (layerExMovie.cpp openMovie :127-288, startMovie :293-308, stopMovie :313-324, isPlayingMovie :326-330, start/stop :332-348, OnContinuousCallback :350-421; main.cpp:26-30) over plugin_api::video and the engine's own per-frame callback: openMovie reads the storage through the host's decoder factory (VideoSource::Bytes, what VideoOverlay does) and pushes the decoded size through Layer.setImageSize (:247-248); startMovie registers a System.addContinuousHandler pump and fires onStartMovie (:301-303); each tick copies the next due frame into the layer bitmap (alpha movies: RGB from the left half, alpha from the right half's byte 0, :369-381, the copy at :376-379) and fires onUpdateMovie (:391-393); stopMovie removes the pump, drops the decoder and fires onStopMovie only when it was playing (:319-323) — a real latch, so a second stop is a no-op and a stopped movie needs a new openMovie. The end of a stream rewinds a looping movie (Seek(0), :402) and stops a non-looping one (:411); a decode failure stops it too (:416). Per-layer sessions live in a KrkrHost layer extension, so invalidating the layer drops its decoder. Not reproduced: the ltOpaque/ltAlpha push (:249), because a plugin cannot write a layer's native type property; and the frame copy is clamped to both bitmaps where the reference's alpha branch reads past its own surface once the layer was resized. The layer is repainted after each frame (layer_update) where the reference leaves redraw() commented out (:390).",
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

/// One layer's open movie — the reference's per-layer native instance
/// (`layerExMovie`): the decoder, the flags `openMovie`/`startMovie` set, and
/// the timeline the pump paces frames against.
struct MovieSession {
    decoder: Box<dyn VideoPort>,
    /// `openMovie`'s second argument: the frame is double-width and carries
    /// its alpha in the right half (`:369-381`).
    alpha: bool,
    /// `startMovie`'s argument: rewind at the end of the stream (`:400-409`).
    looping: bool,
    /// The latch `isPlayingMovie` reports (`:326-330`).
    playing: bool,
    /// The tick the current timeline started at — the first pump tick after
    /// `startMovie`, or the tick a loop rewound on. Anchoring on a pump tick
    /// rather than on a clock sample taken between frames keeps playback
    /// independent of when the script called `startMovie`.
    timeline_start_ms: Option<i64>,
    /// The next frame, decoded ahead of its presentation time.
    pending: Option<VideoFrame>,
    /// The last tick the pump saw. A callback always carries the tick the
    /// engine is running at; reusing the previous one means a missing tick
    /// advances no time instead of rewinding the timeline.
    last_tick: i64,
    /// The handler `startMovie` registered, so `stopMovie` can remove it
    /// (`TVPAddContinuousEventHook`/`TVPRemoveContinuousEventHook`, `:336` and
    /// `:346`).
    handler: Option<Variant>,
}

/// The per-layer slot. `KrkrHost::layer_extension*` prunes it when the layer
/// is invalidated, which is what drops the decoder with the layer; the slot is
/// keyed by layer object *and* type, so no other plugin's state collides.
type MovieSlot = Mutex<MovieSession>;

/// The pump's own handler value, kept where the pump can reach it: a layer
/// that goes away leaves a session-less handler behind, and that handler has
/// to be able to retire itself.
type HandlerCell = Arc<Mutex<Option<Variant>>>;

/// What one continuous tick decided, computed with the session lock held and
/// acted on after it is released — the hooks a tick fires may call back into
/// `stopMovie`, which needs the same lock.
enum TickAction {
    /// Nothing to do: the next frame is not due yet.
    Idle,
    /// Copy this frame into the layer and fire `onUpdateMovie`.
    Frame(VideoFrame),
    /// End of stream (`None`) or a decode failure (`Some`, the reason): stop
    /// the movie the way the reference's callback does (`:411` for the end of
    /// a non-looping stream, `:416` for the error path).
    Stop(Option<String>),
}

impl MovieSession {
    fn new(decoder: Box<dyn VideoPort>, alpha: bool) -> Self {
        Self {
            decoder,
            alpha,
            looping: false,
            playing: false,
            timeline_start_ms: None,
            pending: None,
            last_tick: 0,
            handler: None,
        }
    }

    /// `OnContinuousCallback` (`:350-421`) for one tick.
    ///
    /// A frame is held back until its `pts_ms` has passed the timeline, so a
    /// movie plays on its own clock; one frame is copied per tick, which means
    /// a tick that arrives late plays its backlog at the tick rate rather than
    /// skipping it. Nothing is pulled at all once the session stopped playing.
    fn tick(&mut self, tick: i64) -> TickAction {
        if !self.playing {
            return TickAction::Idle;
        }
        self.timeline_start_ms.get_or_insert(tick);
        let mut rewound = false;
        loop {
            if self.pending.is_none() {
                match self.decoder.next_frame() {
                    Ok(Some(frame)) => self.pending = Some(frame),
                    Ok(None) if self.looping && !rewound => {
                        // `MS_S_ENDOFSTREAM` with `loop`: `pAMStream->Seek(0)`
                        // (`:402`, in the loop branch `:400-409`). A backend
                        // that cannot rewind ends the movie the way a decode
                        // failure does.
                        rewound = true;
                        if let Err(error) = self.decoder.seek_ms(0) {
                            return TickAction::Stop(Some(format!(
                                "movie loop restart failed: {error}"
                            )));
                        }
                        self.timeline_start_ms = Some(tick);
                        continue;
                    }
                    Ok(None) => return TickAction::Stop(None),
                    Err(error) => {
                        return TickAction::Stop(Some(format!("movie decode failed: {error}")));
                    }
                }
            }
            let elapsed = tick - self.timeline_start_ms.unwrap_or(tick);
            let due = self
                .pending
                .as_ref()
                .is_some_and(|frame| frame.pts_ms <= elapsed);
            if due && let Some(frame) = self.pending.take() {
                return TickAction::Frame(frame);
            }
            return TickAction::Idle;
        }
    }
}

/// `layerExMovie::openMovie` (`layerExMovie.cpp:127-288`).
///
/// The reference's temporary-file copy and DirectShow open become
/// [`open_movie`]; what it does to the layer is the same: on a failure it logs
/// `<filename>:ファイルが開けません` and returns with the layer untouched
/// (`:142-147`, the log at `:145`), on success it sizes the layer's image to
/// the movie (`:247-248`) and leaves playback to `startMovie`.
fn layer_open_movie(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let filename = args.first().cloned().unwrap_or_default().to_tjs_string()?;
    let alpha = args.get(1).is_some_and(Variant::is_truthy);
    // `clearMovie()` runs first (`:129`): an open replaces whatever was open,
    // and a playing movie stops here — the reference's `clearMovie` fires no
    // event, only `stopMovie` does.
    stop_session(runtime, layer)?;
    let decoder = match open_movie(runtime, &filename) {
        Ok(decoder) => decoder,
        Err(error) => {
            runtime.host_mut().log(&format!(
                "layerExMovie.dll: openMovie: cannot open {filename:?}: {error}"
            ));
            return Ok(Variant::Void);
        }
    };
    let metadata = decoder.metadata().clone();
    // `_pWidth`/`_pHeight` (`:247-248`): the layer's image takes the movie's
    // size, half the frame's width in the double-width alpha form (`:243-245`).
    let width = i64::from(if alpha {
        metadata.width / 2
    } else {
        metadata.width
    });
    let height = i64::from(metadata.height);
    runtime.call_object_method(
        layer,
        "setImageSize",
        vec![Variant::Integer(width), Variant::Integer(height)],
    )?;
    runtime.host_mut().log(&format!(
        "layerExMovie.dll: openMovie: {filename:?} ({width}x{height}, alpha={alpha})"
    ));
    runtime
        .host_mut()
        .layer_extension_or_insert_with(layer, || Mutex::new(MovieSession::new(decoder, alpha)));
    Ok(Variant::Void)
}

/// `layerExMovie::startMovie` (`layerExMovie.cpp:293-308`).
///
/// `if (pSample)` (`:296`): with no movie open there is nothing to start, and
/// the reference says nothing — the path a failed `openMovie` or a
/// `stopMovie` that released the stream leaves behind.
fn layer_start_movie(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let looping = args.first().is_some_and(Variant::is_truthy);
    let Some(slot) = runtime.host().layer_extension::<MovieSlot>(layer) else {
        return Ok(Variant::Void);
    };
    // `start()` (`:332-338`) is `stop()` then
    // `TVPAddContinuousEventHook(this)` (`:336`); the hook object is the same
    // either way, so a movie that is already playing only re-arms its flags —
    // and fires `onStartMovie` again, as the reference's unconditional call at
    // `:301-303` does.
    let pumping = lock(&slot).handler.is_some();
    let handler = if pumping {
        None
    } else {
        Some(start_pump(runtime, layer)?)
    };
    {
        let mut session = lock(&slot);
        session.looping = looping;
        session.playing = true;
        if let Some(handler) = handler {
            session.handler = Some(handler);
        }
    }
    fire_hook(runtime, layer, "onStartMovie", Vec::new())?;
    Ok(Variant::Void)
}

/// `layerExMovie::stopMovie` (`layerExMovie.cpp:313-324`).
fn layer_stop_movie(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    // `bool p = playing; stop(); clearMovie();` (`:316-318`): the hook and the
    // stream go either way, the event only when it was playing (`:319-323`).
    if stop_session(runtime, layer)? {
        fire_hook(runtime, layer, "onStopMovie", Vec::new())?;
    }
    Ok(Variant::Void)
}

/// `layerExMovie::isPlayingMovie` (`layerExMovie.cpp:326-330`).
fn layer_is_playing_movie(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let layer = this_layer(this_obj)?;
    let playing = runtime
        .host()
        .layer_extension::<MovieSlot>(layer)
        .is_some_and(|slot| lock(&slot).playing);
    Ok(Variant::Integer(i64::from(playing)))
}

/// One tick of the pump — `OnContinuousCallback` (`layerExMovie.cpp:350-421`).
fn pump_movie(
    runtime: &mut Runtime<KrkrHost>,
    layer: ObjectHandle,
    cell: &HandlerCell,
    tick: Option<i64>,
) -> Result<Variant> {
    let Some(slot) = runtime.host().layer_extension::<MovieSlot>(layer) else {
        // The layer is gone (`invalidate` prunes its slots, decoder and all):
        // retire the handler, or a dead layer would cost a callback per frame
        // for the rest of the session.
        if let Some(handler) = lock(cell).take() {
            remove_continuous_handler(runtime, &handler)?;
        }
        return Ok(Variant::Void);
    };
    // The action is decided under the lock and carried out after it is
    // released: a hook it fires may call back into `stopMovie`.
    let (action, alpha) = {
        let mut session = lock(&slot);
        let tick = tick.unwrap_or(session.last_tick);
        session.last_tick = tick;
        (session.tick(tick), session.alpha)
    };
    match action {
        TickAction::Idle => {}
        TickAction::Frame(frame) => {
            // `if (_buffer != NULL)` (`:365`, the guarded block `:365-394`): a
            // layer without a bitmap gets neither the copy nor the event.
            if copy_frame_into_layer(runtime, layer, &frame, alpha)? {
                fire_hook(runtime, layer, "onUpdateMovie", Vec::new())?;
            }
        }
        TickAction::Stop(reason) => {
            if let Some(reason) = reason {
                runtime
                    .host_mut()
                    .log(&format!("layerExMovie.dll: {reason}"));
            }
            // The reference's own `stopMovie()` from the callback (`:411` for
            // the end of a non-looping stream, `:416` for the error path),
            // which is where a finished or broken stream fires its event.
            if stop_session(runtime, layer)? {
                fire_hook(runtime, layer, "onStopMovie", Vec::new())?;
            }
        }
    }
    Ok(Variant::Void)
}

/// Registers the per-frame pump — `start()`'s `TVPAddContinuousEventHook(this)`
/// (`:332-338`, the call at `:336`). The handler is the object
/// `System.removeContinuousHandler` later has to match by identity, so the pump
/// keeps a handle on itself.
fn start_pump(runtime: &mut Runtime<KrkrHost>, layer: ObjectHandle) -> Result<Variant> {
    let self_cell: HandlerCell = Arc::new(Mutex::new(None));
    let cell = Arc::clone(&self_cell);
    let handle = runtime.alloc_native_function(
        move |runtime: &mut Runtime<KrkrHost>,
              _this_obj: Option<ObjectHandle>,
              args: Vec<Variant>| {
            // `OnContinuousCallback(tick)`: the engine hands the callback the
            // clock sample it is running at (the continuous dispatch in
            // `engine.rs`).
            let tick = args.first().and_then(|value| value.to_integer().ok());
            pump_movie(runtime, layer, &cell, tick)
        },
    );
    let handler = Variant::Object(handle);
    *lock(&self_cell) = Some(handler.clone());
    add_continuous_handler(runtime, &handler)?;
    Ok(handler)
}

/// `stop()` + `clearMovie()` (`:343-348` and `:74-119`): remove the continuous
/// handler, drop the decoder, forget the session.
///
/// Returns the `playing` latch, which is what decides whether `onStopMovie`
/// fires (`bool p = playing;`, `:316`) — and `false` when no movie was open,
/// where the reference's `stop()`/`clearMovie()` are no-ops too.
fn stop_session(runtime: &mut Runtime<KrkrHost>, layer: ObjectHandle) -> Result<bool> {
    let Some(slot) = runtime
        .host_mut()
        .remove_layer_extension::<MovieSlot>(layer)
    else {
        return Ok(false);
    };
    let (playing, handler) = {
        let mut session = lock(&slot);
        (session.playing, session.handler.take())
    };
    if let Some(handler) = handler {
        remove_continuous_handler(runtime, &handler)?;
    }
    Ok(playing)
}

/// `TVPAddContinuousEventHook(this)` (`:336`): the plugin registers the same
/// per-frame callback a script gets from `System.addContinuousHandler`.
fn add_continuous_handler(runtime: &mut Runtime<KrkrHost>, handler: &Variant) -> Result<()> {
    let Variant::Object(system) = runtime.global_member("System") else {
        return Err(TjsError::runtime(
            "layerExMovie.dll: this engine has no System class, so a movie cannot be pumped",
        ));
    };
    runtime.call_object_method(system, "addContinuousHandler", vec![handler.clone()])?;
    Ok(())
}

/// `TVPRemoveContinuousEventHook(this)` (`:346`): the scheduler matches the
/// handler by identity, and removing one that is no longer registered is the
/// no-op the reference's removal is.
fn remove_continuous_handler(runtime: &mut Runtime<KrkrHost>, handler: &Variant) -> Result<()> {
    let Variant::Object(system) = runtime.global_member("System") else {
        return Ok(());
    };
    runtime.call_object_method(system, "removeContinuousHandler", vec![handler.clone()])?;
    Ok(())
}

/// Copies one decoded frame into the layer's bitmap and repaints, reporting
/// whether a frame landed.
///
/// A layer whose image the script freed is the reference's `_buffer == NULL`
/// (`:365`): the frame is skipped, and no event fires.
fn copy_frame_into_layer(
    runtime: &mut Runtime<KrkrHost>,
    layer: ObjectHandle,
    frame: &VideoFrame,
    alpha: bool,
) -> Result<bool> {
    if layer_bitmap_write(runtime, layer, |view| copy_frame(view, frame, alpha)).is_err() {
        return Ok(false);
    }
    layer_update(runtime, layer)?;
    Ok(true)
}

/// `layerExMovie`'s frame copy (`:355-390`) over the engine's RGBA plane.
///
/// The alpha form takes RGB from the left half and the alpha from the right
/// half's byte 0 — the reference's three `*dst++ = *src1++` followed by
/// `*dst++ = *src2` (`:376-379`, in the alpha branch `:369-381`), read against
/// the frame this engine decodes, whose byte order is R, G, B, A. Both forms
/// stop at the smaller of the two bitmaps, where the reference's alpha branch
/// would read past its own surface (`:370` sets `w = movieWidth * 4`, the x
/// loop at `:375` runs to `_width`).
fn copy_frame(view: &mut LayerBitmapViewMut<'_>, frame: &VideoFrame, alpha: bool) {
    let pitch = view.bitmap.pitch as usize;
    let stride = frame.stride as usize;
    let rows = (view.bitmap.height as usize).min(frame.height as usize);
    if alpha {
        let half = frame.width as usize / 2;
        let columns = (view.bitmap.width as usize).min(half);
        for y in 0..rows {
            for x in 0..columns {
                let left = y * stride + x * 4;
                let (Some(rgb), Some(alpha)) = (
                    frame.data.get(left..left + 3),
                    frame.data.get(left + half * 4).copied(),
                ) else {
                    continue;
                };
                let dest = y * pitch + x * 4;
                let Some(pixel) = view.pixels.get_mut(dest..dest + 4) else {
                    continue;
                };
                pixel[..3].copy_from_slice(rgb);
                pixel[3] = alpha;
            }
        }
        return;
    }
    let columns = (view.bitmap.width as usize).min(frame.width as usize);
    for y in 0..rows {
        let (Some(source), Some(dest)) = (
            frame.data.get(y * stride..y * stride + columns * 4),
            view.pixels.get_mut(y * pitch..y * pitch + columns * 4),
        ) else {
            continue;
        };
        dest.copy_from_slice(source);
    }
}

/// Fires one of the layer's three movie events the way the reference's
/// `FuncCall(0, NULL, NULL, NULL, 0, NULL, _obj)` does (`:301-303` for
/// `onStartMovie`, `:391-393` for `onUpdateMovie`, `:319-323` for
/// `onStopMovie`): the layer is `this`, a member that is absent or not callable
/// is ignored, and an exception inside the handler propagates.
fn fire_hook(
    runtime: &mut Runtime<KrkrHost>,
    layer: ObjectHandle,
    name: &str,
    args: Vec<Variant>,
) -> Result<()> {
    let member = runtime.resolve_object_member(layer, name)?;
    let callable = match &member {
        Variant::Closure(_) => true,
        Variant::Object(handle) => runtime.object_is_callable(*handle),
        _ => false,
    };
    if !callable {
        return Ok(());
    }
    runtime.call_object_method(layer, name, args)?;
    Ok(())
}

fn this_layer(this_obj: Option<ObjectHandle>) -> Result<ObjectHandle> {
    this_obj.ok_or_else(|| TjsError::runtime("Layer method requires this"))
}

/// A poisoned lock means a hook panicked while holding it; the movie state is
/// still consistent, so it is taken back rather than panicking the frame again.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    use krkr_assets::ProjectStorage;
    use krkr_core::{FrameInput, Size};
    use krkr_engine::{
        EngineConfig, EngineInput, KrkrEngine,
        plugin_api::video::{
            VideoCapabilities, VideoDecoder, VideoDecoderFactory, VideoError, VideoFrame,
            VideoMetadata, VideoPort, VideoSource,
        },
    };
    use krkr_tjs2::runtime::{ObjectHandle, Variant};

    use super::LayerExMoviePlugin;

    /// A decoder over canned frames, in stream order: no platform codec, no
    /// clock, and `seek_ms(0)` back to the first frame, the way a looping
    /// movie restarts.
    struct FakeDecoder {
        metadata: VideoMetadata,
        frames: Vec<VideoFrame>,
        next: usize,
    }

    impl VideoDecoder for FakeDecoder {
        fn metadata(&self) -> &VideoMetadata {
            &self.metadata
        }

        fn next_frame(&mut self) -> std::result::Result<Option<VideoFrame>, VideoError> {
            let frame = self.frames.get(self.next).cloned();
            self.next += usize::from(frame.is_some());
            Ok(frame)
        }

        fn seek_ms(&mut self, ms: i64) -> std::result::Result<(), VideoError> {
            assert_eq!(ms, 0, "a loop rewinds to the start (`:402`)");
            self.next = 0;
            Ok(())
        }
    }

    impl VideoPort for FakeDecoder {}

    /// The host's decoder capability, scripted: hands one canned movie to each
    /// open, records the names it was asked for, and can fail the way a
    /// backend with no codec for the bytes does.
    struct FakeVideoFactory {
        movies: Mutex<Vec<Vec<VideoFrame>>>,
        fail: Option<String>,
        opened: Mutex<Vec<String>>,
    }

    impl FakeVideoFactory {
        /// A factory serving one movie.
        fn new(frames: Vec<VideoFrame>) -> Self {
            Self::queue(vec![frames])
        }

        /// A factory serving each of `movies` to one open, in order.
        fn queue(movies: Vec<Vec<VideoFrame>>) -> Self {
            Self {
                movies: Mutex::new(movies),
                fail: None,
                opened: Mutex::new(Vec::new()),
            }
        }

        /// A factory whose backend refuses every movie.
        fn failing(message: &str) -> Self {
            Self {
                movies: Mutex::new(Vec::new()),
                fail: Some(message.to_string()),
                opened: Mutex::new(Vec::new()),
            }
        }
    }

    impl VideoDecoderFactory for FakeVideoFactory {
        fn capabilities(&self) -> VideoCapabilities {
            VideoCapabilities::unavailable()
        }

        fn create(
            &self,
            source: VideoSource,
        ) -> std::result::Result<Box<dyn VideoPort>, VideoError> {
            let name = match source {
                VideoSource::Bytes { name, .. } => name,
                VideoSource::Path(path) => Some(path.display().to_string()),
            };
            self.opened
                .lock()
                .expect("lock")
                .push(name.unwrap_or_default());
            if let Some(message) = &self.fail {
                return Err(VideoError::Open(message.clone()));
            }
            let frames = if self.movies.lock().expect("lock").is_empty() {
                Vec::new()
            } else {
                self.movies.lock().expect("lock").remove(0)
            };
            Ok(Box::new(FakeDecoder {
                metadata: metadata_for(&frames),
                frames,
                next: 0,
            }))
        }
    }

    fn metadata_for(frames: &[VideoFrame]) -> VideoMetadata {
        VideoMetadata {
            width: frames.first().map_or(0, |frame| frame.width),
            height: frames.first().map_or(0, |frame| frame.height),
            fps: 30.0,
            frame_count: frames.len() as i64,
            duration_ms: frames.last().map_or(0, |frame| frame.pts_ms),
            has_audio: false,
        }
    }

    /// One packed RGBA frame at `pts_ms`.
    fn frame(width: u32, height: u32, pts_ms: i64, rgba: &[u8]) -> VideoFrame {
        assert_eq!(rgba.len(), (width * height * 4) as usize);
        VideoFrame {
            pts_ms,
            width,
            height,
            stride: width * 4,
            data: rgba.to_vec(),
        }
    }

    /// An engine over an in-memory project holding `movie.mp4`, decoding
    /// through `factory`.
    fn engine(factory: Arc<FakeVideoFactory>) -> KrkrEngine {
        let storage = ProjectStorage::from_memory([("movie.mp4", b"not really a movie".to_vec())]);
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            video_factory: factory,
            ..EngineConfig::default()
        })
        .expect("engine");
        engine.register_plugin(LayerExMoviePlugin).expect("plugin");
        engine
    }

    /// The layer the reference reads its three hooks off (`layerExMovie.cpp:55-60`),
    /// counting the calls from TJS and recording that `this` was the layer.
    const HOOKED_LAYER: &str = r#"
        global.hooked = new Layer();
        global.started = 0;
        global.updated = 0;
        global.stopped = 0;
        global.this_was_layer = 0;
        hooked.onStartMovie = function() { global.started++; global.this_was_layer += (this === hooked); };
        hooked.onUpdateMovie = function() { global.updated++; global.this_was_layer += (this === hooked); };
        hooked.onStopMovie = function() { global.stopped++; global.this_was_layer += (this === hooked); };
    "#;

    fn run(engine: &mut KrkrEngine, script: &str) {
        engine.execute_script("inline.tjs", script).expect("script");
    }

    fn integer(engine: &mut KrkrEngine, expression: &str) -> i64 {
        engine
            .execute_expression("read.tjs", expression)
            .expect("expression")
            .to_integer()
            .expect("integer")
    }

    /// One engine frame whose host clock sample is `now_millis` — the tick the
    /// continuous callback is handed.
    fn tick(engine: &mut KrkrEngine, now_millis: i64) {
        engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new())
                    .with_now_millis(now_millis),
                Duration::ZERO,
            )
            .expect("frame");
    }

    /// One pixel of `layer` as `(r, g, b, a)`, read through the engine's own
    /// accessors.
    fn pixel(engine: &mut KrkrEngine, layer: &str, x: i64, y: i64) -> (i64, i64, i64, i64) {
        let packed = integer(engine, &format!("{layer}.getMainPixel({x}, {y})"));
        let alpha = integer(engine, &format!("{layer}.getMaskPixel({x}, {y})"));
        (
            (packed >> 16) & 0xff,
            (packed >> 8) & 0xff,
            packed & 0xff,
            alpha,
        )
    }

    /// The registered per-frame callbacks, from the engine's scheduler
    /// diagnostics.
    fn handlers(engine: &KrkrEngine) -> usize {
        engine.scheduler_diagnostics().0
    }

    fn logs(engine: &KrkrEngine) -> String {
        engine.host().logs().join("\n")
    }

    /// The whole contract in one run: open, size push, start, one frame per
    /// due tick, stop — and the latch behaviour around it.
    #[test]
    fn frames_reach_the_layer_and_the_three_hooks_fire_with_the_layer_as_this() {
        let first = frame(2, 1, 0, &[1, 2, 3, 4, 5, 6, 7, 8]);
        let second = frame(2, 1, 1000, &[9, 10, 11, 12, 13, 14, 15, 16]);
        let factory = Arc::new(FakeVideoFactory::new(vec![first, second]));
        let mut engine = engine(Arc::clone(&factory));
        run(&mut engine, HOOKED_LAYER);
        run(&mut engine, r#"hooked.openMovie("movie.mp4", false);"#);

        // `openMovie` pushes the decoded size (`:247-248`) and reads the
        // storage through the host's factory; nothing plays yet.
        assert_eq!(integer(&mut engine, "hooked.imageWidth"), 2);
        assert_eq!(integer(&mut engine, "hooked.imageHeight"), 1);
        assert_eq!(integer(&mut engine, "hooked.isPlayingMovie()"), 0);
        assert_eq!(integer(&mut engine, "global.started"), 0);
        assert_eq!(
            factory.opened.lock().expect("lock").as_slice(),
            ["movie.mp4"],
            "the movie was opened through the host's decoder factory"
        );

        run(&mut engine, "hooked.startMovie(true);");
        assert_eq!(integer(&mut engine, "global.started"), 1);
        assert_eq!(integer(&mut engine, "hooked.isPlayingMovie()"), 1);
        assert_eq!(handlers(&engine), 1, "one per-frame callback is registered");
        assert_eq!(
            integer(&mut engine, "global.updated"),
            0,
            "no frame until a tick"
        );

        // The frame at pts 0 is due on the first tick, and only that one.
        tick(&mut engine, 0);
        assert_eq!(integer(&mut engine, "global.updated"), 1);
        assert_eq!(pixel(&mut engine, "hooked", 0, 0), (1, 2, 3, 4));
        assert_eq!(pixel(&mut engine, "hooked", 1, 0), (5, 6, 7, 8));

        // The second frame waits for its own presentation time.
        tick(&mut engine, 999);
        assert_eq!(
            integer(&mut engine, "global.updated"),
            1,
            "pts 1000 is not due at 999 ms"
        );
        tick(&mut engine, 1000);
        assert_eq!(integer(&mut engine, "global.updated"), 2);
        assert_eq!(pixel(&mut engine, "hooked", 0, 0), (9, 10, 11, 12));

        // `stopMovie` is a real latch: the callback goes with the decoder.
        run(&mut engine, "hooked.stopMovie();");
        assert_eq!(integer(&mut engine, "global.stopped"), 1);
        assert_eq!(integer(&mut engine, "hooked.isPlayingMovie()"), 0);
        assert_eq!(handlers(&engine), 0);
        tick(&mut engine, 2000);
        tick(&mut engine, 3000);
        assert_eq!(
            integer(&mut engine, "global.updated"),
            2,
            "no frame reaches the layer after stop"
        );
        assert_eq!(
            pixel(&mut engine, "hooked", 0, 0),
            (9, 10, 11, 12),
            "the last frame stays"
        );

        // A second stop is the reference's no-op, and the movie needs a new
        // `openMovie` before it can play again — `clearMovie` released it.
        run(&mut engine, "hooked.stopMovie();");
        assert_eq!(integer(&mut engine, "global.stopped"), 1);
        run(&mut engine, "hooked.startMovie(true);");
        assert_eq!(integer(&mut engine, "global.started"), 1);
        assert_eq!(handlers(&engine), 0);

        // Every hook ran with the layer as `this`.
        assert_eq!(
            integer(&mut engine, "global.this_was_layer"),
            integer(&mut engine, "global.started")
                + integer(&mut engine, "global.updated")
                + integer(&mut engine, "global.stopped"),
        );
    }

    /// `alpha=true` is the double-width form (`:369-381`): RGB from the left
    /// half, the alpha from the right half's byte 0.
    #[test]
    fn an_alpha_movie_takes_its_alpha_from_the_right_halfs_byte_zero() {
        let double = frame(
            4,
            1,
            0,
            &[10, 20, 30, 0, 40, 50, 60, 0, 0x80, 0, 0, 0, 0x40, 0, 0, 0],
        );
        let factory = Arc::new(FakeVideoFactory::new(vec![double]));
        let mut engine = engine(factory);
        run(&mut engine, HOOKED_LAYER);
        run(&mut engine, r#"hooked.openMovie("movie.mp4", true);"#);
        assert_eq!(
            integer(&mut engine, "hooked.imageWidth"),
            2,
            "the layer is the drawn half of a double-width frame (`:243-245`)"
        );
        assert_eq!(integer(&mut engine, "hooked.imageHeight"), 1);

        run(&mut engine, "hooked.startMovie(false);");
        tick(&mut engine, 0);
        assert_eq!(pixel(&mut engine, "hooked", 0, 0), (10, 20, 30, 0x80));
        assert_eq!(pixel(&mut engine, "hooked", 1, 0), (40, 50, 60, 0x40));
        assert_eq!(integer(&mut engine, "global.updated"), 1);
    }

    /// The end of a non-looping stream stops the movie and fires
    /// `onStopMovie` (`:411`).
    #[test]
    fn the_end_of_a_non_looping_stream_stops_the_movie() {
        let factory = Arc::new(FakeVideoFactory::new(vec![frame(1, 1, 0, &[1, 2, 3, 255])]));
        let mut engine = engine(factory);
        run(&mut engine, HOOKED_LAYER);
        run(
            &mut engine,
            r#"hooked.openMovie("movie.mp4", false); hooked.startMovie(false);"#,
        );
        tick(&mut engine, 0);
        assert_eq!(integer(&mut engine, "global.updated"), 1);
        assert_eq!(integer(&mut engine, "hooked.isPlayingMovie()"), 1);

        tick(&mut engine, 100);
        assert_eq!(integer(&mut engine, "global.stopped"), 1);
        assert_eq!(integer(&mut engine, "hooked.isPlayingMovie()"), 0);
        assert_eq!(handlers(&engine), 0, "the callback retired itself");
        tick(&mut engine, 200);
        assert_eq!(integer(&mut engine, "global.updated"), 1);
        assert_eq!(integer(&mut engine, "global.stopped"), 1, "only once");
    }

    /// A looping movie rewinds at the end of the stream (`Seek(0)`, `:402`)
    /// and keeps playing.
    #[test]
    fn a_looping_movie_rewinds_at_the_end_of_the_stream() {
        let factory = Arc::new(FakeVideoFactory::new(vec![
            frame(1, 1, 0, &[1, 2, 3, 255]),
            frame(1, 1, 100, &[4, 5, 6, 255]),
        ]));
        let mut engine = engine(factory);
        run(&mut engine, HOOKED_LAYER);
        run(
            &mut engine,
            r#"hooked.openMovie("movie.mp4", false); hooked.startMovie(true);"#,
        );
        tick(&mut engine, 0);
        tick(&mut engine, 100);
        tick(&mut engine, 200);
        tick(&mut engine, 300);
        assert_eq!(integer(&mut engine, "global.updated"), 4);
        assert_eq!(
            integer(&mut engine, "global.stopped"),
            0,
            "a loop does not stop"
        );
        assert_eq!(integer(&mut engine, "hooked.isPlayingMovie()"), 1);
        assert_eq!(
            pixel(&mut engine, "hooked", 0, 0),
            (4, 5, 6, 255),
            "the second pass reached its second frame"
        );
        assert_eq!(handlers(&engine), 1);
    }

    /// Sessions are per layer: two movies play side by side, each with its own
    /// per-frame callback, and one ending leaves the other playing.
    #[test]
    fn two_layers_play_independently() {
        let factory = Arc::new(FakeVideoFactory::queue(vec![
            vec![
                frame(1, 1, 0, &[1, 2, 3, 255]),
                frame(1, 1, 100, &[4, 5, 6, 255]),
            ],
            vec![frame(1, 1, 0, &[7, 8, 9, 255])],
        ]));
        let mut engine = engine(factory);
        run(&mut engine, HOOKED_LAYER);
        run(
            &mut engine,
            r#"
            global.other = new Layer();
            global.other_updated = 0;
            other.onUpdateMovie = function() { global.other_updated++; };
            "#,
        );
        run(
            &mut engine,
            r#"hooked.openMovie("movie.mp4", false); hooked.startMovie(true);"#,
        );
        run(
            &mut engine,
            r#"other.openMovie("movie.mp4", false); other.startMovie(false);"#,
        );
        assert_eq!(handlers(&engine), 2, "one callback per playing movie");

        tick(&mut engine, 0);
        assert_eq!(pixel(&mut engine, "hooked", 0, 0), (1, 2, 3, 255));
        assert_eq!(integer(&mut engine, "global.other_updated"), 1);

        // The second movie is one frame long and stops itself; the first one
        // keeps going.
        tick(&mut engine, 100);
        assert_eq!(pixel(&mut engine, "hooked", 0, 0), (4, 5, 6, 255));
        assert_eq!(integer(&mut engine, "global.other_updated"), 1);
        assert_eq!(integer(&mut engine, "other.isPlayingMovie()"), 0);
        assert_eq!(handlers(&engine), 1);
        run(&mut engine, "hooked.stopMovie();");
        assert_eq!(handlers(&engine), 0);
    }

    /// A movie that cannot open reports and leaves the layer alone: the
    /// storage's own failure (`:142-147`) and the backend's (`:180-184`).
    #[test]
    fn a_movie_that_cannot_open_reports_and_leaves_the_layer_alone() {
        // The storage has no such file.
        let factory = Arc::new(FakeVideoFactory::new(vec![frame(1, 1, 0, &[1, 2, 3, 255])]));
        let mut missing = engine(Arc::clone(&factory));
        run(&mut missing, HOOKED_LAYER);
        let size = (
            integer(&mut missing, "hooked.imageWidth"),
            integer(&mut missing, "hooked.imageHeight"),
        );
        run(&mut missing, r#"hooked.openMovie("missing.mp4", false);"#);
        assert!(
            logs(&missing).contains("cannot open \"missing.mp4\""),
            "the missing movie is diagnosed: {}",
            logs(&missing)
        );
        assert_eq!(
            (
                integer(&mut missing, "hooked.imageWidth"),
                integer(&mut missing, "hooked.imageHeight"),
            ),
            size,
            "the size push (`:247-248`) only happens for a movie that opened"
        );
        run(&mut missing, "hooked.startMovie(true);");
        assert_eq!(integer(&mut missing, "global.started"), 0);
        assert_eq!(integer(&mut missing, "hooked.isPlayingMovie()"), 0);
        assert_eq!(handlers(&missing), 0);
        tick(&mut missing, 0);
        assert_eq!(integer(&mut missing, "global.updated"), 0);

        // The file opens and the backend refuses the bytes.
        let failing = Arc::new(FakeVideoFactory::failing("no codec for these bytes"));
        let mut refused = engine(Arc::clone(&failing));
        run(&mut refused, HOOKED_LAYER);
        run(&mut refused, r#"hooked.openMovie("movie.mp4", false);"#);
        let logs = logs(&refused);
        assert!(
            logs.contains("cannot open \"movie.mp4\"") && logs.contains("no codec for these bytes"),
            "the backend's refusal is reported: {logs}"
        );
        assert_eq!(
            failing.opened.lock().expect("lock").as_slice(),
            ["movie.mp4"],
            "the storage bytes reached the backend"
        );
        assert_eq!(integer(&mut refused, "hooked.isPlayingMovie()"), 0);
        assert_eq!(handlers(&refused), 0);
    }

    /// Opening another movie replaces the playing one (`clearMovie()`, `:129`)
    /// without an event, and playback needs a new `startMovie`.
    #[test]
    fn opening_another_movie_replaces_the_playing_one() {
        let factory = Arc::new(FakeVideoFactory::queue(vec![
            vec![frame(2, 1, 0, &[1, 2, 3, 255, 4, 5, 6, 255])],
            vec![frame(1, 1, 0, &[9, 9, 9, 255])],
        ]));
        let mut engine = engine(factory);
        run(&mut engine, HOOKED_LAYER);
        run(
            &mut engine,
            r#"hooked.openMovie("movie.mp4", false); hooked.startMovie(true);"#,
        );
        tick(&mut engine, 0);
        assert_eq!(integer(&mut engine, "global.updated"), 1);
        assert_eq!(integer(&mut engine, "hooked.imageWidth"), 2);

        run(&mut engine, r#"hooked.openMovie("movie.mp4", false);"#);
        assert_eq!(integer(&mut engine, "hooked.imageWidth"), 1);
        assert_eq!(integer(&mut engine, "hooked.isPlayingMovie()"), 0);
        assert_eq!(
            integer(&mut engine, "global.stopped"),
            0,
            "an open is not a stop"
        );
        assert_eq!(
            handlers(&engine),
            0,
            "the replaced movie's callback is gone"
        );
        tick(&mut engine, 100);
        assert_eq!(integer(&mut engine, "global.updated"), 1);

        run(&mut engine, "hooked.startMovie(false);");
        tick(&mut engine, 200);
        assert_eq!(integer(&mut engine, "global.updated"), 2);
        assert_eq!(pixel(&mut engine, "hooked", 0, 0), (9, 9, 9, 255));
    }

    /// A frame is clamped to the layer's bitmap, so a script that resized the
    /// layer after `openMovie` cannot make the copy run off either plane.
    #[test]
    fn a_frame_is_clamped_to_both_bitmaps() {
        let factory = Arc::new(FakeVideoFactory::new(vec![frame(
            2,
            1,
            0,
            &[1, 2, 3, 255, 4, 5, 6, 255],
        )]));
        let mut engine = engine(factory);
        run(&mut engine, HOOKED_LAYER);
        run(
            &mut engine,
            r#"hooked.openMovie("movie.mp4", false); hooked.startMovie(true); hooked.setImageSize(4, 2);"#,
        );
        // What the enlarged image holds outside the movie's 2x1 corner is the
        // resize's own fill; the copy must leave it alone.
        let outside = (
            pixel(&mut engine, "hooked", 2, 0),
            pixel(&mut engine, "hooked", 3, 1),
        );
        tick(&mut engine, 0);
        assert_eq!(integer(&mut engine, "global.updated"), 1);
        assert_eq!(pixel(&mut engine, "hooked", 0, 0), (1, 2, 3, 255));
        assert_eq!(pixel(&mut engine, "hooked", 1, 0), (4, 5, 6, 255));
        assert_eq!(
            (
                pixel(&mut engine, "hooked", 2, 0),
                pixel(&mut engine, "hooked", 3, 1)
            ),
            outside,
            "the copy stops at the decoded frame (`:383` clamps it here)"
        );
    }

    /// A layer that goes away drops its movie with it: the slot is pruned, and
    /// the session-less callback retires itself on the next tick.
    #[test]
    fn invalidating_the_layer_retires_the_per_frame_callback() {
        let factory = Arc::new(FakeVideoFactory::new(vec![frame(1, 1, 0, &[1, 2, 3, 255])]));
        let mut engine = engine(factory);
        run(&mut engine, HOOKED_LAYER);
        run(
            &mut engine,
            r#"hooked.openMovie("movie.mp4", false); hooked.startMovie(true);"#,
        );
        tick(&mut engine, 0);
        assert_eq!(handlers(&engine), 1);

        run(&mut engine, "invalidate hooked;");
        tick(&mut engine, 100);
        assert_eq!(handlers(&engine), 0);
        tick(&mut engine, 200);
        assert_eq!(integer(&mut engine, "global.updated"), 1);
        assert_eq!(integer(&mut engine, "global.stopped"), 0);
    }

    /// The four members plus the three script hooks the reference reads off
    /// the layer object (`layerExMovie.cpp:55-60`) exist as callable members.
    #[test]
    fn the_layer_surface_is_registered() {
        let engine = engine(Arc::new(FakeVideoFactory::failing("unused")));
        let layer = layer_class(&engine);
        for name in ["openMovie", "startMovie", "stopMovie", "isPlayingMovie"] {
            assert!(
                is_callable_member(&engine, layer, name),
                "Layer.{name} is registered"
            );
        }
    }

    fn engine_without_storage() -> KrkrEngine {
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
        let mut engine = engine_without_storage();
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

    /// A second registration (the engine re-installs a module at `Plugins.link`)
    /// keeps the surface, and a script-owned member is left alone.
    #[test]
    fn registration_is_idempotent_and_respects_script_members() {
        let mut re_registered = engine_without_storage();
        re_registered
            .register_plugin(LayerExMoviePlugin)
            .expect("second");
        re_registered
            .execute_script("inline.tjs", "global.layer = new Layer();")
            .expect("layer");
        assert_eq!(
            re_registered
                .execute_expression("read.tjs", "layer.isPlayingMovie()")
                .expect("isPlayingMovie")
                .to_integer()
                .expect("integer"),
            0
        );

        let mut patched = engine_without_storage();
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
}
