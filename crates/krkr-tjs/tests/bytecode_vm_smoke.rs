use krkr_tjs::{Bytecode, Instruction, Value, Vm};

#[test]
fn bytecode_vm_returns_pushed_constant() {
    let bytecode = Bytecode::new(vec![
        Instruction::Push(Value::Integer(42)),
        Instruction::Return,
    ]);
    let mut vm = Vm::new();

    let result = vm.execute(&bytecode).expect("execute bytecode");

    assert_eq!(result, Value::Integer(42));
    assert_eq!(vm.stack_len(), 0);
}

#[test]
fn bytecode_vm_adds_top_two_stack_values() {
    let bytecode = Bytecode::new(vec![
        Instruction::Push(Value::Integer(40)),
        Instruction::Push(Value::Integer(2)),
        Instruction::Add,
        Instruction::Return,
    ]);
    let mut vm = Vm::new();

    let result = vm.execute(&bytecode).expect("execute bytecode");

    assert_eq!(result, Value::Integer(42));
}

#[test]
#[ignore = "requires bytecode format, locals, and jumps for compiled TJS2 functions"]
fn conformance_bytecode_vm_executes_compiled_function_fixture() {
    let source = include_str!("fixtures/conformance/call/recursive_factorial.tjs");

    assert!(!source.trim().is_empty());
    panic!("compile-to-bytecode is not implemented yet");
}
