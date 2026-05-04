use std::{
    fs,
    path::{Path, PathBuf},
};

use krkr_engine::{KrkrEngine, host::KrkrHost};
use krkr_tjs2::runtime::{ObjectHandle, Runtime, Variant};

fn main() {
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let storage = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "savedata/data0.bmp".to_string());
    let mode = std::env::args().nth(3).unwrap_or_else(|| {
        appended_bmp_offset(&root.join(&storage))
            .map(|offset| format!("o{offset}"))
            .unwrap_or_default()
    });

    let mut engine = KrkrEngine::for_project(&root).expect("engine");
    let load_script = format!(
        "global.__save_probe_data = Scripts.evalStorage({}, {}); return true;",
        tjs_quote(&storage),
        tjs_quote(&mode)
    );
    engine
        .execute_script("save-probe-load.tjs", &load_script)
        .expect("load save");

    println!("root={}", root.display());
    println!("storage={storage}");
    println!("mode={mode}");

    let Variant::Object(data) = engine.tjs_runtime().global_member("__save_probe_data") else {
        println!("data=<not object>");
        return;
    };

    print_members(engine.tjs_runtime(), data, "data", 0);
    print_path(engine.tjs_runtime(), data, "data.id", &["id"]);
    print_path(
        engine.tjs_runtime(),
        data,
        "data.core.currentLabel",
        &["core", "currentLabel"],
    );
    print_path(
        engine.tjs_runtime(),
        data,
        "data.core.currentPageName",
        &["core", "currentPageName"],
    );
    print_path(
        engine.tjs_runtime(),
        data,
        "data.core.currentMessage",
        &["core", "currentMessage"],
    );
    for member in [
        "storageName",
        "storageShortName",
        "curLabel",
        "curLine",
        "curPos",
        "lineBufferUsing",
        "ExcludeLevel",
        "IfLevel",
        "ExcludeLevelStack",
        "IfLevelExecutedStack",
        "macroArgStackBase",
        "macroArgStackDepth",
    ] {
        print_path(
            engine.tjs_runtime(),
            data,
            &format!("data.core.mainConductor.{member}"),
            &["core", "mainConductor", member],
        );
    }
    print_path(
        engine.tjs_runtime(),
        data,
        "data.core.mainConductor.callStack.count",
        &["core", "mainConductor", "callStack", "count"],
    );

    let target_storage = object_path(
        engine.tjs_runtime(),
        data,
        &["core", "mainConductor", "storageName"],
    )
    .to_tjs_string()
    .unwrap_or_default();
    let target_label = object_path(engine.tjs_runtime(), data, &["core", "currentLabel"])
        .to_tjs_string()
        .unwrap_or_default();
    if !target_storage.is_empty() && !target_label.is_empty() {
        let callback_probe = format!(
            r#"
            var p = new KAGParser();
            p.onLabel = function(label, page) {{
                global.__save_probe_callback_label = label;
                global.__save_probe_callback_curLabel = this.curLabel;
                global.__save_probe_callback_store = this.store();
            }};
            p.loadScenario({});
            p.goToLabel({});
            global.__save_probe_callback_parser = p;
            global.__save_probe_callback_first_tag = p.getNextTag();
            return true;
            "#,
            tjs_quote(&target_storage),
            tjs_quote(&target_label)
        );
        match engine.execute_script("save-probe-callback-store.tjs", &callback_probe) {
            Ok(_) => {
                println!("callback.targetStorage={}", target_storage);
                println!("callback.targetLabel={target_label}");
                println!(
                    "callback.label={}",
                    engine
                        .tjs_runtime()
                        .global_member("__save_probe_callback_label")
                        .to_tjs_string()
                        .unwrap_or_default()
                );
                println!(
                    "callback.parserCurLabel={}",
                    engine
                        .tjs_runtime()
                        .global_member("__save_probe_callback_curLabel")
                        .to_tjs_string()
                        .unwrap_or_default()
                );
                if let Variant::Object(stored) = engine
                    .tjs_runtime()
                    .global_member("__save_probe_callback_store")
                {
                    println!(
                        "callback.store.curLabel={}",
                        string_member(engine.tjs_runtime(), stored, "curLabel")
                    );
                    println!(
                        "callback.store.curLine={}",
                        string_member(engine.tjs_runtime(), stored, "curLine")
                    );
                    println!(
                        "callback.store.curPos={}",
                        string_member(engine.tjs_runtime(), stored, "curPos")
                    );
                }
            }
            Err(error) => println!("callback_probe_error={error}"),
        }
    }

    let restore = engine.execute_script(
        "save-probe-restore.tjs",
        r#"
        var p = new KAGParser();
        p.restore(global.__save_probe_data.core.mainConductor);
        global.__save_probe_parser = p;
        return true;
        "#,
    );
    if let Err(error) = restore {
        println!("restore_error={error}");
        return;
    }

    let Variant::Object(parser) = engine.tjs_runtime().global_member("__save_probe_parser") else {
        println!("parser=<not object>");
        return;
    };
    println!(
        "restored.curStorage={}",
        string_member(engine.tjs_runtime(), parser, "curStorage")
    );
    println!(
        "restored.curLabel={}",
        string_member(engine.tjs_runtime(), parser, "curLabel")
    );
    println!(
        "restored.curLine={}",
        string_member(engine.tjs_runtime(), parser, "curLine")
    );
    println!(
        "restored.curPos={}",
        string_member(engine.tjs_runtime(), parser, "curPos")
    );

    for index in 0..20 {
        let next = engine
            .tjs_runtime_mut()
            .call_object_method(parser, "getNextTag", Vec::new());
        match next {
            Ok(Variant::Object(tag)) => {
                let runtime = engine.tjs_runtime();
                let name = string_member(runtime, tag, "tagname");
                let text = string_member(runtime, tag, "text");
                let storage_attr = string_member(runtime, tag, "storage");
                let target = string_member(runtime, tag, "target");
                println!(
                    "next[{index}]=tagname:{name} text:{text} storage:{storage_attr} target:{target}"
                );
            }
            Ok(Variant::Void) => {
                println!("next[{index}]=<eof>");
                break;
            }
            Ok(value) => {
                println!("next[{index}]={}", variant_string(&value));
                break;
            }
            Err(error) => {
                println!("next[{index}] error={error}");
                break;
            }
        }
    }
}

fn appended_bmp_offset(path: &Path) -> Option<u64> {
    let bytes = fs::read(path).ok()?;
    if bytes.len() < 54 || &bytes[..2] != b"BM" {
        return None;
    }
    let data_offset = u32::from_le_bytes(bytes[10..14].try_into().ok()?) as u64;
    let dib_header_size = u32::from_le_bytes(bytes[14..18].try_into().ok()?);
    if dib_header_size < 40 {
        return None;
    }
    let width = i32::from_le_bytes(bytes[18..22].try_into().ok()?).unsigned_abs() as u64;
    let height = i32::from_le_bytes(bytes[22..26].try_into().ok()?).unsigned_abs() as u64;
    let planes = u16::from_le_bytes(bytes[26..28].try_into().ok()?);
    let bits_per_pixel = u16::from_le_bytes(bytes[28..30].try_into().ok()?) as u64;
    let compression = u32::from_le_bytes(bytes[30..34].try_into().ok()?);
    let image_size = u32::from_le_bytes(bytes[34..38].try_into().ok()?) as u64;
    if planes != 1 || compression != 0 || width == 0 || height == 0 || bits_per_pixel == 0 {
        return None;
    }
    let row_bytes = (width * bits_per_pixel).div_ceil(32) * 4;
    Some(data_offset + image_size.max(row_bytes * height))
}

fn print_members(runtime: &Runtime<KrkrHost>, object: ObjectHandle, label: &str, depth: usize) {
    let members = runtime.object_members(object);
    println!("{label}.members={}", members.len());
    if depth >= 1 {
        return;
    }
    for (name, value) in members {
        println!("{label}.{name}={}", variant_string(&value));
    }
}

fn print_path(runtime: &Runtime<KrkrHost>, root: ObjectHandle, label: &str, path: &[&str]) {
    println!(
        "{label}={}",
        variant_string(&object_path(runtime, root, path))
    );
}

fn object_path(runtime: &Runtime<KrkrHost>, root: ObjectHandle, path: &[&str]) -> Variant {
    let mut current = Variant::Object(root);
    for member in path {
        let Variant::Object(object) = current else {
            return Variant::Void;
        };
        current = runtime.object_member(object, member);
    }
    current
}

fn string_member(runtime: &Runtime<KrkrHost>, object: ObjectHandle, name: &str) -> String {
    runtime
        .object_member(object, name)
        .to_tjs_string()
        .unwrap_or_default()
}

fn variant_string(value: &Variant) -> String {
    match value {
        Variant::Void => "<void>".to_string(),
        Variant::Null => "null".to_string(),
        Variant::Integer(value) => value.to_string(),
        Variant::Real(value) => value.to_string(),
        Variant::String(value) => format!("{value:?}"),
        Variant::Octet(value) => format!("<octet:{}>", value.len()),
        Variant::Object(_) => "[object]".to_string(),
        Variant::Closure(_) => "[closure]".to_string(),
        Variant::CodeObject(_) => "[code]".to_string(),
    }
}

fn tjs_quote(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}
