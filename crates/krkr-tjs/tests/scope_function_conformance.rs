use krkr_tjs::{Runtime, Value};

#[test]
#[ignore = "requires lexical/global scope resolution"]
fn conformance_scope_shadowing_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/scope/scope_shadowing.tjs"),
        Value::from("local/global"),
    );
}

#[test]
#[ignore = "requires closures and captured variables"]
fn conformance_scope_closure_capture_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/scope/closure_capture.tjs"),
        Value::Integer(6),
    );
}

#[test]
#[ignore = "requires this binding for method calls"]
fn conformance_scope_this_binding_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/scope/this_binding.tjs"),
        Value::Integer(42),
    );
}

#[test]
#[ignore = "requires function call frame argument/default handling"]
fn conformance_call_arguments_and_defaults_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/call/arguments_and_defaults.tjs"),
        Value::from("1,void,3"),
    );
}

#[test]
#[ignore = "requires native Rust function callback bridge"]
fn conformance_call_native_callback_bridge_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/call/native_callback_bridge.tjs"),
        Value::Integer(42),
    );
}

fn assert_source_returns(source: &str, expected: Value) {
    let mut runtime = Runtime::new();

    assert_eq!(runtime.eval(source), Ok(expected));
}
