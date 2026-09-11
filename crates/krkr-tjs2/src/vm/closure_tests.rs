//! Closure, `this`-binding and frame-slot semantics, pinned against the
//! reference implementation.
//!
//! TJS2 has **no lexical capture of locals**. The official compiler resolves
//! an identifier through the *current* code context's local namespace
//! (`tTJSInterCodeGen.cpp:2037`, `GenNodeCode` `case T_SYMBOL`) and, when the
//! name is not a local of that context, rewrites the access as a member read
//! on the `T_THIS_PROXY` node (`tTJSInterCodeGen.cpp:2149-2166`) -- the
//! `gpd %1, %-2.*N` form the game's KAGEX handlers are compiled to. Nested
//! contexts get their own namespace (`tTJSInterCodeContext::Namespace`;
//! `tTJSScriptBlock::PushContextStack` constructs a fresh context), so an
//! enclosing function's register can never resolve from an inner function:
//! the name instead reaches the `%-2` object proxy the VM builds in
//! `ExecuteAsFunction` (`tjsInterCodeExec.cpp:789-806`), i.e. objthis first,
//! then the global object (`tTJSObjectProxy::PropGet`,
//! `tjsInterCodeExec.cpp:284`).
//!
//! The one thing a closure does capture is `this`: the compiler emits
//! `chgthis` for `incontextof` and for functions registered on a context or
//! instance (`tjsInterCodeGen.cpp:1207`, `:3483`), and the VM prefers that
//! bound ObjThis over the call site's receiver (`CallFunctionDirect`,
//! `tjsInterCodeExec.cpp:2434-2438`, through `TJS_SELECT_OBJTHIS`).
//!
//! Both halves are exercised here through the engine's own script path, and
//! once more with checked-in official `TJS2100` bytecode produced by the byte
//! builder at the bottom of this file, so an accidental JavaScript-style
//! capture -- in either direction -- cannot land unnoticed.

use crate::bytecode::{BytecodeFile, DataSlot, DataSlotType};
use crate::compiler::compile_source_to_bytecode;
use crate::error::{Result, TjsError, TjsErrorKind};
use crate::runtime::{Runtime, Variant};

fn run(source: &str) -> Result<Variant> {
    let file = compile_source_to_bytecode("closure-test.tjs", source).expect("compile");
    Runtime::new().execute_file(&file)
}

fn ok(source: &str) -> Variant {
    run(source).unwrap_or_else(|error| panic!("script failed: {error}"))
}

fn failure(source: &str) -> TjsError {
    match run(source) {
        Ok(value) => panic!("script unexpectedly succeeded with {value:?}"),
        Err(error) => error,
    }
}

/// `function outer() { var q = 5; return function() { return q; }; }` cannot
/// see `q`: the inner function compiles it as `%-2.q`, and neither the
/// enclosing frame's registers nor the global object carry it.
#[test]
fn enclosing_local_is_not_visible_to_a_nested_function() {
    let error = failure(
        r#"
        function outer() { var q = 5; return function() { return q; }; }
        return outer()();
        "#,
    );
    assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
    assert_eq!(error.message, "Member \"q\" does not exist");
}

/// The `%-2` proxy's first object is the callee's `this`, so a name that an
/// enclosing local shadows still resolves to the instance member.
#[test]
fn nested_function_reads_the_this_member_before_the_global() {
    assert_eq!(
        ok(r#"
        class Window {
            var q = 7;
            function make() { var q = 5; return function() { return q; }; }
        }
        global.window = new Window();
        return global.window.make()();
        "#),
        Variant::Integer(7)
    );
}

/// ... and its second object is the global object: the outer register is
/// shadowed by a global of the same name, and the inner read sees the global.
#[test]
fn nested_function_reads_the_global_when_this_has_no_such_member() {
    assert_eq!(
        ok(r#"
        global.q = 100;
        function outer() { var q = 5; return function() { return q; }; }
        return outer()();
        "#),
        Variant::Integer(100)
    );
}

/// A write from a nested function follows the same rule: it lands on the
/// global object, and the enclosing register keeps its own value.
#[test]
fn nested_function_write_lands_on_the_global_not_the_enclosing_register() {
    assert_eq!(
        ok(r#"
        global.q = 100;
        function outer() {
            var q = 5;
            var f = function() { q = 42; };
            f();
            return q + ":" + global.q;
        }
        return outer();
        "#),
        Variant::String("5:42".to_string())
    );
}

/// A nested `function` declaration is registered as a local variable of the
/// enclosing frame (`tTJSInterCodeContext::InitLocalFunction`,
/// `tjsInterCodeGen.cpp:951-964`), but its body still addresses enclosing
/// names through the this-proxy.
#[test]
fn nested_function_declaration_resolves_identifiers_the_same_way() {
    assert_eq!(
        ok(r#"
        global.q = 100;
        function outer() { var q = 5; function inner() { return q; } return inner(); }
        return outer();
        "#),
        Variant::Integer(100)
    );
}

/// An inner function that declares its own local wins over the global, which
/// pins that arguments and locals *of the same context* stay authoritative.
#[test]
fn inner_function_local_shadows_the_global() {
    assert_eq!(
        ok(r#"
        global.q = 100;
        function outer() {
            var q = 5;
            return function() { var q = 9; return q; };
        }
        return outer()();
        "#),
        Variant::Integer(9)
    );
}

/// `incontextof` is the language's capture: the value keeps the bound ObjThis,
/// and a later member read plus call site must prefer it over the receiver.
#[test]
fn incontextof_binding_survives_a_member_read_and_call() {
    assert_eq!(
        ok(r#"
        class Holder { var marker = "bound"; }
        var first = new Holder();
        var second = new Holder();
        second.marker = "receiver";
        second.handler = (function() { return this.marker; } incontextof first);
        var escaped = second.handler;
        return escaped();
        "#),
        Variant::String("bound".to_string())
    );
}

/// The KAGEX `getHandlers()` shape: handlers are built in a method, bound to
/// the instance with `chgthis`, stored in a dictionary, and later read out of
/// it. An unqualified global inside the handler must still resolve through
/// the `%-2` proxy's global fallback.
#[test]
fn kagex_style_handler_dictionary_resolves_globals_through_the_proxy() {
    assert_eq!(
        ok(r#"
        class KAGWindow {
            var tagHandlers = %[];
            function pick(elm) { return elm.page; }
            function getHandlers() {
                return %[
                    syspage: function(elm) { return kag.pick(elm); },
                    free: function(elm) { return this.pick(elm); }
                ];
            }
        }
        global.kag = new KAGWindow();
        global.kag.tagHandlers = global.kag.getHandlers();
        return global.kag.tagHandlers.syspage(%[free => 1, page => "back"])
             + ":" + global.kag.tagHandlers.free(%[page => "back"]);
        "#),
        Variant::String("back:back".to_string())
    );
}

/// A closure in a loop cannot keep the loop variable either: the argument of
/// the per-iteration helper is a local register of that helper, so the reader
/// still addresses `%-2.value`.
#[test]
fn per_iteration_argument_is_not_visible_to_the_returned_closure() {
    let error = failure(
        r#"
        var readers = [];
        for (var i = 0; i < 3; i += 1) {
            readers.add((function(value) { return function() { return value; }; }(i)));
        }
        return readers[0]();
        "#,
    );
    assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
    assert_eq!(error.message, "Member \"value\" does not exist");
}

/// The reference idiom for per-iteration state is to materialise it on an
/// object, because that object is what the returned closure's `this` reaches.
#[test]
fn per_iteration_state_travels_on_an_object() {
    assert_eq!(
        ok(r#"
        var readers = [];
        for (var i = 0; i < 3; i += 1) {
            readers.add((function(value) {
                var holder = %[ get: function() { return this.value; }, value: value ];
                return holder;
            }(i * 10)));
        }
        return readers[0].get() + ":" + readers[1].get() + ":" + readers[2].get();
        "#),
        Variant::String("0:10:20".to_string())
    );
}

/// Official bytecode fixtures. The builder writes the exact binary layout the
/// loader accepts (`TJS2100\0`, `DATA`, `OBJS`), with the register operands
/// stored as indexes and converted at load time
/// (`tTJSByteCodeLoader::TranslateCodeAddress`, `tjsByteCodeLoader.cpp:358`).
mod official {
    use super::*;

    #[derive(Default)]
    struct Fixture {
        strings: Vec<String>,
        integers: Vec<i32>,
        objects: Vec<FixtureObject>,
    }

    struct FixtureObject {
        parent: i32,
        name: i16,
        context: i32,
        variables: u32,
        reserve: u32,
        frames: u32,
        args: u32,
        code: Vec<i16>,
        data: Vec<DataSlot>,
    }

    impl FixtureObject {
        fn new(name: i16, context: i32) -> Self {
            Self {
                parent: -1,
                name,
                context,
                variables: 0,
                reserve: 2,
                frames: 2,
                args: 0,
                code: Vec::new(),
                data: Vec::new(),
            }
        }

        fn variables(mut self, variables: u32) -> Self {
            self.variables = variables;
            self
        }

        fn frames(mut self, frames: u32) -> Self {
            self.frames = frames;
            self
        }

        fn code(mut self, words: &[i16]) -> Self {
            self.code = words.to_vec();
            self
        }

        fn data(mut self, data: Vec<DataSlot>) -> Self {
            self.data = data;
            self
        }
    }

    impl Fixture {
        fn string(&mut self, value: &str) -> i16 {
            self.strings.push(value.to_string());
            (self.strings.len() - 1) as i16
        }

        fn integer(&mut self, value: i32) -> i16 {
            self.integers.push(value);
            (self.integers.len() - 1) as i16
        }

        fn object(&mut self, object: FixtureObject) -> i16 {
            self.objects.push(object);
            (self.objects.len() - 1) as i16
        }

        fn parse(self, top_level: i16) -> BytecodeFile {
            let bytes = self.finish(top_level);
            BytecodeFile::parse(&bytes).expect("fixture parses and verifies")
        }

        fn finish(self, top_level: i16) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(&crate::bytecode::BYTECODE_SIGNATURE);
            out.extend_from_slice(&[0; 4]);

            out.extend_from_slice(b"DATA");
            let data_size = out.len();
            out.extend_from_slice(&[0; 4]);
            let data_start = out.len();
            put_i32(&mut out, 0);
            align_4(&mut out);
            put_i32(&mut out, 0);
            align_4(&mut out);
            put_i32(&mut out, self.integers.len() as i32);
            for value in &self.integers {
                put_i32(&mut out, *value);
            }
            put_i32(&mut out, 0);
            put_i32(&mut out, 0);
            put_i32(&mut out, self.strings.len() as i32);
            for value in &self.strings {
                let units: Vec<u16> = value.encode_utf16().collect();
                put_i32(&mut out, units.len() as i32);
                for unit in &units {
                    out.extend_from_slice(&unit.to_le_bytes());
                }
                if units.len() % 2 == 1 {
                    out.extend_from_slice(&0_u16.to_le_bytes());
                }
            }
            put_i32(&mut out, 0);
            let data_end = out.len();
            patch_i32(&mut out, data_size, (data_end - data_start + 8) as i32);

            out.extend_from_slice(b"OBJS");
            let objs_size = out.len();
            out.extend_from_slice(&[0; 4]);
            let objs_start = out.len();
            put_i32(&mut out, i32::from(top_level));
            put_i32(&mut out, self.objects.len() as i32);
            for object in &self.objects {
                write_object(&mut out, object);
            }
            let objs_end = out.len();
            patch_i32(&mut out, objs_size, (objs_end - objs_start + 8) as i32);
            let total = out.len() as i32;
            patch_i32(&mut out, 8, total);
            out
        }
    }

    fn write_object(out: &mut Vec<u8>, object: &FixtureObject) {
        out.extend_from_slice(b"TJS2");
        let size = out.len();
        out.extend_from_slice(&[0; 4]);
        let start = out.len();
        put_i32(out, object.parent);
        put_i32(out, i32::from(object.name));
        put_i32(out, object.context);
        put_i32(out, object.variables as i32);
        put_i32(out, object.reserve as i32);
        put_i32(out, object.frames as i32);
        put_i32(out, object.args as i32);
        put_i32(out, 0);
        put_i32(out, -1);
        put_i32(out, -1);
        put_i32(out, -1);
        put_i32(out, -1);
        put_i32(out, 0);
        put_i32(out, object.code.len() as i32);
        for word in &object.code {
            out.extend_from_slice(&word.to_le_bytes());
        }
        if object.code.len() % 2 == 1 {
            out.extend_from_slice(&0_i16.to_le_bytes());
        }
        put_i32(out, object.data.len() as i32);
        for slot in &object.data {
            let ty = match slot.ty {
                DataSlotType::InterObject => 2_i16,
                DataSlotType::String => 3,
                DataSlotType::Integer => 8,
                other => panic!("fixture builder does not encode {other:?}"),
            };
            out.extend_from_slice(&ty.to_le_bytes());
            out.extend_from_slice(&slot.index.to_le_bytes());
        }
        put_i32(out, 0);
        put_i32(out, 0);
        let end = out.len();
        patch_i32(out, size, (end - start) as i32);
    }

    fn put_i32(out: &mut Vec<u8>, value: i32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn patch_i32(out: &mut [u8], at: usize, value: i32) {
        out[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn align_4(out: &mut Vec<u8>) {
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
    }

    const GLOBAL: i32 = 0;
    const FUNCTION: i32 = 1;
    const EXPR_FUNCTION: i32 = 2;

    /// The game's `syspage` shape, in official bytecode: a handler created in
    /// one context, `chgthis`-bound to the instance, stored under a dictionary
    /// key, and called through the instance's member; its body reads an
    /// unqualified name (`kag`) that only the global object carries, so the
    /// `%-2` proxy must walk from the bound `this` to the global.
    #[test]
    fn official_handler_reads_the_global_through_the_bound_this_proxy() {
        let file = official_handler_fixture("Array");
        let mut runtime = Runtime::new();
        let result = runtime.execute_file(&file).expect("execute fixture");
        let Variant::Object(kag) = runtime.global_member("kag") else {
            panic!("global kag was not created");
        };
        assert_eq!(result, Variant::Object(kag));
    }

    /// The same fixture with a Dictionary as the bound `this`. A Dictionary
    /// answers `void` for a missing member instead of `TJS_E_MEMBERNOTFOUND`
    /// unless `TJS_MEMBERMUSTEXIST` is set (`tjsDictionary.cpp:727-731`), which
    /// a script `gpd` never sets (`tjsInterCodeExec.cpp:1338`), so the proxy
    /// stops at the first object and the global is never consulted
    /// (`tTJSObjectProxy::PropGet`, `tjsInterCodeExec.cpp:284`).
    #[test]
    fn official_dictionary_this_stops_the_proxy_before_the_global() {
        let file = official_handler_fixture("Dictionary");
        let mut runtime = Runtime::new();
        assert_eq!(
            runtime.execute_file(&file).expect("execute fixture"),
            Variant::Void
        );
        assert!(matches!(
            runtime.global_member("kag"),
            Variant::Object(_) | Variant::Closure(_)
        ));
    }

    /// `constructor` is a global class the fixture instantiates as the
    /// `this` the handler is bound to.
    fn official_handler_fixture(constructor: &str) -> BytecodeFile {
        let mut fixture = Fixture::default();
        let global_name = fixture.string("global");
        let constructor = fixture.string(constructor);
        let kag_name = fixture.string("kag");
        let syspage = fixture.string("syspage");
        let anonymous = fixture.string("(anonymous)");
        let argument = fixture.string("hit");

        let handler = fixture.object(
            FixtureObject::new(anonymous, EXPR_FUNCTION)
                .frames(2)
                .code(&[
                    103, 1, -2, 0, // gpd %1, %-2.*0   // *0 = "kag"
                    118, 1,   // srv %1
                    119, // ret
                ])
                .data(vec![DataSlot {
                    ty: DataSlotType::String,
                    index: kag_name,
                }]),
        );
        let top_level = fixture.object(
            FixtureObject::new(global_name, GLOBAL)
                .frames(8)
                .code(&[
                    124, 1, // global %1
                    103, 2, 1, 0, // gpd %2, %1.*0        // *0 = the constructor
                    102, 1, 2, 0, // new %1, %2()
                    124, 3, // global %3
                    105, 3, 1, 1, // spde %3.*1, %1       // *1 = "kag"
                    1, 2, 2, // const %2, *2             // *2 = "syspage"
                    1, 3, 3, // const %3, *3             // *3 = handler object
                    123, 3, 1, // chgthis %3, %1
                    113, 1, 2, 3, // spis %1[%2], %3
                    1, 4, 4, // const %4, *4             // *4 = "hit"
                    100, 5, 1, 2, 1, 4, // calld %5, %1.*2(%4)
                    118, 5,   // srv %5
                    119, // ret
                ])
                .data(vec![
                    DataSlot {
                        ty: DataSlotType::String,
                        index: constructor,
                    },
                    DataSlot {
                        ty: DataSlotType::String,
                        index: kag_name,
                    },
                    DataSlot {
                        ty: DataSlotType::String,
                        index: syspage,
                    },
                    DataSlot {
                        ty: DataSlotType::InterObject,
                        index: handler,
                    },
                    DataSlot {
                        ty: DataSlotType::String,
                        index: argument,
                    },
                ]),
        );
        fixture.parse(top_level)
    }

    /// The counter-fixture for capture: `outer` stores 5 in its own local
    /// register and returns a nested function that reads `q`. Official
    /// bytecode has no way to address that register from the inner context, so
    /// the read must resolve against the `%-2` proxy instead -- the global
    /// here -- and the enclosing 5 stays invisible.
    #[test]
    fn official_nested_function_reads_the_global_not_the_enclosing_register() {
        let file = official_capture_fixture(true);
        let mut runtime = Runtime::new();
        assert_eq!(
            runtime.execute_file(&file).expect("execute fixture"),
            Variant::Integer(100)
        );
    }

    /// The same fixture without the global: the enclosing register is present
    /// and holds 5, and the read still fails with the official message.
    #[test]
    fn official_nested_function_cannot_read_the_enclosing_register() {
        let file = official_capture_fixture(false);
        let mut runtime = Runtime::new();
        let error = runtime
            .execute_file(&file)
            .expect_err("the enclosing local is not reachable");
        assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
        assert_eq!(error.message, "Member \"q\" does not exist");
    }

    /// `outer` writes 5 to `%-3` (its first local, the layout
    /// `ExecuteAsFunction` gives a `FuncDeclArgCount == 0` context) and
    /// returns the nested reader; the top level stores it in `global.probe`
    /// and calls it. With `with_global` the top level also sets `global.q`.
    fn official_capture_fixture(with_global: bool) -> BytecodeFile {
        let mut fixture = Fixture::default();
        let global_name = fixture.string("global");
        let q = fixture.string("q");
        let five = fixture.integer(5);
        let hundred = fixture.integer(100);
        let outer_name = fixture.string("outer");
        let anonymous = fixture.string("(anonymous)");
        let probe = fixture.string("probe");

        let reader = fixture.object(
            FixtureObject::new(anonymous, EXPR_FUNCTION)
                .frames(2)
                .code(&[
                    103, 1, -2, 0, // gpd %1, %-2.*0   // *0 = "q"
                    118, 1,   // srv %1
                    119, // ret
                ])
                .data(vec![DataSlot {
                    ty: DataSlotType::String,
                    index: q,
                }]),
        );
        let outer = fixture.object(
            FixtureObject::new(outer_name, FUNCTION)
                .variables(1)
                .frames(4)
                .code(&[
                    1, 1, 0, // const %1, *0           // *0 = 5
                    2, -3, 1, // cp %-3, %1           // outer's local q
                    1, 1, 1, // const %1, *1           // *1 = the reader object
                    118, 1,   // srv %1
                    119, // ret
                ])
                .data(vec![
                    DataSlot {
                        ty: DataSlotType::Integer,
                        index: five,
                    },
                    DataSlot {
                        ty: DataSlotType::InterObject,
                        index: reader,
                    },
                ]),
        );

        let mut code = vec![
            124, 1, // global %1
        ];
        if with_global {
            code.extend_from_slice(&[
                1, 2, 3, // const %2, *3           // *3 = 100
                111, 1, 0, 2, // spds %1.*0, %2        // *0 = "q"
            ]);
        }
        code.extend_from_slice(&[
            1, 2, 4, // const %2, *4           // *4 = outer object
            99, 3, 2, 0, // call %3, %2()
            111, 1, 5, 3, // spds %1.*5, %3        // *5 = "probe"
            100, 4, 1, 5, 0, // calld %4, %1.*5()
            118, 4,   // srv %4
            119, // ret
        ]);
        let top_level = fixture.object(
            FixtureObject::new(global_name, GLOBAL)
                .frames(6)
                .code(&code)
                .data(vec![
                    DataSlot {
                        ty: DataSlotType::String,
                        index: q,
                    },
                    DataSlot {
                        ty: DataSlotType::InterObject,
                        index: outer,
                    },
                    DataSlot {
                        ty: DataSlotType::InterObject,
                        index: reader,
                    },
                    DataSlot {
                        ty: DataSlotType::Integer,
                        index: hundred,
                    },
                    DataSlot {
                        ty: DataSlotType::InterObject,
                        index: outer,
                    },
                    DataSlot {
                        ty: DataSlotType::String,
                        index: probe,
                    },
                ]),
        );
        fixture.parse(top_level)
    }
}
