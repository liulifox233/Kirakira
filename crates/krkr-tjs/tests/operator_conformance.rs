use krkr_tjs::{Runtime, Value};

#[test]
#[ignore = "requires parser/evaluator wiring for numeric arithmetic operators"]
fn conformance_operator_numeric_matrix_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/operator/numeric_matrix.tjs"),
        Value::Integer(36),
    );
}

#[test]
#[ignore = "requires parser/evaluator wiring for equality/comparison and logical &&"]
fn conformance_operator_equality_comparison_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/operator/equality_comparison.tjs"),
        Value::Boolean(true),
    );
}

#[test]
#[ignore = "requires TJS2 logical short-circuit semantics"]
fn conformance_operator_logical_short_circuit_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/operator/logical_short_circuit.tjs"),
        Value::Integer(1),
    );
}

#[test]
#[ignore = "requires TJS2 bitwise and shift operator semantics"]
fn conformance_operator_bitwise_shift_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/operator/bitwise_shift.tjs"),
        Value::Integer(18),
    );
}

#[test]
#[ignore = "requires TJS2 increment/decrement and compound assignment semantics"]
fn conformance_operator_increment_compound_assignment_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/operator/increment_compound_assignment.tjs"),
        Value::Integer(11),
    );
}

fn assert_source_returns(source: &str, expected: Value) {
    let mut runtime = Runtime::new();

    assert_eq!(runtime.eval(source), Ok(expected));
}
