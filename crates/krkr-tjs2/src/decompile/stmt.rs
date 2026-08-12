//! Statement-level bytecode scanner.
//!
//! Decompiles a single code object body into `syntax::Stmt`s. The scanner is
//! a forward dataflow pass over the instruction stream:
//!
//! - every instruction updates a `register -> Expr` map (values only live
//!   forward, because the TJS2 compilers allocate temporaries with stack
//!   discipline and never clear them);
//! - the VM condition flag is tracked as a `Cond` value;
//! - statement boundaries come from `source_positions` (each slice ends with
//!   one statement built from its last instruction, mid-slice side effects
//!   are joined into the statement as comma expressions);
//! - instructions that are not yet covered by a pattern (control flow in
//!   this milestone) emit an `// <unhandled: ...>` comment marker instead of
//!   failing.

use std::collections::{BTreeMap, BTreeSet};

use crate::bytecode::{
    BinaryForm, BytecodeContextType, BytecodeFile, CallArgs, CodeObject, Instruction, binary_form,
};
use crate::error::Span;
use crate::frontend::syntax::{
    self, AssignOp, BinaryOp, Expr, ExprKind, Ident, Stmt, StmtKind, UnaryOp, VarDecl, VarKind,
};

use super::expr;
use super::naming::Names;

pub(crate) struct BodyOutput {
    pub statements: Vec<Stmt>,
    pub unhandled: usize,
}

/// Builds a stable marker identifier for an unhandled fragment. The emitter
/// post-process replaces `marker;` lines with `// <unhandled: reason>`
/// comments; the sanitized reason is part of the identifier so the comment
/// can name the unmatched pattern.
pub(crate) fn unhandled_marker(reason: &str) -> String {
    super::count_unhandled_fragment();
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in reason.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let sanitized: String = reason
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    format!("__krkr_decomp_unhandled_{hash:016x}_{sanitized}")
}

pub(crate) fn decompile_body(file: &BytecodeFile, object: &CodeObject) -> BodyOutput {
    super::control::decompile_body(file, object)
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Cond {
    Truthy { reg: i16, inv: bool },
    Compare { op: BinaryOp, lhs: i16, rhs: i16, inv: bool },
}

enum Effect {
    /// Defines a register; `side` is an optional heap side effect that must
    /// be emitted before the value is used (compound member writes).
    Def {
        reg: i16,
        expr: Expr,
        side: Option<Expr>,
    },
    /// Sets the condition flag.
    Flag(Cond),
    /// Pure side effect with a discarded result.
    Side(Expr),
    /// A complete statement.
    Stmt(Stmt),
    /// Not covered by a pattern yet: emit a comment marker.
    Unhandled(String),
    None,
}

enum SideEffect {
    Expr(Expr),
    Stmt(Stmt),
    Comment(String),
}

pub(crate) struct Scanner<'f> {
    file: &'f BytecodeFile,
    object: &'f CodeObject,
    names: Names,
    regs: BTreeMap<i16, Expr>,
    flag: Option<Cond>,
    declared: BTreeSet<i16>,
    /// Positive registers whose value was already materialized under its
    /// `tN` name (later redefinitions reassign instead of re-declaring).
    materialized: BTreeSet<i16>,
    out: Vec<Stmt>,
    unhandled: usize,
    at_top_level: bool,
    pending_srv: Option<i16>,
    /// When false (condition blocks), impure temporaries are not
    /// materialized: the condition expression is evaluated exactly once by
    /// the construct that consumes it.
    pub(crate) materialize: bool,
    /// Condition-block scan: the final flag test is the condition the
    /// consuming construct owns, so it is not emitted as a statement (which
    /// would duplicate its member reads/calls as side effects).
    cond_scan: bool,
}

impl<'f> Scanner<'f> {
    pub(crate) fn new(file: &'f BytecodeFile, object: &'f CodeObject) -> Self {
        Self {
            file,
            object,
            names: Names::new(object),
            regs: BTreeMap::new(),
            flag: None,
            declared: BTreeSet::new(),
            materialized: BTreeSet::new(),
            out: Vec::new(),
            unhandled: 0,
            at_top_level: object.context_type == BytecodeContextType::TopLevel,
            pending_srv: None,
            materialize: true,
            cond_scan: false,
        }
    }

    /// Linearly scans an instruction range (one or more basic blocks in
    /// execution order), emitting statements into `self.out`.
    pub(crate) fn scan_linear(&mut self, instructions: &[Instruction]) {
        if instructions.is_empty() {
            return;
        }

        // Statement slices from source positions: each position starts a new
        // slice; instructions beyond the last position form the final slice.
        let mut boundaries: Vec<usize> = self
            .object
            .source_positions
            .iter()
            .map(|position| position.code_pos as usize)
            .filter(|code_pos| *code_pos > 0)
            .collect();
        boundaries.sort_unstable();
        boundaries.dedup();

        let first_offset = instructions[0].offset;
        let last_offset = instructions[instructions.len() - 1].offset;
        let mut slice_starts: Vec<usize> = vec![0];
        let mut index = 0;
        for boundary in boundaries {
            if boundary <= first_offset || boundary > last_offset {
                continue;
            }
            while index < instructions.len() && instructions[index].offset < boundary {
                index += 1;
            }
            if index > *slice_starts.last().unwrap_or(&0) {
                slice_starts.push(index);
            }
        }
        let mut ends = slice_starts[1..].to_vec();
        ends.push(instructions.len());

        for (start, end) in slice_starts.iter().zip(ends) {
            self.scan_slice(&instructions[*start..end]);
        }
    }

    /// Takes the emitted statements.
    pub(crate) fn take_out(&mut self) -> Vec<Stmt> {
        std::mem::take(&mut self.out)
    }

    /// Restores the statement output (undoing a speculative scan).
    pub(crate) fn restore_out(&mut self, out: Vec<Stmt>) {
        self.out = out;
    }

    /// Whether the last scan produced no statements.
    pub(crate) fn out_is_empty(&self) -> bool {
        self.out.is_empty()
    }

    pub(crate) fn unhandled_count(&self) -> usize {
        self.unhandled
    }

    /// Extracts the current condition expression (the VM flag as a boolean
    /// expression), clearing the flag.
    pub(crate) fn take_condition(&mut self) -> Option<Expr> {
        self.flag.take().map(|cond| self.cond_expr(cond, false))
    }

    /// Extracts the raw flag condition without clearing it.
    pub(crate) fn take_raw_condition(&mut self) -> Option<Cond> {
        self.flag.take()
    }

    /// Overrides the value expression of a register (used by condition
    /// fusion).
    pub(crate) fn set_reg(&mut self, reg: i16, expr: Expr) {
        self.regs.insert(reg, expr);
    }

    /// Whether the instruction list of a candidate condition block contains
    /// only flag computation (no statement roots) — i.e. it is a pure
    /// condition block safe to fuse.
    pub(crate) fn is_pure_condition_block(&self, instructions: &[Instruction]) -> bool {
        instructions.iter().all(|inst| {
            let opcode = inst.opcode;
            if !matches!(
                opcode,
                1 | 2 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12 | 13 | 14
                    | 26..=81 | 82 | 83 | 88 | 89..=98 | 99..=102 | 103 | 107 | 110 | 112 | 115 | 124
            ) {
                return false;
            }
            // Flag-testing instructions only read their operand; everything
            // else defines `operands[0]` and must target a temporary (or the
            // void register) so no statement is produced.
            matches!(opcode, 5 | 6 | 7 | 8 | 9 | 10)
                || inst.operands.first().is_none_or(|operand| *operand >= 0)
        })
    }

    /// Scans a condition block without emitting statements and returns the
    /// resulting condition expression (or None when the block computes no
    /// flag), together with any statements the scan produced (side effects
    /// interleaved with the condition evaluation — the caller must emit them
    /// before the construct).
    pub(crate) fn scan_condition_block(&mut self, instructions: &[Instruction]) -> (Option<Expr>, Vec<Stmt>) {
        let saved = self.materialize;
        self.materialize = false;
        let saved_cond_scan = self.cond_scan;
        self.cond_scan = true;
        let saved_out = std::mem::take(&mut self.out);
        self.scan_linear(instructions);
        let mut side = std::mem::replace(&mut self.out, saved_out);
        // Pure value statements (comparison materializations etc.) are not
        // observable side effects; only keep impure statements.
        side.retain(|stmt| match &stmt.kind {
            StmtKind::Expr(expr) => is_impure(expr),
            _ => true,
        });
        self.materialize = saved;
        self.cond_scan = saved_cond_scan;
        (self.take_condition(), side)
    }

    /// Like [`Self::scan_condition_block`] but returns the raw flag
    /// condition instead of the boolean expression.
    pub(crate) fn scan_condition_block_raw(
        &mut self,
        instructions: &[Instruction],
    ) -> (Option<Cond>, Vec<Stmt>) {
        let saved = self.materialize;
        self.materialize = false;
        let saved_cond_scan = self.cond_scan;
        self.cond_scan = true;
        let saved_out = std::mem::take(&mut self.out);
        self.scan_linear(instructions);
        let mut side = std::mem::replace(&mut self.out, saved_out);
        side.retain(|stmt| match &stmt.kind {
            StmtKind::Expr(expr) => is_impure(expr),
            _ => true,
        });
        self.materialize = saved;
        self.cond_scan = saved_cond_scan;
        (self.take_raw_condition(), side)
    }

    fn scan_slice(&mut self, slice: &[Instruction]) {
        if slice.is_empty() {
            return;
        }
        let mut side = Vec::new();
        for inst in &slice[..slice.len() - 1] {
            let effect = self.effect(inst);
            match effect {
                Effect::Def { reg, expr, side: extra } => {
                    if let Some(extra) = extra {
                        side.push(SideEffect::Expr(extra));
                    }
                    // A local/this/global write mid-slice is a real
                    // statement: emit it in place so it is never lost when
                    // the slice's tail defines a different register.
                    if let Some(stmt) = self.def_statement(reg, &expr) {
                        side.push(SideEffect::Stmt(stmt));
                    }
                    self.define(reg, expr, &mut side);
                }
                Effect::Flag(cond) => {
                    self.flag = Some(cond);
                    if !self.cond_scan {
                        side.push(SideEffect::Stmt(Stmt::new(
                            StmtKind::Expr(self.cond_expr(cond, false)),
                            Span::empty(0),
                        )));
                    }
                }
                Effect::Side(expr) => side.push(SideEffect::Expr(expr)),
                Effect::Stmt(stmt) => side.push(SideEffect::Stmt(stmt)),
                Effect::Unhandled(reason) => {
                    self.unhandled += 1;
                    side.push(SideEffect::Comment(reason));
                    // Control-flow instructions end the linear slice; the
                    // remaining instructions belong to other blocks.
                    if matches!(inst.opcode, 15..=17) {
                        return;
                    }
                }
                Effect::None => {}
            }
        }
        let last = &slice[slice.len() - 1];
        self.finalize(last, side);
    }

    /// The statement a register definition renders when it is a statement
    /// root (local writes become `var`/assignments, `this`/void writes are
    /// plain expression statements); pure temporary definitions render
    /// nothing.
    fn def_statement(&mut self, reg: i16, expr: &Expr) -> Option<Stmt> {
        match reg {
            r if r < 0 && r != -2 => Some(self.assign_stmt(r, expr.clone())),
            0 | -2 => Some(Stmt::new(StmtKind::Expr(expr.clone()), Span::empty(0))),
            _ => None,
        }
    }

    /// Records a register definition. Impure temporary values (calls, member
    /// reads, assignments, ...) are materialized into a named temporary so
    /// re-evaluating the expression never duplicates side effects.
    fn define(&mut self, reg: i16, expr: Expr, side: &mut Vec<SideEffect>) {
        if self.materialize && reg > 0 && is_impure(&expr) {
            let name = format!("t{reg}");
            // The first materialization declares the temporary; later
            // redefinitions of the same register reassign it instead of
            // repeating `var tN = ...`.
            let stmt = if self.materialized.insert(reg) {
                Stmt::new(
                    StmtKind::Var {
                        kind: VarKind::Var,
                        declarations: vec![VarDecl {
                            name: Ident::new(name.clone()),
                            ty: None,
                            initializer: Some(expr),
                            span: Span::empty(0),
                        }],
                    },
                    Span::empty(0),
                )
            } else {
                Stmt::new(
                    StmtKind::Expr(Expr::new(
                        ExprKind::Assignment {
                            op: AssignOp::Assign,
                            target: Box::new(Expr::new(
                                ExprKind::Identifier(Ident::new(name.clone())),
                                Span::empty(0),
                            )),
                            value: Box::new(expr),
                        },
                        Span::empty(0),
                    )),
                    Span::empty(0),
                )
            };
            side.push(SideEffect::Stmt(self.top_level_stmt(stmt)));
            self.regs.insert(
                reg,
                Expr::new(
                    ExprKind::Identifier(Ident::new(name)),
                    Span::empty(0),
                ),
            );
        } else {
            self.regs.insert(reg, expr);
        }
    }

    fn finalize(&mut self, last: &Instruction, mut side: Vec<SideEffect>) {
        match self.effect(last) {
            Effect::Def { reg, expr, side: extra } => {
                if let Some(extra) = extra {
                    side.push(SideEffect::Expr(extra));
                }
                self.define(reg, expr.clone(), &mut side);
                match reg {
                    // Temporary definitions are pure values: they only feed
                    // later statements (or are discarded); mid-slice
                    // statements already sit in `side`.
                    r if r > 0 => self.push_side_only(side),
                    0 | -2 => {
                        let stmt = Stmt::new(StmtKind::Expr(expr), Span::empty(0));
                        self.push_combined(side, stmt);
                    }
                    r => {
                        let stmt = self.assign_stmt(r, expr);
                        self.push_combined(side, stmt);
                    }
                }
            }
            Effect::Flag(cond) => {
                self.flag = Some(cond);
                if self.cond_scan {
                    // The condition expression belongs to the construct that
                    // scans this block; only its side effects stay observable.
                    self.push_side_only(side);
                } else {
                    let stmt = Stmt::new(
                        StmtKind::Expr(self.cond_expr(cond, false)),
                        Span::empty(0),
                    );
                    self.push_combined(side, stmt);
                }
            }
            Effect::Side(expr) => {
                let stmt = Stmt::new(StmtKind::Expr(expr), Span::empty(0));
                self.push_combined(side, stmt);
            }
            Effect::Stmt(stmt) => self.push_combined(side, stmt),
            Effect::Unhandled(reason) => {
                self.unhandled += 1;
                side.push(SideEffect::Comment(reason));
                self.push_side_only(side);
            }
            Effect::None => self.push_side_only(side),
        }
    }

    fn push_side_only(&mut self, side: Vec<SideEffect>) {
        for effect in side {
            match effect {
                SideEffect::Expr(expr) => {
                    let stmt = Stmt::new(StmtKind::Expr(expr), Span::empty(0));
                    self.out.push(self.top_level_stmt(stmt));
                }
                SideEffect::Stmt(stmt) => self.out.push(stmt),
                SideEffect::Comment(reason) => {
                    self.unhandled += 1;
                    self.out.push(self.marker_stmt(&reason));
                }
            }
        }
    }

    /// Emits a statement, folding pending side-effect expressions into a
    /// comma expression when the statement is an expression statement.
    fn push_combined(&mut self, side: Vec<SideEffect>, stmt: Stmt) {
        let mut exprs = Vec::new();
        let mut before = Vec::new();
        for effect in side {
            match effect {
                SideEffect::Expr(expr) => exprs.push(expr),
                SideEffect::Stmt(stmt) => before.push(stmt),
                SideEffect::Comment(reason) => {
                    self.unhandled += 1;
                    before.push(self.marker_stmt(&reason));
                }
            }
        }
        for stmt in before {
            self.out.push(stmt);
        }
        if exprs.is_empty() {
            self.out.push(self.top_level_stmt(stmt));
        } else if let StmtKind::Expr(final_expr) = &stmt.kind {
            // Join a few mid-statement side effects into one comma statement;
            // long runs (whole blocks without source-position info) read
            // better as separate statements and are semantically identical.
            if exprs.len() < 3 {
                let mut all = exprs;
                all.push(final_expr.clone());
                let stmt = Stmt::new(
                    StmtKind::Expr(Expr::new(ExprKind::Comma(all), Span::empty(0))),
                    Span::empty(0),
                );
                self.out.push(self.top_level_stmt(stmt));
            } else {
                for expr in exprs {
                    let stmt = Stmt::new(StmtKind::Expr(expr), Span::empty(0));
                    self.out.push(self.top_level_stmt(stmt));
                }
                self.out.push(self.top_level_stmt(stmt));
            }
        } else {
            for expr in exprs {
                let stmt = Stmt::new(StmtKind::Expr(expr), Span::empty(0));
                self.out.push(self.top_level_stmt(stmt));
            }
            self.out.push(self.top_level_stmt(stmt));
        }
    }

    fn assign_stmt(&mut self, reg: i16, expr: Expr) -> Stmt {
        let name = self.names.name(reg);
        let is_local = reg <= -3 && self.object.func_decl_arg_count as i16 + 3 <= -reg;
        if is_local && !self.declared.contains(&reg) {
            self.declared.insert(reg);
            return Stmt::new(
                StmtKind::Var {
                    kind: VarKind::Var,
                    declarations: vec![VarDecl {
                        name: Ident::new(name),
                        ty: None,
                        initializer: Some(expr),
                        span: Span::empty(0),
                    }],
                },
                Span::empty(0),
            );
        }
        let target = if reg == -1 {
            Expr::new(ExprKind::This, Span::empty(0))
        } else {
            self.names.ident_expr(reg)
        };
        Stmt::new(
            StmtKind::Expr(Expr::new(
                ExprKind::Assignment {
                    op: AssignOp::Assign,
                    target: Box::new(target),
                    value: Box::new(expr),
                },
                Span::empty(0),
            )),
            Span::empty(0),
        )
    }

    fn marker_stmt(&self, reason: &str) -> Stmt {
        Stmt::new(
            StmtKind::Expr(Expr::new(
                ExprKind::Identifier(Ident::new(unhandled_marker(reason))),
                Span::empty(0),
            )),
            Span::empty(0),
        )
    }

    pub(crate) fn reg_expr(&self, reg: i16) -> Expr {
        // Negative registers are the real variables (this / args / locals):
        // they always render as their identifier, never as an inlined
        // expression, so re-evaluation can never duplicate side effects.
        match reg {
            0 => Expr::new(ExprKind::Void, Span::empty(0)),
            -1 | -2 => Expr::new(ExprKind::This, Span::empty(0)),
            r if r < 0 => self.names.ident_expr(r),
            _ => match self.regs.get(&reg) {
                Some(expr) => expr.clone(),
                None => self.names.ident_expr(reg),
            },
        }
    }

    fn member(&self, object_reg: i16, name: &str) -> Expr {
        if object_reg == -1 || object_reg == -2 {
            expr::member_expr(&self.names, object_reg, name, self.at_top_level)
        } else {
            Expr::new(
                ExprKind::Member {
                    object: Box::new(self.reg_expr(object_reg)),
                    property: name.to_string(),
                },
                Span::empty(0),
            )
        }
    }

    fn index(&self, object_reg: i16, key_reg: i16) -> Expr {
        Expr::new(
            ExprKind::Index {
                object: Box::new(self.reg_expr(object_reg)),
                index: Box::new(self.reg_expr(key_reg)),
            },
            Span::empty(0),
        )
    }

    fn direct_name(&self, data_index: i16) -> Option<String> {
        let index = usize::try_from(data_index).ok()?;
        let slot = self.object.data_slots.get(index)?;
        match slot.value(self.file) {
            Ok(crate::runtime::Variant::String(value)) => Some(value),
            _ => None,
        }
    }

    fn member_from_data(&self, object_reg: i16, data_index: i16) -> Expr {
        match self.direct_name(data_index) {
            Some(name) if super::naming::is_ident_name(&name) => self.member(object_reg, &name),
            Some(name) => self.index_expr_str(object_reg, &name),
            None => {
                // A non-string member name: render as computed access with a
                // placeholder for the unknown key.
                self.index(object_reg, 0)
            }
        }
    }

    fn index_expr_str(&self, object_reg: i16, name: &str) -> Expr {
        Expr::new(
            ExprKind::Index {
                object: Box::new(self.reg_expr(object_reg)),
                index: Box::new(Expr::new(ExprKind::String(name.to_string()), Span::empty(0))),
            },
            Span::empty(0),
        )
    }

    /// At the top level, plain `name = value` statements that the bytecode
    /// Top-level assignments recompile faithfully as plain assignments (the
    /// frontend auto-creates global members), so the `var` conversion only
    /// applies where the bytecode pattern is unambiguous: kept as a hook for
    /// later polish, currently a no-op for correctness (block-scoped `var`
    /// would change semantics inside branches).
    fn top_level_stmt(&self, stmt: Stmt) -> Stmt {
        let _ = self.at_top_level;
        stmt
    }

    fn call_expr(&mut self, opcode: u8, inst: &Instruction) -> Expr {
        let (callee, args) = match opcode {
            99 => (
                self.reg_expr(inst.operands[1]),
                inst.call_args.as_ref(),
            ),
            100 => (
                self.member_from_data(inst.operands[1], inst.operands[2]),
                inst.call_args.as_ref(),
            ),
            101 => (
                self.index(inst.operands[1], inst.operands[2]),
                inst.call_args.as_ref(),
            ),
            _ => (self.reg_expr(inst.operands[1]), inst.call_args.as_ref()),
        };
        let kind = if opcode == 102 {
            ExprKind::New {
                callee: Box::new(callee),
                args: self.call_args(args),
            }
        } else {
            ExprKind::Call {
                callee: Box::new(callee),
                args: self.call_args(args),
            }
        };
        Expr::new(kind, Span::empty(0))
    }

    fn call_args(&self, args: Option<&CallArgs>) -> Vec<syntax::CallArg> {
        match args {
            Some(CallArgs::Normal(regs)) => regs
                .iter()
                .map(|reg| syntax::CallArg::Value(self.reg_expr(*reg)))
                .collect(),
            Some(CallArgs::OmittedCallerArgs) => vec![syntax::CallArg::Omitted],
            Some(CallArgs::Expanded(args)) => args
                .iter()
                .map(|arg| match arg.arg_type {
                    1 => syntax::CallArg::Expand(Some(self.reg_expr(arg.reg))),
                    2 => syntax::CallArg::Expand(None),
                    _ => syntax::CallArg::Value(self.reg_expr(arg.reg)),
                })
                .collect(),
            None => Vec::new(),
        }
    }

    fn cond_expr(&self, cond: Cond, negate: bool) -> Expr {
        cond_expr(cond, negate, |reg| self.reg_expr(reg))
    }

    /// Interprets one instruction as an effect.
    fn effect(&mut self, inst: &Instruction) -> Effect {
        let opcode = inst.opcode;
        let unhandled = |reason: &str| {
            Effect::Unhandled(format!(
                "{} at offset {} ({reason})",
                inst.mnemonic(),
                inst.offset
            ))
        };
        match opcode {
            0 => Effect::None,
            1 => {
                let dst = inst.operands[0];
                match expr::data_slot_expr(self.file, self.object, inst.operands[1], &self.names) {
                    Ok(expr) => Effect::Def {
                        reg: dst,
                        expr,
                        side: None,
                    },
                    Err(error) => unhandled(&error.message),
                }
            }
            2 => {
                let expr = self.reg_expr(inst.operands[1]);
                Effect::Def {
                    reg: inst.operands[0],
                    expr,
                    side: None,
                }
            }
            3 => {
                let reg = inst.operands[0];
                if reg < 0 {
                    // `var x;` — mark the local as declared.
                    if !self.declared.contains(&reg) {
                        self.declared.insert(reg);
                    }
                    Effect::Stmt(Stmt::new(
                        StmtKind::Var {
                            kind: VarKind::Var,
                            declarations: vec![VarDecl {
                                name: Ident::new(self.names.name(reg)),
                                ty: None,
                                initializer: None,
                                span: Span::empty(0),
                            }],
                        },
                        Span::empty(0),
                    ))
                } else {
                    // Bookkeeping clear of a temporary (class init, function
                    // registration block): values flow from here as void.
                    Effect::Def {
                        reg,
                        expr: Expr::new(ExprKind::Void, Span::empty(0)),
                        side: None,
                    }
                }
            }
            4 => unhandled("register range clear"),
            5 => Effect::Flag(Cond::Truthy {
                reg: inst.operands[0],
                inv: false,
            }),
            6 => Effect::Flag(Cond::Truthy {
                reg: inst.operands[0],
                inv: true,
            }),
            7 => Effect::Flag(Cond::Compare {
                op: BinaryOp::Equal,
                lhs: inst.operands[0],
                rhs: inst.operands[1],
                inv: false,
            }),
            8 => Effect::Flag(Cond::Compare {
                op: BinaryOp::DiscernEqual,
                lhs: inst.operands[0],
                rhs: inst.operands[1],
                inv: false,
            }),
            9 => Effect::Flag(Cond::Compare {
                op: BinaryOp::Less,
                lhs: inst.operands[0],
                rhs: inst.operands[1],
                inv: false,
            }),
            10 => Effect::Flag(Cond::Compare {
                op: BinaryOp::Greater,
                lhs: inst.operands[0],
                rhs: inst.operands[1],
                inv: false,
            }),
            11 => {
                let expr = self.flag.map(|cond| self.cond_expr(cond, false)).unwrap_or_else(|| {
                    Expr::new(ExprKind::Bool(false), Span::empty(0))
                });
                Effect::Def {
                    reg: inst.operands[0],
                    expr,
                    side: None,
                }
            }
            12 => {
                let expr = self.flag.map(|cond| self.cond_expr(cond, true)).unwrap_or_else(|| {
                    Expr::new(ExprKind::Bool(false), Span::empty(0))
                });
                Effect::Def {
                    reg: inst.operands[0],
                    expr,
                    side: None,
                }
            }
            13 => {
                let reg = inst.operands[0];
                let expr = Expr::new(
                    ExprKind::Unary {
                        op: UnaryOp::LogicalNot,
                        expr: Box::new(self.reg_expr(reg)),
                    },
                    Span::empty(0),
                );
                Effect::Def {
                    reg,
                    expr,
                    side: None,
                }
            }
            14 => {
                self.flag = self.flag.map(invert_cond);
                Effect::None
            }
            15..=17 => unhandled("control flow"),
            120 | 121 => Effect::None,
            18 | 22 => {
                let reg = inst.operands[0];
                let old = self.reg_expr(reg);
                let op = if opcode == 18 { BinaryOp::Add } else { BinaryOp::Sub };
                // Fold literal increments so `t1[0 + 1]` prints as `t1[1]`.
                let expr = match (&old.kind, op) {
                    (ExprKind::Integer(value), BinaryOp::Add) => {
                        Expr::new(ExprKind::Integer(value + 1), Span::empty(0))
                    }
                    (ExprKind::Integer(value), BinaryOp::Sub) => {
                        Expr::new(ExprKind::Integer(value - 1), Span::empty(0))
                    }
                    _ => Expr::new(
                        ExprKind::Binary {
                            op,
                            lhs: Box::new(old),
                            rhs: Box::new(Expr::new(ExprKind::Integer(1), Span::empty(0))),
                        },
                        Span::empty(0),
                    ),
                };
                Effect::Def {
                    reg,
                    expr,
                    side: None,
                }
            }
            19 | 20 | 23 | 24 => {
                let dst = inst.operands[0];
                let inc = matches!(opcode, 19 | 20);
                let target = if matches!(opcode, 19 | 23) {
                    self.member_from_data(inst.operands[1], inst.operands[2])
                } else {
                    self.index(inst.operands[1], inst.operands[2])
                };
                let one = Expr::new(ExprKind::Integer(1), Span::empty(0));
                let compound = Expr::new(
                    ExprKind::Assignment {
                        op: if inc { AssignOp::Add } else { AssignOp::Sub },
                        target: Box::new(target.clone()),
                        value: Box::new(one),
                    },
                    Span::empty(0),
                );
                let new_value = Expr::new(
                    ExprKind::Binary {
                        op: if inc { BinaryOp::Add } else { BinaryOp::Sub },
                        lhs: Box::new(target),
                        rhs: Box::new(Expr::new(ExprKind::Integer(1), Span::empty(0))),
                    },
                    Span::empty(0),
                );
                if dst == 0 {
                    Effect::Side(compound)
                } else {
                    Effect::Def {
                        reg: dst,
                        expr: new_value,
                        side: Some(compound),
                    }
                }
            }
            21 | 25 => {
                let dst = inst.operands[0];
                let prop = self.reg_expr(inst.operands[1]);
                let inc = opcode == 21;
                let one = Expr::new(ExprKind::Integer(1), Span::empty(0));
                let compound = Expr::new(
                    ExprKind::Assignment {
                        op: if inc { AssignOp::Add } else { AssignOp::Sub },
                        target: Box::new(Expr::new(
                            ExprKind::Unary {
                                op: UnaryOp::PropAccess,
                                expr: Box::new(prop.clone()),
                            },
                            Span::empty(0),
                        )),
                        value: Box::new(one),
                    },
                    Span::empty(0),
                );
                let new_value = Expr::new(
                    ExprKind::Binary {
                        op: if inc { BinaryOp::Add } else { BinaryOp::Sub },
                        lhs: Box::new(prop),
                        rhs: Box::new(Expr::new(ExprKind::Integer(1), Span::empty(0))),
                    },
                    Span::empty(0),
                );
                if dst == 0 {
                    Effect::Side(compound)
                } else {
                    Effect::Def {
                        reg: dst,
                        expr: new_value,
                        side: Some(compound),
                    }
                }
            }
            26..=81 => self.binary_effect(inst),
            82 => self.unary_effect(inst, UnaryOp::BitNot),
            83 => self.unary_effect(inst, UnaryOp::TypeOf),
            84 | 85 => {
                let dst = inst.operands[0];
                let target = if opcode == 84 {
                    self.member_from_data(inst.operands[1], inst.operands[2])
                } else {
                    self.index(inst.operands[1], inst.operands[2])
                };
                Effect::Def {
                    reg: dst,
                    expr: Expr::new(
                        ExprKind::Unary {
                            op: UnaryOp::TypeOf,
                            expr: Box::new(target),
                        },
                        Span::empty(0),
                    ),
                    side: None,
                }
            }
            86 => {
                let reg = inst.operands[0];
                let expr = Expr::new(
                    ExprKind::Postfix {
                        op: UnaryOp::Eval,
                        expr: Box::new(self.reg_expr(reg)),
                    },
                    Span::empty(0),
                );
                Effect::Def {
                    reg,
                    expr,
                    side: None,
                }
            }
            87 => {
                let reg = inst.operands[0];
                Effect::Side(Expr::new(
                    ExprKind::Postfix {
                        op: UnaryOp::Eval,
                        expr: Box::new(self.reg_expr(reg)),
                    },
                    Span::empty(0),
                ))
            }
            88 => {
                let reg = inst.operands[0];
                let expr = Expr::new(
                    ExprKind::Binary {
                        op: BinaryOp::InstanceOf,
                        lhs: Box::new(self.reg_expr(reg)),
                        rhs: Box::new(self.reg_expr(inst.operands[1])),
                    },
                    Span::empty(0),
                );
                Effect::Def {
                    reg,
                    expr,
                    side: None,
                }
            }
            89 => self.unary_effect(inst, UnaryOp::Sharp),
            90 => self.unary_effect(inst, UnaryOp::Dollar),
            91 => self.unary_effect(inst, UnaryOp::Plus),
            92 => self.unary_effect(inst, UnaryOp::Minus),
            93 => self.unary_effect(inst, UnaryOp::Invalidate),
            94 => self.unary_effect(inst, UnaryOp::IsValid),
            95 => self.unary_effect(inst, UnaryOp::AsInt),
            96 => self.unary_effect(inst, UnaryOp::AsReal),
            97 => self.unary_effect(inst, UnaryOp::AsString),
            98 => unhandled("octet conversion"),
            99..=102 => {
                let dst = inst.operands[0];
                let call = self.call_expr(opcode, inst);
                if dst == 0 {
                    Effect::Side(call)
                } else {
                    Effect::Def {
                        reg: dst,
                        expr: call,
                        side: None,
                    }
                }
            }
            103 | 110 => {
                let dst = inst.operands[0];
                let target = self.member_from_data(inst.operands[1], inst.operands[2]);
                Effect::Def {
                    reg: dst,
                    expr: target,
                    side: None,
                }
            }
            104 | 105 | 111 => {
                let target = self.member_from_data(inst.operands[0], inst.operands[1]);
                let value = self.reg_expr(inst.operands[2]);
                Effect::Side(Expr::new(
                    ExprKind::Assignment {
                        op: AssignOp::Assign,
                        target: Box::new(target),
                        value: Box::new(value),
                    },
                    Span::empty(0),
                ))
            }
            106 => unhandled("hidden member write"),
            107 | 112 => {
                let dst = inst.operands[0];
                let target = self.index(inst.operands[1], inst.operands[2]);
                Effect::Def {
                    reg: dst,
                    expr: target,
                    side: None,
                }
            }
            108 | 109 | 113 => {
                let target = self.index(inst.operands[0], inst.operands[1]);
                let value = self.reg_expr(inst.operands[2]);
                Effect::Side(Expr::new(
                    ExprKind::Assignment {
                        op: AssignOp::Assign,
                        target: Box::new(target),
                        value: Box::new(value),
                    },
                    Span::empty(0),
                ))
            }
            114 => {
                let target = Expr::new(
                    ExprKind::Unary {
                        op: UnaryOp::PropAccess,
                        expr: Box::new(self.reg_expr(inst.operands[0])),
                    },
                    Span::empty(0),
                );
                let value = self.reg_expr(inst.operands[1]);
                Effect::Side(Expr::new(
                    ExprKind::Assignment {
                        op: AssignOp::Assign,
                        target: Box::new(target),
                        value: Box::new(value),
                    },
                    Span::empty(0),
                ))
            }
            115 => {
                let dst = inst.operands[0];
                Effect::Def {
                    reg: dst,
                    expr: Expr::new(
                        ExprKind::Unary {
                            op: UnaryOp::PropAccess,
                            expr: Box::new(self.reg_expr(inst.operands[1])),
                        },
                        Span::empty(0),
                    ),
                    side: None,
                }
            }
            116 | 117 => {
                let target = if opcode == 116 {
                    self.member_from_data(inst.operands[1], inst.operands[2])
                } else {
                    self.index(inst.operands[1], inst.operands[2])
                };
                Effect::Side(Expr::new(
                    ExprKind::Unary {
                        op: UnaryOp::Delete,
                        expr: Box::new(target),
                    },
                    Span::empty(0),
                ))
            }
            118 => {
                self.pending_srv = Some(inst.operands[0]);
                Effect::None
            }
            119 => {
                let value = self.pending_srv.take().and_then(|reg| {
                    if reg == 0 {
                        None
                    } else {
                        Some(self.reg_expr(reg))
                    }
                });
                Effect::Stmt(Stmt::new(StmtKind::Return(value), Span::empty(0)))
            }
            122 => {
                let value = self.reg_expr(inst.operands[0]);
                Effect::Stmt(Stmt::new(StmtKind::Throw(value), Span::empty(0)))
            }
            123 => {
                let reg = inst.operands[0];
                let expr = Expr::new(
                    ExprKind::Binary {
                        op: BinaryOp::InContextOf,
                        lhs: Box::new(self.reg_expr(reg)),
                        rhs: Box::new(self.reg_expr(inst.operands[1])),
                    },
                    Span::empty(0),
                );
                Effect::Def {
                    reg,
                    expr,
                    side: None,
                }
            }
            124 => {
                let dst = inst.operands[0];
                Effect::Def {
                    reg: dst,
                    expr: Expr::new(ExprKind::Global, Span::empty(0)),
                    side: None,
                }
            }
            125 | 126 => Effect::None,
            127 => Effect::Stmt(Stmt::new(StmtKind::Debugger, Span::empty(0))),
            _ => unhandled("unknown opcode"),
        }
    }

    fn unary_effect(&mut self, inst: &Instruction, op: UnaryOp) -> Effect {
        let reg = inst.operands[0];
        Effect::Def {
            reg,
            expr: Expr::new(
                ExprKind::Unary {
                    op,
                    expr: Box::new(self.reg_expr(reg)),
                },
                Span::empty(0),
            ),
            side: None,
        }
    }

    fn binary_effect(&mut self, inst: &Instruction) -> Effect {
        let op = binary_op(inst.opcode);
        match binary_form(inst.opcode) {
            BinaryForm::Slot => {
                let dst = inst.operands[0];
                let src = inst.operands[1];
                let expr = Expr::new(
                    ExprKind::Binary {
                        op,
                        lhs: Box::new(self.reg_expr(dst)),
                        rhs: Box::new(self.reg_expr(src)),
                    },
                    Span::empty(0),
                );
                Effect::Def {
                    reg: dst,
                    expr,
                    side: None,
                }
            }
            BinaryForm::DirectProperty => {
                let dst = inst.operands[0];
                let target = self.member_from_data(inst.operands[1], inst.operands[2]);
                self.binary_member_effect(op, dst, target, inst.operands[3])
            }
            BinaryForm::IndirectProperty => {
                let dst = inst.operands[0];
                let target = self.index(inst.operands[1], inst.operands[2]);
                self.binary_member_effect(op, dst, target, inst.operands[3])
            }
            BinaryForm::DefaultProperty => {
                let dst = inst.operands[0];
                let target = Expr::new(
                    ExprKind::Unary {
                        op: UnaryOp::PropAccess,
                        expr: Box::new(self.reg_expr(inst.operands[1])),
                    },
                    Span::empty(0),
                );
                self.binary_member_effect(op, dst, target, inst.operands[2])
            }
        }
    }

    fn binary_member_effect(
        &mut self,
        op: BinaryOp,
        dst: i16,
        target: Expr,
        src_reg: i16,
    ) -> Effect {
        let src = self.reg_expr(src_reg);
        let compound = Expr::new(
            ExprKind::Assignment {
                op: compound_assign_op(op),
                target: Box::new(target.clone()),
                value: Box::new(src.clone()),
            },
            Span::empty(0),
        );
        let value = Expr::new(
            ExprKind::Binary {
                op,
                lhs: Box::new(target),
                rhs: Box::new(src),
            },
            Span::empty(0),
        );
        if dst == 0 {
            Effect::Side(compound)
        } else {
            Effect::Def {
                reg: dst,
                expr: value,
                side: Some(compound),
            }
        }
    }
}

pub(crate) fn cond_expr(
    cond: Cond,
    negate: bool,
    resolve: impl Fn(i16) -> Expr,
) -> Expr {
    match cond {
        Cond::Truthy { reg, inv } => {
            let base = resolve(reg);
            if inv ^ negate {
                Expr::new(
                    ExprKind::Unary {
                        op: UnaryOp::LogicalNot,
                        expr: Box::new(base),
                    },
                    Span::empty(0),
                )
            } else {
                base
            }
        }
        Cond::Compare {
            op,
            lhs,
            rhs,
            inv,
        } => {
            let base = Expr::new(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(resolve(lhs)),
                    rhs: Box::new(resolve(rhs)),
                },
                Span::empty(0),
            );
            if inv ^ negate {
                Expr::new(
                    ExprKind::Unary {
                        op: UnaryOp::LogicalNot,
                        expr: Box::new(base),
                    },
                    Span::empty(0),
                )
            } else {
                base
            }
        }
    }
}

fn invert_cond(cond: Cond) -> Cond {
    match cond {
        Cond::Truthy { reg, inv } => Cond::Truthy { reg, inv: !inv },
        Cond::Compare {
            op,
            lhs,
            rhs,
            inv,
        } => Cond::Compare {
            op,
            lhs,
            rhs,
            inv: !inv,
        },
    }
}

/// Whether re-evaluating `expr` could duplicate an observable side effect.
/// Only structural literals, identifiers, and pure operators count as pure.
fn is_impure(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Void
        | ExprKind::Null
        | ExprKind::Bool(_)
        | ExprKind::Integer(_)
        | ExprKind::Real(_)
        | ExprKind::String(_)
        | ExprKind::Octet(_)
        | ExprKind::RegExp { .. }
        | ExprKind::Identifier(_)
        | ExprKind::This
        | ExprKind::Super
        | ExprKind::Global
        | ExprKind::Nan
        | ExprKind::Infinity => false,
        ExprKind::Array(elements) => elements.iter().all(|element| match element {
            syntax::ArrayElement::Value(expr) => !is_impure(expr),
            syntax::ArrayElement::Hole => true,
        }),
        ExprKind::ConstArray(elements) => elements.iter().all(|element| match element {
            syntax::ArrayElement::Value(expr) => !is_impure(expr),
            syntax::ArrayElement::Hole => true,
        }),
        ExprKind::Dictionary(entries) | ExprKind::ConstDictionary(entries) => entries
            .iter()
            .all(|entry| !is_impure(&entry.key) && !is_impure(&entry.value)),
        ExprKind::Unary { op, expr } => {
            if is_impure(expr) {
                return true;
            }
            !matches!(
                op,
                UnaryOp::Delete
                    | UnaryOp::Invalidate
                    | UnaryOp::PropAccess
                    | UnaryOp::IgnoreProp
                    | UnaryOp::Eval
            )
        }
        ExprKind::Binary { op, lhs, rhs } => {
            // `expr if cond` short-circuits like control flow: still
            // single-evaluation, but keep it impure to stay conservative.
            matches!(op, BinaryOp::If) || is_impure(lhs) || is_impure(rhs)
        }
        ExprKind::Conditional {
            condition,
            then_expr,
            else_expr,
        } => is_impure(condition) || is_impure(then_expr) || is_impure(else_expr),
        ExprKind::Member { .. }
        | ExprKind::WithMember { .. }
        | ExprKind::Index { .. }
        | ExprKind::Call { .. }
        | ExprKind::New { .. }
        | ExprKind::Postfix { .. }
        | ExprKind::Assignment { .. }
        | ExprKind::Comma(_) => true,
        // Creating a closure is pure: the body only runs when called.
        ExprKind::Function(_) => false,
    }
}

fn binary_op(opcode: u8) -> BinaryOp {
    let bases = [
        BinaryOp::LogicalOr,
        BinaryOp::LogicalAnd,
        BinaryOp::BitOr,
        BinaryOp::BitXor,
        BinaryOp::BitAnd,
        BinaryOp::ShiftArithmeticRight,
        BinaryOp::ShiftLeft,
        BinaryOp::ShiftLogicalRight,
        BinaryOp::Add,
        BinaryOp::Sub,
        BinaryOp::Mod,
        BinaryOp::Div,
        BinaryOp::Idiv,
        BinaryOp::Mul,
    ];
    bases[usize::from((opcode - 26) / 4)]
}

fn compound_assign_op(op: BinaryOp) -> AssignOp {
    match op {
        BinaryOp::LogicalOr => AssignOp::LogicalOr,
        BinaryOp::LogicalAnd => AssignOp::LogicalAnd,
        BinaryOp::BitOr => AssignOp::BitOr,
        BinaryOp::BitXor => AssignOp::BitXor,
        BinaryOp::BitAnd => AssignOp::BitAnd,
        BinaryOp::ShiftArithmeticRight => AssignOp::ShiftArithmeticRight,
        BinaryOp::ShiftLeft => AssignOp::ShiftLeft,
        BinaryOp::ShiftLogicalRight => AssignOp::ShiftLogicalRight,
        BinaryOp::Add => AssignOp::Add,
        BinaryOp::Sub => AssignOp::Sub,
        BinaryOp::Mod => AssignOp::Mod,
        BinaryOp::Div => AssignOp::Div,
        BinaryOp::Idiv => AssignOp::Idiv,
        BinaryOp::Mul => AssignOp::Mul,
        _ => AssignOp::Assign,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::compile_source_to_bytecode;
    use crate::frontend::printer::print_statements;

    fn decompile_source(source: &str) -> (String, usize) {
        let file = compile_source_to_bytecode("test.tjs", source).expect("compile");
        let object = &file.objects[file.top_level.expect("top level")];
        let output = decompile_body(&file, object);
        (print_statements(&output.statements), output.unhandled)
    }

    #[test]
    fn decompiles_linear_arithmetic() {
        let (text, unhandled) = decompile_source("return 1 + 2 * 3;");
        assert_eq!(unhandled, 0, "{text}");
        assert!(text.contains("return 1 + 2 * 3;"), "{text}");
    }

    #[test]
    fn decompiles_assignment_and_locals() {
        let (text, unhandled) = decompile_source("var x = 5; x = x + 1; return x;");
        assert_eq!(unhandled, 0, "{text}");
        // Top-level bytecode stores globals as member writes; the
        // decompiler renders them as plain assignments.
        assert!(text.contains("x = 5;"), "{text}");
        assert!(text.contains("x = x + 1;"), "{text}");
        assert!(text.contains("return x;"), "{text}");
    }

    #[test]
    fn decompiles_member_access_and_calls() {
        let (text, unhandled) = decompile_source("return a.b.c(1, 2) + d[e];");
        assert_eq!(unhandled, 0, "{text}");
        // Member reads materialize into named temporaries so side effects
        // cannot be duplicated; the structure is preserved.
        assert!(text.contains("a.b"), "{text}");
        assert!(text.contains("c(1, 2)"), "{text}");
        assert!(text.contains("d[e]"), "{text}");
    }
}
