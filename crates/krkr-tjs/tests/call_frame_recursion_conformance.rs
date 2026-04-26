use krkr_tjs::{CallFrame, CallFrameStack, Runtime, RuntimeErrorKind, Value};

#[test]
fn call_frame_stack_tracks_current_frame_and_depth() {
    let mut stack = CallFrameStack::new(8);

    stack.push(CallFrame::new("outer")).expect("push outer");
    stack.push(CallFrame::new("inner")).expect("push inner");

    assert_eq!(stack.depth(), 2);
    assert_eq!(stack.current().map(CallFrame::function_name), Some("inner"));
    assert_eq!(
        stack.pop().map(|frame| frame.function_name().to_owned()),
        Some("inner".to_owned())
    );
    assert_eq!(stack.current().map(CallFrame::function_name), Some("outer"));
}

#[test]
fn call_frame_stack_reports_overflow_at_configured_depth() {
    let mut stack = CallFrameStack::new(2);

    stack.push(CallFrame::new("a")).expect("push first frame");
    stack.push(CallFrame::new("b")).expect("push second frame");
    let error = stack
        .push(CallFrame::new("c"))
        .expect_err("third frame overflows");

    assert_eq!(error.kind(), &RuntimeErrorKind::StackOverflow);
    assert_eq!(stack.depth(), 2);
}

#[test]
#[ignore = "requires function declarations, calls, returns, and recursive execution"]
fn conformance_call_recursive_factorial_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/call/recursive_factorial.tjs"),
        Value::Integer(120),
    );
}

fn assert_source_returns(source: &str, expected: Value) {
    let mut runtime = Runtime::new();

    let actual = runtime.eval(source);

    assert_eq!(actual, Ok(expected));
}
