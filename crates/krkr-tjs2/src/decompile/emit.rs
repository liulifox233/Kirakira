//! Text emission: renders the reconstructed program and post-processes
//! unhandled markers into comments — `// <unhandled: ...>` when the marker is
//! the whole line, `/* <unhandled: ...> */` when it shares its line with code
//! (bodies the printer renders inline). The identifier must not survive: it
//! reads an undefined variable, so evaluating it throws `MemberNotFound`, and
//! a `//` form on a shared line would comment out the code after the marker.

use crate::frontend::printer::print_program;
use crate::frontend::syntax::Program;

#[derive(Clone, Debug, Default)]
pub struct DecompileStats {
    /// Number of code objects that were decompiled.
    pub objects: usize,
    /// Number of unhandled bytecode fragments (constructs no pattern covers).
    pub unhandled: usize,
    /// Number of dropped regions: reachable bytecode the walk abandoned,
    /// marked in the output with its byte range as `// <unhandled: dropped
    /// region ...>` (or `/* <unhandled: ...> */` where the marker shares its
    /// line with code). Counted apart from [`Self::unhandled`] so the
    /// pattern-gap measure keeps its meaning (the fuzz corpus's completeness
    /// net).
    pub dropped_regions: usize,
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

/// Prefixes of the placeholder identifiers the decompiler emits for
/// bytecode it could not reconstruct as statements.
const UNHANDLED_MARKER: &str = "__krkr_decomp_unhandled_";
const OBJECT_MARKER: &str = "__krkr_decomp_object_";

/// Renders a program, replacing unhandled marker statements with comments.
///
/// A marker does not always occupy its whole line: the printer renders some
/// bodies inline (function-literal registration expressions), and there a
/// marker shares the line with real code. A whole-line marker becomes a
/// `// <unhandled: ...>` comment (the historical form); an inline one becomes
/// a `/* <unhandled: ...> */` block comment, because leaving the identifier
/// as-is emits live code that throws `MemberNotFound` when the expression
/// around it is evaluated, and a `//` comment would swallow the code that
/// follows the marker on the same line.
pub(crate) fn render_program(program: &Program, name: &str) -> String {
    let body = print_program(program);
    let mut out = String::with_capacity(body.len() + 64);
    out.push_str(&format!("// decompiled from {name}\n"));
    for line in body.lines() {
        render_line(line, &mut out);
    }
    out
}

/// Renders one printed line into `out`, substituting every marker on it.
fn render_line(line: &str, out: &mut String) {
    let trimmed = line.trim();
    let indent = &line[..line.len() - trimmed.len()];
    let bare = trimmed.strip_suffix(';').unwrap_or(trimmed);
    if let Some(reason) = marker_reason(bare)
        && bare.len() == marker_token_end(bare, 0)
    {
        // The marker statement is the whole line: keep the readable comment
        // form the decompiler has always produced.
        out.push_str(&format!("{indent}// <unhandled: {reason}>\n"));
        return;
    }
    let mut cursor = 0;
    while let Some(start) = next_marker_start(line, cursor) {
        let end = marker_token_end(line, start);
        let token = line[start..end]
            .strip_suffix(';')
            .unwrap_or(&line[start..end]);
        let reason = marker_reason(token).unwrap_or_else(|| "bytecode fragment".to_string());
        out.push_str(&line[cursor..start]);
        out.push_str(&format!("/* <unhandled: {reason}> */"));
        cursor = end;
    }
    out.push_str(&line[cursor..]);
    out.push('\n');
}

/// The `// <unhandled: ...>` reason of a marker identifier, or `None` when
/// `token` is not a marker identifier.
fn marker_reason(token: &str) -> Option<String> {
    if let Some(index) = token.strip_prefix(OBJECT_MARKER) {
        return Some(format!("code object {index} body"));
    }
    let rest = token.strip_prefix(UNHANDLED_MARKER)?;
    // The marker carries the sanitized reason after the hash.
    let reason = rest
        .split_once('_')
        .map(|(_, reason)| reason.replace('_', " "))
        .unwrap_or_default();
    let reason = reason.trim();
    Some(if reason.is_empty() {
        "bytecode fragment".to_string()
    } else {
        reason.to_string()
    })
}

/// Offset of the earliest marker identifier at or after `from`.
fn next_marker_start(line: &str, from: usize) -> Option<usize> {
    let rest = line.get(from..)?;
    let unhandled = rest.find(UNHANDLED_MARKER);
    let object = rest.find(OBJECT_MARKER);
    let offset = match (unhandled, object) {
        (Some(unhandled), Some(object)) => unhandled.min(object),
        (Some(unhandled), None) => unhandled,
        (None, Some(object)) => object,
        (None, None) => return None,
    };
    Some(from + offset)
}

/// Offset just past the marker identifier starting at `start`, including a
/// directly attached `;` (the marker statement's terminator).
fn marker_token_end(line: &str, start: usize) -> usize {
    let bytes = line.as_bytes();
    let mut end = start;
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }
    if end < bytes.len() && bytes[end] == b';' {
        end += 1;
    }
    end
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

    /// A marker the printer places after other code on its line (what the
    /// rendered bodies of function-literal registration expressions do) must
    /// not survive as an executable identifier: evaluated, it throws
    /// `MemberNotFound`, and `--verify` cannot see that (the identifier is
    /// valid syntax). The line below is a real one, from PARQUET's
    /// `system/Initialize.tjs` before the dropped-region markers were
    /// token-substituted.
    #[test]
    fn replaces_inline_markers_with_block_comments() {
        let line = "} __krkr_decomp_unhandled_6939a59a18bd95ad_dropped_region_bytecode_0xd6_to_0xdc_3_instructions_never_reconstructed; } incontextof global;";
        let mut out = String::new();
        render_line(line, &mut out);
        assert!(!out.contains("__krkr_decomp_unhandled_"), "{out}");
        assert!(
            out.contains(
                "/* <unhandled: dropped region bytecode 0xd6 to 0xdc 3 instructions never reconstructed> */"
            ),
            "{out}"
        );
        assert!(out.contains("} incontextof global;"), "{out}");
        assert!(out.starts_with("} "), "{out}");
    }

    /// The whole-line form stays the readable `//` comment it has always
    /// been, and code lines with no marker are passed through untouched.
    #[test]
    fn renders_whole_line_markers_and_plain_lines_unchanged() {
        let mut out = String::new();
        render_line(
            "    __krkr_decomp_unhandled_0000000000000000_control_flow;",
            &mut out,
        );
        assert_eq!(out, "    // <unhandled: control flow>\n");
        let mut out = String::new();
        render_line("    hit = hit + 2;", &mut out);
        assert_eq!(out, "    hit = hit + 2;\n");
    }
}
