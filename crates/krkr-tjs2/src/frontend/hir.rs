use crate::error::{Span, TjsErrorKind};

use super::diagnostic::{
    Diagnostic, DiagnosticSeverity, FrontendOptions, FrontendOutput, attach_source_locations,
};
use super::parser;
use super::syntax;

#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    pub statements: Vec<syntax::Stmt>,
    pub span: Span,
    pub scopes: Vec<Scope>,
    pub bindings: Vec<Binding>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Scope {
    pub id: syntax::ScopeId,
    pub kind: ScopeKind,
    pub parent: Option<syntax::ScopeId>,
    pub bindings: Vec<syntax::BindingId>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScopeKind {
    Global,
    Block,
    Function,
    Class,
    Property,
    Catch,
    With,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub id: syntax::BindingId,
    pub scope: syntax::ScopeId,
    pub kind: BindingKind,
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingKind {
    Var,
    Const,
    Function,
    Parameter,
    CollapseParameter,
    Class,
    Property,
    Catch,
}

pub fn analyze_script(
    name: impl Into<String>,
    source: impl AsRef<str>,
    options: FrontendOptions,
) -> FrontendOutput<Program> {
    let source_name = name.into();
    let source = source.as_ref();
    let parsed = parser::parse_script(source_name.clone(), source, options);
    if parsed.has_errors() {
        return FrontendOutput::new(None, parsed.diagnostics);
    }

    let Some(program) = parsed.value else {
        return FrontendOutput::new(None, parsed.diagnostics);
    };

    let mut output = analyze_program(program, parsed.diagnostics);
    output.diagnostics = attach_source_locations(output.diagnostics, &source_name, source);
    output
}

pub fn analyze_program(
    mut program: syntax::Program,
    mut diagnostics: Vec<Diagnostic>,
) -> FrontendOutput<Program> {
    let mut analyzer = Analyzer {
        diagnostics: Vec::new(),
        scopes: Vec::new(),
        bindings: Vec::new(),
        scope_stack: Vec::new(),
        loop_depth: 0,
        switch_depth: 0,
    };
    analyzer.program(&mut program);
    diagnostics.extend(analyzer.diagnostics);

    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
    {
        return FrontendOutput::new(None, diagnostics);
    }

    FrontendOutput::new(
        Some(Program {
            statements: program.statements,
            span: program.span,
            scopes: analyzer.scopes,
            bindings: analyzer.bindings,
        }),
        diagnostics,
    )
}

struct Analyzer {
    diagnostics: Vec<Diagnostic>,
    scopes: Vec<Scope>,
    bindings: Vec<Binding>,
    scope_stack: Vec<syntax::ScopeId>,
    loop_depth: u32,
    switch_depth: u32,
}

enum AnalyzeTask<'a> {
    Stmt(&'a mut syntax::Stmt),
    Expr(&'a mut syntax::Expr),
    ForInit(&'a mut syntax::ForInit),
    Function(&'a mut syntax::FunctionDecl),
    VarDecl {
        kind: syntax::VarKind,
        decl: &'a mut syntax::VarDecl,
        declare_if_unbound: bool,
    },
    DeclareIdent {
        kind: BindingKind,
        ident: &'a mut syntax::Ident,
        span: Span,
        declare_if_unbound: bool,
    },
    EnterScope(ScopeKind, Span),
    LeaveScope,
    EnterLoop,
    LeaveLoop,
    EnterSwitch,
    LeaveSwitch,
    RestoreControl {
        loop_depth: u32,
        switch_depth: u32,
    },
    PredeclareClassMembersAndVisit(&'a mut Vec<syntax::Stmt>),
    CatchClause(&'a mut syntax::CatchClause),
}

impl Analyzer {
    fn program(&mut self, program: &mut syntax::Program) {
        self.enter_scope(ScopeKind::Global, program.span);
        let mut tasks = Vec::new();
        push_statements(&mut tasks, &mut program.statements);
        while let Some(task) = tasks.pop() {
            self.run_task(task, &mut tasks);
        }
        self.leave_scope();
    }

    fn run_task<'a>(&mut self, task: AnalyzeTask<'a>, tasks: &mut Vec<AnalyzeTask<'a>>) {
        match task {
            AnalyzeTask::Stmt(statement) => {
                self.push_statement_tasks(statement, tasks);
            }
            AnalyzeTask::Expr(expr) => {
                self.push_expr_tasks(expr, tasks);
            }
            AnalyzeTask::ForInit(init) => {
                self.push_for_init_tasks(init, tasks);
            }
            AnalyzeTask::Function(decl) => {
                self.push_function_tasks(decl, tasks);
            }
            AnalyzeTask::VarDecl {
                kind,
                decl,
                declare_if_unbound,
            } => {
                let binding_kind = match kind {
                    syntax::VarKind::Var => BindingKind::Var,
                    syntax::VarKind::Const => BindingKind::Const,
                };
                tasks.push(AnalyzeTask::DeclareIdent {
                    kind: binding_kind,
                    ident: &mut decl.name,
                    span: decl.span,
                    declare_if_unbound,
                });
                if let Some(initializer) = &mut decl.initializer {
                    tasks.push(AnalyzeTask::Expr(initializer));
                }
            }
            AnalyzeTask::DeclareIdent {
                kind,
                ident,
                span,
                declare_if_unbound,
            } => {
                if !declare_if_unbound || ident.binding.is_none() {
                    self.declare_ident(ident, kind, span);
                }
            }
            AnalyzeTask::EnterScope(kind, span) => {
                self.enter_scope(kind, span);
            }
            AnalyzeTask::LeaveScope => self.leave_scope(),
            AnalyzeTask::EnterLoop => {
                self.loop_depth += 1;
            }
            AnalyzeTask::LeaveLoop => {
                self.loop_depth -= 1;
            }
            AnalyzeTask::EnterSwitch => {
                self.switch_depth += 1;
            }
            AnalyzeTask::LeaveSwitch => {
                self.switch_depth -= 1;
            }
            AnalyzeTask::RestoreControl {
                loop_depth,
                switch_depth,
            } => {
                self.loop_depth = loop_depth;
                self.switch_depth = switch_depth;
            }
            AnalyzeTask::PredeclareClassMembersAndVisit(body) => {
                for member in body.iter_mut() {
                    self.predeclare_class_member(member);
                }
                push_statements(tasks, body);
            }
            AnalyzeTask::CatchClause(catch) => {
                self.enter_scope(ScopeKind::Catch, catch.span);
                if let Some(binding) = &mut catch.binding {
                    self.declare_ident(binding, BindingKind::Catch, catch.span);
                }
                tasks.push(AnalyzeTask::LeaveScope);
                tasks.push(AnalyzeTask::Stmt(catch.body.as_mut()));
            }
        }
    }

    fn push_statement_tasks<'a>(
        &mut self,
        statement: &'a mut syntax::Stmt,
        tasks: &mut Vec<AnalyzeTask<'a>>,
    ) {
        match &mut statement.kind {
            syntax::StmtKind::Empty | syntax::StmtKind::Debugger => {}
            syntax::StmtKind::Break => {
                if self.loop_depth == 0 && self.switch_depth == 0 {
                    self.error(
                        statement.span,
                        "break statement is not inside a loop or switch",
                    );
                }
            }
            syntax::StmtKind::Continue => {
                if self.loop_depth == 0 {
                    self.error(statement.span, "continue statement is not inside a loop");
                }
            }
            syntax::StmtKind::Case { test } => {
                if self.switch_depth == 0 {
                    self.error(statement.span, "case statement is not inside a switch");
                }
                if let Some(test) = test {
                    tasks.push(AnalyzeTask::Expr(test));
                }
            }
            syntax::StmtKind::Block(statements) => {
                tasks.push(AnalyzeTask::LeaveScope);
                push_statements(tasks, statements);
                tasks.push(AnalyzeTask::EnterScope(ScopeKind::Block, statement.span));
            }
            syntax::StmtKind::Expr(expr)
            | syntax::StmtKind::Return(Some(expr))
            | syntax::StmtKind::Throw(expr) => tasks.push(AnalyzeTask::Expr(expr)),
            syntax::StmtKind::Return(None) => {}
            syntax::StmtKind::Var { kind, declarations } => {
                for declaration in declarations.iter_mut().rev() {
                    tasks.push(AnalyzeTask::VarDecl {
                        kind: *kind,
                        decl: declaration,
                        declare_if_unbound: true,
                    });
                }
            }
            syntax::StmtKind::FunctionDecl(decl) => {
                if let Some(name) = &mut decl.name
                    && name.binding.is_none()
                {
                    self.declare_ident(name, BindingKind::Function, decl.span);
                }
                tasks.push(AnalyzeTask::Function(decl));
            }
            syntax::StmtKind::ClassDecl(decl) => {
                if decl.name.binding.is_none() {
                    self.declare_ident(&mut decl.name, BindingKind::Class, decl.span);
                }
                tasks.push(AnalyzeTask::LeaveScope);
                tasks.push(AnalyzeTask::PredeclareClassMembersAndVisit(&mut decl.body));
                push_exprs(tasks, &mut decl.extends);
                tasks.push(AnalyzeTask::EnterScope(ScopeKind::Class, decl.span));
            }
            syntax::StmtKind::PropertyDecl(decl) => {
                if decl.name.binding.is_none() {
                    self.declare_ident(&mut decl.name, BindingKind::Property, decl.span);
                }
                tasks.push(AnalyzeTask::LeaveScope);
                if let Some(setter) = &mut decl.setter {
                    tasks.push(AnalyzeTask::Function(setter));
                }
                if let Some(getter) = &mut decl.getter {
                    tasks.push(AnalyzeTask::Function(getter));
                }
                tasks.push(AnalyzeTask::EnterScope(ScopeKind::Property, decl.span));
            }
            syntax::StmtKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                if let Some(else_branch) = else_branch {
                    tasks.push(AnalyzeTask::Stmt(else_branch.as_mut()));
                }
                tasks.push(AnalyzeTask::Stmt(then_branch.as_mut()));
                tasks.push(AnalyzeTask::Expr(condition));
            }
            syntax::StmtKind::While { condition, body }
            | syntax::StmtKind::DoWhile { condition, body } => {
                tasks.push(AnalyzeTask::LeaveLoop);
                tasks.push(AnalyzeTask::Stmt(body.as_mut()));
                tasks.push(AnalyzeTask::EnterLoop);
                tasks.push(AnalyzeTask::Expr(condition));
            }
            syntax::StmtKind::For {
                init,
                condition,
                step,
                body,
            } => {
                tasks.push(AnalyzeTask::LeaveLoop);
                tasks.push(AnalyzeTask::Stmt(body.as_mut()));
                tasks.push(AnalyzeTask::EnterLoop);
                if let Some(step) = step {
                    tasks.push(AnalyzeTask::Expr(step));
                }
                if let Some(condition) = condition {
                    tasks.push(AnalyzeTask::Expr(condition));
                }
                if let Some(init) = init {
                    tasks.push(AnalyzeTask::ForInit(init));
                }
            }
            syntax::StmtKind::With { object, body } => {
                let body_span = body.span;
                tasks.push(AnalyzeTask::LeaveScope);
                tasks.push(AnalyzeTask::Stmt(body.as_mut()));
                tasks.push(AnalyzeTask::EnterScope(ScopeKind::With, body_span));
                tasks.push(AnalyzeTask::Expr(object));
            }
            syntax::StmtKind::Try { body, catch } => {
                if let Some(catch) = catch {
                    tasks.push(AnalyzeTask::CatchClause(catch));
                }
                tasks.push(AnalyzeTask::Stmt(body.as_mut()));
            }
            syntax::StmtKind::Switch {
                discriminant,
                cases,
            } => {
                tasks.push(AnalyzeTask::LeaveSwitch);
                for case in cases.iter_mut().rev() {
                    push_statements(tasks, &mut case.body);
                    if let Some(test) = &mut case.test {
                        tasks.push(AnalyzeTask::Expr(test));
                    }
                }
                tasks.push(AnalyzeTask::EnterSwitch);
                tasks.push(AnalyzeTask::Expr(discriminant));
            }
        }
    }

    fn predeclare_class_member(&mut self, statement: &mut syntax::Stmt) {
        match &mut statement.kind {
            syntax::StmtKind::Var { kind, declarations } => {
                let binding_kind = match kind {
                    syntax::VarKind::Var => BindingKind::Var,
                    syntax::VarKind::Const => BindingKind::Const,
                };
                for declaration in declarations {
                    if declaration.name.binding.is_none() {
                        self.declare_ident(&mut declaration.name, binding_kind, declaration.span);
                    }
                }
            }
            syntax::StmtKind::FunctionDecl(decl) => {
                if let Some(name) = &mut decl.name
                    && name.binding.is_none()
                {
                    self.declare_ident(name, BindingKind::Function, decl.span);
                }
            }
            syntax::StmtKind::ClassDecl(decl) => {
                if decl.name.binding.is_none() {
                    self.declare_ident(&mut decl.name, BindingKind::Class, decl.span);
                }
            }
            syntax::StmtKind::PropertyDecl(decl) => {
                if decl.name.binding.is_none() {
                    self.declare_ident(&mut decl.name, BindingKind::Property, decl.span);
                }
            }
            _ => {}
        }
    }

    fn push_function_tasks<'a>(
        &mut self,
        decl: &'a mut syntax::FunctionDecl,
        tasks: &mut Vec<AnalyzeTask<'a>>,
    ) {
        self.validate_params(decl);
        let outer_loop_depth = self.loop_depth;
        let outer_switch_depth = self.switch_depth;
        self.loop_depth = 0;
        self.switch_depth = 0;
        self.enter_scope(ScopeKind::Function, decl.span);
        for param in &mut decl.params {
            if let Some(name) = &mut param.name {
                let kind = if param.collapse {
                    BindingKind::CollapseParameter
                } else {
                    BindingKind::Parameter
                };
                self.declare_ident(name, kind, param.span);
            }
        }
        tasks.push(AnalyzeTask::RestoreControl {
            loop_depth: outer_loop_depth,
            switch_depth: outer_switch_depth,
        });
        tasks.push(AnalyzeTask::LeaveScope);
        tasks.push(AnalyzeTask::Stmt(decl.body.as_mut()));
        for param in decl.params.iter_mut().rev() {
            if let Some(default) = &mut param.default {
                tasks.push(AnalyzeTask::Expr(default));
            }
        }
    }

    fn validate_params(&mut self, decl: &syntax::FunctionDecl) {
        let mut collapse_seen = false;
        for (index, param) in decl.params.iter().enumerate() {
            if !param.collapse {
                if collapse_seen {
                    self.error(param.span, "parameter follows a collapse parameter");
                }
                continue;
            }
            collapse_seen = true;
            if param.default.is_some() {
                self.error(param.span, "collapse parameter cannot have a default value");
            }
            if index + 1 != decl.params.len() {
                self.error(param.span, "collapse parameter must be the final parameter");
            }
        }
    }

    fn push_for_init_tasks<'a>(
        &mut self,
        init: &'a mut syntax::ForInit,
        tasks: &mut Vec<AnalyzeTask<'a>>,
    ) {
        match init {
            syntax::ForInit::Var { kind, declarations } => {
                for declaration in declarations.iter_mut().rev() {
                    tasks.push(AnalyzeTask::VarDecl {
                        kind: *kind,
                        decl: declaration,
                        declare_if_unbound: false,
                    });
                }
            }
            syntax::ForInit::Expr(expr) => tasks.push(AnalyzeTask::Expr(expr)),
        }
    }

    fn push_expr_tasks<'a>(
        &mut self,
        expr: &'a mut syntax::Expr,
        tasks: &mut Vec<AnalyzeTask<'a>>,
    ) {
        match &mut expr.kind {
            syntax::ExprKind::Void
            | syntax::ExprKind::Null
            | syntax::ExprKind::Bool(_)
            | syntax::ExprKind::Integer(_)
            | syntax::ExprKind::Real(_)
            | syntax::ExprKind::String(_)
            | syntax::ExprKind::Octet(_)
            | syntax::ExprKind::RegExp { .. }
            | syntax::ExprKind::This
            | syntax::ExprKind::Super
            | syntax::ExprKind::Global
            | syntax::ExprKind::Nan
            | syntax::ExprKind::Infinity
            | syntax::ExprKind::WithMember { .. } => {}
            syntax::ExprKind::Identifier(ident) => {
                ident.binding = self.find_binding(&ident.name);
            }
            syntax::ExprKind::Array(elements) | syntax::ExprKind::ConstArray(elements) => {
                for element in elements.iter_mut().rev() {
                    if let syntax::ArrayElement::Value(expr) = element {
                        tasks.push(AnalyzeTask::Expr(expr));
                    }
                }
            }
            syntax::ExprKind::Dictionary(entries) | syntax::ExprKind::ConstDictionary(entries) => {
                for entry in entries.iter_mut().rev() {
                    tasks.push(AnalyzeTask::Expr(&mut entry.value));
                    tasks.push(AnalyzeTask::Expr(&mut entry.key));
                }
            }
            syntax::ExprKind::Unary { op, expr: inner } => {
                if matches!(
                    op,
                    syntax::UnaryOp::Delete
                        | syntax::UnaryOp::Invalidate
                        | syntax::UnaryOp::Increment
                        | syntax::UnaryOp::Decrement
                ) {
                    self.require_lvalue(inner, expr.span);
                }
                tasks.push(AnalyzeTask::Expr(inner.as_mut()));
            }
            syntax::ExprKind::Binary { lhs, rhs, .. } => {
                tasks.push(AnalyzeTask::Expr(rhs.as_mut()));
                tasks.push(AnalyzeTask::Expr(lhs.as_mut()));
            }
            syntax::ExprKind::Assignment { target, value, .. } => {
                self.require_lvalue(target, target.span);
                tasks.push(AnalyzeTask::Expr(value.as_mut()));
                tasks.push(AnalyzeTask::Expr(target.as_mut()));
            }
            syntax::ExprKind::Conditional {
                condition,
                then_expr,
                else_expr,
            } => {
                tasks.push(AnalyzeTask::Expr(else_expr.as_mut()));
                tasks.push(AnalyzeTask::Expr(then_expr.as_mut()));
                tasks.push(AnalyzeTask::Expr(condition.as_mut()));
            }
            syntax::ExprKind::Member { object, .. } => {
                tasks.push(AnalyzeTask::Expr(object.as_mut()));
            }
            syntax::ExprKind::Index { object, index } => {
                tasks.push(AnalyzeTask::Expr(index.as_mut()));
                tasks.push(AnalyzeTask::Expr(object.as_mut()));
            }
            syntax::ExprKind::Call { callee, args } | syntax::ExprKind::New { callee, args } => {
                for arg in args.iter_mut().rev() {
                    match arg {
                        syntax::CallArg::Value(expr) | syntax::CallArg::Expand(Some(expr)) => {
                            tasks.push(AnalyzeTask::Expr(expr));
                        }
                        syntax::CallArg::Expand(None) | syntax::CallArg::Omitted => {}
                    }
                }
                tasks.push(AnalyzeTask::Expr(callee.as_mut()));
            }
            syntax::ExprKind::Function(decl) => tasks.push(AnalyzeTask::Function(decl.as_mut())),
            syntax::ExprKind::Postfix { op, expr: inner } => {
                if matches!(op, syntax::UnaryOp::Increment | syntax::UnaryOp::Decrement) {
                    self.require_lvalue(inner, expr.span);
                }
                tasks.push(AnalyzeTask::Expr(inner.as_mut()));
            }
            syntax::ExprKind::Comma(exprs) => {
                push_exprs(tasks, exprs);
            }
        }
    }

    fn enter_scope(&mut self, kind: ScopeKind, span: Span) -> syntax::ScopeId {
        let id = syntax::ScopeId(self.scopes.len());
        let parent = self.scope_stack.last().copied();
        self.scopes.push(Scope {
            id,
            kind,
            parent,
            bindings: Vec::new(),
            span,
        });
        self.scope_stack.push(id);
        id
    }

    fn leave_scope(&mut self) {
        self.scope_stack.pop();
    }

    fn declare_ident(
        &mut self,
        ident: &mut syntax::Ident,
        kind: BindingKind,
        span: Span,
    ) -> syntax::BindingId {
        let scope = *self
            .scope_stack
            .last()
            .expect("analyzer always has an active scope");
        let id = syntax::BindingId(self.bindings.len());
        self.bindings.push(Binding {
            id,
            scope,
            kind,
            name: ident.name.clone(),
            span,
        });
        self.scopes[scope.0].bindings.push(id);
        ident.bind(id);
        id
    }

    fn find_binding(&self, name: &str) -> Option<syntax::BindingId> {
        for scope in self.scope_stack.iter().rev() {
            for binding in self.scopes[scope.0].bindings.iter().rev() {
                if self.bindings[binding.0].name == name {
                    return Some(*binding);
                }
            }
        }
        None
    }

    fn require_lvalue(&mut self, expr: &syntax::Expr, span: Span) {
        if is_lvalue(expr) {
            return;
        }
        self.diagnostics.push(Diagnostic::error(
            TjsErrorKind::Parse,
            Some(span),
            "expression is not assignable",
        ));
    }

    fn error(&mut self, span: Span, message: impl Into<String>) {
        self.diagnostics
            .push(Diagnostic::error(TjsErrorKind::Parse, Some(span), message));
    }
}

fn push_statements<'a>(tasks: &mut Vec<AnalyzeTask<'a>>, statements: &'a mut [syntax::Stmt]) {
    for statement in statements.iter_mut().rev() {
        tasks.push(AnalyzeTask::Stmt(statement));
    }
}

fn push_exprs<'a>(tasks: &mut Vec<AnalyzeTask<'a>>, exprs: &'a mut [syntax::Expr]) {
    for expr in exprs.iter_mut().rev() {
        tasks.push(AnalyzeTask::Expr(expr));
    }
}

fn is_lvalue(expr: &syntax::Expr) -> bool {
    let mut current = expr;
    loop {
        match &current.kind {
            syntax::ExprKind::Identifier(_)
            | syntax::ExprKind::Member { .. }
            | syntax::ExprKind::WithMember { .. }
            | syntax::ExprKind::Index { .. } => return true,
            syntax::ExprKind::Unary {
                op: syntax::UnaryOp::IgnoreProp | syntax::UnaryOp::PropAccess,
                expr,
            } => current = expr,
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BindingKind, analyze_program};
    use crate::Span;
    use crate::frontend::syntax::ExprKind;
    use crate::frontend::syntax::{Program as SyntaxProgram, Stmt, StmtKind};
    use crate::{FrontendOptions, analyze_script};

    #[test]
    fn analyze_script_returns_hir_for_valid_source() {
        let output = analyze_script(
            "inline.tjs",
            "var x = 1; x = x + 1;",
            FrontendOptions::default(),
        );
        assert!(output.diagnostics.is_empty());
        assert!(output.value.is_some());
    }

    #[test]
    fn analyze_script_reports_non_lvalue_assignment() {
        let output = analyze_script("inline.tjs", "1 = 2;", FrontendOptions::default());
        assert!(output.value.is_none());
        assert!(
            output
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("not assignable"))
        );
    }

    #[test]
    fn analyze_script_reports_obvious_control_context_errors() {
        let output = analyze_script("inline.tjs", "break;", FrontendOptions::default());
        assert!(output.value.is_none());
        assert!(
            output
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("not inside"))
        );

        let output = analyze_script(
            "inline.tjs",
            "while (true) { break; continue; } switch (x) { case 1: break; }",
            FrontendOptions::default(),
        );
        assert!(output.value.is_some());
        assert!(output.diagnostics.is_empty());
    }

    #[test]
    fn analyze_script_annotates_identifier_bindings() {
        let output = analyze_script(
            "inline.tjs",
            "var x = 1; x = x + 1; { var x = x; } x;",
            FrontendOptions::default(),
        );
        assert!(output.diagnostics.is_empty());
        let program = output.value.expect("hir");

        let StmtKind::Var { declarations, .. } = &program.statements[0].kind else {
            panic!("expected var");
        };
        let outer_x = declarations[0].name.binding.expect("outer binding");
        assert_eq!(program.bindings[outer_x.0].kind, BindingKind::Var);
        assert_eq!(program.bindings[outer_x.0].name, "x");

        let StmtKind::Expr(assign) = &program.statements[1].kind else {
            panic!("expected assignment");
        };
        let ExprKind::Assignment { target, value, .. } = &assign.kind else {
            panic!("expected assignment");
        };
        assert!(matches!(
            &target.kind,
            ExprKind::Identifier(ident) if ident.binding == Some(outer_x)
        ));
        assert!(matches!(
            &value.kind,
            ExprKind::Binary { lhs, .. }
                if matches!(&lhs.kind, ExprKind::Identifier(ident) if ident.binding == Some(outer_x))
        ));

        let StmtKind::Block(block) = &program.statements[2].kind else {
            panic!("expected block");
        };
        let StmtKind::Var { declarations, .. } = &block[0].kind else {
            panic!("expected block var");
        };
        let block_x = declarations[0].name.binding.expect("block binding");
        assert_ne!(outer_x, block_x);
        assert!(matches!(
            declarations[0].initializer.as_ref().map(|expr| &expr.kind),
            Some(ExprKind::Identifier(ident)) if ident.binding == Some(outer_x)
        ));

        let StmtKind::Expr(expr) = &program.statements[3].kind else {
            panic!("expected trailing expr");
        };
        assert!(matches!(
            &expr.kind,
            ExprKind::Identifier(ident) if ident.binding == Some(outer_x)
        ));
    }

    #[test]
    fn analyze_script_records_function_class_property_and_catch_bindings() {
        let output = analyze_script(
            "inline.tjs",
            r#"
            function f(a, rest*) { return a; }
            class C { property value { getter { return 1; } } }
            try { throw 1; } catch (e) { e; }
            missing;
            "#,
            FrontendOptions::default(),
        );
        assert!(output.diagnostics.is_empty());
        let program = output.value.expect("hir");
        let names = program
            .bindings
            .iter()
            .map(|binding| (binding.name.as_str(), binding.kind))
            .collect::<Vec<_>>();
        assert!(names.contains(&("f", BindingKind::Function)));
        assert!(names.contains(&("a", BindingKind::Parameter)));
        assert!(names.contains(&("rest", BindingKind::CollapseParameter)));
        assert!(names.contains(&("C", BindingKind::Class)));
        assert!(names.contains(&("value", BindingKind::Property)));
        assert!(names.contains(&("e", BindingKind::Catch)));

        let StmtKind::Expr(expr) = &program.statements[3].kind else {
            panic!("expected dynamic expr");
        };
        assert!(matches!(
            &expr.kind,
            ExprKind::Identifier(ident) if ident.name == "missing" && ident.binding.is_none()
        ));
    }

    #[test]
    fn analyze_diagnostics_include_source_locations() {
        let output = analyze_script("foo.tjs", "\n break;", FrontendOptions::default());
        let diagnostic = output.diagnostics.first().expect("diagnostic");
        assert_eq!(diagnostic.source_name.as_deref(), Some("foo.tjs"));
        assert_eq!(diagnostic.start.map(|location| location.line), Some(2));
        assert_eq!(diagnostic.start.map(|location| location.column), Some(2));
    }

    #[test]
    fn analyze_program_handles_deep_block_nesting_iteratively() {
        let span = Span::empty(0);
        let mut statement = Stmt::new(StmtKind::Empty, span);
        for _ in 0..2048 {
            statement = Stmt::new(StmtKind::Block(vec![statement]), span);
        }
        let output = analyze_program(
            SyntaxProgram {
                statements: vec![statement],
                span,
            },
            Vec::new(),
        );
        assert!(output.diagnostics.is_empty());
        assert!(output.value.is_some());
    }
}
