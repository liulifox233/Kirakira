//! Interactive debugger support for the TJS2 VM and the engine's KAG loop.
//!
//! The VM checks [`Debugger`] before every dispatched instruction (and on the
//! central runtime-error path). When a stop condition matches, the registered
//! [`DebugUi`] is invoked synchronously with a [`Pause`] context that exposes
//! the call stack, registers, expression evaluation, and disassembly. The UI
//! returns a [`DebugAction`] which drives the stepping state machine.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::bytecode::{BytecodeFile, CodeObject};
use crate::error::{Result, TjsError, TjsStackFrame};
use crate::runtime::{Runtime, TjsHost, Variant};
use crate::vm::Frame;

/// The TJS `debugger;` statement compiles to this opcode.
const DEBUGGER_OPCODE: u8 = 127;

/// Identifies a source position for step-mode comparisons: `(file_id, line)`
/// where `line` falls back to the UTF-16 source offset when no source text is
/// available (making line stepping degrade to instruction stepping).
pub type LocationKey = (usize, usize);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum StepMode {
    #[default]
    Continue,
    StepInst,
    StepInto {
        last: Option<LocationKey>,
    },
    StepOver {
        depth: usize,
        last: Option<LocationKey>,
    },
    StepOut {
        depth: usize,
    },
}

/// Why execution stopped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StopReason {
    /// A resolved TJS source-line breakpoint was hit.
    Breakpoint,
    /// A KAG line/label breakpoint or a KAG tag step stopped before a tag.
    Kag {
        storage: String,
        line: Option<usize>,
        label: Option<String>,
        stepped: bool,
    },
    /// A step mode (`si`/`s`/`n`/`fin`) reached its target.
    Step,
    /// A TJS runtime error is about to be handled (`caught`) or to unwind.
    Exception { caught: bool, message: String },
    /// The script executed a `debugger;` statement.
    DebuggerStmt,
}

/// What the debug UI wants execution to do next.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DebugAction {
    Continue,
    /// Stop at the next instruction.
    StepInst,
    /// Stop at the next instruction on a different source line (steps into
    /// calls).
    StepInto,
    /// Stop at the next instruction on a different source line at the current
    /// call depth or shallower (steps over calls).
    StepOver,
    /// Stop when the call stack becomes shallower than the current depth.
    StepOut,
    /// Stop before the next KAG tag.
    KagStep,
    /// Terminate the debug session. Propagates as [`TjsError::debug_quit`].
    Quit,
}

#[derive(Clone, Debug)]
struct TjsBreakpoint {
    id: usize,
    file_pattern: String,
    line: usize,
}

#[derive(Clone, Debug)]
struct KagLineBreakpoint {
    id: usize,
    storage_pattern: String,
    line: usize,
}

#[derive(Clone, Debug)]
struct KagLabelBreakpoint {
    id: usize,
    label: String,
}

/// Debugger state shared by the VM (TJS stops) and the engine (KAG stops).
///
/// Stored on [`Runtime`] via [`Runtime::enable_debugger`]; when no debugger is
/// enabled the VM hook is a single `Option` check per instruction.
#[derive(Default)]
pub struct Debugger {
    tjs_breakpoints: Vec<TjsBreakpoint>,
    kag_line_breakpoints: Vec<KagLineBreakpoint>,
    kag_label_breakpoints: Vec<KagLabelBreakpoint>,
    // (file_id, object_index) -> resolved code offsets. Keyed per code
    // object because offsets are only unique within one function.
    resolved: HashMap<(usize, usize), HashSet<u32>>,
    seen_files: HashSet<usize>,
    file_line_starts: HashMap<usize, Option<Arc<Vec<usize>>>>,
    next_id: usize,
    mode: StepMode,
    kag_step: bool,
    break_on_exception: bool,
    eval_depth: usize,
}

impl Debugger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a TJS source-line breakpoint. `file_pattern` matches a script
    /// storage name exactly or as a `/`-separated suffix (e.g. `startup.tjs`
    /// matches `scripts/startup.tjs`). Returns the breakpoint id.
    pub fn add_tjs_breakpoint(&mut self, file_pattern: impl Into<String>, line: usize) -> usize {
        let id = self.alloc_id();
        self.tjs_breakpoints.push(TjsBreakpoint {
            id,
            file_pattern: file_pattern.into(),
            line,
        });
        // Force lazy re-resolution against already-loaded script files.
        self.resolved.clear();
        self.seen_files.clear();
        id
    }

    /// Adds a KAG line breakpoint (e.g. `first.ks:12`). Returns the id.
    pub fn add_kag_line_breakpoint(
        &mut self,
        storage_pattern: impl Into<String>,
        line: usize,
    ) -> usize {
        let id = self.alloc_id();
        self.kag_line_breakpoints.push(KagLineBreakpoint {
            id,
            storage_pattern: storage_pattern.into(),
            line,
        });
        id
    }

    /// Adds a KAG label breakpoint. A leading `*` on the label is optional.
    pub fn add_kag_label_breakpoint(&mut self, label: impl Into<String>) -> usize {
        let id = self.alloc_id();
        self.kag_label_breakpoints.push(KagLabelBreakpoint {
            id,
            label: normalize_label(&label.into()),
        });
        id
    }

    /// Removes a breakpoint by id. Returns whether one existed.
    pub fn remove_breakpoint(&mut self, id: usize) -> bool {
        let before = self.tjs_breakpoints.len();
        self.tjs_breakpoints.retain(|bp| bp.id != id);
        let tjs_removed = self.tjs_breakpoints.len() != before;
        if tjs_removed {
            self.resolved.clear();
            self.seen_files.clear();
        }
        let before = self.kag_line_breakpoints.len() + self.kag_label_breakpoints.len();
        self.kag_line_breakpoints.retain(|bp| bp.id != id);
        self.kag_label_breakpoints.retain(|bp| bp.id != id);
        tjs_removed || self.kag_line_breakpoints.len() + self.kag_label_breakpoints.len() != before
    }

    /// Human-readable breakpoint list for the UI.
    pub fn breakpoint_descriptions(&self) -> Vec<String> {
        let mut out = Vec::new();
        for bp in &self.tjs_breakpoints {
            out.push(format!("#{} tjs {}:{}", bp.id, bp.file_pattern, bp.line));
        }
        for bp in &self.kag_line_breakpoints {
            out.push(format!("#{} kag {}:{}", bp.id, bp.storage_pattern, bp.line));
        }
        for bp in &self.kag_label_breakpoints {
            out.push(format!("#{} kag-label *{}", bp.id, bp.label));
        }
        out
    }

    pub fn set_break_on_exception(&mut self, enabled: bool) {
        self.break_on_exception = enabled;
    }

    pub fn break_on_exception(&self) -> bool {
        self.break_on_exception
    }

    /// Stops at the very next dispatched instruction. Set before startup to
    /// pause on the first executed instruction.
    pub fn pause_at_start(&mut self) {
        self.mode = StepMode::StepInst;
    }

    /// Applies a UI action to the stepping state machine. `key` and `depth`
    /// describe the stop location for TJS pauses; KAG pauses pass `None`/`0`.
    pub fn apply_action(
        &mut self,
        action: DebugAction,
        key: Option<LocationKey>,
        depth: usize,
    ) -> Result<()> {
        match action {
            DebugAction::Continue => {
                self.mode = StepMode::Continue;
                self.kag_step = false;
            }
            DebugAction::StepInst => self.mode = StepMode::StepInst,
            DebugAction::StepInto => self.mode = StepMode::StepInto { last: key },
            DebugAction::StepOver => self.mode = StepMode::StepOver { depth, last: key },
            DebugAction::StepOut => self.mode = StepMode::StepOut { depth },
            DebugAction::KagStep => {
                self.mode = StepMode::Continue;
                self.kag_step = true;
            }
            DebugAction::Quit => return Err(TjsError::debug_quit()),
        }
        Ok(())
    }

    /// Called by the engine before processing each KAG tag.
    pub fn check_kag(
        &mut self,
        storage: &str,
        line: Option<usize>,
        label: Option<&str>,
    ) -> Option<StopReason> {
        if self.eval_depth > 0 {
            return None;
        }
        let stepped = self.kag_step;
        let line_hit = line.is_some_and(|line| {
            self.kag_line_breakpoints
                .iter()
                .any(|bp| bp.line == line && name_matches(storage, &bp.storage_pattern))
        });
        let label_hit = label.is_some_and(|label| {
            self.kag_label_breakpoints
                .iter()
                .any(|bp| normalize_label(label) == bp.label)
        });
        (stepped || line_hit || label_hit).then(|| StopReason::Kag {
            storage: storage.to_string(),
            line,
            label: label.map(str::to_string),
            stepped,
        })
    }

    /// Called by the VM before each instruction dispatch.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn check_tjs(
        &mut self,
        file: &BytecodeFile,
        file_id: usize,
        object_index: Option<usize>,
        object: &CodeObject,
        offset: usize,
        opcode: u8,
        depth: usize,
    ) -> Option<StopReason> {
        if self.eval_depth > 0 {
            return None;
        }
        if opcode == DEBUGGER_OPCODE {
            return Some(StopReason::DebuggerStmt);
        }
        if !self.tjs_breakpoints.is_empty()
            && let Some(object_index) = object_index
        {
            self.ensure_resolved(file_id, file);
            if self
                .resolved
                .get(&(file_id, object_index))
                .is_some_and(|offsets| offsets.contains(&(offset as u32)))
            {
                return Some(StopReason::Breakpoint);
            }
        }
        match self.mode {
            StepMode::Continue => None,
            StepMode::StepInst => Some(StopReason::Step),
            StepMode::StepInto { last } => {
                let key = self.location_key(file, file_id, object, offset);
                (Some(key) != last).then_some(StopReason::Step)
            }
            StepMode::StepOver {
                depth: stop_depth,
                last,
            } => {
                if depth > stop_depth {
                    return None;
                }
                let key = self.location_key(file, file_id, object, offset);
                (Some(key) != last).then_some(StopReason::Step)
            }
            StepMode::StepOut { depth: stop_depth } => {
                (depth < stop_depth).then_some(StopReason::Step)
            }
        }
    }

    fn alloc_id(&mut self) -> usize {
        self.next_id += 1;
        self.next_id
    }

    fn ensure_resolved(&mut self, file_id: usize, file: &BytecodeFile) {
        if self.seen_files.contains(&file_id) {
            return;
        }
        self.seen_files.insert(file_id);
        let Some(source) = file.debug_info.sources.first() else {
            return;
        };
        let lines: Vec<usize> = self
            .tjs_breakpoints
            .iter()
            .filter(|bp| name_matches(&source.name, &bp.file_pattern))
            .map(|bp| bp.line)
            .collect();
        if lines.is_empty() {
            return;
        }
        let Some(text) = source.text.as_deref() else {
            return;
        };
        let starts = line_start_offsets(text);
        for (object_index, object) in file.objects.iter().enumerate() {
            // Resolve each line to the first code position of the first basic
            // block on that line, so a line breakpoint stops once per line.
            let mut best: HashMap<usize, u32> = HashMap::new();
            for position in &object.source_positions {
                let line = starts.partition_point(|start| *start <= position.source_pos as usize);
                if lines.contains(&line) {
                    best.entry(line)
                        .and_modify(|code_pos| *code_pos = (*code_pos).min(position.code_pos))
                        .or_insert(position.code_pos);
                }
            }
            if !best.is_empty() {
                self.resolved
                    .entry((file_id, object_index))
                    .or_default()
                    .extend(best.into_values());
            }
        }
    }

    fn location_key(
        &mut self,
        file: &BytecodeFile,
        file_id: usize,
        object: &CodeObject,
        offset: usize,
    ) -> LocationKey {
        let utf16_offset = object
            .source_positions
            .iter()
            .take_while(|position| position.code_pos as usize <= offset)
            .last()
            .map(|position| position.source_pos as usize)
            .unwrap_or(0);
        match self.line_for_utf16_offset(file, file_id, utf16_offset) {
            Some(line) => (file_id, line),
            None => (file_id, utf16_offset),
        }
    }

    fn line_for_utf16_offset(
        &mut self,
        file: &BytecodeFile,
        file_id: usize,
        utf16_offset: usize,
    ) -> Option<usize> {
        let starts = self
            .file_line_starts
            .entry(file_id)
            .or_insert_with(|| {
                let text = file.debug_info.sources.first()?.text.as_ref()?;
                Some(Arc::new(line_start_offsets(text)))
            })
            .clone()?;
        Some(starts.partition_point(|start| *start <= utf16_offset))
    }
}

/// UTF-16 offsets at which each line of `text` begins (line 1 starts at 0).
fn line_start_offsets(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    let mut offset = 0;
    for ch in text.chars() {
        offset += ch.len_utf16();
        if ch == '\n' {
            starts.push(offset);
        }
    }
    starts
}

fn name_matches(name: &str, pattern: &str) -> bool {
    name == pattern
        || (name.len() > pattern.len()
            && name.ends_with(pattern)
            && name.as_bytes()[name.len() - pattern.len() - 1] == b'/')
}

fn normalize_label(label: &str) -> String {
    label.strip_prefix('*').unwrap_or(label).to_string()
}

/// Synchronous debug UI invoked while execution is paused.
///
/// The UI is taken out of the [`Runtime`] before `on_pause` is called and put
/// back afterwards, so [`Pause`] can hand out full `&mut Runtime` access for
/// inspection and evaluation. Implementations must be `Send`; the runtime
/// itself stays single-threaded.
pub trait DebugUi<H: TjsHost + 'static>: Send {
    fn on_pause(&mut self, pause: &mut Pause<'_, H>) -> DebugAction;
}

pub(crate) struct TjsPause<'a> {
    frame: &'a mut Frame,
    file: Arc<BytecodeFile>,
    object_index: Option<usize>,
}

/// Inspection context handed to [`DebugUi::on_pause`].
pub struct Pause<'a, H: TjsHost + 'static> {
    reason: StopReason,
    runtime: &'a mut Runtime<H>,
    tjs: Option<TjsPause<'a>>,
    backtrace: Vec<TjsStackFrame>,
}

impl<'a, H: TjsHost + 'static> Pause<'a, H> {
    pub(crate) fn new_tjs(
        reason: StopReason,
        runtime: &'a mut Runtime<H>,
        frame: &'a mut Frame,
        file: Arc<BytecodeFile>,
        object_index: Option<usize>,
        backtrace: Vec<TjsStackFrame>,
    ) -> Self {
        Self {
            reason,
            runtime,
            tjs: Some(TjsPause {
                frame,
                file,
                object_index,
            }),
            backtrace,
        }
    }

    /// A pause without a live TJS call stack (KAG tag stop).
    pub fn new_kag(reason: StopReason, runtime: &'a mut Runtime<H>) -> Self {
        Self {
            reason,
            runtime,
            tjs: None,
            backtrace: Vec::new(),
        }
    }

    pub fn reason(&self) -> &StopReason {
        &self.reason
    }

    /// Innermost frame first; empty for KAG stops.
    pub fn backtrace(&self) -> &[TjsStackFrame] {
        &self.backtrace
    }

    pub fn location(&self) -> Option<&TjsStackFrame> {
        self.backtrace.first()
    }

    pub fn runtime(&mut self) -> &mut Runtime<H> {
        self.runtime
    }

    /// All registers of the innermost frame: `-1` = `this`, `-2` = this-proxy,
    /// `-3..` = locals/args, `0..` = temporaries. Empty for KAG stops.
    pub fn registers(&self) -> Vec<(i16, Variant)> {
        self.tjs
            .as_ref()
            .map(|tjs| tjs.frame.debug_registers())
            .unwrap_or_default()
    }

    pub fn read_register(&self, reg: i16) -> Option<Variant> {
        self.tjs.as_ref()?.frame.get(reg).ok()
    }

    pub fn write_register(&mut self, reg: i16, value: Variant) -> Result<()> {
        let Some(tjs) = self.tjs.as_mut() else {
            return Err(TjsError::runtime("not paused in TJS code"));
        };
        tjs.frame.set(reg, value)
    }

    /// Evaluates a TJS expression in the global context. Debugger checks are
    /// suppressed while it runs. Local variables are not in scope; use
    /// [`Pause::registers`] for locals.
    pub fn eval(&mut self, expression: &str) -> Result<Variant> {
        let source = format!("return ({expression});");
        let file = crate::compiler::compile_source_to_bytecode("<debug-eval>", &source)?;
        if let Some(debugger) = self.runtime.debugger.as_mut() {
            debugger.eval_depth += 1;
        }
        let result = self.runtime.execute_file_with_this(&file, None);
        if let Some(debugger) = self.runtime.debugger.as_mut() {
            debugger.eval_depth = debugger.eval_depth.saturating_sub(1);
        }
        result
    }

    /// Disassembles the function containing the stop location.
    pub fn disassemble_current(&self) -> Option<Result<Vec<String>>> {
        let tjs = self.tjs.as_ref()?;
        Some(tjs.file.disassemble_object(tjs.object_index?))
    }

    /// The source line text of the innermost stop location, if available.
    pub fn current_source_line(&self) -> Option<String> {
        let tjs = self.tjs.as_ref()?;
        let location = self.backtrace.first()?.source.as_ref()?;
        let text = tjs.file.debug_info.sources.first()?.text.as_ref()?;
        text.lines()
            .nth(location.line?.saturating_sub(1))
            .map(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::compiler::compile_source_to_bytecode;
    use crate::runtime::NoHost;

    #[derive(Clone, Debug, Default)]
    struct StopLog {
        entries: Vec<StopEntry>,
    }

    #[derive(Clone, Debug)]
    struct StopEntry {
        reason: String,
        line: Option<usize>,
        depth: usize,
    }

    type Handler = Box<dyn for<'a> FnMut(&mut Pause<'a, NoHost>) -> DebugAction + Send>;

    struct MockUi {
        log: Arc<Mutex<StopLog>>,
        actions: std::collections::VecDeque<DebugAction>,
        handler: Option<Handler>,
    }

    impl MockUi {
        fn scripted(log: Arc<Mutex<StopLog>>, actions: Vec<DebugAction>) -> Self {
            Self {
                log,
                actions: actions.into(),
                handler: None,
            }
        }

        fn with_handler(log: Arc<Mutex<StopLog>>, handler: Handler) -> Self {
            Self {
                log,
                actions: Default::default(),
                handler: Some(handler),
            }
        }
    }

    impl DebugUi<NoHost> for MockUi {
        fn on_pause(&mut self, pause: &mut Pause<'_, NoHost>) -> DebugAction {
            let line = pause
                .location()
                .and_then(|frame| frame.source.as_ref())
                .and_then(|source| source.line);
            self.log.lock().unwrap().entries.push(StopEntry {
                reason: format!("{:?}", pause.reason()),
                line,
                depth: pause.backtrace().len(),
            });
            if let Some(handler) = self.handler.as_mut() {
                return handler(pause);
            }
            self.actions.pop_front().unwrap_or(DebugAction::Continue)
        }
    }

    fn run_with_ui(source_name: &str, source: &str, ui: MockUi) -> Result<Variant> {
        let file = compile_source_to_bytecode(source_name, source).expect("compile");
        let mut runtime = Runtime::new();
        runtime.enable_debugger();
        runtime.set_debug_ui(Box::new(ui));
        runtime.execute_file(&file)
    }

    fn lines(log: &Arc<Mutex<StopLog>>) -> Vec<Option<usize>> {
        log.lock()
            .unwrap()
            .entries
            .iter()
            .map(|entry| entry.line)
            .collect()
    }

    #[test]
    fn line_breakpoint_stops_on_the_requested_line() {
        let log = Arc::new(Mutex::new(StopLog::default()));
        let source = "var a = 1;\nvar b = 2;\nvar c = a + b;\nc;\n";
        let file = compile_source_to_bytecode("test.tjs", source).expect("compile");
        let mut runtime = Runtime::new();
        runtime.enable_debugger().add_tjs_breakpoint("test.tjs", 3);
        runtime.set_debug_ui(Box::new(MockUi::scripted(Arc::clone(&log), vec![])));
        runtime.execute_file(&file).expect("run");

        let entries = log.lock().unwrap();
        assert_eq!(entries.entries.len(), 1);
        assert_eq!(entries.entries[0].line, Some(3));
        assert!(entries.entries[0].reason.starts_with("Breakpoint"));
    }

    #[test]
    fn breakpoint_file_pattern_matches_as_suffix() {
        let log = Arc::new(Mutex::new(StopLog::default()));
        let source = "var a = 1;\na;\n";
        let file = compile_source_to_bytecode("scripts/deep/test.tjs", source).expect("compile");
        let mut runtime = Runtime::new();
        runtime.enable_debugger().add_tjs_breakpoint("test.tjs", 2);
        runtime.set_debug_ui(Box::new(MockUi::scripted(Arc::clone(&log), vec![])));
        runtime.execute_file(&file).expect("run");
        assert_eq!(log.lock().unwrap().entries.len(), 1);
    }

    #[test]
    fn step_into_over_and_out_follow_source_lines() {
        let log = Arc::new(Mutex::new(StopLog::default()));
        let source = concat!(
            "var r = 0;\n",           // 1
            "function add(a, b) {\n", // 2
            "    var s = a + b;\n",   // 3
            "    return s;\n",        // 4
            "}\n",                    // 5
            "r = add(1, 2);\n",       // 6
            "r = add(3, 4);\n",       // 7
            "global.result = r;\n",   // 8
        );
        let file = compile_source_to_bytecode("test.tjs", source).expect("compile");
        let mut runtime = Runtime::new();
        runtime.enable_debugger().add_tjs_breakpoint("test.tjs", 6);
        runtime.set_debug_ui(Box::new(MockUi::scripted(
            Arc::clone(&log),
            vec![
                DebugAction::StepInto,
                DebugAction::StepOver,
                DebugAction::StepOut,
            ],
        )));
        runtime.execute_file(&file).expect("run");
        assert_eq!(runtime.global_member("result"), Variant::Integer(7));

        // breakpoint at 6, step into add -> 3, step over -> 4, step out ->
        // back at the call site on 6 (the assignment completes there).
        assert_eq!(
            lines(&log),
            vec![Some(6), Some(3), Some(4), Some(6)],
            "unexpected stop line sequence"
        );
    }

    #[test]
    fn step_inst_stops_on_consecutive_instructions() {
        let log = Arc::new(Mutex::new(StopLog::default()));
        let source = "var a = 1;\nvar b = 2;\nvar c = 3;\nc;\n";
        let file = compile_source_to_bytecode("test.tjs", source).expect("compile");
        let mut runtime = Runtime::new();
        runtime.enable_debugger().add_tjs_breakpoint("test.tjs", 2);
        runtime.set_debug_ui(Box::new(MockUi::scripted(
            Arc::clone(&log),
            vec![DebugAction::StepInst; 3],
        )));
        runtime.execute_file(&file).expect("run");
        assert_eq!(log.lock().unwrap().entries.len(), 1 + 3);
    }

    #[test]
    fn registers_are_visible_and_writable_at_a_stop() {
        let log = Arc::new(Mutex::new(StopLog::default()));
        let source = concat!(
            "function f() {\n",         // 1
            "    var a = 41;\n",        // 2
            "    global.result = a;\n", // 3
            "}\n",                      // 4
            "f();\n",                   // 5
        );
        let file = compile_source_to_bytecode("test.tjs", source).expect("compile");
        let mut runtime = Runtime::new();
        runtime.enable_debugger().add_tjs_breakpoint("test.tjs", 3);
        runtime.set_debug_ui(Box::new(MockUi::with_handler(
            Arc::clone(&log),
            Box::new(|pause| {
                // Function locals live in the negative registers (reg <= -3);
                // positive registers are temporaries.
                let target = pause
                    .registers()
                    .into_iter()
                    .find(|(reg, value)| *reg <= -3 && *value == Variant::Integer(41))
                    .map(|(reg, _)| reg)
                    .expect("a local register holds 41");
                pause
                    .write_register(target, Variant::Integer(100))
                    .expect("write register");
                DebugAction::Continue
            }),
        )));
        runtime.execute_file(&file).expect("run");
        assert_eq!(runtime.global_member("result"), Variant::Integer(100));
    }

    #[test]
    fn break_on_exception_distinguishes_caught_from_uncaught() {
        let caught_log = Arc::new(Mutex::new(StopLog::default()));
        let source = "try { missing_fn(); } catch (e) {}\n1;\n";
        let file = compile_source_to_bytecode("test.tjs", source).expect("compile");
        let mut runtime = Runtime::new();
        runtime.enable_debugger().set_break_on_exception(true);
        runtime.set_debug_ui(Box::new(MockUi::scripted(Arc::clone(&caught_log), vec![])));
        runtime.execute_file(&file).expect("caught run");
        {
            let entries = caught_log.lock().unwrap();
            assert_eq!(entries.entries.len(), 1);
            assert!(
                entries.entries[0]
                    .reason
                    .starts_with("Exception { caught: true")
            );
        }

        let uncaught_log = Arc::new(Mutex::new(StopLog::default()));
        let source = "missing_fn();\n";
        let file = compile_source_to_bytecode("test.tjs", source).expect("compile");
        let mut runtime = Runtime::new();
        runtime.enable_debugger().set_break_on_exception(true);
        runtime.set_debug_ui(Box::new(MockUi::scripted(
            Arc::clone(&uncaught_log),
            vec![],
        )));
        assert!(runtime.execute_file(&file).is_err());
        let entries = uncaught_log.lock().unwrap();
        assert_eq!(entries.entries.len(), 1);
        assert!(
            entries.entries[0]
                .reason
                .starts_with("Exception { caught: false")
        );
    }

    #[test]
    fn debugger_statement_triggers_a_stop() {
        let log = Arc::new(Mutex::new(StopLog::default()));
        let source = "var a = 1;\ndebugger;\nvar b = 2;\nb;\n";
        let file = compile_source_to_bytecode("test.tjs", source).expect("compile");
        let mut runtime = Runtime::new();
        runtime.enable_debugger();
        runtime.set_debug_ui(Box::new(MockUi::scripted(Arc::clone(&log), vec![])));
        runtime.execute_file(&file).expect("run");
        let entries = log.lock().unwrap();
        assert_eq!(entries.entries.len(), 1);
        assert_eq!(entries.entries[0].reason, "DebuggerStmt");
        assert_eq!(entries.entries[0].line, Some(2));
    }

    #[test]
    fn eval_runs_in_global_context_and_skips_breakpoints() {
        let log = Arc::new(Mutex::new(StopLog::default()));
        let source = "var a = 5;\nvar b = 6;\nb;\n";
        let file = compile_source_to_bytecode("test.tjs", source).expect("compile");
        let mut runtime = Runtime::new();
        runtime.enable_debugger().add_tjs_breakpoint("test.tjs", 2);
        // Also break inside the eval source; it must not fire while evaluating.
        runtime
            .enable_debugger()
            .add_tjs_breakpoint("<debug-eval>", 1);
        runtime.set_debug_ui(Box::new(MockUi::with_handler(
            Arc::clone(&log),
            Box::new(|pause| {
                let value = pause.eval("1 + 2").expect("eval");
                assert_eq!(value, Variant::Integer(3));
                DebugAction::Continue
            }),
        )));
        runtime.execute_file(&file).expect("run");
        assert_eq!(log.lock().unwrap().entries.len(), 1);
    }

    #[test]
    fn quit_aborts_execution_without_being_caught() {
        let log = Arc::new(Mutex::new(StopLog::default()));
        let source = "try { var a = 1; } catch (e) {}\n2;\n";
        let file = compile_source_to_bytecode("test.tjs", source).expect("compile");
        let mut runtime = Runtime::new();
        runtime.enable_debugger().add_tjs_breakpoint("test.tjs", 1);
        runtime.set_debug_ui(Box::new(MockUi::scripted(
            Arc::clone(&log),
            vec![DebugAction::Quit],
        )));
        let error = runtime.execute_file(&file).expect_err("quit aborts");
        assert!(error.is_debug_quit());
    }
}
