use krkr_tjs::{ObjectValue, Runtime, Value};

#[test]
fn object_property_set_get_and_overwrite_are_stable() {
    let mut object = ObjectValue::new();

    assert_eq!(object.set_property("answer", Value::Integer(41)), None);
    assert_eq!(object.get_property("answer"), Some(&Value::Integer(41)));
    assert_eq!(
        object.set_property("answer", Value::Integer(42)),
        Some(Value::Integer(41))
    );

    assert_eq!(object.get_property("answer"), Some(&Value::Integer(42)));
    assert_eq!(object.property_count(), 1);
}

#[test]
fn object_missing_property_is_distinct_from_void_value() {
    let mut object = ObjectValue::new();
    object.set_property("present", Value::Void);

    assert!(object.has_property("present"));
    assert_eq!(object.get_property("present"), Some(&Value::Void));
    assert_eq!(object.get_property("missing"), None);
}

#[test]
#[ignore = "requires TJS2 object model and prototype/property resolution"]
fn conformance_object_prototype_lookup_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/object/prototype_lookup.tjs"),
        Value::Integer(12),
    );
}

#[test]
#[ignore = "requires TJS2 property attributes and delete semantics"]
fn conformance_object_property_attributes_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/object/property_attributes.tjs"),
        Value::Boolean(true),
    );
}

fn assert_source_returns(source: &str, expected: Value) {
    let mut runtime = Runtime::new();

    let actual = runtime.eval(source);

    assert_eq!(actual, Ok(expected));
}
