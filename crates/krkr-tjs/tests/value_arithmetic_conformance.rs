use krkr_tjs::{
    Runtime, Value, add_values, div_values, equal_values, greater_than_values, less_than_values,
    mul_values, rem_values, strict_equal_values, sub_values,
};

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
fn value_numeric_arithmetic_preserves_integer_results_except_division() {
    assert_eq!(
        sub_values(&Value::Integer(9), &Value::Integer(3)).expect("subtract integers"),
        Value::Integer(6)
    );
    assert_eq!(
        mul_values(&Value::Integer(4), &Value::Integer(2)).expect("multiply integers"),
        Value::Integer(8)
    );
    assert_eq!(
        div_values(&Value::Integer(8), &Value::Integer(2)).expect("divide integers"),
        Value::Real(4.0)
    );
    assert_eq!(
        rem_values(&Value::Integer(7), &Value::Integer(3)).expect("integer remainder"),
        Value::Integer(1)
    );
}

#[test]
fn value_non_plus_arithmetic_coerces_numeric_strings() {
    assert_eq!(
        sub_values(&Value::from("10"), &Value::from("4")).expect("subtract numeric strings"),
        Value::Integer(6)
    );
    assert_eq!(
        sub_values(&Value::from("010"), &Value::from("0b10"))
            .expect("subtract non-decimal numeric strings"),
        Value::Integer(6)
    );
    assert_eq!(
        mul_values(&Value::from("2.5"), &Value::Integer(2)).expect("multiply real string"),
        Value::Real(5.0)
    );
}

#[test]
fn value_addition_handles_void_numeric_identity_and_octet_concat() {
    assert_eq!(
        add_values(&Value::Void, &Value::Integer(3)).expect("void coerces to zero"),
        Value::Integer(3)
    );
    assert_eq!(
        add_values(&Value::Octet(vec![0x01]), &Value::Octet(vec![0x02, 0x03]))
            .expect("octet concat"),
        Value::Octet(vec![0x01, 0x02, 0x03])
    );
}

#[test]
fn value_comparison_and_equality_cover_first_operator_slice() {
    assert!(less_than_values(&Value::Integer(1), &Value::Integer(2)).expect("numeric less-than"));
    assert!(greater_than_values(&Value::Integer(2), &Value::from("1")).expect("string number"));
    assert!(less_than_values(&Value::from("a"), &Value::from("b")).expect("string less-than"));

    assert!(equal_values(&Value::Integer(1), &Value::from("1")));
    assert!(!strict_equal_values(&Value::Integer(1), &Value::from("1")));
    assert!(strict_equal_values(&Value::Integer(1), &Value::Integer(1)));
}

#[test]
#[ignore = "requires the TJS2 parser/evaluator to execute the arithmetic precedence fixture"]
fn conformance_value_arithmetic_precedence_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/value/arithmetic_precedence.tjs"),
        Value::Integer(7),
    );
}

#[test]
#[ignore = "requires the TJS2 parser/evaluator to execute the string/numeric coercion fixture"]
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
