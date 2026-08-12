//! Declaration skeleton reconstruction.
//!
//! The top-level object's body registers functions, classes, and properties
//! through a fixed bytecode pattern (`const` the code object, `chgthis` it to
//! this, `spds` it into this/global). This pass lifts those registration
//! triples back into `function` / `class` / `property` declarations using the
//! object table (parameter lists come from the object headers, bodies from
//! decompiling the target code object).

use std::collections::BTreeSet;

use crate::bytecode::{BytecodeContextType, BytecodeFile, CodeObject};
use crate::error::{Result, Span, TjsError};
use crate::frontend::syntax::{
    self, Expr, ExprKind, FunctionDecl, Ident, ParamDecl, Stmt, StmtKind,
};

use super::naming::Names;
use super::stmt::{self, BodyOutput};

pub(crate) fn decompile_file(
    file: &BytecodeFile,
    objects: &[usize],
    unhandled: &mut usize,
) -> Result<Vec<Stmt>> {
    let Some(top_level) = file.top_level else {
        return Err(TjsError::bytecode("bytecode has no top-level object"));
    };
    let top = &file.objects[top_level];
    let body = stmt::decompile_body(file, top);
    *unhandled += body.unhandled;
    lift_registrations(file, objects, &body.statements)
}

/// Lifts registration statements into declarations.
fn lift_registrations(
    file: &BytecodeFile,
    objects: &[usize],
    statements: &[Stmt],
) -> Result<Vec<Stmt>> {
    let mut lifted = Vec::with_capacity(statements.len());
    let mut lifted_names = std::collections::BTreeMap::new();
    for statement in statements {
        let statements = split_registration_comma(statement);
        for statement in &statements {
            if let Some(lifted_stmt) = lift_registration(file, objects, statement)? {
                // Recompiled hoisting preambles can register the same object
                // twice with identical bodies; keep the first declaration and
                // drop exact repeats (re-registering the same body is a no-op).
                if lifted_names.get(&lifted_stmt.name) == Some(&lifted_stmt.stmt) {
                    continue;
                }
                lifted_names.insert(lifted_stmt.name.clone(), lifted_stmt.stmt.clone());
                lifted.push(lifted_stmt.stmt);
                continue;
            }
            lifted.push(statement.clone());
        }
    }
    Ok(lifted)
}

struct LiftedDeclaration {
    stmt: Stmt,
    name: String,
}

/// Splits a comma of registration statements (`a = function(){}, b =
/// function(){} incontextof this;`) into individual statements; collapses
/// repeated registrations of the same name to the last one (duplicate
/// closure creations are pure). Elements that are not registrations keep
/// their position as standalone statements.
fn split_registration_comma(stmt: &Stmt) -> Vec<Stmt> {
    let StmtKind::Expr(Expr {
        kind: ExprKind::Comma(elements),
        ..
    }) = &stmt.kind
    else {
        return vec![stmt.clone()];
    };
    if elements.len() < 2 {
        return vec![stmt.clone()];
    }
    let mut registrations: Vec<(String, Stmt)> = Vec::new();
    let mut other = Vec::new();
    for element in elements {
        let Some((target, value)) = assignment_parts(element) else {
            other.push(element.clone());
            continue;
        };
        if is_function_placeholder_value(value).is_none() && function_literal_value(value).is_none() {
            other.push(element.clone());
            continue;
        }
        registrations.retain(|(name, _)| *name != target);
        registrations.push((
            target,
            Stmt::new(StmtKind::Expr(element.clone()), Span::empty(0)),
        ));
    }
    let mut statements = Vec::with_capacity(registrations.len() + other.len());
    // Keep the original relative order: registrations and other elements can
    // interleave in the comma.
    if let StmtKind::Expr(Expr {
        kind: ExprKind::Comma(elements),
        ..
    }) = &stmt.kind
    {
        for element in elements {
            if let Some((target, _)) = assignment_parts(element)
                && registrations
                    .iter()
                    .any(|(name, _)| *name == target)
            {
                let registration = registrations
                    .iter()
                    .find(|(name, _)| *name == target)
                    .map(|(_, stmt)| stmt.clone())
                    .expect("registration kept above");
                statements.push(registration);
            } else {
                statements.push(Stmt::new(StmtKind::Expr(element.clone()), Span::empty(0)));
            }
        }
        return statements;
    }
    for (_, stmt) in registrations {
        statements.push(stmt);
    }
    statements.extend(other.into_iter().map(|expr| {
        Stmt::new(StmtKind::Expr(expr), Span::empty(0))
    }));
    statements
}

/// Returns `(target name, value)` for a plain assignment expression.
fn assignment_parts(expr: &Expr) -> Option<(String, &Expr)> {
    match &expr.kind {
        ExprKind::Assignment {
            op: syntax::AssignOp::Assign,
            target,
            value,
        } => match &target.kind {
            ExprKind::Identifier(target) => Some((target.name.clone(), value)),
            _ => None,
        },
        _ => None,
    }
}

/// Returns the placeholder object index when `expr` is a bare function
/// placeholder or one bound with `incontextof this`.
fn is_function_placeholder_value(expr: &Expr) -> Option<usize> {
    match &expr.kind {
        ExprKind::Function(_) => object_placeholder(expr),
        ExprKind::Binary {
            op: syntax::BinaryOp::InContextOf,
            lhs,
            rhs,
        } => match &rhs.kind {
            ExprKind::This => object_placeholder(lhs),
            _ => None,
        },
        _ => None,
    }
}

/// Whether `expr` is the placeholder for code object `object_index`.
fn object_placeholder(expr: &Expr) -> Option<usize> {
    // Registrations bind the placeholder with `incontextof this`; unwrap it.
    let function = match &expr.kind {
        ExprKind::Function(decl) => decl,
        ExprKind::Binary {
            op: syntax::BinaryOp::InContextOf,
            lhs,
            ..
        } => match &lhs.kind {
            ExprKind::Function(decl) => decl,
            _ => return None,
        },
        _ => return None,
    };
    let StmtKind::Block(body) = &function.body.kind else {
        return None;
    };
    let [Stmt {
        kind: StmtKind::Expr(marker),
        ..
    }] = body.as_slice()
    else {
        return None;
    };
    match &marker.kind {
        ExprKind::Identifier(ident) => ident
            .name
            .strip_prefix("__krkr_decomp_object_")
            .and_then(|index| index.parse().ok()),
        _ => None,
    }
}

/// Matches `name = function(){...} [incontextof this];` — the scanner's
/// rendering of the registration pattern (`const` the code object, optional
/// `chgthis` it to this, then store into a variable/member) — and lifts it
/// into a declaration. Function literals decompile in place, so only their
/// name is added; classes/properties still carry the object-index
/// placeholder and are reconstructed from the object table.
fn lift_registration(
    file: &BytecodeFile,
    objects: &[usize],
    stmt: &Stmt,
) -> Result<Option<LiftedDeclaration>> {
    let Some((name, value)) = registration_target(stmt) else {
        return Ok(None);
    };
    // Plain function literal (possibly bound with `incontextof this`): lift
    // by giving the already-decompiled body a declaration name.
    if let Some(mut decl) = function_literal_value(&value) {
        decl.name = Some(Ident::new(name.clone()));
        return Ok(Some(LiftedDeclaration {
            stmt: Stmt::new(StmtKind::FunctionDecl(decl), Span::empty(0)),
            name,
        }));
    }
    // Class/property registrations keep the placeholder and rebuild from the
    // object table.
    let Some(placeholder) = object_placeholder(&value) else {
        return Ok(None);
    };
    let Some(&object_index) = objects.iter().find(|&&candidate| candidate == placeholder) else {
        return Ok(None);
    };
    let object = &file.objects[object_index];
    let stmt = match object.context_type {
        BytecodeContextType::Class => {
            let body = stmt::decompile_body(file, object);
            let statements = class_body_statements(body);
            // Class members arrive through the `properties` table (the body
            // itself is just `regmember; ret`).
            let mut members = Vec::new();
            for property in &object.properties {
                let Some(member) = file.objects.get(property.object) else {
                    continue;
                };
                let Some(name) = file.data.strings.get(property.name) else {
                    continue;
                };
                match member.context_type {
                    BytecodeContextType::Function | BytecodeContextType::ExprFunction => {
                        let decl = function_decl(file, member, Some(name.clone()))?;
                        members.push(Stmt::new(StmtKind::FunctionDecl(decl), Span::empty(0)));
                    }
                    BytecodeContextType::Property => {
                        let getter = member
                            .prop_getter
                            .map(|index| file.objects.get(index))
                            .flatten()
                            .map(|getter| function_decl(file, getter, None))
                            .transpose()?;
                        let setter = member
                            .prop_setter
                            .map(|index| file.objects.get(index))
                            .flatten()
                            .map(|setter| function_decl(file, setter, None))
                            .transpose()?;
                        let decl = syntax::PropertyDecl {
                            name: Ident::new(name.clone()),
                            getter,
                            setter,
                            span: Span::empty(0),
                        };
                        members.push(Stmt::new(StmtKind::PropertyDecl(decl), Span::empty(0)));
                    }
                    _ => {}
                }
            }
            members.extend(statements);
            // `extends` comes from the super-class getter object: its body
            // evaluates the superclass expression and returns it.
            let mut extends = Vec::new();
            if let Some(getter_index) = object.super_class_getter
                && let Some(getter) = file.objects.get(getter_index)
            {
                let getter_body = stmt::decompile_body(file, getter);
                if let [Stmt {
                    kind: StmtKind::Return(Some(expr)),
                    ..
                }] = getter_body.statements.as_slice()
                {
                    extends.push(expr.clone());
                }
            }
            let decl = syntax::ClassDecl {
                name: Ident::new(name.clone()),
                extends,
                body: members,
                span: Span::empty(0),
            };
            Stmt::new(StmtKind::ClassDecl(decl), Span::empty(0))
        }
        BytecodeContextType::Property => {
            let getter = object
                .prop_getter
                .map(|index| file.objects.get(index))
                .flatten()
                .map(|getter| function_decl(file, getter, None))
                .transpose()?;
            let setter = object
                .prop_setter
                .map(|index| file.objects.get(index))
                .flatten()
                .map(|setter| function_decl(file, setter, None))
                .transpose()?;
            let decl = syntax::PropertyDecl {
                name: Ident::new(name.clone()),
                getter,
                setter,
                span: Span::empty(0),
            };
            Stmt::new(StmtKind::PropertyDecl(decl), Span::empty(0))
        }
        _ => return Ok(None),
    };
    Ok(Some(LiftedDeclaration { stmt, name }))
}

/// The `FunctionDecl` of a real function-literal value, unwrapping an
/// `incontextof this` binder. Class/property placeholders (whose body is the
/// object-index marker) are not function literals.
fn function_literal_value(expr: &Expr) -> Option<FunctionDecl> {
    if object_placeholder(expr).is_some() {
        return None;
    }
    match &expr.kind {
        ExprKind::Function(decl) => Some((**decl).clone()),
        ExprKind::Binary {
            op: syntax::BinaryOp::InContextOf,
            lhs,
            rhs,
        } if matches!(rhs.kind, ExprKind::This) => {
            // The placeholder guard must also apply to the unwrapped
            // operand: class/property registrations bind the placeholder
            // with `incontextof this` and must take the object-table path.
            if object_placeholder(lhs).is_some() {
                return None;
            }
            match &lhs.kind {
                ExprKind::Function(decl) => Some((**decl).clone()),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Extracts `(name, value)` from an assignment-shaped or var-shaped
/// statement.
fn registration_target(stmt: &Stmt) -> Option<(String, Expr)> {
    match &stmt.kind {
        StmtKind::Var { declarations, .. } => {
            let [decl] = declarations.as_slice() else {
                return None;
            };
            let initializer = decl.initializer.as_ref()?;
            Some((decl.name.name.clone(), initializer.clone()))
        }
        StmtKind::Expr(Expr {
            kind:
                ExprKind::Assignment {
                    op: syntax::AssignOp::Assign,
                    target,
                    value,
                },
            ..
        }) => match &target.kind {
            ExprKind::Identifier(target) => {
                Some((target.name.clone(), (**value).clone()))
            }
            // Class-body method registrations render as `this.name = ...`.
            ExprKind::Member { object, property } if matches!(object.kind, ExprKind::This) => {
                Some((property.clone(), (**value).clone()))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Builds a `FunctionDecl` for a code object: the parameter list comes from
/// the object header, the body from decompiling the object.
pub(crate) fn function_decl(
    file: &BytecodeFile,
    object: &CodeObject,
    name: Option<String>,
) -> Result<FunctionDecl> {
    let mut params = Vec::new();
    let arg_count = object.func_decl_arg_count as usize;
    let collapse_base = object.func_decl_collapse_base.map(|base| base as usize);
    let unnamed_base = object.func_decl_unnamed_arg_array_base as usize;
    let names = Names::new(object);
    for index in 0..arg_count {
        if object.func_decl_unnamed_arg_array_base != 0
            && index == unnamed_base
        {
            // Bare `*`: unnamed argument array parameter.
            params.push(ParamDecl {
                name: None,
                ty: None,
                default: None,
                collapse: true,
                span: Span::empty(0),
            });
        } else {
            params.push(ParamDecl {
                name: Some(Ident::new(names.name(-3 - index as i16))),
                ty: None,
                default: None,
                collapse: collapse_base == Some(index),
                span: Span::empty(0),
            });
        }
    }
    let body = stmt::decompile_body(file, object);
    let mut body_statements = body.statements;
    // Function bodies end with the implicit `srv %0; ret`.
    drop_trailing_bare_return(&mut body_statements);
    merge_for_init(&mut body_statements);
    let body = Stmt::new(StmtKind::Block(body_statements), Span::empty(0));
    Ok(FunctionDecl {
        name: name.map(Ident::new),
        params,
        return_type: None,
        body: Box::new(body),
        span: Span::empty(0),
    })
}

/// Drops the class bookkeeping preamble (the class-name constant statement)
/// and the constructor's implicit trailing `return;` from a decompiled
/// class body.
fn class_body_statements(body: BodyOutput) -> Vec<Stmt> {
    let mut statements = body.statements;
    if let Some(Stmt {
        kind: StmtKind::Expr(Expr {
            kind: ExprKind::String(name),
            ..
        }),
        ..
    }) = statements.first()
        && statements.len() > 1
    {
        let _ = name.clone();
        statements.remove(0);
    }
    drop_trailing_bare_return(&mut statements);
    statements
}

/// Removes a trailing bare `return;` (the implicit function epilogue).
fn drop_trailing_bare_return(statements: &mut Vec<Stmt>) {
    if let Some(Stmt {
        kind: StmtKind::Return(None),
        ..
    }) = statements.last()
    {
        statements.pop();
    }
}

/// Merges `var x = init; for (; cond; step) { ... }` into
/// `for (var x = init; cond; step) { ... }` when `x` feeds the loop (the
/// decompiler reconstructs `for` loops with their init hoisted). The merge
/// is scope-preserving: the variable stays declared in the same scope.
pub(crate) fn merge_for_init(statements: &mut Vec<Stmt>) {
    let mut index = 0;
    while index + 1 < statements.len() {
        let StmtKind::Var { declarations, .. } = &statements[index].kind else {
            index += 1;
            continue;
        };
        let [decl] = declarations.as_slice() else {
            index += 1;
            continue;
        };
        let name = decl.name.name.clone();
        let StmtKind::For {
            init: None,
            condition,
            step,
            body,
        } = &statements[index + 1].kind
        else {
            index += 1;
            continue;
        };
        let used = condition
            .as_ref()
            .is_some_and(|cond| expr_mentions(cond, &name))
            || step.as_ref().is_some_and(|step| expr_mentions(step, &name))
            || block_mentions(body, &name);
        if !used {
            index += 1;
            continue;
        }
        let var_stmt = statements.remove(index);
        let StmtKind::For { init, .. } = &mut statements[index].kind else {
            unreachable!("the for moved with the var");
        };
        *init = Some(syntax::ForInit::Var {
            kind: syntax::VarKind::Var,
            declarations: match var_stmt.kind {
                StmtKind::Var { declarations, .. } => declarations,
                _ => unreachable!("checked above"),
            },
        });
        index += 1;
    }
}

fn block_mentions(stmt: &Stmt, name: &str) -> bool {
    match &stmt.kind {
        StmtKind::Block(statements) => statements.iter().any(|stmt| stmt_mentions(stmt, name)),
        _ => stmt_mentions(stmt, name),
    }
}

fn stmt_mentions(stmt: &Stmt, name: &str) -> bool {
    match &stmt.kind {
        StmtKind::Expr(expr) | StmtKind::Return(Some(expr)) => expr_mentions(expr, name),
        StmtKind::Var { declarations, .. } => declarations
            .iter()
            .any(|decl| decl.initializer.as_ref().is_some_and(|expr| expr_mentions(expr, name))),
        StmtKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            expr_mentions(condition, name)
                || stmt_mentions(then_branch, name)
                || else_branch
                    .as_ref()
                    .is_some_and(|branch| stmt_mentions(branch, name))
        }
        StmtKind::While { condition, body } | StmtKind::DoWhile { condition, body } => {
            expr_mentions(condition, name) || stmt_mentions(body, name)
        }
        StmtKind::For {
            condition,
            step,
            body,
            ..
        } => {
            condition
                .as_ref()
                .is_some_and(|cond| expr_mentions(cond, name))
                || step.as_ref().is_some_and(|step| expr_mentions(step, name))
                || stmt_mentions(body, name)
        }
        StmtKind::Switch {
            discriminant, cases, ..
        } => {
            expr_mentions(discriminant, name)
                || cases
                    .iter()
                    .any(|case| case.body.iter().any(|stmt| stmt_mentions(stmt, name)))
        }
        StmtKind::Try { body, catch, .. } => {
            stmt_mentions(body, name)
                || catch
                    .as_ref()
                    .is_some_and(|catch| stmt_mentions(&catch.body, name))
        }
        StmtKind::Block(statements) => statements.iter().any(|stmt| stmt_mentions(stmt, name)),
        _ => false,
    }
}

fn expr_mentions(expr: &Expr, name: &str) -> bool {
    match &expr.kind {
        ExprKind::Identifier(ident) => ident.name == name,
        ExprKind::Unary { expr, .. } | ExprKind::Postfix { expr, .. } => expr_mentions(expr, name),
        ExprKind::Binary { lhs, rhs, .. }
        | ExprKind::Assignment { target: lhs, value: rhs, .. }
        | ExprKind::Index { object: lhs, index: rhs, .. } => {
            expr_mentions(lhs, name) || expr_mentions(rhs, name)
        }
        ExprKind::Conditional {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_mentions(condition, name)
                || expr_mentions(then_expr, name)
                || expr_mentions(else_expr, name)
        }
        ExprKind::Member { object, .. } => expr_mentions(object, name),
        ExprKind::Call { callee, args } | ExprKind::New { callee, args } => {
            expr_mentions(callee, name)
                || args.iter().any(|arg| match arg {
                    syntax::CallArg::Value(expr)
                    | syntax::CallArg::Expand(Some(expr)) => expr_mentions(expr, name),
                    _ => false,
                })
        }
        ExprKind::Array(elements) | ExprKind::ConstArray(elements) => {
            elements.iter().any(|element| match element {
                syntax::ArrayElement::Value(expr) => expr_mentions(expr, name),
                syntax::ArrayElement::Hole => false,
            })
        }
        ExprKind::Dictionary(entries) | ExprKind::ConstDictionary(entries) => entries
            .iter()
            .any(|entry| expr_mentions(&entry.key, name) || expr_mentions(&entry.value, name)),
        ExprKind::Comma(exprs) => exprs.iter().any(|expr| expr_mentions(expr, name)),
        ExprKind::Function(decl) => block_mentions(&decl.body, name),
        _ => false,
    }
}

/// Keeps only objects matching the filter (or all of them).
pub(crate) fn select_objects(
    file: &BytecodeFile,
    filter: Option<&str>,
    object_index: Option<usize>,
) -> Result<Vec<usize>> {
    let mut selected = Vec::new();
    for (index, object) in file.objects.iter().enumerate() {
        if let Some(wanted) = object_index {
            if wanted != index {
                continue;
            }
        }
        if let Some(filter) = filter {
            let name = object.name(file).unwrap_or("");
            if !name.contains(filter) {
                continue;
            }
        }
        selected.push(index);
    }
    if let Some(wanted) = object_index
        && !file.objects.get(wanted).is_some()
    {
        return Err(TjsError::bytecode(format!("object {wanted} does not exist")));
    }
    let _ = BTreeSet::<usize>::new();
    Ok(selected)
}

