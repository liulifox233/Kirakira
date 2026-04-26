use krkr_tjs::{Bytecode, Instruction, Runtime, RuntimeErrorKind, Value, Vm};

#[test]
fn bytecode_vm_throw_returns_uncaught_exception_error() {
    let bytecode = Bytecode::new(vec![Instruction::Throw(Value::from("boom"))]);
    let mut vm = Vm::new();

    let error = vm
        .execute(&bytecode)
        .expect_err("throw should fail without a handler");

    assert_eq!(error.kind(), &RuntimeErrorKind::Exception);
    assert_eq!(error.thrown(), Some(&Value::from("boom")));
}

#[test]
fn bytecode_vm_stack_underflow_reports_instruction_name() {
    let bytecode = Bytecode::new(vec![Instruction::Add]);
    let mut vm = Vm::new();

    let error = vm
        .execute(&bytecode)
        .expect_err("add without operands should fail");

    assert_eq!(error.kind(), &RuntimeErrorKind::StackUnderflow);
    assert!(error.message().contains("Add"));
}

#[test]
#[ignore = "requires TJS2 try/catch/finally control flow and exception objects"]
fn conformance_exception_try_catch_finally_fixture() {
    assert_source_returns(
        include_str!("fixtures/conformance/exception/try_catch_finally.tjs"),
        Value::Integer(3),
    );
}

fn assert_source_returns(source: &str, expected: Value) {
    let mut runtime = Runtime::new();

    let actual = runtime.eval(source);

    assert_eq!(actual, Ok(expected));
}
