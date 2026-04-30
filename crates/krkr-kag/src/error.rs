use std::{error::Error, fmt};

use crate::source::SourceSpan;

pub type Result<T> = std::result::Result<T, KagError>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KagError {
    NoScenario,
    ScenarioLoadUnsupported {
        storage: String,
    },
    ScenarioNotLoaded {
        storage: String,
    },
    LabelNotFound {
        storage: String,
        label: String,
    },
    MissingAttribute {
        tag: String,
        attribute: String,
    },
    ReturnStackEmpty,
    ReturnLostSync {
        storage: String,
    },
    MacroDepthExceeded {
        limit: usize,
    },
    Parse {
        storage: Option<String>,
        span: Option<SourceSpan>,
        message: String,
    },
    EvalUnsupported {
        expression: String,
    },
    Host {
        message: String,
    },
}

impl KagError {
    pub fn parse(message: impl Into<String>) -> Self {
        Self::Parse {
            storage: None,
            span: None,
            message: message.into(),
        }
    }

    pub fn parse_at(
        storage: impl Into<String>,
        span: SourceSpan,
        message: impl Into<String>,
    ) -> Self {
        Self::Parse {
            storage: Some(storage.into()),
            span: Some(span),
            message: message.into(),
        }
    }

    pub fn host(message: impl Into<String>) -> Self {
        Self::Host {
            message: message.into(),
        }
    }
}

impl fmt::Display for KagError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoScenario => write!(f, "no KAG scenario is loaded"),
            Self::ScenarioLoadUnsupported { storage } => {
                write!(f, "scenario loading is not available for {storage:?}")
            }
            Self::ScenarioNotLoaded { storage } => {
                write!(f, "KAG scenario is not loaded: {storage}")
            }
            Self::LabelNotFound { storage, label } => {
                write!(f, "label {label:?} was not found in scenario {storage:?}")
            }
            Self::MissingAttribute { tag, attribute } => {
                write!(f, "tag {tag:?} requires attribute {attribute:?}")
            }
            Self::ReturnStackEmpty => write!(f, "return tag used with an empty call stack"),
            Self::ReturnLostSync { storage } => {
                write!(
                    f,
                    "return target in scenario {storage:?} no longer matches the call site"
                )
            }
            Self::MacroDepthExceeded { limit } => {
                write!(f, "KAG macro expansion exceeded depth limit {limit}")
            }
            Self::Parse {
                storage,
                span,
                message,
            } => match (storage, span) {
                (Some(storage), Some(span)) => write!(
                    f,
                    "parse error in {storage} at {}..{}: {message}",
                    span.start, span.end
                ),
                (Some(storage), None) => write!(f, "parse error in {storage}: {message}"),
                (None, Some(span)) => {
                    write!(f, "parse error at {}..{}: {message}", span.start, span.end)
                }
                (None, None) => write!(f, "parse error: {message}"),
            },
            Self::EvalUnsupported { expression } => {
                write!(
                    f,
                    "KAG expression evaluation is not available: {expression:?}"
                )
            }
            Self::Host { message } => write!(f, "{message}"),
        }
    }
}

impl Error for KagError {}
