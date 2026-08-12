//! TJS2 bytecode decompiler.
//!
//! Recovers readable, recompilable TJS2 source from `TJS2100\0` bytecode:
//!
//! ```text
//! BytecodeFile -> skeleton (declaration lifting) -> statement scanner
//!              -> syntax::Program -> printer -> text
//! ```
//!
//! The scanner is a forward dataflow pass keyed to the official compiler's
//! emission patterns (see `stmt`); fragments no pattern covers yet are
//! emitted as `// <unhandled: ...>` comments — decompilation never fails on
//! unknown bytecode, it degrades.

mod control;
mod emit;
mod expr;
#[cfg(test)]
mod fuzz;
mod naming;
mod skeleton;
mod stmt;

use std::cell::RefCell;

use crate::bytecode::BytecodeFile;
use crate::error::{Result, TjsError};
use crate::frontend::syntax::Program;

pub use emit::{DecompiledSource, DecompileOutput, DecompileStats};

thread_local! {
    /// Object indices whose bodies are currently being decompiled (the
    /// literal-inlining recursion chain). Malformed bytecode can reference
    /// objects cyclically; the guard degrades such cycles to placeholders
    /// instead of recursing forever.
    static DECOMPILE_CHAIN: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    /// Total unhandled-fragment markers created during the current
    /// decompilation (across every nested code object).
    static UNHANDLED_TOTAL: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(crate) fn count_unhandled_fragment() {
    UNHANDLED_TOTAL.with(|total| total.set(total.get() + 1));
}

fn take_unhandled_total() -> usize {
    UNHANDLED_TOTAL.with(|total| total.replace(0))
}

/// RAII entry into the decompilation chain; returns `None` when
/// `object_index` is already on the chain (a cycle).
pub(crate) struct DecompileChainGuard {
    entered: bool,
}

impl DecompileChainGuard {
    pub(crate) fn enter(object_index: usize) -> Option<Self> {
        DECOMPILE_CHAIN.with(|chain| {
            let mut chain = chain.borrow_mut();
            if chain.contains(&object_index) {
                return None;
            }
            chain.push(object_index);
            Some(Self { entered: true })
        })
    }
}

impl Drop for DecompileChainGuard {
    fn drop(&mut self) {
        if self.entered {
            DECOMPILE_CHAIN.with(|chain| {
                chain.borrow_mut().pop();
            });
        }
    }
}

/// Options controlling [`decompile`].
#[derive(Clone, Debug, Default)]
pub struct DecompileOptions {
    /// Only decompile code objects whose name contains this substring.
    pub object_name_filter: Option<String>,
    /// Only decompile code object `n` (its whole file is still emitted so
    /// the output recompiles, but only that object's body is decompiled;
    /// other bodies become unhandled placeholders).
    pub object_index: Option<usize>,
}

/// Decompiles a bytecode file into TJS2 source text.
pub fn decompile(file: &BytecodeFile, options: &DecompileOptions) -> Result<DecompileOutput> {
    let objects = skeleton::select_objects(
        file,
        options.object_name_filter.as_deref(),
        options.object_index,
    )?;
    let mut stats = DecompileStats::default();
    stats.objects = objects.len();
    let top_index = file.top_level;
    let _guard = top_index.and_then(DecompileChainGuard::enter);
    let mut statements = skeleton::decompile_file(file, &objects, &mut stats.unhandled)?;
    skeleton::merge_for_init(&mut statements);
    // The marker count spans every nested code object decompiled in place.
    stats.unhandled = take_unhandled_total();
    let program = Program {
        statements,
        span: crate::error::Span::empty(0),
    };
    let name = file
        .debug_info
        .sources
        .first()
        .map(|source| source.name.clone())
        .or_else(|| {
            file.top_level
                .map(|index| file.objects[index].name(file).unwrap_or("").to_string())
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "decompiled.tjs".to_string());
    let text = emit::render_program(&program, &name);
    Ok(DecompileOutput {
        sources: vec![DecompiledSource { name, text }],
        stats,
    })
}

/// Decompiles and re-parses the output as a sanity check (used by tests and
/// the `--verify` path of the CLI).
pub fn decompile_and_parse(file: &BytecodeFile, options: &DecompileOptions) -> Result<Program> {
    let output = decompile(file, options)?;
    let text = output
        .sources
        .into_iter()
        .next()
        .map(|source| source.text)
        .unwrap_or_default();
    crate::compiler::parse_source(&text).map_err(|error| {
        TjsError::bytecode(format!("decompiled output does not reparse: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Variant;

    fn execute(file: &BytecodeFile) -> Variant {
        crate::runtime::Runtime::new()
            .execute_file(file)
            .expect("execute")
    }

    fn round_trip(source: &str) -> String {
        let file =
            crate::compiler::compile_source_to_bytecode("round_trip.tjs", source).expect("compile");
        let output = decompile(&file, &DecompileOptions::default()).expect("decompile");
        let text = output.sources[0].text.clone();
        // The output must reparse and recompile.
        let reparsed = crate::compiler::parse_source(&text).expect("decompiled output reparse");
        assert!(!reparsed.statements.is_empty(), "empty program from {source:?}");
        let file2 =
            crate::compiler::compile_source_to_bytecode("round_trip.tjs", &text).expect("recompile");
        // And behave identically in the VM.
        assert_eq!(execute(&file), execute(&file2), "semantic mismatch for {source:?}\n{text}");
        text
    }

    #[test]
    fn round_trips_linear_scripts() {
        round_trip("return 1 + 2 * 3;");
        round_trip("var x = 5; x = x + 1; return x;");
        round_trip("function inc(x) { return x + 1; } var a = %[\"b\" => %[\"c\" => inc]]; var d = %[\"e\" => 2]; return a.b.c(1, 2) + d.e;");
        round_trip("return \"hello \" + \"world\";");
        round_trip("var d = %[\"a\" => 1, 2 => 3]; return d.a;");
        round_trip("return [1, 2, 3];");
        round_trip("function f(*) { return 0; } return f(...);");
        round_trip("var x = 5; return -x + !0;");
        round_trip("var a = %[\"b\" => 1]; return typeof a.b;");
        round_trip("function Base() {} return Base instanceof Object;");
    }

    #[test]
    fn round_trips_function_declarations() {
        round_trip("function add(a, b) { return a + b; } return add(1, 2);");
        round_trip("function f(a) { return a + 1; } return f(41);");
    }

    #[test]
    fn round_trips_control_flow() {
        round_trip("var x = 0; if (x) { x = 1; } else { x = 2; } return x;");
        round_trip("var i = 0; while (i < 3) { i++; } return i;");
        round_trip("var i = 3; do { i--; } while (i > 0); return i;");
        round_trip("var s = 0; for (var j = 0; j < 4; j++) { s += j; } return s;");
        round_trip("var i = 0; while (i < 9) { i++; if (i > 3) { break; } } return i;");
        round_trip("var s = 0; for (var j = 0; j < 5; j++) { if (j == 2) { continue; } s += j; } return s;");
        round_trip("var a = 1; var b = 0; var x = a && b; var y = a || b; return x + y;");
        round_trip("var a = 1; if (a && 2) { return 1; } if (0 || a) { return 2; } return 3;");
        round_trip("try { return 1; } catch (e) { return 2; }");
        round_trip("var t = a ? 1 : 2; return t;");
        round_trip("var x = 2; var r = 0; switch (x) { case 1: r = 1; case 2: r = 2; break; default: r = 3; } return r;");
        round_trip("var x = 9; var r = 0; switch (x) { case 1: r = 1; break; case 9: r = 9; break; } return r;");
        round_trip("function f(a, b = 2) { return a + b; } return f(1) + f(1, 3);");
    }

    #[test]
    fn round_trips_classes_and_properties() {
        round_trip(
            "class Base { function m() { return 1; } } class Derived extends Base { function n() { return this.m() + 1; } } var d = new Derived(); return d.n();",
        );
        round_trip(
            "var p = 0; property holder { getter() { return p; } setter(v) { p = v; } } holder = 41; return holder;",
        );
    }
}
