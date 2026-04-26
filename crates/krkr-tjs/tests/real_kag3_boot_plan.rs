use krkr_tjs::scan_kag3_boot_plan;

const KAG3_STARTUP_TJS: &str = include_str!("fixtures/third_party/krkrz-kag3/startup.tjs");
const KAG3_INITIALIZE_TJS: &str =
    include_str!("fixtures/third_party/krkrz-kag3/system/Initialize.tjs");

#[test]
fn real_kag3_startup_boot_plan_loads_initialize() {
    let plan = scan_kag3_boot_plan(KAG3_STARTUP_TJS);

    assert_eq!(plan.exec_storage, ["system/Initialize.tjs"]);
    assert!(plan.load_scripts.is_empty());
    assert!(plan.process_scenarios.is_empty());
}

#[test]
fn real_kag3_initialize_boot_plan_tracks_core_system_scripts() {
    let plan = scan_kag3_boot_plan(KAG3_INITIALIZE_TJS);

    assert!(plan.load_scripts.contains(&"Utils.tjs".to_owned()));
    assert!(plan.load_scripts.contains(&"KAGLayer.tjs".to_owned()));
    assert!(plan.load_scripts.contains(&"MessageLayer.tjs".to_owned()));
    assert!(plan.load_scripts.contains(&"MainWindow.tjs".to_owned()));
    assert_eq!(plan.process_scenarios, ["first.ks"]);
}
