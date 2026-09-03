use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use krkr_core::{FrameInput, Size};
use krkr_engine::{EngineConfig, EngineInput, KrkrEngine, ProjectStorage, SystemPaths};
use krkr_tjs2::runtime::Variant;

fn main() {
    let process_started = Instant::now();
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let warm_frames = env_usize("KRKR_CONFIG_PROBE_WARM_FRAMES").unwrap_or(2);
    let title_frames = env_usize("KRKR_CONFIG_PROBE_TITLE_FRAMES").unwrap_or(1800);
    let post_frames = env_usize("KRKR_CONFIG_PROBE_POST_FRAMES").unwrap_or(120);
    let repeats = env_usize("KRKR_CONFIG_PROBE_REPEATS").unwrap_or(1);
    let delta = Duration::from_millis(1000 / 60);

    let storage = ProjectStorage::for_root(&root).expect("storage");
    let mut engine = KrkrEngine::new(EngineConfig {
        project_storage: Some(storage),
        system_paths: SystemPaths {
            exe_path: format!("{}/", root.display()),
            ..SystemPaths::default()
        },
        ..EngineConfig::default()
    })
    .expect("engine");
    let startup = engine.execute_startup().expect("startup");
    println!("startup={startup}");

    for _ in 0..warm_frames {
        update(&mut engine, delta);
    }
    force_title_menu(&mut engine);
    wait_for_title_menu(&mut engine, delta, title_frames);
    let (storage, label) = conductor_location(&engine);
    println!("title_storage={storage} title_label={label}");

    println!("target_begin_ms={}", process_started.elapsed().as_millis());
    for repeat in 0..repeats {
        let Variant::Object(kag) = engine.tjs_runtime().global_member("kag") else {
            panic!("kag global is not initialized");
        };
        let value = engine
            .tjs_runtime_mut()
            .call_object_method(
                kag,
                "process",
                vec![
                    Variant::String(String::new()),
                    Variant::String("*title_menu_config".to_string()),
                ],
            )
            .expect("open config");
        println!("open_config[{repeat}]={value}");

        for _ in 0..post_frames {
            update(&mut engine, delta);
        }
    }
    println!("target_end_ms={}", process_started.elapsed().as_millis());
    println!("logs={}", engine.host().logs().len());
}

fn env_usize(name: &str) -> Option<usize> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
}

fn update(engine: &mut KrkrEngine, delta: Duration) {
    engine
        .update(
            EngineInput::new(
                FrameInput::new(Size::new(1280.0, 720.0), 1.0 / 60.0),
                Vec::new(),
            ),
            delta,
        )
        .expect("update");
}

fn wait_for_title_menu(engine: &mut KrkrEngine, delta: Duration, max_frames: usize) {
    for frame in 0..max_frames {
        update(engine, delta);
        let (storage, label) = conductor_location(engine);
        if storage == "title.ks" && label == "*title_menu_loop" {
            println!("title_ready_frame={frame}");
            return;
        }
    }
    let (storage, label) = conductor_location(engine);
    panic!("title menu not reached after {max_frames} frames: {storage} {label}");
}

fn force_title_menu(engine: &mut KrkrEngine) {
    let Variant::Object(kag) = engine.tjs_runtime().global_member("kag") else {
        panic!("kag global is not initialized");
    };
    let Variant::Object(conductor) = engine.tjs_runtime().object_member(kag, "conductor") else {
        panic!("kag conductor is not initialized");
    };
    let runtime = engine.tjs_runtime_mut();
    runtime
        .call_object_method(
            conductor,
            "loadScenario",
            vec![Variant::String("title.ks".to_string())],
        )
        .expect("load title scenario");
    runtime
        .call_object_method(
            conductor,
            "goToLabel",
            vec![Variant::String("*title_menu".to_string())],
        )
        .expect("go to title menu");
    runtime
        .call_object_method(conductor, "run", vec![Variant::Integer(1)])
        .expect("run title menu");
}

fn conductor_location(engine: &KrkrEngine) -> (String, String) {
    let runtime = engine.tjs_runtime();
    let Variant::Object(kag) = runtime.global_member("kag") else {
        return (String::new(), String::new());
    };
    let Variant::Object(conductor) = runtime.object_member(kag, "conductor") else {
        return (String::new(), String::new());
    };
    let storage = runtime
        .object_member(conductor, "curStorage")
        .to_tjs_string()
        .unwrap_or_default();
    let label = runtime
        .object_member(conductor, "curLabel")
        .to_tjs_string()
        .unwrap_or_default();
    (storage, label)
}
