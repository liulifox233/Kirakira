//! LLDB-style interactive script debugger and headless probe for KRKR games.
//!
//! Usage:
//!   cargo run -p krkr-debug -- <game_dir> [options]
//!
//! Debugger options:
//!   -b <spec>               breakpoint, repeatable:
//!                             file.tjs:LINE   TJS source line
//!                             file.ks:LINE    KAG scenario line
//!                             *label          KAG label
//!   --break-on-exception    stop when a TJS runtime error is raised
//!   --pause-at-start        stop at the first executed instruction
//!   --commands <file>       read debugger commands from a file, then stdin
//!   --scenario <name>       explicit probe override: load a KAG scenario
//!                           after startup if the game did not start one
//!                           itself
//!
//! Probe options:
//!   --before-script <tjs>   execute TJS source before the frame loop
//!   --before-expr <expr>    evaluate a TJS expression before the frame loop
//!   --at-frame <n> --at-script <tjs>
//!                           execute TJS source when frame n begins (pair in
//!                           order; repeatable)
//!   --click <n,x,y>         inject a click at (x, y) on frame n, release on
//!                           frame n+1 (repeatable)
//!   --auto-click            automatically confirm [l]/[p] click waits
//!   --expr <expr>           evaluate a TJS expression after the frame loop
//!   --shot <path>           write a composited screenshot PNG
//!   --shot-frame <n>        capture draw commands at frame n (default last)
//!   --pixels                print per-image pixel statistics while running
//!   --layers                dump the layer tree at the end
//!   --dump-global <name>    dump a global variable's members at the end
//!   --dump-storage <name>   print a storage file's contents and exit
//!   --dump-layer-images <dir>
//!                           write one PNG per layer image at the end
//!   --logs                  dump host logs at the end
//!   --trace <cats>          enable trace categories: audio,kag or all
//!                           (same syntax as the KRKR_TRACE env var)
//!   --timed-transitions     keep timed transitions (default: immediate)
//!   --time-scale <f>        virtual clock multiplier (default 1.0)
//!   --realtime              sleep per frame instead of fast-forwarding
//!   --virtual-audio         consume audio commands without an output device
//!   --max-frames <n>        frame budget (default 100000)
//!
//! Debugger commands (LLDB style):
//!   b <spec> / bl / bd <id>     add / list / delete breakpoints
//!   c                           continue
//!   si                          step one instruction
//!   s / n / fin                 step line into / over / out
//!   ks                          step one KAG tag
//!   bt                          TJS backtrace
//!   f                           current location + source line
//!   p <expr>                    evaluate a TJS expression (global context)
//!   regs                        registers of the innermost frame
//!   set reg <n> <expr>          write a register from an expression
//!   dis                         disassemble the current function
//!   catch [on|off]              show/toggle break-on-exception
//!   kag                         current KAG stop info
//!   q                           quit the session
//! An empty line repeats the last control command.

mod cli;
mod snapshot;

use std::{path::PathBuf, sync::Arc, time::Duration};

use krkr_assets::{NativeAssetStore, ProjectStorage};
use krkr_audio::VirtualAudioSink;
use krkr_core::{
    AudioCommand, AudioInstanceId, ButtonState, DrawCommand, EngineEvent, FrameInput, Point,
    PointerButton, Size,
};
use krkr_engine::{
    EngineConfig, EngineInput, KagTaskState, KrkrEngine, RuntimeSession, SystemPaths,
    TransitionPolicy,
};
use krkr_tjs2::runtime::Variant;
use snapshot::TextureCache;

use crate::cli::{BreakpointSpec, CliDebugger, parse_breakpoint_spec};

#[derive(Default)]
struct Config {
    root: Option<PathBuf>,
    breakpoints: Vec<String>,
    break_on_exception: bool,
    pause_at_start: bool,
    commands_file: Option<PathBuf>,
    scenario: Option<String>,
    before_script: Option<String>,
    before_expr: Option<String>,
    at_frames: Vec<(usize, Option<String>)>,
    clicks: Vec<(usize, Point)>,
    auto_click: bool,
    expr: Option<String>,
    shot: Option<String>,
    shot_frame: Option<usize>,
    pixels: bool,
    layers: bool,
    dump_globals: Vec<String>,
    dump_storages: Vec<String>,
    dump_layer_images: Option<String>,
    logs: bool,
    timed_transitions: bool,
    time_scale: f64,
    realtime: bool,
    virtual_audio: bool,
    trace: Option<String>,
    max_frames: usize,
}

fn next_arg(args: &mut impl Iterator<Item = String>, flag: &str) -> String {
    args.next()
        .unwrap_or_else(|| panic!("{flag} requires a value"))
}

fn parse_args() -> Config {
    let mut config = Config {
        max_frames: 100_000,
        time_scale: 1.0,
        ..Config::default()
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-b" | "--break" => config.breakpoints.push(next_arg(&mut args, "-b")),
            "--break-on-exception" => config.break_on_exception = true,
            "--pause-at-start" => config.pause_at_start = true,
            "--commands" => {
                config.commands_file = Some(PathBuf::from(next_arg(&mut args, "--commands")))
            }
            "--scenario" => config.scenario = Some(next_arg(&mut args, "--scenario")),
            "--before-script" => {
                config.before_script = Some(next_arg(&mut args, "--before-script"))
            }
            "--before-expr" => config.before_expr = Some(next_arg(&mut args, "--before-expr")),
            "--at-frame" => {
                let frame = next_arg(&mut args, "--at-frame")
                    .parse()
                    .expect("--at-frame must be a frame index");
                config.at_frames.push((frame, None));
            }
            "--at-script" => {
                let script = next_arg(&mut args, "--at-script");
                let Some((_, slot)) = config.at_frames.last_mut() else {
                    panic!("--at-script requires a preceding --at-frame");
                };
                *slot = Some(script);
            }
            "--click" => {
                let value = next_arg(&mut args, "--click");
                let parts: Vec<&str> = value.split(',').collect();
                let [frame, x, y] = parts.as_slice() else {
                    panic!("--click expects <frame>,<x>,<y>");
                };
                config.clicks.push((
                    frame.parse().expect("--click frame must be a number"),
                    Point::new(
                        x.parse().expect("--click x must be a number"),
                        y.parse().expect("--click y must be a number"),
                    ),
                ));
            }
            "--auto-click" => config.auto_click = true,
            "--expr" => config.expr = Some(next_arg(&mut args, "--expr")),
            "--shot" => config.shot = Some(next_arg(&mut args, "--shot")),
            "--shot-frame" => {
                config.shot_frame = Some(
                    next_arg(&mut args, "--shot-frame")
                        .parse()
                        .expect("--shot-frame must be a number"),
                );
            }
            "--pixels" => config.pixels = true,
            "--layers" => config.layers = true,
            "--dump-global" => config
                .dump_globals
                .push(next_arg(&mut args, "--dump-global")),
            "--dump-storage" => config
                .dump_storages
                .push(next_arg(&mut args, "--dump-storage")),
            "--dump-layer-images" => {
                config.dump_layer_images = Some(next_arg(&mut args, "--dump-layer-images"));
            }
            "--logs" => config.logs = true,
            "--timed-transitions" => config.timed_transitions = true,
            "--time-scale" => {
                config.time_scale = next_arg(&mut args, "--time-scale")
                    .parse()
                    .expect("--time-scale must be a number");
                assert!(
                    config.time_scale.is_finite() && config.time_scale > 0.0,
                    "--time-scale must be positive"
                );
            }
            "--realtime" => config.realtime = true,
            "--virtual-audio" => config.virtual_audio = true,
            "--trace" => config.trace = Some(next_arg(&mut args, "--trace")),
            "--max-frames" => {
                config.max_frames = next_arg(&mut args, "--max-frames")
                    .parse()
                    .expect("--max-frames must be a number");
            }
            _ if config.root.is_none() && !arg.starts_with('-') => {
                config.root = Some(PathBuf::from(arg));
            }
            _ => panic!("unknown argument: {arg}"),
        }
    }
    for (frame, script) in &config.at_frames {
        assert!(
            script.is_some(),
            "--at-frame {frame} is missing its --at-script"
        );
    }
    config
}

fn main() {
    let config = parse_args();
    let root = config
        .root
        .clone()
        .expect("usage: krkr-debug <game_dir> [-b spec]... [options]");

    let storage = ProjectStorage::for_root(&root).expect("storage");
    let mut engine = KrkrEngine::new(EngineConfig {
        project_storage: Some(Arc::new(storage)),
        system_paths: system_paths_for_project(&root),
        video_factory: std::sync::Arc::new(krkr_video::PlatformVideoFactory),
        ..EngineConfig::default()
    })
    .expect("engine");
    if let Some(categories) = &config.trace {
        engine.host_mut().set_trace_categories(categories);
    }
    if !config.timed_transitions {
        engine
            .host_mut()
            .set_transition_policy(TransitionPolicy::Immediate);
    }
    krkr_plugins::register_reference_plugins(&mut engine).expect("plugins");

    {
        let runtime = engine.tjs_runtime_mut();
        let debugger = runtime.enable_debugger();
        for spec in &config.breakpoints {
            match parse_breakpoint_spec(spec) {
                Ok(BreakpointSpec::Tjs { file, line }) => {
                    let id = debugger.add_tjs_breakpoint(file, line);
                    println!("breakpoint #{id} tjs {spec}");
                }
                Ok(BreakpointSpec::KagLine { storage, line }) => {
                    let id = debugger.add_kag_line_breakpoint(storage, line);
                    println!("breakpoint #{id} kag {spec}");
                }
                Ok(BreakpointSpec::KagLabel { label }) => {
                    let id = debugger.add_kag_label_breakpoint(&label);
                    println!("breakpoint #{id} kag-label *{label}");
                }
                Err(message) => panic!("invalid breakpoint spec `{spec}`: {message}"),
            }
        }
        debugger.set_break_on_exception(config.break_on_exception);
        if config.pause_at_start {
            debugger.pause_at_start();
        }
        runtime.set_debug_ui(Box::new(CliDebugger::new(config.commands_file.clone())));
    }

    let mut runtime = RuntimeSession::new(
        engine,
        Box::new(NativeAssetStore::new(root.clone())),
        Box::new(VirtualAudioSink::default()),
        Box::new(krkr_core::VirtualClock::default()),
    );

    match runtime.start_project() {
        Ok(()) => println!("startup=ok (project dispatcher)"),
        Err(error) if error.is_debug_quit() => {
            println!("debug session terminated during startup");
            dump_stub_calls(runtime.engine());
            return;
        }
        Err(error) => {
            println!("startup=error\n{error}\n---debug---\n{error:?}");
            dump_logs(runtime.engine());
            dump_stub_calls(runtime.engine());
            std::process::exit(1);
        }
    }
    println!(
        "preferred_viewport={:?} has_kag_scenario={} state={:?}",
        runtime.engine().preferred_viewport_size(),
        runtime.engine().has_kag_scenario(),
        runtime.engine().kag_state()
    );
    if let Some(scenario) = &config.scenario
        && !runtime.engine().has_kag_scenario()
    {
        runtime
            .engine_mut()
            .load_kag_scenario(scenario)
            .expect("load scenario");
        println!("loaded scenario {scenario}");
    }

    for storage in &config.dump_storages {
        let bytes = runtime
            .engine()
            .host()
            .read_binary_storage(storage)
            .expect("storage dump");
        println!("---storage {storage} bytes={}---", bytes.len());
        print!("{}", String::from_utf8_lossy(&bytes));
    }

    if let Some(script) = &config.before_script {
        runtime
            .engine_mut()
            .execute_script("krkr_debug_before.tjs", script)
            .expect("before script");
    }
    if let Some(expression) = &config.before_expr {
        runtime
            .engine_mut()
            .execute_expression("krkr_debug_before.tjs", expression)
            .expect("before expression");
    }

    let delta = Duration::from_millis(1000 / 60);
    let shot_frame = config
        .shot_frame
        .unwrap_or_else(|| config.max_frames.saturating_sub(1));
    let mut textures: TextureCache = TextureCache::new();
    let mut shot_commands: Option<Vec<DrawCommand>> = None;
    let mut pending_audio_stops: Vec<AudioInstanceId> = Vec::new();
    for frame_index in 0..config.max_frames {
        for (at_frame, script) in &config.at_frames {
            if *at_frame == frame_index {
                let script = script.as_deref().expect("checked in parse_args");
                println!("executing at frame={frame_index}");
                runtime
                    .engine_mut()
                    .execute_script("krkr_debug_at_frame.tjs", script)
                    .expect("at-frame script");
            }
        }
        for id in pending_audio_stops.drain(..) {
            if let Err(error) = runtime.engine_mut().notify_audio_stopped(id) {
                println!("audio completion error: {error}");
            }
        }
        // RuntimeSession advances its injected VirtualClock at the frame
        // boundary. Keeping clock ownership there avoids the old split-brain
        // behavior where this host advanced one clock and RuntimeSession then
        // overwrote it with a stale zero timestamp.
        let frame_delta = delta.mul_f64(config.time_scale.max(0.0));
        let mut events = Vec::new();
        for (click_frame, position) in &config.clicks {
            if frame_index == *click_frame {
                println!("click press at frame={frame_index} position={position:?}");
                events.push(EngineEvent::CursorMoved {
                    position: *position,
                });
                events.push(EngineEvent::PointerInput {
                    button: PointerButton::Primary,
                    state: ButtonState::Pressed,
                });
            } else if frame_index == click_frame + 1 {
                println!("click release at frame={frame_index} position={position:?}");
                events.push(EngineEvent::CursorMoved {
                    position: *position,
                });
                events.push(EngineEvent::PointerInput {
                    button: PointerButton::Primary,
                    state: ButtonState::Released,
                });
            }
        }
        if config.auto_click && matches!(runtime.engine().kag_state(), KagTaskState::WaitingClick) {
            runtime.engine_mut().signal_kag_click();
        }
        match runtime.update(
            EngineInput::new(
                FrameInput::new(
                    runtime
                        .engine()
                        .content_viewport_size()
                        .unwrap_or(Size::new(1280.0, 720.0)),
                    frame_delta.as_secs_f32(),
                ),
                events,
            ),
            frame_delta,
        ) {
            Ok(runtime_frame) => {
                let frame = runtime_frame.engine;
                for upload in &frame.output.image_uploads {
                    textures.insert(
                        upload.texture_id,
                        (upload.width, upload.height, upload.rgba.clone()),
                    );
                }
                if config.shot.is_some() && frame_index == shot_frame {
                    shot_commands = Some(frame.output.draw_commands.clone());
                }
                let commands = runtime.take_audio_commands();
                if config.virtual_audio {
                    queue_virtual_audio_completions(&commands, &mut pending_audio_stops);
                }
                if frame_index % 20 == 0 {
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
                    println!(
                        "frame={frame_index} images={images} texts={texts} kag={:?} location={:?}",
                        runtime.engine().kag_state(),
                        runtime.engine().kag_location()
                    );
                    if frame_index % 100 == 0 {
                        let timer_diagnostics = runtime.engine_mut().scheduler_timer_diagnostics();
                        println!(
                            "scheduler={:?} timers={:?}",
                            runtime.engine().scheduler_diagnostics(),
                            timer_diagnostics
                        );
                    }
                }
                if config.pixels
                    && (frame_index % 20 == 0
                        || !frame.output.image_uploads.is_empty()
                        || frame_index + 1 == config.max_frames)
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
                            snapshot::print_image_pixels(
                                image,
                                &frame.output.image_uploads,
                                runtime.engine(),
                            );
                        }
                    }
                }
            }
            Err(error) if error.is_debug_quit() => {
                println!("debug session terminated at frame={frame_index}");
                dump_stub_calls(runtime.engine());
                return;
            }
            Err(error) => {
                println!("frame={frame_index} error\n{error}\n---debug---\n{error:?}");
                dump_logs(runtime.engine());
                dump_stub_calls(runtime.engine());
                std::process::exit(1);
            }
        }
        if config.realtime {
            std::thread::sleep(delta);
        }
    }

    if let Some(expression) = &config.expr {
        match runtime
            .engine_mut()
            .execute_expression("krkr_debug_inline.tjs", expression)
        {
            Ok(value) => println!("expression={value}"),
            Err(error) => println!("expression_error={error}\n---debug---\n{error:?}"),
        }
    }
    if config.logs {
        dump_logs(runtime.engine());
    }
    dump_stub_calls(runtime.engine());
    if config.layers {
        println!("---layers---");
        for layer in runtime.engine().host().layer_tree().layers() {
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
                runtime.engine().host().layer_image_storage(layer.id),
            );
        }
    }
    if config.pixels {
        println!("---layer pixels---");
        for layer in runtime.engine().host().layer_tree().layers() {
            if let Some(image) = &layer.image {
                println!(
                    "layer id={} name={:?} visible={} texture={} size={}x{} stats={:?} storage={:?}",
                    layer.id,
                    layer.name,
                    layer.visible,
                    image.upload.texture_id,
                    image.upload.width,
                    image.upload.height,
                    snapshot::rgba_stats(
                        image.upload.width,
                        image.upload.height,
                        &image.upload.rgba
                    ),
                    runtime.engine().host().layer_image_storage(layer.id),
                );
            }
        }
    }
    for name in &config.dump_globals {
        println!("---global {name}---");
        let mut current = runtime.engine().tjs_runtime().global_member(name);
        if name.contains('.') {
            let mut parts = name.split('.');
            current = runtime
                .engine()
                .tjs_runtime()
                .global_member(parts.next().unwrap());
            for part in parts {
                let Variant::Object(object) = current else {
                    break;
                };
                current = runtime.engine().tjs_runtime().object_member(object, part);
            }
        }
        match current {
            Variant::Object(object) => {
                for (member, value) in runtime.engine().tjs_runtime().object_members(object) {
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
    println!("done frames={}", config.max_frames);
    if let Some(dir) = &config.dump_layer_images {
        for layer in runtime.engine().host().layer_tree().layers() {
            if let Some(image) = &layer.image {
                let path = format!(
                    "{dir}/layer_{}_{}x{}.png",
                    layer.id, image.upload.width, image.upload.height
                );
                snapshot::write_png(
                    &path,
                    image.upload.width,
                    image.upload.height,
                    &image.upload.rgba,
                )
                .expect("dump layer image");
            }
        }
    }
    if let Some(path) = &config.shot {
        // Live layer images take priority over cached uploads: a layer image
        // can be updated in place without a new upload, which would leave the
        // cached copy stale (e.g. an opaque black texture turned transparent).
        for layer in runtime.engine().host().layer_tree().layers() {
            if let Some(image) = &layer.image {
                textures.insert(
                    image.upload.texture_id,
                    (
                        image.upload.width,
                        image.upload.height,
                        Arc::clone(&image.upload.rgba),
                    ),
                );
            }
        }
        let commands = shot_commands.unwrap_or_default();
        let (width, height, rgba) = snapshot::composite_frame(1280, 720, &commands, &textures);
        snapshot::write_png(path, width, height, &rgba).expect("write screenshot");
        println!("screenshot={path} commands={}", commands.len());
    }
}

fn system_paths_for_project(root: &std::path::Path) -> SystemPaths {
    let root_display = root.display().to_string();
    let temp_display = std::env::temp_dir().display().to_string();
    SystemPaths {
        exe_path: format!("{}/", root_display.trim_end_matches(['/', '\\'])),
        data_path: if cfg!(windows) {
            let data = root.join("savedata").display().to_string();
            format!("{}\\", data.trim_end_matches(['/', '\\']))
        } else {
            "savedata/".to_string()
        },
        personal_path: format!("{}/", temp_display.trim_end_matches(['/', '\\'])),
        app_data_path: format!("{}/", temp_display.trim_end_matches(['/', '\\'])),
    }
}

fn dump_logs(engine: &KrkrEngine) {
    let logs = engine.host().logs();
    println!("---host logs ({})---", logs.len());
    for line in logs {
        println!("log: {line}");
    }
}

/// Prints how often each stubbed native method was actually invoked —
/// the strongest signal for what a game depends on that we have not
/// implemented yet.
fn dump_stub_calls(engine: &KrkrEngine) {
    let counts = engine.host().stub_call_counts();
    if counts.is_empty() {
        return;
    }
    let mut entries: Vec<(&String, &u64)> = counts.iter().collect();
    entries.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    println!("---stub calls ({})---", entries.len());
    for (name, count) in entries {
        println!("stub: {name} x{count}");
    }
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
            AudioCommand::PlayPcmStream { id, .. } => pending_stops.push(*id),
            AudioCommand::Stop { id, .. } => pending_stops.push(*id),
            _ => {}
        }
    }
}
