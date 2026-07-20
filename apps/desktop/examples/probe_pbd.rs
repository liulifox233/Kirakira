use std::path::Path;

use krkr_engine::{EngineConfig, KrkrEngine, SystemMetrics};
use krkr_plugins::register_reference_plugins;

fn main() {
    let game_dir = std::env::args()
        .nth(1)
        .expect("usage: cargo run --example probe_pbd -- <game_dir>");

    eprintln!("[probe] opening project: {game_dir}");
    let mut engine = match KrkrEngine::new(EngineConfig {
        project_root: Some(game_dir.clone().into()),
        system_metrics: SystemMetrics {
            screen_width: 1920,
            screen_height: 1080,
            desktop_left: 0,
            desktop_top: 0,
            desktop_width: 1920,
            desktop_height: 1080,
        },
        ..EngineConfig::default()
    }) {
        Ok(e) => e,
        Err(err) => {
            eprintln!("[probe] KrkrEngine::new failed: {err}");
            std::process::exit(1);
        }
    };

    eprintln!("[probe] registering reference plugins ...");
    if let Err(err) = register_reference_plugins(&mut engine) {
        eprintln!("[probe] register_reference_plugins failed: {err}");
        std::process::exit(1);
    }

    // Probe 1: try to run startup.tjs and capture the exact failure.
    eprintln!("[probe] executing startup.tjs ...");
    match engine.execute_startup() {
        Ok(result) => eprintln!("[probe] startup.tjs OK: {result:?}"),
        Err(err) => {
            eprintln!("[probe] startup.tjs FAILED: {err}");
            // Continue probing .pbd files even after startup fails.
        }
    }

    // Probe 2: enumerate .pbd entries in every XP3 archive and decode them
    // using the same path that Scripts.evalStorage would take.
    eprintln!("[probe] scanning XP3 archives for .pbd files ...");
    let game_path = Path::new(&game_dir);
    let mut xp3_names: Vec<_> = std::fs::read_dir(game_path)
        .unwrap_or_else(|_| panic!("cannot read game dir: {game_dir}"))
        .filter_map(|e| {
            let p = e.ok()?.path();
            let n = p.file_name()?.to_string_lossy().to_string();
            if n.to_ascii_lowercase().ends_with(".xp3") {
                Some(n)
            } else {
                None
            }
        })
        .collect();
    xp3_names.sort();

    let mut total = 0usize;
    let mut failed = 0usize;
    for xp3 in &xp3_names {
        let path = game_path.join(xp3);
        let archive = match krkr_xp3::Xp3Archive::open_file(&path) {
            Ok(a) => a,
            Err(err) => {
                eprintln!("[probe]   cannot open {xp3}: {err}");
                continue;
            }
        };
        for entry in archive.entries() {
            if !entry.name.to_ascii_lowercase().ends_with(".pbd") {
                continue;
            }
            total += 1;
            let name = &entry.name;
            eprintln!("[probe]   decoding {xp3}!{name}");
            // Test the same entry point the game uses: Scripts.evalStorage.
            let script = format!(r#"Scripts.evalStorage("{name}")"#);
            match engine.execute_expression("probe_inline.tjs", &script) {
                Ok(value) => {
                    let summary = summarize_value(&engine, &value, 2);
                    eprintln!("[probe]     Scripts.evalStorage OK -> {summary}");
                }
                Err(err) => {
                    failed += 1;
                    eprintln!("[probe]     Scripts.evalStorage FAILED -> {err}");
                }
            }
        }
    }

    // Probe 3: inspect the specific .pbd files involved in the failing stack.
    eprintln!("[probe] inspecting quick-menu related .pbd files ...");
    for name in [
        "game_qmenu.pbd",
        "game_parts.pbd",
        "game_window.pbd",
        "game_novel.pbd",
    ] {
        let script = format!(r#"Scripts.evalStorage("{name}")"#);
        match engine.execute_expression("probe_inline.tjs", &script) {
            Ok(value) => {
                let summary = summarize_value(&engine, &value, 2);
                eprintln!("[probe]   {name} OK -> {summary}");
            }
            Err(err) => {
                eprintln!("[probe]   {name} FAILED -> {err}");
            }
        }
    }
    eprintln!("[probe] .pbd scan complete: total={total}, failed={failed}");
}

fn summarize_value(engine: &KrkrEngine, value: &krkr_tjs2::runtime::Variant, depth: usize) -> String {
    use krkr_tjs2::runtime::Variant;
    match value {
        Variant::Void => "void".into(),
        Variant::Null => "null".into(),
        Variant::Integer(i) => format!("int({i})"),
        Variant::Real(f) => format!("real({f})"),
        Variant::String(s) => format!("str({})", s.chars().take(40).collect::<String>()),
        Variant::Octet(b) => format!("octet({})", b.len()),
        Variant::Object(_) if depth == 0 => "{...}".into(),
        Variant::Object(h) => {
            let members: Vec<(String, Variant)> = engine.tjs_runtime().object_members(*h);
            if members.len() > 6 {
                format!("dict({} keys)", members.len())
            } else {
                let parts: Vec<String> = members
                    .iter()
                    .map(|(k, v)| format!("{k}: {}", summarize_value(engine, v, depth - 1)))
                    .collect();
                format!("{{ {p} }}", p = parts.join(", "))
            }
        }
        Variant::Closure(_) => "closure".into(),
        Variant::CodeObject(_) => "codeobject".into(),
    }
}
