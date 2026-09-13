//! Layer bitmap access for plugins — Part B §B.3 of
//! `docs/plugins/plugin-facing-engine-facilities.md`.
//!
//! The `layerEx*` family and pixel consumers such as `GlitchEffect` work by
//! read-modify-writing a layer's bitmap, which the reference exposes as
//! `Layer.mainImageBuffer`/`mainImageBufferForWrite` — a raw pointer cast to a
//! TJS integer (`LayerIntf.cpp:9513-9553`).  A memory-safe engine cannot hand
//! that out, so this module hands out **scoped, typed views with an explicit
//! commit** instead: the plugin supplies a closure, the engine clones the
//! plane, lends `&mut [u8]` for the closure's duration, and commits afterwards.
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
//! like any other host closure; the engine's private copy is discarded, so the
//! commit is skipped (§B.5.6).  The engine does not catch-unwind.
//!
//! **Registrations**: this facility adds no TJS members.  `mainImageBuffer*`
//! and the province trio stay registered, read-only and returning `void`
//! (§B.4), and a ported plugin attaches its own members to the global `Layer`
//! class exactly like the existing stubs do
//! (`crates/krkr-plugins/src/layer_ex_draw.rs`).

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

/// Clones the layer's main bitmap, lends the copy to `write`, and commits it.
///
/// The commit replaces the layer's image (a fresh `create_layer_image`, so
/// [`LayerBitmap::generation`] changes) and marks the layer modified exactly
/// like the native pixel setters (`classes.rs` `mutate_layer_pixels_min_with_host`).
/// It does **not** run `Layer.update`: the family contract is "mutate, then
/// `update()`" (`layerExBase.hpp:99-103`), so the plugin calls [`layer_update`]
/// itself when the repaint is due.
pub fn layer_bitmap_write<R>(
    runtime: &mut Runtime<KrkrHost>,
    layer: ObjectHandle,
    write: impl FnOnce(&mut LayerBitmapViewMut<'_>) -> R,
) -> Result<R> {
    let resolved = resolve_layer(runtime, layer).ok_or(LayerBitmapError::NotDrawable)?;
    let (bitmap, mut pixels) = plane_snapshot(&resolved.node)?;
    let result = {
        let mut view = LayerBitmapViewMut {
            bitmap,
            pixels: &mut pixels,
        };
        write(&mut view)
    };
    commit_bitmap(runtime, &resolved, bitmap, pixels);
    Ok(result)
}

/// A source snapshot plus a destination write, in one call.
///
/// `src == dest` is legal and behaves as an in-place operation over a snapshot
/// of the bytes as they were when the call started (the reference shifts rows
/// inside one buffer, `layerExRaster/main.cpp:38-97`).  Only the destination
/// commits; the source is never written.  Both layers must have a main image.
pub fn layer_bitmap_read_write<R>(
    runtime: &mut Runtime<KrkrHost>,
    src: ObjectHandle,
    dest: ObjectHandle,
    run: impl FnOnce(&LayerBitmapView<'_>, &mut LayerBitmapViewMut<'_>) -> R,
) -> Result<R> {
    let source = resolve_layer(runtime, src).ok_or(LayerBitmapError::NotDrawable)?;
    let (source_bitmap, source_pixels) = plane_snapshot(&source.node)?;
    let dest_resolved = resolve_layer(runtime, dest).ok_or(LayerBitmapError::NotDrawable)?;
    let (dest_bitmap, mut dest_pixels) = plane_snapshot(&dest_resolved.node)?;
    let result = {
        let source_view = LayerBitmapView {
            bitmap: source_bitmap,
            pixels: &source_pixels,
        };
        let mut dest_view = LayerBitmapViewMut {
            bitmap: dest_bitmap,
            pixels: &mut dest_pixels,
        };
        run(&source_view, &mut dest_view)
    };
    commit_bitmap(runtime, &dest_resolved, dest_bitmap, dest_pixels);
    Ok(result)
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

/// Replaces the layer's image with the closure's result, the way
/// `mutate_layer_pixels_min_with_host` does.  `imageWidth`/`imageHeight` keep
/// their values — they describe the same bitmap the plugin saw — and are only
/// filled when the layer never had them.
fn commit_bitmap(
    runtime: &mut Runtime<KrkrHost>,
    resolved: &ResolvedLayer,
    bitmap: LayerBitmap,
    pixels: Vec<u8>,
) {
    let image = runtime
        .host_mut()
        .create_layer_image(bitmap.width, bitmap.height, pixels);
    mutate_render_layer(runtime, &resolved.target, |layer| {
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
    mark_image_modified(runtime, resolved.handle);
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
            "the engine's private copy is discarded"
        );
        assert_eq!(
            engine
                .execute_expression("read.tjs", "layer.getMainPixel(0, 0)")
                .expect("pixel"),
            Variant::Integer(0xff0000)
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
