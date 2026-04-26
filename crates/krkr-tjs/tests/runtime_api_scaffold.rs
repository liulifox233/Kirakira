use krkr_tjs::{
    ExecutionMode, Runtime, RuntimeErrorKind, RuntimeOptions, Value, scan_kag3_boot_plan,
};

#[test]
fn runtime_empty_source_returns_void() {
    let mut runtime = Runtime::new();

    assert_eq!(runtime.eval(" \n\t "), Ok(Value::Void));
}

#[test]
fn runtime_non_empty_source_reports_unsupported_boundary() {
    let mut runtime = Runtime::new();

    let error = runtime
        .eval("return 1;")
        .expect_err("evaluator is intentionally not implemented yet");

    assert_eq!(error.kind(), &RuntimeErrorKind::Unsupported);
    assert!(error.message().contains("parser and evaluator"));
}

#[test]
fn runtime_options_record_interpreter_and_future_jit_modes() {
    let interpreter = Runtime::with_options(RuntimeOptions::interpreter().with_max_call_depth(64));
    let jit = Runtime::with_options(RuntimeOptions::jit());

    assert_eq!(
        interpreter.options().execution_mode(),
        ExecutionMode::Interpreter
    );
    assert_eq!(interpreter.options().max_call_depth(), 64);
    assert_eq!(jit.options().execution_mode(), ExecutionMode::Jit);
}

#[test]
fn kag3_boot_plan_scanner_extracts_literal_entrypoints() {
    let source = r#"
        Scripts.execStorage("system/Initialize.tjs");
        KAGLoadScript('MessageLayer.tjs');
        KAGLoadScript("MainWindow.tjs");
        kag.process("first.ks");
    "#;

    let plan = scan_kag3_boot_plan(source);

    assert_eq!(plan.exec_storage, ["system/Initialize.tjs"]);
    assert_eq!(plan.load_scripts, ["MessageLayer.tjs", "MainWindow.tjs"]);
    assert_eq!(plan.process_scenarios, ["first.ks"]);
}

#[test]
fn kag3_boot_plan_scanner_ignores_dynamic_non_literal_calls() {
    let plan = scan_kag3_boot_plan(
        r#"
        Scripts.execStorage(name);
        KAGLoadScript(scriptName);
        kag.process(scenarioName);
    "#,
    );

    assert!(plan.is_empty());
}
