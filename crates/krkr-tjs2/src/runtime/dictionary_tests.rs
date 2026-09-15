//! Reference semantics of the TJS2 `Dictionary` class, pinned as a matrix.
//!
//! The matrix below is the truth table this crate's implementation is held to.
//! It comes from three sources: the class registration and native methods
//! (`krkr2/tjs2/tjsDictionary.cpp`, `tjsNative.cpp`, `tjsObject.h`), the
//! dispatch overrides of `tTJSDictionaryObject` (`tjsDictionary.cpp:713-755`),
//! and the official manual, which states the instance rule outright:
//!
//! > Dictionary クラスのオブジェクトは、作成された状態ではメンバを何一つ
//! > 持っていません。……`dict.assign(src)` のように記述しても、dict が assign
//! > というメソッドを持っていないためにエラーになります。したがって、
//! > incontextof 演算子を使って、Dictionary クラスに直接属しているメソッドを、
//! > 対象となる Dictionary クラスのオブジェクトをコンテキストとして実行させます。
//! > (`docs/tjs2/j/contents/dictionary.html`)
//!
//! | expression | reference result |
//! | --- | --- |
//! | `new Dictionary()` | no members at all |
//! | `d.clear` / `d.assign` / `d.count` | void (`tTJSDictionaryObject::PropGet` maps a miss to void, `tjsDictionary.cpp:720-731`); `typeof` → "undefined" |
//! | `d.clear()` | error `TJS_E_MEMBERNOTFOUND` (`FuncCall` has no such override, `:713-722`) |
//! | `d.assign(src, 1)` | same error: `assign` is not an instance member |
//! | `(Dictionary.clear incontextof d)()` | empties `d` (`ni->Clear()`, `:374-377`) |
//! | `(Dictionary.assign incontextof d)(src, 1)` | clear `d`, then copy every source key |
//! | `(Dictionary.assign incontextof d)(src, 0)` | copy over `d`, keeping keys the source lacks |
//! | a source key named `clear`/`assign`/`count` | copied as plain data -- only `TJS_HIDDENMEMBER` is skipped (`:378-400`) |
//! | Array source | elements read as name/value pairs; a trailing element is dropped (`:329-346`) |
//! | `(Dictionary.assign incontextof d)()` | `TJS_E_BADPARAMCOUNT` (`:353`) |
//! | `(Dictionary.assign incontextof d)(void)` | `TJSNullAccess` (`:359`) |
//! | `Dictionary.assign(src, 1)` (no `incontextof`) | `TJS_E_NATIVECLASSCRASH` (`TJS_GET_NATIVE_INSTANCE`, `tjsNative.h:320-328`) |
//! | `(Dictionary.assign incontextof d)(d, 1)` | `d` ends up empty |
//! | `(Dictionary.load incontextof d)(path)` / `(Dictionary.save ...)` | void: both are registered TODO stubs that validate the instance and return `TJS_S_OK` (`:41-49`, `:110-118`) |
//! | `Dictionary.load(path)` (no `incontextof`) | `TJS_E_NATIVECLASSCRASH`, like every other method |
//! | `Dictionary.unknown` | error `TJS_E_MEMBERNOTFOUND`: the class object is a plain `tTJSNativeClass`, so the void mapping of `tTJSDictionaryObject::PropGet` never applies to it |
//!
//! Two consequences are easy to get wrong and are pinned individually:
//! the class surface never appears in a copy (it lives on the class object),
//! and `Dictionary` has no `count` member -- only `Array` registers
//! `count`/`length` as (non-static) properties (`tjsArray.cpp:980-1013`), so
//! `d.count` is a miss while a data key named `count` is ordinary data.

use crate::compile_source_to_bytecode;
use crate::compiler::execute_source;
use crate::error::{Result, TjsError, TjsErrorKind};
use crate::runtime::value::Variant;
use crate::runtime::{Runtime, TjsHost};

use std::collections::BTreeMap;

fn run(name: &str, source: &str) -> Variant {
    execute_source(name, source).expect("execute")
}

fn failure(source: &str) -> TjsError {
    execute_source("dictionary.tjs", source).expect_err("script must fail")
}

#[test]
fn a_new_dictionary_has_no_members_at_all() {
    // The manual's rule, and the reason KAGEX writes `(Dictionary.assign
    // incontextof dict)(...)`: every method is a static member of the class
    // object (`tjsDictionary.cpp` registers all of them with
    // `TJS_END_NATIVE_STATIC_METHOD_DECL`), and `tTJSNativeClass::FuncCall`
    // copies only non-static members onto a new instance
    // (`tjsNative.cpp:340-364`).
    assert_eq!(
        run(
            "dictionary.tjs",
            r#"
            var d = new Dictionary();
            var names = ["clear", "assign", "assignStruct", "saveStruct",
                "loadStruct", "load", "save", "count", "length"];
            var missing = 0;
            for (var i = 0; i < names.count; i++) {
                if (typeof d[names[i]] == "undefined" && d[names[i]] === void) {
                    missing++;
                }
            }
            return missing;
            "#,
        ),
        Variant::Integer(9)
    );
    // The class object still carries the surface, `load`/`save` included:
    // the reference registers both as no-op stubs (`tjsDictionary.cpp:41-49`,
    // `:110-118`), and a method that is registered but does nothing is part
    // of the surface a script can probe.
    assert_eq!(
        run(
            "dictionary.tjs",
            r#"
            return typeof Dictionary.load + ":" + typeof Dictionary.loadStruct + ":" +
                typeof Dictionary.save + ":" + typeof Dictionary.saveStruct + ":" +
                typeof Dictionary.assign + ":" + typeof Dictionary.assignStruct + ":" +
                typeof Dictionary.clear;
            "#,
        ),
        Variant::String("Object:Object:Object:Object:Object:Object:Object".into())
    );
}

/// `Dictionary.load` and `Dictionary.save` are `// TODO: implement` stubs in
/// the reference: they read the receiver's native instance and return
/// `TJS_S_OK`, so a call answers void without touching a file
/// (`tjsDictionary.cpp:41-49`, `:110-118`).  Without an `incontextof` the
/// `this` is the class object, which has no native instance
/// (`TJS_GET_NATIVE_INSTANCE`, `tjsNative.h:320-328`).
#[test]
fn dictionary_load_and_save_are_registered_no_op_stubs() {
    for source in [
        r#"var d = new Dictionary(); return (Dictionary.load incontextof d)("savedata/x.ksd");"#,
        r#"var d = new Dictionary(); return (Dictionary.save incontextof d)("savedata/x.ksd");"#,
    ] {
        assert_eq!(run("dictionary.tjs", source), Variant::Void, "{source}");
    }
    for source in [
        r#"return Dictionary.load("savedata/x.ksd");"#,
        r#"return Dictionary.save("savedata/x.ksd");"#,
    ] {
        let error = failure(source);
        assert_eq!(error.kind, TjsErrorKind::NativeClassCrash, "{source}");
        assert_eq!(error.tjs_error_code(), Some(-1008), "{source}");
        assert_eq!(error.message, "Invalid object context", "{source}");
    }
}

/// The `Dictionary` class object is a `tTJSNativeClass`, i.e. a plain
/// `tTJSCustomObject` for every protocol `tTJSDictionaryClass` does not
/// override, and the void-on-miss override belongs to
/// `tTJSDictionaryObject` -- the *instance* `CreateBaseTJSObject` builds
/// (`tjsDictionary.cpp:235-238`, `:720-731`).  So a miss on the class object
/// raises, while the same miss on an instance answers void.
#[test]
fn a_miss_on_the_class_object_raises_like_a_plain_object() {
    let error = failure("return Dictionary.unknown;");
    assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
    assert_eq!(error.tjs_error_code(), Some(-1001));
    assert_eq!(error.message, "Member \"unknown\" does not exist");

    let error = failure("return Dictionary.unknown();");
    assert_eq!(error.message, "Member \"unknown\" does not exist");

    // `typeof` still maps that miss to "undefined" (`TypeOfMemberDirect`,
    // `tjsInterCodeExec.cpp:2134-2139`).
    assert_eq!(
        run("dictionary.tjs", "return typeof Dictionary.unknown;"),
        Variant::String("undefined".into())
    );

    // The instance keeps the leniency the class object does not have.
    assert_eq!(
        run(
            "dictionary.tjs",
            "var d = new Dictionary(); return d.unknown === void;"
        ),
        Variant::Integer(1)
    );
}

#[test]
fn instance_style_calls_are_member_not_found() {
    // Official `tTJSDictionaryObject::FuncCall` keeps
    // `tTJSCustomObject::FuncCall`'s `TJS_E_MEMBERNOTFOUND`, so the call never
    // reaches the native method (only `PropGet` maps a miss to void).
    let error = failure("var d = new Dictionary(); d.clear();");
    assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
    assert_eq!(error.message, "Member \"clear\" does not exist");

    let error = failure("var d = new Dictionary(); d.assign(%[a => 1], 1);");
    assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
    assert_eq!(error.message, "Member \"assign\" does not exist");

    // A missing member call is the same miss, not "not a function".
    let error = failure("var d = new Dictionary(); d.missing();");
    assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
    assert_eq!(error.message, "Member \"missing\" does not exist");
}

#[test]
fn a_dictionary_miss_reads_as_void_and_typeof_reports_undefined() {
    // `tTJSDictionaryObject::PropGet` answers void unless the reader passes
    // `TJS_MEMBERMUSTEXIST`; `typeof` does pass it, and the VM turns the
    // resulting `TJS_E_MEMBERNOTFOUND` into "undefined"
    // (`tjsInterCodeExec.cpp:2134`).
    assert_eq!(
        run("dictionary.tjs", r#"var d = %[]; return typeof d.nope;"#),
        Variant::String("undefined".into())
    );
    assert_eq!(
        run(
            "dictionary.tjs",
            r#"var d = %[]; return (d.nope === void) + ":" + (d.nope === null);"#,
        ),
        Variant::String("1:0".into())
    );
    // Host-side readers that demand the member get the miss.
    assert!(matches!(
        run(
            "dictionary.tjs",
            r#"var d = %[]; if (d.nope) { return 1; } return 0;"#
        ),
        Variant::Integer(0)
    ));
}

#[test]
fn assign_clears_the_destination_before_copying() {
    // `if(clear) Owner->Clear();` (`tjsDictionary.cpp:334`/`:347`).
    assert_eq!(
        run(
            "dictionary.tjs",
            r#"
            var dest = %[stale => 1, shared => 1];
            (Dictionary.assign incontextof dest)(%[shared => 2, fresh => 3], 1);
            return (dest.stale === void) + ":" + dest.shared + ":" + dest.fresh;
            "#,
        ),
        Variant::String("1:2:3".into())
    );
    // `clear=false` keeps whatever the source does not mention.
    assert_eq!(
        run(
            "dictionary.tjs",
            r#"
            var dest = %[stale => 1, shared => 1];
            (Dictionary.assign incontextof dest)(%[shared => 2, fresh => 3], 0);
            return dest.stale + ":" + dest.shared + ":" + dest.fresh;
            "#,
        ),
        Variant::String("1:2:3".into())
    );
    // The default for an omitted or void second argument is "clear".
    assert_eq!(
        run(
            "dictionary.tjs",
            r#"
            var dest = %[stale => 1];
            (Dictionary.assign incontextof dest)(%[a => 1]);
            var second = %[stale => 1];
            (Dictionary.assign incontextof second)(%[a => 1], void);
            return (dest.stale === void) + ":" + (second.stale === void);
            "#,
        ),
        Variant::String("1:1".into())
    );
}

#[test]
fn assign_copies_keys_that_are_named_like_builtin_members() {
    // `tAssignCallback` skips only `TJS_HIDDENMEMBER` (`tjsDictionary.cpp:385-390`),
    // so a tag attribute (or any data key) called `clear`, `assign` or `count`
    // is copied verbatim -- the finding that motivated this matrix had the old
    // `is_native_member_name` filter dropping exactly these.
    assert_eq!(
        run(
            "dictionary.tjs",
            r#"
            var src = %[clear => "yes", assign => "no", count => 7, add => 8, length => 9];
            var dest = %[];
            (Dictionary.assign incontextof dest)(src, 1);
            return dest.clear + ":" + dest.assign + ":" + dest.count + ":" +
                dest.add + ":" + dest.length;
            "#,
        ),
        Variant::String("yes:no:7:8:9".into())
    );
    // `typeof` sees the data, not a method, and the key survives a copy.
    assert_eq!(
        run(
            "dictionary.tjs",
            r#"
            var src = %[]; src.clear = "true";
            var dest = %[];
            (Dictionary.assign incontextof dest)(src, 1);
            return typeof dest.clear + ":" + (dest.clear ? "T" : "F");
            "#,
        ),
        Variant::String("String:T".into())
    );
    // The same rule applies when an Array copies a Dictionary: pairs only,
    // and in the source's *enumeration* order -- the reference's bucket walk,
    // where `page` (hash slot 2) comes before `clear` (slot 4).
    assert_eq!(
        run(
            "dictionary.tjs",
            r#"
            var src = %[clear => "true", page => "back"];
            var list = [];
            list.assign(src);
            return list.count + ":" + list[0] + "=" + list[1] + ":" + list[2] + "=" + list[3];
            "#,
        ),
        Variant::String("4:page=back:clear=true".into())
    );
}

#[test]
fn the_class_surface_does_not_leak_into_copies_or_arrays() {
    // The copy holds the source's own keys and nothing else: no `assign`,
    // `clear`, `count` or `length` unless the source had them.
    assert_eq!(
        run(
            "dictionary.tjs",
            r#"
            var src = %[answer => 42];
            var dest = %[];
            (Dictionary.assign incontextof dest)(src, 1);
            return typeof dest.assign + ":" + typeof dest.clear + ":" +
                typeof dest.count + ":" + typeof dest.answer;
            "#,
        ),
        Variant::String("undefined:undefined:undefined:Integer".into())
    );
    assert_eq!(
        run(
            "dictionary.tjs",
            r#"
            var src = %[answer => 42];
            var list = [];
            list.assign(src);
            return list.count + ":" + list[0] + ":" + list[1];
            "#,
        ),
        Variant::String("2:answer:42".into())
    );
}

#[test]
fn assign_from_an_array_reads_name_value_pairs() {
    // `tTJSDictionaryNI::Assign`'s array branch (`tjsDictionary.cpp:329-346`):
    // each pair is (name, value); the name is stringified and a trailing
    // unpaired element is dropped.
    assert_eq!(
        run(
            "dictionary.tjs",
            r#"
            var dest = %[stale => 1];
            (Dictionary.assign incontextof dest)(["a", 1, "b", 2], 1);
            return (dest.stale === void) + ":" + dest.a + ":" + dest.b;
            "#,
        ),
        Variant::String("1:1:2".into())
    );
    assert_eq!(
        run(
            "dictionary.tjs",
            r#"
            var dest = %[];
            (Dictionary.assign incontextof dest)(["a", 1, "trailing"], 1);
            return (dest.trailing === void) + ":" + dest.a;
            "#,
        ),
        Variant::String("1:1".into())
    );
    // A numeric name is stringified the way the reference stringifies it.
    assert_eq!(
        run(
            "dictionary.tjs",
            r#"
            var dest = %[];
            (Dictionary.assign incontextof dest)([1, "one"], 1);
            return dest["1"];
            "#,
        ),
        Variant::String("one".into())
    );
}

#[test]
fn assign_to_itself_empties_the_destination() {
    // The reference clears first and only then enumerates the source, which is
    // the same object.
    assert_eq!(
        run(
            "dictionary.tjs",
            r#"
            var d = %[a => 1, b => 2];
            (Dictionary.assign incontextof d)(d, 1);
            return (d.a === void) + ":" + (d.b === void);
            "#,
        ),
        Variant::String("1:1".into())
    );
}

#[test]
fn assign_argument_errors_match_the_reference() {
    let error = failure("(Dictionary.assign incontextof %[a => 1])();");
    assert_eq!(error.kind, TjsErrorKind::BadParamCount);
    assert_eq!(error.message, "Invalid argument count");

    let error = failure("(Dictionary.assign incontextof %[a => 1])(void, 1);");
    assert_eq!(error.message, "Accessing to null object");

    let error = failure("(Dictionary.assign incontextof %[a => 1])(null, 1);");
    assert_eq!(error.message, "Accessing to null object");

    let error = failure("(Dictionary.assign incontextof %[a => 1])(5, 1);");
    assert_eq!(error.message, "Accessing to null object");
}

#[test]
fn calling_the_class_object_reports_a_native_class_crash() {
    // `TJS_GET_NATIVE_INSTANCE` reads the native instance off `objthis`; the
    // class object has none, so the reference reports
    // `TJS_E_NATIVECLASSCRASH` instead of mutating the class.
    let error = failure("Dictionary.assign(%[a => 1], 1);");
    assert_eq!(error.kind, TjsErrorKind::NativeClassCrash);
    assert_eq!(error.message, "Invalid object context");

    let error = failure("Dictionary.clear();");
    assert_eq!(error.kind, TjsErrorKind::NativeClassCrash);

    // The class surface survived those failed calls.
    assert_eq!(
        run(
            "dictionary.tjs",
            "return typeof Dictionary.assign + \":\" + typeof Dictionary.clear;"
        ),
        Variant::String("Object:Object".into())
    );
}

#[test]
fn clear_empties_the_dictionary_and_keeps_the_class_intact() {
    assert_eq!(
        run(
            "dictionary.tjs",
            r#"
            var d = %[a => 1, b => 2];
            (Dictionary.clear incontextof d)();
            return (d.a === void) + ":" + (d.b === void) + ":" + typeof Dictionary.clear;
            "#,
        ),
        Variant::String("1:1:Object".into())
    );
    // Clearing cannot leave the method surface behind: a cleared Dictionary
    // reads a method name as void, exactly like a fresh one.
    assert_eq!(
        run(
            "dictionary.tjs",
            r#"
            var d = %[a => 1];
            (Dictionary.clear incontextof d)();
            return (d.clear === void) + ":" + (d.assign === void);
            "#,
        ),
        Variant::String("1:1".into())
    );
}

#[test]
fn assign_struct_copies_data_members_including_method_named_keys() {
    // `AssignStructure` enumerates the source's own symbols through the same
    // entry list the text serializer uses (`tjsDictionary.cpp:410-470`): the
    // class surface is not in it, and a data key named like a method is.
    assert_eq!(
        run(
            "dictionary.tjs",
            r#"
            var src = %[answer => 42];
            src.clear = "kept";
            var dest = %[stale => 1];
            (Dictionary.assignStruct incontextof dest)(src);
            return (dest.stale === void) + ":" + dest.answer + ":" + dest.clear +
                ":" + (dest.assign === void);
            "#,
        ),
        Variant::String("1:42:kept:1".into())
    );
    // An Array source is not a Dictionary, so the structure copy refuses it
    // the way the reference's `NativeInstanceSupport` lookup does not find a
    // dictionary behind it.
    assert_eq!(
        run(
            "dictionary.tjs",
            r#"
            var dest = %[];
            (Dictionary.assignStruct incontextof dest)(%[nested => %[a => 1]]);
            return dest.nested.a;
            "#,
        ),
        Variant::Integer(1)
    );
}

#[test]
fn assign_struct_deep_clones_a_self_bound_member() {
    // A member stored from `new` (`source.child = new Dictionary()`) carries
    // its binding, and the deep copy has to clone the object behind it rather
    // than copy the bound value, or the destination would alias the source.
    assert_eq!(
        run(
            "dictionary.tjs",
            r#"
            var source = %[];
            source.name = "outer";
            source.child = new Dictionary();
            source.child.name = "inner";
            var dest = %[];
            (Dictionary.assignStruct incontextof dest)(source);
            dest.child.name = "changed";
            return (dest.child !== source.child) + ":" + source.child.name + ":" +
                dest.name;
            "#,
        ),
        Variant::String("1:inner:outer".into())
    );
}

#[test]
fn kagex_attribute_chain_reaches_the_free_arm() {
    // The shape of the game's `syspage` handler, in official bytecode: the
    // handler copies its attribute dictionary and walks
    // uiload -> position -> current -> clear -> free.  With the class surface
    // off the copy, `free` is the first attribute that is set and the arm runs;
    // before this fix the copy still answered the native `clear` method, the
    // clear arm stole the call, and `[syspage free page=back]` -- the save
    // screen close's only layer-hiding step -- never executed.
    assert_eq!(
        run(
            "dictionary.tjs",
            r#"
            class Handler {
                var arm = "";
                var reached = 0;
                function onTag(elm) {
                    var dict = new Dictionary();
                    (Dictionary.assign incontextof dict)(elm, 1);
                    if (dict.layer === void) { dict.layer = "message1"; }
                    if (dict.uiload) {
                        this.arm = "uiload";
                    } else if (dict.position) {
                        this.arm = "position";
                    } else if (dict.current) {
                        this.arm = "current";
                    } else if (dict.clear) {
                        this.arm = "clear";
                    } else if (dict.free) {
                        this.arm = "free";
                        delete dict.free;
                        dict.height = 1;
                        dict.width = 1;
                        dict.visible = 0;
                        this.reached = 1;
                    }
                    return this.arm + ":" + dict.layer + ":" + dict.page + ":" + dict.visible;
                }
            }
            var handler = new Handler();
            return handler.onTag(%[free => "true", page => "back"]) +
                ":" + handler.arm + ":" + handler.reached;
            "#,
        ),
        Variant::String("free:message1:back:0:free:1".into())
    );
    // A handler call that really carries `clear` still takes the clear arm, so
    // the chain keeps discriminating between attributes.
    assert_eq!(
        run(
            "dictionary.tjs",
            r#"
            var arm = "none";
            var dict = new Dictionary();
            (Dictionary.assign incontextof dict)(%[clear => "true"], 1);
            if (dict.uiload) { arm = "uiload"; }
            else if (dict.position) { arm = "position"; }
            else if (dict.current) { arm = "current"; }
            else if (dict.clear) { arm = "clear"; }
            else if (dict.free) { arm = "free"; }
            return arm;
            "#,
        ),
        Variant::String("clear".into())
    );
}

/// The member order a `Dictionary` (or any ordinary object) enumerates in is
/// the reference's bucket walk, not a key order and not an insertion order.
///
/// `tTJSCustomObject::InternalEnumMembers` (`tjsObject.cpp:1207-1240`) walks
/// `Symbols` bucket by bucket, and within a bucket the slot's chain before the
/// slot itself.  These four keys land in slots 0, 1, 2 and 3 of the default
/// eight-slot table (`hash_name`), so the enumerated order is
/// `e, c, a, b`: sorted would be `a, b, c, e` and insertion `e, a, c, b`.
/// `Array.assign` copies a dictionary as name/value pairs in that order
/// (`tTJSArrayNI::Assign`, `tjsArray.cpp:1060-1085`), so the pairs are the
/// observable.
#[test]
fn members_enumerate_in_the_reference_bucket_order() {
    assert_eq!(
        run(
            "order.tjs",
            r#"
            var data = %[];
            data.e = 1; data.a = 1; data.c = 1; data.b = 1;
            var list = [];
            list.assign(data);
            return list.join(",");
            "#,
        ),
        Variant::String("e,1,c,1,a,1,b,1".into())
    );
}

/// Two keys in one slot: the first takes the slot, the later one hangs off its
/// chain, and the chain is emitted newest-first with the slot last
/// (`InternalEnumMembers`, `tjsObject.cpp:1219-1240`).
#[test]
fn colliding_keys_chain_newest_first_under_their_slot() {
    assert_eq!(
        run(
            "order.tjs",
            r#"
            var data = %[];
            data.e = 1; data.l = 1; data.w = 1; data.y = 1;
            var list = [];
            list.assign(data);
            return list.join(",");
            "#,
        ),
        Variant::String("y,1,w,1,l,1,e,1".into())
    );
}

/// A member found at chain position 3 or later moves to the front of its chain
/// on the read (`Find`'s `if(cnt>2)`, `tjsObject.cpp:1110-1118`), so reading a
/// deep member changes the order the object enumerates in.
#[test]
fn reading_a_deep_chain_member_moves_it_to_the_front() {
    let probe = |read: &str| {
        run(
            "order.tjs",
            &format!(
                r#"
            var data = %[];
            data.e = 1; data.l = 1; data.w = 1; data.y = 1; data.ab = 1;
            {read}
            var list = [];
            list.assign(data);
            return list.join(",");
            "#
            ),
        )
    };
    // All five keys share one slot: `e` took the slot and `l`, `w`, `y`, `ab`
    // chained in front of it newest-first, so the chain runs ab, y, w, l.
    // `w` sits at index 2: no move.
    assert_eq!(
        probe("var value = data.w;"),
        Variant::String("ab,1,y,1,w,1,l,1,e,1".into())
    );
    // `l` sits at index 3: the read moves it to the front.
    assert_eq!(
        probe("var value = data.l;"),
        Variant::String("l,1,ab,1,y,1,w,1,e,1".into())
    );
}

/// Deleting the slot head frees the slot for the next insert into that bucket,
/// and the surviving chain stays behind it (`DeleteByName` + `PostClear`,
/// `tjsObject.cpp:862-866`, `tjsObject.h:451-458`).
#[test]
fn deleting_a_slot_head_frees_it_for_the_next_insert() {
    let build = |extra: &str| {
        run(
            "order.tjs",
            &format!(
                r#"
            var data = %[];
            data.e = 1; data.l = 1; data.w = 1; data.y = 1;
            {extra}
            var list = [];
            list.assign(data);
            return list.join(",");
            "#
            ),
        )
    };
    assert_eq!(
        build("data.ab = 1;"),
        Variant::String("ab,1,y,1,w,1,l,1,e,1".into()),
        "without the delete the new key chains in front of the slot"
    );
    assert_eq!(
        build("delete data.e; data.ab = 1;"),
        Variant::String("y,1,w,1,l,1,ab,1".into()),
        "after deleting the head the new key takes the freed slot"
    );
}

/// `new Dictionary(count)` (`tjsDictionaryClass::CreateNew`,
/// `tjsDictionary.cpp:244-257`) sizes the table from the count the way
/// `RebuildHash` does, and the size is observable: the same eight keys
/// enumerate in a different order from a table that started wider.
#[test]
fn dictionary_creation_size_changes_the_order() {
    let build = |constructor: &str| {
        run(
            "order.tjs",
            &format!(
                r#"
            var data = {constructor};
            data.e = 1; data.a = 1; data.c = 1; data.b = 1;
            data.e2 = 1; data.l2 = 1; data.w2 = 1; data.y2 = 1;
            var list = [];
            list.assign(data);
            return list.join(",");
            "#
            ),
        )
    };
    // Eight slots, as an inline `%[...]` and `new Dictionary()` both start.
    assert_eq!(
        build("new Dictionary()"),
        Variant::String("e2,1,e,1,c,1,l2,1,a,1,b,1,w2,1,y2,1".into())
    );
    // `new Dictionary(16)` starts at 64 slots (`hash_bits_for_count(16)` = 6).
    assert_eq!(
        build("new Dictionary(16)"),
        Variant::String("e2,1,a,1,e,1,w2,1,c,1,b,1,l2,1,y2,1".into())
    );
    assert_eq!(
        build("%[]"),
        Variant::String("e2,1,e,1,c,1,l2,1,a,1,b,1,w2,1,y2,1".into()),
        "an inline dictionary literal is `new Dictionary()` with no arguments"
    );
    // The literal's own entries are inserted at construction, so `%[e2 => 1]`
    // puts `e2` into the slot before the scripted `data.e = 1` runs.
    assert_eq!(
        build("%[e2 => 1]"),
        Variant::String("e,1,e2,1,c,1,l2,1,a,1,b,1,w2,1,y2,1".into())
    );
}

// ---------------------------------------------------------------------------
// `loadStruct`'s container rule

/// A host with the binary storage `loadStruct` reads, and the text storage its
/// expression path reads.  `with` gives it only binary files, so the text path
/// has nothing to fall back on -- which is what the container-rule test needs.
#[derive(Default)]
struct StructStorage {
    binary: BTreeMap<String, Vec<u8>>,
    files: BTreeMap<String, String>,
}

impl StructStorage {
    fn with(files: &[(&str, &[u8])]) -> Self {
        Self {
            binary: files
                .iter()
                .map(|(name, contents)| (name.to_string(), contents.to_vec()))
                .collect(),
            files: BTreeMap::new(),
        }
    }

    fn with_text(files: &[(&str, &str)]) -> Self {
        Self {
            binary: BTreeMap::new(),
            files: files
                .iter()
                .map(|(name, contents)| (name.to_string(), contents.to_string()))
                .collect(),
        }
    }
}

impl TjsHost for StructStorage {
    fn read_binary(&mut self, name: &str, _mode: &str) -> Result<Vec<u8>> {
        self.binary
            .get(name)
            .cloned()
            .ok_or_else(|| TjsError::runtime(format!("cannot open {name}")))
    }

    fn write_binary(&mut self, name: &str, _mode: &str, bytes: &[u8]) -> Result<()> {
        self.binary.insert(name.to_string(), bytes.to_vec());
        Ok(())
    }

    fn read_text(&mut self, name: &str, _mode: &str) -> Result<String> {
        self.files
            .get(name)
            .cloned()
            .ok_or_else(|| TjsError::runtime(format!("cannot open {name}")))
    }
}

fn run_with_struct_storage(files: &[(&str, &[u8])], source: &str) -> Variant {
    let mut runtime = Runtime::with_host(StructStorage::with(files));
    let file = compile_source_to_bytecode("dictionary.tjs", source).expect("compile");
    runtime.execute_file(&file).expect("execute")
}

fn struct_storage_failure(files: &[(&str, &[u8])], source: &str) -> TjsError {
    let mut runtime = Runtime::with_host(StructStorage::with(files));
    let file = compile_source_to_bytecode("dictionary.tjs", source).expect("compile");
    runtime.execute_file(&file).expect_err("script must fail")
}

fn run_with_text_storage(files: &[(&str, &str)], source: &str) -> Variant {
    let mut runtime = Runtime::with_host(StructStorage::with_text(files));
    let file = compile_source_to_bytecode("dictionary.tjs", source).expect("compile");
    runtime.execute_file(&file).expect("execute")
}

/// `Dictionary.loadStruct` reads the binary container through
/// `tTJSBinarySerializer::IsBinary` (`tjsDictionary.cpp:72-76`) and answers the
/// deserialized root, never an Integer success flag.  A data pack of another
/// kind is not a container: with no text stream to fall back on, the call fails
/// with `TJS_E_INVALIDPARAM` (`return TJS_E_INVALIDPARAM;`, `:121`).
#[test]
fn load_struct_answers_the_binary_container_or_invalid_param() {
    // A `TJS/4s0` data pack (`PackinOne.dll`'s LZ4 framing), which is what
    // `patch.xp3>title.pbd` holds.
    let error = struct_storage_failure(
        &[("data.pbd", b"TJS/4s0\0\x01\x02")],
        r#"return Dictionary.loadStruct("data.pbd");"#,
    );
    assert_eq!(error.kind, TjsErrorKind::InvalidParam);
    assert_eq!(error.tjs_error_code(), Some(-1003));

    // An unreadable path: the reference's binary stream creation fails first
    // and answers `TJS_E_INVALIDPARAM` too (`tjsDictionary.cpp:71-72`).
    let error = struct_storage_failure(&[], r#"return Dictionary.loadStruct("missing.ksd");"#);
    assert_eq!(error.kind, TjsErrorKind::InvalidParam);
}

/// The reference's *second* container: the text path
/// (`tjsDictionary.cpp:104-110`) hands the file to
/// `tTJS::LoadTextDictionaryArray` (`tjs.cpp:607-624`), which compiles it as a
/// TJS `expression` -- `SetText(result, buffer, NULL, true)`
/// (`tjsScriptBlock.cpp:230-237`) -- and the call answers that expression's
/// value.  The engine writes exactly such an expression, so a text struct reads
/// back through it, and the answer is the value, not an Integer success flag.
#[test]
fn load_struct_reads_the_text_form_as_an_expression() {
    assert_eq!(
        run_with_text_storage(
            &[(
                "state.ksd",
                r#"%["answer" => 42, "child" => %["name" => "nested"]]"#,
            )],
            r#"
            var loaded = Dictionary.loadStruct("state.ksd");
            return typeof loaded + ":" + loaded.answer + ":" + loaded.child.name;
            "#,
        ),
        Variant::String("Object:42:nested".into())
    );

    // Any expression, not just a dictionary literal: the reference answers
    // `*result = *var` for whatever the file evaluates to.
    assert_eq!(
        run_with_text_storage(
            &[("scalar.ksd", "7 + 35")],
            r#"return Dictionary.loadStruct("scalar.ksd");"#,
        ),
        Variant::Integer(42)
    );

    // An empty file is not an error: `SetText` returns before it touches the
    // result (`tjsScriptBlock.cpp:238-239`), so the call answers void.
    assert_eq!(
        run_with_text_storage(
            &[("empty.ksd", "")],
            r#"return Dictionary.loadStruct("empty.ksd");"#,
        ),
        Variant::Void
    );

    // The `key = value` line format was this crate's invention and stays out:
    // it is not an expression, so the text path does not turn it into members.
    let mut runtime = Runtime::with_host(StructStorage::with_text(&[(
        "lines.ksd",
        "answer = 42\r\n",
    )]));
    let file = compile_source_to_bytecode(
        "dictionary.tjs",
        r#"return Dictionary.loadStruct("lines.ksd");"#,
    )
    .expect("compile");
    runtime
        .execute_file(&file)
        .expect_err("the line format is not the reference's text form");
}

/// The positive half: a `KBAD100\0` pack decodes and the call answers the
/// *deserialized root* (`if(result) *result = *var;`, `tjsDictionary.cpp:84-88`),
/// not a success flag.
#[test]
fn load_struct_returns_the_deserialized_root() {
    assert_eq!(
        run_with_struct_storage(
            &[],
            r#"
            var saved = %[];
            saved.answer = 42;
            saved.child = %[];
            saved.child.name = "nested";
            (Dictionary.saveStruct incontextof saved)("pack.ksd", "b");
            var loaded = Dictionary.loadStruct("pack.ksd");
            return typeof loaded + ":" + loaded.answer + ":" + loaded.child.name;
            "#,
        ),
        Variant::String("Object:42:nested".into())
    );
}

/// A dictionary receiver *is* the serializer's `RootDictionary`
/// (`tTJSBinarySerializer binload(dic)`, `tjsDictionary.cpp:78-82`): it is
/// cleared before the stream is opened (`ni->Clear()`, `:57-62`) and restored
/// in file order.  A receiver without a Dictionary native instance -- the
/// class object -- is left alone and decoded into a throw-away dictionary
/// (`if(!dic) dic = TJSCreateDictionaryObject();`).
#[test]
fn load_struct_restores_a_dictionary_receiver_in_place() {
    assert_eq!(
        run_with_struct_storage(
            &[],
            r#"
            var saved = %[];
            saved.answer = 42;
            (Dictionary.saveStruct incontextof saved)("pack.ksd", "b");
            var loaded = %[];
            loaded.stale = 1;
            (Dictionary.loadStruct incontextof loaded)("pack.ksd");
            var class_object = Dictionary.loadStruct("pack.ksd");
            return typeof loaded.stale + ":" + loaded.answer +
                ":" + class_object.answer;
            "#,
        ),
        Variant::String("undefined:42:42".into())
    );
}
