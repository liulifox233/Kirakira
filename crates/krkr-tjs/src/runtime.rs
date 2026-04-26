use crate::error::{RuntimeError, TjsResult};
use crate::value::Value;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ExecutionMode {
    #[default]
    Interpreter,
    Jit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeOptions {
    execution_mode: ExecutionMode,
    max_call_depth: usize,
}

impl RuntimeOptions {
    pub const DEFAULT_MAX_CALL_DEPTH: usize = 1024;

    pub const fn new() -> Self {
        Self {
            execution_mode: ExecutionMode::Interpreter,
            max_call_depth: Self::DEFAULT_MAX_CALL_DEPTH,
        }
    }

    pub const fn interpreter() -> Self {
        Self::new()
    }

    pub const fn jit() -> Self {
        Self {
            execution_mode: ExecutionMode::Jit,
            max_call_depth: Self::DEFAULT_MAX_CALL_DEPTH,
        }
    }

    pub const fn with_max_call_depth(mut self, max_call_depth: usize) -> Self {
        self.max_call_depth = max_call_depth;
        self
    }

    pub const fn execution_mode(self) -> ExecutionMode {
        self.execution_mode
    }

    pub const fn max_call_depth(self) -> usize {
        self.max_call_depth
    }
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Runtime {
    options: RuntimeOptions,
}

impl Runtime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_options(options: RuntimeOptions) -> Self {
        Self { options }
    }

    pub const fn options(&self) -> RuntimeOptions {
        self.options
    }

    pub fn eval(&mut self, source: &str) -> TjsResult<Value> {
        if source.trim().is_empty() {
            return Ok(Value::Void);
        }

        Err(RuntimeError::unsupported(
            "TJS2 parser and evaluator are not implemented yet",
        ))
    }
}
