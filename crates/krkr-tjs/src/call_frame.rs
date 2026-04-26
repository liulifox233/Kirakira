use crate::error::{RuntimeError, TjsResult};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallFrame {
    function_name: String,
}

impl CallFrame {
    pub fn new(function_name: impl Into<String>) -> Self {
        Self {
            function_name: function_name.into(),
        }
    }

    pub fn function_name(&self) -> &str {
        &self.function_name
    }
}

#[derive(Clone, Debug)]
pub struct CallFrameStack {
    max_depth: usize,
    frames: Vec<CallFrame>,
}

impl CallFrameStack {
    pub fn new(max_depth: usize) -> Self {
        Self {
            max_depth,
            frames: Vec::new(),
        }
    }

    pub fn push(&mut self, frame: CallFrame) -> TjsResult<()> {
        if self.frames.len() >= self.max_depth {
            return Err(RuntimeError::stack_overflow(self.max_depth));
        }

        self.frames.push(frame);
        Ok(())
    }

    pub fn pop(&mut self) -> Option<CallFrame> {
        self.frames.pop()
    }

    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    pub fn max_depth(&self) -> usize {
        self.max_depth
    }

    pub fn current(&self) -> Option<&CallFrame> {
        self.frames.last()
    }
}
