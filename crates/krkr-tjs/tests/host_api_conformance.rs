use krkr_tjs::{Runtime, Value};

#[test]
#[ignore = "requires Kirikiri host System compatibility object"]
fn conformance_host_system_api_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/host/system_api.tjs"),
        Value::Boolean(true),
    );
}

#[test]
#[ignore = "requires Kirikiri host Storages compatibility object"]
fn conformance_host_storages_api_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/host/storages_api.tjs"),
        Value::Boolean(true),
    );
}

#[test]
#[ignore = "requires Kirikiri host Scripts.execStorage integration"]
fn conformance_host_scripts_exec_storage_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/host/scripts_exec_storage.tjs"),
        Value::from("loaded"),
    );
}

#[test]
#[ignore = "requires Kirikiri host Debug compatibility object"]
fn conformance_host_debug_api_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/host/debug_api.tjs"),
        Value::Boolean(true),
    );
}

#[test]
#[ignore = "requires Kirikiri host Plugins compatibility shim"]
fn conformance_host_plugins_api_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/host/plugins_api.tjs"),
        Value::Boolean(true),
    );
}

fn assert_source_returns(source: &str, expected: Value) {
    let mut runtime = Runtime::new();

    assert_eq!(runtime.eval(source), Ok(expected));
}
