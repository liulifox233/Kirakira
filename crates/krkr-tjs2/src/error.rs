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
}

impl TjsError {
    pub fn new(kind: TjsErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            span: None,
            message: message.into(),
        }
    }

    pub fn at(kind: TjsErrorKind, span: Span, message: impl Into<String>) -> Self {
        Self {
            kind,
            span: Some(span),
            message: message.into(),
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
}

impl fmt::Display for TjsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.span {
            Some(span) => write!(
                f,
                "{:?} error at {}..{}: {}",
                self.kind, span.start, span.end, self.message
            ),
            None => write!(f, "{:?} error: {}", self.kind, self.message),
        }
    }
}

impl std::error::Error for TjsError {}
