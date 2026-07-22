use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use krkr_core::{
    AudioCommand, AudioInstanceId, ButtonState, DrawCommand, EngineEvent, FrameInput, ImageCommand,
    ImageUpload, Point, PointerButton, Size,
};
use krkr_engine::{EngineInput, KrkrEngine, TransitionPolicy};
use krkr_tjs2::runtime::Variant;

fn main() {
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .expect("usage: ginka_probe <game_dir> [frames]");
    let frames = std::env::args()
        .nth(2)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(60);

    let mut engine = KrkrEngine::for_project(&root).expect("engine");
    if std::env::var_os("KRKR_PROBE_TIMED_TRANSITIONS").is_none() {
        engine
            .host_mut()
            .set_transition_policy(TransitionPolicy::Immediate);
    }
    krkr_plugins::register_reference_plugins(&mut engine).expect("plugins");

    match engine.execute_startup() {
        Ok(startup) => println!("startup=ok {startup}"),
        Err(error) => {
            println!("startup=error\n{error}\n---debug---\n{error:?}");
            dump_logs(&engine);
            std::process::exit(1);
        }
    }
    println!(
        "preferred_viewport={:?} has_kag_scenario={} state={:?}",
        engine.preferred_viewport_size(),
        engine.has_kag_scenario(),
        engine.kag_state()
    );
    if let Ok(storage) = std::env::var("KRKR_PROBE_DUMP_STORAGE") {
        let bytes = engine
            .host()
            .read_binary_storage(&storage)
            .expect("storage dump");
        println!("---storage {storage} bytes={}---", bytes.len());
        print!("{}", String::from_utf8_lossy(&bytes));
    }

    // A probe must stay silent.  This mode consumes commands and completes
    // one-shot sounds on the next frame so script waits follow the desktop
    // lifecycle without creating an output device or playing game audio.
    let virtual_audio = std::env::var_os("KRKR_PROBE_AUDIO").is_some();
    let pixel_probe = std::env::var_os("KRKR_PROBE_PIXELS").is_some();
    let mut pending_audio_stops = Vec::new();

    if let Ok(script) = std::env::var("KRKR_PROBE_BEFORE_SCRIPT") {
        engine
            .execute_script("ginka_probe_before.tjs", &script)
            .expect("before script");
    }
    if let Ok(expression) = std::env::var("KRKR_PROBE_BEFORE_EXPR") {
        engine
            .execute_expression("ginka_probe_before.tjs", &expression)
            .expect("before expression");
    }
    let at_frame = std::env::var("KRKR_PROBE_AT_FRAME").ok().map(|value| {
        value
            .parse::<usize>()
            .expect("KRKR_PROBE_AT_FRAME must be a frame index")
    });
    let at_script = std::env::var("KRKR_PROBE_AT_SCRIPT").ok();
    let at_frame_2 = std::env::var("KRKR_PROBE_AT_FRAME_2").ok().map(|value| {
        value
            .parse::<usize>()
            .expect("KRKR_PROBE_AT_FRAME_2 must be a frame index")
    });
    let at_script_2 = std::env::var("KRKR_PROBE_AT_SCRIPT_2").ok();
    let click = std::env::var("KRKR_PROBE_CLICK_FRAME").ok().map(|value| {
        let frame = value
            .parse::<usize>()
            .expect("KRKR_PROBE_CLICK_FRAME must be a frame index");
        let x = std::env::var("KRKR_PROBE_CLICK_X")
            .expect("KRKR_PROBE_CLICK_X is required with KRKR_PROBE_CLICK_FRAME")
            .parse::<f32>()
            .expect("KRKR_PROBE_CLICK_X must be a number");
        let y = std::env::var("KRKR_PROBE_CLICK_Y")
            .expect("KRKR_PROBE_CLICK_Y is required with KRKR_PROBE_CLICK_FRAME")
            .parse::<f32>()
            .expect("KRKR_PROBE_CLICK_Y must be a number");
        (frame, Point::new(x, y))
    });
    let click_2 = std::env::var("KRKR_PROBE_CLICK_FRAME_2").ok().map(|value| {
        let frame = value
            .parse::<usize>()
            .expect("KRKR_PROBE_CLICK_FRAME_2 must be a frame index");
        let x = std::env::var("KRKR_PROBE_CLICK_X_2")
            .expect("KRKR_PROBE_CLICK_X_2 is required with KRKR_PROBE_CLICK_FRAME_2")
            .parse::<f32>()
            .expect("KRKR_PROBE_CLICK_X_2 must be a number");
        let y = std::env::var("KRKR_PROBE_CLICK_Y_2")
            .expect("KRKR_PROBE_CLICK_Y_2 is required with KRKR_PROBE_CLICK_FRAME_2")
            .parse::<f32>()
            .expect("KRKR_PROBE_CLICK_Y_2 must be a number");
        (frame, Point::new(x, y))
    });
    let click_3 = std::env::var("KRKR_PROBE_CLICK_FRAME_3").ok().map(|value| {
        let frame = value
            .parse::<usize>()
            .expect("KRKR_PROBE_CLICK_FRAME_3 must be a frame index");
        let x = std::env::var("KRKR_PROBE_CLICK_X_3")
            .expect("KRKR_PROBE_CLICK_X_3 is required with KRKR_PROBE_CLICK_FRAME_3")
            .parse::<f32>()
            .expect("KRKR_PROBE_CLICK_X_3 must be a number");
        let y = std::env::var("KRKR_PROBE_CLICK_Y_3")
            .expect("KRKR_PROBE_CLICK_Y_3 is required with KRKR_PROBE_CLICK_FRAME_3")
            .parse::<f32>()
            .expect("KRKR_PROBE_CLICK_Y_3 must be a number");
        (frame, Point::new(x, y))
    });
    let click_4 = std::env::var("KRKR_PROBE_CLICK_FRAME_4").ok().map(|value| {
        let frame = value
            .parse::<usize>()
            .expect("KRKR_PROBE_CLICK_FRAME_4 must be a frame index");
        let x = std::env::var("KRKR_PROBE_CLICK_X_4")
            .expect("KRKR_PROBE_CLICK_X_4 is required with KRKR_PROBE_CLICK_FRAME_4")
            .parse::<f32>()
            .expect("KRKR_PROBE_CLICK_X_4 must be a number");
        let y = std::env::var("KRKR_PROBE_CLICK_Y_4")
            .expect("KRKR_PROBE_CLICK_Y_4 is required with KRKR_PROBE_CLICK_FRAME_4")
            .parse::<f32>()
            .expect("KRKR_PROBE_CLICK_Y_4 must be a number");
        (frame, Point::new(x, y))
    });

    let delta = Duration::from_millis(1000 / 60);
    let shot_path = std::env::var("KRKR_PROBE_SHOT").ok();
    let shot_frame = std::env::var("KRKR_PROBE_SHOT_FRAME")
        .ok()
        .map(|value| {
            value
                .parse::<usize>()
                .expect("KRKR_PROBE_SHOT_FRAME must be a frame index")
        })
        .unwrap_or(frames.saturating_sub(1));
    let mut textures: HashMap<u64, (u32, u32, Arc<[u8]>)> = HashMap::new();
    let mut shot_commands: Option<Vec<DrawCommand>> = None;
    let realtime = std::env::var_os("KRKR_PROBE_REALTIME").is_some();
    let time_scale = std::env::var("KRKR_PROBE_TIME_SCALE")
        .ok()
        .map(|value| {
            value
                .parse::<f64>()
                .expect("KRKR_PROBE_TIME_SCALE must be a number")
        })
        .unwrap_or(1.0);
    assert!(
        time_scale.is_finite() && time_scale > 0.0,
        "KRKR_PROBE_TIME_SCALE must be positive"
    );
    for frame_index in 0..frames {
        if at_frame == Some(frame_index) {
            let script = at_script
                .as_deref()
                .expect("KRKR_PROBE_AT_SCRIPT is required with KRKR_PROBE_AT_FRAME");
            println!("executing at frame={frame_index}");
            engine
                .execute_script("ginka_probe_at_frame.tjs", script)
                .expect("at-frame script");
        }
        if at_frame_2 == Some(frame_index) {
            let script = at_script_2
                .as_deref()
                .expect("KRKR_PROBE_AT_SCRIPT_2 is required with KRKR_PROBE_AT_FRAME_2");
            println!("executing second script at frame={frame_index}");
            engine
                .execute_script("ginka_probe_at_frame_2.tjs", script)
                .expect("second at-frame script");
        }
        for id in pending_audio_stops.drain(..) {
            engine
                .notify_audio_stopped(id)
                .expect("virtual audio completion callback");
        }
        if !realtime {
            engine.host_mut().advance_clock(delta.mul_f64(time_scale));
        } else if time_scale > 1.0 {
            engine
                .host_mut()
                .advance_clock(delta.mul_f64(time_scale - 1.0));
        }
        let mut events = Vec::new();
        for (click_frame, position) in [click, click_2, click_3, click_4].into_iter().flatten() {
            if frame_index == click_frame {
                println!("click press at frame={frame_index} position={position:?}");
                events.push(EngineEvent::CursorMoved { position });
                events.push(EngineEvent::PointerInput {
                    button: PointerButton::Primary,
                    state: ButtonState::Pressed,
                });
            } else if frame_index == click_frame + 1 {
                println!("click release at frame={frame_index} position={position:?}");
                events.push(EngineEvent::CursorMoved { position });
                events.push(EngineEvent::PointerInput {
                    button: PointerButton::Primary,
                    state: ButtonState::Released,
                });
            }
        }
        match engine.update(
            EngineInput::new(
                FrameInput::new(Size::new(1280.0, 720.0), 1.0 / 60.0),
                events,
            ),
            delta,
        ) {
            Ok(frame) => {
                for upload in &frame.output.image_uploads {
                    textures.insert(
                        upload.texture_id,
                        (upload.width, upload.height, upload.rgba.clone()),
                    );
                }
                if shot_path.is_some() && frame_index == shot_frame {
                    shot_commands = Some(frame.output.draw_commands.clone());
                }
                let commands = engine.host_mut().take_audio_commands();
                if virtual_audio {
                    queue_virtual_audio_completions(&commands, &mut pending_audio_stops);
                }
                let images = frame
                    .output
                    .draw_commands
                    .iter()
                    .filter(|command| matches!(command, DrawCommand::Image(_)))
                    .count();
                let texts = frame
                    .output
                    .draw_commands
                    .iter()
                    .filter(|command| matches!(command, DrawCommand::Text(_)))
                    .count();
                if frame_index % 20 == 0 {
                    println!(
                        "frame={frame_index} images={images} texts={texts} kag={:?}",
                        engine.kag_state()
                    );
                }
                if pixel_probe
                    && (frame_index % 20 == 0
                        || !frame.output.image_uploads.is_empty()
                        || frame_index + 1 == frames)
                {
                    println!("---pixels frame={frame_index}---");
                    println!(
                        "uploads={:?}",
                        frame
                            .output
                            .image_uploads
                            .iter()
                            .map(|upload| upload.texture_id)
                            .collect::<Vec<_>>()
                    );
                    for command in &frame.output.draw_commands {
                        if let DrawCommand::Image(image) = command {
                            print_image_pixels(image, &frame.output.image_uploads, &engine);
                        }
                    }
                }
            }
            Err(error) => {
                println!("frame={frame_index} error\n{error}\n---debug---\n{error:?}");
                dump_logs(&engine);
                std::process::exit(1);
            }
        }
        if realtime {
            std::thread::sleep(delta);
        }
    }
    if let Ok(expression) = std::env::var("KRKR_PROBE_EXPR") {
        match engine.execute_expression("ginka_probe_inline.tjs", &expression) {
            Ok(value) => println!("expression={value}"),
            Err(error) => println!("expression_error={error}\n---debug---\n{error:?}"),
        }
    }
    if std::env::var_os("KRKR_PROBE_LOGS").is_some() {
        dump_logs(&engine);
    }
    if std::env::var_os("KRKR_PROBE_LAYERS").is_some() {
        println!("---layers---");
        for layer in engine.host().layer_tree().layers() {
            println!(
                "layer id={} name={:?} parent={:?} z={} rect=({},{},{},{}) visible={} renderable={} opacity={} image={} storage={:?}",
                layer.id,
                layer.name,
                layer.parent,
                layer.z_order,
                layer.left,
                layer.top,
                layer.width,
                layer.height,
                layer.visible,
                layer.renderable,
                layer.opacity,
                layer.image.is_some(),
                engine.host().layer_image_storage(layer.id),
            );
        }
    }
    if pixel_probe {
        println!("---layer pixels---");
        for layer in engine.host().layer_tree().layers() {
            if let Some(image) = &layer.image {
                println!(
                    "layer id={} name={:?} visible={} texture={} size={}x{} stats={:?} storage={:?}",
                    layer.id,
                    layer.name,
                    layer.visible,
                    image.upload.texture_id,
                    image.upload.width,
                    image.upload.height,
                    rgba_stats(image.upload.width, image.upload.height, &image.upload.rgba),
                    engine.host().layer_image_storage(layer.id),
                );
            }
        }
    }
    if let Ok(name) = std::env::var("KRKR_PROBE_DUMP_GLOBAL") {
        println!("---global {name}---");
        let mut current = engine.tjs_runtime().global_member(&name);
        if name.contains('.') {
            let mut parts = name.split('.');
            current = engine.tjs_runtime().global_member(parts.next().unwrap());
            for part in parts {
                let Variant::Object(object) = current else {
                    break;
                };
                current = engine.tjs_runtime().object_member(object, part);
            }
        }
        match current {
            Variant::Object(object) => {
                for (member, value) in engine.tjs_runtime().object_members(object) {
                    match &value {
                        Variant::Integer(_) | Variant::Real(_) | Variant::String(_) => {
                            println!("{member}={value}")
                        }
                        _ => println!("{member}={}", variant_kind(&value)),
                    }
                }
            }
            value => println!("{name}={}", variant_kind(&value)),
        }
    }
    println!("done frames={frames}");
    if let Ok(dir) = std::env::var("KRKR_PROBE_DUMP_LAYER_IMAGES") {
        for layer in engine.host().layer_tree().layers() {
            if let Some(image) = &layer.image {
                let path = format!(
                    "{dir}/layer_{}_{}x{}.png",
                    layer.id, image.upload.width, image.upload.height
                );
                write_png(
                    &path,
                    image.upload.width,
                    image.upload.height,
                    &image.upload.rgba,
                )
                .expect("dump layer image");
            }
        }
    }
    if let Some(path) = shot_path {
        // Live layer images take priority over cached uploads: a layer image
        // can be updated in place without a new upload, which would leave the
        // cached copy stale (e.g. an opaque black texture turned transparent).
        for layer in engine.host().layer_tree().layers() {
            if let Some(image) = &layer.image {
                textures.insert(
                    image.upload.texture_id,
                    (
                        image.upload.width,
                        image.upload.height,
                        image.upload.rgba.clone(),
                    ),
                );
            }
        }
        let commands = shot_commands.unwrap_or_default();
        let (width, height, rgba) = composite_frame(1280, 720, &commands, &textures);
        write_png(&path, width, height, &rgba).expect("write screenshot");
        println!("screenshot={path} commands={}", commands.len());
    }
}

fn composite_frame(
    width: u32,
    height: u32,
    commands: &[DrawCommand],
    textures: &HashMap<u64, (u32, u32, Arc<[u8]>)>,
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
    let x1 = ((rect.x + rect.width).max(0.0) as u32).min(width);
    let y1 = ((rect.y + rect.height).max(0.0) as u32).min(height);
    for y in y0..y1 {
        for x in x0..x1 {
            blend_pixel(canvas, width, x, y, rgb, alpha);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn blend_image(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    rect: &krkr_core::Rect,
    source: &krkr_core::Rect,
    tex_width: u32,
    tex_height: u32,
    rgba: &[u8],
    opacity: f32,
) {
    if rect.width <= 0.0 || rect.height <= 0.0 || source.width <= 0.0 || source.height <= 0.0 {
        return;
    }
    let x0 = rect.x.max(0.0) as u32;
    let y0 = rect.y.max(0.0) as u32;
    let x1 = ((rect.x + rect.width).max(0.0) as u32).min(width);
    let y1 = ((rect.y + rect.height).max(0.0) as u32).min(height);
    for y in y0..y1 {
        let v = (y as f32 - rect.y) / rect.height;
        let sy = (source.y + v * source.height) as u32;
        if sy >= tex_height {
            continue;
        }
        for x in x0..x1 {
            let u = (x as f32 - rect.x) / rect.width;
            let sx = (source.x + u * source.width) as u32;
            if sx >= tex_width {
                continue;
            }
            let index = ((sy * tex_width + sx) * 4) as usize;
            let alpha = (rgba[index + 3] as f32 * opacity.clamp(0.0, 1.0)) as u8;
            blend_pixel(
                canvas,
                width,
                x,
                y,
                [rgba[index], rgba[index + 1], rgba[index + 2]],
                alpha,
            );
        }
    }
}

fn blend_pixel(canvas: &mut [u8], width: u32, x: u32, y: u32, rgb: [u8; 3], alpha: u8) {
    let index = ((y * width + x) * 4) as usize;
    let dst = &mut canvas[index..index + 4];
    let sa = alpha as u32;
    let da = dst[3] as u32;
    let out_a = sa + da * (255 - sa) / 255;
    if out_a == 0 {
        dst.fill(0);
        return;
    }
    for (channel, src) in dst.iter_mut().take(3).zip(rgb) {
        let s = src as u32;
        let d = *channel as u32;
        *channel = ((s * sa + d * da * (255 - sa) / 255) / out_a) as u8;
    }
    dst[3] = out_a as u8;
}

fn write_png(path: &str, width: u32, height: u32, rgba: &[u8]) -> std::io::Result<()> {
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
        crc ^= u32::from(byte);
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
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn variant_kind(value: &Variant) -> &'static str {
    match value {
        Variant::Void => "void",
        Variant::Null => "null",
        Variant::Integer(_) => "integer",
        Variant::Real(_) => "real",
        Variant::String(_) => "string",
        Variant::Octet(_) => "octet",
        Variant::Object(_) => "object",
        Variant::Closure(_) => "closure",
        Variant::CodeObject(_) => "code-object",
    }
}

fn queue_virtual_audio_completions(
    commands: &[AudioCommand],
    pending_stops: &mut Vec<AudioInstanceId>,
) {
    for command in commands {
        match command {
            AudioCommand::Play { id, looping, .. } if !looping => pending_stops.push(*id),
            AudioCommand::Stop { id, .. } => pending_stops.push(*id),
            _ => {}
        }
    }
}

type AlphaBounds = (u32, u32, u32, u32);
type RgbaStats = (usize, u64, usize, Option<AlphaBounds>);

fn print_image_pixels(image: &ImageCommand, uploads: &[ImageUpload], engine: &KrkrEngine) {
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

fn rgba_stats(width: u32, height: u32, rgba: &[u8]) -> RgbaStats {
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
            alpha_sum += u64::from(pixel[3]);
            let x = index as u32 % width;
            let y = index as u32 / width;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
        if pixel[0] != 0 || pixel[1] != 0 || pixel[2] != 0 {
            nonzero_rgb += 1;
        }
    }
    let bounds = (nonzero_alpha > 0).then_some((min_x, min_y, max_x + 1, max_y + 1));
    (nonzero_alpha, alpha_sum, nonzero_rgb, bounds)
}

fn dump_logs(engine: &KrkrEngine) {
    let logs = engine.host().logs();
    let start = logs.len().saturating_sub(500);
    println!("---host logs (last {})---", logs.len() - start);
    for line in &logs[start..] {
        println!("log: {line}");
    }
}
