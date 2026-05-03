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
        });
    }
    output
        .value
        .ok_or_else(|| TjsError::parse(crate::Span::empty(0), "frontend produced no value"))
}

#[cfg(test)]
mod tests {
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
    fn execute_source_builds_and_indexes_array() {
        assert_eq!(
            execute_source("inline.tjs", "var a = [1, 4, 9]; return a[1];").expect("execute"),
            Variant::Integer(4)
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
            execute_source("inline.tjs", r#"return "voice\\line\"01\n".escape();"#)
                .expect("execute"),
            Variant::String("voice\\\\line\\\"01\\n".to_string())
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
    fn runtime_errors_include_stack_and_member_context() {
        let error = execute_source(
            "debug.tjs",
            "function run() {\n  var d = new Dictionary();\n  d.missing();\n}\nrun();",
        )
        .expect_err("missing member call should fail");
        let text = error.to_string();
        assert!(text.contains("debug.tjs:"), "{text}");
        assert!(text.contains("run [Function] bytecode"), "{text}");
        assert!(text.contains("global [TopLevel] bytecode"), "{text}");
        assert!(text.contains("(debug.tjs:"), "{text}");
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

    fn disassemble_top_level(source: &str) -> Vec<String> {
        let file = compile_source_to_bytecode("inline.tjs", source).expect("bytecode");
        file.disassemble_object(file.top_level.expect("top-level"))
            .expect("disassemble")
    }
}
