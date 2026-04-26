use std::{error::Error, fmt};

use crate::value::Value;

pub type TjsResult<T> = Result<T, RuntimeError>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeErrorKind {
    Unsupported,
    TypeError,
    StackOverflow,
    StackUnderflow,
    Exception,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeError {
    kind: RuntimeErrorKind,
    message: String,
    thrown: Option<Value>,
}

impl RuntimeError {
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self {
            kind: RuntimeErrorKind::Unsupported,
            message: message.into(),
            thrown: None,
        }
    }

    pub fn type_error(message: impl Into<String>) -> Self {
        Self {
            kind: RuntimeErrorKind::TypeError,
            message: message.into(),
            thrown: None,
        }
    }

    pub fn stack_overflow(max_depth: usize) -> Self {
        Self {
            kind: RuntimeErrorKind::StackOverflow,
            message: format!("call stack exceeded maximum depth {max_depth}"),
            thrown: None,
        }
    }

    pub fn stack_underflow(operation: &'static str) -> Self {
        Self {
            kind: RuntimeErrorKind::StackUnderflow,
            message: format!("stack underflow while executing {operation}"),
            thrown: None,
        }
    }

    pub fn exception(value: Value) -> Self {
        Self {
            kind: RuntimeErrorKind::Exception,
            message: "uncaught TJS exception".to_owned(),
            thrown: Some(value),
        }
    }

    pub const fn kind(&self) -> &RuntimeErrorKind {
        &self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn thrown(&self) -> Option<&Value> {
        self.thrown.as_ref()
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl Error for RuntimeError {}
