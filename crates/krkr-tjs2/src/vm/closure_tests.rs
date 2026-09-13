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

use std::sync::Arc;

use super::Vm;
use crate::bytecode::{BytecodeContextType, BytecodeFile, DataSlot, DataSlotType};
use crate::compiler::compile_source_to_bytecode;
use crate::error::{Result, TjsError, TjsErrorKind};
use crate::runtime::{NoHost, ObjectHandle, Runtime, Variant};

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

/// One receiver family per fixture.  Every unqualified read below compiles to
/// a member access on `%-2` (`tjsInterCodeGen.cpp:2149-2166`), which
/// `ExecuteAsFunction` fills with a two-object dispatch: the frame's `this`
/// first, the global object second (`tjsInterCodeExec.cpp:789-806`).  The
/// second object is reached only when the first answers
/// `TJS_E_MEMBERNOTFOUND` (`tTJSObjectProxy::PropGet`,
/// `tjsInterCodeExec.cpp:284`), so each fixture puts the reader on a receiver
/// family the game's own KAGEX handlers run under and reads a name only the
/// global carries.  A fallback that stops working for one family cannot hide
/// behind another.
#[test]
fn bare_object_receiver_reads_the_global() {
    assert_eq!(
        ok(r#"
        global.q = 100;
        class Bare {}
        global.bare = new Bare();
        global.bare.reader = function() { return q; };
        return global.bare.reader();
        "#),
        Variant::Integer(100)
    );
}

/// The receiver is an instance whose *class* carries the member surface, the
/// `KAGWindow` shape: the instance's own map is empty and every method lives
/// on the class, so the proxy's primary walk has to cross the class chain and
/// still fall back for the name nobody has.
#[test]
fn class_instance_with_a_carrier_class_reads_the_global() {
    assert_eq!(
        ok(r#"
        global.q = 100;
        class Carrier { var marker = 1; function pick() { return 2; } }
        global.carrier = new Carrier();
        global.carrier.reader = function() { return q; };
        return global.carrier.reader();
        "#),
        Variant::Integer(100)
    );
}

/// Native-backed chains are the live shape: in `KAGWindow` the class objects
/// are native classes and only the instance's data members are script-side, so
/// the miss has to be recognised after the native chain answers nothing.
/// `Runtime::register_object_native` + `set_object_super_class` build exactly
/// that (see the `WaveSoundBuffer` shape in `vm/mod.rs`).
#[test]
fn native_backed_class_chain_receiver_reads_the_global() {
    let file = compile_source_to_bytecode(
        "proxy-native-chain.tjs",
        r#"
        global.q = 100;
        global.reader = function() { return q; };
        return (global.reader incontextof global.window)();
        "#,
    )
    .expect("compile");

    let mut runtime = Runtime::new();
    let class = runtime.alloc_ordinary_object();
    runtime.register_object_native(
        class,
        "play",
        |_runtime: &mut Runtime<NoHost>, _this: Option<ObjectHandle>, _args: Vec<Variant>| {
            Ok(Variant::Void)
        },
    );
    let instance = runtime.alloc_ordinary_object();
    runtime.set_object_super_class(instance, class);
    runtime.add_object_class_info(instance, "KAGWindow".to_string());
    runtime.set_object_member(instance, "marker", Variant::Integer(1));
    runtime.set_global_member("window", Variant::Object(instance));

    assert_eq!(
        runtime.execute_file(&file).expect("execute"),
        Variant::Integer(100)
    );
}

/// An Array receiver: its own miss rule is the plain `TJS_E_MEMBERNOTFOUND`
/// (`tTJSArrayObject` inherits `tTJSCustomObject::PropGet`), so the fallback
/// applies exactly like it does for a plain object.
#[test]
fn array_receiver_reads_the_global() {
    assert_eq!(
        ok(r#"
        global.q = 100;
        var arr = [];
        arr.reader = function() { return q; };
        return arr.reader();
        "#),
        Variant::Integer(100)
    );
}

/// The reader fetched off one object and called from another context: the
/// fetch itself must not consume the read's fallback, and the call's own
/// `this` (a second instance without the name) must reach the global too.
#[test]
fn detached_member_fetch_still_reads_the_global() {
    assert_eq!(
        ok(r#"
        global.q = 100;
        class Holder {}
        global.holder = new Holder();
        global.holder.reader = function() { return q; };
        global.caller = function() {
            var borrowed = global.holder.reader;
            return borrowed();
        };
        var other = new Holder();
        other.caller = global.caller;
        return other.caller();
        "#),
        Variant::Integer(100)
    );
}

/// `incontextof` is the language's explicit `chgthis`: the bound ObjThis
/// becomes the callee's `this` (`CallFunctionDirect`, `tjsInterCodeExec.cpp
/// :2434-2438`), so the `%-2` proxy is built from it and the global fallback
/// has to work from there as well.
#[test]
fn incontextof_receiver_reads_the_global() {
    assert_eq!(
        ok(r#"
        global.q = 100;
        class Holder {}
        var holder = new Holder();
        var reader = (function() { return q; } incontextof holder);
        return reader();
        "#),
        Variant::Integer(100)
    );
}

/// An unqualified *call* is a member call on the same proxy, and the
/// reference's `tTJSObjectProxy::FuncCall` (`tjsInterCodeExec.cpp:262-275`)
/// looks the member up on the receiver first and calls it on the global
/// object with `OBJ2` -- the caller's own `this` -- as the receiver.
#[test]
fn unqualified_call_reads_the_global_function_with_the_callers_this() {
    assert_eq!(
        ok(r#"
        global.helper = function() { return this.marker; };
        class Holder { var marker = 7; }
        var holder = new Holder();
        holder.run = (function() { return helper(); } incontextof holder);
        return holder.run();
        "#),
        Variant::Integer(7)
    );
}

/// The name exists on the receiver: it wins, the global is never consulted.
#[test]
fn receiver_member_wins_over_the_global() {
    assert_eq!(
        ok(r#"
        global.q = 100;
        class Holder { var q = 7; }
        global.holder = new Holder();
        global.holder.reader = function() { return q; };
        return global.holder.reader();
        "#),
        Variant::Integer(7)
    );
}

/// A member that exists and holds void is a *hit*: `tTJSObjectProxy::PropGet`
/// moves on only for `TJS_E_MEMBERNOTFOUND` (`tjsInterCodeExec.cpp:284`), and
/// `tTJSCustomObject::PropGet` answers `TJS_S_OK` with the void value.  The
/// dispatcher clears its internal `probe` flag for the primary walk exactly
/// for this case.
#[test]
fn void_member_on_the_receiver_does_not_fall_back() {
    assert_eq!(
        ok(r#"
        global.q = 100;
        class Holder {}
        var holder = new Holder();
        holder.q = void;
        holder.reader = function() { return q; };
        return holder.reader() === void;
        "#),
        Variant::Integer(1)
    );
}

/// A Dictionary receiver answers void for a miss unless
/// `TJS_MEMBERMUSTEXIST` is set (`tjsDictionary.cpp:727-731`), and a script
/// `gpd` never sets it (`tjsInterCodeExec.cpp:1338`): the proxy stops at the
/// first object and the global is not consulted.
#[test]
fn dictionary_receiver_stops_the_proxy_before_the_global() {
    assert_eq!(
        ok(r#"
        global.q = 100;
        var d = %[];
        d.reader = function() { return q; };
        return d.reader() === void;
        "#),
        Variant::Integer(1)
    );
}

/// A bare *call* is a different operation from a bare read, and the official
/// compiler keeps them apart: `GenNodeCode`'s `T_LPARENTHESIS` case computes
/// `hasnonlocalsymbol` for a `T_SYMBOL` callee -- a name that is not a local
/// of the current namespace -- and then generates it with `stFuncCall`
/// (`tjsInterCodeGen.cpp:1752-1813`), which lands on `VM_CALLD` for the
/// rewritten `T_THIS_PROXY . name` node (`:2003-2022`).  `iTJSDispatch2::FuncCall`
/// has no "answer void for a miss" rule: only `tTJSDictionaryObject::PropGet`
/// maps a miss to void (`tjsDictionary.cpp:720-731`), while `FuncCall` stays
/// with `tTJSCustomObject::FuncCall` and reports `TJS_E_MEMBERNOTFOUND`
/// (`tjsObject.cpp:1316-1340`), so the `%-2` proxy walks on to the global
/// object.  A global function called with a Dictionary `this` therefore
/// resolves, even though the same name read back through `gpd` answers void
/// (`dictionary_receiver_stops_the_proxy_before_the_global`).  GINKA's
/// `title.ks` depends on this: `Scripts.eval` runs an inline-string source
/// whose `${GetBgmTitleImageFile(file)}` is a bare call, evaluated with the
/// caller's `%[file: ...]` Dictionary as the context.
#[test]
fn dictionary_this_resolves_a_bare_call_through_the_proxy() {
    assert_eq!(
        ok(r#"
        function globalFn() { return "ok"; }
        var d = %[];
        d.reader = function() { return globalFn(); };
        return d.reader();
        "#),
        Variant::String("ok".to_string())
    );
}

/// The same call shape with the name missing everywhere still reports the
/// official member error rather than the void-to-Object conversion: the
/// proxy's fallback ends at the global object, and the miss surfaces from the
/// `FuncCall` walk (`TJSThrowFrom_tjs_error(TJS_E_MEMBERNOTFOUND, name)`).
#[test]
fn dictionary_this_bare_call_miss_reports_the_member_error() {
    let error = failure(
        r#"
        var d = %[];
        d.reader = function() { return no_such_fn(); };
        return d.reader();
        "#,
    );
    assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
    assert_eq!(error.message, "Member \"no_such_fn\" does not exist");
}

/// `typeof <unqualified non-local name>` is *not* the must-exist member read:
/// `GenNodeCode`'s `case T_TYPEOF` (`tjsInterCodeGen.cpp:1691-1730`) turns the
/// child into `VM_TYPEOFD` only when the child node is already a `T_DOT` /
/// `T_LBRACKET` / `T_WITHDOT`, and a bare symbol is rewritten to
/// `T_THIS_PROXY . name` *inside* its own lowering, i.e. with an empty
/// sub-parameter (`:2149-2166`, the synthetic `nodep.SetOpecode(T_DOT)`).
/// The bare read is therefore a plain `gpd`, so the Dictionary keeps its void
/// answer, and a name nobody carries raises instead of answering
/// "undefined" -- which is why KRKR's own KAG scripts write
/// `typeof global.SystemConfig` and never `typeof SystemConfig`
/// (`sysscn/system.tjs`).
#[test]
fn typeof_bare_name_reads_like_a_plain_read() {
    assert_eq!(
        ok(r#"
        global.q = 100;
        var d = %[];
        d.t = function() { return typeof q; };
        d.v = function() { return q; };
        return d.t() + ":" + (d.v() === void);
        "#),
        Variant::String("void:1".to_string())
    );
}

/// The misses that *do* get the must-exist treatment are the explicit member
/// expressions: `VM_TYPEOFD` answers "undefined" for
/// `TJS_E_MEMBERNOTFOUND` (`tjsInterCodeExec.cpp:2127-2143`).
#[test]
fn typeof_member_expression_answers_undefined_for_a_miss() {
    assert_eq!(
        ok(r#"
        class Holder {}
        var holder = new Holder();
        holder.reader = function() { return typeof this.no_such_name; };
        return holder.reader();
        "#),
        Variant::String("undefined".to_string())
    );
}

/// A bare unqualified name is a plain read of the `%-2` proxy, so a name that
/// only the global carries reads back through the fallback and `typeof`
/// reports the value's type...
#[test]
fn typeof_bare_name_reads_the_global_through_the_proxy() {
    assert_eq!(
        ok(r#"
        global.q = 100;
        class Holder {}
        var holder = new Holder();
        var present = (function() { return typeof q; } incontextof holder);
        return present();
        "#),
        Variant::String("Integer".to_string())
    );
}

/// ... and one that nobody carries raises the official member error, exactly
/// like any other bare read, because the plain `gpd` never sets
/// `TJS_MEMBERMUSTEXIST`.
#[test]
fn typeof_bare_name_raises_when_nobody_carries_it() {
    let error = failure(
        r#"
        class Holder {}
        var holder = new Holder();
        var absent = (function() { return typeof no_such_name; } incontextof holder);
        return absent();
        "#,
    );
    assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
    assert_eq!(error.message, "Member \"no_such_name\" does not exist");
}

/// A name nobody carries: the fallback ends at the global object and reports
/// the official text (`TJSThrowFrom_tjs_error(TJS_E_MEMBERNOTFOUND, name)`,
/// `tjsError.cpp:240-244`).
#[test]
fn proxy_miss_everywhere_reports_the_official_member_error() {
    let error = failure(
        r#"
        function read() { return no_such_name; }
        return read();
        "#,
    );
    assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
    assert_eq!(error.message, "Member \"no_such_name\" does not exist");
}

// ---------------------------------------------------------------------------
// `this` at the top level and when a call carries no ObjThis of its own.
//
// `tTJSInterCodeContext::FuncCall` is where the reference splits the cases.
// A **top-level** context substitutes the global object for a NULL context
// (`objthis?objthis:Block->GetTJS()->GetGlobalNoAddRef()`,
// `tjsInterCodeExec.cpp:3083-3087`), so `Scripts.exec` / `Scripts.eval`
// without their context argument (`base/ScriptMgnIntf.cpp:1283-1341`) --
// and `TVPExecuteExpression(content, result)` (`:650-653`) -- run with `this`
// = the global object.
//
// Every *other* context kind hands its objthis straight to
// `ExecuteAsFunction` (`:3089-3099`), which stores it with
// `ra[-1].SetObject(objthis, objthis)` (`:839`) and points the `%-2`
// this-proxy register at it, or at the global object when there is none
// (`proxy.SetObjects(objthis, global)`, `:789-806`).  A NULL survives into
// such a frame only through a *direct* dispatch call (`PropGetter->FuncCall`
// at `:3135`, the setter funnel `:3172`, `SuperClassGetter->ExecuteAsFunction(NULL, ...)`
// at `:3063`), where `this` then reads as the null object.  Every
// variant/closure call -- the host shape `FuncCall(0, NULL, NULL, ...)` and
// `VM_CALL`, whose call sites pass `clo.ObjThis ? clo.ObjThis : ra[-1]`
// (`:2372`, `:2438`) as `tTJSVariantClosure::FuncCall`'s `objthis` argument
// -- substitutes the callee's own `Object` when both are NULL
// (`ObjThis?ObjThis:(objthis?objthis:Object)`, `tjsVariant.h:226-232`), so
// the callee runs on itself, self-bound.

/// Calls the value `source` returns (a function value the script hands to the
/// host) the way host code that holds a bare value calls it:
/// `FuncCall(0, NULL, NULL, result, 0, NULL, NULL)` -- the shape the
/// reference uses for `System.exceptionHandler`
/// (`base/ScriptMgnIntf.cpp:950`) and for a transition's tick callback
/// (`visual/LayerIntf.cpp:6694`).  No receiver argument is passed, so
/// `tTJSVariantClosure::FuncCall` falls back to the callee's own `Object`
/// (`tjsVariant.h:226-232`).
///
/// The value is an expression function (`var probe = function() {...}`),
/// which the compiler leaves unbound at the top level; a *declared* top-level
/// function carries the global as its ObjThis instead (`RegisterFunction`
/// queues it with `changethis`, `tjsInterCodeGen.cpp:935-947`, and `FixCode`
/// emits `chgthis %1, %-1` before the store, `:763-766`), and that binding is
/// consulted by a call site (`clo.ObjThis ? clo.ObjThis : ra[-1]`,
/// `tjsInterCodeExec.cpp:2372`).
fn call_without_this(runtime: &mut Runtime<NoHost>, source: &str) -> Variant {
    let file = compile_source_to_bytecode("closure-test.tjs", source).expect("compile");
    let probe = runtime.execute_file(&file).expect("install probe");
    runtime.call_function(probe, Vec::new()).expect("host call")
}

/// The `ctTopLevel` substitution: a script run without a context has the
/// global object as `this`, not the null object.
///
/// `this == global` holds because `NormalCompare` compares only the object
/// pointer for `tvtObject` values (`tjsVariant.cpp:693-696`, the ObjThis
/// comparison commented out).  `this === global` is *false* in the reference
/// -- `this` is stored as `tTJSVariant(dsp, dsp)` (`tjsInterCodeExec.cpp:839`)
/// while the `global` keyword loads `GetGlobalNoAddRef()` unbound
/// (`VM_GLOBAL`, `:1454-1456`), and `DiscernCompare` compares ObjThis too
/// (`tjsVariant.cpp:778-780`) -- so the identity assertion uses `==` and the
/// null check uses `===`.
#[test]
fn top_level_this_is_the_global_object() {
    assert_eq!(
        ok("return (global == this) + \":\" + (this === null);"),
        Variant::String("1:0".to_string())
    );
}

/// `this` at the top level carries the global object's members, in both
/// directions.
#[test]
fn top_level_this_reads_and_writes_the_global_object() {
    assert_eq!(
        ok("this.answer = 42; return global.answer;"),
        Variant::Integer(42)
    );
    assert_eq!(
        ok("global.answer = 7; return this.answer;"),
        Variant::Integer(7)
    );
}

/// An unqualified name at the top level compiles to a `%-2` this-proxy read
/// (`tjsInterCodeGen.cpp:2149-2166`), and the proxy's first target is the
/// global it was built for (`proxy.SetObjects(objthis, global)`,
/// `tjsInterCodeExec.cpp:794-797`), so reads and writes of an existing
/// global land on the global object.  A *new* name needs the explicit
/// receiver: an unqualified store compiles to `VM_SPD` (flags 0,
/// `tjsInterCodeGen.cpp:1909-1912`) while `global.name = ...` compiles to
/// `VM_SPDE` (MEMBERENSURE, `:1915-1917`), which is why KRKR's scripts write
/// the `global.` prefix to create a global.
#[test]
fn top_level_unqualified_names_read_and_write_the_global_object() {
    assert_eq!(
        ok("var answer = 40; answer = answer + 2; return global.answer;"),
        Variant::Integer(42)
    );
    assert_eq!(
        ok("var answer = 0; global.answer = 7; return answer;"),
        Variant::Integer(7)
    );
    let error = failure("answer = 42; return global.answer;");
    assert_eq!(error.message, "Member \"answer\" does not exist");
}

/// A function value called with no receiver at all runs on itself: both the
/// closure's ObjThis and the call's `objthis` argument are NULL, so
/// `tTJSVariantClosure::FuncCall` substitutes the closure's own `Object`
/// (`ObjThis?ObjThis:(objthis?objthis:Object)`, `tjsVariant.h:226-232`), and
/// `ExecuteAsFunction` stores it self-bound (`ra[-1].SetObject(objthis, objthis)`,
/// `tjsInterCodeExec.cpp:839`).  `Runtime::call_function` drives exactly this
/// shape -- the one `System.exceptionHandler` (`base/ScriptMgnIntf.cpp:950`)
/// and transition tick callbacks (`visual/LayerIntf.cpp:6694`) use.
#[test]
fn a_bare_function_call_runs_on_the_callee_object() {
    let mut runtime = Runtime::new();
    let value = call_without_this(
        &mut runtime,
        "var probe = function() { return (this == probe) + \":\" + (this === null); };\n\
         return probe;",
    );
    assert_eq!(value, Variant::String("1:0".to_string()));
}

/// The member a bare-function frame writes through `this` lands on the callee
/// object, not on the global and not on a null receiver: `this` is the frame's
/// self-bound `ra[-1]` (`tjsInterCodeExec.cpp:839`), and nothing in that frame
/// is a null dispatch.
#[test]
fn a_bare_function_frame_writes_through_this_onto_the_callee() {
    let mut runtime = Runtime::new();
    let file = compile_source_to_bytecode(
        "closure-test.tjs",
        "var probe = function() { this.x = 1; return this.x; };\nreturn probe;",
    )
    .expect("compile");
    let probe = runtime.execute_file(&file).expect("install probe");
    let callee = probe.object_handle().expect("function object");
    assert_eq!(
        runtime.call_function(probe, Vec::new()).expect("host call"),
        Variant::Integer(1)
    );
    assert_eq!(runtime.object_member(callee, "x"), Variant::Integer(1));
    assert_eq!(runtime.global_member("x"), Variant::Void);
}

/// Unqualified *reads* from a bare-function frame still fall through to the
/// global object: `%-2` is the this-proxy built with
/// `proxy.SetObjects(objthis, global)` (`tjsInterCodeExec.cpp:789-806`), and
/// `tTJSObjectProxy::PropGet` moves on to the second object for
/// `TJS_E_MEMBERNOTFOUND` only (`:284-299`).  (The matching unqualified
/// *store* path is a known divergence tracked separately: this engine lands
/// an existing global's unqualified store on the callee object's table where
/// the reference's proxy falls through to the global's, `:318-320` with
/// `OBJ2` = `objthis ? objthis : Dispatch2` at `:264`.)
#[test]
fn a_bare_function_frame_reads_unqualified_globals_through_the_proxy() {
    let mut runtime = Runtime::new();
    let value = call_without_this(
        &mut runtime,
        "var shared = 7;\n\
         var probe = function() { return shared; };\n\
         return probe;",
    );
    assert_eq!(value, Variant::Integer(7));
}

/// A call made *from* a bare-function frame hands the callee that frame's own
/// `this`: the nested closure's ObjThis is null, so the call site supplies
/// `ra[-1]` (`clo.ObjThis ? clo.ObjThis : ra[-1]`, `tjsInterCodeExec.cpp:2372`),
/// which this frame holds as the callee object itself (`:839`) -- the inner
/// fallback to its own `Object` (`tjsVariant.h:226-232`) never fires.
#[test]
fn a_nested_call_inherits_the_bare_frames_this() {
    let mut runtime = Runtime::new();
    let value = call_without_this(
        &mut runtime,
        "var inner = function() { return this == probe; };\n\
         var probe = function() { return inner(); };\n\
         return probe;",
    );
    assert_eq!(value, Variant::Integer(1));
}

/// The null-object rule lives at the *direct* dispatch entries the reference
/// keeps for it: an explicit `objthis = NULL` handed to
/// `tTJSInterCodeContext::FuncCall` (`PropGetter->FuncCall(..., objthis)` at
/// `tjsInterCodeExec.cpp:3135`, the setter funnel at `:3172`,
/// `SuperClassGetter->ExecuteAsFunction(NULL, ...)` at `:3063`) runs the
/// context with `ra[-1].SetObject(NULL, NULL)` (`:839`) while `%-2` is the
/// global object itself (`ra[-2].SetObject(global, global)`, `:805`).
/// Executing a function context directly with no objthis is that shape.
#[test]
fn a_direct_dispatch_with_no_objthis_reads_the_null_object() {
    let file = compile_source_to_bytecode(
        "closure-test.tjs",
        "function probe() { return (this === null) + \":\" + (global == this); }",
    )
    .expect("compile");
    let index = file
        .objects
        .iter()
        .position(|object| object.context_type == BytecodeContextType::Function)
        .expect("function context");
    let mut runtime = Runtime::new();
    let file_id = runtime.install_script_file(Arc::new(file));
    let mut vm = Vm::new(file_id, &mut runtime).expect("vm");
    assert_eq!(
        vm.execute_object_with_this(index, Vec::new(), None)
            .expect("direct dispatch"),
        Variant::String("1:0".to_string())
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
        // A `new` result is `tTJSVariant(dsp, dsp)` (`tjsInterCodeExec.cpp:2384`),
        // i.e. self-bound, so the stored global and the value the handler read
        // back through it are the same bound closure.
        let Some(kag) = runtime.global_member("kag").object_handle() else {
            panic!("global kag was not created");
        };
        assert_eq!(result, Variant::self_bound(kag));
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

    /// The game's failing lookup as pure bytecode: `syspage`'s prologue reads
    /// the unqualified global `kag` (`gpd %1, %-2.*6`,
    /// `sysscn/system.tjs` object 177 at bytecode 0x54) with `this` = a
    /// `KAGWindow`-shaped instance -- an object whose member surface lives on
    /// a native-backed class chain and which carries no `kag` of its own.  The
    /// `%-2` proxy has to walk the instance, the native class, and then the
    /// global object (`tjsInterCodeExec.cpp:789-806`, `:284`).
    #[test]
    fn official_unqualified_global_read_with_a_native_backed_this() {
        let file = official_bound_this_read_fixture();
        let mut runtime = Runtime::new();
        let instance = kag_window_shaped_instance(&mut runtime);
        let expected = runtime.alloc_ordinary_object();
        runtime.set_global_member("kag", Variant::Object(expected));

        let result = runtime
            .execute_file_with_this(&file, Some(instance))
            .expect("execute fixture");
        assert_eq!(result, Variant::Object(expected));
    }

    /// The same fixture with nothing on the global either: the read has to
    /// end at the global object and report the official miss, not the
    /// receiver's chain.
    #[test]
    fn official_unqualified_global_read_missing_everywhere_raises() {
        let file = official_bound_this_read_fixture();
        let mut runtime = Runtime::new();
        let instance = kag_window_shaped_instance(&mut runtime);

        let error = runtime
            .execute_file_with_this(&file, Some(instance))
            .expect_err("the name is on no object");
        assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
        assert_eq!(error.message, "Member \"kag\" does not exist");
    }

    /// One `gpd %1, %-2.*0` object, so the tests above only differ in what
    /// the runtime around it carries.
    fn official_bound_this_read_fixture() -> BytecodeFile {
        let mut fixture = Fixture::default();
        let global_name = fixture.string("global");
        let kag_name = fixture.string("kag");
        let top_level = fixture.object(
            FixtureObject::new(global_name, GLOBAL)
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
        fixture.parse(top_level)
    }

    /// A `KAGWindow`-shaped `this`: data members on the instance, the method
    /// surface on a native class object (`register_object_native`), and the
    /// class chain the game's own window reports.
    fn kag_window_shaped_instance(runtime: &mut Runtime<NoHost>) -> ObjectHandle {
        let class = runtime.alloc_ordinary_object();
        runtime.register_object_native(
            class,
            "getLayerFromElm",
            |_runtime: &mut Runtime<NoHost>, _this: Option<ObjectHandle>, _args: Vec<Variant>| {
                Ok(Variant::Void)
            },
        );
        let instance = runtime.alloc_ordinary_object();
        runtime.set_object_super_class(instance, class);
        runtime.add_object_class_info(instance, "KAGWindow".to_string());
        runtime.add_object_class_info(instance, "KAGWindowBase".to_string());
        runtime.add_object_class_info(instance, "Window".to_string());
        let tag_handlers = runtime.alloc_ordinary_object();
        runtime.set_object_member(instance, "tagHandlers", Variant::Object(tag_handlers));
        instance
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

/// A native class that registers one constant, shaped like the plugins whose
/// constants a class body reads -- `win32dialog.dll`'s `ES_LEFT`/`WS_*`
/// family, which PARQUET's and GINKA's `WIN32DialogEX` initialisers build
/// their `DefaultStyles` dictionaries from.
fn native_class_with_constant(
    name: &str,
    constant: &str,
    value: i64,
) -> (Runtime<NoHost>, ObjectHandle) {
    let mut runtime = Runtime::new();
    let class = runtime.alloc_native_constructor(
        |runtime: &mut Runtime<NoHost>, this_obj: Option<ObjectHandle>, _args: Vec<Variant>| {
            let instance = this_obj.unwrap_or_else(|| runtime.alloc_ordinary_object());
            Ok(Variant::Object(instance))
        },
    );
    runtime.add_object_class_info(class, name.to_string());
    runtime.register_object_native(
        class,
        "finalize",
        |_runtime: &mut Runtime<NoHost>, _this: Option<ObjectHandle>, _args: Vec<Variant>| {
            Ok(Variant::Void)
        },
    );
    runtime.set_object_member(class, constant, Variant::Integer(value));
    runtime.set_global_member(name, Variant::Object(class));
    (runtime, class)
}

/// A class body's field initialiser reads an unqualified constant that only
/// the *native* super class carries.
///
/// M35's minimal reproduction: `class Sub extends WIN32Dialog { var p =
/// ES_LEFT; }` raised `MemberNotFound: Member "ES_LEFT" does not exist` at
/// `WIN32DialogEX [Class] bytecode 21` in PARQUET, and GINKA's
/// `system/win32dialog.tjs` class body failed the same way.  A body's
/// unqualified reads compile to this-proxy reads on the under-construction
/// instance (`gpd %r, %-2.*N`), and krkrz reaches the native super class
/// through the class context's superclass getter
/// (`tTJSInterCodeContext::PropGet`, `tjsInterCodeExec.cpp:3144`).
#[test]
fn class_body_field_reads_an_inherited_native_constant() {
    let (mut runtime, _class) = native_class_with_constant("W32", "ES_LEFT", 7);
    let file = compile_source_to_bytecode(
        "class-body-native-const.tjs",
        r#"
        class Sub extends W32 { var probe = ES_LEFT; }
        var s = new Sub();
        return s.probe;
        "#,
    )
    .expect("compile");
    assert_eq!(
        runtime.execute_file(&file).expect("execute"),
        Variant::Integer(7)
    );
}

/// The same read through `this`, which is the shape `WIN32DialogEX`'s
/// initialiser uses (`this.DefaultStyles = %[...]` with unqualified `WS_*`
/// entries).
#[test]
fn class_body_this_assignment_reads_an_inherited_native_constant() {
    let (mut runtime, _class) = native_class_with_constant("W32", "WS_BORDER", 11);
    let file = compile_source_to_bytecode(
        "class-body-native-const-this.tjs",
        r#"
        class Sub2 extends W32 { this.probe = WS_BORDER; }
        var s = new Sub2();
        return s.probe;
        "#,
    )
    .expect("compile");
    assert_eq!(
        runtime.execute_file(&file).expect("execute"),
        Variant::Integer(11)
    );
}

/// A class body's own member still wins over the native super class's name:
/// the instance's own map is consulted before the class link, so a shadowing
/// declaration never picks up the plugin constant.
#[test]
fn class_body_own_member_shadows_the_inherited_native_constant() {
    let (mut runtime, _class) = native_class_with_constant("W32", "ES_LEFT", 7);
    let file = compile_source_to_bytecode(
        "class-body-native-shadow.tjs",
        r#"
        class Sub3 extends W32 { var ES_LEFT = 99; var probe = ES_LEFT; }
        var s = new Sub3();
        return s.probe;
        "#,
    )
    .expect("compile");
    assert_eq!(
        runtime.execute_file(&file).expect("execute"),
        Variant::Integer(99)
    );
}

/// A name a *script* class in the chain declares is not answered from the
/// class link: the class's own body installs it (a derived class runs it as
/// `super.Base()`), and its initialisers must still run.  This is the
/// `SystemRegistory` shape from GINKA's `sysscn/system.tjs`, where the base
/// body owns `_map`.
#[test]
fn script_super_class_member_is_installed_by_its_body_not_the_link() {
    let (mut runtime, _class) = native_class_with_constant("W32", "WS_BORDER", 11);
    let file = compile_source_to_bytecode(
        "class-body-script-base.tjs",
        r#"
        class Base {
            var value = 40;
            function Base() { value += 2; }
            function getValue() { return value; }
        }
        class Sub5 extends W32 { var probe = WS_BORDER; }
        class Child extends Base {
            function Child() { super.Base(); }
            function getValue() { return value; }
        }
        var c = new Child();
        return c.getValue() + "/" + (new Sub5()).probe;
        "#,
    )
    .expect("compile");
    assert_eq!(
        runtime.execute_file(&file).expect("execute"),
        Variant::String("42/11".to_string())
    );
}

/// The class link does not outlive the body it describes: an instance whose
/// body finished resolves the same name through its own super class, and an
/// unrelated object reports the reference's miss.
#[test]
fn class_body_link_does_not_answer_for_other_receivers() {
    let (mut runtime, _class) = native_class_with_constant("W32", "ES_LEFT", 7);
    let file = compile_source_to_bytecode(
        "class-body-link-scope.tjs",
        r#"
        class Sub4 extends W32 { function probe() { return typeof ES_LEFT; } }
        var s = new Sub4();
        var plain = new global.Array();
        plain.probe = function() { return typeof ES_LEFT; };
        var plainResult;
        try { plainResult = plain.probe(); } catch (e) { plainResult = "miss"; }
        return s.probe() + "/" + plainResult;
        "#,
    )
    .expect("compile");
    assert_eq!(
        runtime.execute_file(&file).expect("execute"),
        Variant::String("Integer/miss".to_string())
    );
}

/// A member read hands back the *stored* value, binding and all.
///
/// `tTJSCustomObject::PropGet` (`tjsObject.cpp:1392`) copies the member
/// variant unchanged -- `TJSDefaultPropGet` (`:1347`) only applies
/// `TJS_SELECT_OBJTHIS` (`:1367`) before its `result->CopyRef(targ)` (`:1386`)
/// -- and `tTJSObjectProxy::PropGet` (`tjsInterCodeExec.cpp:289-299`, the
/// forward at `:295`) merely dispatches the read onward, so a value
/// that was written with an ObjThis -- the self-bound `new Dictionary()`
/// result the reference hands out (`tjsInterCodeExec.cpp:2384`) -- still
/// carries that ObjThis when it comes back out.  Rebinding it to the reading
/// `this` changes what `(Dictionary.assign incontextof dest)(src)` copies,
/// because the native prefers `clo.ObjThis` over `clo.Object`
/// (`tjsDictionary.cpp:179-184`).
///
/// GINKA's `system/uiloader.tjs` stores its extra-command table that way
/// (`UIListParser.ExtraType = System._uiloadExtraType`), reads it back inside
/// the class's constructor and copies it onto the parser instance.  A rebound
/// read made that copy take the *instance's* members instead, so the table
/// came out empty and every `remove`/`clear` command in a `.func` UI layout
/// file silently did nothing: the first-play title menu kept the AFTER/NEXT
/// entries its `title_first.func` removes and drew CONTINUE on top of NEXT.
#[test]
fn member_read_keeps_a_stored_values_own_this() {
    let value = ok(r#"
        class Base { }
        var sharedTable = new Dictionary();
        sharedTable.marker = 7;
        Base.sharedTable = sharedTable;
        class Reader extends Base {
            var copied = new Dictionary();
            function Reader() { refill(); }
            function refill() {
                (Dictionary.assign incontextof copied)(Base.sharedTable, 0);
            }
        }
        var reader = new Reader();
        return reader.copied.marker;
        "#);
    assert_eq!(value, Variant::Integer(7));
}
