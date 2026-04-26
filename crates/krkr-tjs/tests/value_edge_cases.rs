use krkr_tjs::{ObjectValue, RuntimeError, RuntimeErrorKind, Value, add_values};

#[test]
fn value_type_names_cover_all_current_variants() {
    let values = [
        (Value::Void, "void"),
        (Value::Null, "null"),
        (Value::Boolean(true), "boolean"),
        (Value::Integer(1), "integer"),
        (Value::Real(1.5), "real"),
        (Value::from("text"), "string"),
        (Value::from(ObjectValue::new()), "object"),
    ];

    for (value, expected) in values {
        assert_eq!(value.type_name(), expected);
    }
}

#[test]
fn value_string_conversion_current_contract_is_stable() {
    let values = [
        (Value::Void, "void"),
        (Value::Null, "null"),
        (Value::Boolean(false), "false"),
        (Value::Integer(-12), "-12"),
        (Value::Real(1.25), "1.25"),
        (Value::from("abc"), "abc"),
        (Value::from(ObjectValue::new()), "[object Object]"),
    ];

    for (value, expected) in values {
        assert_eq!(value.to_tjs_string(), expected);
    }
}

#[test]
fn integer_addition_overflow_reports_type_error_until_numeric_semantics_expand() {
    let error = add_values(&Value::Integer(i64::MAX), &Value::Integer(1))
        .expect_err("integer overflow should not wrap");

    assert_eq!(error.kind(), &RuntimeErrorKind::TypeError);
}

#[test]
fn unsupported_addition_reports_operand_type_names() {
    let error = add_values(&Value::Boolean(true), &Value::Null)
        .expect_err("boolean/null addition is not scaffolded");

    assert_eq!(error.kind(), &RuntimeErrorKind::TypeError);
    assert!(error.message().contains("boolean"));
    assert!(error.message().contains("null"));
}

#[test]
fn runtime_error_display_uses_message() {
    let error = RuntimeError::type_error("bad value");

    assert_eq!(error.to_string(), "bad value");
}
