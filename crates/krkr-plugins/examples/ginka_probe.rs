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
    engine
        .host_mut()
        .set_transition_policy(TransitionPolicy::Immediate);
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

    let delta = Duration::from_millis(1000 / 60);
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
