use crate::bytecode::BytecodeFile;
use crate::error::{Result, TjsError};
use crate::frontend::diagnostic::DiagnosticSeverity;
use crate::frontend::syntax;
use crate::runtime::{Runtime, Variant};
use crate::{FrontendOptions, FrontendOutput};

pub mod codegen;
pub mod mir;

use self::mir::{MirModule, lower_hir_program};

pub use self::codegen::compile_mir_to_bytecode;

pub fn parse_source(source: &str) -> Result<syntax::Program> {
    let output = crate::parse_script("inline.tjs", source, FrontendOptions::default());
    output_to_result(output)
}

pub fn compile_source_to_mir(source_name: &str, source: &str) -> Result<MirModule> {
    let output = crate::analyze_script(source_name, source, FrontendOptions::default());
    let program = output_to_result(output)?;
    lower_hir_program(&program, source_name, source)
}

pub fn compile_source_to_bytecode(source_name: &str, source: &str) -> Result<BytecodeFile> {
    let module = compile_source_to_mir(source_name, source)?;
    compile_mir_to_bytecode(&module)
}

pub fn execute_source(source_name: &str, source: &str) -> Result<Variant> {
    let file = compile_source_to_bytecode(source_name, source)?;
    Runtime::new().execute_file(&file)
}

fn output_to_result<T>(output: FrontendOutput<T>) -> Result<T> {
    if let Some(diagnostic) = output
        .diagnostics
        .into_iter()
        .find(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
    {
        return Err(TjsError {
            kind: diagnostic.kind,
            span: diagnostic.span,
            message: diagnostic.message,
        });
    }
    output
        .value
        .ok_or_else(|| TjsError::parse(crate::Span::empty(0), "frontend produced no value"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_compiler_glue_parses_source() {
        let program = parse_source("function f() { return 1; } return f;").expect("parse");
        assert_eq!(program.statements.len(), 2);
    }

    #[test]
    fn source_to_mir_lowers_frontend() {
        let module = compile_source_to_mir("inline.tjs", "return 1;").expect("mir");
        module.validate().expect("valid mir");
        assert!(module.snapshot().contains("Return"));
    }

    #[test]
    fn source_to_bytecode_generates_executable_file() {
        let file = compile_source_to_bytecode("inline.tjs", "return 1 + 2;").expect("bytecode");
        let mut runtime = Runtime::new();
        let mut vm = crate::vm::Vm::new(&file, &mut runtime);
        assert_eq!(
            vm.execute_top_level().expect("execute"),
            Variant::Integer(3)
        );
    }

    #[test]
    fn execute_source_runs_control_flow_and_assignment() {
        assert_eq!(
            execute_source(
                "inline.tjs",
                "var x = 0; while (x < 3) { x += 1; } return x;"
            )
            .expect("execute"),
            Variant::Integer(3)
        );
    }

    #[test]
    fn execute_source_runs_function_with_default_arg() {
        assert_eq!(
            execute_source(
                "inline.tjs",
                "function f(a, b = 2) { return a + b; } return f(3);"
            )
            .expect("execute"),
            Variant::Integer(5)
        );
    }

    #[test]
    fn execute_source_builds_and_indexes_array() {
        assert_eq!(
            execute_source("inline.tjs", "var a = [1, 4, 9]; return a[1];").expect("execute"),
            Variant::Integer(4)
        );
    }

    #[test]
    fn execute_source_runs_direct_and_indirect_method_calls() {
        assert_eq!(
            execute_source("inline.tjs", r#"return "abcd".substr(1, 2);"#).expect("execute"),
            Variant::String("bc".to_string())
        );
        assert_eq!(
            execute_source("inline.tjs", r#"var f = "substr"; return "abcd"[f](1, 2);"#)
                .expect("execute"),
            Variant::String("bc".to_string())
        );
    }

    #[test]
    fn execute_source_passes_new_arguments_to_function_constructor() {
        assert_eq!(
            execute_source(
                "inline.tjs",
                "function F(a){ this.x = a; } var o = new F(3); return o.x;"
            )
            .expect("execute"),
            Variant::Integer(3)
        );
    }

    #[test]
    fn execute_source_marks_class_instances_for_instanceof() {
        assert_eq!(
            execute_source(
                "inline.tjs",
                r#"class C { } var c = new C(); return c instanceof "C";"#
            )
            .expect("execute"),
            Variant::Integer(1)
        );
        assert_eq!(
            execute_source(
                "inline.tjs",
                r#"
                class Base { }
                class C extends Base { }
                var c = new C();
                return (c instanceof "C") + (c instanceof "Base");
                "#
            )
            .expect("execute"),
            Variant::Integer(2)
        );
    }

    #[test]
    fn ignore_prop_compound_assignment_uses_raw_member_access() {
        let direct = disassemble_top_level("var o = %[]; o.p = 1; &o.p += 2; return o.p;");
        assert!(direct.iter().any(|line| line.contains("gpds")));
        assert!(direct.iter().any(|line| line.contains("spds")));
        assert!(!direct.iter().any(|line| line.contains("addpd")));

        let indirect =
            disassemble_top_level(r#"var o = %[]; var k = "p"; o.p = 1; &o[k] += 2; return o.p;"#);
        assert!(indirect.iter().any(|line| line.contains("gpis")));
        assert!(indirect.iter().any(|line| line.contains("spis")));
        assert!(!indirect.iter().any(|line| line.contains("addpi")));
    }

    #[test]
    fn ignore_prop_member_update_uses_raw_member_access() {
        let direct = disassemble_top_level("var o = %[]; o.p = 1; (&o.p)++; return o.p;");
        assert!(direct.iter().any(|line| line.contains("gpds")));
        assert!(direct.iter().any(|line| line.contains("spds")));
        assert!(!direct.iter().any(|line| line.contains("incpd")));

        let indirect =
            disassemble_top_level(r#"var o = %[]; var k = "p"; o.p = 1; (&o[k])++; return o.p;"#);
        assert!(indirect.iter().any(|line| line.contains("gpis")));
        assert!(indirect.iter().any(|line| line.contains("spis")));
        assert!(!indirect.iter().any(|line| line.contains("incpi")));
    }

    fn disassemble_top_level(source: &str) -> Vec<String> {
        let file = compile_source_to_bytecode("inline.tjs", source).expect("bytecode");
        file.disassemble_object(file.top_level.expect("top-level"))
            .expect("disassemble")
    }
}
