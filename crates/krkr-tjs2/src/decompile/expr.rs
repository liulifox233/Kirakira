//! Expression construction helpers for the decompiler.
//!
//! These build `syntax::Expr` nodes from bytecode data slots and member
//! access operands, applying the this/this-proxy rewrites that recover the
//! original source shape.

use crate::bytecode::{BytecodeContextType, BytecodeFile, CodeObject, DataSlot};
use crate::error::{Result, Span};
use crate::frontend::syntax::{self, Expr, ExprKind};

use super::naming::Names;

/// Builds the literal expression for a `const` data-slot operand.
///
/// `InterObject` slots wrapping function objects decompile in place as
/// anonymous function literals; class objects keep an index placeholder that
/// the registration lifter replaces with a real declaration.
pub(crate) fn data_slot_expr(
    file: &BytecodeFile,
    object: &CodeObject,
    slot_index: i16,
    names: &Names,
) -> Result<Expr> {
    let slot = object
        .data_slots
        .get(usize::try_from(slot_index).map_err(|_| {
            crate::error::TjsError::bytecode(format!("negative data slot index {slot_index}"))
        })?)
        .ok_or_else(|| crate::error::TjsError::bytecode(format!("data slot {slot_index} missing")))?;
    data_slot_expr_inner(file, slot, names)
}

fn data_slot_expr_inner(file: &BytecodeFile, slot: &DataSlot, names: &Names) -> Result<Expr> {
    let expr = match slot.value(file) {
        Ok(value) => match value {
            crate::runtime::Variant::Void => ExprKind::Void,
            crate::runtime::Variant::Null => ExprKind::Null,
            crate::runtime::Variant::Integer(value) => ExprKind::Integer(value),
            crate::runtime::Variant::Real(value) => ExprKind::Real(value),
            crate::runtime::Variant::String(value) => ExprKind::String(value),
            crate::runtime::Variant::Octet(value) => ExprKind::Octet(value),
            crate::runtime::Variant::CodeObject(object_index) => {
                let Some(object) = file.objects.get(object_index) else {
                    return Err(crate::error::TjsError::bytecode(format!(
                        "code object {object_index} missing"
                    )));
                };
                match object.context_type {
                    // Function literals decompile in place: the InterObject
                    // slot wraps a code object whose header carries the
                    // parameter list and whose code is the body.
                    BytecodeContextType::Function | BytecodeContextType::ExprFunction => {
                        let Some(_guard) = super::DecompileChainGuard::enter(object_index) else {
                            return Ok(function_placeholder(object_index));
                        };
                        return Ok(Expr::new(
                            ExprKind::Function(Box::new(super::skeleton::function_decl(
                                file, object, None,
                            )?)),
                            Span::empty(0),
                        ));
                    }
                    // Class literals keep the object-index placeholder so the
                    // registration lifter can reconstruct the declaration
                    // from the object table.
                    _ => return Ok(function_placeholder(object_index)),
                }
            }
            _ => {
                return Err(crate::error::TjsError::bytecode(
                    "unsupported data slot value for literal",
                ));
            }
        },
        Err(_) => {
            // Unsupported/unknown slot types become a placeholder reference
            // so output stays syntactically valid.
            ExprKind::Identifier(syntax::Ident::new(
                super::stmt::unhandled_marker("unsupported constant"),
            ))
        }
    };
    let _ = names;
    Ok(Expr::new(expr, Span::empty(0)))
}

/// The placeholder for a code object whose body cannot be decompiled here
/// (cycle guard, class objects): an anonymous function whose single body
/// statement carries the object index.
fn function_placeholder(object_index: usize) -> Expr {
    super::count_unhandled_fragment();
    let marker = Expr::new(
        ExprKind::Identifier(syntax::Ident::new(format!(
            "__krkr_decomp_object_{object_index}"
        ))),
        Span::empty(0),
    );
    let body = syntax::Stmt::new(
        syntax::StmtKind::Block(vec![syntax::Stmt::new(
            syntax::StmtKind::Expr(marker),
            Span::empty(0),
        )]),
        Span::empty(0),
    );
    Expr::new(
        ExprKind::Function(Box::new(syntax::FunctionDecl {
            name: None,
            params: Vec::new(),
            return_type: None,
            body: Box::new(body),
            span: Span::empty(0),
        })),
        Span::empty(0),
    )
}

/// Builds a member access expression for `object_reg` with a direct or
/// computed key. `at_top_level` rewrites `this.*name` to a bare identifier
/// (at the top level `this` is the global object).
pub(crate) fn member_expr(
    names: &Names,
    object_reg: i16,
    name: &str,
    at_top_level: bool,
) -> Expr {
    names.member_target(object_reg, name, at_top_level)
}
