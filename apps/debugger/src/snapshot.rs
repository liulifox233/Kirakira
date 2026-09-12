//! Software frame compositing, PNG encoding, and pixel statistics for
//! headless screenshot/pixel probing (no GPU required).

use std::{collections::HashMap, sync::Arc};

use krkr_core::{
    Color, DrawCommand, FrameOutput, FrameTransition, ImageUpload, Rect, TransitionMethod,
};
use krkr_engine::KrkrEngine;

use crate::transition::{self, Face, Faces, Uniforms};

pub type TextureCache = HashMap<u64, (u32, u32, Arc<[u8]>)>;

pub fn composite_frame(
    width: u32,
    height: u32,
    commands: &[DrawCommand],
    textures: &TextureCache,
) -> (u32, u32, Vec<u8>) {
    let mut canvas = vec![0u8; (width * height * 4) as usize];
    let missing = rasterize_commands(&mut canvas, width, height, commands, textures);
    if missing > 0 {
        println!("screenshot missing_textures={missing}");
    }
    flatten_onto_black(&mut canvas);
    (width, height, canvas)
}

/// Composites the frame the window would present, including every running
/// transition.
///
/// The windowed shell draws the live tree and then, for each
/// `FrameOutput::transitions` entry, renders its `frozen` / `under` / `source`
/// faces and runs the method's kernel inside the destination rectangle
/// (`krkr-render::render_transition_to_view`); a `shot` built from
/// `draw_commands` alone showed the un-composited pages mid-transition.  This
/// runs the same composite on the CPU: a frame with no transition is
/// byte-identical to [`composite_frame`], so existing shots do not change, and
/// a transitioning frame blends the faces per the transition's progress.
pub fn composite_frame_output(
    width: u32,
    height: u32,
    frame: &FrameOutput,
    textures: &TextureCache,
) -> (u32, u32, Vec<u8>) {
    if frame.transitions.is_empty() {
        return composite_frame(width, height, &frame.draw_commands, textures);
    }
    // The renderer uploads every face texture before the composite pass, so a
    // face may reference a texture the live tree no longer carries.
    let textures = frame_textures(frame, textures);
    let mut canvas = background_canvas(width, height, frame.clear_color);
    let mut missing =
        rasterize_commands(&mut canvas, width, height, &frame.draw_commands, &textures);
    for transition in &frame.transitions {
        missing += composite_transition(&mut canvas, width, height, frame, transition, &textures);
    }
    if missing > 0 {
        println!("screenshot missing_textures={missing}");
    }
    flatten_onto_black(&mut canvas);
    (width, height, canvas)
}

/// Runs one transition's kernel over the destination rectangle the way
/// `render_transition_to_view` does, replacing the live pixels there.
fn composite_transition(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    frame: &FrameOutput,
    transition: &FrameTransition,
    textures: &TextureCache,
) -> usize {
    let Some((x0, y0, x1, y1)) = transition_bounds(transition.dest_rect, width, height) else {
        return 0;
    };
    let (old, mut missing) = face_canvas(
        width,
        height,
        frame.clear_color,
        &transition.frozen_draw_commands,
        textures,
    );
    let mut incoming = background_canvas(width, height, frame.clear_color);
    missing += rasterize_commands(
        &mut incoming,
        width,
        height,
        &transition.under_draw_commands,
        textures,
    );
    missing += rasterize_commands(
        &mut incoming,
        width,
        height,
        &transition.source_draw_commands,
        textures,
    );
    // Only the `wave` kernel reads the scene beneath the destination layer
    // (`wave_under_face_needed`); every other kernel is told it is absent.
    let under_available = transition.params.method == TransitionMethod::Wave;
    let (under, under_missing) = if under_available {
        face_canvas(
            width,
            height,
            frame.clear_color,
            &transition.under_draw_commands,
            textures,
        )
    } else {
        (Vec::new(), 0)
    };
    missing += under_missing;

    let old_face = Face::new(&old, width, height);
    let faces = Faces {
        old: old_face,
        new: Face::new(&incoming, width, height),
        under: if under_available {
            Face::new(&under, width, height)
        } else {
            Face::EMPTY
        },
        // The renderer binds the frozen face where the rule texture is
        // missing; the rule slot is only read when `data[0].z` says so.
        rule: transition
            .rule_texture_id
            .and_then(|texture_id| {
                textures
                    .get(&texture_id)
                    .map(|(rule_width, rule_height, rgba)| {
                        Face::new(rgba, *rule_width, *rule_height)
                    })
            })
            .unwrap_or(old_face),
    };
    let uniforms =
        Uniforms::for_transition(transition, width as f32, height as f32, under_available);
    for y in y0..y1 {
        for x in x0..x1 {
            let uv = [
                (x as f32 + 0.5) / width as f32,
                (y as f32 + 0.5) / height as f32,
            ];
            let pixel = transition::kernel_pixel(&uniforms, uv, &faces);
            let index = ((y * width + x) * 4) as usize;
            canvas[index..index + 4].copy_from_slice(&[
                channel(pixel[0]),
                channel(pixel[1]),
                channel(pixel[2]),
                channel(pixel[3]),
            ]);
        }
    }
    missing
}

fn face_canvas(
    width: u32,
    height: u32,
    clear_color: Color,
    commands: &[DrawCommand],
    textures: &TextureCache,
) -> (Vec<u8>, usize) {
    let mut canvas = background_canvas(width, height, clear_color);
    let missing = rasterize_commands(&mut canvas, width, height, commands, textures);
    (canvas, missing)
}

/// The destination rectangle in whole pixels.
///
/// `render_transition_to_view` scissor-clips the composite to
/// `physical_rect(dest_rect)` (floor/ceil, clamped to the target); at the
/// debugger's 1:1 transform that is this rectangle.  `None` (the destination
/// has no measurable geometry) covers the whole frame, and an empty rectangle
/// draws nothing, exactly like the renderer's early return.
fn transition_bounds(
    dest_rect: Option<Rect>,
    width: u32,
    height: u32,
) -> Option<(u32, u32, u32, u32)> {
    let Some(rect) = dest_rect else {
        return Some((0, 0, width, height));
    };
    let x0 = rect.x.floor().clamp(0.0, width as f32) as u32;
    let y0 = rect.y.floor().clamp(0.0, height as f32) as u32;
    let x1 = (rect.x + rect.width).ceil().clamp(0.0, width as f32) as u32;
    let y1 = (rect.y + rect.height).ceil().clamp(0.0, height as f32) as u32;
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
}

/// A texture cache extended with the uploads this frame carries, faces
/// included, mirroring the renderer's per-frame upload set.
fn frame_textures(frame: &FrameOutput, textures: &TextureCache) -> TextureCache {
    let mut merged = textures.clone();
    let mut insert = |upload: &ImageUpload| {
        merged.insert(
            upload.texture_id,
            (upload.width, upload.height, Arc::clone(&upload.rgba)),
        );
    };
    for upload in &frame.image_uploads {
        insert(upload);
    }
    for transition in &frame.transitions {
        for upload in &transition.frozen_image_uploads {
            insert(upload);
        }
        for upload in &transition.under_image_uploads {
            insert(upload);
        }
        for upload in &transition.source_image_uploads {
            insert(upload);
        }
        if let Some(upload) = &transition.rule_image_upload {
            insert(upload);
        }
    }
    merged
}

fn background_canvas(width: u32, height: u32, color: Color) -> Vec<u8> {
    let rgba = [
        channel(color.r),
        channel(color.g),
        channel(color.b),
        channel(color.a),
    ];
    let mut canvas = vec![0u8; (width * height * 4) as usize];
    for pixel in canvas.chunks_exact_mut(4) {
        pixel.copy_from_slice(&rgba);
    }
    canvas
}

fn channel(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn rasterize_commands(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    commands: &[DrawCommand],
    textures: &TextureCache,
) -> usize {
    let mut missing = 0usize;
    for command in commands {
        match command {
            DrawCommand::Rect(rect_command) => {
                let r = (rect_command.color.r * 255.0).clamp(0.0, 255.0) as u8;
                let g = (rect_command.color.g * 255.0).clamp(0.0, 255.0) as u8;
                let b = (rect_command.color.b * 255.0).clamp(0.0, 255.0) as u8;
                let a = (rect_command.color.a * 255.0).clamp(0.0, 255.0) as u8;
                fill_rect(canvas, width, height, &rect_command.rect, [r, g, b], a);
            }
            DrawCommand::Image(image) => {
                let Some((tw, th, rgba)) = textures.get(&image.texture_id) else {
                    missing += 1;
                    continue;
                };
                blend_image(
                    canvas,
                    width,
                    height,
                    &image.rect,
                    &image.source_rect,
                    *tw,
                    *th,
                    rgba,
                    image.opacity,
                    image.opaque,
                );
            }
            DrawCommand::Text(_) => {}
        }
    }
    missing
}

/// Flattens the straight-alpha canvas onto an opaque black background.  The
/// transition view starts from the frame's clear colour instead
/// ([`background_canvas`]); this is the historical look the plain
/// [`composite_frame`] keeps.
fn flatten_onto_black(canvas: &mut [u8]) {
    for pixel in canvas.chunks_exact_mut(4) {
        let a = pixel[3] as u16;
        pixel[0] = ((pixel[0] as u16 * a + 127) / 255) as u8;
        pixel[1] = ((pixel[1] as u16 * a + 127) / 255) as u8;
        pixel[2] = ((pixel[2] as u16 * a + 127) / 255) as u8;
        pixel[3] = 255;
    }
}

fn fill_rect(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    rect: &krkr_core::Rect,
    rgb: [u8; 3],
    alpha: u8,
) {
    let x0 = rect.x.max(0.0) as u32;
    let y0 = rect.y.max(0.0) as u32;
    let x1 = ((rect.x + rect.width).max(0.0)).min(width as f32) as u32;
    let y1 = ((rect.y + rect.height).max(0.0)).min(height as f32) as u32;
    for y in y0..y1 {
        for x in x0..x1 {
            blend_pixel(canvas, width, x, y, &[rgb[0], rgb[1], rgb[2]], alpha);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn blend_image(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    rect: &krkr_core::Rect,
    source_rect: &krkr_core::Rect,
    tex_width: u32,
    tex_height: u32,
    rgba: &[u8],
    opacity: f32,
    opaque: bool,
) {
    if rect.width <= 0.0 || rect.height <= 0.0 || tex_width == 0 || tex_height == 0 {
        return;
    }
    let x0 = rect.x.max(0.0) as u32;
    let y0 = rect.y.max(0.0) as u32;
    let x1 = ((rect.x + rect.width).max(0.0)).min(width as f32) as u32;
    let y1 = ((rect.y + rect.height).max(0.0)).min(height as f32) as u32;
    for y in y0..y1 {
        let v = (y as f32 - rect.y) / rect.height;
        let sy = (source_rect.y + v * source_rect.height) as u32;
        if sy >= tex_height {
            continue;
        }
        for x in x0..x1 {
            let u = (x as f32 - rect.x) / rect.width;
            let sx = (source_rect.x + u * source_rect.width) as u32;
            if sx >= tex_width {
                continue;
            }
            let index = ((sy * tex_width + sx) * 4) as usize;
            // `ltOpaque` layers present through `TVPCopyOpaqueImage`, which
            // ignores the stored alpha (`LayerIntf.cpp:5191`); the GPU shader
            // applies the same rule via `force_opaque`.
            let alpha = if opaque {
                (opacity.clamp(0.0, 1.0) * 255.0) as u8
            } else {
                (rgba[index + 3] as f32 * opacity.clamp(0.0, 1.0)) as u8
            };
            blend_pixel(canvas, width, x, y, &rgba[index..index + 4], alpha);
        }
    }
}

fn blend_pixel(canvas: &mut [u8], width: u32, x: u32, y: u32, src: &[u8], alpha: u8) {
    let index = ((y * width + x) * 4) as usize;
    let dst = &mut canvas[index..index + 4];
    let sa = alpha as u32;
    let da = dst[3] as u32;
    let out_a = sa + da * (255 - sa) / 255;
    if out_a == 0 {
        dst.fill(0);
        return;
    }
    for (channel, src) in dst.iter_mut().take(3).zip(src) {
        let s = *src as u32;
        let d = *channel as u32;
        *channel = ((s * sa + d * da * (255 - sa) / 255) / out_a) as u8;
    }
    *dst.last_mut().expect("alpha channel") = out_a as u8;
}

pub fn write_png(path: &str, width: u32, height: u32, rgba: &[u8]) -> std::io::Result<()> {
    let mut raw = Vec::with_capacity((width * height * 4 + height) as usize);
    for row in rgba.chunks_exact((width * 4) as usize) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    // zlib stream with stored (uncompressed) deflate blocks.
    let mut zdata = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65535).peekable();
    while let Some(chunk) = chunks.next() {
        let last = chunks.peek().is_none();
        zdata.push(u8::from(last));
        zdata.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
        zdata.extend_from_slice(&(!(chunk.len() as u16)).to_le_bytes());
        zdata.extend_from_slice(chunk);
    }
    zdata.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut png = Vec::new();
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    write_png_chunk(&mut png, b"IHDR", &ihdr);
    write_png_chunk(&mut png, b"IDAT", &zdata);
    write_png_chunk(&mut png, b"IEND", &[]);
    std::fs::write(path, png)
}

fn write_png_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let mut crc_data = Vec::with_capacity(4 + data.len());
    crc_data.extend_from_slice(kind);
    crc_data.extend_from_slice(data);
    png.extend_from_slice(&crc32(&crc_data).to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

type AlphaBounds = (u32, u32, u32, u32);
type RgbaStats = (usize, u64, usize, Option<AlphaBounds>);

pub fn print_image_pixels(
    image: &krkr_core::ImageCommand,
    uploads: &[ImageUpload],
    engine: &KrkrEngine,
) {
    let stats = uploads
        .iter()
        .find(|upload| upload.texture_id == image.texture_id)
        .map(|upload| rgba_stats(upload.width, upload.height, &upload.rgba))
        .or_else(|| {
            engine.host().layer_tree().layers().find_map(|layer| {
                layer.image.as_ref().and_then(|layer_image| {
                    (layer_image.upload.texture_id == image.texture_id).then(|| {
                        rgba_stats(
                            layer_image.upload.width,
                            layer_image.upload.height,
                            &layer_image.upload.rgba,
                        )
                    })
                })
            })
        });
    println!(
        "image texture={} rect=({},{} {}x{}) source=({},{} {}x{}) opacity={:.3} stats={stats:?}",
        image.texture_id,
        image.rect.x,
        image.rect.y,
        image.rect.width,
        image.rect.height,
        image.source_rect.x,
        image.source_rect.y,
        image.source_rect.width,
        image.source_rect.height,
        image.opacity,
    );
}

pub fn rgba_stats(width: u32, height: u32, rgba: &[u8]) -> RgbaStats {
    let mut nonzero_alpha = 0usize;
    let mut alpha_sum = 0u64;
    let mut nonzero_rgb = 0usize;
    let mut min_x = width;
    let mut min_y = height;
    let mut max_x = 0;
    let mut max_y = 0;
    for (index, pixel) in rgba.chunks_exact(4).enumerate() {
        if pixel[3] != 0 {
            nonzero_alpha += 1;
            alpha_sum += pixel[3] as u64;
            if pixel[0] != 0 || pixel[1] != 0 || pixel[2] != 0 {
                nonzero_rgb += 1;
            }
            let x = index as u32 % width;
            let y = index as u32 / width;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    let bounds = (nonzero_alpha > 0).then_some((min_x, min_y, max_x + 1, max_y + 1));
    (nonzero_alpha, alpha_sum, nonzero_rgb, bounds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use krkr_core::{ImageCommand, RectCommand, Size, TransitionParams};

    const WIDTH: u32 = 4;
    const HEIGHT: u32 = 4;

    fn rect(x: f32, y: f32, width: f32, height: f32, color: [f32; 4]) -> DrawCommand {
        DrawCommand::Rect(RectCommand {
            rect: Rect::new(x, y, width, height),
            color: Color::new(color[0], color[1], color[2], color[3]),
        })
    }

    fn full_frame(color: [f32; 4]) -> DrawCommand {
        rect(0.0, 0.0, WIDTH as f32, HEIGHT as f32, color)
    }

    fn crossfade(progress: f32, dest_rect: Option<Rect>) -> FrameTransition {
        FrameTransition {
            method: "crossfade".to_string(),
            progress,
            params: TransitionParams {
                method: TransitionMethod::Crossfade,
                duration_millis: 1000.0,
                ..TransitionParams::default()
            },
            dest_rect,
            rule_texture_id: None,
            rule_image_upload: None,
            frozen_draw_commands: vec![full_frame([1.0, 0.0, 0.0, 1.0])],
            frozen_image_uploads: Vec::new(),
            under_draw_commands: Vec::new(),
            under_image_uploads: Vec::new(),
            source_draw_commands: vec![full_frame([0.0, 0.0, 1.0, 1.0])],
            source_image_uploads: Vec::new(),
        }
    }

    fn frame(transitions: Vec<FrameTransition>) -> FrameOutput {
        FrameOutput {
            clear_color: Color::new(0.0, 0.0, 0.0, 1.0),
            clip: None,
            // The live frame is the un-composited page the transition replaces.
            draw_commands: vec![full_frame([0.0, 1.0, 0.0, 1.0])],
            image_uploads: Vec::new(),
            image_releases: Vec::new(),
            transitions,
        }
    }

    fn pixel(width: u32, rgba: &[u8], x: u32, y: u32) -> [u8; 4] {
        let index = ((y * width + x) * 4) as usize;
        rgba[index..index + 4].try_into().expect("pixel")
    }

    /// The regression contract: a frame with no transition must composite
    /// byte-for-byte the way `composite_frame` always did, so every existing
    /// shot keeps its meaning and only transitioning frames change.
    #[test]
    fn a_frame_without_transitions_composites_exactly_as_before() {
        let frame = frame(Vec::new());
        let textures = TextureCache::new();
        let before = composite_frame(WIDTH, HEIGHT, &frame.draw_commands, &textures);
        let after = composite_frame_output(WIDTH, HEIGHT, &frame, &textures);
        assert_eq!(before, after);
        assert_eq!(pixel(WIDTH, &after.2, 0, 0), [0, 255, 0, 255]);
    }

    /// Mid-transition the shot must hold the blend, not either page: at
    /// progress 0 / 0.5 / 1 the crossfade kernel returns the frozen face, the
    /// per-channel mix, and the incoming face.
    #[test]
    fn a_crossfade_composites_both_faces_at_the_progress() {
        let textures = TextureCache::new();
        for (progress, expected) in [
            (0.0, [255, 0, 0, 255]),
            (0.5, [128, 0, 128, 255]),
            (1.0, [0, 0, 255, 255]),
        ] {
            let frame = frame(vec![crossfade(progress, None)]);
            let (width, _, rgba) = composite_frame_output(WIDTH, HEIGHT, &frame, &textures);
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    assert_eq!(
                        pixel(width, &rgba, x, y),
                        expected,
                        "progress {progress} at ({x}, {y})"
                    );
                }
            }
        }
    }

    /// A destination rectangle scopes the composite: the renderer scissor-
    /// clips it, so the live frame outside it must survive untouched.
    #[test]
    fn a_transition_only_replaces_its_destination_rectangle() {
        let frame = frame(vec![crossfade(
            1.0,
            Some(Rect::new(0.0, 0.0, WIDTH as f32 / 2.0, HEIGHT as f32)),
        )]);
        let (width, _, rgba) = composite_frame_output(WIDTH, HEIGHT, &frame, &TextureCache::new());
        assert_eq!(pixel(width, &rgba, 0, 0), [0, 0, 255, 255], "inside");
        assert_eq!(
            pixel(width, &rgba, WIDTH - 1, 0),
            [0, 255, 0, 255],
            "outside keeps the live frame"
        );
    }

    /// An empty destination rectangle draws nothing, matching the renderer's
    /// `physical_rect` early return.
    #[test]
    fn a_degenerate_destination_rectangle_composites_nothing() {
        let frame = frame(vec![crossfade(1.0, Some(Rect::new(1.0, 1.0, 0.0, 0.0)))]);
        let (width, _, rgba) = composite_frame_output(WIDTH, HEIGHT, &frame, &TextureCache::new());
        assert_eq!(pixel(width, &rgba, 0, 0), [0, 255, 0, 255]);
    }

    /// The unfinished parts of a face show the frame's clear colour, so a
    /// transition over an uncovered scene matches the window rather than the
    /// black flattening of the plain view.
    #[test]
    fn a_transition_composites_over_the_clear_colour() {
        let mut frame = frame(vec![crossfade(0.0, None)]);
        frame.clear_color = Color::rgb_u8(10, 12, 14);
        frame.draw_commands = Vec::new();
        let (width, _, rgba) = composite_frame_output(WIDTH, HEIGHT, &frame, &TextureCache::new());
        assert_eq!(
            pixel(width, &rgba, 0, 0),
            [255, 0, 0, 255],
            "the face covers"
        );
        frame.transitions[0].frozen_draw_commands = Vec::new();
        let (width, _, rgba) = composite_frame_output(WIDTH, HEIGHT, &frame, &TextureCache::new());
        assert_eq!(
            pixel(width, &rgba, 0, 0),
            [10, 12, 14, 255],
            "the clear colour shows through the empty face"
        );
    }

    /// A face texture may live only in the frame's own uploads: the renderer
    /// uploads every face before the composite pass, so the software view has
    /// to resolve textures from `FrameOutput` too, not just the long-lived
    /// cache.
    #[test]
    fn face_textures_resolve_from_the_frame_uploads() {
        let mut frame = frame(vec![crossfade(0.0, None)]);
        frame.draw_commands = Vec::new();
        frame.transitions[0].frozen_draw_commands = vec![DrawCommand::Image(ImageCommand {
            texture_id: 7,
            rect: Rect::new(0.0, 0.0, WIDTH as f32, HEIGHT as f32),
            source_rect: Rect::new(0.0, 0.0, 1.0, 1.0),
            texture_size: Size::new(1.0, 1.0),
            opacity: 1.0,
            opaque: false,
        })];
        frame.transitions[0].frozen_image_uploads =
            vec![ImageUpload::new(7, 1, 1, Arc::from(vec![0u8, 255, 0, 255]))];
        let (width, _, rgba) = composite_frame_output(WIDTH, HEIGHT, &frame, &TextureCache::new());
        assert_eq!(pixel(width, &rgba, 0, 0), [0, 255, 0, 255]);
    }
}
