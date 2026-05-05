use crate::error::{Result, Span, TjsError};

use super::diagnostic::{Diagnostic, FrontendOptions, FrontendOutput, attach_source_locations};
use super::lexer::{InterpolatedPart, Token, TokenKind, lex};
use super::syntax::*;

pub fn parse(source: &str) -> Result<Program> {
    Parser::new(lex(source)?).parse_program()
}

pub fn parse_script(
    name: impl Into<String>,
    source: impl AsRef<str>,
    options: FrontendOptions,
) -> FrontendOutput<Program> {
    let source_name = name.into();
    let source = source.as_ref();
    let tokens = match lex(source) {
        Ok(tokens) => tokens,
        Err(error) => {
            return FrontendOutput::new(
                None,
                attach_source_locations(vec![Diagnostic::from(error)], &source_name, source),
            );
        }
    };
    let mut parser = Parser::new(tokens);
    if options.recover {
        let (program, diagnostics) = parser.parse_program_recovering();
        FrontendOutput::new(
            Some(program),
            attach_source_locations(diagnostics, &source_name, source),
        )
    } else {
        match parser.parse_program() {
            Ok(program) => FrontendOutput::ok(program),
            Err(error) => FrontendOutput::new(
                None,
                attach_source_locations(vec![Diagnostic::from(error)], &source_name, source),
            ),
        }
    }
}

pub fn parse_expression(
    name: impl Into<String>,
    source: impl AsRef<str>,
    _options: FrontendOptions,
) -> FrontendOutput<Expr> {
    let source_name = name.into();
    let source = source.as_ref();
    let tokens = match lex(source) {
        Ok(tokens) => tokens,
        Err(error) => {
            return FrontendOutput::new(
                None,
                attach_source_locations(vec![Diagnostic::from(error)], &source_name, source),
            );
        }
    };
    let mut parser = Parser::new_with_mode(tokens, ParseMode::Expression);
    match parser.parse_expression().and_then(|expr| {
        let _ = parser.consume(&TokenKind::Semicolon);
        parser.expect(&TokenKind::Eof).map(|_| expr)
    }) {
        Ok(expr) => FrontendOutput::ok(expr),
        Err(error) => FrontendOutput::new(
            None,
            attach_source_locations(vec![Diagnostic::from(error)], &source_name, source),
        ),
    }
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    mode: ParseMode,
    dicfunc_quick_hack: bool,
    dicfunc_used: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ParseMode {
    Script,
    Expression,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self::new_with_mode(tokens, ParseMode::Script)
    }

    fn new_with_mode(tokens: Vec<Token>, mode: ParseMode) -> Self {
        let starts_with_dicfunc_literal = matches!(
            tokens.first().map(|token| &token.kind),
            Some(TokenKind::LeftBracket)
        ) || matches!(
            (
                tokens.first().map(|token| &token.kind),
                tokens.get(1).map(|token| &token.kind)
            ),
            (Some(TokenKind::Percent), Some(TokenKind::LeftBracket))
        );
        let dicfunc_quick_hack = mode == ParseMode::Expression && starts_with_dicfunc_literal;
        Self {
            tokens,
            pos: 0,
            mode,
            dicfunc_quick_hack,
            dicfunc_used: false,
        }
    }

    fn parse_program(&mut self) -> Result<Program> {
        let start = self.current().span.start;
        let mut statements = Vec::new();
        while !self.is(&TokenKind::Eof) {
            statements.push(self.parse_statement()?);
        }
        let end = self.current().span.end;
        Ok(Program {
            statements,
            span: Span::new(start, end),
        })
    }

    fn parse_program_recovering(&mut self) -> (Program, Vec<Diagnostic>) {
        let start = self.current().span.start;
        let mut statements = Vec::new();
        let mut diagnostics = Vec::new();
        while !self.is(&TokenKind::Eof) {
            let before = self.pos;
            match self.parse_statement() {
                Ok(statement) => statements.push(statement),
                Err(error) => {
                    diagnostics.push(Diagnostic::from(error));
                    self.synchronize_statement();
                    if self.pos == before && !self.is(&TokenKind::Eof) {
                        self.advance();
                    }
                }
            }
        }
        let end = self.current().span.end;
        (
            Program {
                statements,
                span: Span::new(start, end),
            },
            diagnostics,
        )
    }

    fn parse_statement(&mut self) -> Result<Stmt> {
        match &self.current().kind {
            TokenKind::Semicolon => {
                let token = self.advance().clone();
                Ok(Stmt::new(StmtKind::Empty, token.span))
            }
            TokenKind::LeftBrace => self.parse_block_statement(),
            TokenKind::KwVar => self.parse_var_statement(VarKind::Var),
            TokenKind::KwConst => self.parse_var_statement(VarKind::Const),
            TokenKind::KwFunction => self.parse_function_statement(),
            TokenKind::KwClass => self.parse_class_statement(),
            TokenKind::KwProperty => self.parse_property_statement(),
            TokenKind::KwReturn => self.parse_return_statement(),
            TokenKind::KwThrow => self.parse_throw_statement(),
            TokenKind::KwIf => self.parse_if_statement(),
            TokenKind::KwWhile => self.parse_while_statement(),
            TokenKind::KwDo => self.parse_do_while_statement(),
            TokenKind::KwFor => self.parse_for_statement(),
            TokenKind::KwWith => self.parse_with_statement(),
            TokenKind::KwBreak => self.parse_keyword_semicolon_statement(StmtKind::Break),
            TokenKind::KwContinue => self.parse_keyword_semicolon_statement(StmtKind::Continue),
            TokenKind::KwTry => self.parse_try_statement(),
            TokenKind::KwSwitch => self.parse_switch_statement(),
            TokenKind::KwCase | TokenKind::KwDefault => self.parse_case_statement(),
            TokenKind::KwDebugger => self.parse_keyword_semicolon_statement(StmtKind::Debugger),
            kind if reserved_keyword_text(kind).is_some() => Err(TjsError::parse(
                self.current().span,
                format!(
                    "reserved TJS2 keyword {} is unsupported",
                    reserved_keyword_text(kind).expect("checked")
                ),
            )),
            _ => self.parse_expression_statement(),
        }
    }

    fn parse_block_statement(&mut self) -> Result<Stmt> {
        let start = self.expect(&TokenKind::LeftBrace)?.span.start;
        let mut statements = Vec::new();
        while !self.is(&TokenKind::RightBrace) && !self.is(&TokenKind::Eof) {
            statements.push(self.parse_statement()?);
        }
        let end = self.expect(&TokenKind::RightBrace)?.span.end;
        Ok(Stmt::new(
            StmtKind::Block(statements),
            Span::new(start, end),
        ))
    }

    fn parse_var_statement(&mut self, kind: VarKind) -> Result<Stmt> {
        let start = self.advance().span.start;
        let declarations = self.parse_var_declarations()?;
        let end = self.expect_statement_semicolon()?.span.end;
        Ok(Stmt::new(
            StmtKind::Var { kind, declarations },
            Span::new(start, end),
        ))
    }

    fn parse_var_declarations(&mut self) -> Result<Vec<VarDecl>> {
        let mut declarations = Vec::new();
        loop {
            let name = self.expect_identifier_text()?;
            let start = self.previous().span.start;
            let ty = self.parse_optional_type()?;
            let initializer = if self.consume(&TokenKind::Assign).is_some() {
                Some(self.parse_expression_no_comma()?)
            } else {
                None
            };
            let end = initializer
                .as_ref()
                .map(|expr| expr.span.end)
                .unwrap_or(self.previous().span.end);
            declarations.push(VarDecl {
                name: Ident::new(name),
                ty,
                initializer,
                span: Span::new(start, end),
            });

            if !self.consume_comma_separator() {
                break;
            }
        }
        Ok(declarations)
    }

    fn parse_function_statement(&mut self) -> Result<Stmt> {
        let decl = self.parse_function_decl(true)?;
        let span = decl.span;
        Ok(Stmt::new(StmtKind::FunctionDecl(decl), span))
    }

    fn parse_function_decl(&mut self, require_name: bool) -> Result<FunctionDecl> {
        let start = self.expect(&TokenKind::KwFunction)?.span.start;
        let name = match &self.current().kind {
            TokenKind::Ident(_) => Some(Ident::new(self.expect_identifier_text()?)),
            _ if require_name => {
                return Err(TjsError::parse(
                    self.current().span,
                    "expected function name",
                ));
            }
            _ => None,
        };
        let params = if self.consume(&TokenKind::LeftParen).is_some() {
            self.parse_param_list()?
        } else {
            Vec::new()
        };
        let return_type = self.parse_optional_type()?;
        let body = Box::new(self.parse_block_statement()?);
        let span = Span::new(start, body.span.end);
        Ok(FunctionDecl {
            name,
            params,
            return_type,
            body,
            span,
        })
    }

    fn parse_param_list(&mut self) -> Result<Vec<ParamDecl>> {
        let mut params = Vec::new();
        if self.consume(&TokenKind::RightParen).is_some() {
            return Ok(params);
        }

        loop {
            let start = self.current().span.start;
            if self.consume(&TokenKind::Star).is_some() {
                params.push(ParamDecl {
                    name: None,
                    ty: None,
                    default: None,
                    collapse: true,
                    span: Span::new(start, self.previous().span.end),
                });
                if !self.is(&TokenKind::RightParen) {
                    return Err(TjsError::parse(
                        self.current().span,
                        "collapse parameter must be the final parameter",
                    ));
                }
                break;
            } else {
                let name = self.expect_identifier_text()?;
                let ty = self.parse_optional_type()?;
                let default = if self.consume(&TokenKind::Assign).is_some() {
                    Some(self.parse_expression_no_comma()?)
                } else {
                    None
                };
                let has_default = default.is_some();
                let collapse = self.consume(&TokenKind::Star).is_some();
                let end = if collapse {
                    self.previous().span.end
                } else {
                    default
                        .as_ref()
                        .map(|expr| expr.span.end)
                        .unwrap_or(self.previous().span.end)
                };
                params.push(ParamDecl {
                    name: Some(Ident::new(name)),
                    ty,
                    default,
                    collapse,
                    span: Span::new(start, end),
                });
                if collapse {
                    if has_default {
                        return Err(TjsError::parse(
                            params.last().expect("pushed").span,
                            "collapse parameter cannot have a default value",
                        ));
                    }
                    if !self.is(&TokenKind::RightParen) {
                        return Err(TjsError::parse(
                            self.current().span,
                            "collapse parameter must be the final parameter",
                        ));
                    }
                    break;
                }
            }

            if !self.consume_comma_separator() {
                break;
            }
        }

        self.expect(&TokenKind::RightParen)?;
        Ok(params)
    }

    fn parse_class_statement(&mut self) -> Result<Stmt> {
        let start = self.expect(&TokenKind::KwClass)?.span.start;
        let name = self.expect_identifier_text()?;
        let mut extends = Vec::new();
        if self.consume(&TokenKind::KwExtends).is_some() {
            extends.push(self.parse_expression_no_comma()?);
            while self.consume_comma_separator() {
                extends.push(self.parse_expression_no_comma()?);
            }
        }
        self.expect(&TokenKind::LeftBrace)?;
        let mut body = Vec::new();
        while !self.is(&TokenKind::RightBrace) && !self.is(&TokenKind::Eof) {
            body.push(self.parse_statement()?);
        }
        let end = self.expect(&TokenKind::RightBrace)?.span.end;
        let decl = ClassDecl {
            name: Ident::new(name),
            extends,
            body,
            span: Span::new(start, end),
        };
        Ok(Stmt::new(StmtKind::ClassDecl(decl), Span::new(start, end)))
    }

    fn parse_property_statement(&mut self) -> Result<Stmt> {
        let start = self.expect(&TokenKind::KwProperty)?.span.start;
        let name = self.expect_identifier_text()?;
        self.expect(&TokenKind::LeftBrace)?;
        let mut getter = None;
        let mut setter = None;

        while !self.is(&TokenKind::RightBrace) && !self.is(&TokenKind::Eof) {
            if self.consume(&TokenKind::KwSetter).is_some() {
                if setter.is_some() {
                    return Err(TjsError::parse(
                        self.previous().span,
                        "property declaration has duplicate setter",
                    ));
                }
                setter = Some(self.parse_property_accessor(format!("set {name}"), true)?);
            } else if self.consume(&TokenKind::KwGetter).is_some() {
                if getter.is_some() {
                    return Err(TjsError::parse(
                        self.previous().span,
                        "property declaration has duplicate getter",
                    ));
                }
                getter = Some(self.parse_property_accessor(format!("get {name}"), false)?);
            } else {
                return Err(TjsError::parse(
                    self.current().span,
                    "expected getter or setter in property declaration",
                ));
            }
        }

        let end = self.expect(&TokenKind::RightBrace)?.span.end;
        if getter.is_none() && setter.is_none() {
            return Err(TjsError::parse(
                Span::new(start, end),
                "property declaration requires a getter or setter",
            ));
        }
        let decl = PropertyDecl {
            name: Ident::new(name),
            getter,
            setter,
            span: Span::new(start, end),
        };
        Ok(Stmt::new(
            StmtKind::PropertyDecl(decl),
            Span::new(start, end),
        ))
    }

    fn parse_property_accessor(&mut self, name: String, is_setter: bool) -> Result<FunctionDecl> {
        let start = self.previous().span.start;
        let params = if is_setter {
            self.expect(&TokenKind::LeftParen)?;
            let binding = self.expect_identifier_text()?;
            let ty = self.parse_optional_type()?;
            self.expect(&TokenKind::RightParen)?;
            vec![ParamDecl {
                name: Some(Ident::new(binding)),
                ty,
                default: None,
                collapse: false,
                span: Span::new(start, self.previous().span.end),
            }]
        } else if self.consume(&TokenKind::LeftParen).is_some() {
            self.expect(&TokenKind::RightParen)?;
            Vec::new()
        } else {
            Vec::new()
        };
        let return_type = self.parse_optional_type()?;
        let body = Box::new(self.parse_block_statement()?);
        let span = Span::new(start, body.span.end);
        Ok(FunctionDecl {
            name: Some(Ident::new(name)),
            params,
            return_type,
            body,
            span,
        })
    }

    fn parse_return_statement(&mut self) -> Result<Stmt> {
        let start = self.expect(&TokenKind::KwReturn)?.span.start;
        let value = if self.is(&TokenKind::Semicolon) {
            None
        } else {
            Some(self.parse_expression()?)
        };
        let end = self.expect_statement_semicolon()?.span.end;
        Ok(Stmt::new(StmtKind::Return(value), Span::new(start, end)))
    }

    fn parse_throw_statement(&mut self) -> Result<Stmt> {
        let start = self.expect(&TokenKind::KwThrow)?.span.start;
        let value = self.parse_expression()?;
        let end = self.expect_statement_semicolon()?.span.end;
        Ok(Stmt::new(StmtKind::Throw(value), Span::new(start, end)))
    }

    fn parse_if_statement(&mut self) -> Result<Stmt> {
        let start = self.expect(&TokenKind::KwIf)?.span.start;
        self.expect(&TokenKind::LeftParen)?;
        let condition = self.parse_expression()?;
        self.expect(&TokenKind::RightParen)?;
        let then_branch = Box::new(self.parse_statement()?);
        let else_branch = if self.consume(&TokenKind::KwElse).is_some() {
            Some(Box::new(self.parse_statement()?))
        } else {
            None
        };
        let end = else_branch
            .as_ref()
            .map(|stmt| stmt.span.end)
            .unwrap_or(then_branch.span.end);
        Ok(Stmt::new(
            StmtKind::If {
                condition,
                then_branch,
                else_branch,
            },
            Span::new(start, end),
        ))
    }

    fn parse_while_statement(&mut self) -> Result<Stmt> {
        let start = self.expect(&TokenKind::KwWhile)?.span.start;
        self.expect(&TokenKind::LeftParen)?;
        let condition = self.parse_expression()?;
        self.expect(&TokenKind::RightParen)?;
        let body = Box::new(self.parse_statement()?);
        let end = body.span.end;
        Ok(Stmt::new(
            StmtKind::While { condition, body },
            Span::new(start, end),
        ))
    }

    fn parse_do_while_statement(&mut self) -> Result<Stmt> {
        let start = self.expect(&TokenKind::KwDo)?.span.start;
        let body = Box::new(self.parse_statement()?);
        self.expect(&TokenKind::KwWhile)?;
        self.expect(&TokenKind::LeftParen)?;
        let condition = self.parse_expression()?;
        self.expect(&TokenKind::RightParen)?;
        let end = self.expect_statement_semicolon()?.span.end;
        Ok(Stmt::new(
            StmtKind::DoWhile { body, condition },
            Span::new(start, end),
        ))
    }

    fn parse_for_statement(&mut self) -> Result<Stmt> {
        let start = self.expect(&TokenKind::KwFor)?.span.start;
        self.expect(&TokenKind::LeftParen)?;
        let init = if self.consume(&TokenKind::Semicolon).is_some() {
            None
        } else if self.consume(&TokenKind::KwVar).is_some() {
            let declarations = self.parse_var_declarations()?;
            self.expect(&TokenKind::Semicolon)?;
            Some(ForInit::Var {
                kind: VarKind::Var,
                declarations,
            })
        } else if self.consume(&TokenKind::KwConst).is_some() {
            let declarations = self.parse_var_declarations()?;
            self.expect(&TokenKind::Semicolon)?;
            Some(ForInit::Var {
                kind: VarKind::Const,
                declarations,
            })
        } else {
            let expr = self.parse_expression()?;
            self.expect(&TokenKind::Semicolon)?;
            Some(ForInit::Expr(expr))
        };

        let condition = if self.is(&TokenKind::Semicolon) {
            None
        } else {
            Some(self.parse_expression()?)
        };
        self.expect(&TokenKind::Semicolon)?;

        let step = if self.is(&TokenKind::RightParen) {
            None
        } else {
            Some(self.parse_expression()?)
        };
        self.expect(&TokenKind::RightParen)?;
        let body = Box::new(self.parse_statement()?);
        let end = body.span.end;
        Ok(Stmt::new(
            StmtKind::For {
                init,
                condition,
                step,
                body,
            },
            Span::new(start, end),
        ))
    }

    fn parse_with_statement(&mut self) -> Result<Stmt> {
        let start = self.expect(&TokenKind::KwWith)?.span.start;
        self.expect(&TokenKind::LeftParen)?;
        let object = self.parse_expression()?;
        self.expect(&TokenKind::RightParen)?;
        let body = Box::new(self.parse_statement()?);
        let end = body.span.end;
        Ok(Stmt::new(
            StmtKind::With { object, body },
            Span::new(start, end),
        ))
    }

    fn parse_keyword_semicolon_statement(&mut self, kind: StmtKind) -> Result<Stmt> {
        let start = self.advance().span.start;
        let end = self.expect_statement_semicolon()?.span.end;
        Ok(Stmt::new(kind, Span::new(start, end)))
    }

    fn parse_try_statement(&mut self) -> Result<Stmt> {
        let start = self.expect(&TokenKind::KwTry)?.span.start;
        let body = Box::new(self.parse_statement()?);
        if self.is(&TokenKind::KwFinally) {
            return Err(TjsError::parse(
                self.current().span,
                "TJS2 try statements use catch; finally is reserved but unsupported",
            ));
        }
        let catch = if self.consume(&TokenKind::KwCatch).is_some() {
            let catch_start = self.previous().span.start;
            let binding = if self.consume(&TokenKind::LeftParen).is_some() {
                let name = if self.is(&TokenKind::RightParen) {
                    None
                } else {
                    Some(Ident::new(self.expect_identifier_text()?))
                };
                self.expect(&TokenKind::RightParen)?;
                name
            } else {
                None
            };
            let catch_body = Box::new(self.parse_statement()?);
            let span = Span::new(catch_start, catch_body.span.end);
            Some(CatchClause {
                binding,
                body: catch_body,
                span,
            })
        } else {
            return Err(TjsError::parse(
                self.current().span,
                "expected catch clause after try body",
            ));
        };
        let end = catch
            .as_ref()
            .map(|clause| clause.span.end)
            .unwrap_or(body.span.end);
        Ok(Stmt::new(
            StmtKind::Try { body, catch },
            Span::new(start, end),
        ))
    }

    fn parse_switch_statement(&mut self) -> Result<Stmt> {
        let start = self.expect(&TokenKind::KwSwitch)?.span.start;
        self.expect(&TokenKind::LeftParen)?;
        let discriminant = self.parse_expression()?;
        self.expect(&TokenKind::RightParen)?;
        self.expect(&TokenKind::LeftBrace)?;
        let mut cases = Vec::new();
        while !self.is(&TokenKind::RightBrace) && !self.is(&TokenKind::Eof) {
            let case_start = self.current().span.start;
            let test = if self.consume(&TokenKind::KwCase).is_some() {
                let expr = self.parse_expression()?;
                self.expect(&TokenKind::Colon)?;
                Some(expr)
            } else {
                self.expect(&TokenKind::KwDefault)?;
                self.expect(&TokenKind::Colon)?;
                None
            };
            let mut body = Vec::new();
            while !self.is(&TokenKind::KwCase)
                && !self.is(&TokenKind::KwDefault)
                && !self.is(&TokenKind::RightBrace)
                && !self.is(&TokenKind::Eof)
            {
                body.push(self.parse_statement()?);
            }
            let end = body
                .last()
                .map(|stmt| stmt.span.end)
                .unwrap_or(self.previous().span.end);
            cases.push(SwitchCase {
                test,
                body,
                span: Span::new(case_start, end),
            });
        }
        let end = self.expect(&TokenKind::RightBrace)?.span.end;
        Ok(Stmt::new(
            StmtKind::Switch {
                discriminant,
                cases,
            },
            Span::new(start, end),
        ))
    }

    fn parse_case_statement(&mut self) -> Result<Stmt> {
        let start = self.current().span.start;
        let test = if self.consume(&TokenKind::KwCase).is_some() {
            let expr = self.parse_expression()?;
            self.expect(&TokenKind::Colon)?;
            Some(expr)
        } else {
            self.expect(&TokenKind::KwDefault)?;
            self.expect(&TokenKind::Colon)?;
            None
        };
        let end = self.previous().span.end;
        Ok(Stmt::new(StmtKind::Case { test }, Span::new(start, end)))
    }

    fn parse_expression_statement(&mut self) -> Result<Stmt> {
        let expr = self.parse_expression()?;
        let start = expr.span.start;
        let end = self.expect_statement_semicolon()?.span.end;
        Ok(Stmt::new(StmtKind::Expr(expr), Span::new(start, end)))
    }

    fn parse_expression(&mut self) -> Result<Expr> {
        let mut expr = self.parse_comma()?;
        if self.consume(&TokenKind::KwIf).is_some() {
            let rhs = self.parse_expression()?;
            let span = expr.span.join(rhs.span);
            expr = Expr::new(
                ExprKind::Binary {
                    op: BinaryOp::If,
                    lhs: Box::new(expr),
                    rhs: Box::new(rhs),
                },
                span,
            );
        }
        Ok(expr)
    }

    fn parse_expression_no_comma(&mut self) -> Result<Expr> {
        self.parse_assignment()
    }

    fn parse_comma(&mut self) -> Result<Expr> {
        let first = self.parse_assignment()?;
        if !self.consume_comma_separator() {
            return Ok(first);
        }

        let mut exprs = vec![first];
        loop {
            exprs.push(self.parse_assignment()?);
            if !self.consume_comma_separator() {
                break;
            }
        }
        let span = exprs
            .first()
            .expect("first expression")
            .span
            .join(exprs.last().expect("last expression").span);
        Ok(Expr::new(ExprKind::Comma(exprs), span))
    }

    fn parse_assignment(&mut self) -> Result<Expr> {
        let target = self.parse_conditional()?;
        let Some(op) = self.assignment_op() else {
            return Ok(target);
        };
        self.advance();
        let value = self.parse_assignment()?;
        let span = target.span.join(value.span);
        Ok(Expr::new(
            ExprKind::Assignment {
                op,
                target: Box::new(target),
                value: Box::new(value),
            },
            span,
        ))
    }

    fn parse_conditional(&mut self) -> Result<Expr> {
        let condition = self.parse_binary(0)?;
        if self.consume(&TokenKind::Question).is_none() {
            return Ok(condition);
        }
        let then_expr = self.parse_conditional()?;
        self.expect(&TokenKind::Colon)?;
        let else_expr = self.parse_conditional()?;
        let span = condition.span.join(else_expr.span);
        Ok(Expr::new(
            ExprKind::Conditional {
                condition: Box::new(condition),
                then_expr: Box::new(then_expr),
                else_expr: Box::new(else_expr),
            },
            span,
        ))
    }

    fn parse_binary(&mut self, min_prec: u8) -> Result<Expr> {
        let mut lhs = self.parse_unary()?;
        while let Some((op, prec)) = self.binary_op() {
            if prec < min_prec {
                break;
            }
            self.advance();
            let rhs = self.parse_binary(prec + 1)?;
            let span = lhs.span.join(rhs.span);
            lhs = Expr::new(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            );
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr> {
        if self.is_cast_expression() {
            let start = self.expect(&TokenKind::LeftParen)?.span.start;
            let op = self.type_unary_op().expect("checked cast operator");
            self.advance();
            self.expect(&TokenKind::RightParen)?;
            let expr = self.parse_unary()?;
            let span = Span::new(start, expr.span.end);
            return Ok(Expr::new(
                ExprKind::Unary {
                    op,
                    expr: Box::new(expr),
                },
                span,
            ));
        }

        if self.is(&TokenKind::KwNew) {
            return self.parse_new_expression();
        }

        if let Some(op) = self.unary_op() {
            let start = self.advance().span.start;
            let expr = self.parse_unary()?;
            let span = Span::new(start, expr.span.end);
            return Ok(Expr::new(
                ExprKind::Unary {
                    op,
                    expr: Box::new(expr),
                },
                span,
            ));
        }

        let lhs = self.parse_incontextof()?;
        if self.consume(&TokenKind::KwIsValid).is_some() {
            let span = lhs.span.join(self.previous().span);
            return Ok(Expr::new(
                ExprKind::Unary {
                    op: UnaryOp::IsValid,
                    expr: Box::new(lhs),
                },
                span,
            ));
        }
        if self.consume(&TokenKind::KwInstanceOf).is_some() {
            let rhs = self.parse_unary()?;
            let span = lhs.span.join(rhs.span);
            return Ok(Expr::new(
                ExprKind::Binary {
                    op: BinaryOp::InstanceOf,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            ));
        }
        Ok(lhs)
    }

    fn parse_incontextof(&mut self) -> Result<Expr> {
        let lhs = self.parse_postfix()?;
        if self.consume(&TokenKind::KwInContextOf).is_none() {
            return Ok(lhs);
        }
        let rhs = self.parse_incontextof()?;
        let span = lhs.span.join(rhs.span);
        Ok(Expr::new(
            ExprKind::Binary {
                op: BinaryOp::InContextOf,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            span,
        ))
    }

    fn parse_new_expression(&mut self) -> Result<Expr> {
        let start = self.expect(&TokenKind::KwNew)?.span.start;
        let call = self.parse_postfix()?;
        let end = call.span.end;
        let ExprKind::Call { callee, args } = call.kind else {
            return Err(TjsError::parse(call.span, "new requires a call expression"));
        };
        Ok(Expr::new(
            ExprKind::New { callee, args },
            Span::new(start, end),
        ))
    }

    fn parse_postfix(&mut self) -> Result<Expr> {
        let expr = self.parse_primary()?;
        self.parse_postfix_suffixes(expr, true)
    }

    fn parse_postfix_suffixes(&mut self, mut expr: Expr, allow_calls: bool) -> Result<Expr> {
        loop {
            if self.consume(&TokenKind::Dot).is_some() {
                let property = self.expect_member_name()?;
                let span = expr.span.join(self.previous().span);
                expr = Expr::new(
                    ExprKind::Member {
                        object: Box::new(expr),
                        property,
                    },
                    span,
                );
            } else if self.consume(&TokenKind::LeftBracket).is_some() {
                let index = self.parse_expression()?;
                let end = self.expect(&TokenKind::RightBracket)?.span.end;
                let span = Span::new(expr.span.start, end);
                expr = Expr::new(
                    ExprKind::Index {
                        object: Box::new(expr),
                        index: Box::new(index),
                    },
                    span,
                );
            } else if allow_calls && self.consume(&TokenKind::LeftParen).is_some() {
                let args = self.parse_call_args()?;
                let span = Span::new(expr.span.start, self.previous().span.end);
                expr = Expr::new(
                    ExprKind::Call {
                        callee: Box::new(expr),
                        args,
                    },
                    span,
                );
            } else if self.consume(&TokenKind::Bang).is_some() {
                let span = expr.span.join(self.previous().span);
                expr = Expr::new(
                    ExprKind::Postfix {
                        op: UnaryOp::Eval,
                        expr: Box::new(expr),
                    },
                    span,
                );
            } else if self.consume(&TokenKind::Increment).is_some() {
                let span = expr.span.join(self.previous().span);
                expr = Expr::new(
                    ExprKind::Postfix {
                        op: UnaryOp::Increment,
                        expr: Box::new(expr),
                    },
                    span,
                );
            } else if self.consume(&TokenKind::Decrement).is_some() {
                let span = expr.span.join(self.previous().span);
                expr = Expr::new(
                    ExprKind::Postfix {
                        op: UnaryOp::Decrement,
                        expr: Box::new(expr),
                    },
                    span,
                );
            } else {
                break;
            }
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr> {
        let token = self.advance().clone();
        let kind = match token.kind {
            TokenKind::Integer(value) => ExprKind::Integer(value),
            TokenKind::Real(value) => ExprKind::Real(value),
            TokenKind::String(value) => {
                return self.parse_string_literal_sequence(TokenKind::String(value), token.span);
            }
            TokenKind::InterpolatedString(parts) => {
                return self.parse_string_literal_sequence(
                    TokenKind::InterpolatedString(parts),
                    token.span,
                );
            }
            TokenKind::Octet(value) => ExprKind::Octet(value),
            TokenKind::RegExp { pattern, flags } => ExprKind::RegExp { pattern, flags },
            TokenKind::Ident(value) => ExprKind::Identifier(Ident::new(value)),
            TokenKind::KwTrue => ExprKind::Bool(true),
            TokenKind::KwFalse => ExprKind::Bool(false),
            TokenKind::KwNull => ExprKind::Null,
            TokenKind::KwVoid => ExprKind::Void,
            TokenKind::KwThis => ExprKind::This,
            TokenKind::KwSuper => ExprKind::Super,
            TokenKind::KwGlobal => ExprKind::Global,
            TokenKind::KwNan => ExprKind::Nan,
            TokenKind::KwInfinity => ExprKind::Infinity,
            TokenKind::KwFunction => {
                self.pos = self.pos.saturating_sub(1);
                return self.parse_function_expression();
            }
            TokenKind::LeftParen => {
                if self.consume(&TokenKind::KwConst).is_some() {
                    self.expect(&TokenKind::RightParen)?;
                    if self.consume(&TokenKind::LeftBracket).is_some() {
                        return self.parse_array_literal(token.span.start, true);
                    }
                    if self.consume(&TokenKind::Percent).is_some() {
                        return self.parse_dictionary_literal(token.span.start, true);
                    }
                    return Err(TjsError::parse(
                        self.current().span,
                        "expected const array or dictionary literal",
                    ));
                }
                let expr = self.parse_expression()?;
                let end = self.expect(&TokenKind::RightParen)?.span.end;
                return Ok(Expr::new(expr.kind, Span::new(token.span.start, end)));
            }
            TokenKind::LeftBracket => {
                let expr = self.parse_array_literal(token.span.start, false)?;
                return Ok(self.wrap_dicfunc_literal_if_needed(expr));
            }
            TokenKind::Percent if self.is(&TokenKind::LeftBracket) => {
                let expr = self.parse_dictionary_literal(token.span.start, false)?;
                return Ok(self.wrap_dicfunc_literal_if_needed(expr));
            }
            TokenKind::Dot => {
                let property = self.expect_member_name()?;
                return Ok(Expr::new(
                    ExprKind::WithMember { property },
                    Span::new(token.span.start, self.previous().span.end),
                ));
            }
            other if reserved_keyword_text(&other).is_some() => {
                return Err(TjsError::parse(
                    token.span,
                    format!(
                        "reserved TJS2 keyword {} is unsupported",
                        reserved_keyword_text(&other).expect("checked")
                    ),
                ));
            }
            other => {
                return Err(TjsError::parse(
                    token.span,
                    format!("expected expression, found {other:?}"),
                ));
            }
        };
        Ok(Expr::new(kind, token.span))
    }

    fn wrap_dicfunc_literal_if_needed(&mut self, expr: Expr) -> Expr {
        if !self.dicfunc_quick_hack || self.dicfunc_used {
            return expr;
        }
        self.dicfunc_used = true;
        let span = expr.span;
        let return_stmt = Stmt::new(StmtKind::Return(Some(expr)), span);
        let body = Stmt::new(StmtKind::Block(vec![return_stmt]), span);
        let decl = FunctionDecl {
            name: None,
            params: Vec::new(),
            return_type: None,
            body: Box::new(body),
            span,
        };
        let callee = Expr::new(ExprKind::Function(Box::new(decl)), span);
        Expr::new(
            ExprKind::Call {
                callee: Box::new(callee),
                args: Vec::new(),
            },
            span,
        )
    }

    fn parse_function_expression(&mut self) -> Result<Expr> {
        if matches!(self.nth(1).kind, TokenKind::Ident(_)) {
            return Err(TjsError::parse(
                self.nth(1).span,
                "function expressions are anonymous in TJS2",
            ));
        }
        let decl = self.parse_function_decl(false)?;
        let span = decl.span;
        Ok(Expr::new(ExprKind::Function(Box::new(decl)), span))
    }

    fn parse_string_literal_sequence(
        &mut self,
        first_kind: TokenKind,
        first_span: Span,
    ) -> Result<Expr> {
        let mut exprs = Vec::new();
        self.append_string_literal_token(&mut exprs, first_kind, first_span)?;

        let mut exprs = exprs.into_iter();
        let Some(mut expr) = exprs.next() else {
            return Ok(Expr::new(ExprKind::String(String::new()), first_span));
        };
        for rhs in exprs {
            let joined = expr.span.join(rhs.span);
            expr = Expr::new(
                ExprKind::Binary {
                    op: BinaryOp::Add,
                    lhs: Box::new(expr),
                    rhs: Box::new(rhs),
                },
                joined,
            );
        }
        Ok(expr)
    }

    fn append_string_literal_token(
        &mut self,
        exprs: &mut Vec<Expr>,
        kind: TokenKind,
        span: Span,
    ) -> Result<()> {
        match kind {
            TokenKind::String(text) => {
                Self::push_string_segment(exprs, text, span);
            }
            TokenKind::InterpolatedString(parts) => {
                self.append_interpolated_string_parts(exprs, parts, span)?;
            }
            _ => unreachable!("only string-like tokens are passed to append_string_literal_token"),
        }
        Ok(())
    }

    fn append_interpolated_string_parts(
        &mut self,
        exprs: &mut Vec<Expr>,
        parts: Vec<InterpolatedPart>,
        span: Span,
    ) -> Result<()> {
        for part in parts {
            match part {
                InterpolatedPart::Text(text) => {
                    Self::push_string_segment(exprs, text, span);
                }
                InterpolatedPart::Expr(source) => {
                    if source.trim().is_empty() {
                        Self::push_string_segment(exprs, String::new(), span);
                        continue;
                    }
                    let mut parser = Parser::new(lex(&source)?);
                    let expr = parser.parse_expression()?;
                    parser.expect(&TokenKind::Eof)?;
                    exprs.push(Expr::new(
                        ExprKind::Unary {
                            op: UnaryOp::AsString,
                            expr: Box::new(expr),
                        },
                        span,
                    ));
                }
            }
        }
        Ok(())
    }

    fn push_string_segment(exprs: &mut Vec<Expr>, text: String, span: Span) {
        if text.is_empty() && !exprs.is_empty() {
            return;
        }
        if let Some(last) = exprs.last_mut()
            && let ExprKind::String(value) = &mut last.kind
        {
            value.push_str(&text);
            last.span = last.span.join(span);
            return;
        }
        exprs.push(Expr::new(ExprKind::String(text), span));
    }

    fn parse_array_literal(&mut self, start: usize, constant: bool) -> Result<Expr> {
        let mut elements = Vec::new();
        if self.consume(&TokenKind::RightBracket).is_some() {
            let kind = if constant {
                ExprKind::ConstArray(elements)
            } else {
                ExprKind::Array(elements)
            };
            return Ok(Expr::new(kind, Span::new(start, self.previous().span.end)));
        }

        if constant {
            loop {
                elements.push(ArrayElement::Value(self.parse_const_literal_expr()?));
                if !self.consume_comma_separator() {
                    break;
                }
                if self.is(&TokenKind::RightBracket) {
                    break;
                }
            }
            let end = self.expect(&TokenKind::RightBracket)?.span.end;
            return Ok(Expr::new(
                ExprKind::ConstArray(elements),
                Span::new(start, end),
            ));
        }

        loop {
            if self.is(&TokenKind::Comma) || self.is(&TokenKind::RightBracket) {
                elements.push(ArrayElement::Hole);
            } else {
                elements.push(ArrayElement::Value(self.parse_expression_no_comma()?));
            }

            if !self.consume_comma_separator() {
                break;
            }
            if self.is(&TokenKind::RightBracket) {
                elements.push(ArrayElement::Hole);
                break;
            }
        }
        let end = self.expect(&TokenKind::RightBracket)?.span.end;
        let kind = if constant {
            ExprKind::ConstArray(elements)
        } else {
            ExprKind::Array(elements)
        };
        Ok(Expr::new(kind, Span::new(start, end)))
    }

    fn parse_dictionary_literal(&mut self, start: usize, constant: bool) -> Result<Expr> {
        self.expect(&TokenKind::LeftBracket)?;
        let mut entries = Vec::new();
        while !self.is(&TokenKind::RightBracket) && !self.is(&TokenKind::Eof) {
            let entry_start = self.current().span.start;
            let (key, value) = if constant {
                let key = self.parse_const_dictionary_key()?;
                self.expect(&TokenKind::Comma)?;
                let value = self.parse_const_literal_expr()?;
                (key, value)
            } else {
                let mut key = self.parse_expression_no_comma()?;
                let colon_style = self.consume(&TokenKind::Colon).is_some();
                if colon_style {
                    let ExprKind::Identifier(name) = &key.kind else {
                        return Err(TjsError::parse(
                            key.span,
                            "colon dictionary keys must be bare symbols",
                        ));
                    };
                    key = Expr::new(ExprKind::String(name.name.clone()), key.span);
                } else if self.consume(&TokenKind::FatArrow).is_none()
                    && self.consume(&TokenKind::Comma).is_none()
                {
                    return Err(TjsError::parse(
                        self.current().span,
                        "expected dictionary key/value separator",
                    ));
                }
                let value = self.parse_expression_no_comma()?;
                (key, value)
            };
            let span = Span::new(entry_start, value.span.end);
            entries.push(DictionaryEntry { key, value, span });
            if !self.consume_comma_separator() {
                break;
            }
            if constant && self.is(&TokenKind::RightBracket) {
                break;
            }
        }
        let end = self.expect(&TokenKind::RightBracket)?.span.end;
        let kind = if constant {
            ExprKind::ConstDictionary(entries)
        } else {
            ExprKind::Dictionary(entries)
        };
        Ok(Expr::new(kind, Span::new(start, end)))
    }

    fn parse_const_dictionary_key(&mut self) -> Result<Expr> {
        let token = self.advance().clone();
        let kind = match token.kind {
            TokenKind::Integer(value) => ExprKind::Integer(value),
            TokenKind::Real(value) => ExprKind::Real(value),
            TokenKind::String(value) => ExprKind::String(value),
            TokenKind::Octet(value) => ExprKind::Octet(value),
            TokenKind::KwTrue => ExprKind::Bool(true),
            TokenKind::KwFalse => ExprKind::Bool(false),
            TokenKind::KwNull => ExprKind::Null,
            TokenKind::KwNan => ExprKind::Nan,
            TokenKind::KwInfinity => ExprKind::Real(f64::INFINITY),
            other => {
                return Err(TjsError::parse(
                    token.span,
                    format!("expected constant dictionary key, found {other:?}"),
                ));
            }
        };
        Ok(Expr::new(kind, token.span))
    }

    fn parse_const_literal_expr(&mut self) -> Result<Expr> {
        let start = self.current().span.start;
        let sign = if self.consume(&TokenKind::Minus).is_some() {
            Some(-1.0)
        } else if self.consume(&TokenKind::Plus).is_some() {
            Some(1.0)
        } else {
            None
        };

        let token = self.advance().clone();
        let mut expr = match token.kind {
            TokenKind::Integer(value) => {
                let value = if sign == Some(-1.0) { -value } else { value };
                Expr::new(ExprKind::Integer(value), Span::new(start, token.span.end))
            }
            TokenKind::Real(value) => {
                let value = if sign == Some(-1.0) { -value } else { value };
                Expr::new(ExprKind::Real(value), Span::new(start, token.span.end))
            }
            TokenKind::String(value) if sign.is_none() => {
                Expr::new(ExprKind::String(value), token.span)
            }
            TokenKind::Octet(value) if sign.is_none() => {
                Expr::new(ExprKind::Octet(value), token.span)
            }
            TokenKind::KwTrue if sign.is_none() => Expr::new(ExprKind::Bool(true), token.span),
            TokenKind::KwFalse if sign.is_none() => Expr::new(ExprKind::Bool(false), token.span),
            TokenKind::KwNull if sign.is_none() => Expr::new(ExprKind::Null, token.span),
            TokenKind::KwVoid if sign.is_none() => Expr::new(ExprKind::Void, token.span),
            TokenKind::KwNan if sign.is_none() => Expr::new(ExprKind::Nan, token.span),
            TokenKind::KwInfinity => {
                let value = if sign == Some(-1.0) {
                    f64::NEG_INFINITY
                } else {
                    f64::INFINITY
                };
                Expr::new(ExprKind::Real(value), Span::new(start, token.span.end))
            }
            TokenKind::LeftParen
                if sign.is_none() && self.consume(&TokenKind::KwConst).is_some() =>
            {
                self.expect(&TokenKind::RightParen)?;
                if self.consume(&TokenKind::LeftBracket).is_some() {
                    return self.parse_array_literal(start, true);
                }
                if self.consume(&TokenKind::Percent).is_some() {
                    return self.parse_dictionary_literal(start, true);
                }
                return Err(TjsError::parse(
                    self.current().span,
                    "expected constant array or dictionary literal",
                ));
            }
            other => {
                return Err(TjsError::parse(
                    token.span,
                    format!("expected constant literal element, found {other:?}"),
                ));
            }
        };
        if sign.is_some() && !matches!(expr.kind, ExprKind::Integer(_) | ExprKind::Real(_)) {
            return Err(TjsError::parse(
                expr.span,
                "constant literal sign requires a numeric element",
            ));
        }
        expr.span = Span::new(start, expr.span.end);
        Ok(expr)
    }

    fn parse_call_args(&mut self) -> Result<Vec<CallArg>> {
        let mut args = Vec::new();
        if self.consume(&TokenKind::RightParen).is_some() {
            return Ok(args);
        }
        if self.consume(&TokenKind::Ellipsis).is_some() {
            self.expect(&TokenKind::RightParen)?;
            return Ok(vec![CallArg::Omitted]);
        }

        loop {
            args.push(self.parse_call_arg()?);

            if !self.consume_comma_separator() {
                break;
            }
        }
        self.expect(&TokenKind::RightParen)?;
        Ok(args)
    }

    fn parse_call_arg(&mut self) -> Result<CallArg> {
        if self.is_comma_separator() || self.is(&TokenKind::RightParen) {
            return Ok(CallArg::Value(Expr::new(
                ExprKind::Void,
                Span::empty(self.current().span.start),
            )));
        }

        if let Some(star_pos) = self.call_arg_trailing_star_pos() {
            if star_pos == self.pos {
                self.advance();
                return Ok(CallArg::Expand(None));
            }

            let mut tokens = self.tokens[self.pos..star_pos].to_vec();
            tokens.push(Token {
                kind: TokenKind::Eof,
                span: Span::empty(self.tokens[star_pos].span.start),
            });
            let mut parser = Parser::new(tokens);
            let expr = parser.parse_binary(10)?;
            parser.expect(&TokenKind::Eof)?;
            self.pos = star_pos + 1;
            return Ok(CallArg::Expand(Some(expr)));
        }

        Ok(CallArg::Value(self.parse_expression_no_comma()?))
    }

    fn call_arg_trailing_star_pos(&self) -> Option<usize> {
        let mut paren_depth = 0_u32;
        let mut bracket_depth = 0_u32;
        let mut brace_depth = 0_u32;
        let mut last_top_level = None;
        let mut index = self.pos;

        while let Some(token) = self.tokens.get(index) {
            if paren_depth == 0
                && bracket_depth == 0
                && brace_depth == 0
                && matches!(
                    &token.kind,
                    TokenKind::Comma | TokenKind::RightParen | TokenKind::Eof
                )
            {
                break;
            }

            if paren_depth == 0 && bracket_depth == 0 && brace_depth == 0 {
                last_top_level = Some(index);
            }

            match &token.kind {
                TokenKind::LeftParen => paren_depth += 1,
                TokenKind::RightParen => paren_depth = paren_depth.saturating_sub(1),
                TokenKind::LeftBracket => bracket_depth += 1,
                TokenKind::RightBracket => bracket_depth = bracket_depth.saturating_sub(1),
                TokenKind::LeftBrace => brace_depth += 1,
                TokenKind::RightBrace => brace_depth = brace_depth.saturating_sub(1),
                _ => {}
            }
            index += 1;
        }

        last_top_level.filter(|index| matches!(&self.tokens[*index].kind, TokenKind::Star))
    }

    fn parse_optional_type(&mut self) -> Result<Option<String>> {
        if self.consume(&TokenKind::Colon).is_none() {
            return Ok(None);
        }
        let token = self.advance().clone();
        let ty = match token.kind {
            TokenKind::Ident(text) => text,
            TokenKind::KwVoid => "void".to_string(),
            TokenKind::KwInt => "int".to_string(),
            TokenKind::KwReal => "real".to_string(),
            TokenKind::KwString => "string".to_string(),
            TokenKind::KwOctet => "octet".to_string(),
            other => {
                return Err(TjsError::parse(
                    token.span,
                    format!("expected type name, found {other:?}"),
                ));
            }
        };
        Ok(Some(ty))
    }

    fn assignment_op(&self) -> Option<AssignOp> {
        Some(match self.current().kind {
            TokenKind::Assign => AssignOp::Assign,
            TokenKind::Swap => AssignOp::Swap,
            TokenKind::AmpAssign => AssignOp::BitAnd,
            TokenKind::PipeAssign => AssignOp::BitOr,
            TokenKind::CaretAssign => AssignOp::BitXor,
            TokenKind::MinusAssign => AssignOp::Sub,
            TokenKind::PlusAssign => AssignOp::Add,
            TokenKind::PercentAssign => AssignOp::Mod,
            TokenKind::SlashAssign => AssignOp::Div,
            TokenKind::BackslashAssign => AssignOp::Idiv,
            TokenKind::StarAssign => AssignOp::Mul,
            TokenKind::LogicalOrAssign => AssignOp::LogicalOr,
            TokenKind::LogicalAndAssign => AssignOp::LogicalAnd,
            TokenKind::UnsignedShiftRightAssign => AssignOp::ShiftLogicalRight,
            TokenKind::ShiftLeftAssign => AssignOp::ShiftLeft,
            TokenKind::ShiftRightAssign => AssignOp::ShiftArithmeticRight,
            _ => return None,
        })
    }

    fn binary_op(&self) -> Option<(BinaryOp, u8)> {
        Some(match self.current().kind {
            TokenKind::LogicalOr => (BinaryOp::LogicalOr, 1),
            TokenKind::LogicalAnd => (BinaryOp::LogicalAnd, 2),
            TokenKind::Pipe => (BinaryOp::BitOr, 3),
            TokenKind::Caret => (BinaryOp::BitXor, 4),
            TokenKind::Amp => (BinaryOp::BitAnd, 5),
            TokenKind::EqualEqual => (BinaryOp::Equal, 6),
            TokenKind::NotEqual => (BinaryOp::NotEqual, 6),
            TokenKind::DiscernEqual => (BinaryOp::DiscernEqual, 6),
            TokenKind::DiscernNotEqual => (BinaryOp::DiscernNotEqual, 6),
            TokenKind::Less => (BinaryOp::Less, 7),
            TokenKind::Greater => (BinaryOp::Greater, 7),
            TokenKind::LessEqual => (BinaryOp::LessEqual, 7),
            TokenKind::GreaterEqual => (BinaryOp::GreaterEqual, 7),
            TokenKind::ShiftRight => (BinaryOp::ShiftArithmeticRight, 8),
            TokenKind::ShiftLeft => (BinaryOp::ShiftLeft, 8),
            TokenKind::UnsignedShiftRight => (BinaryOp::ShiftLogicalRight, 8),
            TokenKind::Plus => (BinaryOp::Add, 9),
            TokenKind::Minus => (BinaryOp::Sub, 9),
            TokenKind::Percent => (BinaryOp::Mod, 10),
            TokenKind::Slash => (BinaryOp::Div, 10),
            TokenKind::Backslash => (BinaryOp::Idiv, 10),
            TokenKind::Star => (BinaryOp::Mul, 10),
            _ => return None,
        })
    }

    fn unary_op(&self) -> Option<UnaryOp> {
        Some(match self.current().kind {
            TokenKind::Plus => UnaryOp::Plus,
            TokenKind::Minus => UnaryOp::Minus,
            TokenKind::Bang => UnaryOp::LogicalNot,
            TokenKind::Tilde => UnaryOp::BitNot,
            TokenKind::KwDelete => UnaryOp::Delete,
            TokenKind::KwTypeOf => UnaryOp::TypeOf,
            TokenKind::KwIsValid => UnaryOp::IsValid,
            TokenKind::KwInvalidate => UnaryOp::Invalidate,
            TokenKind::Amp => UnaryOp::IgnoreProp,
            TokenKind::Star => UnaryOp::PropAccess,
            TokenKind::KwInt => UnaryOp::AsInt,
            TokenKind::KwReal => UnaryOp::AsReal,
            TokenKind::KwString => UnaryOp::AsString,
            TokenKind::Sharp => UnaryOp::Sharp,
            TokenKind::Dollar => UnaryOp::Dollar,
            TokenKind::Increment => UnaryOp::Increment,
            TokenKind::Decrement => UnaryOp::Decrement,
            _ => return None,
        })
    }

    fn type_unary_op(&self) -> Option<UnaryOp> {
        Some(match self.current().kind {
            TokenKind::KwInt => UnaryOp::AsInt,
            TokenKind::KwReal => UnaryOp::AsReal,
            TokenKind::KwString => UnaryOp::AsString,
            _ => return None,
        })
    }

    fn is_cast_expression(&self) -> bool {
        self.is(&TokenKind::LeftParen)
            && matches!(
                self.nth(1).kind,
                TokenKind::KwInt | TokenKind::KwReal | TokenKind::KwString
            )
            && self.nth(2).kind.same_variant(&TokenKind::RightParen)
    }

    fn consume_comma_separator(&mut self) -> bool {
        self.consume(&TokenKind::Comma).is_some() || self.consume(&TokenKind::FatArrow).is_some()
    }

    fn is_comma_separator(&self) -> bool {
        self.is(&TokenKind::Comma) || self.is(&TokenKind::FatArrow)
    }

    fn expect_statement_semicolon(&mut self) -> Result<Token> {
        if self.mode == ParseMode::Expression && self.is(&TokenKind::Eof) {
            return Ok(Token {
                kind: TokenKind::Semicolon,
                span: Span::empty(self.previous().span.end),
            });
        }
        if let Some(keyword) = reserved_keyword_text(&self.current().kind) {
            return Err(TjsError::parse(
                self.current().span,
                format!("reserved TJS2 keyword {keyword} is unsupported"),
            ));
        }
        self.expect(&TokenKind::Semicolon)
    }

    fn expect_identifier_text(&mut self) -> Result<String> {
        let token = self.advance().clone();
        match token.kind {
            TokenKind::Ident(value) => Ok(value),
            other => Err(TjsError::parse(
                token.span,
                format!("expected identifier, found {other:?}"),
            )),
        }
    }

    fn expect_member_name(&mut self) -> Result<String> {
        let token = self.advance().clone();
        token_text(&token.kind).ok_or_else(|| {
            TjsError::parse(
                token.span,
                format!("expected member name, found {:?}", token.kind),
            )
        })
    }

    fn expect(&mut self, kind: &TokenKind) -> Result<Token> {
        if self.is(kind) {
            Ok(self.advance().clone())
        } else {
            Err(TjsError::parse(
                self.current().span,
                format!("expected {kind:?}, found {:?}", self.current().kind),
            ))
        }
    }

    fn consume(&mut self, kind: &TokenKind) -> Option<Token> {
        if self.is(kind) {
            Some(self.advance().clone())
        } else {
            None
        }
    }

    fn is(&self, kind: &TokenKind) -> bool {
        self.current().kind.same_variant(kind)
    }

    fn current(&self) -> &Token {
        self.tokens
            .get(self.pos)
            .unwrap_or_else(|| self.tokens.last().expect("lexer always emits eof"))
    }

    fn nth(&self, offset: usize) -> &Token {
        self.tokens
            .get(self.pos + offset)
            .unwrap_or_else(|| self.tokens.last().expect("lexer always emits eof"))
    }

    fn previous(&self) -> &Token {
        self.tokens
            .get(self.pos.saturating_sub(1))
            .unwrap_or_else(|| self.tokens.first().expect("lexer always emits eof"))
    }

    fn advance(&mut self) -> &Token {
        let index = self.pos;
        if !self.is(&TokenKind::Eof) {
            self.pos += 1;
        }
        &self.tokens[index]
    }

    fn synchronize_statement(&mut self) {
        while !self.is(&TokenKind::Eof) {
            if self.consume(&TokenKind::Semicolon).is_some() {
                return;
            }
            if matches!(
                self.current().kind,
                TokenKind::RightBrace
                    | TokenKind::KwVar
                    | TokenKind::KwConst
                    | TokenKind::KwFunction
                    | TokenKind::KwClass
                    | TokenKind::KwProperty
                    | TokenKind::KwReturn
                    | TokenKind::KwThrow
                    | TokenKind::KwIf
                    | TokenKind::KwWhile
                    | TokenKind::KwDo
                    | TokenKind::KwFor
                    | TokenKind::KwWith
                    | TokenKind::KwBreak
                    | TokenKind::KwContinue
                    | TokenKind::KwTry
                    | TokenKind::KwSwitch
                    | TokenKind::KwCase
                    | TokenKind::KwDefault
            ) {
                return;
            }
            self.advance();
        }
    }
}

fn token_text(kind: &TokenKind) -> Option<String> {
    Some(
        match kind {
            TokenKind::Ident(value) => return Some(value.clone()),
            TokenKind::KwBreak => "break",
            TokenKind::KwCase => "case",
            TokenKind::KwCatch => "catch",
            TokenKind::KwClass => "class",
            TokenKind::KwConst => "const",
            TokenKind::KwContinue => "continue",
            TokenKind::KwDebugger => "debugger",
            TokenKind::KwDefault => "default",
            TokenKind::KwDelete => "delete",
            TokenKind::KwDo => "do",
            TokenKind::KwElse => "else",
            TokenKind::KwEnum => "enum",
            TokenKind::KwExport => "export",
            TokenKind::KwExtends => "extends",
            TokenKind::KwFalse => "false",
            TokenKind::KwFinally => "finally",
            TokenKind::KwFor => "for",
            TokenKind::KwFunction => "function",
            TokenKind::KwGlobal => "global",
            TokenKind::KwGoto => "goto",
            TokenKind::KwGetter => "getter",
            TokenKind::KwIf => "if",
            TokenKind::KwImport => "import",
            TokenKind::KwIn => "in",
            TokenKind::KwInContextOf => "incontextof",
            TokenKind::KwInfinity => "Infinity",
            TokenKind::KwInstanceOf => "instanceof",
            TokenKind::KwInt => "int",
            TokenKind::KwInvalidate => "invalidate",
            TokenKind::KwIsValid => "isvalid",
            TokenKind::KwNan => "NaN",
            TokenKind::KwNew => "new",
            TokenKind::KwNull => "null",
            TokenKind::KwOctet => "octet",
            TokenKind::KwPrivate => "private",
            TokenKind::KwProperty => "property",
            TokenKind::KwProtected => "protected",
            TokenKind::KwPublic => "public",
            TokenKind::KwReal => "real",
            TokenKind::KwReturn => "return",
            TokenKind::KwSetter => "setter",
            TokenKind::KwStatic => "static",
            TokenKind::KwString => "string",
            TokenKind::KwSuper => "super",
            TokenKind::KwSwitch => "switch",
            TokenKind::KwSynchronized => "synchronized",
            TokenKind::KwThis => "this",
            TokenKind::KwThrow => "throw",
            TokenKind::KwTrue => "true",
            TokenKind::KwTry => "try",
            TokenKind::KwTypeOf => "typeof",
            TokenKind::KwVar => "var",
            TokenKind::KwVoid => "void",
            TokenKind::KwWhile => "while",
            TokenKind::KwWith => "with",
            _ => return None,
        }
        .to_string(),
    )
}

fn reserved_keyword_text(kind: &TokenKind) -> Option<&'static str> {
    Some(match kind {
        TokenKind::KwExport => "export",
        TokenKind::KwImport => "import",
        TokenKind::KwEnum => "enum",
        TokenKind::KwGoto => "goto",
        TokenKind::KwSynchronized => "synchronized",
        TokenKind::KwIn => "in",
        TokenKind::KwFinally => "finally",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_return_arithmetic() {
        let program = parse("return 1 + 2 * 3;").expect("parse");
        assert_eq!(program.statements.len(), 1);
        let StmtKind::Return(Some(expr)) = &program.statements[0].kind else {
            panic!("expected return");
        };
        let ExprKind::Binary {
            op: BinaryOp::Add, ..
        } = expr.kind
        else {
            panic!("expected addition");
        };
    }

    #[test]
    fn parses_major_declaration_shapes() {
        let program = parse(
            r#"
            function answer(a:int, rest*) { return a; }
            class C { function method() { return void; } }
            property value { getter() { return 1; } setter(v) { return; } }
            "#,
        )
        .expect("parse");
        assert!(matches!(
            program.statements[0].kind,
            StmtKind::FunctionDecl(_)
        ));
        assert!(matches!(program.statements[1].kind, StmtKind::ClassDecl(_)));
        assert!(matches!(
            program.statements[2].kind,
            StmtKind::PropertyDecl(_)
        ));
    }

    #[test]
    fn parses_control_flow_and_literals() {
        let program = parse(
            r#"
            var a = ["x",, "z"], d = %["a" => 1, b: 2];
            try { throw "boom"; } catch (e) { if (e) return e; else return void; }
            for (var i = 0; i < 3; i++) { continue; }
            "#,
        )
        .expect("parse");
        assert_eq!(program.statements.len(), 3);
    }

    #[test]
    fn parses_tjs2_frontend_surface() {
        let program = parse(
            r#"
            var r = /a\/b/gi, o = <% 11 22,3 %>, s = "a" "b", c = (const)%["x", (const)[1, void]], i = @"1+2=&1+2;";
            with (global) .member = string (int) "3";
            try throw r; catch(e) e!;
            class C extends A, B { var x = (real)1; }
            var holes = [1,];
            "#,
        )
        .expect("parse");
        assert_eq!(program.statements.len(), 5);

        let StmtKind::Var { declarations, .. } = &program.statements[0].kind else {
            panic!("expected var statement");
        };
        assert!(matches!(
            declarations[0].initializer.as_ref().map(|expr| &expr.kind),
            Some(ExprKind::RegExp { .. })
        ));
        assert!(matches!(
            declarations[1].initializer.as_ref().map(|expr| &expr.kind),
            Some(ExprKind::Octet(bytes)) if bytes == &vec![0x11, 0x22, 0x03]
        ));
        assert!(matches!(
            declarations[2].initializer.as_ref().map(|expr| &expr.kind),
            Some(ExprKind::String(text)) if text == "ab"
        ));
        assert!(matches!(
            declarations[3].initializer.as_ref().map(|expr| &expr.kind),
            Some(ExprKind::ConstDictionary(_))
        ));
        assert!(matches!(
            declarations[4].initializer.as_ref().map(|expr| &expr.kind),
            Some(ExprKind::Binary {
                op: BinaryOp::Add,
                ..
            })
        ));
        assert!(matches!(program.statements[1].kind, StmtKind::With { .. }));
        assert!(matches!(program.statements[2].kind, StmtKind::Try { .. }));
        assert!(matches!(program.statements[3].kind, StmtKind::ClassDecl(_)));
        let StmtKind::Var { declarations, .. } = &program.statements[4].kind else {
            panic!("expected var statement");
        };
        assert!(matches!(
            declarations[0].initializer.as_ref().map(|expr| &expr.kind),
            Some(ExprKind::Array(elements))
                if matches!(elements.as_slice(), [ArrayElement::Value(_), ArrayElement::Hole])
        ));
    }

    #[test]
    fn parses_trailing_commas_as_array_holes() {
        let program = parse("var a = [1,], b = [1,,], c = [,], d = [,,];").expect("parse");
        let StmtKind::Var { declarations, .. } = &program.statements[0].kind else {
            panic!("expected var statement");
        };

        fn array_elements(declaration: &VarDecl) -> &[ArrayElement] {
            match declaration.initializer.as_ref().map(|expr| &expr.kind) {
                Some(ExprKind::Array(elements)) => elements,
                other => panic!("expected array initializer, got {other:?}"),
            }
        }

        let trailing = array_elements(&declarations[0]);
        assert_eq!(trailing.len(), 2);
        assert!(matches!(
            &trailing[0],
            ArrayElement::Value(expr) if matches!(&expr.kind, ExprKind::Integer(1))
        ));
        assert!(matches!(&trailing[1], ArrayElement::Hole));

        let interior_hole_with_trailing_comma = array_elements(&declarations[1]);
        assert_eq!(interior_hole_with_trailing_comma.len(), 3);
        assert!(matches!(
            &interior_hole_with_trailing_comma[0],
            ArrayElement::Value(expr) if matches!(&expr.kind, ExprKind::Integer(1))
        ));
        assert!(matches!(
            &interior_hole_with_trailing_comma[1],
            ArrayElement::Hole
        ));
        assert!(matches!(
            &interior_hole_with_trailing_comma[2],
            ArrayElement::Hole
        ));

        let single_hole = array_elements(&declarations[2]);
        assert_eq!(single_hole.len(), 2);
        assert!(matches!(&single_hole[0], ArrayElement::Hole));
        assert!(matches!(&single_hole[1], ArrayElement::Hole));

        let two_holes = array_elements(&declarations[3]);
        assert_eq!(two_holes.len(), 3);
        assert!(matches!(&two_holes[0], ArrayElement::Hole));
        assert!(matches!(&two_holes[1], ArrayElement::Hole));
        assert!(matches!(&two_holes[2], ArrayElement::Hole));
    }

    #[test]
    fn rejects_missing_statement_semicolons() {
        assert!(parse("var x = 1").is_err());
        assert!(parse("return 1").is_err());
        assert!(parse("break").is_err());
        assert!(parse("do ; while (ok)").is_err());
    }

    #[test]
    fn parses_tjs_unary_layer_precedence() {
        let expr = parse_expression(
            "inline.tjs",
            "a incontextof b incontextof c",
            FrontendOptions::default(),
        )
        .value
        .expect("expr");
        let ExprKind::Binary {
            op: BinaryOp::InContextOf,
            rhs,
            ..
        } = expr.kind
        else {
            panic!("expected incontextof");
        };
        assert!(matches!(
            rhs.kind,
            ExprKind::Binary {
                op: BinaryOp::InContextOf,
                ..
            }
        ));

        let expr = parse_expression(
            "inline.tjs",
            "a incontextof b isvalid",
            FrontendOptions::default(),
        )
        .value
        .expect("expr");
        assert!(matches!(
            expr.kind,
            ExprKind::Unary {
                op: UnaryOp::IsValid,
                ..
            }
        ));
    }

    #[test]
    fn expression_mode_accepts_semicolon_and_dicfunc_literals() {
        let expr = parse_expression("inline.tjs", "1 + 2;", FrontendOptions::default())
            .value
            .expect("expr");
        assert!(matches!(
            expr.kind,
            ExprKind::Binary {
                op: BinaryOp::Add,
                ..
            }
        ));

        let expr = parse_expression("inline.tjs", "[1,]", FrontendOptions::default())
            .value
            .expect("expr");
        assert!(matches!(
            expr.kind,
            ExprKind::Call {
                callee,
                args,
            } if args.is_empty() && matches!(&callee.kind, ExprKind::Function(_))
        ));

        let expr = parse_expression("inline.tjs", "%[\"a\", 1]", FrontendOptions::default())
            .value
            .expect("expr");
        assert!(matches!(
            expr.kind,
            ExprKind::Call {
                callee,
                args,
            } if args.is_empty() && matches!(&callee.kind, ExprKind::Function(_))
        ));

        let program = parse("var a = [1,];").expect("script parse");
        let StmtKind::Var { declarations, .. } = &program.statements[0].kind else {
            panic!("expected var statement");
        };
        assert!(matches!(
            declarations[0].initializer.as_ref().map(|expr| &expr.kind),
            Some(ExprKind::Array(_))
        ));
    }

    #[test]
    fn rejects_reserved_syntax_but_allows_reserved_member_names() {
        for source in [
            "import x;",
            "export x;",
            "enum E {};",
            "goto label;",
            "synchronized {};",
            "a in b;",
            "try {} finally {};",
            "public function f() {}",
            "static var x;",
        ] {
            let err = parse(source).expect_err(source);
            assert!(
                err.message.contains("unsupported")
                    || err.message.contains("finally")
                    || err.message.contains("expected expression")
            );
        }

        parse("obj.import; .enum; obj.synchronized;").expect("reserved member names");
    }

    #[test]
    fn parse_diagnostics_include_source_locations() {
        let output = parse_script(
            "foo.tjs",
            "var x = 1\nreturn;",
            FrontendOptions { recover: false },
        );
        let diagnostic = output.diagnostics.first().expect("diagnostic");
        assert_eq!(diagnostic.source_name.as_deref(), Some("foo.tjs"));
        assert_eq!(diagnostic.start.map(|location| location.line), Some(2));
        assert_eq!(diagnostic.start.map(|location| location.column), Some(1));
    }

    #[test]
    fn rejects_non_final_collapse_parameters_and_invalid_const_dictionary_keys() {
        assert!(parse("function f(rest*, tail) {}").is_err());
        assert!(parse("function f(rest*,) {}").is_err());
        assert!(parse("var d = (const)%[\"x\", 1,];").is_ok());
        assert!(parse("var d = (const)%[void, 1];").is_err());
        assert!(parse("var d = (const)%[-1, 1];").is_err());
        assert!(parse("var d = (const)%[(const)[], 1];").is_err());
    }

    #[test]
    fn treats_fat_arrow_as_comma_separator() {
        parse("foo(a => b);").expect("fat arrow separates arguments");
        parse("var a => b;").expect("fat arrow separates variables");
        parse("function f(a => b) {}").expect("fat arrow separates parameters");
        parse("var xs = [a => b];").expect("fat arrow separates array elements");
    }

    #[test]
    fn rejects_named_function_expressions_and_new_without_call() {
        parse("var f = function named() {};").expect_err("named function expression");
        parse("var value = new Type;").expect_err("new requires a call expression");
        parse("var value = new Type().member;").expect_err("new result cannot be postfixed");
        parse("var value = new factory()();").expect("new accepts an outer call expression");
        parse("var value = new Type().member();").expect("new accepts called member expression");
    }

    #[test]
    fn rejects_krkrz_incompatible_conditional_arms_and_mixed_string_concat() {
        parse("var x = a ? b, c : d;").expect_err("comma is not a conditional arm");
        parse("var x = a ? b = 1 : c = 2;").expect_err("assignment is not a conditional arm");
        parse(r#"var s = "a" 'b';"#).expect_err("mixed quote strings are not concatenated");
        parse(r#"var s = "a" "b";"#).expect("same quote strings are lexer-concatenated");
    }

    #[test]
    fn rejects_finally_and_duplicate_property_handlers() {
        parse("try { return 1; } finally { return 2; }").expect_err("finally is unsupported");
        parse("property value { getter { return 1; } getter { return 2; } }")
            .expect_err("duplicate getter");
    }

    #[test]
    fn parses_prop_access_and_trailing_expand_call_args() {
        let program = parse("foo(*bar, bar*, *, a * b);").expect("parse");
        let StmtKind::Expr(expr) = &program.statements[0].kind else {
            panic!("expected expression statement");
        };
        let ExprKind::Call { args, .. } = &expr.kind else {
            panic!("expected call expression");
        };

        assert!(matches!(
            &args[0],
            CallArg::Value(Expr {
                kind: ExprKind::Unary {
                    op: UnaryOp::PropAccess,
                    expr,
                },
                ..
            }) if matches!(&expr.kind, ExprKind::Identifier(name) if name.name == "bar")
        ));
        assert!(matches!(
            &args[1],
            CallArg::Expand(Some(Expr {
                kind: ExprKind::Identifier(name),
                ..
            })) if name.name == "bar"
        ));
        assert!(matches!(&args[2], CallArg::Expand(None)));
        assert!(matches!(
            &args[3],
            CallArg::Value(Expr {
                kind: ExprKind::Binary {
                    op: BinaryOp::Mul,
                    ..
                },
                ..
            })
        ));
    }

    #[test]
    fn parses_regexp_statement_after_control_header() {
        let program = parse("if (ok) /x/.test(s);").expect("parse");
        let StmtKind::If { then_branch, .. } = &program.statements[0].kind else {
            panic!("expected if statement");
        };
        let StmtKind::Expr(expr) = &then_branch.kind else {
            panic!("expected regexp expression statement");
        };
        assert!(matches!(expr.kind, ExprKind::Call { .. }));
    }

    #[test]
    fn parses_regexp_statement_after_block_end() {
        let program = parse("if (ok) {} /x/.test(s); function f(){} /y/.test(s);").expect("parse");
        assert_eq!(program.statements.len(), 4);
        assert!(matches!(program.statements[0].kind, StmtKind::If { .. }));
        assert!(matches!(program.statements[1].kind, StmtKind::Expr(_)));
        assert!(matches!(
            program.statements[2].kind,
            StmtKind::FunctionDecl(_)
        ));
        assert!(matches!(program.statements[3].kind, StmtKind::Expr(_)));
    }

    #[test]
    fn parses_regexp_after_parenthesized_cast() {
        let program = parse("var s = (string) /a/;").expect("parse");
        let StmtKind::Var { declarations, .. } = &program.statements[0].kind else {
            panic!("expected var statement");
        };
        assert!(matches!(
            declarations[0].initializer.as_ref().map(|expr| &expr.kind),
            Some(ExprKind::Unary {
                op: UnaryOp::AsString,
                expr,
            }) if matches!(
                &expr.kind,
                ExprKind::RegExp { pattern, flags } if pattern == "a" && flags.is_empty()
            )
        ));
    }

    #[test]
    fn parses_interpolated_regexp_with_semicolon_pattern() {
        let program = parse(r#"var s = @"&/;/.test(x);";"#).expect("parse");
        let StmtKind::Var { declarations, .. } = &program.statements[0].kind else {
            panic!("expected var statement");
        };
        assert!(matches!(
            declarations[0].initializer.as_ref().map(|expr| &expr.kind),
            Some(ExprKind::Unary {
                op: UnaryOp::AsString,
                expr,
            }) if matches!(&expr.kind, ExprKind::Call { .. })
        ));
    }

    #[test]
    fn parses_division_after_postfix_eval_and_function_expression() {
        let program = parse(
            "var y = source! / 2; var x = function(){} / 2; var z = ok ? 1 : function(){} / 2;",
        )
        .expect("parse");
        assert_eq!(program.statements.len(), 3);

        let StmtKind::Var {
            declarations: y_declarations,
            ..
        } = &program.statements[0].kind
        else {
            panic!("expected var statement");
        };
        assert!(matches!(
            y_declarations[0].initializer.as_ref().map(|expr| &expr.kind),
            Some(ExprKind::Binary {
                op: BinaryOp::Div,
                lhs,
                rhs,
            }) if matches!(&lhs.kind, ExprKind::Postfix { op: UnaryOp::Eval, .. })
                && matches!(&rhs.kind, ExprKind::Integer(2))
        ));

        let StmtKind::Var {
            declarations: x_declarations,
            ..
        } = &program.statements[1].kind
        else {
            panic!("expected var statement");
        };
        assert!(matches!(
            x_declarations[0].initializer.as_ref().map(|expr| &expr.kind),
            Some(ExprKind::Binary {
                op: BinaryOp::Div,
                lhs,
                rhs,
            }) if matches!(&lhs.kind, ExprKind::Function(_))
                && matches!(&rhs.kind, ExprKind::Integer(2))
        ));

        let StmtKind::Var {
            declarations: z_declarations,
            ..
        } = &program.statements[2].kind
        else {
            panic!("expected var statement");
        };
        assert!(matches!(
            z_declarations[0].initializer.as_ref().map(|expr| &expr.kind),
            Some(ExprKind::Conditional { else_expr, .. })
                if matches!(&else_expr.kind, ExprKind::Binary { op: BinaryOp::Div, .. })
        ));
    }

    #[test]
    fn parses_division_after_expression_if_grouping() {
        let program = parse("return a if (b) / c;").expect("parse");
        let StmtKind::Return(Some(expr)) = &program.statements[0].kind else {
            panic!("expected return");
        };
        assert!(matches!(
            &expr.kind,
            ExprKind::Binary {
                op: BinaryOp::If,
                rhs,
                ..
            } if matches!(&rhs.kind, ExprKind::Binary { op: BinaryOp::Div, .. })
        ));
    }

    #[test]
    fn concatenates_string_literals_after_interpolated_string() {
        let program = parse(r#"var s = @"a" "b";"#).expect("parse");
        assert_eq!(program.statements.len(), 1);
        let StmtKind::Var { declarations, .. } = &program.statements[0].kind else {
            panic!("expected var statement");
        };
        assert!(matches!(
            declarations[0].initializer.as_ref().map(|expr| &expr.kind),
            Some(ExprKind::String(text)) if text == "ab"
        ));
    }

    #[test]
    fn empty_interpolated_string_expression_is_empty_text() {
        let program = parse(r#"var s = @"a${}b";"#).expect("parse");
        let StmtKind::Var { declarations, .. } = &program.statements[0].kind else {
            panic!("expected var statement");
        };
        assert!(matches!(
            declarations[0].initializer.as_ref().map(|expr| &expr.kind),
            Some(ExprKind::String(text)) if text == "ab"
        ));
    }
}
