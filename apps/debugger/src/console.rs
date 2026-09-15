//! Interactive console shared by `krkr-debug` and `krkr-desktop`.
//!
//! The command set and dump helpers are identical in both shells; only the
//! host loop differs (a deterministic frame budget vs. a real-time window).

use std::{
    io::BufRead,
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
    thread,
};

use krkr_core::{Color, DrawCommand, FrameOutput, Point, Size};
use krkr_engine::{KagTaskState, KrkrEngine, KrkrHost, RuntimeSession, SystemPaths};
use krkr_tjs2::runtime::{ObjectHandle, Runtime, Variant};

use crate::snapshot::{self, TextureCache};

/// Newest-first log lines shown by `logs` when no `-n` is given.
pub const DEFAULT_LOG_TAIL: usize = 200;

pub enum InteractiveCommand {
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
    Shot {
        path: String,
        /// Write the historical raw view (`draw_commands` only, flattened
        /// onto black) instead of the composited frame with its transitions.
        raw: bool,
    },
    Expression(String),
    Members {
        expression: String,
        filter: Option<String>,
        all: bool,
    },
    Trace(TraceCommand),
    /// Render text through a `TextRender` class and dump the per-character
    /// records the layout produced.
    TextRender {
        class: String,
        size: i64,
        width: f64,
        text: String,
    },
    Quit,
}

/// `trace` sub-commands. Native calls are the boundary where a script value
/// turns into engine geometry, so arming a trace there is usually the fastest
/// way to find which caller passed the wrong number.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TraceCommand {
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
pub enum InteractiveUntil {
    Kag(KagUntil),
    Layer(String),
    Storage(String),
    Log { needle: String, after: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KagUntil {
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

pub fn start_interactive_console() -> Receiver<InteractiveCommand> {
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

pub fn parse_interactive_command(line: &str) -> Result<InteractiveCommand, String> {
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
            let (raw, path) = match rest.strip_prefix("--raw") {
                Some(tail) => (true, tail.trim_start()),
                None => (false, rest),
            };
            if path.is_empty() {
                Err("usage: shot [--raw] <path>".to_string())
            } else {
                Ok(InteractiveCommand::Shot {
                    path: path.to_string(),
                    raw,
                })
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
        "textrender" | "text-render" => parse_textrender_command(rest),
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
pub fn parse_logs_command(rest: &str) -> Result<InteractiveCommand, String> {
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
pub fn parse_members_command(rest: &str) -> Result<InteractiveCommand, String> {
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

/// `textrender [--class <name>] [--size <px>] [--width <px>] <text>`
///
/// The command renders `<text>` through the game's own TextRender class and
/// dumps the character records ([`textrender_source`] documents the call
/// order). Verifying the message text format's escapes — `\n` breaks, `\k`
/// waits, `%f…;` style codes — used to mean hand-writing a TJS probe and
/// re-deriving the game's own setup order every time. The text is passed
/// verbatim: a typed `\n` stays the two characters the message format uses as
/// a line break, not a newline a TJS literal would fold it into.
pub fn parse_textrender_command(rest: &str) -> Result<InteractiveCommand, String> {
    const USAGE: &str = "usage: textrender [--class <name>] [--size <px>] [--width <px>] <text>";
    let mut class = "TextRenderBase".to_string();
    let mut size = 48_i64;
    let mut width = 800.0_f64;
    let mut remainder = rest.trim();
    while let Some(flag) = remainder
        .split_whitespace()
        .next()
        .filter(|token| token.starts_with("--"))
    {
        let tail = remainder[flag.len()..].trim_start();
        let (value, next) = match flag {
            "--class" | "--size" | "--width" => tail
                .split_once(char::is_whitespace)
                .ok_or_else(|| USAGE.to_string())?,
            _ => return Err(format!("unknown textrender option `{flag}`; {USAGE}")),
        };
        match flag {
            "--class" => class = value.to_string(),
            "--size" => {
                size = value
                    .parse::<i64>()
                    .ok()
                    .filter(|size| *size > 0)
                    .ok_or_else(|| "--size must be a positive integer".to_string())?;
            }
            "--width" => {
                width = value
                    .parse::<f64>()
                    .ok()
                    .filter(|width| *width >= 0.0)
                    .ok_or_else(|| "--width must be a non-negative number".to_string())?;
            }
            _ => unreachable!("flags are matched above"),
        }
        remainder = next.trim_start();
    }
    if remainder.is_empty() {
        return Err(USAGE.to_string());
    }
    let mut chars = class.chars();
    let starts_ident = chars
        .next()
        .is_some_and(|ch| ch.is_alphabetic() || matches!(ch, '_' | '$'));
    if !starts_ident || !chars.all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '.' | '$')) {
        return Err(format!("--class must be an identifier, not `{class}`"));
    }
    Ok(InteractiveCommand::TextRender {
        class,
        size,
        width,
        text: remainder.to_string(),
    })
}

pub fn parse_trace_command(rest: &str) -> Result<InteractiveCommand, String> {
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

pub fn parse_interactive_until(rest: &str) -> Result<(InteractiveUntil, usize), String> {
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

/// The TJS script the `textrender` command runs.
///
/// The order is the game's own message path: a `Font` first (`setFont`
/// registers the size and face the layout reads — a probe that skips it
/// measures an empty font), then the render box, then
/// `render(text, indent, speed, …)` with the argument shape the games pass
/// (`system/TextRender.tjs` → `render(a3, a4, a7, a8, 0, 0)`). The records
/// come back through `getCharacters(0, 0)`, the call shape the game's own
/// redraw path uses, and the waits through `getKeyWait()`.
pub fn textrender_source(class: &str, size: i64, width: f64, text: &str) -> String {
    let literal = tjs_string_literal(text);
    format!(
        r#"(function() {{
    var tr = new {class}();
    var fnt = new Font();
    fnt.height = {size};
    tr.setFont(fnt);
    tr.setRenderSize({width}, 0);
    tr.render({literal}, 0, 0, 0, 0, 0);
    var out = %[];
    out.renderCount = tr.renderCount;
    out.renderLines = tr.renderLines;
    out.characters = tr.getCharacters(0, 0);
    out.keywaits = tr.getKeyWait();
    return out;
}})()"#
    )
}

/// A TJS string literal for arbitrary console text.
///
/// The text is passed **verbatim**: a typed `\n` becomes the two characters
/// the message format uses as a line break (`\k`, `%f…;` the same way), not a
/// newline the literal would fold it into.
fn tjs_string_literal(text: &str) -> String {
    let mut literal = String::with_capacity(text.len() + 2);
    literal.push('"');
    for ch in text.chars() {
        match ch {
            '\\' => literal.push_str("\\\\"),
            '"' => literal.push_str("\\\""),
            '\n' => literal.push_str("\\n"),
            '\r' => literal.push_str("\\r"),
            '\t' => literal.push_str("\\t"),
            other => literal.push(other),
        }
    }
    literal.push('"');
    literal
}

/// Runs [`textrender_source`] and prints the render counters, one line per
/// character record (text, position, line, size, face, colour, style flags,
/// delay) and every `getKeyWait()` entry.
pub fn report_textrender(
    runtime: &mut RuntimeSession,
    class: &str,
    size: i64,
    width: f64,
    text: &str,
) {
    let source = textrender_source(class, size, width, text);
    let value = match evaluate_interactive_expression(runtime, &source) {
        Ok(value) => value,
        Err(error) => {
            println!("interactive textrender_error class={class} text={text:?}: {error}");
            return;
        }
    };
    let Some(out) = value.object_handle() else {
        println!(
            "interactive textrender_error class={class} text={text:?}: the render returned no record set"
        );
        return;
    };
    let engine = runtime.engine();
    let tjs = engine.tjs_runtime();
    let render_count = tjs
        .object_member(out, "renderCount")
        .to_integer()
        .unwrap_or(0);
    let render_lines = tjs
        .object_member(out, "renderLines")
        .to_integer()
        .unwrap_or(0);
    println!(
        "interactive textrender class={class} size={size} width={width} text={text:?} renderLines={render_lines} renderCount={render_count}"
    );
    let characters = tjs.object_member(out, "characters").object_handle();
    for (index, record) in characters
        .and_then(|characters| tjs.array_elements(characters))
        .unwrap_or(&[])
        .iter()
        .enumerate()
    {
        let Some(record) = record.object_handle() else {
            continue;
        };
        let int = |name: &str| tjs.object_member(record, name).to_integer().unwrap_or(0);
        let text = tjs
            .object_member(record, "text")
            .to_tjs_string()
            .unwrap_or_default();
        let face = tjs
            .object_member(record, "face")
            .to_tjs_string()
            .unwrap_or_default();
        let link = tjs
            .object_member(record, "link")
            .to_tjs_string()
            .unwrap_or_default();
        let delay = tjs.object_member(record, "delay").to_real().unwrap_or(0.0);
        println!(
            "textrender char[{index}] text={text:?} x={} y={} line={} size={} cw={} face={face:?} color={:#010x} bold={} italic={} shadow={} edge={} delay={delay} link={link:?}",
            int("x"),
            int("y"),
            int("line"),
            int("size"),
            int("cw"),
            int("color"),
            int("bold"),
            int("italic"),
            int("shadow"),
            int("edge"),
        );
    }
    let keywaits = tjs.object_member(out, "keywaits").object_handle();
    for (index, wait) in keywaits
        .and_then(|keywaits| tjs.array_elements(keywaits))
        .unwrap_or(&[])
        .iter()
        .enumerate()
    {
        let Some(wait) = wait.object_handle() else {
            continue;
        };
        let pos = tjs.object_member(wait, "pos").to_integer().unwrap_or(0);
        let time = tjs.object_member(wait, "time").to_real().unwrap_or(0.0);
        println!("textrender keywait[{index}] pos={pos} time={time}");
    }
}

/// Apply a command from a caller that records only the live draw list (the
/// windowed `krkr-desktop --debug-console` shell).
///
/// The frame is reconstructed from the commands, so a running transition is
/// not part of it: `draw` reports `transitions=0` and `shot` writes the
/// historical view.  Frames without transitions are byte-identical between
/// the two entries; a caller that has the `FrameOutput` (as `krkr-debug`
/// does) uses [`apply_interactive_control_with_frame`] instead.
#[allow(clippy::too_many_arguments)]
pub fn apply_interactive_control(
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
    let frame = last_commands
        .map(|commands| FrameOutput::new(Color::new(0.0, 0.0, 0.0, 0.0), commands.to_vec()));
    let mut frame_shots = Vec::new();
    let quit = apply_interactive_control_with_frame(
        command,
        paused,
        budget,
        until,
        pending_clicks,
        &mut frame_shots,
        frame_index,
        runtime,
        textures,
        frame.as_ref(),
        auto_click,
        auto_point,
    );
    // The commands-only caller has no frame to composite, so a queued shot
    // keeps its path and is written by its own next-frame shot loop.
    pending_shots.extend(frame_shots.into_iter().map(|(path, _raw)| path));
    quit
}

/// Evaluates a console expression, turning "the VM is parked" into an explicit
/// error.
///
/// While a call stack is suspended on a pending resource the VM returns `void`
/// from any new evaluation instead of running it, so during a load screen even
/// `expr 1+1` answers `void`. Reporting that as a value makes every inspection
/// look like the object is missing; naming the suspension says to advance a few
/// frames and retry. The batch probe's `--at-frame` handles the same situation
/// by deferring instead (`crate::inject`), sharing [`crate::inject::VM_SUSPENDED`]
/// so both surfaces name the cause in the same words.
pub fn evaluate_interactive_expression(
    runtime: &mut RuntimeSession,
    expression: &str,
) -> Result<Variant, String> {
    let value = runtime
        .engine_mut()
        .execute_expression("krkr_debug_interactive.tjs", expression)
        .map_err(|error| error.to_string())?;
    if matches!(value, Variant::Void) && crate::inject::parked(runtime.engine()) {
        return Err(crate::inject::VM_SUSPENDED.to_string());
    }
    Ok(value)
}

/// Lists the members of a script-visible value for the `members` command.
///
/// The value is unwrapped through `object_handle()`: a value the game stored
/// from `this` or from `new` is a self-bound closure whose script-visible
/// identity is the object behind it. A native property member stores its
/// accessor rather than the value, so those are resolved through the TJS
/// dispatch path before printing.
pub fn member_lines(
    engine: &mut KrkrEngine,
    value: &Variant,
    expression: &str,
    filter: Option<&str>,
    all: bool,
) -> Result<Vec<String>, String> {
    let Some(object) = value.object_handle() else {
        return Err(format!(
            "not-an-object kind={}",
            variant_kind(engine, value)
        ));
    };
    let members = engine.tjs_runtime().object_members(object);
    let total = members.len();
    let shown = members
        .into_iter()
        .filter_map(|(name, value)| {
            let target = value.object_handle();
            let callable =
                target.is_some_and(|handle| engine.tjs_runtime().object_is_callable(handle));
            let property =
                target.is_some_and(|handle| engine.tjs_runtime().object_is_native_property(handle));
            let keep = (all || !callable)
                && filter.is_none_or(|needle| name.to_ascii_lowercase().contains(needle));
            keep.then_some((name, value, property))
        })
        .collect::<Vec<_>>();
    let mut lines = vec![format!(
        "interactive members expression={expression:?} count={} total={total}",
        shown.len()
    )];
    for (name, value, property) in shown {
        let value = if property {
            engine
                .tjs_runtime_mut()
                .resolve_object_member(object, &name)
                .unwrap_or(value)
        } else {
            value
        };
        lines.push(format!(
            "member {name}={} value={}",
            variant_kind(engine, &value),
            display_value(engine, &value)
        ));
    }
    Ok(lines)
}

/// Apply a command from the frame-loop control channel.  Returns `true` when
/// the caller should terminate the probe.  Inspection commands are handled
/// synchronously, while control commands only change the state consumed by
/// the next frame boundary.
///
/// This is the frame-aware entry: `last_frame` is the complete
/// `FrameOutput`, so `draw` reports the running transitions and `shot` writes
/// their composite.  The windowed shell's `--debug-console` records only the
/// live draw list and goes through [`apply_interactive_control`], which
/// reconstructs a transition-free frame.
#[allow(clippy::too_many_arguments)]
pub fn apply_interactive_control_with_frame(
    command: InteractiveCommand,
    paused: &mut bool,
    budget: &mut Option<usize>,
    until: &mut Option<InteractiveUntil>,
    pending_clicks: &mut Vec<Point>,
    pending_shots: &mut Vec<(String, bool)>,
    frame_index: usize,
    runtime: &mut RuntimeSession,
    textures: &mut TextureCache,
    last_frame: Option<&FrameOutput>,
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
            if let Some(frame) = last_frame {
                dump_draw_commands(&frame.draw_commands);
                dump_frame_transitions(frame);
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
            if let Some(frame) = last_frame {
                dump_draw_commands(&frame.draw_commands);
                dump_frame_transitions(frame);
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
            "interactive commands: advance [n], until <condition> [max], run, pause, click <x> <y>, state, layers [filter], layer <id>, hit <x> <y>, draw, probe, auto [on|off], autopoint <x> <y>|off, load <storage>, logs [-n tail] [needle]..., trace [list|add|rm|off|names] [pattern]..., resources, shot [--raw] <path>, expr <tjs>, members [-a] [-f substr] <expr>, textrender [--class name] [--size px] [--width px] <text>, q"
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
        InteractiveCommand::Shot { path, raw } => {
            if let Some(frame) = last_frame {
                write_interactive_shot(&path, runtime.engine(), frame, textures, raw);
            } else {
                pending_shots.push((path, raw));
                println!("interactive=shot queued (advance once to render a frame)");
            }
        }
        InteractiveCommand::Expression(expression) => {
            match evaluate_interactive_expression(runtime, &expression) {
                Ok(value) => println!(
                    "interactive expression={}",
                    display_value(runtime.engine(), &value)
                ),
                Err(error) => println!("interactive expression_error={error}"),
            }
        }
        InteractiveCommand::Members {
            expression,
            filter,
            all,
        } => match evaluate_interactive_expression(runtime, &expression) {
            Ok(value) => {
                match member_lines(
                    runtime.engine_mut(),
                    &value,
                    &expression,
                    filter.as_deref(),
                    all,
                ) {
                    Ok(lines) => {
                        for line in lines {
                            println!("{line}");
                        }
                    }
                    Err(message) => println!("interactive members_error={message}"),
                }
            }
            Err(error) => println!("interactive members_error={error}"),
        },
        InteractiveCommand::TextRender {
            class,
            size,
            width,
            text,
        } => report_textrender(runtime, &class, size, width, &text),
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

/// Copies the live layer images into the texture cache.
///
/// A layer image can be updated in place without a new upload event, which
/// would leave the cached copy stale; the shot compositors therefore refresh
/// from the layer tree first.
pub fn refresh_live_layer_images(engine: &KrkrEngine, textures: &mut TextureCache) {
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
}

/// Writes one frame as a PNG.
///
/// The default view is the composited one: the live draw commands plus every
/// running transition, the way the window presents the frame.  `raw` keeps
/// the historical view (live `draw_commands` only), which is what the tool
/// produced before transitions were composited; a frame without a transition
/// is byte-identical in both views.
pub fn write_interactive_shot(
    path: &str,
    engine: &KrkrEngine,
    frame: &FrameOutput,
    textures: &mut TextureCache,
    raw: bool,
) {
    refresh_live_layer_images(engine, textures);
    let viewport = engine
        .content_viewport_size()
        .unwrap_or(Size::new(1280.0, 720.0));
    let width = viewport.width.max(1.0) as u32;
    let height = viewport.height.max(1.0) as u32;
    let (width, height, rgba) = if raw {
        snapshot::composite_frame(width, height, &frame.draw_commands, textures)
    } else {
        snapshot::composite_frame_output(width, height, frame, textures)
    };
    match snapshot::write_png(path, width, height, &rgba) {
        Ok(()) if raw => {
            println!("interactive screenshot={path} frame_size={width}x{height} raw=true")
        }
        Ok(()) => println!(
            "interactive screenshot={path} frame_size={width}x{height} composited=true transitions={}",
            frame.transitions.len()
        ),
        Err(error) => println!("interactive screenshot_error={path}: {error}"),
    }
}

pub fn interactive_condition_satisfied(engine: &KrkrEngine, condition: &InteractiveUntil) -> bool {
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

pub fn dump_layers(engine: &KrkrEngine, filter: Option<&str>) {
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

pub fn layer_matches_filter(
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
pub const LAYER_BINDING: &str = "dbgLayer";

pub fn dump_layer(engine: &mut KrkrEngine, id: u64) {
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
pub fn dump_layer_script_object(engine: &mut KrkrEngine, id: u64) {
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

pub fn dump_draw_commands(commands: &[DrawCommand]) {
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

/// Prints the transitions the composited view draws on top of the live
/// commands: a `draw` taken mid-transition names the method, its progress and
/// the three faces the kernel blends, so the command list and the composited
/// `shot` can be read against each other.
pub fn dump_frame_transitions(frame: &FrameOutput) {
    println!(
        "---interactive transitions={} live_commands={} composited_commands={}---",
        frame.transitions.len(),
        frame.draw_commands.len(),
        composited_command_count(frame),
    );
    for (index, transition) in frame.transitions.iter().enumerate() {
        println!(
            "transition index={index} method={} progress={:.4} dest_rect={:?} old_commands={} under_commands={} source_commands={} rule_texture={:?} duration_millis={:.1}",
            transition.method,
            transition.progress,
            transition.dest_rect,
            transition.frozen_draw_commands.len(),
            transition.under_draw_commands.len(),
            transition.source_draw_commands.len(),
            transition.rule_texture_id,
            transition.params.duration_millis,
        );
    }
}

/// The draw commands the composited view rasterises for one frame: the live
/// list plus every transition's three faces.
pub fn composited_command_count(frame: &FrameOutput) -> usize {
    frame.draw_commands.len()
        + frame
            .transitions
            .iter()
            .map(|transition| {
                transition.frozen_draw_commands.len()
                    + transition.under_draw_commands.len()
                    + transition.source_draw_commands.len()
            })
            .sum::<usize>()
}

pub fn system_paths_for_project(root: &std::path::Path) -> SystemPaths {
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

pub fn dump_logs(engine: &KrkrEngine) {
    let logs = engine.host().logs();
    println!("---host logs ({})---", logs.len());
    for line in logs {
        println!("log: {line}");
    }
}

/// Prints how often each stubbed native method was actually invoked —
/// the strongest signal for what a game depends on that we have not
/// implemented yet.
pub fn dump_stub_calls(engine: &KrkrEngine) {
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
pub fn apply_trace_command(runtime: &mut Runtime<KrkrHost>, command: TraceCommand) {
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

/// The object a script-visible value carries when that value is a plain
/// object rather than a function.
///
/// Under the `this`/`new` value model a script-visible object is a closure
/// over the object (`tTJSVariant(objthis, objthis)`); a method read off a
/// receiver is a closure whose object is the inter-code context, and that is
/// the only shape a script sees as callable. The console used to print the
/// first shape as `<object #h>` and should keep doing so, or every data
/// member of the game's own objects would read `closure` again.
fn object_valued_closure(engine: &KrkrEngine, value: &Variant) -> Option<ObjectHandle> {
    let handle = value.object_handle()?;
    match value {
        Variant::Closure(_) if !engine.tjs_runtime().object_is_callable(handle) => Some(handle),
        _ => None,
    }
}

/// Renders a script-visible value the way a script sees it: an object-valued
/// closure prints as its object (`<object #h>`), everything else through
/// `Display`.
pub fn display_value(engine: &KrkrEngine, value: &Variant) -> String {
    match object_valued_closure(engine, value) {
        Some(handle) => Variant::Object(handle).to_string(),
        None => value.to_string(),
    }
}

pub fn variant_kind(engine: &KrkrEngine, value: &Variant) -> &'static str {
    if object_valued_closure(engine, value).is_some() {
        return "object";
    }
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
