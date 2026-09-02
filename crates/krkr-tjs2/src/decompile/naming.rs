//! Register and identifier naming for decompiled code.
//!
//! Original local variable names are not stored in bytecode, so the
//! decompiler derives deterministic names from the VM register layout:
//! arguments become `a0..`, locals `l0..`, temporaries `t{n}` (temporaries
//! are usually inlined into expressions and only named when they survive
//! across statements).

use crate::bytecode::CodeObject;
use crate::frontend::syntax::{Expr, ExprKind, Ident};

pub(crate) struct Names {
    arg_count: usize,
}

impl Names {
    pub fn new(object: &CodeObject) -> Self {
        Self {
            arg_count: object.func_decl_arg_count as usize,
        }
    }

    /// Renders the identifier text for a register.
    pub fn name(&self, reg: i16) -> String {
        match reg {
            -1 => "this".to_string(),
            -2 => "this".to_string(),
            0 => "void".to_string(),
            r if r > 0 => format!("t{r}"),
            r => {
                let frame_index = (-3 - r) as usize;
                if frame_index < self.arg_count {
                    format!("a{frame_index}")
                } else {
                    format!("l{}", frame_index - self.arg_count)
                }
            }
        }
    }

    /// Builds an identifier expression for a register.
    pub fn ident_expr(&self, reg: i16) -> Expr {
        Expr::new(
            ExprKind::Identifier(Ident::new(self.name(reg))),
            crate::error::Span::empty(0),
        )
    }

    /// Builds a member read/write target for `object_register` with the given
    /// direct member name, applying the this/this-proxy rewrite that recovers
    /// the original source shape:
    ///
    /// - `%-2.*name` (this-proxy) is a bare identifier lookup: print `name`.
    /// - `%-1.*name` (this) is `this.name`.
    /// - any other object is `object.name`.
    ///
    /// `at_top_level` rewrites `%-1` accesses to bare identifiers too, since
    /// at the top level `this` *is* the global object and original source
    /// refers to globals by bare name.
    pub fn member_target(&self, object_reg: i16, name: &str, at_top_level: bool) -> Expr {
        match object_reg {
            -2 => self.ident_expr_name(name),
            -1 if at_top_level => self.ident_expr_name(name),
            -1 => Expr::new(
                ExprKind::Member {
                    object: Box::new(self.ident_expr(-1)),
                    property: name.to_string(),
                },
                crate::error::Span::empty(0),
            ),
            _ => Expr::new(
                ExprKind::Member {
                    object: Box::new(self.ident_expr(object_reg)),
                    property: name.to_string(),
                },
                crate::error::Span::empty(0),
            ),
        }
    }

    fn ident_expr_name(&self, name: &str) -> Expr {
        Expr::new(
            ExprKind::Identifier(Ident::new(name)),
            crate::error::Span::empty(0),
        )
    }
}

/// Whether a member name is a valid TJS2 identifier that can follow a dot.
pub(crate) fn is_ident_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first == '$' || first.is_alphabetic()) {
        return false;
    }
    chars.all(|ch| ch == '_' || ch == '$' || ch.is_alphanumeric())
}
