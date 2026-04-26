use krkr_tjs::{Runtime, Value, add_values};

#[test]
fn value_integer_addition_returns_integer() {
    let result = add_values(&Value::Integer(20), &Value::Integer(22)).expect("add integers");

    assert_eq!(result, Value::Integer(42));
}

#[test]
fn value_mixed_integer_real_addition_returns_real() {
    let result = add_values(&Value::Integer(2), &Value::Real(0.5)).expect("add mixed numbers");

    assert_eq!(result, Value::Real(2.5));
}

#[test]
fn value_string_addition_concatenates_using_current_value_strings() {
    let result = add_values(&Value::from("count="), &Value::Integer(3)).expect("concat string");

    assert_eq!(result, Value::from("count=3"));
}

#[test]
#[ignore = "requires the TJS2 parser/evaluator and the real arithmetic precedence table"]
fn conformance_value_arithmetic_precedence_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/value/arithmetic_precedence.tjs"),
        Value::Integer(7),
    );
}

#[test]
#[ignore = "requires complete TJS2 numeric/string coercion semantics"]
fn conformance_value_string_numeric_coercion_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/value/string_numeric_coercion.tjs"),
        Value::from("12"),
    );
}

fn assert_source_returns(source: &str, expected: Value) {
    let mut runtime = Runtime::new();

    let actual = runtime.eval(source);

    assert_eq!(actual, Ok(expected));
}
