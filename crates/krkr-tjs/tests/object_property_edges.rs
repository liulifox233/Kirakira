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
        Some(Value::Object(object)) if object.get_property("name") == Some(&Value::from("child"))
    ));
}
