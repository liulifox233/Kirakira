use std::time::{Duration, Instant};

use krkr_tjs::{Bytecode, Instruction, Value, Vm};

fn main() {
    let iterations = 10_000;
    let bytecode = Bytecode::new(vec![
        Instruction::Push(Value::Integer(20)),
        Instruction::Push(Value::Integer(22)),
        Instruction::Add,
        Instruction::Return,
    ]);
    let elapsed = run_add_smoke(iterations, &bytecode);

    println!(
        "tjs_vm_add_smoke: {iterations} iterations in {:?} ({:?}/iter)",
        elapsed,
        elapsed / iterations
    );
}

fn run_add_smoke(iterations: u32, bytecode: &Bytecode) -> Duration {
    let start = Instant::now();
    let mut vm = Vm::new();

    for _ in 0..iterations {
        let value = vm.execute(bytecode).expect("benchmark bytecode should run");
        assert_eq!(value, Value::Integer(42));
    }

    start.elapsed()
}
