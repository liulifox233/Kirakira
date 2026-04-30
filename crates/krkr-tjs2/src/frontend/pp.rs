use crate::error::TjsErrorKind;

use super::diagnostic::{Diagnostic, FrontendOutput};
use super::lexer::lex;

pub fn validate(source: &str) -> FrontendOutput<()> {
    match lex(source) {
        Ok(_) => FrontendOutput::ok(()),
        Err(error) => FrontendOutput::new(
            None,
            vec![Diagnostic::error(
                TjsErrorKind::Lex,
                error.span,
                error.message,
            )],
        ),
    }
}
