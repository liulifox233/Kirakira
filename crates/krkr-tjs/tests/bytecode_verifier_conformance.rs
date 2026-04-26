use krkr_tjs::{Runtime, Value};

#[test]
#[ignore = "requires bytecode verifier and jump validation"]
fn conformance_bytecode_verifier_rejects_bad_jump_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/bytecode/bad_jump.tjs"),
        Value::Boolean(true),
    );
}

#[test]
#[ignore = "requires bytecode verifier for stack height invariants"]
fn conformance_bytecode_verifier_rejects_stack_mismatch_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/bytecode/stack_mismatch.tjs"),
        Value::Boolean(true),
    );
}

#[test]
#[ignore = "requires bytecode compiler constant pool interning"]
fn conformance_bytecode_constant_pool_interning_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/bytecode/constant_pool_interning.tjs"),
        Value::Boolean(true),
    );
}

fn assert_source_returns(source: &str, expected: Value) {
    let mut runtime = Runtime::new();

    assert_eq!(runtime.eval(source), Ok(expected));
}
