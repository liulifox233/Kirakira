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
//!   --kag-state             log semantic KAG state transitions: the game's
//!                           own `kag` object (storage/label/line/conductor
//!                           status/wait keys), unlike `kag=` in the frame
//!                           log which reflects the engine-side session
//!   --kag-click <frame>     semantic click on frame n (repeatable): wake the
//!                           conductor when it waits for a click, or create
//!                           the click wait a `[s]` handler left unset and
//!                           wake it — equivalent to a player's primary click
//!                           and a no-op when the game is not waiting
//!   --kag-auto-click        keep issuing the semantic click on every frame
//!                           the game's own conductor parks on a click wait,
//!                           the KAGEX counterpart of --auto-click
//!   --watch-expr <expr>     evaluate a TJS expression every frame and log it
//!                           whenever its value changes (repeatable)
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
//!   --trace-call <pattern>  log calls into a native method from startup on
//!                           (e.g. `Layer.affineCopy`; repeatable)
//!   --timed-transitions     keep timed transitions (default: immediate)
//!   --time-scale <f>        virtual clock multiplier (default 1.0)
//!   --realtime              sleep per frame instead of fast-forwarding
//!   --virtual-audio         consume audio commands without an output device
//!   --interactive           deterministic stdin control (starts paused)
//!   --quiet                 suppress periodic frame/pixel progress output
//!   --max-frames <n>        frame budget (default 100000)
//!
//! Interactive probe commands:
//!   advance [n] / wait [n]  run n frames (default 1), then pause
//!   until <condition> [max]  run until a KAG/layer/log condition or timeout
//!   run / pause             run continuously / pause before the next frame
//!   click <x> <y>           inject a click and advance two frames
//!   state / layers [filter] print current engine state / layer tree
//!   layer <id> / draw       inspect one layer / rendered draw commands
//!                           (`layer` binds the owning TJS object to `dbgLayer`)
//!   hit <x> <y>             inspect input hit candidates and script handlers
//!   probe                   state + layers + draw in one response
//!   auto [on|off]            toggle automatic KAG click waits
//!   autopoint <x> <y>|off    inject a click at a point every running frame
//!   load <storage>          load a KAG scenario directly
//!   logs [-n <tail>] [needle]...
//!                           print host diagnostics matching every needle
//!                           (newest <tail> lines, default 200, `-n 0` = all)
//!   trace [list|add|rm|off|names] [pattern]...
//!                           log calls into native methods, e.g.
//!                           `trace add Layer.affineCopy`; matches are written
//!                           to the host log, so `logs "native call"` reads
//!                           them back
//!   resources                list pending image/script/external resources
//!   shot <path>             write the last rendered frame as a PNG
//!   expr <tjs>              evaluate an expression in the global context
//!   members [-a] [-f <substr>] <expr>
//!                           list an object's data members (`-a` also shows
//!                           methods, `-f` filters by name)
//!   q                       quit
//!
//! See `docs/DEBUGGING.md` for a worked investigation using these commands.
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

use std::{
    collections::VecDeque,
    io::BufRead,
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
    thread,
    time::Duration,
};

use krkr_assets::{NativeAssetStore, ProjectStorage};
use krkr_audio::VirtualAudioSink;
use krkr_core::{
    AudioCommand, AudioInstanceId, ButtonState, DrawCommand, EngineEvent, FrameInput, Point,
    PointerButton, Size,
};
use krkr_engine::{
    EngineConfig, EngineInput, KagTaskState, KrkrEngine, KrkrHost, RuntimeSession, SystemPaths,
    TransitionPolicy,
};
use krkr_tjs2::runtime::{ObjectHandle, Runtime, Variant};
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
    kag_clicks: Vec<usize>,
    kag_auto_click: bool,
    kag_state: bool,
    watch_exprs: Vec<String>,
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
    interactive: bool,
    quiet: bool,
    trace: Option<String>,
    trace_calls: Vec<String>,
    max_frames: usize,
}

/// Newest-first log lines shown by `logs` when no `-n` is given.
const DEFAULT_LOG_TAIL: usize = 200;

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
            "--kag-click" => {
                config.kag_clicks.push(
                    next_arg(&mut args, "--kag-click")
                        .parse()
                        .expect("--kag-click must be a frame index"),
                );
            }
            "--kag-auto-click" => config.kag_auto_click = true,
            "--kag-state" => config.kag_state = true,
            "--watch-expr" => config.watch_exprs.push(next_arg(&mut args, "--watch-expr")),
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
            "--interactive" => config.interactive = true,
            "--quiet" => config.quiet = true,
            "--trace" => config.trace = Some(next_arg(&mut args, "--trace")),
            "--trace-call" => config.trace_calls.push(next_arg(&mut args, "--trace-call")),
            "--max-frames" => {
                config.max_frames = next_arg(&mut args, "--max-frames")
                    .parse()
                    .expect("--max-frames must be a number");
            }
            _ if config.root.is_none() && !arg.starts_with('-') => {
                config.root = Some(PathBuf::from(arg));
            }
            // KRKR options use a single dash (`-debugwin=no`) and reach scripts
            // through `System.getArgument`, which reads the process arguments
            // directly. Leave them alone so a game option can be set from the
            // debugger command line; the debugger's own flags all use `--`.
            _ if !arg.starts_with("--") => {}
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

/// Semantic click: the accessibility-style way to advance the game, defined
/// in game terms instead of screen coordinates. Wakes the conductor when it
/// already waits for a click, or — when the `[s]` handler has put the game to
/// sleep without establishing a click wait — creates the wait the game's
/// `waitClick` would normally build and then wakes it. Both paths are exactly
/// what a player's primary click triggers in the official engine; when the
/// game is not waiting the script is a no-op.
const KAG_CLICK_SOURCE: &str = r#"
(function() {
    var k = global.kag;
    if (typeof k != "Object") return "no-kag";
    var c = typeof k.conductor == "Object" ? k.conductor : void;
    var stor = typeof k.currentStorage == "String" ? k.currentStorage : "-";
    var line = c != void ? c.curLine : -1;
    if (c != void && c.status == c.mWait) {
        c.trigger("click");
        return "wake " + stor + ":" + line;
    }
    if (k.inSleep) {
        // KAG3's waitClick(elm) ignores elm, but pass a dictionary so a
        // KAGEX override that reads elm members cannot throw on a string.
        if (k.waitClick != void) k.waitClick(%[]);
        if (c != void && c.status == c.mWait) {
            c.trigger("click");
            return "wake-sleep " + stor + ":" + line;
        }
        return "sleep-no-wait " + stor + ":" + line;
    }
    return "not-waiting " + stor + ":" + line;
})()
"#;

/// True when the game's own KAG conductor is parked on a click wait.
/// `--kag-auto-click` uses this to skip the script evaluation on the frames
/// where the click would be a no-op, which matters because a scene runs for
/// tens of thousands of probe frames.  Unlike `--kag-click`, the sleeping
/// `[s]` case is deliberately excluded: synthesizing the wait repeatedly would
/// nest `waitClick` calls until the VM runs out of frames.
fn kag_awaits_click(engine: &KrkrEngine) -> bool {
    let runtime = engine.tjs_runtime();
    let Variant::Object(kag) = runtime.global_member("kag") else {
        return false;
    };
    let Variant::Object(conductor) = runtime.object_member(kag, "conductor") else {
        return false;
    };
    if !matches!(
        runtime.object_member(conductor, "status"),
        Variant::Integer(2)
    ) {
        return false;
    }
    let Variant::Object(wait_until) = runtime.object_member(conductor, "waitUntil") else {
        return false;
    };
    runtime
        .object_members(wait_until)
        .iter()
        .any(|(name, _)| name == "click")
}

/// Reads the game's own KAG layer (`global.kag`, the KAG3/KAGEX object) rather
/// than the engine-side KAG session. The engine session reports `Finished`
/// while the game is sitting at its title screen, so this is the state that
/// reflects what the player actually sees.
fn semantic_kag_state(engine: &KrkrEngine) -> Option<String> {
    let runtime = engine.tjs_runtime();
    let Variant::Object(kag) = runtime.global_member("kag") else {
        return None;
    };
    let member = |object: ObjectHandle, name: &str| runtime.object_member(object, name);
    let as_int = |object: ObjectHandle, name: &str| match member(object, name) {
        Variant::Integer(value) => Some(value),
        _ => None,
    };
    let as_string = |object: ObjectHandle, name: &str| match member(object, name) {
        Variant::String(value) => Some(value),
        _ => None,
    };
    let conductor = match member(kag, "conductor") {
        Variant::Object(object) => Some(object),
        _ => None,
    };
    let status = conductor
        .and_then(|object| as_int(object, "status"))
        .unwrap_or(-1);
    let line = conductor
        .and_then(|object| as_int(object, "curLine"))
        .unwrap_or(-1);
    let timer_enabled = conductor
        .and_then(|object| as_int(object, "timerEnabled"))
        .unwrap_or(-1);
    let waits: Vec<String> = conductor
        .and_then(|object| match member(object, "waitUntil") {
            Variant::Object(wait_until) => Some(wait_until),
            _ => None,
        })
        .map(|wait_until| {
            runtime
                .object_members(wait_until)
                .into_iter()
                .map(|(name, _)| name)
                .collect()
        })
        .unwrap_or_default();
    Some(format!(
        "{}@{}:{} st={} sleep={} clickwait={} timer={} wait=[{}]",
        as_string(kag, "currentStorage").unwrap_or_default(),
        as_string(kag, "currentLabel").unwrap_or_default(),
        line,
        status_name(status),
        as_int(kag, "inSleep").unwrap_or_default(),
        as_int(kag, "clickWaiting").unwrap_or_default(),
        timer_enabled,
        waits.join(","),
    ))
}

fn status_name(status: i64) -> String {
    match status {
        0 => "stop(0)".to_string(),
        1 => "run(1)".to_string(),
        2 => "wait(2)".to_string(),
        other => format!("{other}"),
    }
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
    if !config.trace_calls.is_empty() {
        // Patterns are matched when a call happens, so arming them here also
        // covers native methods installed later on individual instances.
        engine
            .tjs_runtime_mut()
            .set_native_call_traces(&config.trace_calls);
    }

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

    let interactive = config.interactive.then(start_interactive_console);
    if interactive.is_some() {
        println!("interactive=ready paused=true (type `help` for the command list)");
    }

    let delta = Duration::from_millis(1000 / 60);
    let shot_frame = config
        .shot_frame
        .unwrap_or_else(|| config.max_frames.saturating_sub(1));
    let mut textures: TextureCache = TextureCache::new();
    let mut shot_commands: Option<Vec<DrawCommand>> = None;
    let mut pending_audio_stops: Vec<AudioInstanceId> = Vec::new();
    let mut pending_interactive_releases = Vec::new();
    let mut pending_interactive_clicks = Vec::new();
    let mut pending_interactive_shots = Vec::new();
    let mut interactive_paused = interactive.is_some();
    let mut interactive_budget: Option<usize> = None;
    let mut interactive_auto_click = config.auto_click;
    let mut interactive_auto_point: Option<Point> = None;
    let mut interactive_until: Option<InteractiveUntil> = None;
    let mut deferred_interactive_commands = VecDeque::new();
    let mut last_kag_semantic: Option<String> = None;
    let mut watch_values: Vec<Option<String>> = vec![None; config.watch_exprs.len()];
    for frame_index in 0..config.max_frames {
        // Interactive mode is deliberately deterministic: it starts paused,
        // and only `advance`/`run`/`click` allow another frame to execute.
        // While running we still poll without blocking so an agent can issue
        // `pause` or inspect state at any time.
        if let Some(interactive) = &interactive {
            loop {
                if interactive_paused || interactive_budget == Some(0) {
                    if interactive_budget == Some(0) {
                        if let Some(condition) = interactive_until.take() {
                            println!(
                                "interactive=until-timeout condition={condition:?} frame={frame_index}"
                            );
                        }
                        interactive_budget = None;
                        interactive_paused = true;
                        println!("interactive=paused frame={frame_index}");
                    }
                    let command = match deferred_interactive_commands.pop_front() {
                        Some(command) => command,
                        None => match interactive.recv() {
                            Ok(command) => command,
                            Err(_) => InteractiveCommand::Quit,
                        },
                    };
                    if apply_interactive_control(
                        command,
                        &mut interactive_paused,
                        &mut interactive_budget,
                        &mut interactive_until,
                        &mut pending_interactive_clicks,
                        &mut pending_interactive_shots,
                        frame_index,
                        &mut runtime,
                        &mut textures,
                        shot_commands.as_deref(),
                        &mut interactive_auto_click,
                        &mut interactive_auto_point,
                    ) {
                        dump_stub_calls(runtime.engine());
                        return;
                    }
                    if !interactive_paused && interactive_budget != Some(0) {
                        break;
                    }
                } else {
                    while let Ok(command) = interactive.try_recv() {
                        // A bounded advance is a transaction: inspection
                        // commands entered in the same stdin batch must run
                        // after the requested frames, not on frame N+1 while
                        // the batch is still executing.  `pause` and `quit`
                        // remain high-priority so an unbounded `run` can be
                        // interrupted immediately.
                        if interactive_budget.is_some()
                            && !matches!(
                                command,
                                InteractiveCommand::Pause | InteractiveCommand::Quit
                            )
                        {
                            deferred_interactive_commands.push_back(command);
                            continue;
                        }
                        if apply_interactive_control(
                            command,
                            &mut interactive_paused,
                            &mut interactive_budget,
                            &mut interactive_until,
                            &mut pending_interactive_clicks,
                            &mut pending_interactive_shots,
                            frame_index,
                            &mut runtime,
                            &mut textures,
                            shot_commands.as_deref(),
                            &mut interactive_auto_click,
                            &mut interactive_auto_point,
                        ) {
                            dump_stub_calls(runtime.engine());
                            return;
                        }
                    }
                    break;
                }
            }
        }
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
        for click_frame in &config.kag_clicks {
            if frame_index == *click_frame {
                match runtime
                    .engine_mut()
                    .execute_expression("krkr_debug_kag_click.tjs", KAG_CLICK_SOURCE)
                {
                    Ok(value) => println!("kag-click frame={frame_index} -> {value}"),
                    Err(error) => println!("kag-click frame={frame_index} error: {error}"),
                }
            }
        }
        if config.kag_auto_click && kag_awaits_click(runtime.engine()) {
            match runtime
                .engine_mut()
                .execute_expression("krkr_debug_kag_click.tjs", KAG_CLICK_SOURCE)
            {
                Ok(value) => {
                    if !config.quiet {
                        println!("kag-auto-click frame={frame_index} -> {value}");
                    }
                }
                Err(error) => println!("kag-auto-click frame={frame_index} error: {error}"),
            }
        }
        // A parked VM (the game suspended inside a resource load) makes every
        // nested top-level evaluation return `void` immediately, so sampling
        // then would report a spurious change on every such frame.
        let vm_parked = runtime.engine().tjs_runtime().is_suspended();
        for (index, expr) in config.watch_exprs.iter().enumerate() {
            if vm_parked {
                break;
            }
            let value = match runtime
                .engine_mut()
                .execute_expression("krkr_debug_watch.tjs", expr)
            {
                Ok(value) => value
                    .to_tjs_string()
                    .unwrap_or_else(|_| format!("{value:?}")),
                Err(error) => format!("<error: {error}>"),
            };
            if watch_values[index].as_deref() != Some(value.as_str()) {
                println!("watch[{index}] frame={frame_index} {expr} = {value}");
                watch_values[index] = Some(value);
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
        for position in pending_interactive_releases.drain(..) {
            events.push(EngineEvent::CursorMoved { position });
            events.push(EngineEvent::PointerInput {
                button: PointerButton::Primary,
                state: ButtonState::Released,
            });
        }
        for position in pending_interactive_clicks.drain(..) {
            println!("interactive click press frame={frame_index} position={position:?}");
            events.push(EngineEvent::CursorMoved { position });
            events.push(EngineEvent::PointerInput {
                button: PointerButton::Primary,
                state: ButtonState::Pressed,
            });
            pending_interactive_releases.push(position);
        }
        if let Some(position) = interactive_auto_point {
            events.push(EngineEvent::CursorMoved { position });
            events.push(EngineEvent::PointerInput {
                button: PointerButton::Primary,
                state: ButtonState::Pressed,
            });
            pending_interactive_releases.push(position);
        }
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
        if (config.auto_click || interactive_auto_click)
            && matches!(runtime.engine().kag_state(), KagTaskState::WaitingClick)
        {
            runtime.engine_mut().signal_kag_click();
        }
        // A fast-forward probe can otherwise burn its entire frame budget
        // while the background image decoder is still working. Give the
        // resource worker a small scheduling window whenever the VM is
        // explicitly waiting for a resource; virtual time remains unchanged
        // until the resumed frame is actually processed.
        if *runtime.engine().kag_state() == KagTaskState::WaitingResource
            && runtime.engine().host().has_pending_resource_loads()
        {
            thread::sleep(Duration::from_millis(1));
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
                if interactive.is_some() {
                    // Keep the most recent frame available for an immediate
                    // `shot` command while paused between updates.
                    shot_commands = Some(frame.output.draw_commands.clone());
                }
                let commands = runtime.take_audio_commands();
                if config.virtual_audio {
                    queue_virtual_audio_completions(&commands, &mut pending_audio_stops);
                }
                for path in pending_interactive_shots.drain(..) {
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
                    let viewport = runtime
                        .engine()
                        .content_viewport_size()
                        .unwrap_or(Size::new(1280.0, 720.0));
                    let (width, height, rgba) = snapshot::composite_frame(
                        viewport.width.max(1.0) as u32,
                        viewport.height.max(1.0) as u32,
                        &frame.output.draw_commands,
                        &textures,
                    );
                    match snapshot::write_png(&path, width, height, &rgba) {
                        Ok(()) => println!("interactive screenshot={path}"),
                        Err(error) => println!("interactive screenshot_error={path}: {error}"),
                    }
                }
                if interactive.is_some() {
                    if let Some(budget) = interactive_budget.as_mut() {
                        *budget = budget.saturating_sub(1);
                    }
                    if let Some(condition) = interactive_until.as_ref()
                        && interactive_condition_satisfied(runtime.engine(), condition)
                    {
                        println!(
                            "interactive=until-hit condition={condition:?} frame={frame_index}"
                        );
                        interactive_until = None;
                        interactive_budget = Some(0);
                    }
                }
                if !config.quiet && interactive.is_none() && frame_index % 20 == 0 {
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
                if config.kag_state {
                    let semantic = semantic_kag_state(runtime.engine());
                    if semantic != last_kag_semantic {
                        println!(
                            "kag-state frame={frame_index} {}",
                            semantic.as_deref().unwrap_or("-")
                        );
                        last_kag_semantic = semantic;
                    }
                }
                if !config.quiet
                    && config.pixels
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

    if config.interactive {
        if let Some(condition) = interactive_until {
            println!(
                "interactive=until-timeout condition={condition:?} frame={} max_frames={}",
                config.max_frames, config.max_frames
            );
        }
        println!("interactive=limit frame={}", config.max_frames);
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
    if config.kag_state {
        println!(
            "kag-state final {}",
            semantic_kag_state(runtime.engine()).unwrap_or_else(|| "-".to_string())
        );
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

#[derive(Debug)]
enum InteractiveCommand {
    Advance(usize),
    Until {
        condition: InteractiveUntil,
        max_frames: usize,
    },
    Run,
    Pause,
    Click(Point),
    State,
    Layers(Option<String>),
    Layer(u64),
    Hit(Point),
    Draw,
    Probe,
    Auto(Option<bool>),
    AutoPoint(Option<Point>),
    Load(String),
    Logs {
        needles: Vec<String>,
        tail: usize,
    },
    Resources,
    Help,
    Shot(String),
    Expression(String),
    Members {
        expression: String,
        filter: Option<String>,
        all: bool,
    },
    Trace(TraceCommand),
    Quit,
}

/// `trace` sub-commands. Native calls are the boundary where a script value
/// turns into engine geometry, so arming a trace there is usually the fastest
/// way to find which caller passed the wrong number.
#[derive(Clone, Debug, Eq, PartialEq)]
enum TraceCommand {
    /// Print the armed patterns.
    List,
    Add(Vec<String>),
    Remove(Vec<String>),
    Clear,
    /// Print the `Class.method` names a pattern could address.
    Names(Option<String>),
}

/// Conditions understood by the deterministic frame runner.  They are kept
/// deliberately small and side-effect free so an agent can use `until` as a
/// reliable synchronization point between commands.
#[derive(Clone, Debug, Eq, PartialEq)]
enum InteractiveUntil {
    Kag(KagUntil),
    Layer(String),
    Storage(String),
    Log { needle: String, after: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum KagUntil {
    Running,
    WaitingClick,
    WaitingTimer,
    WaitingTransition,
    WaitingAudio,
    WaitingResource,
    WaitingModal,
    Finished,
    Error,
}

fn start_interactive_console() -> Receiver<InteractiveCommand> {
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("krkr-debug-console".to_string())
        .spawn(move || {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let Ok(line) = line else { break };
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let command = match parse_interactive_command(line) {
                    Ok(command) => command,
                    Err(error) => {
                        println!("interactive error: {error}");
                        continue;
                    }
                };
                let quit = matches!(command, InteractiveCommand::Quit);
                if sender.send(command).is_err() || quit {
                    break;
                }
            }
        })
        .expect("spawn interactive debugger console");
    receiver
}

fn parse_interactive_command(line: &str) -> Result<InteractiveCommand, String> {
    let (command, rest) = line
        .split_once(char::is_whitespace)
        .map(|(command, rest)| (command, rest.trim()))
        .unwrap_or((line, ""));
    match command {
        "advance" | "wait" | "tick" => {
            let frames = if rest.is_empty() {
                1
            } else {
                rest.parse::<usize>()
                    .map_err(|_| "advance count must be a non-negative integer".to_string())?
            };
            Ok(InteractiveCommand::Advance(frames))
        }
        "until" => {
            let (condition, max_frames) = parse_interactive_until(rest)?;
            Ok(InteractiveCommand::Until {
                condition,
                max_frames,
            })
        }
        "run" | "resume" => Ok(InteractiveCommand::Run),
        "pause" => Ok(InteractiveCommand::Pause),
        "click" => {
            let mut values = rest.split_whitespace();
            let x = values
                .next()
                .ok_or_else(|| "usage: click <x> <y>".to_string())?
                .parse::<f32>()
                .map_err(|_| "click x must be a number".to_string())?;
            let y = values
                .next()
                .ok_or_else(|| "usage: click <x> <y>".to_string())?
                .parse::<f32>()
                .map_err(|_| "click y must be a number".to_string())?;
            if values.next().is_some() {
                return Err("usage: click <x> <y>".to_string());
            }
            Ok(InteractiveCommand::Click(Point::new(x, y)))
        }
        "state" | "frame" => Ok(InteractiveCommand::State),
        "layers" => Ok(InteractiveCommand::Layers(
            (!rest.is_empty()).then(|| rest.to_string()),
        )),
        "layer" => {
            let id = rest
                .parse::<u64>()
                .map_err(|_| "usage: layer <id>".to_string())?;
            Ok(InteractiveCommand::Layer(id))
        }
        "hit" | "hit-test" | "hittest" => {
            let mut values = rest.split_whitespace();
            let x = values
                .next()
                .ok_or_else(|| "usage: hit <x> <y>".to_string())?
                .parse::<f32>()
                .map_err(|_| "hit x must be a number".to_string())?;
            let y = values
                .next()
                .ok_or_else(|| "usage: hit <x> <y>".to_string())?
                .parse::<f32>()
                .map_err(|_| "hit y must be a number".to_string())?;
            if values.next().is_some() {
                return Err("usage: hit <x> <y>".to_string());
            }
            Ok(InteractiveCommand::Hit(Point::new(x, y)))
        }
        "draw" | "draws" => Ok(InteractiveCommand::Draw),
        "probe" | "inspect" => Ok(InteractiveCommand::Probe),
        "auto" | "autoclick" => {
            let value = match rest {
                "" => None,
                "on" | "true" | "1" => Some(true),
                "off" | "false" | "0" => Some(false),
                _ => return Err("usage: auto [on|off]".to_string()),
            };
            Ok(InteractiveCommand::Auto(value))
        }
        "autopoint" | "auto-point" => {
            if rest.eq_ignore_ascii_case("off") || rest.eq_ignore_ascii_case("none") {
                return Ok(InteractiveCommand::AutoPoint(None));
            }
            let mut values = rest.split_whitespace();
            let x = values
                .next()
                .ok_or_else(|| "usage: autopoint <x> <y>|off".to_string())?
                .parse::<f32>()
                .map_err(|_| "autopoint x must be a number".to_string())?;
            let y = values
                .next()
                .ok_or_else(|| "usage: autopoint <x> <y>|off".to_string())?
                .parse::<f32>()
                .map_err(|_| "autopoint y must be a number".to_string())?;
            if values.next().is_some() {
                return Err("usage: autopoint <x> <y>|off".to_string());
            }
            Ok(InteractiveCommand::AutoPoint(Some(Point::new(x, y))))
        }
        "load" | "scenario" => {
            if rest.is_empty() {
                Err("usage: load <storage>".to_string())
            } else {
                Ok(InteractiveCommand::Load(rest.to_string()))
            }
        }
        "logs" => parse_logs_command(rest),
        "resources" | "pending" => {
            if !rest.is_empty() {
                Err("usage: resources".to_string())
            } else {
                Ok(InteractiveCommand::Resources)
            }
        }
        "shot" => {
            if rest.is_empty() {
                Err("usage: shot <path>".to_string())
            } else {
                Ok(InteractiveCommand::Shot(rest.to_string()))
            }
        }
        "expr" => {
            if rest.is_empty() {
                Err("usage: expr <tjs expression>".to_string())
            } else {
                Ok(InteractiveCommand::Expression(rest.to_string()))
            }
        }
        "members" | "props" => parse_members_command(rest),
        "trace" => parse_trace_command(rest),
        "q" | "quit" => Ok(InteractiveCommand::Quit),
        "help" => Ok(InteractiveCommand::Help),
        _ => Err(format!("unknown command `{command}`")),
    }
}

/// `logs [-n <tail>] [needle]...`
///
/// The host log buffer holds up to 200k lines, so an unbounded dump is
/// unusable and a single substring is a blunt instrument — `_copy` silently
/// misses `copy_rect`.  Needles are ANDed and the newest `tail` matches win.
fn parse_logs_command(rest: &str) -> Result<InteractiveCommand, String> {
    let mut needles = Vec::new();
    let mut tail = DEFAULT_LOG_TAIL;
    let mut tokens = rest.split_whitespace();
    while let Some(token) = tokens.next() {
        match token {
            "-n" | "--tail" => {
                let value = tokens
                    .next()
                    .ok_or_else(|| "usage: logs [-n <tail>] [needle]...".to_string())?;
                // `-n 0` means "no limit"; anything else is a line count.
                tail = value
                    .parse::<usize>()
                    .map_err(|_| "logs tail must be a non-negative integer".to_string())?;
                if tail == 0 {
                    tail = usize::MAX;
                }
            }
            // Needles are whitespace-separated, so quoting one is redundant —
            // but a habitual `logs "native call"` should still work rather
            // than searching for a literal quote character.
            _ => needles.push(token.trim_matches(['"', '\'']).to_string()),
        }
    }
    Ok(InteractiveCommand::Logs { needles, tail })
}

/// `members [-a] [-f <substr>] <expression>`
///
/// A KAGEX layer carries well over a hundred members, most of them methods,
/// so the default view hides callables and keeps the data fields that actually
/// describe the object's state.
fn parse_members_command(rest: &str) -> Result<InteractiveCommand, String> {
    let mut filter = None;
    let mut all = false;
    let mut remainder = rest.trim();
    loop {
        if let Some(tail) = remainder.strip_prefix("-a") {
            all = true;
            remainder = tail.trim_start();
            continue;
        }
        if let Some(tail) = remainder.strip_prefix("-f") {
            let tail = tail.trim_start();
            let (value, next) = match tail.split_once(char::is_whitespace) {
                Some((value, next)) => (value, next.trim_start()),
                None => return Err("usage: members [-a] [-f <substr>] <expression>".to_string()),
            };
            filter = Some(value.to_ascii_lowercase());
            remainder = next;
            continue;
        }
        break;
    }
    if remainder.is_empty() {
        return Err("usage: members [-a] [-f <substr>] <expression>".to_string());
    }
    Ok(InteractiveCommand::Members {
        expression: remainder.to_string(),
        filter,
        all,
    })
}

fn parse_trace_command(rest: &str) -> Result<InteractiveCommand, String> {
    let (verb, rest) = match rest.trim().split_once(char::is_whitespace) {
        Some((verb, rest)) => (verb, rest.trim()),
        None => (rest.trim(), ""),
    };
    let patterns = || {
        rest.split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let command = match verb {
        "" | "list" => TraceCommand::List,
        "add" | "on" => {
            let patterns = patterns();
            if patterns.is_empty() {
                return Err("usage: trace add <Class.method>...".to_string());
            }
            TraceCommand::Add(patterns)
        }
        "rm" | "remove" | "del" => {
            let patterns = patterns();
            if patterns.is_empty() {
                return Err("usage: trace rm <Class.method>...".to_string());
            }
            TraceCommand::Remove(patterns)
        }
        "off" | "clear" | "none" => TraceCommand::Clear,
        "names" | "ls" => TraceCommand::Names((!rest.is_empty()).then(|| rest.to_string())),
        _ => return Err("usage: trace [list|add|rm|off|names] [pattern]...".to_string()),
    };
    Ok(InteractiveCommand::Trace(command))
}

fn parse_interactive_until(rest: &str) -> Result<(InteractiveUntil, usize), String> {
    let mut values = rest.split_whitespace();
    let target = values
        .next()
        .ok_or_else(|| "usage: until <state|layer|storage> [value] [max_frames]".to_string())?;
    let (condition, max_token) = match target.to_ascii_lowercase().as_str() {
        "running" => (InteractiveUntil::Kag(KagUntil::Running), values.next()),
        "click" | "waitingclick" | "waiting-click" => {
            (InteractiveUntil::Kag(KagUntil::WaitingClick), values.next())
        }
        "timer" | "waitingtimer" | "waiting-timer" => {
            (InteractiveUntil::Kag(KagUntil::WaitingTimer), values.next())
        }
        "transition" | "waitingtransition" | "waiting-transition" => (
            InteractiveUntil::Kag(KagUntil::WaitingTransition),
            values.next(),
        ),
        "audio" | "waitingaudio" | "waiting-audio" => {
            (InteractiveUntil::Kag(KagUntil::WaitingAudio), values.next())
        }
        "resource" | "waitingresource" | "waiting-resource" => (
            InteractiveUntil::Kag(KagUntil::WaitingResource),
            values.next(),
        ),
        "modal" | "waitingmodal" | "waiting-modal" => {
            (InteractiveUntil::Kag(KagUntil::WaitingModal), values.next())
        }
        "finished" | "done" => (InteractiveUntil::Kag(KagUntil::Finished), values.next()),
        "error" => (InteractiveUntil::Kag(KagUntil::Error), values.next()),
        "layer" => {
            let filter = values
                .next()
                .ok_or_else(|| "usage: until layer <filter> [max_frames]".to_string())?;
            (InteractiveUntil::Layer(filter.to_string()), values.next())
        }
        "storage" | "image" => {
            let needle = values
                .next()
                .ok_or_else(|| "usage: until storage <substring> [max_frames]".to_string())?;
            (InteractiveUntil::Storage(needle.to_string()), values.next())
        }
        "log" => {
            let needle = values
                .next()
                .ok_or_else(|| "usage: until log <substring> [max_frames]".to_string())?;
            (
                InteractiveUntil::Log {
                    needle: needle.to_string(),
                    after: 0,
                },
                values.next(),
            )
        }
        _ => {
            return Err(
                "usage: until <running|click|timer|transition|audio|resource|modal|finished|error|layer|storage|log> [value] [max_frames]"
                    .to_string(),
            );
        }
    };
    let max_frames = max_token
        .unwrap_or("10000")
        .parse::<usize>()
        .map_err(|_| "until max_frames must be a non-negative integer".to_string())?;
    if values.next().is_some() {
        return Err("until accepts at most one max_frames value".to_string());
    }
    Ok((condition, max_frames))
}

/// Apply a command from the frame-loop control channel.  Returns `true` when
/// the caller should terminate the probe.  Inspection commands are handled
/// synchronously, while control commands only change the state consumed by
/// the next frame boundary.
#[allow(clippy::too_many_arguments)]
/// Evaluates a console expression, turning "the VM is parked" into an explicit
/// error.
///
/// While a call stack is suspended on a pending resource the VM returns `void`
/// from any new evaluation instead of running it, so during a load screen even
/// `expr 1+1` answers `void`. Reporting that as a value makes every inspection
/// look like the object is missing; naming the suspension says to advance a few
/// frames and retry.
fn evaluate_interactive_expression(
    runtime: &mut RuntimeSession,
    expression: &str,
) -> Result<Variant, String> {
    let value = runtime
        .engine_mut()
        .execute_expression("krkr_debug_interactive.tjs", expression)
        .map_err(|error| error.to_string())?;
    if matches!(value, Variant::Void) && runtime.engine().tjs_runtime().is_suspended() {
        return Err(
            "vm-suspended (a call stack is parked on a pending resource; advance frames and retry)"
                .to_string(),
        );
    }
    Ok(value)
}

fn apply_interactive_control(
    command: InteractiveCommand,
    paused: &mut bool,
    budget: &mut Option<usize>,
    until: &mut Option<InteractiveUntil>,
    pending_clicks: &mut Vec<Point>,
    pending_shots: &mut Vec<String>,
    frame_index: usize,
    runtime: &mut RuntimeSession,
    textures: &mut TextureCache,
    last_commands: Option<&[DrawCommand]>,
    auto_click: &mut bool,
    auto_point: &mut Option<Point>,
) -> bool {
    match command {
        InteractiveCommand::Advance(frames) => {
            *until = None;
            if frames == 0 {
                *paused = true;
                *budget = None;
                println!("interactive=paused frame={frame_index} (advance 0)");
            } else {
                *paused = false;
                *budget = Some(frames);
                println!("interactive=advance frames={frames} from={frame_index}");
            }
        }
        InteractiveCommand::Until {
            mut condition,
            max_frames,
        } => {
            if let InteractiveUntil::Log { after, .. } = &mut condition {
                *after = runtime.engine().host().logs().len();
            }
            if max_frames == 0 {
                *until = None;
                *paused = true;
                *budget = None;
                println!(
                    "interactive=until-timeout condition={condition:?} frame={frame_index} max_frames=0"
                );
            } else if interactive_condition_satisfied(runtime.engine(), &condition) {
                *until = None;
                *paused = true;
                *budget = None;
                println!("interactive=until-hit condition={condition:?} frame={frame_index}");
            } else {
                *until = Some(condition.clone());
                *paused = false;
                *budget = Some(max_frames);
                println!(
                    "interactive=until condition={condition:?} max_frames={max_frames} from={frame_index}"
                );
            }
        }
        InteractiveCommand::Run => {
            *until = None;
            *paused = false;
            *budget = None;
            println!("interactive=running from={frame_index}");
        }
        InteractiveCommand::Pause => {
            *until = None;
            *paused = true;
            *budget = None;
            println!("interactive=paused frame={frame_index}");
        }
        InteractiveCommand::Click(position) => {
            *until = None;
            pending_clicks.push(position);
            *paused = false;
            // A click is a press followed by a release on the next frame.
            // Running exactly two frames makes the command deterministic and
            // avoids races with KAG's click-wait state.
            *budget = Some(2);
            println!("interactive=click queued frame={frame_index} position={position:?}");
        }
        InteractiveCommand::State => {
            println!(
                "interactive state frame={frame_index} paused={paused} budget={budget:?} until={until:?} viewport={:?} kag={:?} location={:?} vm_suspended={}",
                runtime.engine().content_viewport_size(),
                runtime.engine().kag_state(),
                runtime.engine().kag_location(),
                runtime.engine().tjs_runtime().is_suspended(),
            );
        }
        InteractiveCommand::Layers(filter) => dump_layers(runtime.engine(), filter.as_deref()),
        InteractiveCommand::Layer(id) => dump_layer(runtime.engine_mut(), id),
        InteractiveCommand::Hit(position) => match runtime.engine_mut().inspect_pointer(position) {
            Ok(lines) => {
                println!("---interactive hit position={position:?}---");
                for line in lines {
                    println!("hit: {line}");
                }
            }
            Err(error) => println!("interactive hit_error position={position:?}: {error}"),
        },
        InteractiveCommand::Draw => {
            if let Some(commands) = last_commands {
                dump_draw_commands(commands);
            } else {
                println!("interactive draw=(no rendered frame)");
            }
        }
        InteractiveCommand::Probe => {
            println!(
                "interactive state frame={frame_index} paused={paused} budget={budget:?} until={until:?} viewport={:?} kag={:?} location={:?} vm_suspended={}",
                runtime.engine().content_viewport_size(),
                runtime.engine().kag_state(),
                runtime.engine().kag_location(),
                runtime.engine().tjs_runtime().is_suspended(),
            );
            dump_layers(runtime.engine(), Some("visible"));
            if let Some(commands) = last_commands {
                dump_draw_commands(commands);
            } else {
                println!("interactive draw=(no rendered frame)");
            }
        }
        InteractiveCommand::Auto(value) => {
            if let Some(value) = value {
                *auto_click = value;
            }
            println!("interactive auto_click={auto_click}");
        }
        InteractiveCommand::AutoPoint(point) => {
            *auto_point = point;
            if auto_point.is_some() {
                *paused = false;
                *budget = None;
            }
            println!("interactive auto_point={auto_point:?}");
        }
        InteractiveCommand::Load(storage) => {
            let mut candidates = vec![storage.clone()];
            if !storage.contains('/') && !storage.ends_with(".scn") {
                candidates.push(format!("scn/{storage}.scn"));
            }
            let mut last_error = None;
            for candidate in candidates {
                match runtime.engine_mut().load_kag_scenario(&candidate) {
                    Ok(()) => {
                        *until = None;
                        *paused = false;
                        *budget = Some(1);
                        println!(
                            "interactive=load storage={candidate} requested={storage} advance=1"
                        );
                        return false;
                    }
                    Err(error) => last_error = Some(error),
                }
            }
            if let Some(error) = last_error {
                println!("interactive load_error storage={storage}: {error}");
            }
        }
        InteractiveCommand::Help => println!(
            "interactive commands: advance [n], until <condition> [max], run, pause, click <x> <y>, state, layers [filter], layer <id>, hit <x> <y>, draw, probe, auto [on|off], autopoint <x> <y>|off, load <storage>, logs [-n tail] [needle]..., trace [list|add|rm|off|names] [pattern]..., resources, shot <path>, expr <tjs>, members [-a] [-f substr] <expr>, q"
        ),
        InteractiveCommand::Logs { needles, tail } => {
            let matches = runtime
                .engine()
                .host()
                .logs()
                .iter()
                .filter(|line| needles.iter().all(|needle| line.contains(needle)))
                .collect::<Vec<_>>();
            let shown = matches.len().min(tail);
            for line in &matches[matches.len() - shown..] {
                println!("log: {line}");
            }
            println!("interactive logs={shown} matched={}", matches.len());
        }
        InteractiveCommand::Resources => {
            let entries = runtime.engine().host().pending_resource_diagnostics();
            println!("---interactive resources ({})---", entries.len());
            for entry in entries {
                println!("resource: {entry}");
            }
        }
        InteractiveCommand::Shot(path) => {
            if let Some(commands) = last_commands {
                write_interactive_shot(&path, runtime.engine(), commands, textures);
            } else {
                pending_shots.push(path);
                println!("interactive=shot queued (advance once to render a frame)");
            }
        }
        InteractiveCommand::Expression(expression) => {
            match evaluate_interactive_expression(runtime, &expression) {
                Ok(value) => println!("interactive expression={value}"),
                Err(error) => println!("interactive expression_error={error}"),
            }
        }
        InteractiveCommand::Members {
            expression,
            filter,
            all,
        } => {
            match evaluate_interactive_expression(runtime, &expression) {
                Ok(Variant::Object(object)) => {
                    let tjs = runtime.engine().tjs_runtime();
                    let members = tjs.object_members(object);
                    let total = members.len();
                    let shown = members
                        .into_iter()
                        .filter_map(|(name, value)| {
                            let target = match &value {
                                Variant::Object(handle) => Some(*handle),
                                Variant::Closure(closure) => Some(closure.object),
                                _ => None,
                            };
                            let callable =
                                target.is_some_and(|handle| tjs.object_is_callable(handle));
                            let property =
                                target.is_some_and(|handle| tjs.object_is_native_property(handle));
                            let keep = (all || !callable)
                                && filter.as_deref().is_none_or(|needle| {
                                    name.to_ascii_lowercase().contains(needle)
                                });
                            keep.then_some((name, value, property))
                        })
                        .collect::<Vec<_>>();
                    println!(
                        "interactive members expression={expression:?} count={} total={total}",
                        shown.len()
                    );
                    for (name, value, property) in shown {
                        // A native property member stores its accessor, not the
                        // value; printing the accessor tells the reader nothing
                        // about the object's state.
                        let value = if property {
                            runtime
                                .engine_mut()
                                .tjs_runtime_mut()
                                .resolve_object_member(object, &name)
                                .unwrap_or(value)
                        } else {
                            value
                        };
                        println!("member {name}={} value={value}", variant_kind(&value));
                    }
                }
                Ok(value) => println!(
                    "interactive members_error=not-an-object kind={}",
                    variant_kind(&value)
                ),
                Err(error) => println!("interactive members_error={error}"),
            }
        }
        InteractiveCommand::Trace(command) => {
            apply_trace_command(runtime.engine_mut().tjs_runtime_mut(), command)
        }
        InteractiveCommand::Quit => {
            println!("interactive quit");
            return true;
        }
    }
    false
}

fn write_interactive_shot(
    path: &str,
    engine: &KrkrEngine,
    commands: &[DrawCommand],
    textures: &mut TextureCache,
) {
    // Live layer images take priority over cached uploads because a layer may
    // be updated in place without emitting a new upload event.
    for layer in engine.host().layer_tree().layers() {
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
    let viewport = engine
        .content_viewport_size()
        .unwrap_or(Size::new(1280.0, 720.0));
    let (width, height, rgba) = snapshot::composite_frame(
        viewport.width.max(1.0) as u32,
        viewport.height.max(1.0) as u32,
        commands,
        textures,
    );
    match snapshot::write_png(path, width, height, &rgba) {
        Ok(()) => println!("interactive screenshot={path} frame_size={width}x{height}"),
        Err(error) => println!("interactive screenshot_error={path}: {error}"),
    }
}

fn interactive_condition_satisfied(engine: &KrkrEngine, condition: &InteractiveUntil) -> bool {
    match condition {
        InteractiveUntil::Kag(target) => match target {
            KagUntil::Running => matches!(engine.kag_state(), KagTaskState::Running),
            KagUntil::WaitingClick => matches!(engine.kag_state(), KagTaskState::WaitingClick),
            KagUntil::WaitingTimer => {
                matches!(engine.kag_state(), KagTaskState::WaitingTimer { .. })
            }
            KagUntil::WaitingTransition => {
                matches!(engine.kag_state(), KagTaskState::WaitingTransition)
            }
            KagUntil::WaitingAudio => matches!(engine.kag_state(), KagTaskState::WaitingAudio),
            KagUntil::WaitingResource => {
                matches!(engine.kag_state(), KagTaskState::WaitingResource)
            }
            KagUntil::WaitingModal => matches!(engine.kag_state(), KagTaskState::WaitingModal),
            KagUntil::Finished => matches!(engine.kag_state(), KagTaskState::Finished),
            KagUntil::Error => matches!(engine.kag_state(), KagTaskState::Error { .. }),
        },
        InteractiveUntil::Layer(filter) => engine
            .host()
            .layer_tree()
            .layers()
            .any(|layer| layer_matches_filter(engine, layer, Some(filter))),
        InteractiveUntil::Storage(needle) => engine.host().layer_tree().layers().any(|layer| {
            layer.image.is_some()
                && engine
                    .host()
                    .layer_image_storage(layer.id)
                    .is_some_and(|storage| {
                        storage
                            .to_ascii_lowercase()
                            .contains(&needle.to_ascii_lowercase())
                    })
        }),
        InteractiveUntil::Log { needle, after } => engine
            .host()
            .logs()
            .iter()
            .skip(*after)
            .any(|line| line.contains(needle)),
    }
}

fn dump_layers(engine: &KrkrEngine, filter: Option<&str>) {
    println!("---interactive layers filter={filter:?}---");
    for layer in engine.host().layer_tree().layers() {
        if !layer_matches_filter(engine, layer, filter) {
            continue;
        }
        let absolute = engine.host().layer_tree().absolute_position(layer.id);
        let image = layer.image.as_ref().map(|image| {
            format!(
                "texture={} size={}x{} stats={:?}",
                image.upload.texture_id,
                image.upload.width,
                image.upload.height,
                snapshot::rgba_stats(image.upload.width, image.upload.height, &image.upload.rgba,)
            )
        });
        println!(
            "layer id={} name={:?} parent={:?} abs={absolute:?} z={} rect=({},{},{},{}) image_rect=({},{},{},{}) visible={} renderable={} enabled={} node_enabled={} opacity={} type={} face={} hit=({}, {}) image={:?} storage={:?}",
            layer.id,
            layer.name,
            layer.parent,
            layer.z_order,
            layer.left,
            layer.top,
            layer.width,
            layer.height,
            layer.image_left,
            layer.image_top,
            layer.image_width,
            layer.image_height,
            layer.visible,
            layer.renderable,
            layer.enabled,
            layer.node_enabled,
            layer.opacity,
            layer.layer_type,
            layer.face,
            layer.hit_type,
            layer.hit_threshold,
            image,
            engine.host().layer_image_storage(layer.id),
        );
    }
}

fn layer_matches_filter(
    engine: &KrkrEngine,
    layer: &krkr_core::LayerNode,
    filter: Option<&str>,
) -> bool {
    let Some(filter) = filter else { return true };
    match filter.to_ascii_lowercase().as_str() {
        "visible" => layer.visible && layer.renderable,
        "images" | "image" => layer.image.is_some(),
        "visible-images" | "visible_images" => {
            layer.visible && layer.renderable && layer.image.is_some()
        }
        needle => {
            layer.name.to_ascii_lowercase().contains(needle)
                || engine
                    .host()
                    .layer_image_storage(layer.id)
                    .is_some_and(|storage| storage.to_ascii_lowercase().contains(needle))
        }
    }
}

/// The name `layer <id>` binds the layer's owning TJS object to.
const LAYER_BINDING: &str = "dbgLayer";

fn dump_layer(engine: &mut KrkrEngine, id: u64) {
    let Some(layer) = engine.host().layer_tree().layer(id) else {
        println!("interactive layer_error=no such layer id={id}");
        return;
    };
    let absolute = engine.host().layer_tree().absolute_position(id);
    println!(
        "interactive layer id={} name={:?} parent={:?} abs={absolute:?} z={} rect=({},{},{},{}) image_rect=({},{},{},{}) visible={} renderable={} enabled={} node_enabled={} opacity={} type={} face={} hit=({}, {}) storage={:?}",
        layer.id,
        layer.name,
        layer.parent,
        layer.z_order,
        layer.left,
        layer.top,
        layer.width,
        layer.height,
        layer.image_left,
        layer.image_top,
        layer.image_width,
        layer.image_height,
        layer.visible,
        layer.renderable,
        layer.enabled,
        layer.node_enabled,
        layer.opacity,
        layer.layer_type,
        layer.face,
        layer.hit_type,
        layer.hit_threshold,
        engine.host().layer_image_storage(layer.id),
    );
    if let Some(image) = &layer.image {
        println!(
            "interactive layer_image texture={} size={}x{} stats={:?}",
            image.upload.texture_id,
            image.upload.width,
            image.upload.height,
            snapshot::rgba_stats(image.upload.width, image.upload.height, &image.upload.rgba,),
        );
    } else {
        println!("interactive layer_image=none");
    }
    dump_layer_script_object(engine, id);
}

/// Reports the TJS object behind a layer id and binds it to a global so it can
/// be inspected without first guessing a path through `kag.fore.base.children`.
fn dump_layer_script_object(engine: &mut KrkrEngine, id: u64) {
    let Some(object) = engine.host().native_object_for_layer(id) else {
        println!("interactive layer_object=none id={id}");
        return;
    };
    let classes = engine.tjs_runtime().object_class_infos(object).join(",");
    engine
        .tjs_runtime_mut()
        .set_global_member(LAYER_BINDING, Variant::Object(object));
    println!(
        "interactive layer_object=#{} classes=[{classes}] bound_as={LAYER_BINDING}",
        object.0
    );
}

fn dump_draw_commands(commands: &[DrawCommand]) {
    println!("---interactive draw commands={}---", commands.len());
    for (index, command) in commands.iter().enumerate() {
        match command {
            DrawCommand::Image(image) => println!(
                "draw index={index} image texture={} rect=({},{},{},{}) source=({},{},{},{}) texture_size=({},{}) opacity={:.4}",
                image.texture_id,
                image.rect.x,
                image.rect.y,
                image.rect.width,
                image.rect.height,
                image.source_rect.x,
                image.source_rect.y,
                image.source_rect.width,
                image.source_rect.height,
                image.texture_size.width,
                image.texture_size.height,
                image.opacity,
            ),
            DrawCommand::Rect(rect) => println!(
                "draw index={index} rect=({},{},{},{}) color=({:.3},{:.3},{:.3},{:.3})",
                rect.rect.x,
                rect.rect.y,
                rect.rect.width,
                rect.rect.height,
                rect.color.r,
                rect.color.g,
                rect.color.b,
                rect.color.a,
            ),
            DrawCommand::Text(text) => println!(
                "draw index={index} text position=({},{}) size={} color=({:.3},{:.3},{:.3},{:.3}) value={:?}",
                text.position.x,
                text.position.y,
                text.size,
                text.color.r,
                text.color.g,
                text.color.b,
                text.color.a,
                text.text,
            ),
        }
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

/// Arms, disarms, and reports the runtime's native call traces.
///
/// Traced calls are written to the host log, so `logs "native call"` (plus a
/// method name) reads them back, and a trace can be armed for just the frames
/// of interest instead of drowning the buffer.
fn apply_trace_command(runtime: &mut Runtime<KrkrHost>, command: TraceCommand) {
    match command {
        TraceCommand::List => {}
        TraceCommand::Add(patterns) => {
            let mut armed = runtime.native_call_traces().to_vec();
            for pattern in patterns {
                let known = runtime
                    .traceable_native_names()
                    .iter()
                    .any(|name| name.to_ascii_lowercase().contains(&pattern.to_lowercase()));
                if !known {
                    println!(
                        "interactive trace_warning pattern={pattern:?} matches-no-native-method"
                    );
                }
                armed.push(pattern);
            }
            runtime.set_native_call_traces(armed);
        }
        TraceCommand::Remove(patterns) => {
            let lowered = patterns
                .iter()
                .map(|pattern| pattern.to_ascii_lowercase())
                .collect::<Vec<_>>();
            let armed = runtime
                .native_call_traces()
                .iter()
                .filter(|pattern| !lowered.contains(pattern))
                .cloned()
                .collect::<Vec<_>>();
            runtime.set_native_call_traces(armed);
        }
        TraceCommand::Clear => runtime.set_native_call_traces(Vec::<String>::new()),
        TraceCommand::Names(filter) => {
            let needle = filter.map(|filter| filter.to_ascii_lowercase());
            let names = runtime
                .traceable_native_names()
                .into_iter()
                .filter(|name| {
                    needle
                        .as_deref()
                        .is_none_or(|needle| name.to_ascii_lowercase().contains(needle))
                })
                .collect::<Vec<_>>();
            println!("interactive trace_names={}", names.len());
            for name in names {
                println!("trace_name {name}");
            }
            return;
        }
    }
    println!(
        "interactive trace=[{}]",
        runtime.native_call_traces().join(", ")
    );
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

#[cfg(test)]
mod tests {
    use super::{DEFAULT_LOG_TAIL, InteractiveCommand, TraceCommand, parse_interactive_command};

    #[test]
    fn interactive_control_commands_are_deterministic() {
        assert!(matches!(
            parse_interactive_command("advance"),
            Ok(InteractiveCommand::Advance(1))
        ));
        assert!(matches!(
            parse_interactive_command("advance 42"),
            Ok(InteractiveCommand::Advance(42))
        ));
        assert!(matches!(
            parse_interactive_command("run"),
            Ok(InteractiveCommand::Run)
        ));
        assert!(matches!(
            parse_interactive_command("pause"),
            Ok(InteractiveCommand::Pause)
        ));
        assert!(matches!(
            parse_interactive_command("click 12.5 9"),
            Ok(InteractiveCommand::Click(_))
        ));
        assert!(matches!(
            parse_interactive_command("autopoint 640 620"),
            Ok(InteractiveCommand::AutoPoint(Some(_)))
        ));
    }

    #[test]
    fn logs_command_ands_needles_and_takes_a_tail_limit() {
        // A single substring is a blunt filter: `_copy` does not match
        // `copy_rect`, so the useful form is several independent needles.
        assert!(matches!(
            parse_interactive_command("logs -n 20 native copyRect"),
            Ok(InteractiveCommand::Logs { needles, tail })
                if needles == ["native", "copyRect"] && tail == 20
        ));
        assert!(matches!(
            parse_interactive_command("logs -n 0 affine"),
            Ok(InteractiveCommand::Logs { tail, .. }) if tail == usize::MAX
        ));
        assert!(parse_interactive_command("logs -n").is_err());
        assert!(matches!(
            parse_interactive_command(r#"logs "native call""#),
            Ok(InteractiveCommand::Logs { needles, .. }) if needles == ["native", "call"]
        ));
    }

    #[test]
    fn members_command_parses_flags_before_the_expression() {
        assert!(matches!(
            parse_interactive_command("members -a -f offset kag.fore.base"),
            Ok(InteractiveCommand::Members { expression, filter: Some(filter), all: true })
                if expression == "kag.fore.base" && filter == "offset"
        ));
    }

    #[test]
    fn trace_command_parses_subcommands() {
        assert!(matches!(
            parse_interactive_command("trace"),
            Ok(InteractiveCommand::Trace(TraceCommand::List))
        ));
        assert!(matches!(
            parse_interactive_command("trace add Layer.affineCopy Layer.copyRect"),
            Ok(InteractiveCommand::Trace(TraceCommand::Add(patterns)))
                if patterns == ["Layer.affineCopy", "Layer.copyRect"]
        ));
        assert!(matches!(
            parse_interactive_command("trace off"),
            Ok(InteractiveCommand::Trace(TraceCommand::Clear))
        ));
        assert!(matches!(
            parse_interactive_command("trace names Layer."),
            Ok(InteractiveCommand::Trace(TraceCommand::Names(Some(filter))))
                if filter == "Layer."
        ));
    }

    #[test]
    fn interactive_inspection_commands_parse_and_reject_bad_input() {
        assert!(matches!(
            parse_interactive_command("probe"),
            Ok(InteractiveCommand::Probe)
        ));
        assert!(matches!(
            parse_interactive_command("members foo.bar"),
            Ok(InteractiveCommand::Members { expression, filter: None, all: false })
                if expression == "foo.bar"
        ));
        assert!(matches!(
            parse_interactive_command("layer 7"),
            Ok(InteractiveCommand::Layer(7))
        ));
        assert!(matches!(
            parse_interactive_command("hit 1690 670"),
            Ok(InteractiveCommand::Hit(_))
        ));
        assert!(matches!(
            parse_interactive_command("auto off"),
            Ok(InteractiveCommand::Auto(Some(false)))
        ));
        assert!(matches!(
            parse_interactive_command("autopoint off"),
            Ok(InteractiveCommand::AutoPoint(None))
        ));
        assert!(matches!(
            parse_interactive_command("logs affine"),
            Ok(InteractiveCommand::Logs { needles, tail })
                if needles == ["affine"] && tail == DEFAULT_LOG_TAIL
        ));
        assert!(parse_interactive_command("advance nope").is_err());
        assert!(parse_interactive_command("click 1").is_err());
        assert!(parse_interactive_command("members").is_err());
        assert!(parse_interactive_command("trace add").is_err());
        assert!(parse_interactive_command("trace nope pattern").is_err());
        assert!(matches!(
            parse_interactive_command("until waiting-click 240"),
            Ok(InteractiveCommand::Until {
                condition: super::InteractiveUntil::Kag(super::KagUntil::WaitingClick),
                max_frames: 240,
            })
        ));
        assert!(matches!(
            parse_interactive_command("until storage stand 1200"),
            Ok(InteractiveCommand::Until {
                condition: super::InteractiveUntil::Storage(needle),
                max_frames: 1200,
            }) if needle == "stand"
        ));
        assert!(matches!(
            parse_interactive_command("pending"),
            Ok(InteractiveCommand::Resources)
        ));
        assert!(matches!(
            parse_interactive_command("until log affine_copy 600"),
            Ok(InteractiveCommand::Until {
                condition: super::InteractiveUntil::Log { needle, after: 0 },
                max_frames: 600,
            }) if needle == "affine_copy"
        ));
        assert!(parse_interactive_command("until layer").is_err());
        assert!(parse_interactive_command("until running 1 2").is_err());
    }
}
