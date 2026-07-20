use std::{path::PathBuf, time::Duration};

use krkr_core::{DrawCommand, FrameInput, Size};
use krkr_engine::{EngineInput, KrkrEngine, TransitionPolicy};

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

    let delta = Duration::from_millis(1000 / 60);
    let realtime = std::env::var_os("KRKR_PROBE_REALTIME").is_some();
    for frame_index in 0..frames {
        match engine.update(
            EngineInput::new(
                FrameInput::new(Size::new(1280.0, 720.0), 1.0 / 60.0),
                Vec::new(),
            ),
            delta,
        ) {
            Ok(frame) => {
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
    println!("done frames={frames}");
}

fn dump_logs(engine: &KrkrEngine) {
    let logs = engine.host().logs();
    let start = logs.len().saturating_sub(500);
    println!("---host logs (last {})---", logs.len() - start);
    for line in &logs[start..] {
        println!("log: {line}");
    }
}
