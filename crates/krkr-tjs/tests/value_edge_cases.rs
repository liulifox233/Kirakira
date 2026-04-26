use krkr_tjs::{NumberValue, ObjectValue, RuntimeError, RuntimeErrorKind, Value, add_values};

#[test]
fn value_type_names_cover_all_current_variants() {
    let values = [
        (Value::Void, "void"),
        (Value::Null, "null"),
        (Value::Boolean(true), "boolean"),
        (Value::Integer(1), "integer"),
        (Value::Real(1.5), "real"),
        (Value::from("text"), "string"),
        (Value::Octet(vec![0x01, 0xFF]), "octet"),
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
        (Value::Octet(vec![0x01, 0xFF]), "<% 01 FF %>"),
        (Value::from(ObjectValue::new()), "[object Object]"),
    ];

    for (value, expected) in values {
        assert_eq!(value.to_tjs_string(), expected);
    }
}

#[test]
fn value_conversions_cover_numeric_strings_void_and_legacy_bool() {
    assert_eq!(
        Value::from("0x10").to_number(),
        Ok(NumberValue::Integer(16))
    );
    assert_eq!(Value::from("0b10").to_number(), Ok(NumberValue::Integer(2)));
    assert_eq!(Value::from("010").to_number(), Ok(NumberValue::Integer(8)));
    assert_eq!(Value::from("0o10").to_number(), Ok(NumberValue::Integer(8)));
    assert_eq!(
        Value::from("-0b10").to_number(),
        Ok(NumberValue::Integer(-2))
    );
    assert_eq!(Value::from("true").to_number(), Ok(NumberValue::Integer(1)));
    assert_eq!(
        Value::from("false").to_number(),
        Ok(NumberValue::Integer(0))
    );
    assert_eq!(Value::from("2.5").to_number(), Ok(NumberValue::Real(2.5)));
    assert_eq!(Value::from("not a number").to_integer(), Ok(0));
    assert_eq!(Value::Void.to_number(), Ok(NumberValue::Integer(0)));
    assert_eq!(Value::Boolean(true).to_integer(), Ok(1));
    assert_eq!(Value::Boolean(false).coerce_to_string(), Ok("0".to_owned()));
    assert_eq!(Value::Void.coerce_to_string(), Ok(String::new()));
}

#[test]
fn value_truthiness_matches_first_tjs_runtime_contract() {
    assert!(!Value::Void.is_truthy());
    assert!(!Value::Null.is_truthy());
    assert!(!Value::Boolean(false).is_truthy());
    assert!(!Value::Integer(0).is_truthy());
    assert!(!Value::Real(f64::NAN).is_truthy());
    assert!(!Value::from("").is_truthy());
    assert!(!Value::from("false").is_truthy());
    assert!(!Value::from("text").is_truthy());
    assert!(!Value::Octet(Vec::new()).is_truthy());

    assert!(Value::Integer(1).is_truthy());
    assert!(Value::Real(-0.25).is_truthy());
    assert!(Value::from("true").is_truthy());
    assert!(Value::from("0b10").is_truthy());
    assert!(Value::from("010").is_truthy());
    assert!(Value::from("1").is_truthy());
    assert!(Value::from("-2").is_truthy());
    assert!(Value::Octet(vec![0]).is_truthy());
    assert!(Value::from(ObjectValue::new()).is_truthy());
}

#[test]
fn integer_addition_overflow_reports_type_error_until_numeric_semantics_expand() {
    let error = add_values(&Value::Integer(i64::MAX), &Value::Integer(1))
        .expect_err("integer overflow should not wrap");

    assert_eq!(error.kind(), &RuntimeErrorKind::TypeError);
}

#[test]
fn unsupported_addition_reports_operand_type_names() {
    let error = add_values(&Value::from(ObjectValue::new()), &Value::Integer(1))
        .expect_err("object arithmetic is not scaffolded");

    assert_eq!(error.kind(), &RuntimeErrorKind::TypeError);
    assert!(error.message().contains("object"));
    assert!(error.message().contains("integer"));
}

#[test]
fn octet_conversion_to_string_reports_type_error() {
    let error = Value::Octet(vec![0x41])
        .coerce_to_string()
        .expect_err("octet is binary and should not implicitly stringify");

    assert_eq!(error.kind(), &RuntimeErrorKind::TypeError);
    assert!(error.message().contains("octet"));
}

#[test]
fn runtime_error_display_uses_message() {
    let error = RuntimeError::type_error("bad value");

    assert_eq!(error.to_string(), "bad value");
}
