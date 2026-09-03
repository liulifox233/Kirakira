use std::{path::PathBuf, time::Duration};

use krkr_core::{
    ButtonState, DrawCommand, EngineEvent, EngineKey, FrameInput, ImageCommand, ImageUpload, Point,
    PointerButton, Size,
};
use krkr_engine::{EngineConfig, EngineInput, KrkrEngine, ProjectStorage, SystemPaths};

type AlphaBounds = (u32, u32, u32, u32);
type RgbaStats = (usize, u64, usize, Option<AlphaBounds>);

fn main() {
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let frames = std::env::args()
        .nth(2)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(180);
    let delta = Duration::from_millis(1000 / 60);
    let click_through = std::env::var("KRKR_PROBE_CLICK_THROUGH").is_ok();
    let full_images = std::env::var("KRKR_PROBE_FULL").is_ok();
    let frame_expression = std::env::var("KRKR_PROBE_FRAME_EXPR").ok();
    let frame_expression_at = std::env::var("KRKR_PROBE_FRAME_EXPR_AT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let frame_expression_each = std::env::var("KRKR_PROBE_FRAME_EXPR_EACH").is_ok();
    let probe_events = std::env::var("KRKR_PROBE_EVENTS")
        .ok()
        .map(|value| parse_probe_events(&value))
        .unwrap_or_default();

    let storage = ProjectStorage::for_root(&root).expect("storage");
    let mut engine = KrkrEngine::new(EngineConfig {
        project_storage: Some(storage),
        system_paths: SystemPaths {
            exe_path: format!("{}/", root.display()),
            ..SystemPaths::default()
        },
        ..EngineConfig::default()
    })
    .expect("engine");
    let startup = engine.execute_startup().expect("startup");
    println!("startup={startup}");
    if std::env::var("KRKR_PROBE_IGNORE_UNKNOWN").is_ok() {
        let value = engine
            .execute_expression(
                "probe_ignore_unknown.tjs",
                "kag.onConductorUnknownTag = function(tagname, elm) { return 0; }",
            )
            .expect("ignore unknown");
        println!("ignore_unknown={value}");
    }
    if std::env::var("KRKR_PROBE_START").is_ok() {
        let start = engine
            .execute_expression(
                "probe_start.tjs",
                "kag.process(\"title.ks\", \"*title_menu_start\", true, true)",
            )
            .expect("start");
        println!("start={start}");
    }
    if let Ok(storage) = std::env::var("KRKR_PROBE_PROCESS") {
        let label = std::env::var("KRKR_PROBE_LABEL").unwrap_or_default();
        let expression = format!(
            "kag.process({}, {}, true, true)",
            tjs_string_literal(&storage),
            tjs_string_literal(&label)
        );
        let value = engine
            .execute_expression("probe_process.tjs", &expression)
            .expect("process scenario");
        println!("process={value} expr={expression}");
    }
    if let Ok(expression) = std::env::var("KRKR_PROBE_EXPR") {
        let value = engine
            .execute_expression("probe_expr.tjs", &expression)
            .expect("probe expression");
        println!("expr={value}");
    }
    println!(
        "preferred_viewport={:?} has_kag_scenario={} state={:?}",
        engine.preferred_viewport_size(),
        engine.has_kag_scenario(),
        engine.kag_state()
    );

    for frame_index in 0..frames {
        if (frame_expression_each || frame_expression_at == frame_index)
            && let Some(expression) = &frame_expression
        {
            let value = engine
                .execute_expression("probe_frame_expr.tjs", expression)
                .expect("probe frame expression");
            println!("frame_expr@{frame_index}={value}");
        }
        if click_through && frame_index == 750 {
            let value = engine
                .execute_expression("probe_click_start.tjs", "kag.current.onButtonClick(0)")
                .expect("script start click");
            println!("script_start_click={value}");
        }
        let mut events = probe_events_for_frame(&probe_events, frame_index);
        if click_through {
            events.extend(probe_click_events(frame_index));
        }
        let frame = engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(1280.0, 720.0), 1.0 / 60.0),
                    events,
                ),
                delta,
            )
            .expect("update");
        if frame_index % 30 == 0
            || (frame.output.transition.is_some() && frame_index % 15 == 0)
            || frame_index + 1 == frames
        {
            let images = frame
                .output
                .draw_commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::Image(image) => Some(image),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let transition = frame.output.transition.as_ref().map(|transition| {
                let frozen_images = transition
                    .frozen_draw_commands
                    .iter()
                    .filter(|command| matches!(command, DrawCommand::Image(_)))
                    .count();
                format!(
                    "{}:{:.3}:frozen_images={}",
                    transition.method, transition.progress, frozen_images
                )
            });
            println!(
                "frame={frame_index:03} tick={:?} reason={:?} images={} uploads={} transition={}",
                frame.tick.state,
                frame.tick.reason,
                images.len(),
                frame.output.image_uploads.len(),
                transition.unwrap_or_else(|| "none".to_string())
            );
            let image_limit = if full_images { images.len() } else { 12 };
            for image in images.iter().take(image_limit) {
                print_image("  image", image, &frame.output.image_uploads, &engine);
            }
            if let Some(transition) = &frame.output.transition {
                let frozen_images = transition
                    .frozen_draw_commands
                    .iter()
                    .filter_map(|command| match command {
                        DrawCommand::Image(image) => Some(image),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let frozen_limit = if full_images {
                    frozen_images.len()
                } else {
                    frozen_images.len().min(8)
                };
                for image in frozen_images.into_iter().take(frozen_limit) {
                    print_image("  frozen", image, &transition.frozen_image_uploads, &engine);
                }
            }
            let mut counts = std::collections::BTreeMap::<u64, usize>::new();
            for image in &images {
                *counts.entry(image.texture_id).or_default() += 1;
            }
            let repeated = counts
                .into_iter()
                .filter(|(_, count)| *count > 1)
                .collect::<Vec<_>>();
            if !repeated.is_empty() {
                println!("  repeated_textures={repeated:?}");
            }
        }
    }

    println!("logs={}", engine.host().logs().len());
    for log in engine.host().logs().iter().take(200) {
        println!("  log {log}");
    }

    let nonempty_only = std::env::var("KRKR_PROBE_NONEMPTY").is_ok();
    println!("layers:");
    for layer in engine.host().layer_tree().layers() {
        let (texture, image_size, nonzero_alpha, alpha_sum, nonzero_rgb, bbox) = layer
            .image
            .as_ref()
            .map(|image| {
                let (nonzero_alpha, alpha_sum, nonzero_rgb, bbox) =
                    rgba_stats(image.upload.width, image.upload.height, &image.upload.rgba);
                (
                    image.upload.texture_id.to_string(),
                    format!("{}x{}", image.upload.width, image.upload.height),
                    nonzero_alpha,
                    alpha_sum,
                    nonzero_rgb,
                    bbox,
                )
            })
            .unwrap_or_else(|| ("-".to_string(), "-".to_string(), 0usize, 0u64, 0usize, None));
        if nonempty_only && nonzero_alpha == 0 && !layer.visible && layer.image.is_some() {
            continue;
        }
        println!(
            "  id={} parent={:?} z={} name={:?} vis={} rend={} pos=({},{} {}x{}) image_rect=({},{} {}x{}) type={} face={} opacity={} tex={} img={} alpha_pixels={} alpha_sum={} rgb_pixels={} bbox={:?}",
            layer.id,
            layer.parent,
            layer.z_order,
            layer.name,
            layer.visible,
            layer.renderable,
            layer.left,
            layer.top,
            layer.width,
            layer.height,
            layer.image_left,
            layer.image_top,
            layer.image_width,
            layer.image_height,
            layer.layer_type,
            layer.face,
            layer.opacity,
            texture,
            image_size,
            nonzero_alpha,
            alpha_sum,
            nonzero_rgb,
            bbox
        );
    }
}

fn tjs_string_literal(value: &str) -> String {
    format!("{value:?}")
}

fn parse_probe_events(value: &str) -> Vec<(usize, EngineEvent)> {
    value
        .split(';')
        .filter_map(|item| {
            let mut parts = item.split(':');
            let frame = parts.next()?.parse::<usize>().ok()?;
            let kind = parts.next()?;
            match kind {
                "move" => {
                    let x = parts.next()?.parse::<f32>().ok()?;
                    let y = parts.next()?.parse::<f32>().ok()?;
                    Some((
                        frame,
                        EngineEvent::CursorMoved {
                            position: Point::new(x, y),
                        },
                    ))
                }
                "down" => Some((
                    frame,
                    EngineEvent::PointerInput {
                        button: PointerButton::Primary,
                        state: ButtonState::Pressed,
                    },
                )),
                "up" => Some((
                    frame,
                    EngineEvent::PointerInput {
                        button: PointerButton::Primary,
                        state: ButtonState::Released,
                    },
                )),
                "rightdown" => Some((
                    frame,
                    EngineEvent::PointerInput {
                        button: PointerButton::Secondary,
                        state: ButtonState::Pressed,
                    },
                )),
                "rightup" => Some((
                    frame,
                    EngineEvent::PointerInput {
                        button: PointerButton::Secondary,
                        state: ButtonState::Released,
                    },
                )),
                "enterdown" => Some((
                    frame,
                    EngineEvent::KeyboardInput {
                        key: EngineKey::Enter,
                        state: ButtonState::Pressed,
                        repeat: false,
                    },
                )),
                "enterup" => Some((
                    frame,
                    EngineEvent::KeyboardInput {
                        key: EngineKey::Enter,
                        state: ButtonState::Released,
                        repeat: false,
                    },
                )),
                _ => None,
            }
        })
        .collect()
}

fn probe_events_for_frame(events: &[(usize, EngineEvent)], frame: usize) -> Vec<EngineEvent> {
    events
        .iter()
        .filter_map(|(event_frame, event)| (*event_frame == frame).then_some(*event))
        .collect()
}

fn print_image(label: &str, image: &ImageCommand, uploads: &[ImageUpload], engine: &KrkrEngine) {
    let upload_stats = uploads
        .iter()
        .find(|upload| upload.texture_id == image.texture_id)
        .map(|upload| rgba_stats(upload.width, upload.height, &upload.rgba));
    let layers = engine
        .host()
        .layer_tree()
        .layers()
        .filter(|layer| {
            layer
                .image
                .as_ref()
                .is_some_and(|layer_image| layer_image.upload.texture_id == image.texture_id)
        })
        .map(|layer| {
            format!(
                "{}:{:?}:vis{}:rend{}:pos{},{}",
                layer.id, layer.name, layer.visible, layer.renderable, layer.left, layer.top
            )
        })
        .collect::<Vec<_>>();
    println!(
        "{label} texture={} rect=({},{} {}x{}) source=({},{} {}x{}) opacity={:.3} stats={:?} layers={:?}",
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
        upload_stats,
        layers
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
    let bbox = (nonzero_alpha > 0).then_some((min_x, min_y, max_x + 1, max_y + 1));
    (nonzero_alpha, alpha_sum, nonzero_rgb, bbox)
}

fn probe_click_events(frame_index: usize) -> Vec<EngineEvent> {
    if matches!(frame_index, 360 | 420 | 480) {
        return click_at(Point::new(1106.0, 203.0));
    }
    if frame_index > 650 && frame_index.is_multiple_of(45) {
        return click_at(Point::new(640.0, 620.0));
    }
    Vec::new()
}

fn click_at(position: Point) -> Vec<EngineEvent> {
    vec![
        EngineEvent::CursorMoved { position },
        EngineEvent::PointerInput {
            button: PointerButton::Primary,
            state: ButtonState::Pressed,
        },
        EngineEvent::PointerInput {
            button: PointerButton::Primary,
            state: ButtonState::Released,
        },
    ]
}
