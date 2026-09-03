use std::{path::PathBuf, sync::Arc, time::Duration};

use krkr_assets::ProjectStorage;
use krkr_core::{DrawCommand, FrameInput, Size};
use krkr_engine::{EngineConfig, EngineInput, KrkrEngine, SystemPaths};

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
    let sleep = std::env::var("KRKR_PROBE_SLEEP_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis);
    let compact = std::env::var("KRKR_PROBE_COMPACT").is_ok();

    let storage = ProjectStorage::for_root(&root).expect("storage");
    let mut engine = KrkrEngine::new(EngineConfig {
        project_storage: Some(Arc::new(storage)),
        system_paths: SystemPaths {
            exe_path: format!("{}/", root.display()),
            ..SystemPaths::default()
        },
        ..EngineConfig::default()
    })
    .expect("engine");
    let startup = engine.execute_startup().expect("startup");
    println!("startup={startup}");
    println!(
        "preferred_viewport={:?} has_kag_scenario={} state={:?}",
        engine.preferred_viewport_size(),
        engine.has_kag_scenario(),
        engine.kag_state()
    );

    for frame_index in 0..frames {
        let frame = engine
            .update(
                EngineInput::new(
                    FrameInput::new(Size::new(1280.0, 720.0), 1.0 / 60.0),
                    Vec::new(),
                ),
                delta,
            )
            .expect("update");
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
            format!(
                "{}:{:.3}:frozen_images={}",
                transition.method,
                transition.progress,
                transition
                    .frozen_draw_commands
                    .iter()
                    .filter(|command| matches!(command, DrawCommand::Image(_)))
                    .count()
            )
        });
        let transition = transition.unwrap_or_else(|| "none".to_string());
        let image_signature = images
            .iter()
            .take(4)
            .map(|image| {
                format!(
                    "{}:{}x{}@{},{}",
                    image.texture_id,
                    image.rect.width,
                    image.rect.height,
                    image.rect.x,
                    image.rect.y
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        if !compact || frame_index % 30 == 0 || transition != "none" {
            println!(
                "frame={frame_index:03} tick={:?} reason={:?} images={} uploads={} transition={} sig={}",
                frame.tick.state,
                frame.tick.reason,
                images.len(),
                frame.output.image_uploads.len(),
                transition,
                image_signature
            );
            if !compact {
                for (index, image) in images.iter().take(4).enumerate() {
                    println!(
                        "  image[{index}] texture={} rect=({},{} {}x{}) source=({},{} {}x{}) opacity={:.3}",
                        image.texture_id,
                        image.rect.x,
                        image.rect.y,
                        image.rect.width,
                        image.rect.height,
                        image.source_rect.x,
                        image.source_rect.y,
                        image.source_rect.width,
                        image.source_rect.height,
                        image.opacity
                    );
                }
            }
        }
        if frame.tick.state == krkr_engine::KagTaskState::Finished
            && frame.output.transition.is_none()
            && frame_index > 30
        {
            // TJS-driven KAG keeps advancing through timers even though the engine KAG task is idle.
        }
        if let Some(sleep) = sleep {
            std::thread::sleep(sleep);
        }
    }
}
