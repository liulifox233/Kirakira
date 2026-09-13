//! Layer bitmap access for plugins — Part B §B.3 of
//! `docs/plugins/plugin-facing-engine-facilities.md`.
//!
//! The `layerEx*` family and pixel consumers such as `GlitchEffect` work by
//! read-modify-writing a layer's bitmap, which the reference exposes as
//! `Layer.mainImageBuffer`/`mainImageBufferForWrite` — a raw pointer cast to a
//! TJS integer (`LayerIntf.cpp:9513-9553`).  A memory-safe engine cannot hand
//! that out, so this module hands out **scoped, typed views with an explicit
//! commit** instead: the plugin supplies a closure, the engine lends
//! `&mut [u8]` for the closure's duration — writing the layer's own buffer when
//! the call owns it, a private copy when the buffer is shared with an upload
//! record, a frozen transition face or another layer — and commits afterwards.
//! The reference's use-after-resize bugs become unrepresentable, and the
//! `&mut Runtime<KrkrHost>` argument proves at compile time that no view ever
//! leaves the script thread.
//!
//! * [`layer_bitmap_read`] / [`layer_bitmap_write`] / [`layer_bitmap_read_write`]
//!   — the main plane.
//! * [`layer_province_read`] / [`layer_province_write`] — the 8bpp province
//!   plane (`layerExBTOA`'s `copyAlphaToProvince`/`fillByProvince`).
//! * [`layer_update`] — `Layer.update()`, the explicit repaint step the family
//!   contract calls after a mutation.
//! * [`create_canvas_layer`] / [`fit_canvas_layer`] / [`attach_canvas_layer`] —
//!   a native `Layer` for a plugin that needs a private draw target and a
//!   publishable image, as `motionplayer.dll`'s `Motion.SeparateLayerAdaptor`
//!   is (the KAG motion layer draws into it and the reference's adaptor reaches
//!   the screen as a visible child of its owner, which `attach_canvas_layer`
//!   reproduces).
//!
//! **Byte order**: the views expose the engine's own store — R, G, B, A per
//! pixel, top-down, tightly packed.  The reference's buffer is B, G, R, A
//! (`docs/plugins/layer-ex-family.md` §1: memory order `0xAARRGGBB`), so a
//! ported algorithm that indexes byte 0 as blue must index byte 2 instead
//! (§B.3.4).  **Alpha**: layers converted with `Layer.convertType` to a
//! premultiplied type (`ltAddAlpha`) hold premultiplied bytes; the view exposes
//! `layer_type` so a plugin can refuse or convert, and the engine never
//! silently un-premultiplies.
//!
//! **The clip box is data, not policy**: [`LayerBitmap::clip`] carries the
//! live `ClipRect`, and the plugin applies it the way `layerExBase` does
//! (`layerExBase.hpp:108-117`).  The engine does not silently clip writes,
//! because the family deliberately operates on the clip box and expects the
//! whole buffer otherwise (§B.3.1).
//!
//! **Errors**: a layer without a main image fails with
//! [`LayerBitmapError::NotDrawable`], the reference's `TVPNotDrawableLayerType`
//! ("Not drawable layer type", `LayerIntf.cpp:2594-2596`) — a layer whose
//! image the script freed must not be resurrected.
//!
//! **Panics**: a panic inside a closure unwinds to the TJS native call handler
//! like any other host closure and the commit never runs.  A closure writing a
//! private copy (the shared-buffer case) leaves the layer untouched, the way
//! the old unconditional copy did; a closure writing the layer's own buffer in
//! place (the exclusively owned case) leaves the bytes written before the
//! unwind in the bitmap — the reference hands out the live buffer too, so a
//! crashing plugin leaves partial writes there as well.  The engine does not
//! catch-unwind (§B.5.6).
//!
//! **Registrations**: this facility adds no TJS members.  `mainImageBuffer*`
//! and the province trio stay registered, read-only and returning `void`
//! (§B.4), and a ported plugin attaches its own members to the global `Layer`
//! class exactly like the existing stubs do
//! (`crates/krkr-plugins/src/layer_ex_draw.rs`).

use std::sync::Arc;

use krkr_core::{LayerImage, LayerNode, ProvinceImage};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{ObjectHandle, Runtime},
};

use crate::host::{KrkrHost, LayerRenderTarget};
use crate::native::classes::{
    LAYER_CLASS, allocate_layer_province_plane, construct_native_instance,
    internal_set_layer_image_size, join_layer_under_parent, layer_update_by_script,
    mark_image_modified, mutate_render_layer, not_drawable_layer_type, render_layer_snapshot,
    set_layer_geographical_size, this_render_layer_target,
};

/// Failure of a plugin-facing layer bitmap call.
///
/// The engine has exactly one failure mode here, matching the reference's
/// `TVPNotDrawableLayerType`: the layer has no main image (freed with
/// `Layer.freeImage`, or the object is not a layer attached to a render node
/// at all).  The text a script sees is "Not drawable layer type"
/// (`LayerIntf.cpp:2594-2596`), the same error `setMainPixel` throws.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayerBitmapError {
    /// The layer has no drawable main image.
    NotDrawable,
}

impl From<LayerBitmapError> for TjsError {
    fn from(error: LayerBitmapError) -> Self {
        match error {
            LayerBitmapError::NotDrawable => not_drawable_layer_type(),
        }
    }
}

/// One layer's main bitmap, as handed to a plugin closure.
///
/// This is a snapshot taken when the call starts; the bytes live in
/// [`LayerBitmapView`]/[`LayerBitmapViewMut`], not here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LayerBitmap {
    pub width: u32,
    pub height: u32,
    /// Bytes per row.  The engine's store is tightly packed today, so this is
    /// always `width * 4`; the field exists because the reference's
    /// `mainImageBufferPitch` can exceed it and plugins must not assume
    /// otherwise (§B.5.3 keeps `pitch == width * 4` for now).
    pub pitch: u32,
    /// `layer.image.upload.texture_id`: changes whenever the engine replaces
    /// the image (every commit allocates a fresh id).  Per-layer plugin caches
    /// key on this to detect replacement instead of caching pointers (§B.3.3).
    pub generation: u64,
    /// The layer's active `ClipRect` as `(left, top, width, height)` in
    /// layer-local coordinates, already defaulted to the full image when unset
    /// (reference `ClipRect`, `LayerIntf.cpp:3709-3716`).
    pub clip: (i64, i64, i64, i64),
    /// `layer_type` (`ltBinder`=0, `ltAlpha`=2, …), so a plugin can apply the
    /// same type checks the reference's family does — the binder/effect/filter
    /// types (0, 6, 7) never carry an image.
    pub layer_type: i32,
}

/// A read-only view of a layer's pixels.
pub struct LayerBitmapView<'a> {
    pub bitmap: LayerBitmap,
    pub pixels: &'a [u8],
}

/// A writable view of a layer's pixels: mutate `pixels`, and the engine
/// commits the copy when the closure returns.
pub struct LayerBitmapViewMut<'a> {
    pub bitmap: LayerBitmap,
    pub pixels: &'a mut [u8],
}

/// Lends the layer's main bitmap to `read`.
///
/// The layer must have a main image; a freed one fails with
/// [`LayerBitmapError::NotDrawable`] rather than resurrecting the bitmap.
pub fn layer_bitmap_read<R>(
    runtime: &mut Runtime<KrkrHost>,
    layer: ObjectHandle,
    read: impl FnOnce(&LayerBitmapView<'_>) -> R,
) -> Result<R> {
    let resolved = resolve_layer(runtime, layer).ok_or(LayerBitmapError::NotDrawable)?;
    let image = resolved
        .node
        .image
        .as_ref()
        .ok_or(LayerBitmapError::NotDrawable)?;
    let view = LayerBitmapView {
        bitmap: bitmap_metadata(&resolved.node, image),
        pixels: image.upload.rgba.as_ref(),
    };
    Ok(read(&view))
}

/// Lends the layer's main bitmap to `write` and commits the result.
///
/// The commit replaces the layer's image with a fresh texture id (so the frame
/// output re-uploads it) and marks the layer modified exactly like the native
/// pixel setters (`classes.rs` `mutate_layer_pixels_min_with_host`).  It does
/// **not** run `Layer.update`: the family contract is "mutate, then `update()`"
/// (`layerExBase.hpp:99-103`), so the plugin calls [`layer_update`] itself when
/// the repaint is due.
///
/// **The ownership rule**: the closure writes the layer's live buffer *in
/// place* exactly when this call holds the only reference to it — the image is
/// lifted out of the node, the bytes are written where they already are, and
/// the commit puts the same allocation back under a freshly minted texture id.
/// When any other holder exists the closure writes a private clone instead and
/// the layer keeps its old bytes until the commit: that covers the host's
/// upload record for the image's current texture id (the frame that already
/// published it), a frozen transition face, another layer sharing the image
/// through `Layer.assignImages`, and any snapshot a caller still owns.  The
/// check is `Arc::strong_count` on the pixel buffer, so the fallback is the
/// conservative side of every case; see [`WritePlane`] for the two paths.
pub fn layer_bitmap_write<R>(
    runtime: &mut Runtime<KrkrHost>,
    layer: ObjectHandle,
    write: impl FnOnce(&mut LayerBitmapViewMut<'_>) -> R,
) -> Result<R> {
    let Some(taken) = take_write_plane(runtime, layer) else {
        return Err(LayerBitmapError::NotDrawable.into());
    };
    Ok(run_write(runtime, taken, write))
}

/// A source snapshot plus a destination write, in one call.
///
/// `src == dest` is legal and behaves as an in-place operation over a snapshot
/// of the bytes as they were when the call started (the reference shifts rows
/// inside one buffer, `layerExRaster/main.cpp:38-97`).  Only the destination
/// commits; the source is never written.  Both layers must have a main image.
///
/// The source is always snapshotted first, so a destination that shares the
/// source's buffer (`Layer.assignImages`, or `src == dest`) still sees the
/// pre-call bytes for the whole closure.  The destination commits through
/// [`layer_bitmap_write`]'s ownership rule: in place when nothing else holds
/// its buffer, over a private clone when a holder exists — including the
/// source layer itself, whose pixels a shared buffer must not change.
pub fn layer_bitmap_read_write<R>(
    runtime: &mut Runtime<KrkrHost>,
    src: ObjectHandle,
    dest: ObjectHandle,
    run: impl FnOnce(&LayerBitmapView<'_>, &mut LayerBitmapViewMut<'_>) -> R,
) -> Result<R> {
    let source = resolve_layer(runtime, src).ok_or(LayerBitmapError::NotDrawable)?;
    let (source_bitmap, source_pixels) = plane_snapshot(&source.node)?;
    let Some(taken) = take_write_plane(runtime, dest) else {
        return Err(LayerBitmapError::NotDrawable.into());
    };
    let source_view = LayerBitmapView {
        bitmap: source_bitmap,
        pixels: &source_pixels,
    };
    Ok(run_write(runtime, taken, |dest_view| {
        run(&source_view, dest_view)
    }))
}

/// `Layer.update()`: set `callOnPaint` and post the window update — what a
/// ported plugin calls after mutating (`tTJSNI_BaseLayer::UpdateByScript`,
/// `LayerIntf.cpp:7638-7660`).
pub fn layer_update(runtime: &mut Runtime<KrkrHost>, layer: ObjectHandle) -> Result<()> {
    let layer = runtime.bound_this(layer).unwrap_or(layer);
    layer_update_by_script(runtime, layer)
}

/// Creates a canvas layer: a real native `Layer` a plugin can draw into and
/// publish from, invisible and detached until [`attach_canvas_layer`] hangs it
/// under the layer it stands in for.
///
/// `motionplayer.dll`'s `Motion.SeparateLayerAdaptor` is exactly such a
/// canvas.  The KAG motion layer constructs one per owner
/// (`system/AffineSourceMotion.tjs` `entryOwner`), hands it to
/// `Motion.Player.clear`/`draw` as the draw target, and then publishes it with
/// `Layer.assignImages` on the visible layer — so the object the plugin works
/// with must be a real layer (`instanceof "Layer"`, a `__nativeLayerId`, a
/// `LayerBitmap` to write).
///
/// The object is built the way `new Layer()` with no arguments is: the full
/// native method surface, class info `Layer`, no window and no parent, and the
/// constructor's 32×32 holder bitmap, so [`layer_bitmap_write`] has pixels to
/// lend immediately.  [`fit_canvas_layer`] sizes it to the layer whose content
/// it stands in for; [`attach_canvas_layer`] gives it the tree placement the
/// reference's adaptor has.
pub fn create_canvas_layer(runtime: &mut Runtime<KrkrHost>) -> Result<ObjectHandle> {
    let layer = construct_native_instance(runtime, &LAYER_CLASS, None, Vec::new())?;
    layer
        .object_handle()
        .ok_or_else(|| TjsError::runtime("canvas layer construction produced no object"))
}

/// Sizes a canvas layer to `source` — the layer whose content it stands in for
/// — matching both the layer rect and the main bitmap.
///
/// The adaptor is constructed with its owner layer, but the owner need not
/// have its final rect yet when that happens (`system/AffineLayer.tjs` runs
/// `entryOwner` before its `onResize`), so the caller re-runs this before each
/// write; once the sizes match it is a no-op.  A `source` that is not a layer
/// attached to a render node, or that has no positive size yet, leaves the
/// canvas untouched.
pub fn fit_canvas_layer(
    runtime: &mut Runtime<KrkrHost>,
    canvas: ObjectHandle,
    source: ObjectHandle,
) -> Result<()> {
    let Some((width, height)) = layer_draw_size(runtime, source) else {
        return Ok(());
    };
    if canvas_matches_size(runtime, canvas, width, height) {
        return Ok(());
    }
    set_layer_geographical_size(runtime, canvas, i64::from(width), i64::from(height))?;
    internal_set_layer_image_size(runtime, canvas, i64::from(width), i64::from(height))?;
    Ok(())
}

/// Hangs a canvas layer under `parent` as a visible child — the placement the
/// reference's `Motion.SeparateLayerAdaptor` has: the DLL builds one host
/// `Layer` per motion layer, parented to the adaptor's `targetLayer` at
/// creation and visible as soon as it has pixels, with `hitThreshold = 0x100`
/// so it never swallows a mouse hit (`motionplayer_nod3d.dll` `FUN_1000d280`).
/// A canvas whose owner is a `ltBinder` layer is drawn *through* the binder,
/// which itself never carries an image (`LayerIntf.cpp:5280-5281`, `:5912`,
/// `:5931`).
///
/// The game's own `entryOwner` is the evidence for the shape: right after
/// `new Motion.SeparateLayerAdaptor(owner incontextof global.Layer)` it rewrites
/// an `ltAlpha` owner to `ltBinder` (`system/AffineSourceMotion.tjs`) — the
/// owner stops drawing, so the adaptor must reach the screen on its own, which
/// only a visible child of that owner can do (the game's per-frame
/// `Layer.assignImages(owner, adaptor)` publish is undone by the type restore
/// at the end of `drawAffine`, which frees the owner's image again).
///
/// A `parent` that is not a layer attached to a render node is ignored: the
/// canvas stays detached, exactly as `create_canvas_layer` built it.
pub fn attach_canvas_layer(
    runtime: &mut Runtime<KrkrHost>,
    canvas: ObjectHandle,
    parent: ObjectHandle,
) -> Result<()> {
    let parent_is_layer = this_render_layer_target(runtime, Some(parent))
        .ok()
        .and_then(|(_, target)| target)
        .is_some();
    if !parent_is_layer {
        return Ok(());
    }
    join_layer_under_parent(runtime, canvas, parent)
}

/// The drawable size of a layer: its rect when non-empty, else its bitmap.
fn layer_draw_size(runtime: &mut Runtime<KrkrHost>, layer: ObjectHandle) -> Option<(u32, u32)> {
    let (_, target) = this_render_layer_target(runtime, Some(layer)).ok()?;
    let node = render_layer_snapshot(runtime, &target?)?;
    let (width, height) = if node.width > 0.0 && node.height > 0.0 {
        (node.width, node.height)
    } else {
        let image = node.image.as_ref()?;
        (image.upload.width as f32, image.upload.height as f32)
    };
    (width > 0.0 && height > 0.0).then(|| (width.round() as u32, height.round() as u32))
}

/// Whether a canvas already has the requested rect *and* bitmap.
fn canvas_matches_size(
    runtime: &mut Runtime<KrkrHost>,
    canvas: ObjectHandle,
    width: u32,
    height: u32,
) -> bool {
    let Ok((_, target)) = this_render_layer_target(runtime, Some(canvas)) else {
        return false;
    };
    let Some(node) = target.and_then(|target| render_layer_snapshot(runtime, &target)) else {
        return false;
    };
    let rect = (node.width.round(), node.height.round());
    let image = node
        .image
        .as_ref()
        .map(|image| (image.upload.width, image.upload.height));
    rect == (width as f32, height as f32) && image == Some((width, height))
}

/// One layer's province plane size, as handed to a plugin closure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LayerProvince {
    pub width: u32,
    pub height: u32,
}

/// A read-only view of a layer's province plane (one 8bpp index per pixel).
pub struct LayerProvinceView<'a> {
    pub province: LayerProvince,
    pub pixels: &'a [u8],
}

/// A writable view of a layer's province plane: mutate `pixels`, and the
/// engine commits the copy when the closure returns.
pub struct LayerProvinceViewMut<'a> {
    pub province: LayerProvince,
    pub pixels: &'a mut [u8],
}

/// Reads the layer's province plane.
///
/// A layer without a plane (never loaded, or deallocated) hands `read` an
/// empty view — `GetProvincePixel` answers 0 outside the plane and without one
/// (`LayerIntf.cpp:2637`), and an all-zero plane is the same representation —
/// so a plugin checks `province.width`/`height` instead of handling an error.
pub fn layer_province_read<R>(
    runtime: &mut Runtime<KrkrHost>,
    layer: ObjectHandle,
    read: impl FnOnce(&LayerProvinceView<'_>) -> R,
) -> Result<R> {
    let Some(resolved) = resolve_layer(runtime, layer) else {
        return Ok(read(&empty_province_view()));
    };
    match resolved.node.province.as_ref() {
        Some(province) => Ok(read(&LayerProvinceView {
            province: LayerProvince {
                width: province.width,
                height: province.height,
            },
            pixels: province.pixels.as_ref(),
        })),
        None => Ok(read(&empty_province_view())),
    }
}

/// Clones the layer's province plane, lends the copy to `write`, and commits.
///
/// When the layer has no plane, `allocate` mirrors `setProvincePixel`'s
/// `AllocateProvinceImage` path (`LayerIntf.cpp:2647-2663`): the plane is
/// created at the main image's size, or the layer Rect without one.  Without
/// `allocate` the closure gets an empty view and nothing is committed.  A
/// plane the closure leaves all zero is deallocated — `FillRect`'s `dfProvince`
/// branch does the same (`classes.rs` `fill_layer_province`), and an all-zero
/// plane reads identically to no plane.
///
/// An object that is not a layer attached to a render node (a non-`Layer`
/// object, or an already-invalidated layer) fails with
/// [`LayerBitmapError::NotDrawable`] even when `allocate` is false: there is no
/// plane to hand out and nowhere to commit one.  [`layer_province_read`] hands
/// that object an empty view instead, and the native `setProvincePixel` is a
/// silent no-op there.
pub fn layer_province_write<R>(
    runtime: &mut Runtime<KrkrHost>,
    layer: ObjectHandle,
    allocate: bool,
    write: impl FnOnce(&mut LayerProvinceViewMut<'_>) -> R,
) -> Result<R> {
    let resolved = resolve_layer(runtime, layer).ok_or(LayerBitmapError::NotDrawable)?;
    let plane = resolved
        .node
        .province
        .clone()
        .or_else(|| allocate.then(|| allocate_layer_province_plane(&resolved.node)));
    let Some(plane) = plane else {
        let mut view = LayerProvinceViewMut {
            province: LayerProvince {
                width: 0,
                height: 0,
            },
            pixels: &mut [],
        };
        return Ok(write(&mut view));
    };
    let province = LayerProvince {
        width: plane.width,
        height: plane.height,
    };
    let mut pixels = plane.pixels.as_ref().to_vec();
    let result = {
        let mut view = LayerProvinceViewMut {
            province,
            pixels: &mut pixels,
        };
        write(&mut view)
    };
    let committed = if pixels.iter().all(|&value| value == 0) {
        None
    } else {
        Some(ProvinceImage::new(province.width, province.height, pixels))
    };
    mutate_render_layer(runtime, &resolved.target, |layer| {
        layer.province = committed;
    });
    mark_image_modified(runtime, resolved.handle);
    Ok(result)
}

fn empty_province_view() -> LayerProvinceView<'static> {
    LayerProvinceView {
        province: LayerProvince {
            width: 0,
            height: 0,
        },
        pixels: &[],
    }
}

/// The layer a plugin-facing call acts on: the bound TJS object, its render
/// target, and a snapshot of the render node the call reads.
struct ResolvedLayer {
    handle: ObjectHandle,
    target: LayerRenderTarget,
    node: LayerNode,
}

/// Resolves the layer object the same way the natives do
/// (`this_render_layer_target` → `render_layer_snapshot`), so a plugin sees
/// exactly the bitmap `setMainPixel`/`copyRect` see.  `None` when the object
/// is not a layer attached to a render node.
fn resolve_layer(runtime: &mut Runtime<KrkrHost>, layer: ObjectHandle) -> Option<ResolvedLayer> {
    let (handle, target) = this_render_layer_target(runtime, Some(layer)).ok()?;
    let target = target?;
    let node = render_layer_snapshot(runtime, &target)?;
    Some(ResolvedLayer {
        handle,
        target,
        node,
    })
}

fn bitmap_metadata(layer: &LayerNode, image: &LayerImage) -> LayerBitmap {
    let width = image.upload.width;
    let height = image.upload.height;
    LayerBitmap {
        width,
        height,
        pitch: width.saturating_mul(4),
        generation: image.upload.texture_id,
        clip: match layer.clip {
            Some(clip) => (
                clip.x.round() as i64,
                clip.y.round() as i64,
                clip.width.round() as i64,
                clip.height.round() as i64,
            ),
            None => (0, 0, i64::from(width), i64::from(height)),
        },
        layer_type: layer.layer_type,
    }
}

/// The image size and a copy of the pixels a commit is based on.
fn plane_snapshot(node: &LayerNode) -> Result<(LayerBitmap, Vec<u8>)> {
    let image = node.image.as_ref().ok_or(LayerBitmapError::NotDrawable)?;
    Ok((
        bitmap_metadata(node, image),
        image.upload.rgba.as_ref().to_vec(),
    ))
}

/// How a write call obtained the buffer it lends to its closure.
///
/// The decision is the strong count of the layer image's pixel buffer at the
/// moment the write looks at it; exactly one of these is chosen per call.
enum WritePlane {
    /// Nothing else holds the buffer.  The image is lifted out of the node, the
    /// closure writes the live bytes, and the commit installs the same
    /// allocation back under a freshly minted texture id.  This is what keeps a
    /// per-frame `Player.clear`/`Player.draw` sequence free of full-canvas
    /// copies: the first commit of a frame publishes a buffer nothing else has
    /// seen yet, so the later writes of that frame are its sole owner.
    Exclusive(LayerImage),
    /// Something else holds the buffer: the host's upload record for its
    /// current texture id (the last frame that published it), a frozen
    /// transition face, another layer sharing the image through
    /// `Layer.assignImages`, or a caller's own snapshot.  The closure writes a
    /// private clone (`Arc::make_mut`) and the live bytes change only when the
    /// commit lands, which is the copy-on-write contract the old unconditional
    /// copy gave every call.
    Copied(LayerImage),
}

/// A layer's plane, lifted out of its render node for one write call.
struct TakenPlane {
    handle: ObjectHandle,
    target: LayerRenderTarget,
    bitmap: LayerBitmap,
    plane: WritePlane,
}

/// Lifts the layer's plane out of its node for a write call.
///
/// The metadata (`width`, `height`, `clip`, `layer_type`, `generation`) and the
/// ownership decision come from the same live node the commit writes back to,
/// so a plugin sees exactly the bitmap the native pixel setters see.
fn take_write_plane(runtime: &mut Runtime<KrkrHost>, layer: ObjectHandle) -> Option<TakenPlane> {
    let (handle, target) = this_render_layer_target(runtime, Some(layer)).ok()?;
    let target = target?;
    let (bitmap, plane) = mutate_render_layer(runtime, &target, |node| {
        let image = node.image.as_ref()?;
        let bitmap = bitmap_metadata(node, image);
        let exclusive = Arc::strong_count(&image.upload.rgba) == 1;
        let plane = if exclusive {
            WritePlane::Exclusive(node.image.take().expect("the image is present"))
        } else {
            WritePlane::Copied(image.clone())
        };
        Some((bitmap, plane))
    })??;
    Some(TakenPlane {
        handle,
        target,
        bitmap,
        plane,
    })
}

/// Takes the layer's image when this call holds the only reference to its
/// pixel buffer, so a caller that replaces the whole plane can write the live
/// bytes in place.
///
/// `None` when another holder exists — the host's upload record for the
/// image's texture id, a frozen transition face, a layer sharing the image
/// through `Layer.assignImages`, a snapshot a caller owns — or when the layer
/// has no image at all; a caller that replaces every pixel then builds a fresh
/// image, exactly like the old unconditional copy did.
pub(crate) fn take_unique_plane(
    runtime: &mut Runtime<KrkrHost>,
    target: &LayerRenderTarget,
) -> Option<LayerImage> {
    mutate_render_layer(runtime, target, |node| {
        let image = node.image.as_ref()?;
        if Arc::strong_count(&image.upload.rgba) != 1 {
            return None;
        }
        node.image.take()
    })?
}

/// Runs the write closure over the taken plane, mints the image's fresh texture
/// id, and commits the result to the layer.
///
/// `Exclusive` writes the layer's live allocation (the bytes are already there,
/// so nothing is copied); `Copied` writes the private clone the take made.  In
/// both cases the closure's view is the only way to reach the buffer, and the
/// commit replaces the layer's image under a new texture id so the upload
/// pipeline republishes it.
fn run_write<R>(
    runtime: &mut Runtime<KrkrHost>,
    taken: TakenPlane,
    write: impl FnOnce(&mut LayerBitmapViewMut<'_>) -> R,
) -> R {
    let TakenPlane {
        handle,
        target,
        bitmap,
        plane,
    } = taken;
    match plane {
        WritePlane::Exclusive(image) => {
            // The image is out of the node, so a panic cannot leave the layer
            // without its bitmap; the guard puts it back on the way out.
            let mut restore = RestorePlane {
                runtime: &mut *runtime,
                target: target.clone(),
                image: Some(image),
            };
            let result = {
                let image = restore.image.as_mut().expect("the plane is present");
                let pixels = Arc::make_mut(&mut image.upload.rgba);
                let mut view = LayerBitmapViewMut { bitmap, pixels };
                write(&mut view)
            };
            let mut image = restore.image.take().expect("the plane is present");
            drop(restore);
            image.upload.texture_id = runtime.host_mut().allocate_video_texture_id();
            commit_plane(runtime, handle, &target, bitmap, image);
            result
        }
        WritePlane::Copied(mut image) => {
            let result = {
                let pixels = Arc::make_mut(&mut image.upload.rgba);
                let mut view = LayerBitmapViewMut { bitmap, pixels };
                write(&mut view)
            };
            image.upload.texture_id = runtime.host_mut().allocate_video_texture_id();
            commit_plane(runtime, handle, &target, bitmap, image);
            result
        }
    }
}

/// Puts an exclusively taken plane back into its layer if the write closure
/// unwinds, so a panicking plugin never leaves the layer without its bitmap.
///
/// The bytes written before the unwind stay in the buffer — the reference hands
/// plugins the live bitmap (`Layer.mainImageBufferForWrite`), so a crashing
/// plugin leaves partial writes there too — but the texture id does not change
/// and the commit never runs.  A `Copied` plane needs no guard: its clone is
/// simply dropped and the layer was never touched.
struct RestorePlane<'a> {
    runtime: &'a mut Runtime<KrkrHost>,
    target: LayerRenderTarget,
    image: Option<LayerImage>,
}

impl Drop for RestorePlane<'_> {
    fn drop(&mut self) {
        let Some(image) = self.image.take() else {
            return;
        };
        mutate_render_layer(self.runtime, &self.target, |node| {
            node.image = Some(image);
        });
    }
}

/// Installs the written plane as the layer's image, the way
/// `mutate_layer_pixels_min_with_host` does.  `imageWidth`/`imageHeight` keep
/// their values — they describe the same bitmap the plugin saw — and are only
/// filled when the layer never had them.
fn commit_plane(
    runtime: &mut Runtime<KrkrHost>,
    handle: ObjectHandle,
    target: &LayerRenderTarget,
    bitmap: LayerBitmap,
    image: LayerImage,
) {
    mutate_render_layer(runtime, target, |layer| {
        layer.image = Some(image);
        if layer.image_width == 0.0 {
            layer.image_width = bitmap.width as f32;
        }
        if layer.image_height == 0.0 {
            layer.image_height = bitmap.height as f32;
        }
        if layer.width <= 0.0 {
            layer.width = bitmap.width as f32;
        }
        if layer.height <= 0.0 {
            layer.height = bitmap.height as f32;
        }
    });
    mark_image_modified(runtime, handle);
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    use krkr_core::{FrameInput, Size};
    use krkr_tjs2::runtime::Variant;

    use crate::{EngineConfig, EngineInput, KrkrEngine};

    use super::*;

    /// A visible, parentless 4×4 layer filled with opaque red: the smallest
    /// drawable layer a plugin can be handed.
    const FILLED_LAYER: &str = r#"
        global.layer = new Layer();
        layer.setImageSize(4, 4);
        layer.fillRect(0, 0, 4, 4, 0xffff0000);
        layer.visible = true;
    "#;

    fn layer_handle(engine: &KrkrEngine, name: &str) -> ObjectHandle {
        engine
            .tjs_runtime()
            .global_member(name)
            .object_handle()
            .unwrap_or_else(|| panic!("{name} missing"))
    }

    fn engine() -> KrkrEngine {
        KrkrEngine::new(EngineConfig::default()).expect("engine")
    }

    fn generation(engine: &mut KrkrEngine, layer: ObjectHandle) -> u64 {
        layer_bitmap_read(engine.tjs_runtime_mut(), layer, |view| {
            view.bitmap.generation
        })
        .expect("read generation")
    }

    /// Byte offset of pixel `(x, y)` in a tightly packed 4-byte RGBA plane.
    fn offset(x: usize, y: usize, width: usize) -> usize {
        (y * width + x) * 4
    }

    #[test]
    fn bitmap_read_sees_the_loaded_pixels_metadata_and_clip() {
        let mut engine = engine();
        engine
            .execute_script("inline.tjs", FILLED_LAYER)
            .expect("script");
        let layer = layer_handle(&engine, "layer");
        layer_bitmap_read(engine.tjs_runtime_mut(), layer, |view| {
            assert_eq!(view.bitmap.width, 4);
            assert_eq!(view.bitmap.height, 4);
            assert_eq!(view.bitmap.pitch, 16, "tightly packed today");
            assert!(
                view.bitmap.generation > 0,
                "a texture id identifies the image"
            );
            assert_eq!(
                view.bitmap.clip,
                (0, 0, 4, 4),
                "unset clip covers the image"
            );
            // `new Layer()` without a window/parent is not primary, so it keeps
            // the ctor's `ltAlpha` type.
            assert_eq!(view.bitmap.layer_type, 2);
            assert_eq!(view.pixels.len(), 4 * 4 * 4);
            // `fillRect(0xffff0000)` writes the engine's RGBA order straight:
            // R,G,B,A — byte 2 is blue, not the reference's byte 0.
            assert_eq!(&view.pixels[..4], [255, 0, 0, 255]);
        })
        .expect("read");

        engine
            .execute_script("clip.tjs", "layer.setClip(1, 1, 2, 2);")
            .expect("clip");
        layer_bitmap_read(engine.tjs_runtime_mut(), layer, |view| {
            assert_eq!(view.bitmap.clip, (1, 1, 2, 2));
        })
        .expect("read clip");
    }

    #[test]
    fn bitmap_write_commits_pixels_the_script_and_the_renderer_see() {
        let mut engine = engine();
        engine
            .execute_script("inline.tjs", FILLED_LAYER)
            .expect("script");
        let layer = layer_handle(&engine, "layer");
        let before = generation(&mut engine, layer);

        layer_bitmap_write(engine.tjs_runtime_mut(), layer, |view| {
            assert_eq!(view.bitmap.width, 4);
            let index = offset(1, 1, 4);
            view.pixels[index..index + 4].copy_from_slice(&[0, 0, 255, 255]);
        })
        .expect("write");

        assert!(
            !engine.host().has_pending_window_update(layer),
            "the commit itself does not post a repaint; `layer_update` is the explicit step"
        );
        let after = generation(&mut engine, layer);
        assert_ne!(before, after, "the commit replaces the image");
        layer_bitmap_read(engine.tjs_runtime_mut(), layer, |view| {
            let index = offset(1, 1, 4);
            assert_eq!(&view.pixels[index..index + 4], [0, 0, 255, 255]);
        })
        .expect("read back");
        // The engine's own accessors read the committed bytes.
        assert_eq!(
            engine
                .execute_expression("read.tjs", "layer.getMainPixel(1, 1)")
                .expect("main pixel"),
            Variant::Integer(0x0000ff)
        );
        assert_eq!(
            engine
                .execute_expression("read.tjs", "layer.getMaskPixel(1, 1)")
                .expect("mask pixel"),
            Variant::Integer(0xff)
        );

        // And the frame output re-uploads the replacement texture with the new
        // pixels: the renderer reads texture ids, so the commit has to allocate
        // one.
        let frame = engine
            .update(
                EngineInput::new(FrameInput::new(Size::new(320.0, 240.0), 0.0), Vec::new()),
                Duration::ZERO,
            )
            .expect("frame");
        let upload = frame
            .output
            .image_uploads
            .iter()
            .find(|upload| upload.texture_id == after)
            .expect("committed image reaches the frame output");
        let index = offset(1, 1, 4);
        assert_eq!(&upload.rgba[index..index + 4], [0, 0, 255, 255]);
    }

    /// The render node id `__nativeLayerId` reports.
    fn native_layer_id(engine: &mut KrkrEngine, name: &str) -> u64 {
        engine
            .execute_expression("id.tjs", &format!("{name}.__nativeLayerId"))
            .expect("layer id")
            .to_integer()
            .expect("integer") as u64
    }

    /// The image a holder keeps alive, cloned out of the render tree the way a
    /// frozen transition face or the frame pipeline's upload record does.
    fn held_image(engine: &KrkrEngine, layer_id: u64) -> LayerImage {
        engine
            .host()
            .layer_tree()
            .layer(layer_id)
            .expect("layer node")
            .image
            .clone()
            .expect("layer image")
    }

    /// The address of the bytes a plugin view is handed.
    fn pixel_pointer(engine: &mut KrkrEngine, layer: ObjectHandle) -> usize {
        layer_bitmap_read(engine.tjs_runtime_mut(), layer, |view| {
            view.pixels.as_ptr() as usize
        })
        .expect("read pointer")
    }

    #[test]
    fn bitmap_write_edits_a_uniquely_owned_buffer_in_place() {
        let mut engine = engine();
        engine
            .execute_script("inline.tjs", FILLED_LAYER)
            .expect("script");
        let layer = layer_handle(&engine, "layer");
        let before_generation = generation(&mut engine, layer);
        let before_pointer = pixel_pointer(&mut engine, layer);

        layer_bitmap_write(engine.tjs_runtime_mut(), layer, |view| {
            assert_eq!(
                view.pixels.as_ptr() as usize,
                before_pointer,
                "nothing else holds the buffer, so the closure gets the layer's own bytes"
            );
            view.pixels[offset(1, 1, 4)] = 0x2a;
        })
        .expect("write");

        assert_ne!(
            generation(&mut engine, layer),
            before_generation,
            "the commit still mints a fresh texture id, so the upload pipeline republishes"
        );
        assert_eq!(
            pixel_pointer(&mut engine, layer),
            before_pointer,
            "the commit keeps the same allocation: a full-canvas copy is elided"
        );
        layer_bitmap_read(engine.tjs_runtime_mut(), layer, |view| {
            assert_eq!(view.pixels[offset(1, 1, 4)], 0x2a);
        })
        .expect("read back");
    }

    #[test]
    fn bitmap_write_copies_when_a_holder_shares_the_buffer() {
        let mut engine = engine();
        engine
            .execute_script("inline.tjs", FILLED_LAYER)
            .expect("script");
        let layer = layer_handle(&engine, "layer");
        let layer_id = native_layer_id(&mut engine, "layer");
        // The holder covers both shapes the frame pipeline produces: a frozen
        // transition face and the upload record of the frame that published
        // the image.  Both are `Arc<[u8]>` clones of the live buffer.
        let held = held_image(&engine, layer_id);
        let held_bytes = held.upload.rgba.to_vec();
        let held_texture_id = held.upload.texture_id;
        let before_pointer = pixel_pointer(&mut engine, layer);

        layer_bitmap_write(engine.tjs_runtime_mut(), layer, |view| {
            assert_ne!(
                view.pixels.as_ptr() as usize,
                before_pointer,
                "a shared buffer is written through a private copy"
            );
            view.pixels[offset(1, 1, 4)] = 0x2a;
        })
        .expect("write");

        assert_eq!(
            held.upload.rgba.as_ref(),
            held_bytes.as_slice(),
            "the holder's bytes are untouched"
        );
        assert_eq!(
            held.upload.texture_id, held_texture_id,
            "the holder's own image keeps its texture id"
        );
        assert_ne!(
            pixel_pointer(&mut engine, layer),
            before_pointer,
            "the layer moved to a fresh allocation"
        );
        assert_ne!(generation(&mut engine, layer), held_texture_id);
        let committed = held_image(&engine, layer_id);
        assert_eq!(committed.upload.rgba[offset(1, 1, 4)], 0x2a);
    }

    #[test]
    fn a_later_write_in_the_same_frame_edits_the_buffer_the_first_one_committed() {
        // The shape of the per-frame `Motion.Player.clear` / `draw` sequence:
        // the previous frame's published buffer is still held, so the clear
        // writes a private copy; the buffer it commits is unshared, so the
        // draw that follows in the same frame writes it in place.  Two calls,
        // one full-canvas copy.
        let mut engine = engine();
        engine
            .execute_script("inline.tjs", FILLED_LAYER)
            .expect("script");
        let layer = layer_handle(&engine, "layer");
        let layer_id = native_layer_id(&mut engine, "layer");
        // The holder covers both shapes the frame pipeline produces: a frozen
        // transition face and the upload record of the frame that published
        // the image.  Both are `Arc<[u8]>` clones of the live buffer.
        let published = held_image(&engine, layer_id);
        let published_bytes = published.upload.rgba.to_vec();

        layer_bitmap_write(engine.tjs_runtime_mut(), layer, |view| {
            view.pixels.fill(0x11);
        })
        .expect("clear");
        let cleared_pointer = pixel_pointer(&mut engine, layer);
        assert_ne!(
            cleared_pointer,
            published.upload.rgba.as_ptr() as usize,
            "the clear wrote a private copy"
        );

        layer_bitmap_write(engine.tjs_runtime_mut(), layer, |view| {
            assert_eq!(
                view.pixels.as_ptr() as usize,
                cleared_pointer,
                "the draw writes the buffer the clear committed"
            );
            view.pixels[0] = 0x22;
        })
        .expect("draw");

        assert_eq!(
            published.upload.rgba.as_ref(),
            published_bytes.as_slice(),
            "the buffer the earlier frame published is never mutated"
        );
        assert_eq!(
            pixel_pointer(&mut engine, layer),
            cleared_pointer,
            "the second write did not copy"
        );
    }

    #[test]
    fn read_write_leaves_a_layer_that_shares_the_destination_buffer_untouched() {
        // `Layer.assignImages` points the destination at the source's bitmap
        // (both nodes hold one `Arc<[u8]>`, `classes.rs` `copy_layer_images`),
        // so a destination write must not reach the bytes the source layer
        // still shows.  The sizes match, which is what makes the assignment
        // share the image instead of resizing a copy into the destination.
        let mut engine = engine();
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.src = new Layer();
                src.setImageSize(2, 1);
                src.fillRect(0, 0, 2, 1, 0xffff0000);
                src.fillRect(1, 0, 1, 1, 0xff0000ff);

                global.dest = new Layer();
                dest.setImageSize(2, 1);
                dest.assignImages(src);
                "#,
            )
            .expect("script");
        let src = layer_handle(&engine, "src");
        let dest = layer_handle(&engine, "dest");
        let src_id = native_layer_id(&mut engine, "src");
        let dest_id = native_layer_id(&mut engine, "dest");
        let shared = held_image(&engine, src_id);
        let shared_bytes = shared.upload.rgba.to_vec();
        assert!(
            Arc::ptr_eq(
                &shared.upload.rgba,
                &held_image(&engine, dest_id).upload.rgba
            ),
            "assignImages shares one buffer between the two layers"
        );

        layer_bitmap_read_write(engine.tjs_runtime_mut(), src, dest, |source, dest_view| {
            dest_view.pixels.copy_from_slice(source.pixels);
        })
        .expect("read_write");

        assert_eq!(
            shared.upload.rgba.as_ref(),
            shared_bytes.as_slice(),
            "a destination that shares the source's buffer must copy, not write it"
        );
        assert_eq!(
            engine
                .execute_expression("read.tjs", "src.getMainPixel(1, 0)")
                .expect("source pixel"),
            Variant::Integer(0x0000ff),
            "the source layer keeps its pixels"
        );
        assert_eq!(
            engine
                .execute_expression("read.tjs", "dest.getMainPixel(0, 0)")
                .expect("destination pixel"),
            Variant::Integer(0xff0000),
            "the destination committed the source's bytes"
        );
    }

    #[test]
    fn read_write_copies_between_layers_and_supports_src_equal_dest() {
        let mut engine = engine();
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.src = new Layer();
                src.setImageSize(2, 1);
                src.fillRect(0, 0, 2, 1, 0xffff0000);
                src.fillRect(1, 0, 1, 1, 0xff0000ff);

                global.dest = new Layer();
                dest.setImageSize(2, 1);
                dest.fillRect(0, 0, 2, 1, 0xff00ff00);
                "#,
            )
            .expect("script");
        let src = layer_handle(&engine, "src");
        let dest = layer_handle(&engine, "dest");

        layer_bitmap_read_write(engine.tjs_runtime_mut(), src, dest, |source, dest_view| {
            assert_eq!(source.bitmap.width, 2);
            assert_eq!(dest_view.bitmap.width, 2);
            dest_view.pixels.copy_from_slice(source.pixels);
        })
        .expect("read_write");
        assert_eq!(
            engine
                .execute_expression("read.tjs", "dest.getMainPixel(0, 0)")
                .expect("pixel"),
            Variant::Integer(0xff0000)
        );
        assert_eq!(
            engine
                .execute_expression("read.tjs", "dest.getMainPixel(1, 0)")
                .expect("pixel"),
            Variant::Integer(0x0000ff)
        );
        assert_eq!(
            engine
                .execute_expression("read.tjs", "src.getMainPixel(1, 0)")
                .expect("pixel"),
            Variant::Integer(0x0000ff),
            "the source is never written"
        );

        // `src == dest` is an in-place operation over the call's snapshot: a
        // swap would collapse both pixels if the view read the live buffer.
        layer_bitmap_read_write(engine.tjs_runtime_mut(), dest, dest, |source, view| {
            let last = source.pixels.len() - 4;
            let first = source.pixels[..4].to_vec();
            view.pixels[..4].copy_from_slice(&source.pixels[last..last + 4]);
            view.pixels[last..].copy_from_slice(&first);
        })
        .expect("in place");
        assert_eq!(
            engine
                .execute_expression("read.tjs", "dest.getMainPixel(0, 0)")
                .expect("pixel"),
            Variant::Integer(0x0000ff)
        );
        assert_eq!(
            engine
                .execute_expression("read.tjs", "dest.getMainPixel(1, 0)")
                .expect("pixel"),
            Variant::Integer(0xff0000)
        );

        // Mismatched sizes are legal and each side reports its own bitmap: a
        // resampling port (`layerExAreaAverage`) reads the source at its size
        // and writes the destination at its own.
        engine
            .execute_script("resize.tjs", "dest.setImageSize(1, 1);")
            .expect("resize");
        layer_bitmap_read_write(engine.tjs_runtime_mut(), src, dest, |source, view| {
            assert_eq!((source.bitmap.width, source.bitmap.height), (2, 1));
            assert_eq!((view.bitmap.width, view.bitmap.height), (1, 1));
            view.pixels.copy_from_slice(&source.pixels[..4]);
        })
        .expect("mismatched sizes");
        assert_eq!(
            engine
                .execute_expression("read.tjs", "dest.getMainPixel(0, 0)")
                .expect("pixel"),
            Variant::Integer(0xff0000)
        );
    }

    #[test]
    fn freed_image_reports_not_drawable_without_resurrection() {
        let mut engine = engine();
        engine
            .execute_script("inline.tjs", FILLED_LAYER)
            .expect("script");
        let layer = layer_handle(&engine, "layer");
        let before = generation(&mut engine, layer);
        assert!(before > 0, "the filled layer owns a texture");
        engine
            .execute_script("free.tjs", "layer.freeImage();")
            .expect("freeImage");

        let error = layer_bitmap_write(engine.tjs_runtime_mut(), layer, |view| {
            view.pixels[0] = 1;
        })
        .expect_err("a freed bitmap refuses the write");
        assert_eq!(error.kind, krkr_tjs2::TjsErrorKind::Runtime);
        assert_eq!(error.message, "Not drawable layer type");
        let error = layer_bitmap_read(engine.tjs_runtime_mut(), layer, |_| ())
            .expect_err("a freed bitmap refuses the read");
        assert_eq!(error.message, "Not drawable layer type");
        assert_eq!(
            engine
                .execute_expression("read.tjs", "layer.hasImage")
                .expect("hasImage"),
            Variant::Integer(0),
            "the failed write must not resurrect the freed image"
        );
    }

    #[test]
    fn province_views_allocate_write_read_and_deallocate() {
        let mut engine = engine();
        engine
            .execute_script("inline.tjs", FILLED_LAYER)
            .expect("script");
        let layer = layer_handle(&engine, "layer");

        // No plane yet: an empty view, the same representation an all-zero
        // plane has (`getProvincePixel` answers 0 there).
        layer_province_read(engine.tjs_runtime_mut(), layer, |view| {
            assert_eq!((view.province.width, view.province.height), (0, 0));
            assert!(view.pixels.is_empty());
        })
        .expect("empty read");

        layer_province_write(engine.tjs_runtime_mut(), layer, true, |view| {
            assert_eq!(
                (view.province.width, view.province.height),
                (4, 4),
                "the plane is allocated at the main image's size"
            );
            view.pixels[0] = 7;
            view.pixels[view.province.width as usize + 1] = 9;
        })
        .expect("allocate and write");
        assert_eq!(
            engine
                .execute_expression("read.tjs", "layer.getProvincePixel(0, 0)")
                .expect("province pixel"),
            Variant::Integer(7)
        );
        assert_eq!(
            engine
                .execute_expression("read.tjs", "layer.getProvincePixel(1, 1)")
                .expect("province pixel"),
            Variant::Integer(9)
        );
        layer_province_read(engine.tjs_runtime_mut(), layer, |view| {
            assert_eq!(view.pixels.len(), 16);
            assert_eq!(view.pixels[0], 7);
            assert_eq!(view.pixels[5], 9);
        })
        .expect("read back");

        // `FillRect` with `dfProvince` and a whole-plane zero fill deallocates
        // the plane (`classes.rs` `fill_layer_province`): no plane, an
        // all-zero plane and `getProvincePixel` == 0 are one state.
        let layer_id = engine
            .execute_expression("id.tjs", "layer.__nativeLayerId")
            .expect("layer id")
            .to_integer()
            .expect("integer") as u64;
        engine
            .execute_script(
                "clear.tjs",
                "layer.face = 3; layer.fillRect(0, 0, 4, 4, 0);",
            )
            .expect("clear province");
        assert!(
            engine
                .host()
                .layer_tree()
                .layer(layer_id)
                .expect("layer node")
                .province
                .is_none()
        );

        // Without `allocate` a layer without a plane gets an empty view and
        // nothing is committed.
        layer_province_write(engine.tjs_runtime_mut(), layer, false, |view| {
            assert!(view.pixels.is_empty());
        })
        .expect("no allocate");
        assert!(
            engine
                .host()
                .layer_tree()
                .layer(layer_id)
                .expect("layer node")
                .province
                .is_none()
        );

        // `allocate` re-creates it at the main image's size…
        layer_province_write(engine.tjs_runtime_mut(), layer, true, |view| {
            assert_eq!(view.pixels.len(), 16);
            view.pixels[0] = 3;
        })
        .expect("allocate");
        // …and a write that leaves it all zero deallocates it again.
        layer_province_write(engine.tjs_runtime_mut(), layer, false, |view| {
            assert_eq!(view.pixels.len(), 16, "the allocated plane is back");
            view.pixels.fill(0);
        })
        .expect("clear");
        assert!(
            engine
                .host()
                .layer_tree()
                .layer(layer_id)
                .expect("layer node")
                .province
                .is_none(),
            "an all-zero plane is the same state as no plane"
        );
        layer_province_read(engine.tjs_runtime_mut(), layer, |view| {
            assert!(view.pixels.is_empty());
        })
        .expect("empty again");
    }

    #[test]
    fn layer_update_posts_the_repaint_the_family_contract_expects() {
        let mut engine = engine();
        engine
            .execute_script("inline.tjs", FILLED_LAYER)
            .expect("script");
        let layer = layer_handle(&engine, "layer");
        layer_update(engine.tjs_runtime_mut(), layer).expect("update");
        assert_eq!(
            engine
                .execute_expression("read.tjs", "layer.callOnPaint")
                .expect("callOnPaint"),
            Variant::Integer(1)
        );
        assert!(engine.host().has_pending_window_update(layer));
    }

    #[test]
    fn a_panicking_write_closure_leaves_the_commit_undone() {
        let mut engine = engine();
        engine
            .execute_script("inline.tjs", FILLED_LAYER)
            .expect("script");
        let layer = layer_handle(&engine, "layer");
        let before = generation(&mut engine, layer);

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = layer_bitmap_write(engine.tjs_runtime_mut(), layer, |view| {
                view.pixels[0] = 7;
                panic!("plugin panic");
            });
        }));
        assert!(outcome.is_err(), "the panic propagates to the caller");

        assert_eq!(
            generation(&mut engine, layer),
            before,
            "the commit never runs, so the texture id is untouched"
        );
        // The exclusively taken bitmap is put back, with the bytes the closure
        // wrote before unwinding still in it: the reference hands plugins the
        // live buffer (`Layer.mainImageBufferForWrite`), so a crashing plugin
        // leaves partial writes there too.
        layer_bitmap_read(engine.tjs_runtime_mut(), layer, |view| {
            assert_eq!(&view.pixels[..4], [7, 0, 0, 255]);
        })
        .expect("the layer keeps its bitmap");
        assert_eq!(
            engine
                .execute_expression("read.tjs", "layer.getMainPixel(0, 0)")
                .expect("pixel"),
            Variant::Integer(0x070000)
        );
    }

    #[test]
    fn layer_extensions_are_typed_per_layer_and_pruned_on_invalidate() {
        let mut engine = engine();
        engine
            .execute_script(
                "inline.tjs",
                r#"
                global.first = new Layer();
                global.second = new Layer();
                "#,
            )
            .expect("script");
        let first = layer_handle(&engine, "first");
        let second = layer_handle(&engine, "second");

        let host = engine.host_mut();
        let state = host.layer_extension_or_insert_with(first, || Mutex::new(1u32));
        let same = host.layer_extension_or_insert_with(first, || Mutex::new(2u32));
        assert!(Arc::ptr_eq(&state, &same), "the existing slot is reused");
        assert_eq!(
            *same.lock().expect("lock"),
            1,
            "the initializer did not run again"
        );

        let other = host.layer_extension_or_insert_with(second, || Mutex::new(3u32));
        assert!(
            !Arc::ptr_eq(&state, &other),
            "a second layer is independent"
        );
        assert_eq!(*other.lock().expect("lock"), 3);
        assert!(Arc::ptr_eq(
            &host
                .layer_extension::<Mutex<u32>>(second)
                .expect("second slot"),
            &other
        ));

        let label = host.layer_extension_or_insert_with(first, || String::from("first"));
        assert_eq!(
            host.layer_extension::<String>(first)
                .as_deref()
                .map(String::as_str),
            Some(label.as_str())
        );
        assert!(
            host.layer_extension::<u64>(first).is_none(),
            "a type without a slot stays absent"
        );
        assert!(host.remove_layer_extension::<Mutex<u32>>(second).is_some());
        assert!(host.layer_extension::<Mutex<u32>>(second).is_none());
        host.layer_extension_or_insert_with(second, || Mutex::new(3u32));

        let weak = Arc::downgrade(&state);
        engine
            .execute_script("invalidate.tjs", "invalidate first;")
            .expect("invalidate");

        let host = engine.host();
        assert!(
            host.layer_extension::<Mutex<u32>>(first).is_none(),
            "the invalidated layer drops its state"
        );
        assert!(host.layer_extension::<String>(first).is_none());
        assert_eq!(
            *host
                .layer_extension::<Mutex<u32>>(second)
                .expect("the second layer keeps its state")
                .lock()
                .expect("lock"),
            3
        );

        drop(state);
        drop(same);
        drop(label);
        drop(other);
        assert!(
            weak.upgrade().is_none(),
            "the pruned state is actually dropped"
        );
    }
}
