use std::{path::PathBuf, time::Duration};

use krkr_core::{
    AudioCommand, AudioInstanceId, ButtonState, DrawCommand, EngineEvent, FrameInput, Point,
    PointerButton, Size,
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

    // A probe must stay silent.  This mode consumes commands and completes
    // one-shot sounds on the next frame so script waits follow the desktop
    // lifecycle without creating an output device or playing game audio.
    let virtual_audio = std::env::var_os("KRKR_PROBE_AUDIO").is_some();
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
    let at_frame = std::env::var("KRKR_PROBE_AT_FRAME")
        .ok()
        .map(|value| value.parse::<usize>().expect("KRKR_PROBE_AT_FRAME must be a frame index"));
    let at_script = std::env::var("KRKR_PROBE_AT_SCRIPT").ok();
    let at_frame_2 = std::env::var("KRKR_PROBE_AT_FRAME_2")
        .ok()
        .map(|value| value.parse::<usize>().expect("KRKR_PROBE_AT_FRAME_2 must be a frame index"));
    let at_script_2 = std::env::var("KRKR_PROBE_AT_SCRIPT_2").ok();
    let click = std::env::var("KRKR_PROBE_CLICK_FRAME")
        .ok()
        .map(|value| {
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

    let delta = Duration::from_millis(1000 / 60);
    let realtime = std::env::var_os("KRKR_PROBE_REALTIME").is_some();
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
        let mut events = Vec::new();
        if let Some((click_frame, position)) = click {
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
    if let Ok(name) = std::env::var("KRKR_PROBE_DUMP_GLOBAL") {
        println!("---global {name}---");
        match engine.tjs_runtime().global_member(&name) {
            Variant::Object(object) => {
                for (member, value) in engine.tjs_runtime().object_members(object) {
                    println!("{member}={}", variant_kind(&value));
                }
            }
            value => println!("{name}={}", variant_kind(&value)),
        }
    }
    println!("done frames={frames}");
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

fn dump_logs(engine: &KrkrEngine) {
    let logs = engine.host().logs();
    let start = logs.len().saturating_sub(500);
    println!("---host logs (last {})---", logs.len() - start);
    for line in &logs[start..] {
        println!("log: {line}");
    }
}
