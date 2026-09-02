//! Text emission: renders the reconstructed program and post-processes
//! unhandled markers into `// <unhandled: ...>` comments.

use crate::frontend::printer::print_program;
use crate::frontend::syntax::Program;

#[derive(Clone, Debug, Default)]
pub struct DecompileStats {
    /// Number of code objects that were decompiled.
    pub objects: usize,
    /// Number of unhandled bytecode fragments.
    pub unhandled: usize,
}

#[derive(Clone, Debug)]
pub struct DecompiledSource {
    pub name: String,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct DecompileOutput {
    pub sources: Vec<DecompiledSource>,
    pub stats: DecompileStats,
}

/// Renders a program, replacing unhandled marker statements with comments.
pub(crate) fn render_program(program: &Program, name: &str) -> String {
    let body = print_program(program);
    let mut out = String::with_capacity(body.len() + 64);
    out.push_str(&format!("// decompiled from {name}\n"));
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(index) = trimmed
            .strip_prefix("__krkr_decomp_object_")
            .and_then(|rest| rest.strip_suffix(';'))
        {
            let indent = &line[..line.len() - trimmed.len()];
            out.push_str(&format!(
                "{indent}// <unhandled: code object {index} body>\n"
            ));
        } else if let Some(marker) = trimmed
            .strip_prefix("__krkr_decomp_unhandled_")
            .and_then(|rest| rest.strip_suffix(';'))
        {
            // The marker carries the sanitized reason after the hash.
            let reason = marker
                .split_once('_')
                .map(|(_, reason)| reason.replace('_', " "))
                .unwrap_or_default();
            let reason = reason.trim();
            let indent = &line[..line.len() - trimmed.len()];
            if reason.is_empty() {
                out.push_str(&format!("{indent}// <unhandled: bytecode fragment>\n"));
            } else {
                out.push_str(&format!("{indent}// <unhandled: {reason}>\n"));
            }
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_markers_with_comments() {
        let program =
            crate::compiler::parse_source("__krkr_decomp_unhandled_abcd; __krkr_decomp_object_3;")
                .expect("parse");
        let text = render_program(&program, "test.tjs");
        assert!(text.contains("// <unhandled: bytecode fragment>"), "{text}");
        assert!(
            text.contains("// <unhandled: code object 3 body>"),
            "{text}"
        );
    }
}
