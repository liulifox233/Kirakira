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
            contexts: Vec::new(),
            exception_object: None,
            exception_class: None,
            exception_message: None,
        });
    }
    output
        .value
        .ok_or_else(|| TjsError::parse(crate::Span::empty(0), "frontend produced no value"))
}

#[cfg(test)]
mod tests {
    use crate::error::TjsErrorKind;
    use crate::runtime::{ObjectHandle, TjsHost};

    use super::*;

    #[derive(Default)]
    struct InvalidateTrackingHost {
        invalidated: Vec<ObjectHandle>,
    }

    impl TjsHost for InvalidateTrackingHost {
        fn invalidate_object(&mut self, handle: ObjectHandle) {
            self.invalidated.push(handle);
        }
    }

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
        let file_id = runtime.install_script_file(std::sync::Arc::new(file));
        let mut vm = crate::vm::Vm::new(file_id, &mut runtime).expect("vm");
        assert_eq!(
            vm.execute_top_level().expect("execute"),
            Variant::Integer(3)
        );
    }

    #[test]
    fn discarded_eval_operator_evaluates_a_statement_list() {
        // `eval!;` in statement position generates VM_EEXP, and
        // EvalExpression without a result slot parses the string as a
        // statement list.  k2compat's `makeDelay` uses that to build a lazy
        // property object, then reads it back through a plain member access.
        assert_eq!(
            execute_source(
                "inline.tjs",
                r#"var unnamed = %[];
                   (function (e) { e!; } incontextof unnamed)("property _ { getter { return 42; } }");
                   var holder = %[];
                   &holder.value = (&unnamed._) incontextof unnamed;
                   return holder.value;"#
            )
            .expect("execute"),
            Variant::Integer(42)
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
    fn global_functions_are_registered_before_source_order() {
        assert_eq!(
            execute_source("inline.tjs", "return f(4); function f(x) { return x + 3; }")
                .expect("execute"),
            Variant::Integer(7)
        );
    }

    #[test]
    fn compiler_uses_krkr2_argument_register_layout() {
        let file =
            compile_source_to_bytecode("registers.tjs", "function f(a, b) { return a + b; }")
                .expect("bytecode");
        let function_index = file
            .objects
            .iter()
            .position(|object| object.name(&file) == Some("f"))
            .expect("function object");
        let disasm = file
            .disassemble_object(function_index)
            .expect("function disassembly");

        assert!(disasm.iter().any(|line| line.contains("%-3")), "{disasm:?}");
        assert!(disasm.iter().any(|line| line.contains("%-4")), "{disasm:?}");
    }

    #[test]
    fn execute_source_runs_deep_recursive_return_values_on_vm_stack() {
        assert_eq!(
            execute_source(
                "recursive_return.tjs",
                "function sum(n) { if (n == 0) return 0; return n + sum(n - 1); } return sum(900);"
            )
            .expect("execute"),
            Variant::Integer(405450)
        );
    }

    #[test]
    fn codegen_relaxes_out_of_range_branches_with_veneers() {
        // A branch spanning more than the VM's i16-relative range must be
        // routed through jmp veneers and still execute correctly.
        let mut source = String::from("var x = 0; if (x) { return 1; }\n");
        for index in 0..4000 {
            source.push_str(&format!("x = x + {index};\n"));
        }
        source.push_str("return x;\n");
        let file = compile_source_to_bytecode("big_function.tjs", &source).expect("compile");
        // The `jf` from the top to the function end crosses the whole body
        // (~36K words), forcing veneer insertion; the executed result must
        // match direct compilation semantics.
        let result = crate::runtime::Runtime::new()
            .execute_file(&file)
            .expect("execute");
        // x is 0, so the if is skipped and all 4000 additions
        // run: sum(0..4000) = 4000 * 3999 / 2.
        assert_eq!(result, Variant::Integer(7_998_000));
    }

    #[test]
    fn execute_source_increment_coerces_numeric_strings() {
        assert_eq!(
            execute_source("increment.tjs", "var value = '5'; ++value; return value;")
                .expect("execute"),
            Variant::Integer(6)
        );
    }

    #[test]
    fn constant_nan_condition_is_truthy_like_runtime_numeric_values() {
        assert_eq!(
            execute_source("nan-branch.tjs", "if (NaN) return 1; else return 2;").expect("execute"),
            Variant::Integer(1)
        );
    }

    #[test]
    fn execute_source_array_delete_shifts_later_elements() {
        assert_eq!(
            execute_source(
                "array_delete.tjs",
                "var a = [1, 2, 3]; delete a[1]; return a.count + ':' + a.join(',');"
            )
            .expect("execute"),
            Variant::String("2:1,3".to_string())
        );
    }

    #[test]
    fn execute_source_builds_and_indexes_array() {
        assert_eq!(
            execute_source("inline.tjs", "var a = [1, 4, 9]; return a[1];").expect("execute"),
            Variant::Integer(4)
        );
    }

    /// A dictionary element evaluates its key: `dic_elm` is the two-expression
    /// production `expr_no_comma "," expr_no_comma` (`syntax/tjs.y:799-801`),
    /// and the `=>` spelling of that separator lexes as `T_COMMA`
    /// (`tjsLex.cpp:1367`), so `%[ tag => name ]` keys the entry by the
    /// *value* of `tag` -- `tjsInterCodeGen.cpp:2461-2472` compiles both
    /// nodes and emits `VM_SPIS %object.%name, %value`.  PARQUET's
    /// `LangNameBrackets` builds its `"【${pad}${name}${pad}】"` replacement
    /// dictionary exactly that way (`tag = "name"`); reading it as the
    /// literal name left the nameplate template's `${name}` unbound.
    #[test]
    fn dictionary_bare_identifier_key_is_evaluated() {
        assert_eq!(
            execute_source(
                "dict_key.tjs",
                r#"
                var tag = "name";
                var name = "ABC";
                var d = %[ tag => name, pad: " " ];
                return d["name"] + "|" + d["pad"] + "|" + (d["tag"] === void);
                "#,
            )
            .expect("execute"),
            Variant::String("ABC| |1".to_string())
        );
    }

    /// The literal forms keep their names: the colon production wraps its
    /// symbol in a constant string node (`syntax/tjs.y:800-803`, which the
    /// parser rewrites into `ExprKind::String`), and a string key evaluates to
    /// itself.
    #[test]
    fn dictionary_literal_keys_stay_literal() {
        assert_eq!(
            execute_source(
                "dict_key.tjs",
                r#"var d = %[ "name" => "LIT", pad: "P", kind: 2 ];
                   return d["name"] + "|" + d["pad"] + "|" + d["kind"];"#,
            )
            .expect("execute"),
            Variant::String("LIT|P|2".to_string())
        );
    }

    /// An integer-valued key is evaluated too, and lands under the decimal
    /// spelling `PropSetByNum` gives it (`tjsObject.cpp:167-180`) -- the way
    /// the games key their tables by `VK_F1`-style constants.
    #[test]
    fn dictionary_constant_valued_key_is_evaluated() {
        assert_eq!(
            execute_source(
                "dict_key.tjs",
                r#"var vk = 112; var d = %[ vk => "F1" ]; return d[112] + "|" + d["112"];"#,
            )
            .expect("execute"),
            Variant::String("F1|F1".to_string())
        );
    }

    /// A key expression that evaluates to void reaches `spis` with a void
    /// member, and `SetPropertyIndirect` hands the store to `PropSetByVS` with
    /// a NULL member name (`tjsInterCodeExec.cpp:1771`), which answers
    /// `TJS_E_INVALIDTYPE` (`tjsObject.cpp:1577-1581`; the read side does the
    /// same at `:1405-1408`) -- so the dictionary construction raises instead
    /// of storing a void-named entry.
    #[test]
    fn dictionary_void_key_raises_invalid_type() {
        let error = execute_source(
            "dict_key.tjs",
            "var empty; var d = %[ empty => 1 ]; return d;",
        )
        .expect_err("void key should fail");
        assert_eq!(error.kind, TjsErrorKind::InvalidType);
        assert_eq!(
            error.message,
            "Not a function or invalid method/property type"
        );
    }

    #[test]
    fn execute_source_erases_array_element() {
        assert_eq!(
            execute_source(
                "inline.tjs",
                "var a = [1, 4, 9]; a.erase(1); return a.join(',');"
            )
            .expect("execute"),
            Variant::String("1,9".to_string())
        );
        assert_eq!(
            execute_source(
                "inline.tjs",
                "var a = [1, 4, 9]; a.erase(-1); return a.join(',');"
            )
            .expect("execute"),
            Variant::String("1,4".to_string())
        );
    }

    #[test]
    fn array_remove_matches_krkr2_discern_compare_and_count() {
        assert_eq!(
            execute_source(
                "array_remove.tjs",
                r#"
                var a = [1, "1", 1, 2];
                var first = a.remove(1, false);
                var rest = a.remove(1);
                return first + ":" + rest + ":" + a.join(",");
                "#,
            )
            .expect("execute"),
            Variant::String("1:1:1,2".to_string())
        );
    }

    #[test]
    fn array_sort_covers_krkr2_builtin_orders() {
        assert_eq!(
            execute_source(
                "array_sort.tjs",
                r#"
                var text = ["b", "a", "c"]; text.sort("a");
                var nums = [3, 10, 2]; nums.sort("9");
                return text.join("") + ":" + nums.join(",");
                "#,
            )
            .expect("execute"),
            Variant::String("abc:10,3,2".to_string())
        );
    }

    #[test]
    fn array_sort_accepts_a_script_comparison_function() {
        assert_eq!(
            execute_source(
                "array_sort_function.tjs",
                r#"
                var values = [[2, "second"], [1, "first"], [2, "third"]];
                values.sort(function(a, b) { return a[0] < b[0]; }, true);
                return values[0][1] + ":" + values[1][1] + ":" + values[2][1];
                "#,
            )
            .expect("execute"),
            Variant::String("first:second:third".to_string())
        );
    }

    #[test]
    fn dictionary_assign_copies_data_without_builtin_members() {
        // The full reference matrix lives in `runtime::dictionary_tests`; this
        // keeps the copy itself pinned where the source-level behavior was
        // first noticed: the class surface is not part of a copy, and the copy
        // is reached through the class method (`Dictionary.assign`), because a
        // Dictionary instance has no members of its own to call.
        assert_eq!(
            execute_source(
                "dictionary_assign.tjs",
                r#"
                var source = new Dictionary();
                source.answer = 42;
                var dest = new Dictionary();
                (Dictionary.assign incontextof dest)(source, 1);
                return typeof dest.answer + ":" + typeof dest.assign + ":" + dest.answer;
                "#,
            )
            .expect("execute"),
            Variant::String("Integer:undefined:42".to_string())
        );
    }

    #[test]
    fn execute_source_assigns_sparse_array_index() {
        assert_eq!(
            execute_source("inline.tjs", "var a = []; a[30] = false; return a.count;")
                .expect("execute"),
            Variant::Integer(31)
        );
        assert_eq!(
            execute_source(
                "inline.tjs",
                "var sf = %[]; sf.album_flag = []; sf.album_flag[30] = false; return sf.album_flag.count;"
            )
            .expect("execute"),
            Variant::Integer(31)
        );
    }

    #[test]
    fn execute_source_array_count_assignment_resizes_elements() {
        assert_eq!(
            execute_source(
                "array_count_set.tjs",
                "var a = [1, 2, 3]; a.count = 1; a[1] = 9; a.length = 4; return a.count + ':' + a.join(',');"
            )
            .expect("execute"),
            Variant::String("4:1,9,,".to_string())
        );
    }

    #[test]
    fn string_and_array_split_use_krkr2_delimiter_semantics() {
        assert_eq!(
            execute_source(
                "split.tjs",
                r#"return "a/b//c".split("/", void, false).join("|");"#
            )
            .expect("execute"),
            Variant::String("a|b||c".to_string())
        );
        assert_eq!(
            execute_source(
                "split.tjs",
                r#"return "a/b//c".split("/", void, true).join("|");"#
            )
            .expect("execute"),
            Variant::String("a|b|c".to_string())
        );
        assert_eq!(
            execute_source(
                "split.tjs",
                r#"var a = [].split("(), ", "x(12, y)", void, true); return a.join("|");"#
            )
            .expect("execute"),
            Variant::String("x|12|y".to_string())
        );
    }

    #[test]
    fn string_and_array_split_support_regexp_patterns() {
        assert_eq!(
            execute_source(
                "split_regexp.tjs",
                r#"
                var re = new RegExp();
                re._compile("///[\\t ]+");
                var direct = "TitleCaption\t\tGINKA".split(re, void, true);
                var viaArray = [].split(re, "a  b\tc", void, true);
                return direct.join("|") + ":" + viaArray.join("|");
                "#
            )
            .expect("execute"),
            Variant::String("TitleCaption|GINKA:a|b|c".to_string())
        );
        assert_eq!(
            execute_source(
                "split_regexp_flags.tjs",
                r#"
                var re = new RegExp();
                re._compile("//i/x+");
                return "axXbx".split(re, void, true).join("|");
                "#
            )
            .expect("execute"),
            Variant::String("a|b".to_string())
        );
        assert_eq!(
            execute_source(
                "split_regexp_keep_empty.tjs",
                r#"
                var re = new RegExp();
                re._compile("///,");
                return "a,,b".split(re).join("|");
                "#
            )
            .expect("execute"),
            Variant::String("a||b".to_string())
        );
    }

    #[test]
    fn regexp_internal_compile_rejects_malformed_literal() {
        let error = execute_source(
            "split_regexp_bad.tjs",
            r#"var re = new RegExp(); re._compile("not-a-literal");"#,
        )
        .expect_err("malformed literal should fail");
        assert!(error.message.contains("RegExp._compile"));
    }

    #[test]
    fn class_static_array_member_keeps_identity_in_constructor_calls() {
        assert_eq!(
            execute_source(
                "static_array.tjs",
                r#"
                class Base { function Base() {} }
                class Derived extends Base {
                    function Derived() { global.Derived.instances.add(this); }
                }
                Derived.instances = new Array();
                var a = new Derived();
                return Derived.instances.count + ":" + (Derived.instances[0] === a);
                "#
            )
            .expect("execute"),
            Variant::String("1:1".to_string())
        );
    }

    #[test]
    fn multiple_inheritance_explicit_parent_ctor_runs_on_instance() {
        assert_eq!(
            execute_source(
                "mi_ctor.tjs",
                r#"
                class A {
                    function A() { initialized = 1; }
                    var initialized;
                }
                class B {
                    function B() { bCount = 1; }
                    var bCount;
                }
                class C extends A, B {
                    function C() {
                        this.PA.A();
                        this.PB.B();
                    }
                    var PA = global.A;
                    var PB = global.B;
                }
                var c = new C();
                return c.initialized + ":" + c.bCount;
                "#
            )
            .expect("execute"),
            Variant::String("1:1".to_string())
        );
    }

    #[test]
    fn class_qualified_call_finds_secondary_extender_method() {
        assert_eq!(
            execute_source(
                "mi_qualified_call.tjs",
                r#"
                class Pool {}
                class Action {
                    function stopAllActions() { return 42; }
                }
                class Base extends Pool, Action {}
                class Child extends Base {
                    function stop() { return global.Base.stopAllActions(); }
                }
                return (new Child()).stop();
                "#,
            )
            .expect("execute"),
            Variant::Integer(42)
        );
    }

    #[test]
    fn primary_class_method_prefers_first_script_extender() {
        let mut runtime = Runtime::new();
        let file = compile_source_to_bytecode(
            "mi_primary_event.tjs",
            r#"
                class A { function onPaint() { global.trace = "A"; } }
                class B { function onPaint() { global.trace = "B"; } }
                class C extends A, B {}
                var c = new C();
            "#,
        )
        .expect("bytecode");
        runtime.execute_file(&file).expect("execute");
        let c = match runtime.global_member("c") {
            Variant::Object(handle) => handle,
            Variant::Closure(closure) => closure.object,
            value => panic!("expected C instance, got {value:?}"),
        };
        assert!(
            runtime
                .call_primary_class_method(c, "onPaint", Vec::new())
                .expect("primary class method")
        );
        assert_eq!(
            runtime.global_member("trace"),
            Variant::String("A".to_string())
        );
    }

    /// Host that keeps the lines a traced native call writes.
    #[derive(Default)]
    struct RecordingHost {
        logs: Vec<String>,
    }

    impl crate::runtime::TjsHost for RecordingHost {
        fn log(&mut self, message: &str) {
            self.logs.push(message.to_string());
        }
    }

    #[test]
    fn native_call_traces_match_by_class_and_method() {
        let mut runtime = Runtime::with_host(RecordingHost::default());
        let class = runtime.alloc_ordinary_object();
        runtime.add_object_class_info(class, "Layer");
        runtime.register_object_native(
            class,
            "copyRect",
            |_: &mut Runtime<RecordingHost>, _, _| Ok(Variant::Integer(1)),
        );
        runtime.register_object_native(class, "update", |_: &mut Runtime<RecordingHost>, _, _| {
            Ok(Variant::Void)
        });
        runtime.set_global_member("layer", Variant::Object(class));

        let file = compile_source_to_bytecode(
            "native_trace.tjs",
            "layer.copyRect(0, 0, 1397, 2227); layer.update();",
        )
        .expect("bytecode");

        // Nothing is armed, so nothing is logged.
        runtime.execute_file(&file).expect("execute");
        assert!(runtime.host().logs.is_empty());

        // The bare method name addresses it on whatever class it lives on.
        runtime.set_native_call_traces(["copyrect"]);
        runtime.execute_file(&file).expect("execute");
        assert_eq!(runtime.host().logs.len(), 1);
        assert!(
            runtime.host().logs[0].starts_with("native call Layer.copyRect this=Layer#"),
            "unexpected trace line {:?}",
            runtime.host().logs[0]
        );
        assert!(runtime.host().logs[0].ends_with("args=[0, 0, 1397, 2227]"));

        // A class prefix arms every method of that class.
        runtime.set_native_call_traces(["Layer."]);
        runtime.host_mut().logs.clear();
        runtime.execute_file(&file).expect("execute");
        assert_eq!(runtime.host().logs.len(), 2);

        assert!(
            runtime
                .traceable_native_names()
                .contains(&"Layer.copyRect".to_string())
        );
    }

    #[test]
    fn string_methods_cover_krkr2_char_trim_reverse_repeat() {
        assert_eq!(
            execute_source(
                "string_methods.tjs",
                r#"return "abcd".charAt(2) + ":" + "abcd".charAt(9);"#
            )
            .expect("execute"),
            Variant::String("c:".to_string())
        );
        assert_eq!(
            execute_source(
                "string_methods.tjs",
                r#"return " \tname\r\n".trim() + ":" + "abc".reverse() + ":" + "ab".repeat(3);"#
            )
            .expect("execute"),
            Variant::String("name:cba:ababab".to_string())
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
    fn string_escape_escapes_tjs_string_literal_fragments() {
        assert_eq!(
            execute_source("inline.tjs", r#"return "voice\\line'\"01\n".escape();"#)
                .expect("execute"),
            Variant::String("voice\\\\line\\x27\\\"01\\n".to_string())
        );
    }

    #[test]
    fn string_sprintf_formats_common_krkr_patterns() {
        assert_eq!(
            execute_source(
                "inline.tjs",
                r#"return "%04d/%02d/%02d %02d:%02d".sprintf(2026, 5, 3, 9, 4);"#
            )
            .expect("execute"),
            Variant::String("2026/05/03 09:04".to_string())
        );
        assert_eq!(
            execute_source("inline.tjs", r#"return "%4d%s".sprintf(75, "%");"#).expect("execute"),
            Variant::String("  75%".to_string())
        );
        assert_eq!(
            execute_source("inline.tjs", r#"return "%-5s:%+04d:%%".sprintf("ok", 7);"#)
                .expect("execute"),
            Variant::String("ok   :+007:%".to_string())
        );
    }

    #[test]
    fn bare_method_calls_dispatch_through_receiver() {
        let result = execute_source(
            "inline.tjs",
            r#"
            class Base {
                function callHook() { return hook(); }
                function hook() { return "base"; }
            }
            class Child extends Base {
                function hook() { return "child"; }
            }
            var child = new Child();
            return child.callHook();
            "#,
        )
        .expect("execute");
        assert_eq!(result, Variant::String("child".to_string()));
    }

    #[test]
    fn captured_base_method_uses_instance_for_bare_calls() {
        let result = execute_source(
            "inline.tjs",
            r#"
            class Base {
                function Base() { global.callback = timerCallback; }
                function timerCallback() { return onTag(); }
                function onTag() { return "base"; }
            }
            class Child extends Base {
                function Child() { super.Base(); }
                function onTag() { return "child"; }
            }
            var child = new Child();
            return global.callback();
            "#,
        )
        .expect("execute");
        assert_eq!(result, Variant::String("child".to_string()));
    }

    #[test]
    fn function_values_match_function_instance_class() {
        assert_eq!(
            execute_source(
                "inline.tjs",
                r#"
                function f() { return 1; }
                var g = function() { return 2; };
                return (f instanceof "Function") + ":" + (g instanceof "Function");
                "#,
            )
            .expect("execute"),
            Variant::String("1:1".to_string())
        );
    }

    #[test]
    fn regexp_match_returns_krkr2_result_array_shape() {
        assert_eq!(
            execute_source(
                "inline.tjs",
                r#"
                var matched = RegExp("^Windows [^\\s]+ (\\d+\\.\\d+)", "i").match("Windows NT 10.0");
                var missed = RegExp("^Windows").match("Darwin");
                return matched.count + ":" + matched[0] + ":" + matched[1] + ":" + missed.count;
                "#,
            )
            .expect("execute"),
            Variant::String("2:Windows NT 10.0:10.0:0".to_string())
        );
    }

    #[test]
    fn regexp_values_report_regexp_class_identity() {
        assert_eq!(
            execute_source(
                "regexp_instanceof.tjs",
                r#"
                var literal = /^title_/;
                var constructed = new RegExp("^title_");
                return (literal instanceof "RegExp") + ":" +
                    (constructed instanceof "RegExp") + ":" +
                    literal.match("title_image").count;
                "#,
            )
            .expect("execute"),
            Variant::String("1:1:1".to_string())
        );
    }

    #[test]
    fn runtime_errors_from_nested_calls_are_catchable() {
        assert_eq!(
            execute_source(
                "catch_runtime.tjs",
                r#"
                function inner() { var v; v.remove(1); }
                function outer() { inner(); }
                try { outer(); return "uncaught"; } catch (e) { return "caught:" + (typeof e.message != "undefined"); }
                "#
            )
            .expect("execute"),
            Variant::String("caught:1".to_string())
        );
        assert_eq!(
            execute_source(
                "catch_native.tjs",
                r#"
                try { var a = new Array(); a.load("definitely/missing/file.txt"); return "uncaught"; }
                catch (e) { return "caught"; }
                "#
            )
            .expect("execute"),
            Variant::String("caught".to_string())
        );
    }

    #[test]
    fn runtime_errors_include_stack_and_member_context() {
        // A member the receiver does not have is the reference's
        // `TJS_E_MEMBERNOTFOUND` miss, name included (`FuncCall` keeps the
        // miss even for a Dictionary, `tjsDictionary.cpp:713-722`).
        let error = execute_source(
            "debug.tjs",
            "function run() {\n  var d = new Dictionary();\n  d.missing();\n}\nrun();",
        )
        .expect_err("missing member call should fail");
        let text = error.to_string();
        assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
        assert_eq!(error.message, "Member \"missing\" does not exist");
        assert!(text.contains("debug.tjs:"), "{text}");
        assert!(text.contains("run [Function] bytecode"), "{text}");
        assert!(text.contains("global [TopLevel] bytecode"), "{text}");
        assert!(text.contains("(debug.tjs:"), "{text}");
        assert!(text.contains("calling member `missing`"), "{text}");

        // A member that exists but holds void reaches
        // `TJSDefaultFuncCall`'s `TJS_E_INVALIDTYPE` branch
        // (`tjsObject.cpp:1280-1312`), which is where the callee type comes
        // from.  The key is written as a string literal because a bare
        // identifier would be *evaluated* (`dic_elm`, `syntax/tjs.y:799`)
        // and `missing` is not a variable.
        let error = execute_source(
            "debug.tjs",
            "function run() {\n  var d = %[\"missing\" => void];\n  d.missing();\n}\nrun();",
        )
        .expect_err("void callee should fail");
        let text = error.to_string();
        assert_eq!(error.kind, TjsErrorKind::InvalidType);
        assert!(text.contains("calling member `missing`"), "{text}");
        assert!(text.contains("callee void"), "{text}");
    }

    #[test]
    fn recursive_script_calls_fail_before_rust_stack_overflow() {
        let error = execute_source("recursive.tjs", "function f() { return f(); }\nf();")
            .expect_err("recursive script should hit the VM call guard");
        let text = error.to_string();
        assert!(text.contains("TJS call stack exceeded"), "{text}");
        assert!(text.contains("more context entries omitted"), "{text}");
    }

    #[test]
    fn super_member_call_dispatches_to_base_class() {
        assert_eq!(
            execute_source(
                "super.tjs",
                r#"
                    class Base {
                        function f() { return "base"; }
                    }
                    class Child extends Base {
                        function f() { return super.f(); }
                    }
                    var c = new Child();
                    return c.f();
                "#
            )
            .expect("execute"),
            Variant::String("base".to_string())
        );
    }

    #[test]
    fn super_constructor_initializes_base_class_members() {
        assert_eq!(
            execute_source(
                "super_ctor.tjs",
                r#"
                    class Base {
                        var value = 42;
                        function Base() {}
                    }
                    class Child extends Base {
                        function Child() { super.Base(); }
                        function getValue() { return value; }
                    }
                    var c = new Child();
                    return c.getValue();
                "#
            )
            .expect("execute"),
            Variant::Integer(42)
        );
    }

    #[test]
    fn class_extender_initializes_base_body_before_constructor_once() {
        assert_eq!(
            execute_source(
                "class_extender_body.tjs",
                r#"
                    class Base {
                        var value = 40;
                        function Base() { value += 2; }
                        function getValue() { return value; }
                    }
                    class Child extends Base {
                        function Child() { super.Base(); }
                    }
                    var c = new Child();
                    return c.getValue();
                "#
            )
            .expect("execute"),
            Variant::Integer(42)
        );
    }

    #[test]
    fn repeated_instances_do_not_pollute_class_super_chain() {
        assert_eq!(
            execute_source(
                "class_super_chain.tjs",
                r#"
                    class Root {
                        function rootValue() { return "root"; }
                    }
                    class Middle extends Root {
                        function Middle() { }
                    }
                    class Leaf extends Middle {
                        function Leaf() { super.Middle(); }
                    }
                    var first = new Leaf();
                    var second = new Leaf();
                    return typeof Leaf.rootValue + ":" + second.rootValue() + ":" +
                        (Leaf instanceof "Root") + ":" + (second instanceof "Root");
                "#
            )
            .expect("execute"),
            Variant::String("Object:root:1:1".to_string())
        );
    }

    #[test]
    fn backslash_operator_performs_integer_division() {
        assert_eq!(
            execute_source(
                "idiv.tjs",
                r#"var x = 690; x \= 3; return x + ":" + (10 \ 4);"#
            )
            .expect("execute"),
            Variant::String("230:2".to_string())
        );
    }

    #[test]
    fn invalidate_operator_notifies_runtime_host() {
        let file = compile_source_to_bytecode(
            "invalidate_host.tjs",
            r#"
            var object = %[];
            invalidate object;
            return 1;
            "#,
        )
        .expect("bytecode");
        let mut runtime = Runtime::with_host(InvalidateTrackingHost::default());

        assert_eq!(
            runtime.execute_file(&file).expect("execute"),
            Variant::Integer(1)
        );
        assert_eq!(runtime.host().invalidated.len(), 1);
    }

    #[test]
    fn super_native_constructor_and_method_bind_leaf_instance() {
        let mut runtime = Runtime::new();
        install_native_base(&mut runtime);
        let file = compile_source_to_bytecode(
            "native_super.tjs",
            r#"
            class Middle extends NativeBase {
                function Middle() { super.NativeBase(); }
                function checkNative() { return super.nativeValue(); }
            }
            class Child extends Middle {
                function Child() { super.Middle(); }
            }
            var c = new Child();
            return c.checkNative() + ":" + (typeof c.initialized != "undefined");
            "#,
        )
        .expect("bytecode");

        assert_eq!(
            runtime.execute_file(&file).expect("execute"),
            Variant::String("7:1".to_string())
        );
    }

    #[test]
    fn native_base_class_remains_visible_through_script_chain() {
        let mut runtime = Runtime::new();
        install_native_base(&mut runtime);
        let file = compile_source_to_bytecode(
            "native_chain.tjs",
            r#"
            class Middle extends NativeBase {
                function Middle() { super.NativeBase(); }
            }
            class Child extends Middle {
                function Child() { super.Middle(); }
            }
            var first = new Child();
            var second = new Child();
            return typeof Child.nativeValue + ":" + second.nativeValue() + ":" +
                (Child instanceof "NativeBase") + ":" + (second instanceof "NativeBase");
            "#,
        )
        .expect("bytecode");

        assert_eq!(
            runtime.execute_file(&file).expect("execute"),
            Variant::String("Object:7:1:1".to_string())
        );
    }

    #[test]
    fn super_native_instance_member_access_uses_bound_instance() {
        let mut runtime = Runtime::new();
        install_native_base(&mut runtime);
        let file = compile_source_to_bytecode(
            "native_super_member.tjs",
            r#"
            class Child extends NativeBase {
                function Child() { super.NativeBase(); }
                function setNativeSlot(value) { super.nativeSlot = value; }
                function getNativeSlot() { return super.nativeSlot; }
            }
            var c = new Child();
            c.setNativeSlot(23);
            return c.nativeSlot + ":" + c.getNativeSlot() + ":" + typeof global.nativeSlot;
            "#,
        )
        .expect("bytecode");

        assert_eq!(
            runtime.execute_file(&file).expect("execute"),
            Variant::String("23:23:undefined".to_string())
        );
    }

    #[test]
    fn returned_super_expression_keeps_current_objthis() {
        // `answer` is a declared member: the constructor's unqualified store
        // is an `spd` through the `%-2` proxy, which finds the instance's
        // inherited member but cannot create an undeclared name.
        assert_eq!(
            execute_source(
                "super_objthis.tjs",
                r#"
                class Base {
                    var answer;
                    function value() { return answer; }
                }
                class Child extends Base {
                    function Child() { answer = 42; }
                    function baseProxy() { return super; }
                }
                var child = new Child();
                return child.baseProxy().value();
                "#,
            )
            .expect("execute"),
            Variant::Integer(42)
        );
    }

    #[test]
    fn super_native_property_assignment_walks_inherited_super_chain() {
        let mut runtime = Runtime::new();
        install_native_base(&mut runtime);
        let file = compile_source_to_bytecode(
            "super_inherited_native_property_set.tjs",
            r#"
            class Middle extends NativeBase {
                function Middle() { super.NativeBase(); }
            }
            class Child extends Middle {
                function Child() { super.Middle(); }
                function setNativeProp(value) { super.nativeProp = value; }
                function getNativeProp() { return super.nativeProp; }
            }
            var c = new Child();
            c.setNativeProp(41);
            return c.getNativeProp() + ":" + c.nativePropValue + ":" +
                typeof global.nativeProp;
            "#,
        )
        .expect("bytecode");

        assert_eq!(
            runtime.execute_file(&file).expect("execute"),
            Variant::String("41:41:undefined".to_string())
        );
    }

    #[test]
    fn super_property_get_skips_overriding_instance_property_before_superclass() {
        let mut runtime = Runtime::new();
        install_native_base(&mut runtime);
        let file = compile_source_to_bytecode(
            "super_overridden_property_get.tjs",
            r#"
            class Middle extends NativeBase {
                function Middle() { super.NativeBase(); }
            }
            class Child extends Middle {
                function Child() { super.Middle(); }
                property nativeProp {
                    getter { return super.nativeProp; }
                    setter(value) { super.nativeProp = value; }
                }
                function scaled(value) { return -value * nativeProp; }
            }
            var c = new Child();
            c.nativeProp = 7;
            return c.scaled(2);
            "#,
        )
        .expect("bytecode");

        assert_eq!(
            runtime.execute_file(&file).expect("execute"),
            Variant::Integer(-14)
        );
    }

    #[test]
    fn super_finalize_uses_declaring_class_super_chain() {
        let mut runtime = Runtime::new();
        install_native_base(&mut runtime);
        let file = compile_source_to_bytecode(
            "native_super_finalize.tjs",
            r#"
            global.trace = "";
            class Middle extends NativeBase {
                function Middle() { super.NativeBase(); }
                function finalize() {
                    global.trace += "M";
                    super.finalize(...);
                }
            }
            class Child extends Middle {
                function Child() { super.Middle(); }
                function finalize() {
                    global.trace += "C";
                    super.finalize(...);
                }
            }
            var c = new Child();
            invalidate c;
            return global.trace + ":" + (isvalid c);
            "#,
        )
        .expect("bytecode");

        assert_eq!(
            runtime.execute_file(&file).expect("execute"),
            Variant::String("CMN:0".to_string())
        );
    }

    #[test]
    fn class_qualified_typeof_does_not_fall_back_to_instance_override() {
        assert_eq!(
            execute_source(
                "class_qualified_typeof.tjs",
                r#"
                class Base {}
                class Child extends Base {
                    function finalize() {}
                    function probe() { return typeof Base.finalize; }
                }
                return (new Child()).probe();
                "#
            )
            .expect("execute"),
            Variant::String("undefined".to_string())
        );
    }

    #[test]
    fn native_class_object_method_call_uses_current_instance_this() {
        let mut runtime = Runtime::new();
        install_native_base(&mut runtime);
        let file = compile_source_to_bytecode(
            "native_class_object_call.tjs",
            r#"
            class KAGBuffer extends NativeBase {
                var sbclass;
                function KAGBuffer() {
                    super.NativeBase();
                    sbclass = global.NativeBase;
                }
                function callStoredBaseMethod() {
                    return sbclass.nativeValue();
                }
            }
            var buffer = new KAGBuffer();
            return buffer.callStoredBaseMethod();
            "#,
        )
        .expect("bytecode");

        assert_eq!(
            runtime.execute_file(&file).expect("execute"),
            Variant::Integer(7)
        );
    }

    #[test]
    fn new_native_constructor_does_not_reuse_caller_this() {
        let mut runtime = Runtime::new();
        install_native_base(&mut runtime);
        let file = compile_source_to_bytecode(
            "native_new.tjs",
            r#"
            class Maker {
                function make() { return new NativeBase(); }
            }
            var maker = new Maker();
            var created = maker.make();
            return (typeof maker.initialized == "undefined") + ":" + created.nativeValue();
            "#,
        )
        .expect("bytecode");

        assert_eq!(
            runtime.execute_file(&file).expect("execute"),
            Variant::String("1:7".to_string())
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

    fn install_native_base(runtime: &mut Runtime) {
        let constructor = runtime.alloc_native_constructor(
            |runtime: &mut Runtime,
             this_obj: Option<ObjectHandle>,
             _args: Vec<Variant>|
             -> Result<Variant> {
                let handle = this_obj
                    .filter(|handle| *handle != runtime.global_handle())
                    .unwrap_or_else(|| runtime.alloc_ordinary_object());
                runtime.add_object_class_info(handle, "NativeBase");
                runtime.set_object_member(handle, "initialized", Variant::Integer(1));
                runtime.set_object_member(handle, "nativeSlot", Variant::Integer(0));
                runtime.register_object_native(handle, "nativeValue", native_base_value);
                if matches!(runtime.object_member(handle, "finalize"), Variant::Void) {
                    runtime.register_object_native(handle, "finalize", native_base_finalize);
                }
                Ok(Variant::Object(handle))
            },
        );
        runtime.add_object_class_info(constructor, "NativeBase");
        runtime.register_object_native(constructor, "nativeValue", native_base_value);
        runtime.register_object_native(constructor, "finalize", native_base_finalize);
        let native_prop = runtime.alloc_native_property(native_base_prop_get, native_base_prop_set);
        runtime.set_object_member(constructor, "nativeProp", Variant::Object(native_prop));
        runtime.set_global_member("NativeBase", Variant::Object(constructor));
    }

    fn native_base_value(
        runtime: &mut Runtime,
        this_obj: Option<ObjectHandle>,
        _args: Vec<Variant>,
    ) -> Result<Variant> {
        let handle =
            this_obj.ok_or_else(|| TjsError::runtime("NativeBase.nativeValue requires this"))?;
        Ok(match runtime.object_member(handle, "initialized") {
            Variant::Void => Variant::Integer(0),
            _ => Variant::Integer(7),
        })
    }

    fn native_base_finalize(
        runtime: &mut Runtime,
        _this_obj: Option<ObjectHandle>,
        _args: Vec<Variant>,
    ) -> Result<Variant> {
        let trace = runtime.global_member("trace").to_tjs_string()?;
        runtime.set_global_member("trace", Variant::String(format!("{trace}N")));
        Ok(Variant::Void)
    }

    fn native_base_prop_get(
        runtime: &mut Runtime,
        this_obj: Option<ObjectHandle>,
    ) -> Result<Variant> {
        let handle =
            this_obj.ok_or_else(|| TjsError::runtime("NativeBase.nativeProp requires this"))?;
        Ok(runtime.object_member(handle, "nativePropValue"))
    }

    fn native_base_prop_set(
        runtime: &mut Runtime,
        this_obj: Option<ObjectHandle>,
        value: Variant,
    ) -> Result<()> {
        let handle =
            this_obj.ok_or_else(|| TjsError::runtime("NativeBase.nativeProp requires this"))?;
        runtime.set_object_member(handle, "nativePropValue", value);
        Ok(())
    }

    #[test]
    fn execute_source_runs_property_getter_and_setter_calls() {
        assert_eq!(
            execute_source(
                "property.tjs",
                r#"
                    class C {
                        var stored = 0;
                        property value {
                            getter { return stored + 1; }
                            setter(v) { stored = v * 2; }
                        }
                    }
                    var c = new C();
                    c.value = 5;
                    return c.value;
                "#
            )
            .expect("execute"),
            Variant::Integer(11)
        );
    }

    #[test]
    fn class_property_identifier_assignment_uses_setter() {
        assert_eq!(
            execute_source(
                "class_property_identifier_set.tjs",
                r#"
                    class C {
                        var stored = 0;
                        function setValue(v) { value = v; }
                        property value {
                            getter { return stored; }
                            setter(v) { stored = v * 3; }
                        }
                    }
                    var c = new C();
                    c.setValue(7);
                    return c.value + ":" + c.stored;
                "#
            )
            .expect("execute"),
            Variant::String("21:21".to_string())
        );
    }

    #[test]
    fn inherited_property_identifier_assignment_uses_setter() {
        assert_eq!(
            execute_source(
                "inherited_property_identifier_set.tjs",
                r#"
                    class Base {
                        var stored = 0;
                        property value {
                            getter { return stored; }
                            setter(v) { stored = v * 3; }
                        }
                    }
                    class Sub extends Base {
                        function Sub() { value = 7; }
                        function setViaMethod(v) { value = v; }
                    }
                    var sub = new Sub();
                    sub.setViaMethod(9);
                    sub.value = 4;
                    return sub.value + ":" + sub.stored;
                "#
            )
            .expect("execute"),
            Variant::String("12:12".to_string())
        );
    }

    #[test]
    fn nested_class_reusing_base_class_name_calls_the_base_constructor() {
        // KAGEX specialises a system class by re-declaring it inside a derived
        // dialog class under the same name. `super.Render()` must reach the
        // global base constructor even though regmember has already installed
        // the derived one on the instance.
        //
        // `trace` is declared: the unqualified stores below are `VM_SPD`
        // through the `%-2` proxy, which does not create members -- an
        // undeclared name raises `Member "trace" does not exist` there, in
        // this engine and in the reference alike.
        assert_eq!(
            execute_source(
                "nested_class_shadow.tjs",
                r#"
                    class Render {
                        var trace;
                        function Render() { trace = "base"; }
                        function tag() { return "base:" + trace; }
                    }
                    class Outer {
                        function Outer() { }
                        class Render extends Render {
                            var trace;
                            function Render() { super.Render(); }
                            function tag() { return "inner:" + trace; }
                        }
                        function make() { return new this.Render(); }
                    }
                    var made = (new Outer()).make();
                    return (new global.Render()).tag() + "/" + made.tag();
                "#
            )
            .expect("execute"),
            Variant::String("base:base/inner:base".to_string())
        );
    }

    #[test]
    fn class_regmember_copies_child_methods_to_instance() {
        assert_eq!(
            execute_source(
                "class_regmember_methods.tjs",
                r#"
                    class U {
                        function delayLoadFunction(x) { return makeDelay(x); }
                        function makeDelay(x) { return x + "!"; }
                    }
                    var u = new U();
                    return u.delayLoadFunction("ok");
                "#
            )
            .expect("execute"),
            Variant::String("ok!".to_string())
        );
    }

    #[test]
    fn eval_operator_executes_expression_source() {
        assert_eq!(
            execute_source(
                "eval_expression.tjs",
                r#"global.value = 40; return "value + 2"!;"#
            )
            .expect("execute"),
            Variant::Integer(42)
        );
    }

    #[test]
    fn eval_operator_preserves_escaped_interpolated_string_delimiters() {
        assert_eq!(
            execute_source(
                "eval_interpolated_string.tjs",
                r#"return ("@'\\\"Let\\x27s go\\\"'")!;"#,
            )
            .expect("execute"),
            Variant::String("\"Let's go\"".to_string())
        );
    }

    #[test]
    fn eexp_operator_executes_statement_source_in_current_this() {
        let file = compile_source_to_bytecode(
            "eexp_statement.tjs",
            r#"
                function evalit(source) { source!; }
                var d = new Dictionary();
                (evalit incontextof d)("property answer { getter { return 42; } }");
                return d.answer;
            "#,
        )
        .expect("compile");
        // tjsInterCodeGen picks VM_EEXP (87) over VM_EVAL (86) for a `!` whose
        // result is discarded, and EvalExpression then compiles the string as a
        // statement list rather than `return <expr>;`.
        let emitted_eexp = file.objects.iter().any(|object| {
            object
                .decode_instructions()
                .expect("decode")
                .iter()
                .any(|inst| inst.opcode == 87)
        });
        assert!(emitted_eexp, "expected a discarded eval to compile to eexp");
        assert_eq!(
            Runtime::new().execute_file(&file).expect("execute"),
            Variant::Integer(42)
        );
    }

    #[test]
    fn chgthis_accepts_null_objthis_like_krkr2() {
        assert_eq!(
            execute_source(
                "chgthis_null.tjs",
                r#"
                    function f() { return 1; }
                    var c = f incontextof null;
                    return c();
                "#
            )
            .expect("execute"),
            Variant::Integer(1)
        );
    }

    #[test]
    fn class_members_are_registered_before_field_initializers_run() {
        // The official compiler emits `regmember` right after the extenders
        // (FunctionRegisterCodePoint), so a field initialiser may call a
        // method, and an initialiser sharing a method's name wins.
        assert_eq!(
            execute_source(
                "field_init.tjs",
                r#"
                    class Foo {
                        var x = tag();
                        var tag2 = 5;
                        function tag() { return 7; }
                        function tag2() { return 1; }
                    }
                    var f = new Foo();
                    return f.x + ":" + f.tag2;
                "#
            )
            .expect("execute"),
            Variant::String("7:5".to_string())
        );
    }

    #[test]
    fn thrown_object_keeps_its_identity_across_call_frames() {
        // A `throw` in a callee must hand the very same object to the
        // caller's catch (krkrz rethrows the tTJSVariant); wrapping it in a
        // fresh Exception broke `instanceof` and custom members.
        assert_eq!(
            execute_source(
                "cross_throw.tjs",
                r#"
                    class E extends Exception {
                        var code = 42;
                        function E(m) { super.Exception(m); }
                    }
                    function f() { throw new E("x"); }
                    function g() { try { f(); } catch (e) { throw e; } }
                    var r = "";
                    try { g(); } catch (e) {
                        r = (e instanceof "E") + ":" + e.message + ":" + e.code;
                    }
                    // A VM failure still arrives as an Exception object.
                    try { f2(); } catch (e) { r += ":" + (e instanceof "Exception"); }
                    return r;
                "#
            )
            .expect("execute"),
            Variant::String("1:x:42:1".to_string())
        );
    }

    #[test]
    fn regmember_stores_through_the_destination_missing_hook() {
        // mixinclass.tjs probes a class by running its body against an
        // object whose `missing` throws on every store and swallows reads.
        // RegisterObjectMember goes through PropSet in krkrz, so the first
        // member copy hits the hook and aborts the body before any field
        // initialiser runs; the thrown object must reach the probe's catch.
        let mut runtime = Runtime::new();
        runtime.register_global_native(
            "setCallMissing",
            |runtime: &mut Runtime, _this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
                let handle = match args.first() {
                    Some(Variant::Object(handle)) => *handle,
                    Some(Variant::Closure(closure)) => closure.object,
                    _ => return Err(TjsError::runtime("setCallMissing requires object")),
                };
                runtime.set_object_call_missing(handle, "missing");
                Ok(Variant::Void)
            },
        );
        let file = compile_source_to_bytecode(
            "mixin_probe.tjs",
            r#"
                var initialised = 0;
                class WorkerException extends Exception {
                    function WorkerException(msg) { super.Exception(msg); }
                }
                class Worker {
                    var __get; var __err;
                    function Worker(get, err) {
                        __get = get; __err = err;
                        setCallMissing(this);
                    }
                    function missing(set, name, value) {
                        if (set) throw new __err(name);
                        *value = __get;
                        return true;
                    }
                }
                function Module() {}
                class Foo extends Module {
                    var flags = ++initialised;
                    function tag() {}
                }
                var w = new Worker(function {}, WorkerException);
                var caught = "";
                try { (Foo incontextof w)(); } catch (e) {
                    if (!(e instanceof "WorkerException")) throw e;
                    caught = e.message;
                }
                return caught + ":" + initialised;
            "#,
        )
        .expect("bytecode");
        assert_eq!(
            runtime.execute_file(&file).expect("execute"),
            Variant::String("tag:0".to_string())
        );
    }

    #[test]
    fn nested_declarations_are_members_of_their_function_or_class() {
        // tTJSInterCodeContext::RegisterFunction publishes a declaration on a
        // function or class parent through the `properties` table; KAGEX
        // reaches helpers as `BuildMixinClass.__work__` and nested classes as
        // `_.classNamesWorkerException`.
        assert_eq!(
            execute_source(
                "nested_members.tjs",
                r#"
                    function f() {
                        function g() { return 1; }
                        class K { function K() { } function v() { return 2; } }
                        property p { getter { return 3; } }
                        return 0;
                    }
                    class Outer {
                        function Outer() {}
                        class Inner { function Inner() {} function v() { return 4; } }
                    }
                    var inner = new Outer.Inner();
                    var inner2 = new (new Outer()).Inner();
                    return f.g() + ":" + (new f.K()).v() + ":" + f.p + ":" + inner.v()
                        + ":" + inner2.v();
                "#
            )
            .expect("execute"),
            Variant::String("1:2:3:4:4".to_string())
        );
    }

    #[test]
    fn class_object_members_resolve_through_every_extender() {
        // tTJSInterCodeContext::PropGet/PropSet on a class object fall back
        // along all superclass getter entries (TJS_DO_SUPERCLASS_PROXY), and
        // a write to an inherited member lands on the class that owns it.
        assert_eq!(
            execute_source(
                "class_chain.tjs",
                r#"
                    class A { function A() {} function fromA() { return "a"; } }
                    class B { function B() {} function fromB() { return "b"; } }
                    class C extends A, B { function C() {} }
                    A.count = 1;
                    C.count = 2;
                    var r = C.fromA() + C.fromB() + ":" + A.count + ":" + C.count;
                    C.own = 3;
                    r += ":" + typeof A.own + ":" + C.own;
                    return r;
                "#
            )
            .expect("execute"),
            Variant::String("ab:2:2:undefined:3".to_string())
        );
    }

    #[test]
    fn set_call_missing_routes_absent_gets_and_sets_through_missing() {
        let mut runtime = Runtime::new();
        runtime.register_global_native(
            "setCallMissing",
            |runtime: &mut Runtime, _this_obj: Option<ObjectHandle>, args: Vec<Variant>| {
                let handle = match args.first() {
                    Some(Variant::Object(handle)) => *handle,
                    Some(Variant::Closure(closure)) => closure.object,
                    Some(other) => {
                        return Err(TjsError::runtime(format!(
                            "setCallMissing requires object, got {}",
                            other.type_name()
                        )));
                    }
                    None => return Err(TjsError::runtime("setCallMissing requires object")),
                };
                runtime.set_object_call_missing(handle, "missing");
                Ok(Variant::Void)
            },
        );
        let file = compile_source_to_bytecode(
            "missing_proxy.tjs",
            r#"
                class StaticSetterProxy {
                    var target;
                    function StaticSetterProxy(target) {
                        this.target = target;
                        setCallMissing(this);
                    }
                    function missing(set, name, value) {
                        if (set) {
                            target[name] = *value;
                        } else {
                            *value = target[name];
                        }
                        return true;
                    }
                }
                var target = %["existing" => 2];
                var proxy = new StaticSetterProxy(target);
                proxy.answer = 40 + 2;
                target.answer += 1;
                return target.answer + ":" + proxy.answer + ":" + proxy.existing;
            "#,
        )
        .expect("bytecode");

        assert_eq!(
            runtime.execute_file(&file).expect("execute"),
            Variant::String("43:43:2".to_string())
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
    fn class_definitions_are_instances_of_class_but_not_function() {
        assert_eq!(
            execute_source(
                "class_instanceof.tjs",
                r#"
                class C { }
                return (C instanceof "Class") + ":" + (C instanceof "Function");
                "#,
            )
            .expect("execute"),
            Variant::String("1:0".to_string())
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
    fn star_of_call_is_assignable_as_default_property() {
        assert_eq!(
            execute_source(
                "inline.tjs",
                r#"
                var o = %[];
                function prop(name) { return o; }
                (*prop("skipSpeed")) = 9;
                return *o;
                "#
            )
            .expect("execute"),
            Variant::Integer(9)
        );
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

    #[test]
    fn continue_inside_try_stays_in_loop() {
        let result = execute_source(
            "inline.tjs",
            r#"
            function f() {
                var n = 0;
                try {
                    for (;;) {
                        n++;
                        if (n < 3) continue;
                        break;
                    }
                } catch (e) {
                    return -1;
                }
                return n;
            }
            return f();
            "#,
        )
        .expect("execute");
        assert_eq!(result, Variant::Integer(3));
    }

    #[test]
    fn unqualified_identifier_store_compiles_to_plain_spd() {
        // `tjsInterCodeGen.cpp:1909-1921` picks the store opcode from the
        // assignment target's object node: the this-proxy takes `VM_SPD`
        // (flags 0), a real receiver takes `VM_SPDE` (`MEMBERENSURE`), and
        // `&`/declaration stores take `VM_SPDS`
        // (`MEMBERENSURE|TJS_IGNOREPROP`, `:2699-2703`).  The flags are what
        // makes the proxy fall through to the global for a miss
        // (`tTJSObjectProxy::PropSet`, `tjsInterCodeExec.cpp:318-320`), so an
        // assignment inside a method must not carry either flag: the
        // declaration `var c = 0` is the only `spds` in this fixture, and the
        // assignment in `f` was a second one before the fix.
        let file = compile_source_to_bytecode(
            "inline.tjs",
            "var c = 0;\nfunction f() { c = c + 1; }\nf();\nreturn c;",
        )
        .expect("bytecode");
        let top_level = file.top_level.expect("top-level");
        let lines_of = |index: usize| file.disassemble_object(index).expect("disassemble");
        // The store inside `f` is `spd %-2.*N` -- flags 0 on the this-proxy --
        // and carries neither `MEMBERENSURE` (`spde`) nor
        // `MEMBERENSURE|TJS_IGNOREPROP` (`spds`); before the fix it was a
        // second `spds` there.
        let body: Vec<String> = (0..file.objects.len())
            .filter(|index| *index != top_level)
            .flat_map(&lines_of)
            .collect();
        assert!(
            body.iter().any(|line| line.contains("spd %-2.")),
            "{body:#?}"
        );
        assert!(
            !body
                .iter()
                .any(|line| line.contains("spds") || line.contains("spde")),
            "{body:#?}"
        );
        // The fixture really does exercise a statically bound global: the
        // top-level `var c = 0` is still an ensured, ignore-prop store.
        let top = lines_of(top_level);
        assert!(
            top.iter().any(|line| line.contains("spds %-2.")),
            "{top:#?}"
        );
    }

    fn disassemble_top_level(source: &str) -> Vec<String> {
        let file = compile_source_to_bytecode("inline.tjs", source).expect("bytecode");
        file.disassemble_object(file.top_level.expect("top-level"))
            .expect("disassemble")
    }
}
