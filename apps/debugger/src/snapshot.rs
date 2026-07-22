//! Software frame compositing, PNG encoding, and pixel statistics for
//! headless screenshot/pixel probing (no GPU required).

use std::{collections::HashMap, sync::Arc};

use krkr_core::{DrawCommand, ImageUpload};
use krkr_engine::KrkrEngine;

pub(crate) type TextureCache = HashMap<u64, (u32, u32, Arc<[u8]>)>;

pub(crate) fn composite_frame(
    width: u32,
    height: u32,
    commands: &[DrawCommand],
    textures: &TextureCache,
) -> (u32, u32, Vec<u8>) {
    let mut canvas = vec![0u8; (width * height * 4) as usize];
    let mut missing = 0usize;
    for command in commands {
        match command {
            DrawCommand::Rect(rect_command) => {
                let r = (rect_command.color.r * 255.0).clamp(0.0, 255.0) as u8;
                let g = (rect_command.color.g * 255.0).clamp(0.0, 255.0) as u8;
                let b = (rect_command.color.b * 255.0).clamp(0.0, 255.0) as u8;
                let a = (rect_command.color.a * 255.0).clamp(0.0, 255.0) as u8;
                fill_rect(&mut canvas, width, height, &rect_command.rect, [r, g, b], a);
            }
            DrawCommand::Image(image) => {
                let Some((tw, th, rgba)) = textures.get(&image.texture_id) else {
                    missing += 1;
                    continue;
                };
                blend_image(
                    &mut canvas,
                    width,
                    height,
                    &image.rect,
                    &image.source_rect,
                    *tw,
                    *th,
                    rgba,
                    image.opacity,
                );
            }
            DrawCommand::Text(_) => {}
        }
    }
    if missing > 0 {
        println!("screenshot missing_textures={missing}");
    }
    // Flatten onto an opaque black background, matching the window clear color.
    for pixel in canvas.chunks_exact_mut(4) {
        let a = pixel[3] as u16;
        pixel[0] = ((pixel[0] as u16 * a + 127) / 255) as u8;
        pixel[1] = ((pixel[1] as u16 * a + 127) / 255) as u8;
        pixel[2] = ((pixel[2] as u16 * a + 127) / 255) as u8;
        pixel[3] = 255;
    }
    (width, height, canvas)
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
            let alpha = (rgba[index + 3] as f32 * opacity.clamp(0.0, 1.0)) as u8;
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

pub(crate) fn write_png(path: &str, width: u32, height: u32, rgba: &[u8]) -> std::io::Result<()> {
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

pub(crate) fn print_image_pixels(
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

pub(crate) fn rgba_stats(width: u32, height: u32, rgba: &[u8]) -> RgbaStats {
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
