use krkr_tjs::{Bytecode, Instruction, RuntimeErrorKind, Value, Vm};

#[test]
fn bytecode_empty_program_returns_void() {
    let mut vm = Vm::new();

    assert_eq!(vm.execute(&Bytecode::default()), Ok(Value::Void));
}

#[test]
fn bytecode_return_without_value_returns_void() {
    let bytecode = Bytecode::new(vec![Instruction::Return]);
    let mut vm = Vm::new();

    assert_eq!(vm.execute(&bytecode), Ok(Value::Void));
}

#[test]
fn bytecode_vm_clears_stack_between_executions() {
    let mut vm = Vm::new();
    let first = Bytecode::new(vec![Instruction::Push(Value::Integer(1))]);
    let second = Bytecode::new(vec![Instruction::Return]);

    assert_eq!(vm.execute(&first), Ok(Value::Void));
    assert_eq!(vm.stack_len(), 1);
    assert_eq!(vm.execute(&second), Ok(Value::Void));
    assert_eq!(vm.stack_len(), 0);
}

#[test]
fn bytecode_add_with_one_operand_reports_stack_underflow() {
    let bytecode = Bytecode::new(vec![Instruction::Push(Value::Integer(1)), Instruction::Add]);
    let mut vm = Vm::new();

    let error = vm.execute(&bytecode).expect_err("missing lhs should fail");

    assert_eq!(error.kind(), &RuntimeErrorKind::StackUnderflow);
    assert_eq!(vm.stack_len(), 0);
}

#[test]
fn bytecode_instruction_slice_exposes_stable_program_order() {
    let bytecode = Bytecode::new(vec![
        Instruction::Push(Value::Integer(1)),
        Instruction::Push(Value::Integer(2)),
        Instruction::Add,
    ]);

    assert_eq!(
        bytecode.instructions(),
        [
            Instruction::Push(Value::Integer(1)),
            Instruction::Push(Value::Integer(2)),
            Instruction::Add,
        ]
    );
    assert!(!bytecode.is_empty());
}
