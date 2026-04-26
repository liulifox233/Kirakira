use krkr_tjs::{Runtime, RuntimeOptions, Value};

#[test]
#[ignore = "requires optional JIT backend and interpreter/JIT parity harness"]
fn conformance_jit_numeric_loop_parity_fixture() {
    assert_interpreter_jit_parity(
        include_str!("fixtures/conformance/jit/numeric_loop_parity.tjs"),
        Value::Integer(499_500),
    );
}

#[test]
#[ignore = "requires optional JIT backend and property shape guards"]
fn conformance_jit_property_guard_deopt_fixture() {
    assert_interpreter_jit_parity(
        include_str!("fixtures/conformance/jit/property_guard_deopt.tjs"),
        Value::Integer(12),
    );
}

#[test]
#[ignore = "requires optional JIT backend fallback for unsupported opcodes"]
fn conformance_jit_unsupported_opcode_fallback_fixture() {
    assert_interpreter_jit_parity(
        include_str!("fixtures/conformance/jit/unsupported_opcode_fallback.tjs"),
        Value::from("handled"),
    );
}

#[test]
#[ignore = "requires stable benchmark thresholds and measurement harness"]
fn conformance_performance_property_loop_fixture() {
    assert_interpreter_jit_parity(
        include_str!("fixtures/conformance/performance/property_loop.tjs"),
        Value::Integer(1000),
    );
}

#[test]
#[ignore = "requires stable benchmark thresholds and measurement harness"]
fn conformance_performance_function_call_loop_fixture() {
    assert_interpreter_jit_parity(
        include_str!("fixtures/conformance/performance/function_call_loop.tjs"),
        Value::Integer(1000),
    );
}

fn assert_interpreter_jit_parity(source: &str, expected: Value) {
    let mut interpreter = Runtime::with_options(RuntimeOptions::interpreter());
    let mut jit = Runtime::with_options(RuntimeOptions::jit());

    let interpreted = interpreter.eval(source);
    let jitted = jit.eval(source);

    assert_eq!(interpreted, Ok(expected.clone()));
    assert_eq!(jitted, Ok(expected));
}
