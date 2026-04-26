use krkr_tjs::{Runtime, Value};

#[test]
#[ignore = "requires TJS2 class constructor and member dispatch"]
fn conformance_class_constructor_method_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/class/class_constructor_method.tjs"),
        Value::Integer(42),
    );
}

#[test]
#[ignore = "requires TJS2 inheritance and super dispatch"]
fn conformance_class_inheritance_override_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/class/class_inheritance_override.tjs"),
        Value::from("base/child"),
    );
}

#[test]
#[ignore = "requires TJS2 array indexing and length semantics"]
fn conformance_array_index_length_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/array/array_index_length.tjs"),
        Value::Integer(4),
    );
}

#[test]
#[ignore = "requires TJS2 array mutation methods"]
fn conformance_array_push_pop_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/array/array_push_pop.tjs"),
        Value::from("a/b"),
    );
}

#[test]
#[ignore = "requires runtime object graph rooting and cycle collection"]
fn conformance_gc_cycle_collection_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/gc/cycle_collection.tjs"),
        Value::Boolean(true),
    );
}

#[test]
#[ignore = "requires rooted temporaries across nested function calls"]
fn conformance_gc_temporaries_survive_calls_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/gc/temporaries_survive_calls.tjs"),
        Value::Integer(42),
    );
}

fn assert_source_returns(source: &str, expected: Value) {
    let mut runtime = Runtime::new();

    assert_eq!(runtime.eval(source), Ok(expected));
}
