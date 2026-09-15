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
//!                           order; repeatable); while the VM is parked the
//!                           script waits for the first frame it runs again,
//!                           and a script that never runs before the frame
//!                           budget ends fails the run
//!   --click <n,x,y>         inject a click at (x, y) on frame n, release on
//!                           frame n+1 (repeatable)
//!   --move <n,x,y>          move the cursor to (x, y) on frame n without
//!                           pressing (repeatable); hover probes need the
//!                           cursor to rest on a control across many frames
//!                           without activating it
//!   --auto-click            automatically confirm [l]/[p] click waits
//!   --kag-state             log semantic KAG state transitions: the game's
//!                           own `kag` object (storage/label/line/conductor
//!                           status/wait keys), unlike `kag=` in the frame
//!                           log which reflects the engine-side session
//!   --kag-click <frame>     semantic click on frame n (repeatable): a
//!                           player's primary click, delivered through the
//!                           engine's input dispatch (`kag.onPrimaryClick`,
//!                           or the layer under a cursor that was placed),
//!                           so an exception escaping the game's own handler
//!                           is contained exactly like every other event's; a
//!                           sleeping `[s]` conductor is woken through the
//!                           game's own `waitClick`, and a click is a no-op
//!                           when the game waits on neither
//!   --kag-auto-click        keep issuing the semantic click on every frame
//!                           the game's own conductor parks on a click wait,
//!                           the KAGEX counterpart of --auto-click
//!   --watch-expr <expr>     evaluate a TJS expression every frame and log it
//!                           whenever its value changes (repeatable)
//!   --expr <expr>           evaluate a TJS expression after the frame loop
//!   --shot <path>           write a screenshot PNG: the composited frame
//!                           (live tree plus every active transition)
//!   --shot-raw              write the historical raw view instead: draw
//!                           commands only, no transition composite
//!   --shot-frame <n>        capture frame output at frame n (default last)
//!   --pixels                print per-image pixel statistics while running
//!   --layers                dump the layer tree at the end
//!   --dump-global <name>    dump a global variable's members at the end
//!   --dump-storage <name>   print a storage file's contents and exit; the
//!                           bytes are written verbatim, so a redirect keeps
//!                           every binary pack byte-exact
//!   --dump-auto-paths       print the auto search paths the storage holds,
//!                           in declaration order (last wins), after startup
//!   --dump-layer-images <dir>
//!                           write one PNG per layer image at the end,
//!                           creating the directory when it does not exist; a
//!                           file that cannot be written prints
//!                           `layer-image error:` and fails the run
//!   --logs                  dump host logs at the end
//!   --trace <cats>          enable trace categories: audio,kag or all
//!                           (same syntax as the KRKR_TRACE env var)
//!   --trace-call <pattern>  log calls into a native method from startup on
//!                           (e.g. `Layer.affineCopy`; repeatable)
//!   --immediate-transitions finish every transition in the frame it starts
//!                           (default: the engine's real frame clock; see the
//!                           policy note in `main`)
//!   --time-scale <f>        virtual clock multiplier (default 1.0)
//!   --realtime              sleep per frame instead of fast-forwarding
//!   --virtual-audio         consume audio commands without an output device
//!                           (default: decode and play through the real audio
//!                           system, like krkr-desktop; the silent sink
//!                           synthesizes the completion events instead)
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
//!   shot [--raw] <path>      write the last rendered frame as a PNG; the
//!                           default view composites active transitions,
//!                           `--raw` keeps draw commands only
//!   expr <tjs>              evaluate an expression in the global context
//!   members [-a] [-f <substr>] <expr>
//!                           list an object's data members (`-a` also shows
//!                           methods, `-f` filters by name)
//!   textrender [--class <name>] [--size <px>] [--width <px>] <text>
//!                           render message text through a TextRender class
//!                           (default TextRenderBase) and print one line per
//!                           character record plus getKeyWait() entries; the
//!                           text is verbatim, so `\n`/`\k` stay the message
//!                           format's escapes
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

use krkr_debug::{
    console::*,
    inject::{AtFrameScripts, AtScriptOutcome, AtScriptUnfinished, VM_SUSPENDED},
    snapshot,
};

use std::{
    collections::VecDeque,
    io::Write,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use krkr_assets::{NativeAssetStore, ProjectStorage};
use krkr_audio::{AudioError, AudioEvent, AudioSink, AudioSystem, PcmTap, VirtualAudioSink};
use krkr_core::{
    AudioCommand, AudioInstanceId, ButtonState, DrawCommand, EngineEvent, FrameInput, FrameOutput,
    Point, PointerButton, Size,
};
use krkr_debug::snapshot::TextureCache;
use krkr_engine::{
    EngineConfig, EngineInput, KagTaskState, KrkrEngine, KrkrHost, RuntimeSession, TransitionPolicy,
};
use krkr_tjs2::runtime::{ObjectHandle, Runtime, Variant};

use krkr_debug::cli::{BreakpointSpec, CliDebugger, parse_breakpoint_spec};

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
    moves: Vec<(usize, Point)>,
    auto_click: bool,
    kag_clicks: Vec<usize>,
    kag_auto_click: bool,
    kag_state: bool,
    watch_exprs: Vec<String>,
    expr: Option<String>,
    shot: Option<String>,
    shot_raw: bool,
    shot_frame: Option<usize>,
    pixels: bool,
    layers: bool,
    dump_globals: Vec<String>,
    dump_storages: Vec<String>,
    dump_auto_paths: bool,
    dump_layer_images: Option<String>,
    logs: bool,
    immediate_transitions: bool,
    time_scale: f64,
    realtime: bool,
    virtual_audio: bool,
    interactive: bool,
    quiet: bool,
    trace: Option<String>,
    trace_calls: Vec<String>,
    max_frames: usize,
}

fn next_arg(args: &mut impl Iterator<Item = String>, flag: &str) -> String {
    args.next()
        .unwrap_or_else(|| panic!("{flag} requires a value"))
}

fn parse_args() -> Config {
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from(args: impl Iterator<Item = String>) -> Config {
    let mut config = Config {
        max_frames: 100_000,
        time_scale: 1.0,
        ..Config::default()
    };
    let mut args = args;
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
            "--move" => {
                let value = next_arg(&mut args, "--move");
                let parts: Vec<&str> = value.split(',').collect();
                let [frame, x, y] = parts.as_slice() else {
                    panic!("--move expects <frame>,<x>,<y>");
                };
                config.moves.push((
                    frame.parse().expect("--move frame must be a number"),
                    Point::new(
                        x.parse().expect("--move x must be a number"),
                        y.parse().expect("--move y must be a number"),
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
            "--shot-raw" => config.shot_raw = true,
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
            "--dump-auto-paths" => config.dump_auto_paths = true,
            "--dump-layer-images" => {
                config.dump_layer_images = Some(next_arg(&mut args, "--dump-layer-images"));
            }
            "--logs" => config.logs = true,
            "--immediate-transitions" => config.immediate_transitions = true,
            // The flag that used to request this behavior before it was the
            // default; keep accepting it so existing probe scripts do not die.
            "--timed-transitions" => {}
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

/// True when the game's own KAG conductor is parked on a click wait.
/// `--kag-auto-click` uses this to skip the click on the frames where it
/// would be a no-op, which matters because a scene runs for tens of
/// thousands of probe frames.  The sleeping `[s]` state is left to an
/// explicit `--kag-click`, so an auto-driven run never turns every frame of
/// a sleep into a click.
fn kag_awaits_click(engine: &KrkrEngine) -> bool {
    let runtime = engine.tjs_runtime();
    // The game builds `global.kag` and its members from `new` results, so the
    // reader has to unwrap the self-bound closure to reach the object identity.
    let Some(kag) = runtime.global_member("kag").object_handle() else {
        return false;
    };
    let Some(conductor) = runtime.object_member(kag, "conductor").object_handle() else {
        return false;
    };
    if !matches!(
        runtime.object_member(conductor, "status"),
        Variant::Integer(2)
    ) {
        return false;
    }
    let Some(wait_until) = runtime
        .object_member(conductor, "waitUntil")
        .object_handle()
    else {
        return false;
    };
    runtime
        .object_members(wait_until)
        .iter()
        .any(|(name, _)| name == "click")
}

/// True while the game's own KAG window sits in the `[s]` sleep state: the
/// conductor has stopped without establishing a wait, and a player's click
/// is the only thing that resumes it.
fn kag_in_sleep(engine: &KrkrEngine) -> bool {
    let runtime = engine.tjs_runtime();
    let Some(kag) = runtime.global_member("kag").object_handle() else {
        return false;
    };
    runtime.object_member(kag, "inSleep").is_truthy()
}

/// The `[s]` sleep wake — the one semantic-click action the engine's input
/// dispatch has no entry for.
///
/// A conductor that stopped through `[s]` (KAGEX's `s` handler sets `inSleep`
/// and returns -1) waits on no signal key.  A primary click that carries no
/// cursor reaches no layer (`layer_at_cursor`, `engine.rs:2699`, is `None`)
/// and the window handlers need a position too (`dispatch_window_pointer_event`,
/// :2316), so its only entry is `kag.onPrimaryClick` — and KAGEX's own handler
/// acts on that only while the conductor waits, so nothing wakes the sleep.
///
/// A *positioned* click can wake one, but only where the game has a control
/// that does the waking, and the harness cannot pick such a point.  On
/// PARQUET's title a click at (227,438) wakes `title.ks@*wait:35` through the
/// layer under it (layer 294, local (175,28), `pressed=Some(294)` on the
/// release that advances the label to `:68`), while (960,540) — layer 10, also
/// script-handled — plus (100,100) and (1850,1050) leave it parked.  The
/// semantic click is defined in game terms instead of screen coordinates, so
/// it asks the game for the wait its own `waitClick` builds and fires it, the
/// wake this flag has always used.  It is a game-side synthesis (the script
/// runs the game's own two functions), so a raise from one of them is
/// reported as the harness error it is; the click a game waits on goes
/// through the engine instead.
const KAG_SLEEP_WAKE_SOURCE: &str = r#"
(function() {
    var k = global.kag;
    if (typeof k != "Object") return "no-kag";
    var c = typeof k.conductor == "Object" ? k.conductor : void;
    var stor = typeof k.currentStorage == "String" ? k.currentStorage : "-";
    var line = c != void ? c.curLine : -1;
    // KAG3's waitClick(elm) ignores elm, but pass a dictionary so a KAGEX
    // override that reads elm members cannot throw on a string.
    if (k.waitClick != void) k.waitClick(%[]);
    if (c != void && c.status == c.mWait) {
        c.trigger("click");
        return "wake-sleep " + stor + ":" + line;
    }
    return "sleep-no-wait " + stor + ":" + line;
})()
"#;

/// True when the game's KAG object exposes the primary-click entry a click no
/// layer claims goes to.  The check mirrors `fire_kag_primary_click`
/// (`engine.rs:2552-2567`) exactly, including the unwrap of the self-bound
/// closure the game stores in `global.kag`.
fn kag_has_primary_click(engine: &KrkrEngine) -> bool {
    let runtime = engine.tjs_runtime();
    let Some(kag) = runtime.global_member("kag").object_handle() else {
        return false;
    };
    !matches!(runtime.object_member(kag, "onPrimaryClick"), Variant::Void)
}

/// True when the engine's input dispatch has an entry a primary click can
/// reach.
///
/// `handle_input_events` posts the click to the layer under the cursor
/// (`layer_at_cursor`, `engine.rs:1996`/`:2016`, whose `onMouseDown`,
/// `onMouseUp` and `onClick` the engine calls through `call_event_method`),
/// and falls back to `kag.onPrimaryClick` for a click no layer claims
/// (`fire_kag_primary_click`, `:2552`).  With no cursor placed and no
/// `onPrimaryClick` the engine has nowhere to deliver the click at all, and
/// the harness says so rather than arming one that can only be dropped —
/// whether the layer under a placed cursor actually has a handler is the
/// engine's own hit test to make (it may run the game's `onHitTest`), so this
/// mirrors the entries, not the per-layer outcome.
fn click_has_target(engine: &KrkrEngine, cursor: Option<Point>) -> bool {
    cursor.is_some() || kag_has_primary_click(engine)
}

/// `storage:line st=<status>` for a click report: enough of
/// [`semantic_kag_state`] to tell which wait a click acted on.
fn kag_click_context(engine: &KrkrEngine) -> String {
    let runtime = engine.tjs_runtime();
    let Some(kag) = runtime.global_member("kag").object_handle() else {
        return "-".to_string();
    };
    let conductor = runtime.object_member(kag, "conductor").object_handle();
    let member = |name: &str| match conductor {
        Some(object) => runtime.object_member(object, name),
        None => Variant::Void,
    };
    let line = match member("curLine") {
        Variant::Integer(line) => line.to_string(),
        _ => "-".to_string(),
    };
    let status = match member("status") {
        Variant::Integer(status) => status_name(status),
        _ => "-".to_string(),
    };
    let storage = match runtime.object_member(kag, "currentStorage") {
        Variant::String(storage) => storage,
        _ => "-".to_string(),
    };
    format!("{storage}:{line} st={status}")
}

/// Reports and arms one semantic click on the frame's input, and returns
/// true when the click was armed.
///
/// Semantic click: the accessibility-style way to advance the game, defined
/// in game terms instead of screen coordinates. It is the player's primary
/// click, and it reaches the game the way the engine delivers one — as the
/// input the engine's own dispatch handles — instead of the harness running
/// the game's conductor itself.
///
/// `KrkrEngine::fire_kag_primary_click` (`engine.rs:2552`) is where a primary
/// click reaches the game's own KAG object: it posts `kag.onPrimaryClick`
/// through `call_event_method`, and that boundary (`engine.rs:2364-2403`)
/// hands an exception escaping the handler to `System.exceptionHandler`,
/// logs, and carries on — the reference's
/// `TVP_CATCH_AND_SHOW_SCRIPT_EXCEPTION` disposition. Evaluating
/// `conductor.trigger("click")` as a bare expression skipped that boundary,
/// so a handler raise the real game contains ended the click as a debugger
/// error and a headless run parked on an exception the game survives
/// (M220's follow-up, M223).
///
/// A click only happens when the game is actually waiting on one — the
/// conductor parked on a `click` wait, or the `[s]` sleep state — so a
/// `--kag-click` on a frame where the game is not waiting stays the no-op it
/// has always been.  What the game receives is the engine's primary click: a
/// press and release that moves no cursor, so a placed cursor (`--click`,
/// `--move`, the interactive console) selects the layer under it and that
/// layer's own handler takes the click — the same target a player's click
/// there would select, and one whose raise the engine contains the same way —
/// while on a project with no cursor the click reaches `kag.onPrimaryClick`
/// (`fire_kag_primary_click`).  [`click_has_target`] reports the case where
/// the engine has neither.  A sleeping `[s]` conductor keeps
/// [`KAG_SLEEP_WAKE_SOURCE`].  `quiet` suppresses only the success lines, as
/// `--kag-auto-click` has always behaved.
fn arm_semantic_click(
    runtime: &mut RuntimeSession,
    frame_index: usize,
    label: &str,
    quiet: bool,
    cursor: Option<Point>,
) -> bool {
    let context = kag_click_context(runtime.engine());
    if kag_awaits_click(runtime.engine()) {
        if !click_has_target(runtime.engine(), cursor) {
            // Neither entry exists: naming it beats arming a click the engine
            // would drop, and beats the harness calling the game's conductor
            // itself (the divergence this replaced).
            println!(
                "{label} frame={frame_index} -> no click target: no cursor is placed and the game's kag has no onPrimaryClick, so the engine's input dispatch has nowhere to deliver the click ({context})"
            );
            return false;
        }
        if !quiet {
            println!("{label} frame={frame_index} -> engine primary click ({context})");
        }
        return true;
    }
    if kag_in_sleep(runtime.engine()) {
        match runtime
            .engine_mut()
            .execute_expression("krkr_debug_kag_sleep_wake.tjs", KAG_SLEEP_WAKE_SOURCE)
        {
            Ok(value) => {
                if !quiet {
                    println!("{label} frame={frame_index} -> {value}");
                }
            }
            Err(error) => println!("{label} frame={frame_index} error: {error}"),
        }
        return false;
    }
    println!("{label} frame={frame_index} -> not waiting ({context})");
    false
}

/// Reads the game's own KAG layer (`global.kag`, the KAG3/KAGEX object) rather
/// than the engine-side KAG session. The engine session reports `Finished`
/// while the game is sitting at its title screen, so this is the state that
/// reflects what the player actually sees.
fn semantic_kag_state(engine: &KrkrEngine) -> Option<String> {
    let runtime = engine.tjs_runtime();
    // The game stores `global.kag`, the conductor and the wait set as `new`
    // results (self-bound closures); each step needs the object behind the
    // binding, not the raw member shape.
    let kag = runtime.global_member("kag").object_handle()?;
    let member = |object: ObjectHandle, name: &str| runtime.object_member(object, name);
    let as_int = |object: ObjectHandle, name: &str| match member(object, name) {
        Variant::Integer(value) => Some(value),
        _ => None,
    };
    let as_string = |object: ObjectHandle, name: &str| match member(object, name) {
        Variant::String(value) => Some(value),
        _ => None,
    };
    let conductor = member(kag, "conductor").object_handle();
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
        .and_then(|object| member(object, "waitUntil").object_handle())
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

/// Resolves a `--dump-globals` path (`kag.conductor.waitUntil`). Every step
/// unwraps through `object_handle()`: a value the game stored from `this` or
/// from `new` is a self-bound closure, not a plain object.
fn global_member_path(runtime: &Runtime<KrkrHost>, name: &str) -> Variant {
    let mut parts = name.split('.');
    let mut current = runtime.global_member(parts.next().unwrap());
    for part in parts {
        let Some(object) = current.object_handle() else {
            break;
        };
        current = runtime.object_member(object, part);
    }
    current
}

/// How the run ended when the unfinished injections are reported.
///
/// A requested termination is not the frame budget running out — the message
/// must not claim it is: an operator's `q` at frame 5 has 99995 frames left.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunEnd {
    /// The frame loop ran out its `--max-frames` budget.
    FrameBudget,
    /// The run stopped on request (an interactive `q`, a debugger quit).
    Requested,
}

/// Names every `--at-frame`/`--at-script` request the run ended without
/// running, with the cause the queue recorded — the VM stayed parked at the
/// request's frame — or, for a request whose frame never arrived, the reason
/// the run ended (`end`). Returns true when something was reported, so the
/// normal end of the run can exit non-zero; the deliberate terminations (an
/// interactive `q`, a debugger quit) only report -- their exit code stays what
/// it was.
fn report_unfinished_at_scripts(scripts: &AtFrameScripts, max_frames: usize, end: RunEnd) -> bool {
    let mut any = false;
    for unfinished in scripts.unfinished() {
        match unfinished {
            AtScriptUnfinished::VmStayedParked { requested_frame } => println!(
                "at-frame error: script for frame={requested_frame} never ran: {VM_SUSPENDED}"
            ),
            AtScriptUnfinished::BudgetEnded { requested_frame } => match end {
                RunEnd::FrameBudget => println!(
                    "at-frame error: script for frame={requested_frame} never ran: the frame budget (--max-frames {max_frames}) ended first"
                ),
                RunEnd::Requested => println!(
                    "at-frame error: script for frame={requested_frame} never ran: the run ended before that frame"
                ),
            },
        }
        any = true;
    }
    any
}

/// The transition policy the headless shell asks the engine for.
///
/// Transitions run on the engine's real frame clock — the windowed shell's
/// behavior — unless the caller explicitly asks for the immediate policy,
/// which a probe can be killed by without any visible symptom: it finishes the
/// transition inside `Layer.beginTransition`, so `onTransitionCompleted` is
/// delivered before the script that started the transition has returned.
/// KAGEX registers the wait that completion satisfies in that same script turn
/// (right after beginning the transition), keyed `trans_<layer>` /
/// `trans_<layer>_arg`; a completion that fires first leaves the conductor
/// parked on a wait no later event can satisfy — the restored title ignores
/// every click while every screenshot still looks right.  The reference never
/// completes a transition inside that call: it clamps the `time` option to a
/// minimum of 2 ms (`visual/TransIntf.cpp:530`, `:768`) and only stops the
/// transition on an update tick (`visual/LayerIntf.cpp:6338`, `:6420`).
fn requested_transition_policy(config: &Config) -> Option<TransitionPolicy> {
    config
        .immediate_transitions
        .then_some(TransitionPolicy::Immediate)
}

fn main() {
    let config = parse_args();
    let root = config
        .root
        .clone()
        .expect("usage: krkr-debug <game_dir> [-b spec]... [options]");

    let storage = ProjectStorage::for_root(&root).expect("storage");
    // The engine takes its own `Arc`; this handle stays with the debugger so
    // diagnostics can read storage state the engine interface does not expose.
    let storage_probe = storage.clone();
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
    if let Some(policy) = requested_transition_policy(&config) {
        engine.host_mut().set_transition_policy(policy);
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

    // The audio wiring mirrors `krkr-desktop` (its `AudioSystem::new` /
    // `prepare` / `set_resource_provider` sequence) unless `--virtual-audio`
    // asks for the silent sink explicitly. The audio system is prepared
    // before the session takes it, and the tap is kept so the end of the run
    // can report what was actually decoded.
    let audio_degraded = Arc::new(AtomicBool::new(false));
    let virtual_audio = config.virtual_audio;
    let (audio, audio_note, audio_tap): (Box<dyn AudioSink>, String, Option<PcmTap>) =
        if virtual_audio {
            (
                Box::new(DebugAudioSink::Virtual(VirtualAudioSink::default())),
                "audio sink=virtual (silent: commands are consumed and their completions synthesized, nothing decodes)"
                    .to_string(),
                None,
            )
        } else {
            let mut audio = AudioSystem::new();
            let tap = audio.pcm_tap();
            let note = match audio.prepare() {
                Ok(()) => match audio.set_resource_provider(engine.host().resource_provider()) {
                    Ok(()) => "audio sink=system (decoding)".to_string(),
                    Err(error) => format!(
                        "audio sink=system provider_error={error}; nothing decodes (`--virtual-audio` selects the silent sink)"
                    ),
                },
                Err(error) => format!(
                    "audio sink=system prepare_error={error}; nothing decodes (`--virtual-audio` selects the silent sink)"
                ),
            };
            (
                Box::new(DebugAudioSink::System {
                    system: audio,
                    degraded: Arc::clone(&audio_degraded),
                }),
                note,
                Some(tap),
            )
        };
    let mut runtime = RuntimeSession::new(
        engine,
        Box::new(NativeAssetStore::new(root.clone())),
        audio,
        Box::new(krkr_core::VirtualClock::default()),
    );
    println!("{audio_note}");

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

    if config.dump_auto_paths {
        let auto_paths = storage_probe.auto_paths();
        println!("---auto-paths count={}---", auto_paths.len());
        for path in &auto_paths {
            println!("auto-path: {path}");
        }
    }

    for storage in &config.dump_storages {
        let bytes = runtime
            .engine()
            .host()
            .read_binary_storage(storage)
            .expect("storage dump");
        println!("---storage {storage} bytes={}---", bytes.len());
        // Write the bytes verbatim. `String::from_utf8_lossy` would corrupt
        // every binary pack (a PBD, an XP3 segment, a `TJS/ns0` payload), so a
        // redirected dump could no longer be compared or decoded byte-exactly.
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&bytes).expect("storage dump write");
        stdout.flush().expect("storage dump flush");
    }

    // `--before-script`/`--before-expr` run before the frame loop, where a
    // parked VM swallows both (they return `void` instead of running). There
    // is no earlier frame to retry on, so refuse loudly rather than report
    // success for setup that never happened.
    if runtime.engine().is_script_suspended()
        && (config.before_script.is_some() || config.before_expr.is_some())
    {
        let option = if config.before_script.is_some() {
            "--before-script"
        } else {
            "--before-expr"
        };
        println!("{option} error: {VM_SUSPENDED}; the script was not executed");
        dump_logs(runtime.engine());
        dump_stub_calls(runtime.engine());
        std::process::exit(1);
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
    // The most recent frame output, kept whole (transitions included) so a
    // `shot`/`draw` can show the composited frame rather than just the live
    // draw list.
    let mut last_frame_output: Option<FrameOutput> = None;
    let mut pending_audio_stops: Vec<AudioInstanceId> = Vec::new();
    // The last non-info status the sink itself reported (a codec that could
    // not load, a backend that could not open a device), named in the end-of-
    // run audio verdict.
    let mut audio_last_report: Option<String> = None;
    let mut pending_interactive_releases = Vec::new();
    let mut pending_interactive_clicks = Vec::new();
    // `(path, raw)`: a shot issued while no frame is available yet.
    let mut pending_interactive_shots: Vec<(String, bool)> = Vec::new();
    let mut interactive_paused = interactive.is_some();
    let mut interactive_budget: Option<usize> = None;
    let mut interactive_auto_click = config.auto_click;
    let mut interactive_auto_point: Option<Point> = None;
    let mut interactive_until: Option<InteractiveUntil> = None;
    let mut deferred_interactive_commands = VecDeque::new();
    let mut last_kag_semantic: Option<String> = None;
    let mut watch_values: Vec<Option<String>> = vec![None; config.watch_exprs.len()];
    // The `--at-frame`/`--at-script` queue: a request whose frame arrives
    // while the VM is parked waits there for the first frame the VM runs
    // again (see `krkr_debug::inject`), so this is the record of which
    // injections are still outstanding.
    let mut at_scripts = AtFrameScripts::new(
        config
            .at_frames
            .iter()
            .map(|(frame, script)| (*frame, script.clone().expect("checked in parse_args")))
            .collect(),
    );
    let mut injection_failed = false;
    let mut kag_auto_click_parked = false;
    // The cursor the engine holds: every `CursorMoved` the harness puts on a
    // frame is applied in the order the events are built, so replaying this
    // frame's list at the end of the frame keeps the value the engine has
    // when the next frame's click is armed (the semantic click's own events
    // carry no position and are processed first).
    let mut pointer_position: Option<Point> = None;
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
                    if apply_interactive_control_with_frame(
                        command,
                        &mut interactive_paused,
                        &mut interactive_budget,
                        &mut interactive_until,
                        &mut pending_interactive_clicks,
                        &mut pending_interactive_shots,
                        frame_index,
                        &mut runtime,
                        &mut textures,
                        last_frame_output.as_ref(),
                        &mut interactive_auto_click,
                        &mut interactive_auto_point,
                    ) {
                        report_unfinished_at_scripts(
                            &at_scripts,
                            config.max_frames,
                            RunEnd::Requested,
                        );
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
                        if apply_interactive_control_with_frame(
                            command,
                            &mut interactive_paused,
                            &mut interactive_budget,
                            &mut interactive_until,
                            &mut pending_interactive_clicks,
                            &mut pending_interactive_shots,
                            frame_index,
                            &mut runtime,
                            &mut textures,
                            last_frame_output.as_ref(),
                            &mut interactive_auto_click,
                            &mut interactive_auto_point,
                        ) {
                            report_unfinished_at_scripts(
                                &at_scripts,
                                config.max_frames,
                                RunEnd::Requested,
                            );
                            dump_stub_calls(runtime.engine());
                            return;
                        }
                    }
                    break;
                }
            }
        }
        // Deferred injections run before this frame's own, so a script never
        // overtakes an earlier one; while the VM is parked nothing runs and
        // the queue keeps waiting.
        for outcome in at_scripts.inject(runtime.engine_mut(), frame_index) {
            match outcome {
                AtScriptOutcome::Ran {
                    requested_frame,
                    frame,
                    deferred,
                } => {
                    if deferred {
                        println!(
                            "at-frame script frame={requested_frame} executing at frame={frame} (deferred while the VM was parked)"
                        );
                    } else {
                        println!("executing at frame={frame}");
                    }
                }
                AtScriptOutcome::Deferred { requested_frame } => {
                    println!(
                        "at-frame script frame={requested_frame} deferred: {VM_SUSPENDED}; it runs on the first frame the VM runs again"
                    )
                }
            }
        }
        // The semantic click is armed onto this frame's input (the events
        // built further down) and dispatched by the engine, so the game's own
        // click handler runs inside `call_event_method`'s boundary — see
        // `arm_semantic_click`.  The `[s]` sleep wake stays game-side and runs
        // right here.
        let mut semantic_click = false;
        for click_frame in &config.kag_clicks {
            if frame_index == *click_frame {
                // A click is an action tied to its frame, not setup that can
                // wait a few frames: while the VM is parked the input is not
                // dispatched to the game either, so name that and fail the run
                // instead of dropping the click silently.
                if runtime.engine().is_script_suspended() {
                    println!(
                        "kag-click frame={frame_index} error: {VM_SUSPENDED}; the click was not attempted"
                    );
                    injection_failed = true;
                    continue;
                }
                semantic_click |= arm_semantic_click(
                    &mut runtime,
                    frame_index,
                    "kag-click",
                    false,
                    pointer_position,
                );
            }
        }
        // A parked VM cannot dispatch the click either, so the automatic
        // click skips until the VM runs again. The notice is printed once per
        // park instead of on every frame of a load window.
        if config.kag_auto_click {
            if runtime.engine().is_script_suspended() {
                if !kag_auto_click_parked {
                    kag_auto_click_parked = true;
                    println!("kag-auto-click frame={frame_index} skipped: {VM_SUSPENDED}");
                }
            } else {
                kag_auto_click_parked = false;
                if kag_awaits_click(runtime.engine())
                    && arm_semantic_click(
                        &mut runtime,
                        frame_index,
                        "kag-auto-click",
                        config.quiet,
                        pointer_position,
                    )
                {
                    semantic_click = true;
                }
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
        if semantic_click {
            // A player's primary click.  No `CursorMoved` is sent, so on a
            // project whose cursor was never placed the engine's hit test
            // finds no layer and `fire_kag_primary_click`
            // (`engine.rs:2538`, `:2552`) hands the click to the game's KAG
            // object itself; a cursor an earlier `--click`/`--move` left over
            // a layer selects that layer's own handler instead.  Either way
            // the game's handler runs inside `call_event_method`'s boundary,
            // which contains its exception.
            for state in [ButtonState::Pressed, ButtonState::Released] {
                events.push(EngineEvent::PointerInput {
                    button: PointerButton::Primary,
                    state,
                });
            }
        }
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
        for (move_frame, position) in &config.moves {
            if frame_index == *move_frame {
                println!("move at frame={frame_index} position={position:?}");
                events.push(EngineEvent::CursorMoved {
                    position: *position,
                });
            }
        }
        // Replay this frame's own cursor moves onto the tracked position, so
        // the next frame's semantic click sees what the engine will hold when
        // its press-and-release (which carries no position and is processed
        // first) is dispatched.
        for event in &events {
            if let EngineEvent::CursorMoved { position } = event {
                pointer_position = Some(*position);
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
                // The decoder's own diagnostics (a backend that could not
                // open an output device, a codec that could not load) reach
                // the console the same way `krkr-desktop` shows them; a
                // silently quiet audio path is what made the reviewed wiring
                // invisible.
                for event in &runtime_frame.audio {
                    if let krkr_core::AudioEvent::Status(status) = event {
                        println!("audio status={:?} {}", status.level, status.message);
                        // The backend can die before the first command (no
                        // output device), where nothing fails synchronously:
                        // the error only exists in this event, and the end of
                        // the run must not then look like a healthy sink that
                        // happened to play nothing.
                        audio_last_report = Some(status.message.clone());
                    }
                }
                let frame = runtime_frame.engine;
                for upload in &frame.output.image_uploads {
                    textures.insert(
                        upload.texture_id,
                        (upload.width, upload.height, upload.rgba.clone()),
                    );
                }
                if config.shot.is_some() && frame_index == shot_frame {
                    last_frame_output = Some(frame.output.clone());
                }
                if interactive.is_some() {
                    // Keep the most recent frame available for an immediate
                    // `shot` command while paused between updates.
                    last_frame_output = Some(frame.output.clone());
                }
                // A real sink executes its commands and returns none here (its
                // `PlaybackStopped` events complete the waits inside
                // `RuntimeSession::update`); the silent sink hands over what
                // it stored, and the harness synthesizes their completions.
                let commands = runtime.take_audio_commands();
                if !commands.is_empty() {
                    queue_virtual_audio_completions(&commands, &mut pending_audio_stops);
                }
                for (path, raw) in pending_interactive_shots.drain(..) {
                    write_interactive_shot(
                        &path,
                        runtime.engine(),
                        &frame.output,
                        &mut textures,
                        raw,
                    );
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
                report_unfinished_at_scripts(&at_scripts, config.max_frames, RunEnd::Requested);
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

    // An injection the run never got to is a failed probe: name every one of
    // them with the cause the queue recorded (the run ends non-zero below)
    // instead of leaving a silent gap where the script should have run.
    if report_unfinished_at_scripts(&at_scripts, config.max_frames, RunEnd::FrameBudget) {
        injection_failed = true;
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
            // A parked VM answers `void` to every expression without
            // evaluating it; reporting that as the value hides the failed
            // probe behind a legitimate-looking `expression=void`.
            Ok(value)
                if matches!(value, Variant::Void) && runtime.engine().is_script_suspended() =>
            {
                println!("expression_error={VM_SUSPENDED}");
                injection_failed = true;
            }
            Ok(value) => println!("expression={}", display_value(runtime.engine(), &value)),
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
        let engine = runtime.engine();
        let tjs = engine.tjs_runtime();
        let current = global_member_path(tjs, name);
        match current.object_handle() {
            Some(object) => {
                for (member, value) in tjs.object_members(object) {
                    match &value {
                        Variant::Integer(_) | Variant::Real(_) | Variant::String(_) => {
                            println!("{member}={value}")
                        }
                        _ => println!("{member}={}", variant_kind(engine, &value)),
                    }
                }
            }
            None => println!("{name}={}", variant_kind(engine, &current)),
        }
    }
    if config.kag_state {
        println!(
            "kag-state final {}",
            semantic_kag_state(runtime.engine()).unwrap_or_else(|| "-".to_string())
        );
    }
    // A backend that cannot open a device fails on its own thread, and a run
    // that ends before the worker reports (a five-frame probe) would then read
    // exactly like a healthy sink that played nothing. Give a silent real sink
    // a short grace to report before the verdict is printed; the events are
    // handled the way `RuntimeSession` handles them, so watching for the
    // report here drops nothing.
    if audio_tap.is_some() && audio_last_report.is_none() && !audio_degraded.load(Ordering::Relaxed)
    {
        let deadline = Instant::now() + Duration::from_millis(200);
        while Instant::now() < deadline && audio_last_report.is_none() {
            for event in runtime.audio_mut().poll_events() {
                match event {
                    krkr_core::AudioEvent::Status(status) => {
                        println!("audio status={:?} {}", status.level, status.message);
                        audio_last_report = Some(status.message);
                    }
                    krkr_core::AudioEvent::PlaybackStopped { id } => {
                        if let Err(error) = runtime.engine_mut().notify_audio_stopped(id) {
                            println!("audio completion error: {error}");
                        }
                    }
                }
            }
            if audio_last_report.is_none() {
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
    report_audio_tap(
        audio_tap.as_ref(),
        audio_degraded.load(Ordering::Relaxed),
        audio_last_report.as_deref(),
    );
    println!("done frames={}", config.max_frames);
    // A requested dump is a result the caller scripted around: a layer image
    // or a shot that could not be written must be reported and fail the run,
    // never panic the rest of this section (and the exit status) away.
    let mut dump_failed = false;
    if let Some(dir) = &config.dump_layer_images {
        for layer in runtime.engine().host().layer_tree().layers() {
            if let Some(image) = &layer.image {
                let path = format!(
                    "{dir}/layer_{}_{}x{}.png",
                    layer.id, image.upload.width, image.upload.height
                );
                // `write_png` creates the target directory, so a dump into a
                // path the caller never created works (the reviewed panic).
                if let Err(error) = snapshot::write_png(
                    &path,
                    image.upload.width,
                    image.upload.height,
                    &image.upload.rgba,
                ) {
                    println!("layer-image error: {path}: {error}");
                    dump_failed = true;
                }
            }
        }
    }
    if let Some(path) = &config.shot {
        match last_frame_output.take() {
            Some(frame) => {
                // Live layer images take priority over cached uploads: a layer
                // image can be updated in place without a new upload, which
                // would leave the cached copy stale (e.g. an opaque black
                // texture turned transparent).
                refresh_live_layer_images(runtime.engine(), &mut textures);
                let viewport = runtime
                    .engine()
                    .content_viewport_size()
                    .unwrap_or(Size::new(1280.0, 720.0));
                let width = viewport.width.max(1.0) as u32;
                let height = viewport.height.max(1.0) as u32;
                let (width, height, rgba) = if config.shot_raw {
                    snapshot::composite_frame(width, height, &frame.draw_commands, &textures)
                } else {
                    snapshot::composite_frame_output(width, height, &frame, &textures)
                };
                match snapshot::write_png(path, width, height, &rgba) {
                    Ok(()) => println!(
                        "screenshot={path} size={width}x{height} transitions={} raw={}",
                        frame.transitions.len(),
                        config.shot_raw
                    ),
                    Err(error) => {
                        println!("screenshot_error={path}: {error}");
                        dump_failed = true;
                    }
                }
            }
            None => println!("screenshot_error={path}: no frame was rendered"),
        }
    }
    // Diagnostics are printed first: a run whose requested injection,
    // expression or dump never happened must not look like a successful probe.
    if injection_failed {
        println!("injection=error");
    }
    if dump_failed {
        println!("dump=error");
    }
    if injection_failed || dump_failed {
        std::process::exit(1);
    }
}

/// The sink the headless harness hands to `RuntimeSession`.
///
/// The default is the real decoder ([`AudioSystem`]) — what `krkr-desktop`
/// passes — so a probe decodes the game's audio and the audio worker's own
/// `PlaybackStopped` events complete the waits; `--virtual-audio` selects the
/// silent sink instead. The reviewed bug passed `VirtualAudioSink`
/// unconditionally, so no run ever decoded a sample and its help text
/// described the opposite of the code.
///
/// The silence case needs this wrapper rather than a bare
/// `Box::new(VirtualAudioSink::default())`: `VirtualAudioSink::take_commands`
/// is an *inherent* method, and the session calls the trait method on a
/// `Box<dyn AudioSink>`, where only the trait's default (an empty queue) is
/// reachable. The harness's completion synthesis therefore never saw a single
/// command and a `--virtual-audio` run could park on an audio wait forever.
/// Overriding the trait method here reaches the stored commands.
///
/// A real decoder whose backend dies (a headless box has no output device)
/// degrades to that same silent sink with a printed notification: the
/// alternative on this machine was a run that aborted at frame 4 of the first
/// BGM cue with `audio command failed`. The end of the run reports the sink
/// it really had, so a probe can never mistake "no audio decoded" for "the
/// game has no audio".
enum DebugAudioSink {
    /// The decoder: commands execute on the audio worker, and completions
    /// arrive through [`AudioSink::poll_events`].
    System {
        system: AudioSystem,
        /// Set when the backend died and the sink degraded to `Virtual`, so
        /// the end of the run reports the audio path it really had.
        degraded: Arc<AtomicBool>,
    },
    /// The deterministic sink: commands accumulate until the harness takes
    /// them and synthesizes the completions the game waits on.
    Virtual(VirtualAudioSink),
}

impl AudioSink for DebugAudioSink {
    fn prepare(&mut self) -> Result<(), AudioError> {
        match self {
            Self::System { system, .. } => system.prepare(),
            Self::Virtual(sink) => sink.prepare(),
        }
    }

    fn submit(&mut self, commands: &[AudioCommand]) -> Result<(), AudioError> {
        match self {
            Self::System { system, degraded } => match system.submit(commands) {
                Ok(()) => Ok(()),
                // A headless box has no output device, and kira's backend then
                // dies on the first command: the run must say so and continue
                // on the silent sink (with the harness's completion synthesis,
                // so a game waiting on an audio instance is not parked
                // forever) instead of failing at the game's first BGM cue.
                Err(error) => {
                    println!(
                        "audio error: {error}; continuing with the silent sink (no audio decodes)"
                    );
                    degraded.store(true, Ordering::Relaxed);
                    let mut sink = VirtualAudioSink::default();
                    sink.submit(commands)?;
                    *self = Self::Virtual(sink);
                    Ok(())
                }
            },
            Self::Virtual(sink) => sink.submit(commands),
        }
    }

    fn poll_events(&mut self) -> Vec<AudioEvent> {
        match self {
            Self::System { system, .. } => system.poll_events(),
            Self::Virtual(sink) => sink.poll_events(),
        }
    }

    fn take_commands(&mut self) -> Vec<AudioCommand> {
        match self {
            Self::System { .. } => Vec::new(),
            Self::Virtual(sink) => sink.take_commands(),
        }
    }
}

/// The verdict line of the audio path.
///
/// The three ways a run can end with nothing decoded look alike in a bare
/// count: the silent sink asked for it (`--virtual-audio`), the backend was
/// unavailable and the run degraded, or a live sink simply played nothing.
/// Only the first is the operator's choice, and a sink that reported an error
/// must name it — otherwise a dead backend and a healthy silent run are
/// byte-identical (the review's measurement).
fn audio_verdict(
    instances: usize,
    rendered: u64,
    virtual_sink: bool,
    degraded: bool,
    last_report: Option<&str>,
) -> String {
    if virtual_sink {
        return format!(
            "audio decoded instances={instances} rendered_frames={rendered} (virtual sink: nothing decodes)"
        );
    }
    if degraded {
        return format!(
            "audio decoded instances={instances} rendered_frames={rendered} (the backend was unavailable; the run continued on the silent sink, nothing decodes)"
        );
    }
    match last_report {
        Some(report) => format!(
            "audio decoded instances={instances} rendered_frames={rendered} (the sink reported: {report})"
        ),
        None => format!("audio decoded instances={instances} rendered_frames={rendered}"),
    }
}

/// Reports what the audio path actually did, from the decoder's PCM tap.
///
/// `cursor` is the instance's rendered-frame clock: frames the decoder
/// actually played. A virtual run has no tap, so it reports nothing decoded —
/// the difference a probe needs between "the audio path ran" and "the
/// commands were merely consumed".
fn report_audio_tap(tap: Option<&PcmTap>, degraded: bool, last_report: Option<&str>) {
    let mut lines = Vec::new();
    let mut rendered = 0_u64;
    let mut instances = 0;
    if let Some(tap) = tap
        && !degraded
    {
        let ids = tap.instance_ids();
        instances = ids.len();
        for id in &ids {
            let cursor = tap.cursor(*id).unwrap_or(0);
            rendered += cursor;
            let spec = tap
                .spec(*id)
                .map(|spec| format!("{}Hz/{}ch", spec.sample_rate, spec.channels))
                .unwrap_or_else(|| "?".to_string());
            let state = tap.state(*id).map(|state| format!("{state:?}"));
            // The tap only allocates when somebody reads; a 480-frame window
            // (~10 ms) is enough to tell decoded audio from silence.
            let recent = tap
                .read_recent(*id, 480)
                .map(|snapshot| snapshot.available_frames)
                .unwrap_or(0);
            lines.push(format!(
                "audio instance id={} spec={spec} state={} rendered_frames={cursor} recent_decoded_frames={recent}/480",
                id.0,
                state.as_deref().unwrap_or("?")
            ));
        }
    }
    println!(
        "{}",
        audio_verdict(instances, rendered, tap.is_none(), degraded, last_report)
    );
    for line in lines {
        println!("{line}");
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
    use super::{TransitionPolicy, parse_args_from, requested_transition_policy};
    use krkr_debug::console::{
        DEFAULT_LOG_TAIL, InteractiveCommand, TraceCommand, parse_interactive_command,
    };

    fn args(values: &[&str]) -> impl Iterator<Item = String> {
        values.iter().map(|value| value.to_string())
    }

    /// The headless shell must leave the engine's Animated default in place:
    /// the immediate policy finishes a transition inside `Layer.beginTransition`,
    /// which delivers `onTransitionCompleted` before the game's own script turn
    /// has registered the `conductor.wait` keys that completion satisfies — the
    /// returned-to PARQUET title then parks forever on a wait no later event can
    /// trigger while every screenshot still looks right.  Only an explicit flag
    /// may select it (`requested_transition_policy` carries the reference
    /// argument).
    #[test]
    fn immediate_transitions_are_opt_in() {
        for default_args in [&["--kag-state"][..], &["--timed-transitions"][..]] {
            assert_eq!(
                requested_transition_policy(&parse_args_from(args(default_args))),
                None,
                "{default_args:?}"
            );
        }
        assert_eq!(
            requested_transition_policy(&parse_args_from(args(&["--immediate-transitions"]))),
            Some(TransitionPolicy::Immediate)
        );
    }

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

    /// A script-born `global.kag` (and its conductor/wait members) is a
    /// self-bound closure under the `this`/`new` value model, which is the
    /// shape that made `--kag-state` print nothing and `--kag-auto-click`
    /// never fire. Every reader that needs the object identity has to unwrap
    /// it, so the same fixture is read through all three fixed paths.
    #[test]
    fn kag_readers_unwrap_script_born_globals() {
        let mut engine =
            krkr_engine::KrkrEngine::new(krkr_engine::EngineConfig::default()).expect("engine");
        engine
            .execute_script(
                "probe.tjs",
                r#"
                global.kag = new Dictionary();
                global.kag.currentStorage = "custom.ks";
                global.kag.currentLabel = "*logo";
                global.kag.inSleep = 0;
                global.kag.clickWaiting = 1;
                global.kag.conductor = new Dictionary();
                global.kag.conductor.status = 2;
                global.kag.conductor.curLine = 144;
                global.kag.conductor.timerEnabled = 0;
                global.kag.conductor.waitUntil = new Dictionary();
                global.kag.conductor.waitUntil.click = 1;
                global.kag.conductor.waitUntil.timeout = 1;
                "#,
            )
            .expect("script stores global.kag");

        // Guard the fixture: a plain object would not exercise the unwrap.
        assert!(matches!(
            engine.tjs_runtime().global_member("kag"),
            krkr_tjs2::runtime::Variant::Closure(_)
        ));

        let state = super::semantic_kag_state(&engine).expect("kag state");
        assert!(state.contains("custom.ks@*logo:144 st=wait(2)"), "{state}");
        assert!(state.contains("wait=[click,timeout]"), "{state}");
        assert!(super::kag_awaits_click(&engine));

        let wait_until = super::global_member_path(engine.tjs_runtime(), "kag.conductor.waitUntil");
        let Some(wait_until) = wait_until.object_handle() else {
            panic!("waitUntil path did not resolve to an object");
        };
        assert!(engine.tjs_runtime().has_object_member(wait_until, "click"));

        let value = engine
            .execute_expression("probe.tjs", "global.kag")
            .expect("read global.kag");
        // The console renders the bound value the way a script sees it.
        assert_eq!(krkr_debug::console::variant_kind(&engine, &value), "object");
        assert!(
            krkr_debug::console::display_value(&engine, &value).starts_with("<object #"),
            "{}",
            krkr_debug::console::display_value(&engine, &value)
        );
        let lines =
            krkr_debug::console::member_lines(&mut engine, &value, "global.kag", None, false)
                .expect("member lines");
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("member conductor=object")),
            "{lines:?}"
        );
    }

    /// `textrender` keeps the text verbatim, so a typed `\n` stays the two
    /// characters the message format uses as a line break, and every flag is
    /// validated before the script is built.
    #[test]
    fn textrender_command_keeps_the_text_verbatim() {
        assert!(matches!(
            parse_interactive_command(r"textrender --size 48 --width 320 A\nB"),
            Ok(InteractiveCommand::TextRender { class, size, width, text })
                if class == "TextRenderBase" && size == 48 && width == 320.0 && text == r"A\nB"
        ));
        assert!(matches!(
            parse_interactive_command("textrender --class TextRender hi"),
            Ok(InteractiveCommand::TextRender { class, .. }) if class == "TextRender"
        ));
        assert!(matches!(
            parse_interactive_command("textrender plain text with spaces"),
            Ok(InteractiveCommand::TextRender { text, .. }) if text == "plain text with spaces"
        ));
        assert!(parse_interactive_command("textrender").is_err());
        assert!(parse_interactive_command("textrender --size 0 x").is_err());
        assert!(parse_interactive_command("textrender --nope x").is_err());
        assert!(parse_interactive_command("textrender --class 1bad x").is_err());
    }

    /// The three ways a run can end with nothing decoded must not read alike:
    /// the silent sink is the operator's own choice, a degraded run says so,
    /// and a sink that reported an error is named — a dead backend used to be
    /// byte-identical to a healthy run that simply played nothing (the
    /// review's measurement of the asynchronous failure path).
    #[test]
    fn the_audio_verdict_names_the_sink_it_actually_had() {
        assert_eq!(
            super::audio_verdict(0, 0, true, false, None),
            "audio decoded instances=0 rendered_frames=0 (virtual sink: nothing decodes)"
        );
        assert!(
            super::audio_verdict(0, 0, false, true, None)
                .contains("the backend was unavailable; the run continued on the silent sink"),
            "{}",
            super::audio_verdict(0, 0, false, true, None)
        );
        assert!(
            super::audio_verdict(
                0,
                0,
                false,
                false,
                Some("audio backend is unavailable: no output device")
            )
            .contains("(the sink reported: audio backend is unavailable: no output device)"),
            "a dead backend must not look like a silent game"
        );
        assert_eq!(
            super::audio_verdict(2, 25_595_788, false, false, None),
            "audio decoded instances=2 rendered_frames=25595788"
        );
    }

    /// The reviewed wiring: every run was handed a `VirtualAudioSink`, so no
    /// run ever decoded a sample although the tool links the Opus decoder and
    /// `krkr-desktop` plays through `AudioSystem`. The flag is now the opt-in
    /// that selects the silent sink, matching its own help text.
    #[test]
    fn audio_decodes_by_default_and_the_silent_sink_is_opt_in() {
        let config = parse_args_from(args(&["--kag-state"]));
        assert!(
            !config.virtual_audio,
            "the default must be the real audio system, like krkr-desktop"
        );
        assert!(parse_args_from(args(&["--virtual-audio"])).virtual_audio);
    }

    /// The silent sink must hand its commands to the harness: `RuntimeSession`
    /// reaches `take_commands` through `Box<dyn AudioSink>`, where the trait's
    /// default (an empty queue) is what a bare `VirtualAudioSink` answers,
    /// because the crate's `take_commands` is an inherent method. Before this
    /// wrapper, `--virtual-audio` synthesized no completion at all, so a game
    /// waiting on an audio instance parked forever.
    #[test]
    fn the_silent_sink_hands_its_commands_to_the_harness() {
        use super::{AudioCommand, AudioInstanceId, DebugAudioSink, VirtualAudioSink};
        use krkr_audio::AudioSink;

        let mut sink: Box<dyn AudioSink> =
            Box::new(DebugAudioSink::Virtual(VirtualAudioSink::default()));
        sink.submit(&[AudioCommand::Stop {
            id: AudioInstanceId(7),
            fade_seconds: 0.0,
        }])
        .expect("submit");
        let commands = sink.take_commands();
        assert_eq!(commands.len(), 1, "{commands:?}");

        // The trap this wrapper exists for: the same commands through the
        // crate's own sink reach the trait's default empty queue.
        let mut bare: Box<dyn AudioSink> = Box::new(VirtualAudioSink::default());
        bare.submit(&[AudioCommand::Stop {
            id: AudioInstanceId(7),
            fade_seconds: 0.0,
        }])
        .expect("submit");
        assert!(
            bare.take_commands().is_empty(),
            "a bare `VirtualAudioSink` answers the trait's empty default; \
             if this changed, the wrapper is no longer needed"
        );
    }
}
