use std::fmt;

pub type Result<T> = std::result::Result<T, TjsError>;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub const fn empty(offset: usize) -> Self {
        Self {
            start: offset,
            end: offset,
        }
    }

    pub fn join(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TjsErrorKind {
    Lex,
    Parse,
    Mir,
    Bytecode,
    Verify,
    Runtime,
    Codegen,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TjsError {
    pub kind: TjsErrorKind,
    pub span: Option<Span>,
    pub message: String,
    pub contexts: Vec<TjsErrorContext>,
}

impl TjsError {
    pub fn new(kind: TjsErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            span: None,
            message: message.into(),
            contexts: Vec::new(),
        }
    }

    pub fn at(kind: TjsErrorKind, span: Span, message: impl Into<String>) -> Self {
        Self {
            kind,
            span: Some(span),
            message: message.into(),
            contexts: Vec::new(),
        }
    }

    pub fn lex(span: Span, message: impl Into<String>) -> Self {
        Self::at(TjsErrorKind::Lex, span, message)
    }

    pub fn parse(span: Span, message: impl Into<String>) -> Self {
        Self::at(TjsErrorKind::Parse, span, message)
    }

    pub fn mir(message: impl Into<String>) -> Self {
        Self::new(TjsErrorKind::Mir, message)
    }

    pub fn bytecode(message: impl Into<String>) -> Self {
        Self::new(TjsErrorKind::Bytecode, message)
    }

    pub fn verify(message: impl Into<String>) -> Self {
        Self::new(TjsErrorKind::Verify, message)
    }

    pub fn runtime(message: impl Into<String>) -> Self {
        Self::new(TjsErrorKind::Runtime, message)
    }

    pub fn codegen(message: impl Into<String>) -> Self {
        Self::new(TjsErrorKind::Codegen, message)
    }

    pub fn with_context(mut self, context: TjsErrorContext) -> Self {
        self.contexts.push(context);
        self
    }

    pub fn with_stack_frame(self, frame: TjsStackFrame) -> Self {
        self.with_context(TjsErrorContext::StackFrame(frame))
    }

    pub fn with_member_access(self, access: TjsMemberAccess) -> Self {
        self.with_context(TjsErrorContext::MemberAccess(access))
    }
}

impl fmt::Display for TjsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const MAX_CONTEXTS: usize = 24;
        match self.span {
            Some(span) => write!(
                f,
                "{:?} error at {}..{}: {}",
                self.kind, span.start, span.end, self.message
            ),
            None => write!(f, "{:?} error: {}", self.kind, self.message),
        }?;
        for context in self.contexts.iter().take(MAX_CONTEXTS) {
            write!(f, "\n  {context}")?;
        }
        if self.contexts.len() > MAX_CONTEXTS {
            write!(
                f,
                "\n  ... {} more context entries omitted",
                self.contexts.len() - MAX_CONTEXTS
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for TjsError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TjsErrorContext {
    StackFrame(TjsStackFrame),
    MemberAccess(TjsMemberAccess),
}

impl fmt::Display for TjsErrorContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StackFrame(frame) => write!(f, "at {frame}"),
            Self::MemberAccess(access) => write!(f, "while {access}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TjsStackFrame {
    pub storage: Option<String>,
    pub object_name: String,
    pub context: String,
    pub bytecode_offset: usize,
    pub source: Option<TjsSourceLocation>,
}

impl fmt::Display for TjsStackFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(storage) = &self.storage {
            write!(f, "{storage}:")?;
        }
        write!(
            f,
            "{} [{}] bytecode {}",
            self.object_name, self.context, self.bytecode_offset
        )?;
        if let Some(source) = &self.source {
            write!(f, " ({source})")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TjsSourceLocation {
    pub storage: Option<String>,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub utf16_offset: Option<usize>,
}

impl fmt::Display for TjsSourceLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(storage) = &self.storage {
            write!(f, "{storage}")?;
        } else {
            write!(f, "source")?;
        }
        match (self.line, self.column) {
            (Some(line), Some(column)) => write!(f, ":{line}:{column}"),
            _ => match self.utf16_offset {
                Some(offset) => write!(f, " utf16-offset {offset}"),
                None => Ok(()),
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TjsMemberAccess {
    pub operation: TjsMemberOperation,
    pub receiver_type: String,
    pub member_name: String,
    pub callee_type: Option<String>,
}

impl fmt::Display for TjsMemberAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} member `{}` on {}",
            self.operation, self.member_name, self.receiver_type
        )?;
        if let Some(callee_type) = &self.callee_type {
            write!(f, " with callee {callee_type}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TjsMemberOperation {
    Getting,
    Setting,
    Calling,
    Deleting,
}

impl fmt::Display for TjsMemberOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Getting => write!(f, "getting"),
            Self::Setting => write!(f, "setting"),
            Self::Calling => write!(f, "calling"),
            Self::Deleting => write!(f, "deleting"),
        }
    }
}
