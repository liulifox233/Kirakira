use krkr_tjs::{Runtime, RuntimeErrorKind, Value};

const KAG3_STARTUP_TJS: &str = include_str!("fixtures/third_party/krkrz-kag3/startup.tjs");
const KAG3_INITIALIZE_TJS: &str =
    include_str!("fixtures/third_party/krkrz-kag3/system/Initialize.tjs");

#[test]
fn real_kag3_startup_fixture_reports_unsupported_until_tjs_runtime_exists() {
    let mut runtime = Runtime::new();

    let error = runtime
        .eval(KAG3_STARTUP_TJS)
        .expect_err("startup.tjs needs the future TJS2 runtime");

    assert_eq!(error.kind(), &RuntimeErrorKind::Unsupported);
}

#[test]
fn real_kag3_initialize_fixture_tracks_first_scenario_entrypoint() {
    assert!(KAG3_INITIALIZE_TJS.contains("kag.process(\"first.ks\")"));
    assert!(KAG3_INITIALIZE_TJS.contains("var kagVersion = \"3.32 stable rev. 2\""));
}

#[test]
#[ignore = "requires TJS2 parser/evaluator, preprocessor directives, properties, with, and Scripts.execStorage"]
fn conformance_real_kag3_startup_executes_initialize_script() {
    let mut runtime = Runtime::new();

    let actual = runtime.eval(KAG3_STARTUP_TJS);

    assert_eq!(actual, Ok(Value::Void));
}
