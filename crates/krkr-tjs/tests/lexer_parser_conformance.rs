use krkr_tjs::{Runtime, Value};

#[test]
#[ignore = "requires TJS2 lexer comments and whitespace handling"]
fn conformance_lexer_comments_and_whitespace_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/lexer/comments_and_whitespace.tjs"),
        Value::Integer(3),
    );
}

#[test]
#[ignore = "requires TJS2 string escape lexer semantics"]
fn conformance_lexer_string_escapes_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/lexer/string_escapes.tjs"),
        Value::from("line\nquote=\" slash=\\"),
    );
}

#[test]
#[ignore = "requires TJS2 numeric literal parsing"]
fn conformance_lexer_numeric_literals_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/lexer/numeric_literals.tjs"),
        Value::Integer(255),
    );
}

#[test]
#[ignore = "requires TJS2 regular expression literal parsing"]
fn conformance_lexer_regex_literal_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/lexer/regex_literal.tjs"),
        Value::Boolean(true),
    );
}

#[test]
#[ignore = "requires TJS2 parser support for property declarations"]
fn conformance_parser_property_getter_setter_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/parser/property_getter_setter.tjs"),
        Value::Integer(42),
    );
}

#[test]
#[ignore = "requires TJS2 parser support for with blocks"]
fn conformance_parser_with_block_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/parser/with_block.tjs"),
        Value::Integer(9),
    );
}

#[test]
#[ignore = "requires TJS2 parser support for array and dictionary literals"]
fn conformance_parser_array_dictionary_literals_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/parser/array_dictionary_literals.tjs"),
        Value::from("ok"),
    );
}

#[test]
#[ignore = "requires TJS2 parser support for ternary and assignment expressions"]
fn conformance_parser_ternary_assignment_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/parser/ternary_assignment.tjs"),
        Value::Integer(14),
    );
}

fn assert_source_returns(source: &str, expected: Value) {
    let mut runtime = Runtime::new();

    assert_eq!(runtime.eval(source), Ok(expected));
}
