//! Interactive stdin REPL implementing the tjs2 [`DebugUi`] trait.

use std::{collections::VecDeque, io::BufRead, io::Write, path::PathBuf};

use krkr_engine::KrkrHost;
use krkr_tjs2::{
    debug::{DebugAction, DebugUi, Pause, StopReason},
    runtime::Variant,
};

pub(crate) enum BreakpointSpec {
    Tjs { file: String, line: usize },
    KagLine { storage: String, line: usize },
    KagLabel { label: String },
}

pub(crate) fn parse_breakpoint_spec(spec: &str) -> Result<BreakpointSpec, String> {
    if let Some(label) = spec.strip_prefix('*') {
        if label.is_empty() {
            return Err("empty label".to_string());
        }
        return Ok(BreakpointSpec::KagLabel {
            label: label.to_string(),
        });
    }
    let (file, line) = spec
        .rsplit_once(':')
        .ok_or_else(|| "expected <file>:<line> or *<label>".to_string())?;
    let line: usize = line
        .parse()
        .map_err(|_| "line must be a number".to_string())?;
    if file.ends_with(".ks") {
        Ok(BreakpointSpec::KagLine {
            storage: file.to_string(),
            line,
        })
    } else {
        Ok(BreakpointSpec::Tjs {
            file: file.to_string(),
            line,
        })
    }
}

pub(crate) struct CliDebugger {
    stdin: std::io::BufReader<std::io::Stdin>,
    scripted: VecDeque<String>,
    last_action: Option<DebugAction>,
}

impl CliDebugger {
    pub(crate) fn new(commands_file: Option<PathBuf>) -> Self {
        let scripted = commands_file
            .map(|path| {
                std::fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
                    .lines()
                    .map(str::to_string)
                    .filter(|line| !line.trim_start().starts_with('#'))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            stdin: std::io::BufReader::new(std::io::stdin()),
            scripted,
            last_action: None,
        }
    }

    fn read_command(&mut self) -> Option<String> {
        if let Some(command) = self.scripted.pop_front() {
            println!("(krkr-debug) {command}");
            return Some(command);
        }
        print!("(krkr-debug) ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        match self.stdin.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line),
        }
    }

    fn print_stop(pause: &Pause<'_, KrkrHost>) {
        match pause.reason() {
            StopReason::Breakpoint => println!("stop: breakpoint"),
            StopReason::Step => println!("stop: step"),
            StopReason::DebuggerStmt => println!("stop: debugger statement"),
            StopReason::Exception { caught, message } => {
                println!(
                    "stop: {} exception: {message}",
                    if *caught { "caught" } else { "uncaught" }
                );
            }
            StopReason::Kag {
                storage,
                line,
                label,
                stepped,
            } => {
                println!(
                    "stop: kag {} {}:{} label={:?}",
                    if *stepped { "step" } else { "breakpoint" },
                    storage,
                    line.map(|line| line.to_string())
                        .unwrap_or_else(|| "?".to_string()),
                    label
                );
                return;
            }
        }
        if let Some(location) = pause.location() {
            println!("  at {location}");
        }
        if let Some(text) = pause.current_source_line() {
            println!("  | {}", text.trim_end());
        }
    }
}

impl DebugUi<KrkrHost> for CliDebugger {
    fn on_pause(&mut self, pause: &mut Pause<'_, KrkrHost>) -> DebugAction {
        Self::print_stop(pause);
        loop {
            let Some(line) = self.read_command() else {
                return DebugAction::Quit;
            };
            let line = line.trim();
            if line.is_empty() {
                if let Some(action) = self.last_action {
                    return action;
                }
                continue;
            }
            match handle_command(pause, line) {
                CommandOutcome::Resume(action) => {
                    if action != DebugAction::Quit {
                        self.last_action = Some(action);
                    }
                    return action;
                }
                CommandOutcome::Stay => {}
            }
        }
    }
}

enum CommandOutcome {
    Resume(DebugAction),
    Stay,
}

fn handle_command(pause: &mut Pause<'_, KrkrHost>, line: &str) -> CommandOutcome {
    let (command, rest) = line
        .split_once(char::is_whitespace)
        .map(|(command, rest)| (command, rest.trim()))
        .unwrap_or((line, ""));
    match command {
        "b" | "break" => {
            if rest.is_empty() {
                println!("usage: b <file>:<line> | b *<label>");
                return CommandOutcome::Stay;
            }
            match parse_breakpoint_spec(rest) {
                Ok(BreakpointSpec::Tjs { file, line }) => {
                    let id = pause
                        .runtime()
                        .enable_debugger()
                        .add_tjs_breakpoint(&file, line);
                    println!("breakpoint #{id} tjs {file}:{line}");
                }
                Ok(BreakpointSpec::KagLine { storage, line }) => {
                    let id = pause
                        .runtime()
                        .enable_debugger()
                        .add_kag_line_breakpoint(&storage, line);
                    println!("breakpoint #{id} kag {storage}:{line}");
                }
                Ok(BreakpointSpec::KagLabel { label }) => {
                    let id = pause
                        .runtime()
                        .enable_debugger()
                        .add_kag_label_breakpoint(&label);
                    println!("breakpoint #{id} kag-label *{label}");
                }
                Err(message) => println!("invalid breakpoint spec `{rest}`: {message}"),
            }
            CommandOutcome::Stay
        }
        "bl" => {
            let descriptions = pause
                .runtime()
                .debugger()
                .map(|debugger| debugger.breakpoint_descriptions())
                .unwrap_or_default();
            if descriptions.is_empty() {
                println!("no breakpoints");
            }
            for description in descriptions {
                println!("{description}");
            }
            CommandOutcome::Stay
        }
        "bd" => {
            match rest.parse::<usize>() {
                Ok(id) => {
                    let removed = pause.runtime().enable_debugger().remove_breakpoint(id);
                    println!(
                        "{}",
                        if removed {
                            "deleted"
                        } else {
                            "no such breakpoint"
                        }
                    );
                }
                Err(_) => println!("usage: bd <id>"),
            }
            CommandOutcome::Stay
        }
        "c" | "continue" => CommandOutcome::Resume(DebugAction::Continue),
        "si" => CommandOutcome::Resume(DebugAction::StepInst),
        "s" | "step" => CommandOutcome::Resume(DebugAction::StepInto),
        "n" | "next" => CommandOutcome::Resume(DebugAction::StepOver),
        "fin" | "finish" => CommandOutcome::Resume(DebugAction::StepOut),
        "ks" => CommandOutcome::Resume(DebugAction::KagStep),
        "bt" => {
            if pause.backtrace().is_empty() {
                println!("(no TJS stack)");
            }
            for (index, frame) in pause.backtrace().iter().enumerate() {
                println!("  #{index} {frame}");
            }
            CommandOutcome::Stay
        }
        "f" | "frame" => {
            if let Some(location) = pause.location() {
                println!("{location}");
            }
            if let Some(text) = pause.current_source_line() {
                println!("| {}", text.trim_end());
            }
            CommandOutcome::Stay
        }
        "p" | "print" => {
            if rest.is_empty() {
                println!("usage: p <expression>");
            } else {
                match pause.eval(rest) {
                    Ok(value) => println!("{value}"),
                    Err(error) => println!("error: {error}"),
                }
            }
            CommandOutcome::Stay
        }
        "regs" => {
            let mut printed = 0;
            for (reg, value) in pause.registers() {
                if matches!(value, Variant::Void) {
                    continue;
                }
                println!("%{reg} = {value}");
                printed += 1;
            }
            if printed == 0 {
                println!("(all registers void)");
            }
            CommandOutcome::Stay
        }
        "set" => {
            let usage = "usage: set reg <n> <expression>";
            let mut tokens = rest.splitn(4, char::is_whitespace);
            let (Some("reg"), Some(reg), Some(_eq), Some(expression)) =
                (tokens.next(), tokens.next(), tokens.next(), tokens.next())
            else {
                println!("{usage}");
                return CommandOutcome::Stay;
            };
            let Ok(reg) = reg.parse::<i16>() else {
                println!("{usage}");
                return CommandOutcome::Stay;
            };
            match pause.eval(expression.trim()) {
                Ok(value) => match pause.write_register(reg, value) {
                    Ok(()) => println!("%{reg} written"),
                    Err(error) => println!("error: {error}"),
                },
                Err(error) => println!("error: {error}"),
            }
            CommandOutcome::Stay
        }
        "dis" => {
            match pause.disassemble_current() {
                Some(Ok(lines)) => {
                    for line in lines {
                        println!("{line}");
                    }
                }
                Some(Err(error)) => println!("error: {error}"),
                None => println!("(not paused in TJS code)"),
            }
            CommandOutcome::Stay
        }
        "catch" => {
            let debugger = pause.runtime().enable_debugger();
            let enabled = match rest {
                "on" | "throw" => {
                    debugger.set_break_on_exception(true);
                    true
                }
                "off" => {
                    debugger.set_break_on_exception(false);
                    false
                }
                "" => debugger.break_on_exception(),
                _ => {
                    println!("usage: catch [on|off]");
                    return CommandOutcome::Stay;
                }
            };
            println!("break-on-exception = {enabled}");
            CommandOutcome::Stay
        }
        "kag" => {
            match pause.reason() {
                StopReason::Kag {
                    storage,
                    line,
                    label,
                    ..
                } => println!("kag {storage}:{line:?} label={label:?}"),
                _ => println!("(not at a KAG stop)"),
            }
            CommandOutcome::Stay
        }
        "h" | "help" => {
            println!(
                "b <spec>|bl|bd <id>  breakpoints (spec: file.tjs:N, file.ks:N, *label)\n\
                 c|si|s|n|fin|ks     continue / step inst / into / over / out / KAG tag\n\
                 bt|f|kag            backtrace / location / KAG stop info\n\
                 p <expr>|regs|dis   evaluate / registers / disassemble\n\
                 set reg <n> <expr>  write register\n\
                 catch [on|off]      break on exception\n\
                 q                   quit"
            );
            CommandOutcome::Stay
        }
        "q" | "quit" => CommandOutcome::Resume(DebugAction::Quit),
        _ => {
            println!("unknown command `{command}` (h for help)");
            CommandOutcome::Stay
        }
    }
}
