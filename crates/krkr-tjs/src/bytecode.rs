use crate::error::{RuntimeError, TjsResult};
use crate::value::{Value, add_values};

#[derive(Clone, Debug, PartialEq)]
pub enum Instruction {
    Push(Value),
    Add,
    Return,
    Throw(Value),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bytecode {
    instructions: Vec<Instruction>,
}

impl Bytecode {
    pub fn new(instructions: Vec<Instruction>) -> Self {
        Self { instructions }
    }

    pub fn instructions(&self) -> &[Instruction] {
        &self.instructions
    }

    pub fn is_empty(&self) -> bool {
        self.instructions.is_empty()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Vm {
    stack: Vec<Value>,
}

impl Vm {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn execute(&mut self, bytecode: &Bytecode) -> TjsResult<Value> {
        self.stack.clear();

        for instruction in bytecode.instructions() {
            match instruction {
                Instruction::Push(value) => self.stack.push(value.clone()),
                Instruction::Add => {
                    let rhs = self
                        .stack
                        .pop()
                        .ok_or_else(|| RuntimeError::stack_underflow("Add"))?;
                    let lhs = self
                        .stack
                        .pop()
                        .ok_or_else(|| RuntimeError::stack_underflow("Add"))?;
                    self.stack.push(add_values(&lhs, &rhs)?);
                }
                Instruction::Return => {
                    return Ok(self.stack.pop().unwrap_or(Value::Void));
                }
                Instruction::Throw(value) => {
                    return Err(RuntimeError::exception(value.clone()));
                }
            }
        }

        Ok(Value::Void)
    }

    pub fn stack_len(&self) -> usize {
        self.stack.len()
    }
}
