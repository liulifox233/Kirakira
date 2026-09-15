//! Reference semantics of `Array.load`, pinned against `tjsArray.cpp`.
//!
//! `Array.load` replaces the receiver's *items*, never the receiver itself.
//! The whole method body works on the native instance behind `this`:
//! `TJS_GET_NATIVE_INSTANCE(ni, tTJSArrayNI)` (`tjsArray.cpp:264-268`), then
//! `ni->Items.clear()` (`:270`), then the lines are pushed back into the same
//! `Items` vector (`:329-332`), and the result is `tTJSVariant(objthis, objthis)`
//! (`:358`).  The dispatch object, its `Symbols` table and every extra member
//! therefore survive the call -- which is exactly what the reference's
//! `saveStruct.dll` relies on: `Array.save2`, `saveStruct2` and
//! `toStructString` are registered *without* `TJS_STATICMEMBER`, so
//! `tTJSNativeClass::CreateNew` copies them onto each Array *instance*
//! (`tjsNative.cpp:340-364`, `Main.cpp:287-291`).  A `load` that swapped the
//! object payload would take them away with it.
//!
//! | expression | reference result |
//! | --- | --- |
//! | `arr.load(path)` | the receiver's items become the file's lines; `arr` itself is unchanged |
//! | extra members of `arr` (`arr.save2 = ...`) | still there after the call |
//! | the call's result | the receiver (`tTJSVariant(objthis, objthis)`, `:358`) |
//! | `arr.load()` | `TJS_E_BADPARAMCOUNT` (`:264`) |
//! | `arr.load(path)` with an unreadable file | the stream is created before the clear, so the receiver keeps its items (`:270-273`) |
//! | a non-array receiver, e.g. `Array.load(path)` | `TJS_E_NATIVECLASSCRASH` (`TJS_GET_NATIVE_INSTANCE`, `tjsNative.h:312-319`) |
//! | line separators | `\n`, `\r` and `\r\n` all end a line (`:276-317`) |
//! | a trailing separator | does not add a final empty element (`:319-327`); `"\n"` alone is one empty line |
//! | an empty file | no elements at all |
//!
//! One spelling boundary: this engine answers the receiver in the unbound
//! `(h, NULL)` form (`Variant::Object`), where the reference's
//! `tTJSVariant(objthis, objthis)` is the `(h, h)` form `new`'s result carries,
//! so `arr.load(path) === arr` reads false here.  `Variant::discern_eq`
//! (`value.rs:242-266`) compares the binding, exactly as
//! `tTJSVariant::DiscernCompare` (`tjsVariant.cpp:764-804`) does, which is why
//! the tests below assert the items and members instead.
//!
//! The second half of this file mirrors the shape the game 少女世界的生存之道
//! writes its settings with (`sysscn/Override.tjs` `changeUserConf`): load the
//! `.cfu` when it exists, change the matching line, and write it back through
//! the plugin's `save2`.  The first call has no file to load and succeeds; the
//! second one loads and must still find the writer.

use std::collections::BTreeMap;

use crate::bytecode::BytecodeFile;
use crate::compile_source_to_bytecode;
use crate::error::{Result, TjsError, TjsErrorKind};
use crate::runtime::builtins::install_array_methods;
use crate::runtime::object::Object;
use crate::runtime::{ObjectHandle, Runtime, TjsHost, Variant};

/// A host with the text storage `Array.load`/`Array.save` use, plus the binary
/// storage the structured container (`saveStruct`/`loadStruct`) reads.
#[derive(Default)]
struct StorageHost {
    files: BTreeMap<String, String>,
    binary: BTreeMap<String, Vec<u8>>,
}

impl StorageHost {
    fn with(files: &[(&str, &str)]) -> Self {
        Self {
            files: files
                .iter()
                .map(|(name, contents)| (name.to_string(), contents.to_string()))
                .collect(),
            binary: BTreeMap::new(),
        }
    }

    fn with_binary(files: &[(&str, &[u8])]) -> Self {
        Self {
            files: BTreeMap::new(),
            binary: files
                .iter()
                .map(|(name, contents)| (name.to_string(), contents.to_vec()))
                .collect(),
        }
    }
}

impl TjsHost for StorageHost {
    fn read_text(&mut self, name: &str, _mode: &str) -> Result<String> {
        self.files
            .get(name)
            .cloned()
            .ok_or_else(|| TjsError::runtime(format!("cannot open {name}")))
    }

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

    fn write_text(&mut self, name: &str, _mode: &str, text: &str) -> Result<()> {
        self.files.insert(name.to_string(), text.to_string());
        Ok(())
    }
}

fn compile(source: &str) -> BytecodeFile {
    compile_source_to_bytecode("array.tjs", source).expect("compile")
}

fn run(runtime: &mut Runtime<StorageHost>, source: &str) -> Variant {
    let file = compile(source);
    runtime.execute_file(&file).expect("execute")
}

fn failure(runtime: &mut Runtime<StorageHost>, source: &str) -> TjsError {
    let file = compile(source);
    runtime.execute_file(&file).expect_err("script must fail")
}

/// The lines `Array.load` produced for `text`, rendered `count:joined` so an
/// element that is an empty string is not confused with an absent element.
fn loaded_lines(text: &str) -> String {
    let mut runtime = Runtime::with_host(StorageHost::with(&[("lines.txt", text)]));
    let value = run(
        &mut runtime,
        r#"
        var lines = new Array();
        lines.load("lines.txt");
        return lines.count + ":" + lines.join("|");
        "#,
    );
    let Variant::String(rendered) = value else {
        panic!("expected a rendered string, got {value:?}");
    };
    rendered
}

/// The small shape the settings writer depends on: an Array carrying an extra
/// member keeps that member across `load`, and the items are replaced in
/// place -- a second reference to the receiver sees both.
#[test]
fn load_replaces_the_items_and_keeps_the_receivers_members() {
    let mut runtime = Runtime::with_host(StorageHost::with(&[(
        "settings.cfu",
        "first\r\nsecond\r\n",
    )]));
    let value = run(
        &mut runtime,
        r#"
        var arr = new Array();
        var alias = arr;
        arr.add("stale");
        arr.save2 = function(path) { return "wrote:" + path; };
        var returned = arr.load("settings.cfu");
        return arr.count + ":" + arr[0] + ":" + arr[1] + ":" + typeof arr.save2 +
            ":" + arr.save2("out") + ":" + alias.count + ":" + alias[0] +
            ":" + typeof returned;
        "#,
    );
    assert_eq!(
        value,
        Variant::String("2:first:second:Object:wrote:out:2:first:Object".to_string())
    );
}

/// A member installed the way `savestruct.dll` installs one -- as a native
/// member of the instance, not of the class object -- is what the game's
/// writer probes.  The mission's report was `before:Object after:undefined`.
#[test]
fn load_keeps_a_native_member_installed_on_the_instance() {
    let mut runtime = Runtime::with_host(StorageHost::with(&[("settings.cfu", "first\n")]));
    let array = runtime.alloc_object(Object::array(Vec::new()));
    install_array_methods(&mut runtime, array);
    runtime.register_object_native(array, "save2", save2);
    runtime.set_global_member("arr", Variant::Object(array));

    let value = run(
        &mut runtime,
        r#"
        var before = typeof arr.save2;
        arr.load("settings.cfu");
        var after = typeof arr.save2;
        arr.save2("out.txt");
        return before + ":" + after + ":" + arr.join("|");
        "#,
    );
    assert_eq!(value, Variant::String("Object:Object:first".to_string()));
    assert_eq!(
        runtime.host().files.get("out.txt"),
        Some(&"first\r\n".to_string()),
        "the writer ran after the load; one element, CRLF-terminated (`Main.cpp:224-256`)"
    );
}

/// The game's `changeUserConf` shape, run twice.  The first call has no file
/// to load and always worked; the second call loads the file the first one
/// wrote, and that is where the writer used to disappear.
#[test]
fn the_settings_writer_survives_the_second_change() {
    let mut runtime = Runtime::with_host(StorageHost::with(&[]));
    install_array_save2(&mut runtime);
    // `Storages.isExistentStorage(path)`, the game's existence probe.
    let storages = runtime.alloc_ordinary_object();
    runtime.register_object_native(
        storages,
        "isExistentStorage",
        |runtime: &mut Runtime<StorageHost>, _, args: Vec<Variant>| {
            let path = args
                .first()
                .map(Variant::to_tjs_string)
                .transpose()?
                .unwrap_or_default();
            Ok(Variant::Integer(
                runtime.host().files.contains_key(&path) as i64
            ))
        },
    );
    runtime.set_global_member("Storages", Variant::Object(storages));

    let file = compile(
        r#"
        function changeUserConf(a0, a1) {
            var l0;
            var l1 = "savedata/game.cfu";
            if (Storages.isExistentStorage(l1)) {
                var t2 = global.Array;
                var t1 = new t2();
                l0 = t1;
                try { l0.load(l1); } catch (e) { }
            }
            if (l0 === void) {
                var t2 = global.Array;
                var t1 = new t2();
                t1[0] = "; header";
                l0 = t1;
            }
            var l4 = a0 + "=\"" + a1 + "\"";
            var l5 = 0;
            for (var l6 = 0; l6 < l0.count; l6++) {
                var l7 = l0[l6];
                if (l7.indexOf(a0 + "=\"") == 0) {
                    l0[l6] = l5 ? ";" : l4;
                    l5 = 1;
                }
            }
            if (!l5) l0.add(l4);
            try {
                l0.save2(l1);
                return "ok";
            } catch (e) {
                return "设置保存失败: 更改失败\n" + a0;
            }
        }
        "#,
    );
    runtime.execute_file(&file).expect("execute");

    let first = run(&mut runtime, r#"return changeUserConf("curmove", "yes");"#);
    assert_eq!(first, Variant::String("ok".to_string()));
    assert_eq!(
        runtime.host().files.get("savedata/game.cfu"),
        Some(&"; header\r\ncurmove=\"yes\"\r\n".to_string()),
        "the first call writes the file"
    );

    let second = run(&mut runtime, r#"return changeUserConf("curmove", "no");"#);
    assert_eq!(second, Variant::String("ok".to_string()));
    assert_eq!(
        runtime.host().files.get("savedata/game.cfu"),
        Some(&"; header\r\ncurmove=\"no\"\r\n".to_string()),
        "the second call loads the file, changes the line and writes it back"
    );
}

/// Installs `Array.save2` the way `savestruct.dll` does: on every instance the
/// `Array` constructor produces (`NCB_ATTACH_CLASS` + `RawCallback`,
/// `Main.cpp:281-291`, copied into instances by `tjsNative.cpp:340-364`).
fn install_array_save2(runtime: &mut Runtime<StorageHost>) {
    let original = runtime.global_member("Array");
    let constructor = runtime.alloc_native_function(
        move |runtime: &mut Runtime<StorageHost>, _this_obj, args| {
            let value = runtime.call_function(original.clone(), args)?;
            if let Some(instance) = value.object_handle() {
                runtime.register_object_native(instance, "save2", save2);
            }
            Ok(value)
        },
    );
    runtime.add_object_class_info(constructor, "Array");
    runtime.set_global_member("Array", Variant::Object(constructor));
}

/// `Array.save2(filename, utf8=false, newline=0)`: one element per line, CRLF
/// unless told otherwise (`Main.cpp:224-256`).
fn save2(
    runtime: &mut Runtime<StorageHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let path = args
        .first()
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    let Some(handle) = this_obj else {
        return Err(TjsError::native_class_crash());
    };
    let lines = runtime.heap[handle.0]
        .array_elements()
        .ok_or_else(TjsError::native_class_crash)?
        .iter()
        .map(Variant::to_tjs_string)
        .collect::<Result<Vec<_>>>()?;
    runtime
        .host_mut()
        .write_text(&path, "", &format!("{}\r\n", lines.join("\r\n")))?;
    Ok(Variant::Void)
}

/// The reference splits on `\n`, `\r` and `\r\n` alike, and a separator at the
/// end of the file does not add an empty element (`tjsArray.cpp:276-327`).
/// The expected value is `count:joined`, so `1:` is one empty line.
#[test]
fn load_splits_lines_the_way_the_reference_does() {
    assert_eq!(loaded_lines(""), "0:");
    assert_eq!(loaded_lines("a"), "1:a");
    assert_eq!(loaded_lines("a\n"), "1:a");
    assert_eq!(loaded_lines("a\r\nb\r\n"), "2:a|b");
    assert_eq!(loaded_lines("a\rb"), "2:a|b", "a lone CR ends a line");
    assert_eq!(loaded_lines("\n"), "1:", "one break is one empty line");
    assert_eq!(loaded_lines("a\n\n"), "2:a|");
}

/// The reference opens the text stream before it clears the items, so a read
/// that fails leaves the receiver exactly as it was (`tjsArray.cpp:270-273`).
#[test]
fn a_failed_read_leaves_the_receiver_untouched() {
    let mut runtime = Runtime::with_host(StorageHost::with(&[]));
    let value = run(
        &mut runtime,
        r#"
        var arr = new Array();
        arr.add("keep");
        try { arr.load("missing.txt"); } catch (e) { return arr.count + ":" + arr[0]; }
        return "no error";
        "#,
    );
    assert_eq!(value, Variant::String("1:keep".to_string()));
}

/// A receiver without an Array native instance reports
/// `TJS_E_NATIVECLASSCRASH` (`TJS_GET_NATIVE_INSTANCE`, `tjsNative.h:312-319`)
/// instead of having its payload rewritten into an array.
#[test]
fn load_on_a_non_array_receiver_is_a_native_class_crash() {
    let mut runtime = Runtime::with_host(StorageHost::with(&[("lines.txt", "a\n")]));
    let error = failure(&mut runtime, r#"return Array.load("lines.txt");"#);
    assert_eq!(error.kind, TjsErrorKind::NativeClassCrash);
    assert_eq!(error.tjs_error_code(), Some(-1008));
    assert_eq!(error.message, "Invalid object context");
    assert_eq!(
        run(&mut runtime, "return typeof (new Array()).load;"),
        Variant::String("Object".to_string())
    );
}

/// A numeric member name is what `IsNumber` says it is (`tjsArray.cpp:54-77`):
/// an optional sign, digits and dots -- not Rust's numeric syntax.  `"1e3"` is
/// therefore an ordinary member name, never element 1000, and `"0x10"` is a
/// member name too.
#[test]
fn a_numeric_member_name_is_isnumber_not_rust_number_syntax() {
    let mut runtime = Runtime::with_host(StorageHost::with(&[]));
    let value = run(
        &mut runtime,
        r#"
        var arr = new Array();
        arr["1e3"] = 1;
        arr["0x10"] = 2;
        return arr.count + ":" + arr["1e3"] + ":" + arr["0x10"] + ":" + typeof arr["1e3"];
        "#,
    );
    assert_eq!(value, Variant::String("0:1:2:Integer".to_string()));
}

/// `TJS_atoi` accumulates into a 32-bit `int` (`tjsConfig.cpp:54-79`,
/// `tjsTypes.h:59`), so an index whose value wraps is the wrapped -- small --
/// index.  `"4294967296"` is element 0, not a four-billion element resize.
#[test]
fn a_numeric_member_name_wraps_at_32_bits_like_tjs_atoi() {
    let mut runtime = Runtime::with_host(StorageHost::with(&[]));
    let value = run(
        &mut runtime,
        r#"
        var arr = new Array();
        arr["4294967296"] = 7;
        arr["4294967297"] = 8;
        return arr.count + ":" + arr[0] + ":" + arr[1];
        "#,
    );
    assert_eq!(value, Variant::String("2:7:8".to_string()));
}

/// `Array.split` replaces the receiver's *items* (`ni->Items.resize(0)` plus
/// the pushes, `tjsArray.cpp:508-560`), never the receiver itself, exactly as
/// `Array.load` does (`:270-332`).  Every member and every alias therefore
/// survives the call.
#[test]
fn split_replaces_the_items_and_keeps_the_receivers_members() {
    let mut runtime = Runtime::with_host(StorageHost::with(&[]));
    let value = run(
        &mut runtime,
        r#"
        var arr = new Array();
        var alias = arr;
        arr.add("stale");
        arr.save2 = function(path) { return "wrote:" + path; };
        var returned = arr.split(",", "x,y");
        return arr.count + ":" + arr[0] + ":" + arr[1] + ":" + typeof arr.save2 +
            ":" + arr.save2("out") + ":" + alias.count + ":" + alias[0] +
            ":" + typeof returned;
        "#,
    );
    assert_eq!(
        value,
        Variant::String("2:x:y:Object:wrote:out:2:x:Object".to_string())
    );
}

/// `Array.loadStruct` reads the reference's container and nothing else:
/// `tTJSBinarySerializer::IsBinary` decides (`tjsArray.cpp:379-385`), and a
/// readable path holding anything else is `TJS_E_INVALIDPARAM` (`:400-402`) --
/// there is no text path and no Integer success flag.
#[test]
fn load_struct_accepts_only_the_binary_container() {
    // A `TJS/4s0` data pack, the container `patch.xp3>title.pbd` carries.
    let mut runtime = Runtime::with_host(StorageHost::with_binary(&[(
        "data.pbd",
        b"TJS/4s0\0\x01\x02",
    )]));
    let error = failure(
        &mut runtime,
        r#"return (new Array()).loadStruct("data.pbd");"#,
    );
    assert_eq!(error.kind, TjsErrorKind::InvalidParam);
    assert_eq!(error.tjs_error_code(), Some(-1003));
    // `ni->Items.clear()` runs before the stream is opened (`tjsArray.cpp:373`),
    // so the receiver is empty however the read ends.
    let value = run(
        &mut runtime,
        r#"
        var arr = new Array();
        arr.add("stale");
        try { arr.loadStruct("data.pbd"); } catch (e) { return arr.count; }
        return "no error";
        "#,
    );
    assert_eq!(value, Variant::Integer(0));
}

/// The positive half of the same rule: a `KBAD100\0` pack is restored into the
/// receiver in file order (`tTJSBinarySerializer::CreateArray`'s `RootArray`
/// path, `tjsBinarySerializer.cpp:104-112`), and `ReadArray` hands that same
/// receiver back as the result (`:268-280`).
#[test]
fn load_struct_restores_a_binary_pack_into_the_receiver() {
    let mut runtime = Runtime::with_host(StorageHost::with(&[]));
    let value = run(
        &mut runtime,
        r#"
        var arr = new Array();
        arr.add(5);
        arr.add("six");
        arr.saveStruct("pack.ksd", "b");
        var loaded = new Array();
        loaded.add("stale");
        loaded.save2 = function() { return "kept"; };
        var returned = loaded.loadStruct("pack.ksd");
        return loaded.count + ":" + loaded[0] + ":" + loaded[1] + ":" +
            typeof loaded.save2 + ":" + typeof returned;
        "#,
    );
    assert_eq!(value, Variant::String("2:5:six:Object:Object".to_string()));
    assert!(
        runtime
            .host()
            .binary
            .get("pack.ksd")
            .expect("the binary pack was written")
            .starts_with(b"KBAD100\0")
    );
}
