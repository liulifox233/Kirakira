//! TJS2 source pretty-printer for the parsed syntax tree.
//!
//! The decompiler lowers reconstructed bytecode into [`syntax::Program`] and
//! renders it with this printer. Output is conservative about operator
//! precedence: parentheses are emitted exactly where the parser's precedence
//! levels require them (matching `Parser::parse_binary` / `parse_unary` /
//! `parse_incontextof` / `parse_conditional` recursion), so printing an AST
//! and re-parsing it yields the same expression shape.
//!
//! Layout convention: every block-bodied construct prints its braces on the
//! header line and its body at one deeper indentation level. Bodies that are
//! not blocks in the AST are wrapped in braces when printed (semantically
//! identical, shape-stable).

use super::syntax::{
    ArrayElement, AssignOp, BinaryOp, CallArg, ClassDecl, DictionaryEntry, Expr, ExprKind,
    ForInit, FunctionDecl, ParamDecl, Program, PropertyDecl, Stmt, StmtKind, SwitchCase, UnaryOp,
    VarDecl, VarKind,
};

/// Expression precedence levels, mirroring the parser recursion:
/// loose on top, tight at the bottom.
const PREC_IF: u8 = 0; // postfix `expr if expr` (parsed above comma)
const PREC_COMMA: u8 = 1;
const PREC_ASSIGN: u8 = 2;
const PREC_CONDITIONAL: u8 = 3;
// binary operators occupy levels 4..=13 (see `binary_op_prec`)
const PREC_INCTX: u8 = 14; // `incontextof` (operands are postfix expressions)
const PREC_INSTANCEOF: u8 = 15;
const PREC_UNARY: u8 = 16;
const PREC_POSTFIX: u8 = 17;
const PREC_PRIMARY: u8 = 18;

pub fn print_program(program: &Program) -> String {
    let mut printer = Printer::default();
    printer.statements(&program.statements, 0);
    printer.finish()
}

pub fn print_statements(statements: &[Stmt]) -> String {
    let mut printer = Printer::default();
    printer.statements(statements, 0);
    printer.finish()
}

pub fn print_statement(statement: &Stmt) -> String {
    let mut printer = Printer::default();
    printer.statement(statement, 0);
    printer.finish()
}

pub fn print_expression(expr: &Expr) -> String {
    let mut printer = Printer::default();
    let text = printer.expr(expr, PREC_IF);
    format!("{text}\n")
}

#[derive(Default)]
struct Printer {
    out: String,
    indent: usize,
}

impl Printer {
    fn finish(self) -> String {
        let mut out = self.out;
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out
    }

    fn line(&mut self, text: &str) {
        for _ in 0..self.indent {
            self.out.push_str("    ");
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn statements(&mut self, statements: &[Stmt], depth: usize) {
        for statement in statements {
            self.statement(statement, depth);
        }
    }

    fn statement(&mut self, statement: &Stmt, depth: usize) {
        self.indent = depth;
        match &statement.kind {
            StmtKind::Empty => {}
            StmtKind::Block(body) => {
                self.line("{");
                self.statements(body, depth + 1);
                self.indent = depth;
                self.line("}");
            }
            StmtKind::Expr(expr) => {
                // A function expression cannot start a statement in TJS2
                // (the parser would read it as a named function
                // declaration), so parenthesize it.
                let text = if matches!(expr.kind, ExprKind::Function(_)) {
                    let inner = self.expression(expr);
                    format!("({inner});")
                } else {
                    let inner = self.expression(expr);
                    format!("{inner};")
                };
                self.line(&text);
            }
            StmtKind::Var { kind, declarations } => {
                let keyword = match kind {
                    VarKind::Var => "var ",
                    VarKind::Const => "const ",
                };
                let decls = declarations
                    .iter()
                    .map(|decl| self.var_decl(decl))
                    .collect::<Vec<_>>()
                    .join(", ");
                self.line(&format!("{keyword}{decls};"));
            }
            StmtKind::FunctionDecl(decl) => self.function_statement(decl, depth),
            StmtKind::ClassDecl(decl) => self.class_decl(decl, depth),
            StmtKind::PropertyDecl(decl) => self.property_decl(decl, depth),
            StmtKind::Return(expr) => match expr {
                Some(expr) => {
                    let inner = self.expression(expr);
                    self.line(&format!("return {inner};"));
                }
                None => self.line("return;"),
            },
            StmtKind::Throw(expr) => {
                let inner = self.expression(expr);
                self.line(&format!("throw {inner};"));
            }
            StmtKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let cond = self.expression(condition);
                self.line(&format!("if ({cond}) {{"));
                self.body_statements(then_branch, depth + 1);
                match else_branch {
                    Some(else_branch) => match &else_branch.kind {
                        StmtKind::If { .. } => self.print_else_if(else_branch, depth),
                        _ => {
                            self.indent = depth;
                            self.line("} else {");
                            self.body_statements(else_branch, depth + 1);
                            self.indent = depth;
                            self.line("}");
                        }
                    },
                    None => {
                        self.indent = depth;
                        self.line("}");
                    }
                }
            }
            StmtKind::While { condition, body } => {
                let cond = self.expression(condition);
                self.line(&format!("while ({cond}) {{"));
                self.body_statements(body, depth + 1);
                self.indent = depth;
                self.line("}");
            }
            StmtKind::DoWhile { body, condition } => {
                self.line("do {");
                self.body_statements(body, depth + 1);
                self.indent = depth;
                let cond = self.expression(condition);
                self.line(&format!("}} while ({cond});"));
            }
            StmtKind::For {
                init,
                condition,
                step,
                body,
            } => {
                let init = match init {
                    Some(ForInit::Var { kind, declarations }) => {
                        let keyword = match kind {
                            VarKind::Var => "var ",
                            VarKind::Const => "const ",
                        };
                        let decls = declarations
                            .iter()
                            .map(|decl| self.var_decl(decl))
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!("{keyword}{decls}")
                    }
                    Some(ForInit::Expr(expr)) => self.expression(expr),
                    None => String::new(),
                };
                let cond = condition
                    .as_ref()
                    .map(|expr| self.expression(expr))
                    .unwrap_or_default();
                let step = step
                    .as_ref()
                    .map(|expr| self.expression(expr))
                    .unwrap_or_default();
                self.line(&format!("for ({init}; {cond}; {step}) {{"));
                self.body_statements(body, depth + 1);
                self.indent = depth;
                self.line("}");
            }
            StmtKind::With { object, body } => {
                let obj = self.expression(object);
                self.line(&format!("with ({obj}) {{"));
                self.body_statements(body, depth + 1);
                self.indent = depth;
                self.line("}");
            }
            StmtKind::Break => self.line("break;"),
            StmtKind::Continue => self.line("continue;"),
            StmtKind::Try { body, catch } => {
                self.line("try {");
                self.body_statements(body, depth + 1);
                self.indent = depth;
                match catch {
                    Some(catch) => {
                        let binding = catch
                            .binding
                            .as_ref()
                            .map(|binding| binding.name.as_str())
                            .unwrap_or("");
                        self.line(&format!("}} catch ({binding}) {{"));
                        self.body_statements(&catch.body, depth + 1);
                        self.indent = depth;
                        self.line("}");
                    }
                    None => self.line("}"),
                }
            }
            StmtKind::Switch { discriminant, cases } => {
                let disc = self.expression(discriminant);
                self.line(&format!("switch ({disc}) {{"));
                for case in cases {
                    self.switch_case(case, depth + 1);
                }
                self.indent = depth;
                self.line("}");
            }
            StmtKind::Case { test } => match test {
                Some(test) => {
                    let inner = self.expression(test);
                    self.line(&format!("case {inner}:"));
                }
                None => self.line("default:"),
            },
            StmtKind::Debugger => self.line("debugger;"),
        }
    }

    fn print_else_if(&mut self, stmt: &Stmt, depth: usize) {
        let StmtKind::If {
            condition,
            then_branch,
            else_branch,
        } = &stmt.kind
        else {
            unreachable!("print_else_if called on non-if");
        };
        let cond = self.expression(condition);
        self.indent = depth;
        self.line(&format!("}} else if ({cond}) {{"));
        self.body_statements(then_branch, depth + 1);
        match else_branch {
            Some(else_branch) => match &else_branch.kind {
                StmtKind::If { .. } => self.print_else_if(else_branch, depth),
                _ => {
                    self.indent = depth;
                    self.line("} else {");
                    self.body_statements(else_branch, depth + 1);
                    self.indent = depth;
                    self.line("}");
                }
            },
            None => {
                self.indent = depth;
                self.line("}");
            }
        }
    }

    fn body_statements(&mut self, body: &Stmt, depth: usize) {
        match &body.kind {
            StmtKind::Block(body) => self.statements(body, depth),
            _ => self.statement(body, depth),
        }
    }

    fn switch_case(&mut self, case: &SwitchCase, depth: usize) {
        self.indent = depth - 1;
        match &case.test {
            Some(test) => {
                let inner = self.expression(test);
                self.line(&format!("case {inner}:"));
            }
            None => self.line("default:"),
        }
        self.statements(&case.body, depth);
    }

    fn var_decl(&mut self, decl: &VarDecl) -> String {
        let mut out = decl.name.name.clone();
        if let Some(ty) = &decl.ty {
            out.push_str(&format!(" : {ty}"));
        }
        if let Some(init) = &decl.initializer {
            let init = self.expression(init);
            out.push_str(&format!(" = {init}"));
        }
        out
    }

    fn function_statement(&mut self, decl: &FunctionDecl, depth: usize) {
        let header = self.function_header(decl);
        self.line(&header);
        self.body_statements(&decl.body, depth + 1);
        self.indent = depth;
        self.line("}");
    }

    /// Renders `function name(params) : ret {` (no body, no closing brace).
    fn function_header(&mut self, decl: &FunctionDecl) -> String {
        let mut out = String::from("function");
        if let Some(name) = &decl.name {
            out.push(' ');
            out.push_str(&name.name);
        }
        out.push('(');
        let params = decl
            .params
            .iter()
            .map(|param| self.param_decl(param))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&params);
        out.push(')');
        if let Some(ty) = &decl.return_type {
            out.push_str(&format!(" : {ty}"));
        }
        out.push_str(" {");
        out
    }

    fn param_decl(&mut self, param: &ParamDecl) -> String {
        match &param.name {
            Some(name) => {
                let mut out = name.name.clone();
                if let Some(ty) = &param.ty {
                    out.push_str(&format!(" : {ty}"));
                }
                if let Some(default) = &param.default {
                    let default = self.expression(default);
                    out.push_str(&format!(" = {default}"));
                }
                if param.collapse {
                    out.push('*');
                }
                out
            }
            None => "*".to_string(),
        }
    }

    fn class_decl(&mut self, decl: &ClassDecl, depth: usize) {
        let mut header = format!("class {}", decl.name.name);
        if !decl.extends.is_empty() {
            let extends = decl
                .extends
                .iter()
                .map(|expr| self.expression(expr))
                .collect::<Vec<_>>()
                .join(", ");
            header.push_str(&format!(" extends {extends}"));
        }
        header.push_str(" {");
        self.line(&header);
        self.statements(&decl.body, depth + 1);
        self.indent = depth;
        self.line("}");
    }

    fn property_decl(&mut self, decl: &PropertyDecl, depth: usize) {
        self.line(&format!("property {} {{", decl.name.name));
        if let Some(getter) = &decl.getter {
            let params = getter
                .params
                .iter()
                .map(|param| self.param_decl(param))
                .collect::<Vec<_>>()
                .join(", ");
            let mut header = format!("getter({params})");
            if let Some(ty) = &getter.return_type {
                header.push_str(&format!(" : {ty}"));
            }
            header.push_str(" {");
            self.indent = depth + 1;
            self.line(&header);
            self.body_statements(&getter.body, depth + 2);
            self.indent = depth + 1;
            self.line("}");
        }
        if let Some(setter) = &decl.setter {
            let params = setter
                .params
                .iter()
                .map(|param| self.param_decl(param))
                .collect::<Vec<_>>()
                .join(", ");
            let mut header = format!("setter({params})");
            if let Some(ty) = &setter.return_type {
                header.push_str(&format!(" : {ty}"));
            }
            header.push_str(" {");
            self.indent = depth + 1;
            self.line(&header);
            self.body_statements(&setter.body, depth + 2);
            self.indent = depth + 1;
            self.line("}");
        }
        self.indent = depth;
        self.line("}");
    }

    /// Renders an expression with the given minimum precedence context.
    fn expr(&mut self, expr: &Expr, min_prec: u8) -> String {
        let prec = natural_prec(expr);
        let mut out = self.expr_text(expr);
        if prec < min_prec {
            out = format!("({out})");
        }
        out
    }

    /// Prints an expression in a postfix operand position. `void` cannot
    /// carry a following `.`/`[`/`(` token, and a numeric literal would lex
    /// its `.` into a real literal (`74.y` -> `74.` `y`), so both shapes are
    /// parenthesized.
    fn postfix_operand(&mut self, expr: &Expr) -> String {
        let text = self.expr(expr, PREC_POSTFIX);
        let needs_parens = matches!(
            expr.kind,
            ExprKind::Void | ExprKind::Integer(_) | ExprKind::Real(_)
        ) || matches!(
            expr.kind,
            ExprKind::Unary {
                op: UnaryOp::IsValid,
                ..
            }
        );
        if needs_parens {
            format!("({text})")
        } else {
            text
        }
    }

    /// Renders an expression without precedence parentheses.
    fn expr_text(&mut self, expr: &Expr) -> String {
        match &expr.kind {
            ExprKind::Void => "void".to_string(),
            ExprKind::Null => "null".to_string(),
            ExprKind::Bool(true) => "true".to_string(),
            ExprKind::Bool(false) => "false".to_string(),
            ExprKind::Integer(value) => value.to_string(),
            ExprKind::Real(value) => print_real(*value),
            ExprKind::String(value) => print_string(value),
            ExprKind::Octet(bytes) => print_octet(bytes),
            ExprKind::RegExp { pattern, flags } => {
                // The lexer stores regexp patterns verbatim (escapes like
                // `\/` are kept), so printing them verbatim round-trips.
                format!("/{pattern}/{flags}")
            }
            ExprKind::Identifier(ident) => ident.name.clone(),
            ExprKind::This => "this".to_string(),
            ExprKind::Super => "super".to_string(),
            ExprKind::Global => "global".to_string(),
            ExprKind::Nan => "NaN".to_string(),
            ExprKind::Infinity => "Infinity".to_string(),
            ExprKind::Array(elements) => print_array_literal(self, elements, false),
            ExprKind::ConstArray(elements) => print_array_literal(self, elements, true),
            ExprKind::Dictionary(entries) => print_dictionary_literal(self, entries, false),
            ExprKind::ConstDictionary(entries) => print_dictionary_literal(self, entries, true),
            ExprKind::Unary { op, expr: operand } => self.unary_expr(*op, operand),
            ExprKind::Binary { op, lhs, rhs } => self.binary_expr(*op, lhs, rhs),
            ExprKind::Assignment { op, target, value } => {
                let target = self.expr(target, PREC_CONDITIONAL);
                let is_new = matches!(value.kind, ExprKind::New { .. });
                let value = self.expr(value, PREC_ASSIGN);
                // The parser's assignment operand level does not reach `new`.
                let value = if is_new {
                    format!("({value})")
                } else {
                    value
                };
                format!("{target} {} {value}", assign_op_text(*op))
            }
            ExprKind::Conditional {
                condition,
                then_expr,
                else_expr,
            } => {
                let condition = self.expr(condition, 4);
                let then_expr = self.expr(then_expr, PREC_CONDITIONAL);
                let else_expr = self.expr(else_expr, PREC_CONDITIONAL);
                format!("{condition} ? {then_expr} : {else_expr}")
            }
            ExprKind::Member { object, property } => {
                let object = self.postfix_operand(object);
                format!("{object}.{property}")
            }
            ExprKind::WithMember { property } => format!(".{property}"),
            ExprKind::Index { object, index } => {
                let object = self.postfix_operand(object);
                let index = self.expr(index, PREC_IF);
                format!("{object}[{index}]")
            }
            ExprKind::Call { callee, args } => {
                let callee = self.postfix_operand(callee);
                let args = self.call_args(args);
                format!("{callee}({args})")
            }
            ExprKind::New { callee, args } => {
                let callee = self.postfix_operand(callee);
                let args = self.call_args(args);
                format!("new {callee}({args})")
            }
            ExprKind::Function(decl) => {
                let header = self.function_header(decl);
                let body = match &decl.body.kind {
                    StmtKind::Block(body) => body.clone(),
                    _ => vec![(*decl.body).clone()],
                };
                let mut text = header;
                for stmt in &body {
                    let rendered = print_statement(stmt);
                    text.push(' ');
                    text.push_str(rendered.trim_end());
                }
                text.push_str(" }");
                text
            }
            ExprKind::Postfix { op, expr: operand } => {
                let operand = self.expr(operand, PREC_POSTFIX);
                match op {
                    UnaryOp::Eval => format!("{operand}!"),
                    UnaryOp::Increment => format!("{operand}++"),
                    UnaryOp::Decrement => format!("{operand}--"),
                    _ => unreachable!("invalid postfix operator {op:?}"),
                }
            }
            ExprKind::Comma(exprs) => exprs
                .iter()
                .map(|expr| self.expr(expr, PREC_ASSIGN))
                .collect::<Vec<_>>()
                .join(", "),
        }
    }

    /// Shorthand used by call-site helpers.
    fn expression(&mut self, expr: &Expr) -> String {
        self.expr(expr, PREC_IF)
    }

    fn unary_expr(&mut self, op: UnaryOp, operand: &Expr) -> String {
        match op {
            UnaryOp::IsValid => {
                let is_new = matches!(operand.kind, ExprKind::New { .. });
                let operand = self.expr(operand, PREC_INCTX);
                // The parser's `isvalid` operand level does not reach `new`.
                if is_new {
                    format!("({operand}) isvalid")
                } else {
                    format!("{operand} isvalid")
                }
            }
            UnaryOp::AsInt => {
                let operand = self.expr(operand, PREC_UNARY);
                format!("(int){operand}")
            }
            UnaryOp::AsReal => {
                let operand = self.expr(operand, PREC_UNARY);
                format!("(real){operand}")
            }
            UnaryOp::AsString => {
                let operand = self.expr(operand, PREC_UNARY);
                format!("(string){operand}")
            }
            _ => {
                let operand = self.expr(operand, PREC_UNARY);
                let text = match op {
                    UnaryOp::Plus => "+",
                    UnaryOp::Minus => "-",
                    UnaryOp::LogicalNot => "!",
                    UnaryOp::BitNot => "~",
                    UnaryOp::Delete => "delete ",
                    UnaryOp::TypeOf => "typeof ",
                    UnaryOp::Invalidate => "invalidate ",
                    UnaryOp::IgnoreProp => "&",
                    UnaryOp::PropAccess => "*",
                    UnaryOp::Sharp => "#",
                    UnaryOp::Dollar => "$",
                    UnaryOp::Increment => "++",
                    UnaryOp::Decrement => "--",
                    _ => unreachable!("invalid prefix operator {op:?}"),
                };
                // `- -a` would lex as `--a`; separate identical signs.
                let separator = match op {
                    UnaryOp::Plus if operand.starts_with('+') => " ",
                    UnaryOp::Minus if operand.starts_with('-') => " ",
                    _ => "",
                };
                format!("{text}{separator}{operand}")
            }
        }
    }

    fn binary_expr(&mut self, op: BinaryOp, lhs: &Expr, rhs: &Expr) -> String {
        match op {
            BinaryOp::If => {
                let lhs = self.expr(lhs, PREC_COMMA);
                let rhs = self.expr(rhs, PREC_IF);
                format!("{lhs} if {rhs}")
            }
            BinaryOp::InContextOf => {
                let lhs = self.expr(lhs, PREC_POSTFIX);
                // The parser's `incontextof` operand level does not reach
                // the `new` keyword; parenthesize it.
                let is_new = matches!(rhs.kind, ExprKind::New { .. });
                let rhs = self.expr(rhs, PREC_INCTX);
                let rhs_text = if is_new {
                    format!("({rhs})")
                } else {
                    rhs
                };
                format!("{lhs} incontextof {rhs_text}")
            }
            BinaryOp::InstanceOf => {
                let lhs = self.expr(lhs, PREC_INCTX);
                let is_new = matches!(rhs.kind, ExprKind::New { .. });
                let rhs = self.expr(rhs, PREC_UNARY);
                let rhs_text = if is_new {
                    format!("({rhs})")
                } else {
                    rhs
                };
                format!("{lhs} instanceof {rhs_text}")
            }
            _ => {
                let prec = binary_op_prec(op);
                let lhs = self.expr(lhs, prec);
                let rhs = self.expr(rhs, prec + 1);
                format!("{lhs} {} {rhs}", binary_op_text(op))
            }
        }
    }

    fn call_args(&mut self, args: &[CallArg]) -> String {
        args.iter()
            .map(|arg| match arg {
                CallArg::Value(expr) if matches!(expr.kind, ExprKind::Void) => String::new(),
                CallArg::Value(expr) => self.expr(expr, PREC_ASSIGN),
                CallArg::Expand(Some(expr)) => {
                    // The parser reads the expand operand with
                    // `parse_binary(10)`, i.e. mul-level and tighter.
                    let expr = self.expr(expr, 13);
                    format!("{expr}*")
                }
                CallArg::Expand(None) => "*".to_string(),
                CallArg::Omitted => "...".to_string(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn natural_prec(expr: &Expr) -> u8 {
    match &expr.kind {
        ExprKind::Binary { op: BinaryOp::If, .. } => PREC_IF,
        ExprKind::Comma(_) => PREC_COMMA,
        ExprKind::Assignment { .. } => PREC_ASSIGN,
        ExprKind::Conditional { .. } => PREC_CONDITIONAL,
        ExprKind::Binary { op, .. } => match op {
            BinaryOp::InContextOf => PREC_INCTX,
            BinaryOp::InstanceOf => PREC_INSTANCEOF,
            _ => binary_op_prec(*op),
        },
        ExprKind::Unary { op, .. } => match op {
            UnaryOp::IsValid | UnaryOp::Eval => PREC_POSTFIX,
            _ => PREC_UNARY,
        },
        ExprKind::Postfix { .. }
        | ExprKind::Member { .. }
        | ExprKind::WithMember { .. }
        | ExprKind::Index { .. }
        | ExprKind::Call { .. } => PREC_POSTFIX,
        ExprKind::New { .. } => PREC_UNARY,
        _ => PREC_PRIMARY,
    }
}

fn binary_op_prec(op: BinaryOp) -> u8 {
    match op {
        BinaryOp::LogicalOr => 4,
        BinaryOp::LogicalAnd => 5,
        BinaryOp::BitOr => 6,
        BinaryOp::BitXor => 7,
        BinaryOp::BitAnd => 8,
        BinaryOp::Equal
        | BinaryOp::NotEqual
        | BinaryOp::DiscernEqual
        | BinaryOp::DiscernNotEqual => 9,
        BinaryOp::Less | BinaryOp::Greater | BinaryOp::LessEqual | BinaryOp::GreaterEqual => 10,
        BinaryOp::ShiftArithmeticRight | BinaryOp::ShiftLeft | BinaryOp::ShiftLogicalRight => 11,
        BinaryOp::Add | BinaryOp::Sub => 12,
        BinaryOp::Mod | BinaryOp::Div | BinaryOp::Idiv | BinaryOp::Mul => 13,
        BinaryOp::If => PREC_IF,
        BinaryOp::InContextOf => PREC_INCTX,
        BinaryOp::InstanceOf => PREC_INSTANCEOF,
    }
}

fn binary_op_text(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::LogicalOr => "||",
        BinaryOp::LogicalAnd => "&&",
        BinaryOp::BitOr => "|",
        BinaryOp::BitXor => "^",
        BinaryOp::BitAnd => "&",
        BinaryOp::Equal => "==",
        BinaryOp::NotEqual => "!=",
        BinaryOp::DiscernEqual => "===",
        BinaryOp::DiscernNotEqual => "!==",
        BinaryOp::Less => "<",
        BinaryOp::Greater => ">",
        BinaryOp::LessEqual => "<=",
        BinaryOp::GreaterEqual => ">=",
        BinaryOp::ShiftArithmeticRight => ">>",
        BinaryOp::ShiftLeft => "<<",
        BinaryOp::ShiftLogicalRight => ">>>",
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mod => "%",
        BinaryOp::Div => "/",
        BinaryOp::Idiv => "\\",
        BinaryOp::Mul => "*",
        BinaryOp::If => "if",
        BinaryOp::InContextOf => "incontextof",
        BinaryOp::InstanceOf => "instanceof",
    }
}

fn assign_op_text(op: AssignOp) -> &'static str {
    match op {
        AssignOp::Assign => "=",
        AssignOp::Swap => "<->",
        AssignOp::BitAnd => "&=",
        AssignOp::BitOr => "|=",
        AssignOp::BitXor => "^=",
        AssignOp::Sub => "-=",
        AssignOp::Add => "+=",
        AssignOp::Mod => "%=",
        AssignOp::Div => "/=",
        AssignOp::Idiv => "\\=",
        AssignOp::Mul => "*=",
        AssignOp::LogicalOr => "||=",
        AssignOp::LogicalAnd => "&&=",
        AssignOp::ShiftLogicalRight => ">>>=",
        AssignOp::ShiftLeft => "<<=",
        AssignOp::ShiftArithmeticRight => ">>=",
    }
}

fn print_real(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-Infinity".to_string()
        } else {
            "Infinity".to_string()
        };
    }
    if value == 0.0 && value.is_sign_negative() {
        return "-0.0".to_string();
    }
    // Rust's Debug format for f64 is shortest round-trip, e.g. `1.5`, `1e300`.
    format!("{value:?}")
}

fn print_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\u{0007}' => out.push_str("\\a"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{000b}' => out.push_str("\\v"),
            '\0' => out.push_str("\\0"),
            ch if (ch as u32) < 0x20 || ch == '\u{007f}' => {
                out.push_str(&format!("\\x{:02X}", ch as u32));
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn print_octet(bytes: &[u8]) -> String {
    let mut out = String::from("<%");
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        out.push_str(&format!("{byte:02X}"));
    }
    out.push_str(" %>");
    out
}

fn print_array_literal(printer: &mut Printer, elements: &[ArrayElement], constant: bool) -> String {
    let prefix = if constant { "(const)" } else { "" };
    let parts = elements
        .iter()
        .map(|element| match element {
            ArrayElement::Value(expr) => printer.expr(expr, PREC_ASSIGN),
            ArrayElement::Hole => String::new(),
        })
        .collect::<Vec<_>>();
    let joined = parts.join(", ");
    format!("{prefix}[{joined}]")
}

fn print_dictionary_literal(
    printer: &mut Printer,
    entries: &[DictionaryEntry],
    constant: bool,
) -> String {
    let parts = entries
        .iter()
        .map(|entry| {
            let key = printer.expr(&entry.key, PREC_ASSIGN);
            let value = printer.expr(&entry.value, PREC_ASSIGN);
            if constant {
                format!("{key}, {value}")
            } else {
                format!("{key} => {value}")
            }
        })
        .collect::<Vec<_>>();
    let joined = parts.join(", ");
    if constant {
        format!("(const)%[{joined}]")
    } else {
        format!("%[{joined}]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::parse_source;
    use crate::frontend::syntax::ExprKind as E;

    fn parse(text: &str) -> Program {
        parse_source(text).expect("parse")
    }

    fn shape(expr: &Expr) -> String {
        // A normalized structural rendering used for shape comparison,
        // immune to span/binding identity.
        match &expr.kind {
            E::Void => "void".to_string(),
            E::Null => "null".to_string(),
            E::Bool(v) => format!("bool:{v}"),
            E::Integer(v) => format!("int:{v}"),
            E::Real(v) => format!("real:{v:?}"),
            E::String(v) => format!("str:{v:?}"),
            E::Octet(v) => format!("octet:{v:?}"),
            E::RegExp { pattern, flags } => format!("re:{pattern:?}/{flags:?}"),
            E::Identifier(id) => format!("id:{}", id.name),
            E::This => "this".to_string(),
            E::Super => "super".to_string(),
            E::Global => "global".to_string(),
            E::Nan => "NaN".to_string(),
            E::Infinity => "Infinity".to_string(),
            E::Array(elems) => format!(
                "array:[{}]",
                elems
                    .iter()
                    .map(|el| match el {
                        ArrayElement::Value(v) => shape(v),
                        ArrayElement::Hole => "_".to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            E::ConstArray(elems) => format!(
                "carray:[{}]",
                elems
                    .iter()
                    .map(|el| match el {
                        ArrayElement::Value(v) => shape(v),
                        ArrayElement::Hole => "_".to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            E::Dictionary(entries) => format!(
                "dict:{{{}}}",
                entries
                    .iter()
                    .map(|entry| format!("{}:{}", shape(&entry.key), shape(&entry.value)))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            E::ConstDictionary(entries) => format!(
                "cdict:{{{}}}",
                entries
                    .iter()
                    .map(|entry| format!("{}:{}", shape(&entry.key), shape(&entry.value)))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            E::Unary { op, expr } => format!("unary:{op:?}:{}", shape(expr)),
            E::Binary { op, lhs, rhs } => format!("binary:{op:?}:{}/{}", shape(lhs), shape(rhs)),
            E::Assignment { op, target, value } => {
                format!("assign:{op:?}:{}/{}", shape(target), shape(value))
            }
            E::Conditional {
                condition,
                then_expr,
                else_expr,
            } => format!(
                "cond:{}/{}/{}",
                shape(condition),
                shape(then_expr),
                shape(else_expr)
            ),
            E::Member { object, property } => format!("member:{property}:{}", shape(object)),
            E::WithMember { property } => format!("withmember:{property}"),
            E::Index { object, index } => format!("index:{}/{}", shape(object), shape(index)),
            E::Call { callee, args } => format!(
                "call:{}:[{}]",
                shape(callee),
                args.iter()
                    .map(|arg| match arg {
                        CallArg::Value(v) => format!("v:{}", shape(v)),
                        CallArg::Expand(Some(v)) => format!("e:{}", shape(v)),
                        CallArg::Expand(None) => "e:*".to_string(),
                        CallArg::Omitted => "o".to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            E::New { callee, args } => format!(
                "new:{}:[{}]",
                shape(callee),
                args.iter()
                    .map(|arg| match arg {
                        CallArg::Value(v) => format!("v:{}", shape(v)),
                        CallArg::Expand(Some(v)) => format!("e:{}", shape(v)),
                        CallArg::Expand(None) => "e:*".to_string(),
                        CallArg::Omitted => "o".to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            E::Function(decl) => format!(
                "fn:{}/{}",
                decl.name
                    .as_ref()
                    .map(|name| name.name.clone())
                    .unwrap_or_default(),
                decl.params
                    .iter()
                    .map(|param| format!(
                        "{}{}{}",
                        param
                            .name
                            .as_ref()
                            .map(|n| n.name.clone())
                            .unwrap_or_default(),
                        if param.default.is_some() { "=" } else { "" },
                        if param.collapse { "*" } else { "" }
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            E::Postfix { op, expr } => format!("postfix:{op:?}:{}", shape(expr)),
            E::Comma(exprs) => format!(
                "comma:[{}]",
                exprs.iter().map(shape).collect::<Vec<_>>().join(",")
            ),
        }
    }

    fn round_trip_expr(source: &str) {
        let program = parse(&format!("{source};"));
        let StmtKind::Expr(original) = &program.statements[0].kind else {
            panic!("expected expression statement in {source:?}");
        };
        let printed = print_expression(original);
        let reparsed = parse(&format!("{printed};"));
        let StmtKind::Expr(reparsed) = &reparsed.statements[0].kind else {
            panic!("reparse failed for {printed:?} (source {source:?})");
        };
        assert_eq!(
            shape(original),
            shape(reparsed),
            "shape mismatch for {source:?} printed as {printed:?}"
        );
    }

    /// Span-free structural rendering of a statement.
    fn stmt_shape(stmt: &Stmt) -> String {
        match &stmt.kind {
            StmtKind::Empty => "empty".to_string(),
            StmtKind::Block(body) => format!(
                "block:[{}]",
                body.iter().map(stmt_shape).collect::<Vec<_>>().join(",")
            ),
            StmtKind::Expr(expr) => format!("expr:{}", shape(expr)),
            StmtKind::Var { kind, declarations } => format!(
                "var:{kind:?}:[{}]",
                declarations
                    .iter()
                    .map(|decl| format!(
                        "{}{}{}",
                        decl.name.name,
                        decl.initializer
                            .as_ref()
                            .map(|init| format!("={}", shape(init)))
                            .unwrap_or_default(),
                        decl.ty
                            .as_ref()
                            .map(|ty| format!(":{ty}"))
                            .unwrap_or_default()
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            StmtKind::FunctionDecl(decl) => format!(
                "fn:{}/{}",
                decl.name
                    .as_ref()
                    .map(|name| name.name.clone())
                    .unwrap_or_default(),
                stmt_shape(&decl.body)
            ),
            StmtKind::ClassDecl(decl) => format!(
                "class:{}:[{}]",
                decl.name.name,
                decl.body.iter().map(stmt_shape).collect::<Vec<_>>().join(",")
            ),
            StmtKind::PropertyDecl(decl) => format!(
                "prop:{}:{}/{}",
                decl.name.name,
                decl.getter
                    .as_ref()
                    .map(|getter| stmt_shape(&getter.body))
                    .unwrap_or_default(),
                decl.setter
                    .as_ref()
                    .map(|setter| stmt_shape(&setter.body))
                    .unwrap_or_default()
            ),
            StmtKind::Return(expr) => format!(
                "return:{}",
                expr.as_ref().map(shape).unwrap_or_default()
            ),
            StmtKind::Throw(expr) => format!("throw:{}", shape(expr)),
            StmtKind::If {
                condition,
                then_branch,
                else_branch,
            } => format!(
                "if:{}/{}/{}",
                shape(condition),
                stmt_shape(then_branch),
                else_branch
                    .as_ref()
                    .map(|stmt| stmt_shape(stmt))
                    .unwrap_or_default()
            ),
            StmtKind::While { condition, body } => {
                format!("while:{}/{}", shape(condition), stmt_shape(body))
            }
            StmtKind::DoWhile { body, condition } => {
                format!("do:{}/{}", stmt_shape(body), shape(condition))
            }
            StmtKind::For {
                init,
                condition,
                step,
                body,
            } => format!(
                "for:{}/{}/{}/{}",
                init.as_ref()
                    .map(|init| match init {
                        ForInit::Var { kind, declarations } => format!(
                            "var:{kind:?}:{}",
                            declarations
                                .iter()
                                .map(|decl| decl.name.name.clone())
                                .collect::<Vec<_>>()
                                .join(",")
                        ),
                        ForInit::Expr(expr) => shape(expr),
                    })
                    .unwrap_or_default(),
                condition.as_ref().map(shape).unwrap_or_default(),
                step.as_ref().map(shape).unwrap_or_default(),
                stmt_shape(body)
            ),
            StmtKind::With { object, body } => {
                format!("with:{}/{}", shape(object), stmt_shape(body))
            }
            StmtKind::Break => "break".to_string(),
            StmtKind::Continue => "continue".to_string(),
            StmtKind::Try { body, catch } => format!(
                "try:{}/{}",
                stmt_shape(body),
                catch
                    .as_ref()
                    .map(|catch| format!("catch:{}/{}",
                        catch.binding.as_ref().map(|b| b.name.clone()).unwrap_or_default(),
                        stmt_shape(&catch.body)))
                    .unwrap_or_default()
            ),
            StmtKind::Switch { discriminant, cases } => format!(
                "switch:{}/[{}]",
                shape(discriminant),
                cases
                    .iter()
                    .map(|case| format!(
                        "{}/[{}]",
                        case.test.as_ref().map(shape).unwrap_or_default(),
                        case.body.iter().map(stmt_shape).collect::<Vec<_>>().join(",")
                    ))
                    .collect::<Vec<_>>()
                    .join(";")
            ),
            StmtKind::Case { test } => format!(
                "case:{}",
                test.as_ref().map(shape).unwrap_or_default()
            ),
            StmtKind::Debugger => "debugger".to_string(),
        }
    }

    #[test]
    fn round_trips_literals_and_members() {
        for source in [
            "42",
            "1.5",
            "-3",
            "NaN",
            "Infinity",
            "null",
            "void",
            "true",
            "\"a\\nb\"",
            "'single'",
            "[1, 2, 3]",
            "[1, , 3]",
            "%[\"a\" => 1, 2 => b]",
            "(const)[1, 2]",
            "(const)%[\"x\", 1]",
            "/ab\\/c/gi",
            "<% 00 11 FF %>",
            "a.b.c",
            "a[b]",
            "this",
            "super",
            "global",
            "f()",
            "f(a, b*)",
            "f(*)",
            "f(...)",
            "f(a, , b)",
            "new Foo(1)",
        ] {
            round_trip_expr(source);
        }
    }

    #[test]
    fn round_trips_unary_and_binary_precedence() {
        for source in [
            "-a",
            "- -a",
            "!a && b",
            "a + b * c",
            "(a + b) * c",
            "a - (b - c)",
            "a < b == c",
            "a << b + c",
            "a ? b : c ? d : e",
            "a if b",
            "a, b, c",
            "a = b = c",
            "a = b ? c : d",
            "a && b || c",
            "a instanceof b",
            "a incontextof b",
            "a + b incontextof c",
            "a incontextof b incontextof c",
            "x isvalid",
            "invalidate x",
            "delete a.b",
            "typeof a",
            "&a.b",
            "*a",
            "(int)a",
            "(real)a",
            "(string)a",
            "#a",
            "$a",
            "a++",
            "++a",
            "a!",
            "a <-> b",
            "a += b",
            "a.b += c",
            "a && b",
            "a || b",
            "!a || b",
        ] {
            round_trip_expr(source);
        }
    }

    #[test]
    fn prints_declarations_round_trip() {
        let source = r#"
var x = 1, y : int = 2;
const z = "a";
function f(a, b = 2, c *) : void {
    return a + b;
}
class Base {}
class Derived extends Base, global.Mixin {
    var member = 1;
    function method() { return this.member; }
}
property p {
    getter() { return 1; }
    setter(v) { this.v = v; }
}
var arr = [1, 2, 3];
var dict = %["a" => 1];
return f(1);
"#;
        let program = parse(source);
        let printed = print_program(&program);
        let reparsed = parse(&printed);
        assert_eq!(program.statements.len(), reparsed.statements.len());
        // The printed text must be structurally identical to the original.
        assert_eq!(
            program
                .statements
                .iter()
                .map(stmt_shape)
                .collect::<Vec<_>>(),
            reparsed
                .statements
                .iter()
                .map(stmt_shape)
                .collect::<Vec<_>>(),
        );
        // And printing must be idempotent.
        assert_eq!(printed, print_program(&reparsed));
    }

    #[test]
    fn prints_control_flow_statements() {
        let source = r#"
if (a) {
    b();
} else if (c) {
    d();
} else {
    e();
}
while (a < 3) {
    a++;
}
do {
    a--;
} while (a > 0);
for (var i = 0; i < 10; i++) {
    f(i);
}
for (;;) {
    break;
}
switch (x) {
case 1:
    f();
default:
    g();
}
try {
    h();
} catch (e) {
    throw e;
}
with (obj) {
    .member = 1;
}
"#;
        let program = parse(source);
        let printed = print_program(&program);
        let reparsed = parse(&printed);
        assert_eq!(program.statements.len(), reparsed.statements.len());
    }

    #[test]
    fn wraps_unary_minus_over_binary() {
        let program = parse("-(a + b);");
        let printed = print_program(&program);
        assert!(printed.contains("-(a + b)"), "{printed}");
    }
}
