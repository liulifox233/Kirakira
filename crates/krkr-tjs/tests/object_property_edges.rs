use krkr_tjs::{ObjectValue, Value};

#[test]
fn object_property_names_are_deterministic_for_golden_tests() {
    let mut object = ObjectValue::new();
    object.set_property("z", Value::Integer(1));
    object.set_property("a", Value::Integer(2));
    object.set_property("m", Value::Integer(3));

    let names = object.property_names().collect::<Vec<_>>();

    assert_eq!(names, ["a", "m", "z"]);
}

#[test]
fn nested_object_values_can_be_stored_as_properties() {
    let mut child = ObjectValue::new();
    child.set_property("name", Value::from("child"));

    let mut parent = ObjectValue::new();
    parent.set_property("child", Value::from(child));

    assert!(matches!(
        parent.get_property("child"),
        Some(Value::Object(object)) if object.get_property("name") == Some(Value::from("child"))
    ));
}

#[test]
fn cloned_object_values_share_identity_and_properties() {
    let mut original = ObjectValue::new();
    original.set_property("count", Value::Integer(1));

    let mut clone = original.clone();
    clone.set_property("count", Value::Integer(2));

    assert_eq!(original, clone);
    assert_eq!(original.id(), clone.id());
    assert_eq!(original.get_property("count"), Some(Value::Integer(2)));
    assert_eq!(clone.get_property("count"), Some(Value::Integer(2)));
}
