use std::collections::BTreeMap;

use crate::error::{Result, Span, TjsError};

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    Eof,
    Ident(String),
    Integer(i64),
    Real(f64),
    String(String),
    InterpolatedString(Vec<InterpolatedPart>),
    Octet(Vec<u8>),
    RegExp { pattern: String, flags: String },

    KwBreak,
    KwCase,
    KwCatch,
    KwClass,
    KwConst,
    KwContinue,
    KwDebugger,
    KwDefault,
    KwDelete,
    KwDo,
    KwElse,
    KwEnum,
    KwExport,
    KwExtends,
    KwFalse,
    KwFinally,
    KwFor,
    KwFunction,
    KwGlobal,
    KwGoto,
    KwGetter,
    KwIf,
    KwImport,
    KwIn,
    KwInContextOf,
    KwInfinity,
    KwInstanceOf,
    KwInt,
    KwInvalidate,
    KwIsValid,
    KwNan,
    KwNew,
    KwNull,
    KwOctet,
    KwPrivate,
    KwProperty,
    KwProtected,
    KwPublic,
    KwReal,
    KwReturn,
    KwSetter,
    KwStatic,
    KwString,
    KwSuper,
    KwSwitch,
    KwSynchronized,
    KwThis,
    KwThrow,
    KwTrue,
    KwTry,
    KwTypeOf,
    KwVar,
    KwVoid,
    KwWhile,
    KwWith,

    Comma,
    Assign,
    AmpAssign,
    PipeAssign,
    CaretAssign,
    MinusAssign,
    PlusAssign,
    PercentAssign,
    SlashAssign,
    BackslashAssign,
    StarAssign,
    LogicalOrAssign,
    LogicalAndAssign,
    UnsignedShiftRightAssign,
    ShiftLeftAssign,
    ShiftRightAssign,
    Question,
    LogicalOr,
    LogicalAnd,
    Pipe,
    Caret,
    Amp,
    NotEqual,
    EqualEqual,
    DiscernNotEqual,
    DiscernEqual,
    Swap,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    ShiftRight,
    ShiftLeft,
    UnsignedShiftRight,
    Percent,
    Slash,
    Backslash,
    Star,
    Bang,
    Tilde,
    Decrement,
    Increment,
    Plus,
    Minus,
    Sharp,
    Dollar,
    LeftParen,
    Dot,
    LeftBracket,
    RightBracket,
    RightParen,
    Colon,
    Semicolon,
    LeftBrace,
    RightBrace,
    Ellipsis,
    FatArrow,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InterpolatedPart {
    Text(String),
    Expr(String),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LexMode {
    #[default]
    Normal,
    RegExp,
    BareWord,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InterpolationEnd {
    Semicolon,
    Brace,
}

#[derive(Debug)]
struct InterpolationScanState {
    paren_stack: Vec<ParenContext>,
    bracket_depth: u32,
    brace_depth: u32,
    expect_operand: bool,
    expect_member_name: bool,
    brace_stack: Vec<BraceContext>,
    pending_function_bodies: Vec<FunctionContext>,
}

impl InterpolationScanState {
    fn new(end: InterpolationEnd) -> Self {
        Self {
            paren_stack: Vec::new(),
            bracket_depth: 0,
            brace_depth: u32::from(matches!(end, InterpolationEnd::Brace)),
            expect_operand: true,
            expect_member_name: false,
            brace_stack: Vec::new(),
            pending_function_bodies: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ParenContext {
    Grouping(GroupingContext),
    ControlHeader,
    ForHeader { brace_depth: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GroupingContext {
    NotCast,
    CastCandidate,
    CastType,
}

impl GroupingContext {
    fn new(was_expecting_operand: bool) -> Self {
        if was_expecting_operand {
            Self::CastCandidate
        } else {
            Self::NotCast
        }
    }

    fn observe(&mut self, kind: &TokenKind) {
        *self = match (*self, kind) {
            (Self::CastCandidate, TokenKind::KwInt | TokenKind::KwReal | TokenKind::KwString) => {
                Self::CastType
            }
            (Self::CastCandidate | Self::CastType, _) => Self::NotCast,
            (Self::NotCast, _) => Self::NotCast,
        };
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BraceContext {
    Block,
    FunctionDeclarationBody,
    FunctionExpressionBody,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FunctionContext {
    Declaration,
    Expression,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CaseHeaderContext {
    paren_depth: u32,
    bracket_depth: u32,
    question_depth: u32,
}

impl CaseHeaderContext {
    fn observe(&mut self, kind: &TokenKind) -> bool {
        match kind {
            TokenKind::Colon if self.paren_depth == 0 && self.bracket_depth == 0 => {
                if self.question_depth == 0 {
                    return true;
                }
                self.question_depth -= 1;
            }
            TokenKind::Question if self.paren_depth == 0 && self.bracket_depth == 0 => {
                self.question_depth += 1;
            }
            TokenKind::LeftParen => {
                self.paren_depth += 1;
            }
            TokenKind::RightParen => {
                self.paren_depth = self.paren_depth.saturating_sub(1);
            }
            TokenKind::LeftBracket => {
                self.bracket_depth += 1;
            }
            TokenKind::RightBracket => {
                self.bracket_depth = self.bracket_depth.saturating_sub(1);
            }
            _ => {}
        }
        false
    }
}

impl TokenKind {
    pub fn same_variant(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

pub fn lex(source: &str) -> Result<Vec<Token>> {
    Lexer::new(source).lex_all()
}

pub struct Lexer<'a> {
    source: &'a str,
    chars: std::str::CharIndices<'a>,
    lookahead: Option<(usize, char)>,
    pp_values: BTreeMap<String, i32>,
    pp_if_stack: Vec<usize>,
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a str) -> Self {
        let mut chars = source.char_indices();
        let lookahead = chars.next();
        let mut pp_values = BTreeMap::new();
        pp_values.insert("version".to_string(), 0x02040009);
        Self {
            source,
            chars,
            lookahead,
            pp_values,
            pp_if_stack: Vec::new(),
        }
    }

    pub fn next_token(&mut self, mode: LexMode) -> Result<Token> {
        self.skip_shebang();
        self.skip_trivia()?;
        let start = self.offset();
        let Some((_, ch)) = self.bump() else {
            if let Some(start) = self.pp_if_stack.last().copied() {
                return Err(TjsError::lex(
                    Span::new(start, self.offset()),
                    "unterminated preprocessor block",
                ));
            }
            return Ok(Token {
                kind: TokenKind::Eof,
                span: Span::empty(start),
            });
        };

        if mode == LexMode::RegExp && ch == '/' {
            return self.lex_regexp(start);
        }

        let token = match ch {
            '0'..='9' => self.lex_number(start, ch)?,
            '.' if self.peek().is_some_and(|ch| ch.is_ascii_digit()) => {
                self.lex_number(start, ch)?
            }
            '"' | '\'' => self.lex_string(start, ch)?,
            '@' => self.lex_at_prefixed_string(start)?,
            '<' if self.peek() == Some('%') => {
                self.bump();
                self.lex_octet(start)?
            }
            ch if is_ident_start(ch) && mode == LexMode::BareWord => {
                self.lex_bare_identifier(start, ch)
            }
            ch if is_ident_start(ch) => self.lex_identifier(start, ch),
            _ => self.lex_punctuation(start, ch)?,
        };
        Ok(token)
    }

    fn lex_all(mut self) -> Result<Vec<Token>> {
        let mut tokens = Vec::new();
        let mut expect_operand = true;
        let mut statement_start = true;
        let mut paren_stack = Vec::new();
        let mut brace_stack = Vec::new();
        let mut pending_function_bodies = Vec::new();
        let mut case_header = None::<CaseHeaderContext>;
        let mut previous_starts_control_header = false;
        let mut previous_starts_for_header = false;
        let mut expect_member_name = false;
        self.skip_shebang();
        loop {
            self.skip_trivia()?;
            let start = self.offset();
            let Some((_, ch)) = self.bump() else {
                if let Some(start) = self.pp_if_stack.last().copied() {
                    return Err(TjsError::lex(
                        Span::new(start, self.offset()),
                        "unterminated preprocessor block",
                    ));
                }
                tokens.push(Token {
                    kind: TokenKind::Eof,
                    span: Span::empty(start),
                });
                break;
            };

            let was_expect_operand = expect_operand;
            let was_statement_start = statement_start;
            let was_expect_member_name = expect_member_name;
            let token = match ch {
                '0'..='9' => self.lex_number(start, ch)?,
                '.' if self.peek().is_some_and(|ch| ch.is_ascii_digit()) => {
                    self.lex_number(start, ch)?
                }
                '"' | '\'' => self.lex_string(start, ch)?,
                '@' => self.lex_at_prefixed_string(start)?,
                '/' if expect_operand => self.lex_regexp(start)?,
                '<' if self.peek() == Some('%') => {
                    self.bump();
                    self.lex_octet(start)?
                }
                ch if is_ident_start(ch) && expect_member_name => {
                    self.lex_bare_identifier(start, ch)
                }
                ch if is_ident_start(ch) => self.lex_identifier(start, ch),
                _ => self.lex_punctuation(start, ch)?,
            };
            let is_member_name = was_expect_member_name && token_can_be_member_name(&token.kind);
            let closed_paren = if token.kind == TokenKind::RightParen {
                paren_stack.pop()
            } else {
                None
            };
            let closes_control_header = matches!(
                closed_paren,
                Some(ParenContext::ControlHeader | ParenContext::ForHeader { .. })
            );
            let closes_cast_head = matches!(
                closed_paren,
                Some(ParenContext::Grouping(GroupingContext::CastType))
            );
            let closes_function_expression_body = token.kind == TokenKind::RightBrace
                && brace_stack.pop() == Some(BraceContext::FunctionExpressionBody);
            let is_for_header_separator = token.kind == TokenKind::Semicolon
                && matches!(
                    paren_stack.last(),
                    Some(ParenContext::ForHeader { brace_depth })
                        if brace_stack.len() == *brace_depth
                );
            let closes_case_header = case_header
                .as_mut()
                .is_some_and(|header| header.observe(&token.kind));
            if closes_case_header {
                case_header = None;
            }
            observe_grouping_context(&mut paren_stack, &token.kind);
            if token.kind == TokenKind::LeftParen {
                let context = if previous_starts_for_header {
                    ParenContext::ForHeader {
                        brace_depth: brace_stack.len(),
                    }
                } else if previous_starts_control_header {
                    ParenContext::ControlHeader
                } else {
                    ParenContext::Grouping(GroupingContext::new(was_expect_operand))
                };
                paren_stack.push(context);
            }
            if token.kind == TokenKind::LeftBrace {
                let context = match pending_function_bodies.pop() {
                    Some(FunctionContext::Declaration) => BraceContext::FunctionDeclarationBody,
                    Some(FunctionContext::Expression) => BraceContext::FunctionExpressionBody,
                    None => BraceContext::Block,
                };
                brace_stack.push(context);
            }
            if token.kind == TokenKind::KwFunction && !is_member_name {
                let context = if was_statement_start {
                    FunctionContext::Declaration
                } else {
                    FunctionContext::Expression
                };
                pending_function_bodies.push(context);
            }
            if !is_member_name && token_starts_case_header(&token.kind, was_statement_start) {
                case_header = Some(CaseHeaderContext::default());
            }
            expect_operand = if is_member_name {
                false
            } else if closes_control_header || closes_cast_head {
                true
            } else if closes_function_expression_body {
                false
            } else {
                token_expects_operand_after(&token.kind, was_expect_operand)
            };
            statement_start = if is_member_name || is_for_header_separator {
                false
            } else {
                token_starts_statement_after(
                    &token.kind,
                    closes_control_header,
                    closes_function_expression_body,
                    closes_case_header,
                )
            };
            previous_starts_control_header =
                !is_member_name && token_starts_control_header(&token.kind, was_statement_start);
            previous_starts_for_header =
                !is_member_name && token_starts_for_header(&token.kind, was_statement_start);
            expect_member_name = token.kind == TokenKind::Dot;
            tokens.push(token);
        }
        Ok(tokens)
    }

    fn offset(&self) -> usize {
        self.lookahead
            .map(|(offset, _)| offset)
            .unwrap_or(self.source.len())
    }

    fn bump(&mut self) -> Option<(usize, char)> {
        let current = self.lookahead;
        self.lookahead = self.chars.next();
        current
    }

    fn peek(&self) -> Option<char> {
        self.lookahead.map(|(_, ch)| ch)
    }

    fn starts_with(&self, text: &str) -> bool {
        self.source[self.offset()..].starts_with(text)
    }

    fn consume_if(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn consume_str(&mut self, expected: &str) -> bool {
        if !self.starts_with(expected) {
            return false;
        }
        for _ in expected.chars() {
            self.bump();
        }
        true
    }

    fn skip_trivia(&mut self) -> Result<()> {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.bump();
            }

            if self.starts_with("//") {
                while let Some(ch) = self.peek() {
                    self.bump();
                    if ch == '\n' {
                        break;
                    }
                }
                continue;
            }

            if self.starts_with("/*") {
                let start = self.offset();
                self.skip_nested_block_comment(start, "unterminated block comment")?;
                continue;
            }

            if self.process_pp_directive()? {
                continue;
            }

            break;
        }
        Ok(())
    }

    fn process_pp_directive(&mut self) -> Result<bool> {
        if self.peek() != Some('@') {
            return Ok(false);
        }

        let directive_start = self.offset();
        if self.consume_pp_keyword("@set") {
            let expr = self.read_pp_parenthesized_expr()?;
            let _ = eval_pp_expr(&expr, &mut self.pp_values)?;
            return Ok(true);
        }

        if self.consume_pp_keyword("@if") {
            let expr = self.read_pp_parenthesized_expr()?;
            if eval_pp_expr(&expr, &mut self.pp_values)? == 0 {
                self.skip_disabled_pp_block(directive_start)?;
            } else {
                self.pp_if_stack.push(directive_start);
            }
            return Ok(true);
        }

        if self.consume_pp_keyword("@endif") {
            if self.pp_if_stack.pop().is_none() {
                return Err(TjsError::lex(
                    Span::new(directive_start, self.offset()),
                    "unexpected preprocessor endif",
                ));
            }
            return Ok(true);
        }

        Ok(false)
    }

    fn consume_pp_keyword(&mut self, keyword: &str) -> bool {
        if !self.starts_with(keyword) {
            return false;
        }
        let after = self.offset() + keyword.len();
        if self.source[after..]
            .chars()
            .next()
            .is_some_and(is_ident_continue)
        {
            return false;
        }
        self.consume_str(keyword)
    }

    fn skip_shebang(&mut self) {
        if self.offset() != 0 || !self.starts_with("#!") {
            return;
        }
        while let Some(ch) = self.peek() {
            self.bump();
            if ch == '\n' {
                break;
            }
        }
    }

    fn skip_nested_block_comment(&mut self, start: usize, message: &str) -> Result<()> {
        if self.starts_with("/*") {
            self.consume_str("/*");
        } else if self.peek() == Some('*') {
            self.bump();
        } else {
            return Ok(());
        }

        let mut depth = 1_u32;
        while self.peek().is_some() {
            if self.starts_with("/*") {
                self.consume_str("/*");
                depth += 1;
                continue;
            }
            if self.starts_with("*/") {
                self.consume_str("*/");
                depth -= 1;
                if depth == 0 {
                    return Ok(());
                }
                continue;
            }
            self.bump();
        }

        Err(TjsError::lex(Span::new(start, self.offset()), message))
    }

    fn read_pp_parenthesized_expr(&mut self) -> Result<String> {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
        let start = self.offset();
        if !self.consume_if('(') {
            return Err(TjsError::lex(
                Span::new(start, self.offset()),
                "expected parenthesized preprocessor expression",
            ));
        }

        let mut expr = String::new();
        let mut depth = 1_u32;
        while let Some((_, ch)) = self.bump() {
            match ch {
                '(' => {
                    depth += 1;
                    expr.push(ch);
                }
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(expr);
                    }
                    expr.push(ch);
                }
                _ => expr.push(ch),
            }
        }

        Err(TjsError::lex(
            Span::new(start, self.offset()),
            "unterminated preprocessor expression",
        ))
    }

    fn skip_disabled_pp_block(&mut self, start: usize) -> Result<()> {
        let mut depth = 1_u32;
        while self.peek().is_some() {
            if self.starts_with("//") {
                while let Some(ch) = self.peek() {
                    self.bump();
                    if ch == '\n' {
                        break;
                    }
                }
                continue;
            }

            if self.starts_with("/*") {
                self.skip_nested_block_comment(
                    start,
                    "unterminated block comment in skipped preprocessor block",
                )?;
                continue;
            }

            if matches!(self.peek(), Some('"') | Some('\'')) {
                let quote = self.bump().expect("peeked").1;
                self.skip_raw_string(start, quote)?;
                continue;
            }

            if self.try_skip_raw_regexp() {
                continue;
            }

            if self.consume_pp_keyword("@if") {
                depth += 1;
                if self.peek().is_some_and(char::is_whitespace) || self.peek() == Some('(') {
                    let _ = self.read_pp_parenthesized_expr();
                }
                continue;
            }

            if self.consume_pp_keyword("@endif") {
                depth -= 1;
                if depth == 0 {
                    return Ok(());
                }
                continue;
            }

            self.bump();
        }

        Err(TjsError::lex(
            Span::new(start, self.offset()),
            "unterminated preprocessor block",
        ))
    }

    fn try_skip_raw_regexp(&mut self) -> bool {
        if self.peek() != Some('/')
            || self.starts_with("//")
            || self.starts_with("/*")
            || self.starts_with("/=")
        {
            return false;
        }

        let saved_chars = self.chars.clone();
        let saved_lookahead = self.lookahead;
        self.bump();

        let mut escaped = false;
        let mut closed = false;
        while let Some((_, ch)) = self.bump() {
            if escaped {
                escaped = false;
                continue;
            }

            match ch {
                '\\' => escaped = true,
                '/' => {
                    closed = true;
                    break;
                }
                _ => {}
            }
        }

        if !closed {
            self.chars = saved_chars;
            self.lookahead = saved_lookahead;
            return false;
        }

        while self.peek().is_some_and(|ch| ch.is_ascii_lowercase()) {
            self.bump();
        }
        true
    }

    fn skip_raw_string(&mut self, start: usize, quote: char) -> Result<()> {
        let mut escaped = false;
        while let Some((_, ch)) = self.bump() {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
            } else if ch == quote {
                return Ok(());
            }
        }
        Err(TjsError::lex(
            Span::new(start, self.offset()),
            "unterminated string in skipped preprocessor block",
        ))
    }

    fn lex_at_prefixed_string(&mut self, start: usize) -> Result<Token> {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
        if matches!(self.peek(), Some('"') | Some('\'')) {
            let quote = self.bump().expect("peeked").1;
            return self.lex_interpolated_string(start, quote);
        }
        self.lex_punctuation(start, '@')
    }

    fn lex_number(&mut self, start: usize, first: char) -> Result<Token> {
        if first == '.' {
            return self.lex_decimal_number(start, first);
        }

        if first == '0' {
            return match self.peek() {
                Some('x' | 'X') => {
                    self.bump();
                    self.lex_non_decimal_number(start, 16, None)
                }
                Some('b' | 'B') => {
                    self.bump();
                    self.lex_non_decimal_number(start, 2, None)
                }
                Some('p' | 'P') => Err(TjsError::lex(
                    Span::new(start, self.offset()),
                    "invalid binary exponent on decimal zero literal",
                )),
                Some('.' | 'e' | 'E') => self.lex_decimal_number(start, first),
                _ => self.lex_non_decimal_number(start, 8, Some(0)),
            };
        }

        self.lex_decimal_number(start, first)
    }

    fn lex_decimal_number(&mut self, start: usize, first: char) -> Result<Token> {
        let mut text = String::new();
        let mut is_real = first == '.';
        if first == '.' {
            text.push('0');
        }
        text.push(first);

        while self.peek().is_some_and(|ch| ch.is_ascii_digit()) {
            text.push(self.bump().expect("peeked").1);
        }

        if first != '.' && self.peek() == Some('.') {
            is_real = true;
            text.push(self.bump().expect("peeked").1);
            while self.peek().is_some_and(|ch| ch.is_ascii_digit()) {
                text.push(self.bump().expect("peeked").1);
            }
        }

        if matches!(self.peek(), Some('e' | 'E')) {
            is_real = true;
            let exponent_start = text.len();
            text.push(self.bump().expect("peeked").1);
            self.skip_number_ws();
            if matches!(self.peek(), Some('+' | '-')) {
                text.push(self.bump().expect("peeked").1);
                self.skip_number_ws();
            }
            let digit_start = text.len();
            while self.peek().is_some_and(|ch| ch.is_ascii_digit()) {
                text.push(self.bump().expect("peeked").1);
            }
            if digit_start == text.len() {
                text.truncate(exponent_start);
            }
        }

        let span = Span::new(start, self.offset());
        if is_real {
            let value = text
                .parse::<f64>()
                .map_err(|_| TjsError::lex(span, "invalid real literal"))?;
            return Ok(Token {
                kind: TokenKind::Real(value),
                span,
            });
        }

        let value = if text.len() > 1 && text.starts_with('0') {
            i64::from_str_radix(&text[1..], 8)
                .map_err(|_| TjsError::lex(span, "invalid octal literal"))?
        } else {
            text.parse::<i64>()
                .map_err(|_| TjsError::lex(span, "invalid integer literal"))?
        };
        Ok(Token {
            kind: TokenKind::Integer(value),
            span,
        })
    }

    fn lex_non_decimal_number(
        &mut self,
        start: usize,
        radix: u32,
        initial_digit: Option<u32>,
    ) -> Result<Token> {
        let mut integer = 0_i64;
        let mut digits = Vec::new();
        let mut point_index = 0_usize;
        let mut saw_digit = false;
        let mut saw_point = false;
        let mut is_real = false;

        if let Some(digit) = initial_digit {
            integer = digit as i64;
            digits.push(digit);
            point_index = 1;
            saw_digit = true;
        }

        loop {
            if let Some(digit) = self.peek().and_then(|ch| ch.to_digit(radix)) {
                self.bump();
                saw_digit = true;
                digits.push(digit);
                if !saw_point {
                    point_index += 1;
                    integer = integer
                        .wrapping_mul(radix as i64)
                        .wrapping_add(digit as i64);
                }
                continue;
            }

            if self.peek() == Some('.') && !saw_point {
                self.bump();
                saw_point = true;
                is_real = true;
                continue;
            }

            break;
        }

        if !saw_digit && !saw_point {
            return Err(TjsError::lex(
                Span::new(start, self.offset()),
                "non-decimal literal has no digits",
            ));
        }

        let mut exponent = 0_i32;
        if matches!(self.peek(), Some('p' | 'P')) {
            is_real = true;
            self.bump();
            self.skip_number_ws();
            let negative = if self.consume_if('+') {
                self.skip_number_ws();
                false
            } else if self.consume_if('-') {
                self.skip_number_ws();
                true
            } else {
                false
            };

            let mut saw_exponent_digit = false;
            while let Some(digit) = self.peek().and_then(|ch| ch.to_digit(10)) {
                self.bump();
                saw_exponent_digit = true;
                exponent = exponent.saturating_mul(10).saturating_add(digit as i32);
            }
            if negative {
                exponent = -exponent;
            }
            if !saw_exponent_digit {
                exponent = 0;
            }
        }

        let span = Span::new(start, self.offset());
        if is_real {
            let value = non_decimal_real_value(&digits, point_index, radix, exponent);
            return Ok(Token {
                kind: TokenKind::Real(value),
                span,
            });
        }

        Ok(Token {
            kind: TokenKind::Integer(integer),
            span,
        })
    }

    fn skip_number_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }

    fn lex_string(&mut self, start: usize, quote: char) -> Result<Token> {
        let mut value = String::new();
        loop {
            let Some((_, ch)) = self.bump() else {
                return Err(TjsError::lex(
                    Span::new(start, self.offset()),
                    "unterminated string literal",
                ));
            };

            if ch == quote {
                if self.consume_same_string_delimiter(quote) {
                    continue;
                }
                break;
            }

            if ch != '\\' {
                value.push(ch);
                continue;
            }

            let Some((_, escaped)) = self.bump() else {
                return Err(TjsError::lex(
                    Span::new(start, self.offset()),
                    "unterminated string escape",
                ));
            };

            match escaped {
                'a' => value.push('\u{0007}'),
                'n' => value.push('\n'),
                'r' => value.push('\r'),
                't' => value.push('\t'),
                'b' => value.push('\u{0008}'),
                'f' => value.push('\u{000c}'),
                'v' => value.push('\u{000b}'),
                '0' => value.push(self.read_octal_escape_after_zero()),
                '\\' => value.push('\\'),
                '\'' => value.push('\''),
                '"' => value.push('"'),
                'x' | 'X' => value.push(self.read_variable_hex_escape()),
                'u' => value.push('u'),
                other => value.push(other),
            }
        }

        Ok(Token {
            kind: TokenKind::String(value),
            span: Span::new(start, self.offset()),
        })
    }

    fn lex_interpolated_string(&mut self, start: usize, quote: char) -> Result<Token> {
        let mut parts = Vec::new();
        let mut text = String::new();

        loop {
            let Some((_, ch)) = self.bump() else {
                return Err(TjsError::lex(
                    Span::new(start, self.offset()),
                    "unterminated interpolated string literal",
                ));
            };

            if ch == quote {
                if self.consume_same_string_delimiter(quote) {
                    continue;
                }
                break;
            }

            if ch == '\\' {
                let Some((_, escaped)) = self.bump() else {
                    return Err(TjsError::lex(
                        Span::new(start, self.offset()),
                        "unterminated string escape",
                    ));
                };
                text.push(self.string_escape_char(escaped));
                continue;
            }

            if ch == '&' {
                if !text.is_empty() {
                    parts.push(InterpolatedPart::Text(std::mem::take(&mut text)));
                }
                parts.push(InterpolatedPart::Expr(
                    self.read_semicolon_interpolation(start)?,
                ));
                continue;
            }

            if ch == '$' && self.peek() == Some('{') {
                self.bump();
                if !text.is_empty() {
                    parts.push(InterpolatedPart::Text(std::mem::take(&mut text)));
                }
                parts.push(InterpolatedPart::Expr(
                    self.read_brace_interpolation(start)?,
                ));
                continue;
            }

            text.push(ch);
        }

        if !text.is_empty() || parts.is_empty() {
            parts.push(InterpolatedPart::Text(text));
        }

        Ok(Token {
            kind: TokenKind::InterpolatedString(parts),
            span: Span::new(start, self.offset()),
        })
    }

    fn string_escape_char(&mut self, escaped: char) -> char {
        match escaped {
            'a' => '\u{0007}',
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            'b' => '\u{0008}',
            'f' => '\u{000c}',
            'v' => '\u{000b}',
            '0' => self.read_octal_escape_after_zero(),
            '\\' => '\\',
            '\'' => '\'',
            '"' => '"',
            'x' | 'X' => self.read_variable_hex_escape(),
            'u' => 'u',
            other => other,
        }
    }

    fn read_semicolon_interpolation(&mut self, start: usize) -> Result<String> {
        self.read_interpolation_until(start, InterpolationEnd::Semicolon)
    }

    fn read_brace_interpolation(&mut self, start: usize) -> Result<String> {
        self.read_interpolation_until(start, InterpolationEnd::Brace)
    }

    fn read_interpolation_until(&mut self, start: usize, end: InterpolationEnd) -> Result<String> {
        let mut expr = String::new();
        let mut state = InterpolationScanState::new(end);

        loop {
            let Some((token_start, ch)) = self.bump() else {
                return Err(TjsError::lex(
                    Span::new(start, self.offset()),
                    "unterminated interpolated string expression",
                ));
            };

            if matches!(end, InterpolationEnd::Semicolon)
                && ch == ';'
                && state.paren_stack.is_empty()
                && state.bracket_depth == 0
                && state.brace_depth == 0
            {
                return Ok(expr);
            }

            if matches!(end, InterpolationEnd::Brace) && ch == '}' && state.brace_depth == 1 {
                return Ok(expr);
            }

            if ch.is_whitespace() {
                expr.push_str(&self.source[token_start..self.offset()]);
                continue;
            }

            if ch == '/' && self.peek() == Some('/') {
                while let Some((_, ch)) = self.bump() {
                    if ch == '\n' {
                        break;
                    }
                }
                expr.push_str(&self.source[token_start..self.offset()]);
                continue;
            }

            if ch == '/' && self.peek() == Some('*') {
                self.skip_nested_block_comment(
                    token_start,
                    "unterminated block comment in interpolated string expression",
                )?;
                expr.push_str(&self.source[token_start..self.offset()]);
                continue;
            }

            let token = match ch {
                '0'..='9' => self.lex_number(token_start, ch)?,
                '.' if self.peek().is_some_and(|ch| ch.is_ascii_digit()) => {
                    self.lex_number(token_start, ch)?
                }
                '"' | '\'' => self.lex_string(token_start, ch)?,
                '@' => self.lex_at_prefixed_string(token_start)?,
                '/' if state.expect_operand => self.lex_regexp(token_start)?,
                '<' if self.peek() == Some('%') => {
                    self.bump();
                    self.lex_octet(token_start)?
                }
                ch if is_ident_start(ch) && state.expect_member_name => {
                    self.lex_bare_identifier(token_start, ch)
                }
                ch if is_ident_start(ch) => self.lex_identifier(token_start, ch),
                _ => self.lex_punctuation(token_start, ch)?,
            };
            expr.push_str(&self.source[token_start..self.offset()]);
            observe_interpolation_token(&token.kind, &mut state);
        }
    }

    fn lex_regexp(&mut self, start: usize) -> Result<Token> {
        let mut pattern = String::new();
        let mut escaped = false;

        loop {
            let Some((_, ch)) = self.bump() else {
                return Err(TjsError::lex(
                    Span::new(start, self.offset()),
                    "unterminated regular expression literal",
                ));
            };

            if escaped {
                pattern.push(ch);
                escaped = false;
                continue;
            }

            match ch {
                '\\' => {
                    pattern.push(ch);
                    escaped = true;
                }
                '/' => break,
                _ => pattern.push(ch),
            }
        }

        let mut flags = String::new();
        while self.peek().is_some_and(|ch| ch.is_ascii_lowercase()) {
            flags.push(self.bump().expect("peeked").1);
        }

        Ok(Token {
            kind: TokenKind::RegExp { pattern, flags },
            span: Span::new(start, self.offset()),
        })
    }

    fn lex_octet(&mut self, start: usize) -> Result<Token> {
        let mut bytes = Vec::new();
        let mut high_nibble = None::<u8>;

        loop {
            if self.starts_with("%>") {
                self.consume_str("%>");
                if let Some(value) = high_nibble.take() {
                    bytes.push(value);
                }
                return Ok(Token {
                    kind: TokenKind::Octet(bytes),
                    span: Span::new(start, self.offset()),
                });
            }

            let Some((offset, ch)) = self.bump() else {
                return Err(TjsError::lex(
                    Span::new(start, self.offset()),
                    "unterminated octet literal",
                ));
            };

            if ch.is_whitespace() {
                continue;
            }

            if ch == '/' && self.peek() == Some('/') {
                while let Some(ch) = self.peek() {
                    self.bump();
                    if ch == '\n' {
                        break;
                    }
                }
                continue;
            }

            if ch == '/' && self.peek() == Some('*') {
                self.skip_nested_block_comment(
                    offset,
                    "unterminated block comment in octet literal",
                )?;
                continue;
            }

            if ch == ',' {
                if let Some(value) = high_nibble.take() {
                    bytes.push(value);
                }
                continue;
            }

            if let Some(digit) = ch.to_digit(16) {
                let digit = digit as u8;
                if let Some(high) = high_nibble.take() {
                    bytes.push((high << 4) | digit);
                } else {
                    high_nibble = Some(digit);
                }
            }
        }
    }

    fn read_variable_hex_escape(&mut self) -> char {
        let mut value = 0_u32;
        let mut count = 0_u8;
        while count < 4 {
            let Some(ch) = self.peek() else {
                break;
            };
            let Some(digit) = ch.to_digit(16) else {
                break;
            };
            self.bump();
            value = (value << 4) | digit;
            count += 1;
        }
        char::from_u32(value).unwrap_or(char::REPLACEMENT_CHARACTER)
    }

    fn read_octal_escape_after_zero(&mut self) -> char {
        let mut value = 0_u32;
        while let Some(digit) = self.peek().and_then(|ch| ch.to_digit(8)) {
            self.bump();
            value = value.saturating_mul(8).saturating_add(digit);
        }
        char::from_u32(value).unwrap_or(char::REPLACEMENT_CHARACTER)
    }

    fn consume_same_string_delimiter(&mut self, quote: char) -> bool {
        let saved_chars = self.chars.clone();
        let saved_lookahead = self.lookahead;
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
        if self.peek() == Some(quote) {
            self.bump();
            true
        } else {
            self.chars = saved_chars;
            self.lookahead = saved_lookahead;
            false
        }
    }

    fn lex_identifier(&mut self, start: usize, first: char) -> Token {
        let mut text = String::new();
        text.push(first);
        while self.peek().is_some_and(is_ident_continue) {
            text.push(self.bump().expect("peeked").1);
        }

        let kind = keyword_kind(&text).unwrap_or(TokenKind::Ident(text));
        Token {
            kind,
            span: Span::new(start, self.offset()),
        }
    }

    fn lex_bare_identifier(&mut self, start: usize, first: char) -> Token {
        let mut text = String::new();
        text.push(first);
        while self.peek().is_some_and(is_ident_continue) {
            text.push(self.bump().expect("peeked").1);
        }
        Token {
            kind: TokenKind::Ident(text),
            span: Span::new(start, self.offset()),
        }
    }

    fn lex_punctuation(&mut self, start: usize, first: char) -> Result<Token> {
        let kind = match first {
            ',' => TokenKind::Comma,
            '?' => TokenKind::Question,
            '~' => TokenKind::Tilde,
            '#' => TokenKind::Sharp,
            '$' => TokenKind::Dollar,
            '(' => TokenKind::LeftParen,
            ')' => TokenKind::RightParen,
            '[' => TokenKind::LeftBracket,
            ']' => TokenKind::RightBracket,
            '{' => TokenKind::LeftBrace,
            '}' => TokenKind::RightBrace,
            ':' => TokenKind::Colon,
            ';' => TokenKind::Semicolon,
            '.' => {
                if self.consume_str("..") {
                    TokenKind::Ellipsis
                } else {
                    TokenKind::Dot
                }
            }
            '=' => {
                if self.consume_if('=') {
                    if self.consume_if('=') {
                        TokenKind::DiscernEqual
                    } else {
                        TokenKind::EqualEqual
                    }
                } else if self.consume_if('>') {
                    TokenKind::Comma
                } else {
                    TokenKind::Assign
                }
            }
            '!' => {
                if self.consume_if('=') {
                    if self.consume_if('=') {
                        TokenKind::DiscernNotEqual
                    } else {
                        TokenKind::NotEqual
                    }
                } else {
                    TokenKind::Bang
                }
            }
            '&' => {
                if self.consume_if('&') {
                    if self.consume_if('=') {
                        TokenKind::LogicalAndAssign
                    } else {
                        TokenKind::LogicalAnd
                    }
                } else if self.consume_if('=') {
                    TokenKind::AmpAssign
                } else {
                    TokenKind::Amp
                }
            }
            '|' => {
                if self.consume_if('|') {
                    if self.consume_if('=') {
                        TokenKind::LogicalOrAssign
                    } else {
                        TokenKind::LogicalOr
                    }
                } else if self.consume_if('=') {
                    TokenKind::PipeAssign
                } else {
                    TokenKind::Pipe
                }
            }
            '^' => {
                if self.consume_if('=') {
                    TokenKind::CaretAssign
                } else {
                    TokenKind::Caret
                }
            }
            '+' => {
                if self.consume_if('+') {
                    TokenKind::Increment
                } else if self.consume_if('=') {
                    TokenKind::PlusAssign
                } else {
                    TokenKind::Plus
                }
            }
            '-' => {
                if self.consume_if('-') {
                    TokenKind::Decrement
                } else if self.consume_if('=') {
                    TokenKind::MinusAssign
                } else {
                    TokenKind::Minus
                }
            }
            '*' => {
                if self.consume_if('=') {
                    TokenKind::StarAssign
                } else {
                    TokenKind::Star
                }
            }
            '/' => {
                if self.consume_if('=') {
                    TokenKind::SlashAssign
                } else {
                    TokenKind::Slash
                }
            }
            '\\' => {
                if self.consume_if('=') {
                    TokenKind::BackslashAssign
                } else {
                    TokenKind::Backslash
                }
            }
            '%' => {
                if self.consume_if('=') {
                    TokenKind::PercentAssign
                } else {
                    TokenKind::Percent
                }
            }
            '<' => {
                if self.consume_str("->") {
                    TokenKind::Swap
                } else if self.consume_if('<') {
                    if self.consume_if('=') {
                        TokenKind::ShiftLeftAssign
                    } else {
                        TokenKind::ShiftLeft
                    }
                } else if self.consume_if('=') {
                    TokenKind::LessEqual
                } else {
                    TokenKind::Less
                }
            }
            '>' => {
                if self.consume_str(">>") {
                    if self.consume_if('=') {
                        TokenKind::UnsignedShiftRightAssign
                    } else {
                        TokenKind::UnsignedShiftRight
                    }
                } else if self.consume_if('>') {
                    if self.consume_if('=') {
                        TokenKind::ShiftRightAssign
                    } else {
                        TokenKind::ShiftRight
                    }
                } else if self.consume_if('=') {
                    TokenKind::GreaterEqual
                } else {
                    TokenKind::Greater
                }
            }
            _ => {
                return Err(TjsError::lex(
                    Span::new(start, self.offset()),
                    format!("unexpected character {first:?}"),
                ));
            }
        };

        Ok(Token {
            kind,
            span: Span::new(start, self.offset()),
        })
    }
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic() || !ch.is_ascii()
}

fn is_ident_continue(ch: char) -> bool {
    is_ident_start(ch) || ch.is_ascii_digit()
}

fn non_decimal_real_value(
    digits: &[u32],
    point_index: usize,
    radix: u32,
    binary_exponent: i32,
) -> f64 {
    let radix_bits = match radix {
        2 => 1_i64,
        8 => 3_i64,
        16 => 4_i64,
        _ => unreachable!("TJS2 non-decimal radices are powers of two"),
    };
    let point_index = point_index as i64;
    let binary_exponent = binary_exponent as i64;
    let mut value = 0.0_f64;

    for (index, digit) in digits.iter().copied().enumerate() {
        if digit == 0 {
            continue;
        }
        let index = index as i64;
        let digit_exponent = if index < point_index {
            (point_index - index - 1) * radix_bits
        } else {
            -((index - point_index + 1) * radix_bits)
        };
        value += digit as f64 * pow2_f64(digit_exponent + binary_exponent);
    }

    value
}

fn pow2_f64(exponent: i64) -> f64 {
    if exponent > 1023 {
        f64::INFINITY
    } else if exponent < -1074 {
        0.0
    } else {
        2.0_f64.powi(exponent as i32)
    }
}

fn keyword_kind(text: &str) -> Option<TokenKind> {
    Some(match text {
        "break" => TokenKind::KwBreak,
        "case" => TokenKind::KwCase,
        "catch" => TokenKind::KwCatch,
        "class" => TokenKind::KwClass,
        "const" => TokenKind::KwConst,
        "continue" => TokenKind::KwContinue,
        "debugger" => TokenKind::KwDebugger,
        "default" => TokenKind::KwDefault,
        "delete" => TokenKind::KwDelete,
        "do" => TokenKind::KwDo,
        "else" => TokenKind::KwElse,
        "enum" => TokenKind::KwEnum,
        "export" => TokenKind::KwExport,
        "extends" => TokenKind::KwExtends,
        "false" => TokenKind::KwFalse,
        "finally" => TokenKind::KwFinally,
        "for" => TokenKind::KwFor,
        "function" => TokenKind::KwFunction,
        "global" => TokenKind::KwGlobal,
        "goto" => TokenKind::KwGoto,
        "getter" => TokenKind::KwGetter,
        "if" => TokenKind::KwIf,
        "import" => TokenKind::KwImport,
        "in" => TokenKind::KwIn,
        "incontextof" => TokenKind::KwInContextOf,
        "Infinity" => TokenKind::KwInfinity,
        "instanceof" => TokenKind::KwInstanceOf,
        "int" => TokenKind::KwInt,
        "invalidate" => TokenKind::KwInvalidate,
        "isvalid" => TokenKind::KwIsValid,
        "NaN" => TokenKind::KwNan,
        "new" => TokenKind::KwNew,
        "null" => TokenKind::KwNull,
        "octet" => TokenKind::KwOctet,
        "private" => TokenKind::KwPrivate,
        "property" => TokenKind::KwProperty,
        "protected" => TokenKind::KwProtected,
        "public" => TokenKind::KwPublic,
        "real" => TokenKind::KwReal,
        "return" => TokenKind::KwReturn,
        "setter" => TokenKind::KwSetter,
        "static" => TokenKind::KwStatic,
        "string" => TokenKind::KwString,
        "super" => TokenKind::KwSuper,
        "switch" => TokenKind::KwSwitch,
        "synchronized" => TokenKind::KwSynchronized,
        "this" => TokenKind::KwThis,
        "throw" => TokenKind::KwThrow,
        "true" => TokenKind::KwTrue,
        "try" => TokenKind::KwTry,
        "typeof" => TokenKind::KwTypeOf,
        "var" => TokenKind::KwVar,
        "void" => TokenKind::KwVoid,
        "while" => TokenKind::KwWhile,
        "with" => TokenKind::KwWith,
        _ => return None,
    })
}

fn token_can_be_member_name(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Ident(_)
            | TokenKind::KwBreak
            | TokenKind::KwCase
            | TokenKind::KwCatch
            | TokenKind::KwClass
            | TokenKind::KwConst
            | TokenKind::KwContinue
            | TokenKind::KwDebugger
            | TokenKind::KwDefault
            | TokenKind::KwDelete
            | TokenKind::KwDo
            | TokenKind::KwElse
            | TokenKind::KwEnum
            | TokenKind::KwExport
            | TokenKind::KwExtends
            | TokenKind::KwFalse
            | TokenKind::KwFinally
            | TokenKind::KwFor
            | TokenKind::KwFunction
            | TokenKind::KwGlobal
            | TokenKind::KwGoto
            | TokenKind::KwGetter
            | TokenKind::KwIf
            | TokenKind::KwImport
            | TokenKind::KwIn
            | TokenKind::KwInContextOf
            | TokenKind::KwInfinity
            | TokenKind::KwInstanceOf
            | TokenKind::KwInt
            | TokenKind::KwInvalidate
            | TokenKind::KwIsValid
            | TokenKind::KwNan
            | TokenKind::KwNew
            | TokenKind::KwNull
            | TokenKind::KwOctet
            | TokenKind::KwPrivate
            | TokenKind::KwProperty
            | TokenKind::KwProtected
            | TokenKind::KwPublic
            | TokenKind::KwReal
            | TokenKind::KwReturn
            | TokenKind::KwSetter
            | TokenKind::KwStatic
            | TokenKind::KwString
            | TokenKind::KwSuper
            | TokenKind::KwSwitch
            | TokenKind::KwSynchronized
            | TokenKind::KwThis
            | TokenKind::KwThrow
            | TokenKind::KwTrue
            | TokenKind::KwTry
            | TokenKind::KwTypeOf
            | TokenKind::KwVar
            | TokenKind::KwVoid
            | TokenKind::KwWhile
            | TokenKind::KwWith
    )
}

fn observe_grouping_context(paren_stack: &mut [ParenContext], kind: &TokenKind) {
    if kind == &TokenKind::RightParen {
        return;
    }
    if let Some(ParenContext::Grouping(context)) = paren_stack.last_mut() {
        context.observe(kind);
    }
}

fn observe_interpolation_token(kind: &TokenKind, state: &mut InterpolationScanState) {
    let was_expect_operand = state.expect_operand;
    let was_expect_member_name = state.expect_member_name;
    let is_member_name = was_expect_member_name && token_can_be_member_name(kind);
    let closed_paren = if kind == &TokenKind::RightParen {
        state.paren_stack.pop()
    } else {
        None
    };
    let closes_cast_head = matches!(
        closed_paren,
        Some(ParenContext::Grouping(GroupingContext::CastType))
    );
    let closes_function_expression_body = kind == &TokenKind::RightBrace
        && state.brace_stack.pop() == Some(BraceContext::FunctionExpressionBody);

    observe_grouping_context(&mut state.paren_stack, kind);
    match kind {
        TokenKind::LeftParen => {
            state
                .paren_stack
                .push(ParenContext::Grouping(GroupingContext::new(
                    was_expect_operand,
                )));
        }
        TokenKind::LeftBracket => {
            state.bracket_depth += 1;
        }
        TokenKind::RightBracket => {
            state.bracket_depth = state.bracket_depth.saturating_sub(1);
        }
        TokenKind::LeftBrace => {
            let context = match state.pending_function_bodies.pop() {
                Some(FunctionContext::Declaration) => BraceContext::FunctionDeclarationBody,
                Some(FunctionContext::Expression) => BraceContext::FunctionExpressionBody,
                None => BraceContext::Block,
            };
            state.brace_stack.push(context);
            state.brace_depth += 1;
        }
        TokenKind::RightBrace => {
            state.brace_depth = state.brace_depth.saturating_sub(1);
        }
        _ => {}
    }

    if kind == &TokenKind::KwFunction && !is_member_name {
        state
            .pending_function_bodies
            .push(FunctionContext::Expression);
    }

    state.expect_operand = if is_member_name {
        false
    } else if closes_cast_head {
        true
    } else if closes_function_expression_body {
        false
    } else {
        token_expects_operand_after(kind, was_expect_operand)
    };
    state.expect_member_name = kind == &TokenKind::Dot;
}

fn token_expects_operand_after(kind: &TokenKind, was_expecting_operand: bool) -> bool {
    match kind {
        TokenKind::Ident(_)
        | TokenKind::Integer(_)
        | TokenKind::Real(_)
        | TokenKind::String(_)
        | TokenKind::InterpolatedString(_)
        | TokenKind::Octet(_)
        | TokenKind::RegExp { .. }
        | TokenKind::KwFalse
        | TokenKind::KwTrue
        | TokenKind::KwNull
        | TokenKind::KwVoid
        | TokenKind::KwThis
        | TokenKind::KwSuper
        | TokenKind::KwGlobal
        | TokenKind::KwNan
        | TokenKind::KwInfinity
        | TokenKind::RightParen
        | TokenKind::RightBracket
        | TokenKind::Increment
        | TokenKind::Decrement => false,

        TokenKind::Bang if !was_expecting_operand => false,
        TokenKind::KwIsValid if !was_expecting_operand => false,

        TokenKind::Eof => false,

        _ => true,
    }
}

fn token_starts_control_header(kind: &TokenKind, was_statement_start: bool) -> bool {
    was_statement_start
        && matches!(
            kind,
            TokenKind::KwIf
                | TokenKind::KwWhile
                | TokenKind::KwFor
                | TokenKind::KwWith
                | TokenKind::KwSwitch
                | TokenKind::KwCatch
                | TokenKind::KwSynchronized
        )
}

fn token_starts_for_header(kind: &TokenKind, was_statement_start: bool) -> bool {
    was_statement_start && matches!(kind, TokenKind::KwFor)
}

fn token_starts_case_header(kind: &TokenKind, was_statement_start: bool) -> bool {
    was_statement_start && matches!(kind, TokenKind::KwCase | TokenKind::KwDefault)
}

fn token_starts_statement_after(
    kind: &TokenKind,
    closes_control_header: bool,
    closes_function_expression_body: bool,
    closes_case_header: bool,
) -> bool {
    if closes_control_header || closes_case_header {
        return true;
    }
    if closes_function_expression_body {
        return false;
    }
    matches!(
        kind,
        TokenKind::Semicolon
            | TokenKind::LeftBrace
            | TokenKind::RightBrace
            | TokenKind::KwDo
            | TokenKind::KwElse
            | TokenKind::KwFinally
            | TokenKind::KwPrivate
            | TokenKind::KwProtected
            | TokenKind::KwPublic
            | TokenKind::KwStatic
    )
}

fn eval_pp_expr(source: &str, values: &mut BTreeMap<String, i32>) -> Result<i32> {
    PpParser::new(source, values).parse()
}

#[derive(Clone, Debug, PartialEq)]
enum PpToken {
    Eof,
    Ident(String),
    Number(i32),
    Comma,
    Assign,
    NotEqual,
    Equal,
    LogicalOr,
    LogicalAnd,
    BitOr,
    BitXor,
    BitAnd,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Bang,
    LeftParen,
    RightParen,
}

struct PpParser<'a, 'b> {
    source: &'a str,
    chars: std::str::CharIndices<'a>,
    lookahead_char: Option<(usize, char)>,
    current: PpToken,
    values: &'b mut BTreeMap<String, i32>,
}

impl<'a, 'b> PpParser<'a, 'b> {
    fn new(source: &'a str, values: &'b mut BTreeMap<String, i32>) -> Self {
        let mut chars = source.char_indices();
        let lookahead_char = chars.next();
        Self {
            source,
            chars,
            lookahead_char,
            current: PpToken::Eof,
            values,
        }
    }

    fn parse(mut self) -> Result<i32> {
        self.bump_token()?;
        let value = self.parse_comma()?;
        if self.current != PpToken::Eof {
            return Err(TjsError::lex(
                Span::empty(self.offset()),
                "unexpected token in preprocessor expression",
            ));
        }
        Ok(value)
    }

    fn parse_comma(&mut self) -> Result<i32> {
        let mut value = self.parse_logical_or()?;
        while self.current == PpToken::Comma {
            self.bump_token()?;
            value = self.parse_logical_or()?;
        }
        Ok(value)
    }

    fn parse_assignment(&mut self) -> Result<i32> {
        if let PpToken::Ident(name) = self.current.clone() {
            let saved_chars = self.chars.clone();
            let saved_lookahead = self.lookahead_char;
            let saved_current = self.current.clone();
            self.bump_token()?;
            if self.current == PpToken::Assign {
                self.bump_token()?;
                let value = self.parse_equality()?;
                self.values.insert(name, value);
                return Ok(value);
            }
            self.chars = saved_chars;
            self.lookahead_char = saved_lookahead;
            self.current = saved_current;
        }
        self.parse_equality()
    }

    fn parse_logical_or(&mut self) -> Result<i32> {
        let mut value = self.parse_logical_and()?;
        while self.current == PpToken::LogicalOr {
            self.bump_token()?;
            let rhs = self.parse_logical_and()?;
            value = i32::from(value != 0 || rhs != 0);
        }
        Ok(value)
    }

    fn parse_logical_and(&mut self) -> Result<i32> {
        let mut value = self.parse_bit_or()?;
        while self.current == PpToken::LogicalAnd {
            self.bump_token()?;
            let rhs = self.parse_bit_or()?;
            value = i32::from(value != 0 && rhs != 0);
        }
        Ok(value)
    }

    fn parse_bit_or(&mut self) -> Result<i32> {
        let mut value = self.parse_bit_xor()?;
        while self.current == PpToken::BitOr {
            self.bump_token()?;
            value |= self.parse_bit_xor()?;
        }
        Ok(value)
    }

    fn parse_bit_xor(&mut self) -> Result<i32> {
        let mut value = self.parse_bit_and()?;
        while self.current == PpToken::BitXor {
            self.bump_token()?;
            value ^= self.parse_bit_and()?;
        }
        Ok(value)
    }

    fn parse_bit_and(&mut self) -> Result<i32> {
        let mut value = self.parse_assignment()?;
        while self.current == PpToken::BitAnd {
            self.bump_token()?;
            value &= self.parse_assignment()?;
        }
        Ok(value)
    }

    fn parse_equality(&mut self) -> Result<i32> {
        let mut value = self.parse_compare()?;
        loop {
            match self.current {
                PpToken::Equal => {
                    self.bump_token()?;
                    value = i32::from(value == self.parse_compare()?);
                }
                PpToken::NotEqual => {
                    self.bump_token()?;
                    value = i32::from(value != self.parse_compare()?);
                }
                _ => return Ok(value),
            }
        }
    }

    fn parse_compare(&mut self) -> Result<i32> {
        let mut value = self.parse_add()?;
        loop {
            match self.current {
                PpToken::Less => {
                    self.bump_token()?;
                    value = i32::from(value < self.parse_add()?);
                }
                PpToken::Greater => {
                    self.bump_token()?;
                    value = i32::from(value > self.parse_add()?);
                }
                PpToken::LessEqual => {
                    self.bump_token()?;
                    value = i32::from(value <= self.parse_add()?);
                }
                PpToken::GreaterEqual => {
                    self.bump_token()?;
                    value = i32::from(value >= self.parse_add()?);
                }
                _ => return Ok(value),
            }
        }
    }

    fn parse_add(&mut self) -> Result<i32> {
        let mut value = self.parse_mul()?;
        loop {
            match self.current {
                PpToken::Plus => {
                    self.bump_token()?;
                    value = value.wrapping_add(self.parse_mul()?);
                }
                PpToken::Minus => {
                    self.bump_token()?;
                    value = value.wrapping_sub(self.parse_mul()?);
                }
                _ => return Ok(value),
            }
        }
    }

    fn parse_mul(&mut self) -> Result<i32> {
        let mut value = self.parse_unary()?;
        loop {
            match self.current {
                PpToken::Star => {
                    self.bump_token()?;
                    value = value.wrapping_mul(self.parse_unary()?);
                }
                PpToken::Slash => {
                    self.bump_token()?;
                    let rhs = self.parse_unary()?;
                    if rhs == 0 {
                        return Err(TjsError::lex(
                            Span::empty(self.offset()),
                            "division by zero in preprocessor expression",
                        ));
                    } else {
                        value /= rhs;
                    }
                }
                PpToken::Percent => {
                    self.bump_token()?;
                    let rhs = self.parse_unary()?;
                    if rhs == 0 {
                        return Err(TjsError::lex(
                            Span::empty(self.offset()),
                            "modulo by zero in preprocessor expression",
                        ));
                    } else {
                        value %= rhs;
                    }
                }
                _ => return Ok(value),
            }
        }
    }

    fn parse_unary(&mut self) -> Result<i32> {
        match self.current {
            PpToken::Bang => {
                self.bump_token()?;
                Ok(i32::from(self.parse_unary()? == 0))
            }
            PpToken::Plus => {
                self.bump_token()?;
                self.parse_unary()
            }
            PpToken::Minus => {
                self.bump_token()?;
                Ok(self.parse_unary()?.wrapping_neg())
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Result<i32> {
        match self.current.clone() {
            PpToken::Number(value) => {
                self.bump_token()?;
                Ok(value)
            }
            PpToken::Ident(name) => {
                self.bump_token()?;
                Ok(*self.values.get(&name).unwrap_or(&0))
            }
            PpToken::LeftParen => {
                self.bump_token()?;
                let value = self.parse_comma()?;
                if self.current != PpToken::RightParen {
                    return Err(TjsError::lex(
                        Span::empty(self.offset()),
                        "expected ')' in preprocessor expression",
                    ));
                }
                self.bump_token()?;
                Ok(value)
            }
            _ => Err(TjsError::lex(
                Span::empty(self.offset()),
                "expected value in preprocessor expression",
            )),
        }
    }

    fn bump_token(&mut self) -> Result<()> {
        self.skip_ws();
        let Some((start, ch)) = self.bump_char() else {
            self.current = PpToken::Eof;
            return Ok(());
        };

        self.current = match ch {
            '0'..='9' => self.lex_pp_number(start, ch)?,
            ch if is_ident_start(ch) => self.lex_pp_ident(ch),
            ',' => PpToken::Comma,
            '=' => {
                if self.consume_char_if('=') {
                    PpToken::Equal
                } else {
                    PpToken::Assign
                }
            }
            '!' => {
                if self.consume_char_if('=') {
                    PpToken::NotEqual
                } else {
                    PpToken::Bang
                }
            }
            '|' => {
                if self.consume_char_if('|') {
                    PpToken::LogicalOr
                } else {
                    PpToken::BitOr
                }
            }
            '&' => {
                if self.consume_char_if('&') {
                    PpToken::LogicalAnd
                } else {
                    PpToken::BitAnd
                }
            }
            '^' => PpToken::BitXor,
            '<' => {
                if self.consume_char_if('=') {
                    PpToken::LessEqual
                } else {
                    PpToken::Less
                }
            }
            '>' => {
                if self.consume_char_if('=') {
                    PpToken::GreaterEqual
                } else {
                    PpToken::Greater
                }
            }
            '+' => PpToken::Plus,
            '-' => PpToken::Minus,
            '*' => PpToken::Star,
            '/' => PpToken::Slash,
            '%' => PpToken::Percent,
            '(' => PpToken::LeftParen,
            ')' => PpToken::RightParen,
            _ => {
                return Err(TjsError::lex(
                    Span::new(start, self.offset()),
                    format!("unexpected preprocessor character {ch:?}"),
                ));
            }
        };
        Ok(())
    }

    fn lex_pp_number(&mut self, start: usize, first: char) -> Result<PpToken> {
        if first == '0' {
            let value = match self.peek_char() {
                Some('x' | 'X') => {
                    self.bump_char();
                    self.lex_pp_non_decimal_number(start, 16, None)?
                }
                Some('b' | 'B') => {
                    self.bump_char();
                    self.lex_pp_non_decimal_number(start, 2, None)?
                }
                Some('p' | 'P') => {
                    return Err(TjsError::lex(
                        Span::new(start, self.offset()),
                        "invalid preprocessor number",
                    ));
                }
                Some('.' | 'e' | 'E') => self.lex_pp_decimal_number(first)?,
                _ => self.lex_pp_non_decimal_number(start, 8, Some(0))?,
            };
            return Ok(PpToken::Number(value));
        }

        Ok(PpToken::Number(self.lex_pp_decimal_number(first)?))
    }

    fn lex_pp_decimal_number(&mut self, first: char) -> Result<i32> {
        let mut text = String::new();
        text.push(first);
        let mut is_real = false;

        while self.peek_char().is_some_and(|ch| ch.is_ascii_digit()) {
            text.push(self.bump_char().expect("peeked").1);
        }

        if self.peek_char() == Some('.') {
            is_real = true;
            text.push(self.bump_char().expect("peeked").1);
            while self.peek_char().is_some_and(|ch| ch.is_ascii_digit()) {
                text.push(self.bump_char().expect("peeked").1);
            }
        }

        if matches!(self.peek_char(), Some('e' | 'E')) {
            is_real = true;
            let exponent_start = text.len();
            text.push(self.bump_char().expect("peeked").1);
            self.skip_ws();
            if matches!(self.peek_char(), Some('+' | '-')) {
                text.push(self.bump_char().expect("peeked").1);
                self.skip_ws();
            }
            let digit_start = text.len();
            while self.peek_char().is_some_and(|ch| ch.is_ascii_digit()) {
                text.push(self.bump_char().expect("peeked").1);
            }
            if digit_start == text.len() {
                text.truncate(exponent_start);
            }
        }

        if is_real {
            Ok(text.parse::<f64>().unwrap_or(0.0) as i32)
        } else {
            let mut value = 0_i64;
            for ch in text.chars() {
                let digit = ch.to_digit(10).expect("decimal digits collected");
                value = value.wrapping_mul(10).wrapping_add(digit as i64);
            }
            Ok(value as i32)
        }
    }

    fn lex_pp_non_decimal_number(
        &mut self,
        start: usize,
        radix: u32,
        initial_digit: Option<u32>,
    ) -> Result<i32> {
        let mut integer = 0_i64;
        let mut digits = Vec::new();
        let mut point_index = 0_usize;
        let mut saw_digit = false;
        let mut saw_point = false;
        let mut is_real = false;

        if let Some(digit) = initial_digit {
            integer = digit as i64;
            digits.push(digit);
            point_index = 1;
            saw_digit = true;
        }

        loop {
            if let Some(digit) = self.peek_char().and_then(|ch| ch.to_digit(radix)) {
                self.bump_char();
                saw_digit = true;
                digits.push(digit);
                if !saw_point {
                    point_index += 1;
                    integer = integer
                        .wrapping_mul(radix as i64)
                        .wrapping_add(digit as i64);
                }
                continue;
            }

            if self.peek_char() == Some('.') && !saw_point {
                self.bump_char();
                saw_point = true;
                is_real = true;
                continue;
            }

            break;
        }

        if !saw_digit && !saw_point {
            return Err(TjsError::lex(
                Span::new(start, self.offset()),
                "invalid preprocessor number",
            ));
        }

        let mut exponent = 0_i32;
        if matches!(self.peek_char(), Some('p' | 'P')) {
            is_real = true;
            self.bump_char();
            self.skip_ws();
            let negative = if self.consume_char_if('+') {
                self.skip_ws();
                false
            } else if self.consume_char_if('-') {
                self.skip_ws();
                true
            } else {
                false
            };
            while let Some(digit) = self.peek_char().and_then(|ch| ch.to_digit(10)) {
                self.bump_char();
                exponent = exponent.saturating_mul(10).saturating_add(digit as i32);
            }
            if negative {
                exponent = -exponent;
            }
        }

        if is_real {
            Ok(non_decimal_real_value(&digits, point_index, radix, exponent) as i32)
        } else {
            Ok(integer as i32)
        }
    }

    fn lex_pp_ident(&mut self, first: char) -> PpToken {
        let mut text = String::new();
        text.push(first);
        while self.peek_char().is_some_and(is_ident_continue) {
            text.push(self.bump_char().expect("peeked").1);
        }
        PpToken::Ident(text)
    }

    fn skip_ws(&mut self) {
        while self.peek_char().is_some_and(char::is_whitespace) {
            self.bump_char();
        }
    }

    fn offset(&self) -> usize {
        self.lookahead_char
            .map(|(offset, _)| offset)
            .unwrap_or(self.source.len())
    }

    fn bump_char(&mut self) -> Option<(usize, char)> {
        let current = self.lookahead_char;
        self.lookahead_char = self.chars.next();
        current
    }

    fn peek_char(&self) -> Option<char> {
        self.lookahead_char.map(|(_, ch)| ch)
    }

    fn consume_char_if(&mut self, expected: char) -> bool {
        if self.peek_char() == Some(expected) {
            self.bump_char();
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<TokenKind> {
        lex(source)
            .expect("lex")
            .into_iter()
            .map(|token| token.kind)
            .collect()
    }

    #[test]
    fn lexes_numeric_radices_and_reals() {
        assert_eq!(
            kinds("42 052 0x2a 0b101010 1.5 2e3 0x1p2 0x1.8p1 0b1.1p1 01.4p3"),
            vec![
                TokenKind::Integer(42),
                TokenKind::Integer(42),
                TokenKind::Integer(42),
                TokenKind::Integer(42),
                TokenKind::Real(1.5),
                TokenKind::Real(2000.0),
                TokenKind::Real(4.0),
                TokenKind::Real(3.0),
                TokenKind::Real(3.0),
                TokenKind::Real(12.0),
                TokenKind::Eof,
            ]
        );

        assert_eq!(
            kinds("1e +2 1..foo"),
            vec![
                TokenKind::Real(100.0),
                TokenKind::Real(1.0),
                TokenKind::Dot,
                TokenKind::Ident("foo".to_string()),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_long_non_decimal_reals_without_pre_scale_overflow() {
        let source = format!("0x{}p-1000", "f".repeat(400));
        let tokens = lex(&source).expect("lex");
        let TokenKind::Real(value) = tokens[0].kind else {
            panic!("expected real literal");
        };
        assert!(value.is_finite());
        assert!(value > 2.0_f64.powi(599));
        assert!(value < 2.0_f64.powi(601));
    }

    #[test]
    fn lexes_longest_operators() {
        assert_eq!(
            kinds("=== !== >>>= >>> >>= >> <<= << <-> ... =>"),
            vec![
                TokenKind::DiscernEqual,
                TokenKind::DiscernNotEqual,
                TokenKind::UnsignedShiftRightAssign,
                TokenKind::UnsignedShiftRight,
                TokenKind::ShiftRightAssign,
                TokenKind::ShiftRight,
                TokenKind::ShiftLeftAssign,
                TokenKind::ShiftLeft,
                TokenKind::Swap,
                TokenKind::Ellipsis,
                TokenKind::Comma,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_strings_and_comments() {
        assert_eq!(
            kinds(
                r#""a\n\u0042" // comment
                /* block /* nested */ */ 'c\x64' "\x4142" "\X4142" @"\x4142" @"\X4142""#
            ),
            vec![
                TokenKind::String("a\nu0042".to_string()),
                TokenKind::String("cd".to_string()),
                TokenKind::String("\u{4142}\u{4142}".to_string()),
                TokenKind::InterpolatedString(vec![InterpolatedPart::Text("\u{4142}".to_string())]),
                TokenKind::InterpolatedString(vec![InterpolatedPart::Text("\u{4142}".to_string())]),
                TokenKind::Eof,
            ]
        );

        assert_eq!(
            kinds(r#""\012" "\u0041""#),
            vec![TokenKind::String("\nu0041".to_string()), TokenKind::Eof]
        );
    }

    #[test]
    fn lexes_shebang_as_line_comment() {
        assert_eq!(
            kinds("#!/usr/bin/env tjs\nvar x = 1;"),
            vec![
                TokenKind::KwVar,
                TokenKind::Ident("x".to_string()),
                TokenKind::Assign,
                TokenKind::Integer(1),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_tjs2_frontend_literals() {
        assert_eq!(
            kinds(r#"/a\/b/gi .5 <% 11 zz 22,3 %> @"plain""#),
            vec![
                TokenKind::RegExp {
                    pattern: r#"a\/b"#.to_string(),
                    flags: "gi".to_string(),
                },
                TokenKind::Real(0.5),
                TokenKind::Octet(vec![0x11, 0x22, 0x03]),
                TokenKind::InterpolatedString(vec![InterpolatedPart::Text("plain".to_string())]),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_krkrz_regexp_termination_and_flags() {
        assert_eq!(
            kinds(r#"/[/]/; /a/U1_;"#),
            vec![
                TokenKind::RegExp {
                    pattern: "[".to_string(),
                    flags: String::new(),
                },
                TokenKind::RightBracket,
                TokenKind::Slash,
                TokenKind::Semicolon,
                TokenKind::RegExp {
                    pattern: "a".to_string(),
                    flags: String::new(),
                },
                TokenKind::Ident("U1_".to_string()),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_spaced_interpolated_string_marker() {
        assert_eq!(
            kinds(r#"@ "score=&value;""#),
            vec![
                TokenKind::InterpolatedString(vec![
                    InterpolatedPart::Text("score=".to_string()),
                    InterpolatedPart::Expr("value".to_string()),
                ]),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_semicolon_interpolation_with_balanced_braces() {
        assert_eq!(
            kinds(r#"@"&function(){ return 1; };""#),
            vec![
                TokenKind::InterpolatedString(vec![InterpolatedPart::Expr(
                    "function(){ return 1; }".to_string(),
                )]),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_regexp_literals_inside_interpolations() {
        assert_eq!(
            kinds(r#"@"&/;/.test(x);""#),
            vec![
                TokenKind::InterpolatedString(vec![InterpolatedPart::Expr(
                    "/;/.test(x)".to_string(),
                )]),
                TokenKind::Eof,
            ]
        );

        assert_eq!(
            kinds(r#"@"${/}/.test(x)}""#),
            vec![
                TokenKind::InterpolatedString(vec![InterpolatedPart::Expr(
                    "/}/.test(x)".to_string(),
                )]),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_division_after_function_literals_inside_interpolations() {
        assert_eq!(
            kinds(r#"@"&1 + function(){} / 2;""#),
            vec![
                TokenKind::InterpolatedString(vec![InterpolatedPart::Expr(
                    "1 + function(){} / 2".to_string(),
                )]),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_leading_dot_real_before_member_access() {
        assert_eq!(
            kinds(".5.toString()"),
            vec![
                TokenKind::Real(0.5),
                TokenKind::Dot,
                TokenKind::Ident("toString".to_string()),
                TokenKind::LeftParen,
                TokenKind::RightParen,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_division_after_keyword_member_names() {
        assert_eq!(
            kinds("obj.int / 2; .string / 3;"),
            vec![
                TokenKind::Ident("obj".to_string()),
                TokenKind::Dot,
                TokenKind::Ident("int".to_string()),
                TokenKind::Slash,
                TokenKind::Integer(2),
                TokenKind::Semicolon,
                TokenKind::Dot,
                TokenKind::Ident("string".to_string()),
                TokenKind::Slash,
                TokenKind::Integer(3),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_keyword_function_member_without_pending_body() {
        assert_eq!(
            kinds("var f = obj.function; if (ok) {} /x/.test(s);"),
            vec![
                TokenKind::KwVar,
                TokenKind::Ident("f".to_string()),
                TokenKind::Assign,
                TokenKind::Ident("obj".to_string()),
                TokenKind::Dot,
                TokenKind::Ident("function".to_string()),
                TokenKind::Semicolon,
                TokenKind::KwIf,
                TokenKind::LeftParen,
                TokenKind::Ident("ok".to_string()),
                TokenKind::RightParen,
                TokenKind::LeftBrace,
                TokenKind::RightBrace,
                TokenKind::RegExp {
                    pattern: "x".to_string(),
                    flags: String::new(),
                },
                TokenKind::Dot,
                TokenKind::Ident("test".to_string()),
                TokenKind::LeftParen,
                TokenKind::Ident("s".to_string()),
                TokenKind::RightParen,
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_regexp_statement_after_control_header() {
        assert_eq!(
            kinds("if (ok) /x/.test(s);"),
            vec![
                TokenKind::KwIf,
                TokenKind::LeftParen,
                TokenKind::Ident("ok".to_string()),
                TokenKind::RightParen,
                TokenKind::RegExp {
                    pattern: "x".to_string(),
                    flags: String::new(),
                },
                TokenKind::Dot,
                TokenKind::Ident("test".to_string()),
                TokenKind::LeftParen,
                TokenKind::Ident("s".to_string()),
                TokenKind::RightParen,
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_regexp_statement_after_block_end() {
        assert_eq!(
            kinds("if (ok) {} /x/.test(s);"),
            vec![
                TokenKind::KwIf,
                TokenKind::LeftParen,
                TokenKind::Ident("ok".to_string()),
                TokenKind::RightParen,
                TokenKind::LeftBrace,
                TokenKind::RightBrace,
                TokenKind::RegExp {
                    pattern: "x".to_string(),
                    flags: String::new(),
                },
                TokenKind::Dot,
                TokenKind::Ident("test".to_string()),
                TokenKind::LeftParen,
                TokenKind::Ident("s".to_string()),
                TokenKind::RightParen,
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_regexp_after_parenthesized_cast() {
        assert_eq!(
            kinds("var s = (string) /a/; var n = (int) /1/; foo(string) / 2;"),
            vec![
                TokenKind::KwVar,
                TokenKind::Ident("s".to_string()),
                TokenKind::Assign,
                TokenKind::LeftParen,
                TokenKind::KwString,
                TokenKind::RightParen,
                TokenKind::RegExp {
                    pattern: "a".to_string(),
                    flags: String::new(),
                },
                TokenKind::Semicolon,
                TokenKind::KwVar,
                TokenKind::Ident("n".to_string()),
                TokenKind::Assign,
                TokenKind::LeftParen,
                TokenKind::KwInt,
                TokenKind::RightParen,
                TokenKind::RegExp {
                    pattern: "1".to_string(),
                    flags: String::new(),
                },
                TokenKind::Semicolon,
                TokenKind::Ident("foo".to_string()),
                TokenKind::LeftParen,
                TokenKind::KwString,
                TokenKind::RightParen,
                TokenKind::Slash,
                TokenKind::Integer(2),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_division_after_postfix_eval_and_function_expression() {
        assert_eq!(
            kinds(
                "var y = source! / 2; var x = function(){} / 2; var z = ok ? 1 : function(){} / 2;"
            ),
            vec![
                TokenKind::KwVar,
                TokenKind::Ident("y".to_string()),
                TokenKind::Assign,
                TokenKind::Ident("source".to_string()),
                TokenKind::Bang,
                TokenKind::Slash,
                TokenKind::Integer(2),
                TokenKind::Semicolon,
                TokenKind::KwVar,
                TokenKind::Ident("x".to_string()),
                TokenKind::Assign,
                TokenKind::KwFunction,
                TokenKind::LeftParen,
                TokenKind::RightParen,
                TokenKind::LeftBrace,
                TokenKind::RightBrace,
                TokenKind::Slash,
                TokenKind::Integer(2),
                TokenKind::Semicolon,
                TokenKind::KwVar,
                TokenKind::Ident("z".to_string()),
                TokenKind::Assign,
                TokenKind::Ident("ok".to_string()),
                TokenKind::Question,
                TokenKind::Integer(1),
                TokenKind::Colon,
                TokenKind::KwFunction,
                TokenKind::LeftParen,
                TokenKind::RightParen,
                TokenKind::LeftBrace,
                TokenKind::RightBrace,
                TokenKind::Slash,
                TokenKind::Integer(2),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_division_after_expression_if_grouping() {
        assert_eq!(
            kinds("return a if (b) / c;"),
            vec![
                TokenKind::KwReturn,
                TokenKind::Ident("a".to_string()),
                TokenKind::KwIf,
                TokenKind::LeftParen,
                TokenKind::Ident("b".to_string()),
                TokenKind::RightParen,
                TokenKind::Slash,
                TokenKind::Ident("c".to_string()),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_division_after_function_expression_in_for_header() {
        assert_eq!(
            kinds("for (; function(){} / 2; ) ; for (;; function(){} / 2) ;"),
            vec![
                TokenKind::KwFor,
                TokenKind::LeftParen,
                TokenKind::Semicolon,
                TokenKind::KwFunction,
                TokenKind::LeftParen,
                TokenKind::RightParen,
                TokenKind::LeftBrace,
                TokenKind::RightBrace,
                TokenKind::Slash,
                TokenKind::Integer(2),
                TokenKind::Semicolon,
                TokenKind::RightParen,
                TokenKind::Semicolon,
                TokenKind::KwFor,
                TokenKind::LeftParen,
                TokenKind::Semicolon,
                TokenKind::Semicolon,
                TokenKind::KwFunction,
                TokenKind::LeftParen,
                TokenKind::RightParen,
                TokenKind::LeftBrace,
                TokenKind::RightBrace,
                TokenKind::Slash,
                TokenKind::Integer(2),
                TokenKind::RightParen,
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_regexp_statement_after_case_header_if() {
        assert_eq!(
            kinds("switch (x) { case 1: if (ok) /x/.test(s); }"),
            vec![
                TokenKind::KwSwitch,
                TokenKind::LeftParen,
                TokenKind::Ident("x".to_string()),
                TokenKind::RightParen,
                TokenKind::LeftBrace,
                TokenKind::KwCase,
                TokenKind::Integer(1),
                TokenKind::Colon,
                TokenKind::KwIf,
                TokenKind::LeftParen,
                TokenKind::Ident("ok".to_string()),
                TokenKind::RightParen,
                TokenKind::RegExp {
                    pattern: "x".to_string(),
                    flags: String::new(),
                },
                TokenKind::Dot,
                TokenKind::Ident("test".to_string()),
                TokenKind::LeftParen,
                TokenKind::Ident("s".to_string()),
                TokenKind::RightParen,
                TokenKind::Semicolon,
                TokenKind::RightBrace,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn handles_conditional_compile_directives() {
        assert_eq!(
            kinds(
                r#"
                @set(flag = 1)
                @if(flag && version >= 0x02040009)
                    var kept = 1;
                @endif
                @if(0)
                    var skipped = 1;
                @endif
                "#
            ),
            vec![
                TokenKind::KwVar,
                TokenKind::Ident("kept".to_string()),
                TokenKind::Assign,
                TokenKind::Integer(1),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn skips_regexp_literals_in_disabled_conditional_compile_blocks() {
        assert_eq!(
            kinds(
                r#"
                @if(0)
                    var skipped = /@endif/;
                @endif
                var kept = 1;
                "#
            ),
            vec![
                TokenKind::KwVar,
                TokenKind::Ident("kept".to_string()),
                TokenKind::Assign,
                TokenKind::Integer(1),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn skips_krkrz_regexp_shape_in_disabled_conditional_compile_blocks() {
        assert_eq!(
            kinds(
                r#"
                @if(0)
                /foo
                @endif
                /
                @endif
                var kept = 1;
                "#
            ),
            vec![
                TokenKind::KwVar,
                TokenKind::Ident("kept".to_string()),
                TokenKind::Assign,
                TokenKind::Integer(1),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn rejects_unbalanced_conditional_compile_directives() {
        let err = lex("@if(1)\nvar kept = 1;").expect_err("expected unterminated @if");
        assert_eq!(err.message, "unterminated preprocessor block");

        let err = lex("@endif").expect_err("expected stray @endif rejection");
        assert_eq!(err.message, "unexpected preprocessor endif");
    }

    #[test]
    fn preprocessor_numbers_wrap_to_32_bits() {
        let source = format!(
            r#"
                @set(mask = 0xffffffff)
                @set(hex_real = 0x1p2)
                @set(dec_real = 1e +2)
                @set(big_real = 0x{}p-1596)
                @if(mask == -1)
                    var kept = 1;
                @endif
                @if(hex_real == 4 && dec_real == 100)
                    var real_numbers = 1;
                @endif
                @if(big_real < 2147483647)
                    var big_real_number = 1;
                @endif
                @if(0x80000000 < 0)
                    var sign = 1;
                @endif
                @if(0x100000000)
                    var skipped = 1;
                @endif
                "#,
            "f".repeat(400)
        );
        assert_eq!(
            kinds(&source),
            vec![
                TokenKind::KwVar,
                TokenKind::Ident("kept".to_string()),
                TokenKind::Assign,
                TokenKind::Integer(1),
                TokenKind::Semicolon,
                TokenKind::KwVar,
                TokenKind::Ident("real_numbers".to_string()),
                TokenKind::Assign,
                TokenKind::Integer(1),
                TokenKind::Semicolon,
                TokenKind::KwVar,
                TokenKind::Ident("big_real_number".to_string()),
                TokenKind::Assign,
                TokenKind::Integer(1),
                TokenKind::Semicolon,
                TokenKind::KwVar,
                TokenKind::Ident("sign".to_string()),
                TokenKind::Assign,
                TokenKind::Integer(1),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn preprocessor_assignment_binds_tighter_than_logical_and_bitwise() {
        assert_eq!(
            kinds(
                r#"
                @set(or_flag = 0 || 1)
                @if(or_flag)
                    var skipped_or = 1;
                @endif
                @set(and_flag = 1 && 0)
                @if(and_flag)
                    var kept_and = 1;
                @endif
                @set(bit_flag = 0 | 1)
                @if(bit_flag)
                    var skipped_bit = 1;
                @endif
                "#
            ),
            vec![
                TokenKind::KwVar,
                TokenKind::Ident("kept_and".to_string()),
                TokenKind::Assign,
                TokenKind::Integer(1),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn preprocessor_logical_operators_evaluate_operands() {
        assert_eq!(
            kinds(
                r#"
                @set(x = 0)
                @if(0 && (x = 1))
                    var skipped = 1;
                @endif
                @if(x)
                    var kept = 1;
                @endif
                "#
            ),
            vec![
                TokenKind::KwVar,
                TokenKind::Ident("kept".to_string()),
                TokenKind::Assign,
                TokenKind::Integer(1),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );

        assert_eq!(
            kinds(
                r#"
                @set(x = 0)
                @if(1 || (x = 1))
                @endif
                @if(x)
                    var kept = 1;
                @endif
                "#
            ),
            vec![
                TokenKind::KwVar,
                TokenKind::Ident("kept".to_string()),
                TokenKind::Assign,
                TokenKind::Integer(1),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }
}
