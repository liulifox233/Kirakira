use std::{collections::BTreeMap, error::Error, fmt};

pub type TjsResult<T> = Result<T, RuntimeError>;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Void,
    Null,
    Boolean(bool),
    Integer(i64),
    Real(f64),
    String(String),
    Object(Box<ObjectValue>),
}

impl Value {
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::Void => "void",
            Self::Null => "null",
            Self::Boolean(_) => "boolean",
            Self::Integer(_) => "integer",
            Self::Real(_) => "real",
            Self::String(_) => "string",
            Self::Object(_) => "object",
        }
    }

    pub fn to_tjs_string(&self) -> String {
        match self {
            Self::Void => "void".to_owned(),
            Self::Null => "null".to_owned(),
            Self::Boolean(value) => value.to_string(),
            Self::Integer(value) => value.to_string(),
            Self::Real(value) => value.to_string(),
            Self::String(value) => value.clone(),
            Self::Object(_) => "[object Object]".to_owned(),
        }
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Self::Boolean(value)
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

impl From<i32> for Value {
    fn from(value: i32) -> Self {
        Self::Integer(i64::from(value))
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Self::Real(value)
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<ObjectValue> for Value {
    fn from(value: ObjectValue) -> Self {
        Self::Object(Box::new(value))
    }
}

pub fn add_values(lhs: &Value, rhs: &Value) -> TjsResult<Value> {
    match (lhs, rhs) {
        (Value::String(_), _) | (_, Value::String(_)) => Ok(Value::String(format!(
            "{}{}",
            lhs.to_tjs_string(),
            rhs.to_tjs_string()
        ))),
        (Value::Integer(lhs), Value::Integer(rhs)) => lhs
            .checked_add(*rhs)
            .map(Value::Integer)
            .ok_or_else(|| RuntimeError::type_error("integer addition overflowed")),
        (Value::Integer(lhs), Value::Real(rhs)) => Ok(Value::Real(*lhs as f64 + rhs)),
        (Value::Real(lhs), Value::Integer(rhs)) => Ok(Value::Real(lhs + *rhs as f64)),
        (Value::Real(lhs), Value::Real(rhs)) => Ok(Value::Real(lhs + rhs)),
        _ => Err(RuntimeError::type_error(format!(
            "operator + is not supported for {} and {}",
            lhs.type_name(),
            rhs.type_name()
        ))),
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ObjectValue {
    properties: BTreeMap<String, Value>,
}

impl ObjectValue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_property(&mut self, name: impl Into<String>, value: Value) -> Option<Value> {
        self.properties.insert(name.into(), value)
    }

    pub fn get_property(&self, name: &str) -> Option<&Value> {
        self.properties.get(name)
    }

    pub fn has_property(&self, name: &str) -> bool {
        self.properties.contains_key(name)
    }

    pub fn property_count(&self) -> usize {
        self.properties.len()
    }

    pub fn property_names(&self) -> impl Iterator<Item = &str> {
        self.properties.keys().map(String::as_str)
    }
}

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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Kag3BootPlan {
    pub exec_storage: Vec<String>,
    pub load_scripts: Vec<String>,
    pub process_scenarios: Vec<String>,
}

impl Kag3BootPlan {
    pub fn is_empty(&self) -> bool {
        self.exec_storage.is_empty()
            && self.load_scripts.is_empty()
            && self.process_scenarios.is_empty()
    }
}

pub fn scan_kag3_boot_plan(source: &str) -> Kag3BootPlan {
    Kag3BootPlan {
        exec_storage: scan_string_call_arguments(source, "Scripts.execStorage"),
        load_scripts: scan_string_call_arguments(source, "KAGLoadScript"),
        process_scenarios: scan_string_call_arguments(source, "kag.process"),
    }
}

fn scan_string_call_arguments(source: &str, callee: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut cursor = 0;

    while let Some(relative_position) = source[cursor..].find(callee) {
        cursor += relative_position + callee.len();
        if let Some(argument) = extract_first_string_argument(&source[cursor..]) {
            arguments.push(argument);
        }
    }

    arguments
}

fn extract_first_string_argument(source: &str) -> Option<String> {
    let source = source.trim_start();
    let source = source.strip_prefix('(')?.trim_start();
    let quote = source.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }

    let mut value = String::new();
    let mut escaped = false;
    for character in source[quote.len_utf8()..].chars() {
        if escaped {
            value.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == quote {
            return Some(value);
        } else {
            value.push(character);
        }
    }

    None
}

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
