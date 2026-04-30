use crate::error::Span;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BindingId(pub usize);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ScopeId(pub usize);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ident {
    pub name: String,
    pub binding: Option<BindingId>,
}

impl Ident {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            binding: None,
        }
    }

    pub fn bind(&mut self, binding: BindingId) {
        self.binding = Some(binding);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    pub statements: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StmtKind {
    Empty,
    Block(Vec<Stmt>),
    Expr(Expr),
    Var {
        kind: VarKind,
        declarations: Vec<VarDecl>,
    },
    FunctionDecl(FunctionDecl),
    ClassDecl(ClassDecl),
    PropertyDecl(PropertyDecl),
    Return(Option<Expr>),
    Throw(Expr),
    If {
        condition: Expr,
        then_branch: Box<Stmt>,
        else_branch: Option<Box<Stmt>>,
    },
    While {
        condition: Expr,
        body: Box<Stmt>,
    },
    DoWhile {
        body: Box<Stmt>,
        condition: Expr,
    },
    For {
        init: Option<ForInit>,
        condition: Option<Expr>,
        step: Option<Expr>,
        body: Box<Stmt>,
    },
    With {
        object: Expr,
        body: Box<Stmt>,
    },
    Break,
    Continue,
    Try {
        body: Box<Stmt>,
        catch: Option<CatchClause>,
    },
    Switch {
        discriminant: Expr,
        cases: Vec<SwitchCase>,
    },
    Case {
        test: Option<Expr>,
    },
    Debugger,
}

impl Stmt {
    pub fn new(kind: StmtKind, span: Span) -> Self {
        Self { kind, span }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VarKind {
    Var,
    Const,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VarDecl {
    pub name: Ident,
    pub ty: Option<String>,
    pub initializer: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ForInit {
    Var {
        kind: VarKind,
        declarations: Vec<VarDecl>,
    },
    Expr(Expr),
}

#[derive(Clone, Debug, PartialEq)]
pub struct FunctionDecl {
    pub name: Option<Ident>,
    pub params: Vec<ParamDecl>,
    pub return_type: Option<String>,
    pub body: Box<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParamDecl {
    pub name: Option<Ident>,
    pub ty: Option<String>,
    pub default: Option<Expr>,
    pub collapse: bool,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClassDecl {
    pub name: Ident,
    pub extends: Vec<Expr>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PropertyDecl {
    pub name: Ident,
    pub getter: Option<FunctionDecl>,
    pub setter: Option<FunctionDecl>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CatchClause {
    pub binding: Option<Ident>,
    pub body: Box<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SwitchCase {
    pub test: Option<Expr>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Self { kind, span }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    Void,
    Null,
    Bool(bool),
    Integer(i64),
    Real(f64),
    String(String),
    Octet(Vec<u8>),
    RegExp {
        pattern: String,
        flags: String,
    },
    Identifier(Ident),
    This,
    Super,
    Global,
    Nan,
    Infinity,
    Array(Vec<ArrayElement>),
    ConstArray(Vec<ArrayElement>),
    Dictionary(Vec<DictionaryEntry>),
    ConstDictionary(Vec<DictionaryEntry>),
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Assignment {
        op: AssignOp,
        target: Box<Expr>,
        value: Box<Expr>,
    },
    Conditional {
        condition: Box<Expr>,
        then_expr: Box<Expr>,
        else_expr: Box<Expr>,
    },
    Member {
        object: Box<Expr>,
        property: String,
    },
    WithMember {
        property: String,
    },
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<CallArg>,
    },
    New {
        callee: Box<Expr>,
        args: Vec<CallArg>,
    },
    Function(Box<FunctionDecl>),
    Postfix {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Comma(Vec<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ArrayElement {
    Value(Expr),
    Hole,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DictionaryEntry {
    pub key: Expr,
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CallArg {
    Value(Expr),
    Expand(Option<Expr>),
    Omitted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnaryOp {
    Plus,
    Minus,
    LogicalNot,
    BitNot,
    Delete,
    TypeOf,
    IsValid,
    Invalidate,
    IgnoreProp,
    PropAccess,
    AsInt,
    AsReal,
    AsString,
    Sharp,
    Dollar,
    Eval,
    Increment,
    Decrement,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinaryOp {
    LogicalOr,
    LogicalAnd,
    BitOr,
    BitXor,
    BitAnd,
    Equal,
    NotEqual,
    DiscernEqual,
    DiscernNotEqual,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    InstanceOf,
    InContextOf,
    ShiftArithmeticRight,
    ShiftLeft,
    ShiftLogicalRight,
    Add,
    Sub,
    Mod,
    Div,
    Idiv,
    Mul,
    If,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssignOp {
    Assign,
    Swap,
    BitAnd,
    BitOr,
    BitXor,
    Sub,
    Add,
    Mod,
    Div,
    Idiv,
    Mul,
    LogicalOr,
    LogicalAnd,
    ShiftLogicalRight,
    ShiftLeft,
    ShiftArithmeticRight,
}
