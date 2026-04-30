use crate::error::{Span, TjsError, TjsErrorKind};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrontendOptions {
    pub recover: bool,
}

impl Default for FrontendOptions {
    fn default() -> Self {
        Self { recover: true }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrontendOutput<T> {
    pub value: Option<T>,
    pub diagnostics: Vec<Diagnostic>,
}

impl<T> FrontendOutput<T> {
    pub fn new(value: Option<T>, diagnostics: Vec<Diagnostic>) -> Self {
        Self { value, diagnostics }
    }

    pub fn ok(value: T) -> Self {
        Self {
            value: Some(value),
            diagnostics: Vec::new(),
        }
    }

    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub severity: DiagnosticSeverity,
    pub kind: TjsErrorKind,
    pub span: Option<Span>,
    pub source_name: Option<String>,
    pub start: Option<SourceLocation>,
    pub end: Option<SourceLocation>,
    pub message: String,
}

impl Diagnostic {
    pub fn error(kind: TjsErrorKind, span: Option<Span>, message: impl Into<String>) -> Self {
        Self {
            severity: DiagnosticSeverity::Error,
            kind,
            span,
            source_name: None,
            start: None,
            end: None,
            message: message.into(),
        }
    }

    pub fn warning(kind: TjsErrorKind, span: Option<Span>, message: impl Into<String>) -> Self {
        Self {
            severity: DiagnosticSeverity::Warning,
            kind,
            span,
            source_name: None,
            start: None,
            end: None,
            message: message.into(),
        }
    }

    pub fn with_source_locations(mut self, source_name: &str, source: &str) -> Self {
        self.source_name = Some(source_name.to_string());
        if let Some(span) = self.span {
            let source_map = SourceMap::new(source);
            self.start = Some(source_map.location(span.start));
            self.end = Some(source_map.location(span.end));
        }
        self
    }
}

impl From<TjsError> for Diagnostic {
    fn from(error: TjsError) -> Self {
        Self::error(error.kind, error.span, error.message)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceLocation {
    pub line: usize,
    pub column: usize,
}

pub fn attach_source_locations(
    diagnostics: Vec<Diagnostic>,
    source_name: &str,
    source: &str,
) -> Vec<Diagnostic> {
    let source_map = SourceMap::new(source);
    diagnostics
        .into_iter()
        .map(|mut diagnostic| {
            diagnostic.source_name = Some(source_name.to_string());
            if let Some(span) = diagnostic.span {
                diagnostic.start = Some(source_map.location(span.start));
                diagnostic.end = Some(source_map.location(span.end));
            }
            diagnostic
        })
        .collect()
}

struct SourceMap<'a> {
    source: &'a str,
    line_starts: Vec<usize>,
}

impl<'a> SourceMap<'a> {
    fn new(source: &'a str) -> Self {
        let mut line_starts = vec![0];
        for (offset, ch) in source.char_indices() {
            if ch == '\n' {
                line_starts.push(offset + ch.len_utf8());
            }
        }
        Self {
            source,
            line_starts,
        }
    }

    fn location(&self, offset: usize) -> SourceLocation {
        let offset = offset.min(self.source.len());
        let line_index = match self.line_starts.binary_search(&offset) {
            Ok(index) => index,
            Err(index) => index.saturating_sub(1),
        };
        let line_start = self.line_starts[line_index];
        let column = self.source[line_start..offset].chars().count() + 1;
        SourceLocation {
            line: line_index + 1,
            column,
        }
    }
}
