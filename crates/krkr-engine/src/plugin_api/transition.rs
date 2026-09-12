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
//! # The factory's host services
//!
//! `StartTransition` (`transhandler.h:317-332`) hands a provider more than the
//! options and the sizes: an **`iTVPSimpleImageProvider *imagepro`**, the
//! host's image loader (`:149-166`), and an option provider whose
//! `GetDispatchObject` (`:135-139`) gives back the script object's own
//! dispatch (`tTVPSimpleOptionProvider::GetDispatchObject`,
//! `TransIntf.cpp:116-129`) — the hook a provider calls script from.  M79
//! captured only the option *values*, so a provider whose reference reads a
//! rule graphic or calls back into the script could not be ported.  A provider
//! now opts in by overriding
//! [`TransitionHandlerProvider::start_transition_with`] instead of
//! [`TransitionHandlerProvider::start_transition`]; the default forwards, so
//! every M79 provider keeps compiling and keeps its behaviour.
//!
//! [`TransitionContext`] is what such a factory receives — the host services,
//! scoped to the call:
//!
//! * [`TransitionContext::load_image`] is `imagepro->LoadImage(name, bpp, key,
//!   w, h, &scpro)` (`transhandler.h:149-166`, implemented as
//!   `tTVPSimpleImageProvider::LoadImage`, `TransIntf.cpp:139-162`, over
//!   `TVPLoadGraphic(..., glmGrayscale)`, `GraphicsLoaderIntf.cpp:1672`).
//!   `bpp` is 8 or 32 (`:146`); 8 means an 8bpp grayscale plane, converted
//!   with the reference's own luminance `(B*19 + G*183 + R*54) >> 8`
//!   (`compose_grayscale`, `kirikiri2/.../visual/tvpgl.c:10322`; the same
//!   weights in `do_gray_scale_functor`,
//!   `krkrz/visual/gl/blend_functor_c.h:883-889`), 32 keeps the decoded
//!   RGBA bytes.  `key` is the colour key: a key whose alpha byte is zero
//!   makes every 32bpp pixel equal to it fully transparent and every other
//!   pixel fully opaque (`TVPMakeAlphaFromKey`, `blend_functor_c.h:838-846`,
//!   run when `(key & 0xff000000) == 0`, `GraphicsLoaderIntf.cpp:951`);
//!   `0x02ffffff` is the reference's "no key" spelling
//!   (`TransIntf.cpp:781`).  `w`/`h` are the desired size: the returned plane
//!   is `max(source, desired)` per axis and a source smaller than desired is
//!   tiled (repeated) to it, exactly the loader's
//!   `SizeCallback`/scanline tiling (`GraphicsLoaderIntf.cpp:869-1011`),
//!   with `0` meaning "the source's own size".  The built-in `universal`
//!   provider loads its rule the same way (`TransIntf.cpp:775-784`), and so
//!   do KaichoTrans' `dim` (`dim.cpp:421`) and extNagano's `3duniversal` /
//!   `imagewipe`.
//! * [`TransitionContext::script_callback`] wraps a callable member of the
//!   options object (the reference's dispatch-object hook) in an opaque
//!   [`TransitionScriptCallback`].  The handler may call it from any pass; the
//!   call is queued and the engine runs it on the script thread once the
//!   tick's transition passes are done.  A hook the reference invokes
//!   synchronously inside `Process` therefore lands a few statements later
//!   instead of inside the pass: a handler is `Send` and can never hold the
//!   runtime (the M79 constraint), so the script call is deferred to the
//!   script thread rather than faked.
//!
//!   Which member is the hook is the provider's choice.  extNagano's
//!   `multiripple` reads `callback` (`docs/plugins/extnagano.md`), the member
//!   the engine itself reads as the transition's tick source before the
//!   provider runs (`LayerIntf.cpp:6222-6233`; `transition_driver_options` in
//!   `native/classes.rs`), so in that spelling the hook and the engine's clock
//!   are the same script function — the reference's behaviour too.
//!
//! # Nested option tables
//!
//! The reference reads *nested* members off the options object: extNagano's
//! `morphing` walks `before.Array` and `after.Array` through the child
//! objects (`0x100152d0` reads `Array` and `count` there,
//! `docs/plugins/extnagano.md`).  [`TransitionOptions::snapshot`] captures an
//! object-valued member recursively (bounded at
//! [`TRANSITION_OPTIONS_MAX_DEPTH`] levels and
//! [`TRANSITION_OPTIONS_MAX_TABLES`] tables in total; callable objects are not
//! descended into), and [`TransitionOptions::table`] /
//! [`TransitionOptions::element`] read those child tables back.  An Array's
//! elements are its members `"0"`, `"1"`, ..., matching the array's own member
//! enumeration; a child read is the same raw member read as the top level
//! (`tTVPSimpleOptionProvider::GetValue`'s plain `PropGet`,
//! `TransIntf.cpp:100-114`), so a getter is not run for one either.
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
//! `&mut self`, so a handler needs no internal synchronization.  A
//! [`TransitionScriptCallback`] call queues the invocation instead of running
//! TJS; the engine drains the queue on the script thread once the tick's
//! passes are done, so a callback's script effect is visible to the next
//! statement the scenario runs, never inside `process` itself.

use std::{
    fmt,
    sync::{Arc, Mutex},
    time::Duration,
};

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

    /// `StartTransition` as the engine calls it: [`start_transition`](Self::start_transition)
    /// plus the host services the reference passes next to the options — the
    /// image provider and the script-closure hook, see [`TransitionContext`].
    /// The engine always calls this method; the
    /// default forwards to `start_transition` and ignores the context, so a
    /// provider that needs no rule image and no callback keeps its M79
    /// implementation unchanged.
    ///
    /// The context borrows the host for the duration of the call: copy out
    /// what the handler needs ([`TransitionContext::load_image`]'s pixels, a
    /// [`TransitionScriptCallback`]) before returning, the way the reference
    /// provider keeps the loaded `iTVPScanLineProvider` and the callback's
    /// ref-counted closure.
    fn start_transition_with(
        &self,
        request: &TransitionRequest,
        context: &mut TransitionContext<'_>,
    ) -> Result<Box<dyn TransitionHandler>, TransitionHandlerError> {
        let _ = context;
        self.start_transition(request)
    }
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

/// The host services one `StartTransition` call is handed next to the options
/// — the reference's `iTVPSimpleImageProvider *imagepro` argument plus the
/// script-dispatch hook of its option provider
/// (`transhandler.h:306-332`, `:135-139`).
///
/// The engine constructs it for the duration of
/// [`TransitionHandlerProvider::start_transition_with`] and drops it when the
/// factory returns: a provider copies out everything the handler needs (rule
/// pixels, a [`TransitionScriptCallback`]) and stores those, the way a
/// reference provider keeps its loaded `iTVPScanLineProvider` and the
/// callback's `tTJSVariantClosure`.
pub struct TransitionContext<'a> {
    host: &'a mut KrkrHost,
    scripts: Arc<TransitionScriptCallQueue>,
}

impl<'a> TransitionContext<'a> {
    pub(crate) fn new(host: &'a mut KrkrHost, scripts: Arc<TransitionScriptCallQueue>) -> Self {
        Self { host, scripts }
    }

    /// `iTVPSimpleImageProvider::LoadImage` (`transhandler.h:149-166`): load
    /// the storage graphic `name` as an image of `bpp` bits per pixel whose
    /// buffer is at least `width`x`height`, and return its pixels.
    ///
    /// [`TransitionRuleImage`] documents the pixel layouts, the colour key and
    /// the tiling rule; this method is the seam's counterpart of the reference
    /// engine's `tTVPSimpleImageProvider::LoadImage`
    /// (`TransIntf.cpp:139-162`) — the loader the built-in `universal`
    /// provider reads its `rule` through, and the one KaichoTrans' `dim`
    /// (`dim.cpp:421`) and extNagano's `3duniversal`/`imagewipe` name.
    ///
    /// `name` is a storage name, as in the reference (`LoadImage` takes a
    /// `tjs_char *` and runs it through `TVPLoadGraphic`).  A provider option
    /// that may also carry an *image object* — extNagano's `3duniversal`
    /// accepts one (`docs/plugins/extnagano.md`) — is not covered: the
    /// reference plugin reads such an object's own layer buffer, which this
    /// context does not hand out.
    pub fn load_image(
        &mut self,
        name: &str,
        bpp: u32,
        key: u32,
        width: u32,
        height: u32,
    ) -> std::result::Result<TransitionRuleImage, TransitionImageError> {
        if bpp != 8 && bpp != 32 {
            return Err(TransitionImageError::new(format!(
                "invalid bitmap color depth {bpp}"
            )));
        }
        let image = self
            .host
            .load_image_storage(name)
            .map_err(|error| TransitionImageError::new(error.message))?;
        let source_width = image.upload.width;
        let source_height = image.upload.height;
        if source_width == 0 || source_height == 0 {
            return Err(TransitionImageError::new(format!(
                "image `{name}` has no pixels"
            )));
        }
        let source = image.upload.rgba.as_ref();
        let channels = if bpp == 8 { 1 } else { 4 };
        let mut pixels =
            Vec::with_capacity(source_width as usize * source_height as usize * channels);
        for pixel in source.chunks_exact(4) {
            if bpp == 8 {
                pixels.push(gray_from_rgba(pixel));
            } else if key & 0xff00_0000 == 0 {
                // `TVPMakeAlphaFromKey` (`blend_functor_c.h:838-846`), run by
                // the loader only when the key's alpha byte is zero
                // (`(ColorKey & 0xff000000) == 0`, `GraphicsLoaderIntf.cpp:951`):
                // every pixel equal to the key becomes fully transparent,
                // every other pixel fully opaque.  The comparison is on the
                // 24-bit RGB value, the way the reference masks its
                // memory-order pixel (`d & 0x00ffffff`).
                let rgb =
                    u32::from(pixel[0]) << 16 | u32::from(pixel[1]) << 8 | u32::from(pixel[2]);
                let alpha = if rgb == key { 0 } else { 255 };
                pixels.extend_from_slice(&[pixel[0], pixel[1], pixel[2], alpha]);
            } else {
                pixels.extend_from_slice(pixel);
            }
        }
        // The loader's `SizeCallback` (`GraphicsLoaderIntf.cpp:869-899`):
        // desired sizes only widen the buffer, never scale it down, and a zero
        // means "the source's own size".  A source smaller than the buffer is
        // tiled into it (`:960-1005`).
        let buffer_width = if width == 0 {
            source_width
        } else {
            source_width.max(width)
        };
        let buffer_height = if height == 0 {
            source_height
        } else {
            source_height.max(height)
        };
        if buffer_width != source_width || buffer_height != source_height {
            let mut tiled =
                Vec::with_capacity(buffer_width as usize * buffer_height as usize * channels);
            for y in 0..buffer_height as usize {
                let row = (y % source_height as usize) * source_width as usize;
                for x in 0..buffer_width as usize {
                    let start = (row + x % source_width as usize) * channels;
                    tiled.extend_from_slice(&pixels[start..start + channels]);
                }
            }
            pixels = tiled;
        }
        Ok(TransitionRuleImage::new(
            buffer_width,
            buffer_height,
            bpp,
            pixels,
        ))
    }

    /// An opaque handle for the script closure `callee` — a callable member of
    /// the options object, the reference's dispatch-object hook
    /// (`iTVPSimpleOptionProvider::GetDispatchObject`, `transhandler.h:135-139`;
    /// `tTVPSimpleOptionProvider::GetDispatchObject`, `TransIntf.cpp:116-129`).
    ///
    /// `None` for a value that is not callable (a number, a string, `void`).
    /// The returned handle may be stored in the handler and called from any
    /// pass; the call is queued and the engine runs it on the script thread
    /// (see [`TransitionScriptCallback`]).
    pub fn script_callback(&self, callee: &Variant) -> Option<TransitionScriptCallback> {
        match callee {
            Variant::Object(_) | Variant::Closure(_) => Some(TransitionScriptCallback {
                callee: callee.clone(),
                scripts: Arc::clone(&self.scripts),
            }),
            Variant::Void
            | Variant::Null
            | Variant::Integer(_)
            | Variant::Real(_)
            | Variant::String(_)
            | Variant::Octet(_)
            | Variant::CodeObject(_) => None,
        }
    }
}

/// The reference's grayscale conversion (`compose_grayscale`,
/// `kirikiri2/.../visual/tvpgl.c:10322`; the same weights in the SIMD
/// `do_gray_scale_functor`, `krkrz/visual/gl/blend_functor_c.h:883-889`): the
/// luminance is `(B*19 + G*183 + R*54) >> 8`.  The alpha byte takes no part —
/// the 8bpp form has no alpha channel.
fn gray_from_rgba(pixel: &[u8]) -> u8 {
    let (r, g, b) = (
        u32::from(pixel[0]),
        u32::from(pixel[1]),
        u32::from(pixel[2]),
    );
    ((b * 19 + g * 183 + r * 54) >> 8) as u8
}

/// A rule graphic loaded through [`TransitionContext::load_image`] — the
/// reference's `iTVPScanLineProvider` over the bitmap `TVPLoadGraphic`
/// produced (`transhandler.h:83-111`).
///
/// * `bpp = 8`: one grayscale byte per pixel, row-major, tightly packed — the
///   `glmGrayscale` plane of `TVPBLConvert32BitTo8Bit`
///   (`kirikiri2/.../visual/tvpgl.c:10426-10441`), whose 8bpp bitmap has no
///   alpha.  KaichoTrans' `dim` blur-reads this plane (`dim.cpp:425-429`).
/// * `bpp = 32`: four bytes per pixel in the engine's own store — R, G, B, A
///   (`plugin_api::layer` §B.3.4) — with the colour key applied when one was
///   passed.
///
/// `width`/`height` are the buffer's, which is `max(source, desired)` per axis
/// (`GraphicsLoaderIntf.cpp:869-899`): a source smaller than the desired size
/// is tiled — repeated — into the larger buffer (`:960-1005`), a source larger
/// than desired is returned as it is, exactly the bitmap the reference hands
/// the provider.  The bytes are owned, so a handler may copy or mutate them
/// (the reference's `GetScanLineForWrite`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransitionRuleImage {
    width: u32,
    height: u32,
    bpp: u32,
    pixels: Vec<u8>,
}

impl TransitionRuleImage {
    /// Builds a rule image, for a provider's own tests.
    pub fn new(width: u32, height: u32, bpp: u32, pixels: Vec<u8>) -> Self {
        Self {
            width,
            height,
            bpp,
            pixels,
        }
    }

    /// The buffer's width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// The buffer's height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// 8 for a grayscale plane, 32 for RGBA pixels.
    pub fn bpp(&self) -> u32 {
        self.bpp
    }

    /// `width * height` bytes for an 8bpp plane, `width * height * 4` for a
    /// 32bpp one, row-major, tightly packed.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// The pixels for a handler that mutates its copy in place (the
    /// reference's `GetScanLineForWrite`, `transhandler.h:105-107`).
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }
}

/// A [`TransitionContext::load_image`] failure.
///
/// The reference returns `TJS_E_FAIL` and lets the provider raise its own
/// exception (KaichoTrans' `TVPCannotLoadRuleGraphic` text,
/// `dim.cpp:422-423`); this carries the engine's reason so the provider can
/// log it alongside its own message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransitionImageError {
    message: String,
}

impl TransitionImageError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// The engine's failure text (a missing storage, a decode failure).
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for TransitionImageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TransitionImageError {}

/// A script closure a provider's factory was handed through
/// [`TransitionContext::script_callback`], callable from a handler's pass —
/// extNagano's `multiripple` `callback` is the recovered case
/// (`docs/plugins/extnagano.md`).
///
/// The reference calls such a closure with `FuncCall` from inside `Process`;
/// a handler here is `Send` and its pass runs with the runtime already
/// borrowed, so [`call`](Self::call) does not run TJS inline.  It queues the
/// invocation, and the engine runs it on the script thread with the script
/// runtime in hand once the tick's transition passes are done — the same
/// engine turn, a few statements later.  The reference's `FuncCall` return
/// value is discarded: the hook is a notification.
#[derive(Clone)]
pub struct TransitionScriptCallback {
    callee: Variant,
    scripts: Arc<TransitionScriptCallQueue>,
}

impl TransitionScriptCallback {
    /// Queues one call of the closure with `args`.
    ///
    /// Cloning the handle is cheap and every clone feeds the same queue; a
    /// handle is valid for as long as the transition runs (the queue belongs
    /// to the transition's destination layer).
    pub fn call(&self, args: impl IntoIterator<Item = Variant>) {
        self.scripts
            .push(self.callee.clone(), args.into_iter().collect());
    }
}

/// The per-destination queue of script calls provider handlers posted during
/// their passes — engine-internal transport for [`TransitionScriptCallback`].
///
/// The engine keeps one queue per destination layer in that layer's extension
/// slot (`KrkrHost::layer_extension_or_insert_with`), so the per-tick drain
/// (`native::classes::drain_transition_script_calls`) can find it from the
/// running transition's destination alone; a handler only ever sees the
/// [`TransitionScriptCallback`] handle.
#[derive(Default)]
pub(crate) struct TransitionScriptCallQueue {
    calls: Mutex<Vec<TransitionScriptCall>>,
}

impl TransitionScriptCallQueue {
    fn push(&self, callee: Variant, args: Vec<Variant>) {
        self.lock().push(TransitionScriptCall { callee, args });
    }

    pub(crate) fn take(&self) -> Vec<TransitionScriptCall> {
        std::mem::take(&mut *self.lock())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<TransitionScriptCall>> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// One queued [`TransitionScriptCallback::call`]: the closure and its
/// arguments, run by the engine's drain with the runtime in hand.
pub(crate) struct TransitionScriptCall {
    pub(crate) callee: Variant,
    pub(crate) args: Vec<Variant>,
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
    /// Object-valued members, captured as child snapshots (see
    /// [`TransitionOptions::table`]).
    tables: Vec<(String, TransitionOptions)>,
}

impl TransitionOptions {
    /// Builds a snapshot from `(name, value)` pairs, for a provider's own
    /// tests; [`with_table`](Self::with_table) adds child tables.
    pub fn new(entries: impl IntoIterator<Item = (String, Variant)>) -> Self {
        Self {
            entries: entries.into_iter().collect(),
            tables: Vec::new(),
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

    /// A member whose value is an object, captured as a child snapshot — the
    /// reference's read of a *nested* option table, such as extNagano
    /// `morphing`'s `before`/`after` (`0x100152d0` then walks `Array` and
    /// `count` on the child, `docs/plugins/extnagano.md`).  `None` when the
    /// member is absent or is not an object; a callable object (a function)
    /// is not descended into.
    pub fn table(&self, name: &str) -> Option<&TransitionOptions> {
        self.tables
            .iter()
            .find(|(entry, _)| entry == name)
            .map(|(_, table)| table)
    }

    /// Element `index` of a snapshot of an Array — the array's own member
    /// `"0"`, `"1"`, ... (`ObjectKind::Array` answers `PropGetByNum` from its
    /// item list, so the names are the indices).
    pub fn element(&self, index: usize) -> Option<&Variant> {
        self.value(&index.to_string())
    }

    /// Adds a child table, for a provider's own tests.
    pub fn with_table(mut self, name: impl Into<String>, table: TransitionOptions) -> Self {
        self.tables.push((name.into(), table));
        self
    }

    /// Everything the object's members held when the transition started,
    /// including one child snapshot per object-valued member (see
    /// [`TransitionOptions::table`]).
    pub(crate) fn snapshot(runtime: &Runtime<KrkrHost>, options: Option<ObjectHandle>) -> Self {
        let Some(options) = options else {
            return Self::default();
        };
        let mut budget = TRANSITION_OPTIONS_MAX_TABLES;
        Self::capture(runtime, options, TRANSITION_OPTIONS_MAX_DEPTH, &mut budget)
    }

    /// One object's members, plus the child snapshots `depth` more levels
    /// down.  The depth and budget bounds stop a self-referencing or
    /// explosively wide option object; a callable member (a function object) is
    /// recorded as its raw variant but not descended into, because its members
    /// are its code's own.
    fn capture(
        runtime: &Runtime<KrkrHost>,
        object: ObjectHandle,
        depth: usize,
        budget: &mut usize,
    ) -> Self {
        let mut entries = Vec::new();
        let mut tables = Vec::new();
        for (name, value) in runtime.object_members(object) {
            if depth > 0
                && *budget > 0
                && let Some(handle) = value.object_handle()
                && runtime.object_valid(handle)
                && !runtime.object_is_callable(handle)
            {
                *budget -= 1;
                tables.push((
                    name.clone(),
                    Self::capture(runtime, handle, depth - 1, budget),
                ));
            }
            entries.push((name, value));
        }
        Self { entries, tables }
    }
}

/// How many nested option objects [`TransitionOptions::snapshot`] captures at
/// most, across all levels.  `morphing`'s `before.Array` is two levels
/// (`before`, then the array) with at most 256 six-member patches, so the bound
/// is generous; it exists to keep an option object that points back at itself
/// (or at something as wide as the global object) from blowing the snapshot up.
const TRANSITION_OPTIONS_MAX_TABLES: usize = 4096;

/// How many nested option objects [`TransitionOptions::snapshot`] captures
/// below the options object itself.  `morphing`'s `before.Array` is two levels
/// (`before`, then the array), so the bound is generous; it exists to stop a
/// self-referencing object, not to fit a provider.
const TRANSITION_OPTIONS_MAX_DEPTH: usize = 8;

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

    use krkr_assets::ProjectStorage;
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

    // ------------------------------------------- the factory's host services

    /// An engine whose project storage serves `files` (`rule.png`, ...).
    fn engine_with_storage(files: Vec<(String, Vec<u8>)>) -> KrkrEngine {
        let storage = ProjectStorage::from_memory(files);
        KrkrEngine::new(EngineConfig {
            project_storage: Some(Arc::new(storage)),
            ..EngineConfig::default()
        })
        .expect("engine")
    }

    /// An RGBA PNG the engine's own decoder reads, encoded with the `png`
    /// crate the engine's image tests use.
    fn png_bytes(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(std::io::Cursor::new(&mut bytes), width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("png header");
            writer.write_image_data(rgba).expect("png data");
        }
        bytes
    }

    /// One `TransitionContext::load_image` call a probe provider makes.
    struct RuleLoad {
        storage: String,
        bpp: u32,
        key: u32,
        /// `None` loads at the transition's own size (`request.dest_size`),
        /// the shape `dim.cpp:421` uses (`src1w, src1h`).
        size: Option<(u32, u32)>,
    }

    #[derive(Debug, PartialEq)]
    enum RuleRecord {
        Loaded {
            width: u32,
            height: u32,
            bpp: u32,
            pixels: Vec<u8>,
        },
        Failed(String),
    }

    /// A provider whose factory loads one rule image through the new context —
    /// KaichoTrans `dim`'s and extNagano `3duniversal`/`imagewipe`'s shape.
    struct RuleProvider {
        name: &'static str,
        load: RuleLoad,
        log: Arc<Mutex<Vec<RuleRecord>>>,
    }

    impl TransitionHandlerProvider for RuleProvider {
        fn name(&self) -> &str {
            self.name
        }

        fn start_transition(
            &self,
            _request: &TransitionRequest,
        ) -> std::result::Result<Box<dyn TransitionHandler>, TransitionHandlerError> {
            Err(TransitionHandlerError::new(
                "the rule probe only starts through `start_transition_with`",
            ))
        }

        fn start_transition_with(
            &self,
            request: &TransitionRequest,
            context: &mut TransitionContext<'_>,
        ) -> std::result::Result<Box<dyn TransitionHandler>, TransitionHandlerError> {
            let (width, height) = self.load.size.unwrap_or(request.dest_size);
            let record = match context.load_image(
                &self.load.storage,
                self.load.bpp,
                self.load.key,
                width,
                height,
            ) {
                Ok(image) => RuleRecord::Loaded {
                    width: image.width(),
                    height: image.height(),
                    bpp: image.bpp(),
                    pixels: image.pixels().to_vec(),
                },
                Err(error) => RuleRecord::Failed(error.to_string()),
            };
            self.log.lock().expect("recording").push(record);
            Ok(Box::new(CopyHandler))
        }
    }

    /// A handler that keeps the destination's own pixels — enough for a probe
    /// whose work happens in the factory.
    struct CopyHandler;

    impl TransitionHandler for CopyHandler {
        fn process(&mut self, frame: TransitionFrame<'_>, dest: &mut [u8]) {
            if frame.dest_before.pixels.len() == dest.len() {
                dest.copy_from_slice(frame.dest_before.pixels);
            }
        }
    }

    /// Serves `bytes` under `load.storage`, runs one `beginTransition` through
    /// the rule probe, and returns what its `load_image` call saw.
    fn probe_rule(bytes: Vec<u8>, load: RuleLoad) -> RuleRecord {
        let mut engine = engine_with_storage(vec![(load.storage.clone(), bytes)]);
        let log = Arc::new(Mutex::new(Vec::new()));
        register_transition_provider(
            engine.tjs_runtime_mut(),
            Arc::new(RuleProvider {
                name: "ruleprobe",
                load,
                log: Arc::clone(&log),
            }),
        )
        .expect("register");
        engine
            .execute_script(
                "inline.tjs",
                &format!(
                    r#"
                    {LAYER_PAIR}
                    dest.beginTransition("ruleprobe", true, source, %[time: 10]);
                    "#
                ),
            )
            .expect("begin transition");
        let mut records = log.lock().expect("recording");
        assert_eq!(records.len(), 1, "the factory ran exactly once");
        records.pop().expect("record")
    }

    /// `iTVPSimpleImageProvider::LoadImage` through the context: an 8bpp load
    /// is the reference's grayscale conversion of the storage graphic, tiled to
    /// the transition's own size — `dim.cpp:421`'s call shape.
    #[test]
    fn a_provider_loads_a_rule_image_as_a_grayscale_plane_scaled_to_the_transition_size() {
        let fixture = [
            255, 0, 0, 255, // red
            0, 255, 0, 255, // green
            0, 0, 255, 255, // blue
            255, 255, 255, 255, // white
        ];
        let record = probe_rule(
            png_bytes(2, 2, &fixture),
            RuleLoad {
                storage: "rule.png".to_string(),
                bpp: 8,
                key: 0x02ff_ffff,
                size: None,
            },
        );
        assert_eq!(
            record,
            RuleRecord::Loaded {
                width: 4,
                height: 4,
                bpp: 8,
                // `(B*19 + G*183 + R*54) >> 8` per fixture pixel, tiled into
                // the 4x4 destination by repeating the 2x2 pattern: red 53,
                // green 182, blue 18, white 255.
                pixels: vec![
                    53, 182, 53, 182, //
                    18, 255, 18, 255, //
                    53, 182, 53, 182, //
                    18, 255, 18, 255, //
                ],
            }
        );
    }

    /// The `key` argument of `LoadImage`: a key whose alpha byte is zero
    /// applies `TVPMakeAlphaFromKey` — keyed pixels become transparent, every
    /// other pixel fully opaque — while the reference's `0x02ffffff` "no key"
    /// spelling keeps the decoded alpha (`GraphicsLoaderIntf.cpp:951`).
    #[test]
    fn a_rule_image_colour_key_transparentises_the_keyed_pixels() {
        let fixture = [
            0x11, 0x22, 0x33, 0x80, // the keyed colour, at a non-opaque alpha
            0xff, 0xff, 0xff, 0x40, // white with a non-opaque alpha
            0x00, 0x00, 0x00, 0xff, //
            0xff, 0x00, 0x00, 0xff, //
        ];
        let keyed = probe_rule(
            png_bytes(2, 2, &fixture),
            RuleLoad {
                storage: "key.png".to_string(),
                bpp: 32,
                key: 0x0011_2233,
                size: Some((2, 2)),
            },
        );
        assert_eq!(
            keyed,
            RuleRecord::Loaded {
                width: 2,
                height: 2,
                bpp: 32,
                pixels: vec![
                    0x11, 0x22, 0x33, 0x00, // equal to the key: transparent
                    0xff, 0xff, 0xff, 0xff, // every other pixel: opaque
                    0x00, 0x00, 0x00, 0xff, //
                    0xff, 0x00, 0x00, 0xff, //
                ],
            }
        );
        let unkeyed = probe_rule(
            png_bytes(2, 2, &fixture),
            RuleLoad {
                storage: "key.png".to_string(),
                bpp: 32,
                key: 0x02ff_ffff,
                size: Some((2, 2)),
            },
        );
        assert_eq!(
            unkeyed,
            RuleRecord::Loaded {
                width: 2,
                height: 2,
                bpp: 32,
                pixels: fixture.to_vec(),
            }
        );
    }

    /// A rule graphic that cannot be loaded fails the call with the engine's
    /// reason, which the provider reports its own way (`dim.cpp:422-423`).
    #[test]
    fn a_missing_rule_graphic_fails_the_load() {
        let record = probe_rule(
            b"not an image".to_vec(),
            RuleLoad {
                storage: "missing.png".to_string(),
                bpp: 8,
                key: 0x02ff_ffff,
                size: None,
            },
        );
        let RuleRecord::Failed(message) = record else {
            panic!("a corrupt graphic must fail the load: {record:?}");
        };
        assert!(
            message.contains("missing.png"),
            "the failure names it: {message}"
        );
    }

    /// The nested option tables `morphing` reads: the options object's
    /// `before`/`after` members, then their own `Array`/`count` members.
    #[test]
    fn a_provider_reads_nested_option_tables() {
        struct NestedProvider {
            log: Arc<Mutex<Vec<NestedRecord>>>,
        }

        #[derive(Debug, PartialEq)]
        struct NestedRecord {
            before_count: Option<i64>,
            before_patches: Vec<Option<i64>>,
            after_patches: Vec<Option<i64>>,
            callback_is_table: bool,
        }

        impl TransitionHandlerProvider for NestedProvider {
            fn name(&self) -> &str {
                "nestedprobe"
            }

            fn start_transition(
                &self,
                _request: &TransitionRequest,
            ) -> std::result::Result<Box<dyn TransitionHandler>, TransitionHandlerError>
            {
                Err(TransitionHandlerError::new(
                    "the nested probe only starts through `start_transition_with`",
                ))
            }

            fn start_transition_with(
                &self,
                request: &TransitionRequest,
                _context: &mut TransitionContext<'_>,
            ) -> std::result::Result<Box<dyn TransitionHandler>, TransitionHandlerError>
            {
                let patches = |table: Option<&TransitionOptions>, member: &str| {
                    table
                        .and_then(|table| table.table(member))
                        .map(|patches| {
                            (0..6)
                                .map(|index| {
                                    patches
                                        .element(index)
                                        .and_then(|value| value.to_integer().ok())
                                })
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let before = request.options.table("before");
                self.log.lock().expect("recording").push(NestedRecord {
                    before_count: before.and_then(|table| table.integer("count")),
                    before_patches: patches(before, "Array"),
                    after_patches: patches(request.options.table("after"), "Array"),
                    callback_is_table: request.options.table("callback").is_some(),
                });
                Ok(Box::new(CopyHandler))
            }
        }

        let mut engine = engine();
        let log = Arc::new(Mutex::new(Vec::new()));
        register_transition_provider(
            engine.tjs_runtime_mut(),
            Arc::new(NestedProvider {
                log: Arc::clone(&log),
            }),
        )
        .expect("register");
        engine
            .execute_script(
                "inline.tjs",
                &format!(
                    r#"
                    {LAYER_PAIR}
                    dest.beginTransition("nestedprobe", true, source, %[
                        time: 10,
                        before: %[Array: [1, 2, 3, 4, 5, 6], count: 2],
                        after: %[Array: [7, 8, 9, 10, 11, 12], count: 2],
                        callback: function(value) {{ global.nestedProbe = value; }}
                    ]);
                    "#
                ),
            )
            .expect("begin transition");
        assert_eq!(
            *log.lock().expect("recording"),
            vec![NestedRecord {
                before_count: Some(2),
                before_patches: vec![Some(1), Some(2), Some(3), Some(4), Some(5), Some(6)],
                after_patches: vec![Some(7), Some(8), Some(9), Some(10), Some(11), Some(12)],
                callback_is_table: false,
            }]
        );
    }

    /// A closure handed over through the context and called from a pass —
    /// extNagano `multiripple`'s shape.  The call is queued and runs on the
    /// script thread, so the script's own globals change.
    #[test]
    fn a_provider_calls_a_script_closure_on_the_script_thread() {
        struct CallbackProvider;

        struct CallbackHandler {
            callback: Option<TransitionScriptCallback>,
        }

        impl TransitionHandlerProvider for CallbackProvider {
            fn name(&self) -> &str {
                "callbackprobe"
            }

            fn start_transition(
                &self,
                _request: &TransitionRequest,
            ) -> std::result::Result<Box<dyn TransitionHandler>, TransitionHandlerError>
            {
                Err(TransitionHandlerError::new(
                    "the callback probe only starts through `start_transition_with`",
                ))
            }

            fn start_transition_with(
                &self,
                request: &TransitionRequest,
                context: &mut TransitionContext<'_>,
            ) -> std::result::Result<Box<dyn TransitionHandler>, TransitionHandlerError>
            {
                // The probe reads its own member; a port of extNagano's
                // `multiripple` reads `callback`, which the engine *also* reads
                // as its tick source (`transition_driver_options`, the
                // reference's `LayerIntf.cpp:6222-6233`), so the probe keeps
                // the two apart.
                let callback = request
                    .options
                    .value("hook")
                    .and_then(|value| context.script_callback(value));
                assert!(callback.is_some(), "the probe's hook option is a closure");
                Ok(Box::new(CallbackHandler { callback }))
            }
        }

        impl TransitionHandler for CallbackHandler {
            fn process(&mut self, frame: TransitionFrame<'_>, dest: &mut [u8]) {
                if let Some(callback) = &self.callback {
                    // The pass marker: 1000 + the handler clock, so the test
                    // knows which pass's call it read back.
                    callback.call([Variant::Integer(1000 + frame.tick.as_millis() as i64)]);
                }
                if frame.dest_before.pixels.len() == dest.len() {
                    dest.copy_from_slice(frame.dest_before.pixels);
                }
            }
        }

        let mut engine = engine();
        register_transition_provider(engine.tjs_runtime_mut(), Arc::new(CallbackProvider))
            .expect("register");
        engine
            .execute_script(
                "inline.tjs",
                &format!(
                    r#"
                    {LAYER_PAIR}
                    global.got = 0;
                    global.calls = 0;
                    dest.beginTransition("callbackprobe", true, source, %[time: 20, hook: function(value) {{
                        global.got = value;
                        global.calls = global.calls + 1;
                    }}]);
                    "#
                ),
            )
            .expect("begin transition");
        // `StartTransition` ends with `Update(true)`, so the tick-zero pass ran
        // inside the script call; the call it queued was delivered on the
        // script thread before `beginTransition` returned.
        assert_eq!(pixel(&mut engine, "got"), Variant::Integer(1000));
        assert_eq!(pixel(&mut engine, "calls"), Variant::Integer(1));

        // The idle tick's pass queues its own call, and the engine's per-tick
        // drain delivers it in the same tick.
        engine.update(input(), Duration::ZERO).expect("frame");
        assert_eq!(pixel(&mut engine, "calls"), Variant::Integer(2));
        engine
            .update(input(), Duration::from_millis(10))
            .expect("frame");
        assert_eq!(pixel(&mut engine, "got"), Variant::Integer(1010));
        assert_eq!(pixel(&mut engine, "calls"), Variant::Integer(3));

        // The clock reaches `time`: the transition stops without another pass,
        // so no further call is queued.
        engine
            .update(input(), Duration::from_millis(10))
            .expect("frame");
        assert!(!engine.host().has_active_transition());
        assert_eq!(pixel(&mut engine, "calls"), Variant::Integer(3));
    }
}
