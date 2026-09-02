//! Structured control-flow decompiler.
//!
//! Builds a basic-block CFG from a code object and reconstructs TJS2
//! structured statements (if/if-else/while/do-while/for/break/continue/try,
//! plus `&&`/`||` condition chains and their value forms) by matching the
//! emission patterns of both the official tjs2 compiler and this project's
//! own compiler (which always pairs a conditional jump with an explicit
//! unconditional `jmp`, puts loop conditions at the top as `jf body`, and
//! routes loop pre-entry through `jmp`-only trampoline blocks).
//!
//! Blocks are scanned in execution order with the linear
//! [`super::stmt::Scanner`]; register/flag state carries across blocks.
//! Constructs without a pattern yet degrade to `// <unhandled: ...>`
//! comments.

use std::collections::{BTreeMap, BTreeSet};

use crate::bytecode::{BytecodeFile, CodeObject, Instruction};
use crate::error::Span;
use crate::frontend::syntax::{self, BinaryOp, Expr, ExprKind, Ident, Stmt, StmtKind};

use super::stmt::{BodyOutput, Cond, Scanner, cond_expr};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Term {
    Fall,
    Jmp(usize),
    Cond { target: usize, jump_if_true: bool },
    Ret,
    Throw,
}

struct Block {
    start: usize,
    end: usize,
    term: Term,
}

/// Iterative dominator computation (Cooper-Harvey-Kennedy style).
fn compute_dominators(blocks: &[Block]) -> Vec<BTreeSet<usize>> {
    let successors = |index: usize| -> Vec<usize> {
        let mut succs = Vec::new();
        match blocks[index].term {
            Term::Fall => succs.push(index + 1),
            Term::Jmp(target) => {
                if target != usize::MAX && target < blocks.len() {
                    succs.push(target);
                }
            }
            Term::Cond { target, .. } => {
                if target != usize::MAX && target < blocks.len() {
                    succs.push(target);
                }
                succs.push(index + 1);
            }
            Term::Ret | Term::Throw => {}
        }
        succs
    };
    let predecessors = |index: usize| -> Vec<usize> {
        let mut preds = Vec::new();
        for candidate in 0..blocks.len() {
            if successors(candidate).contains(&index) {
                preds.push(candidate);
            }
        }
        preds
    };
    let all: BTreeSet<usize> = (0..blocks.len()).collect();
    let mut dom = vec![all.clone(); blocks.len()];
    if blocks.is_empty() {
        return dom;
    }
    dom[0] = BTreeSet::from([0usize]);
    loop {
        let mut changed = false;
        for index in 1..blocks.len() {
            let preds = predecessors(index);
            // Unreachable predecessors converge to a self-only dominator
            // set; intersecting with it would collapse real dominators
            // (dead jmp pairs in real bytecode), so only predecessors the
            // entry block dominates participate.
            let mut new_dom: BTreeSet<usize> = match preds
                .iter()
                .filter(|pred| dom[**pred].contains(&0))
                .map(|pred| dom[*pred].clone())
                .reduce(|acc, set| acc.intersection(&set).copied().collect())
            {
                Some(set) => set,
                None => BTreeSet::new(),
            };
            new_dom.insert(index);
            if new_dom != dom[index] {
                dom[index] = new_dom;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    dom
}

fn find_index(instructions: &[Instruction], word_offset: usize) -> usize {
    instructions
        .iter()
        .position(|inst| inst.offset >= word_offset)
        .unwrap_or(instructions.len())
}

fn build_blocks(instructions: &[Instruction]) -> Vec<Block> {
    let mut leaders = BTreeSet::from([0usize]);
    for inst in instructions {
        match inst.opcode {
            15 | 16 | 17 => {
                let target = inst.offset as isize + isize::from(inst.operands[0]);
                if target > 0 {
                    leaders.insert(find_index(instructions, target as usize));
                }
                leaders.insert(find_index(instructions, inst.offset + inst.len_words));
            }
            119 | 122 => {
                leaders.insert(find_index(instructions, inst.offset + inst.len_words));
            }
            120 => {
                leaders.insert(find_index(instructions, inst.offset));
                let target = inst.offset as isize + isize::from(inst.operands[0]);
                if target > 0 {
                    leaders.insert(find_index(instructions, target as usize));
                }
            }
            _ => {}
        }
    }
    let leader_list = leaders
        .into_iter()
        .filter(|leader| *leader < instructions.len())
        .collect::<Vec<_>>();
    let mut blocks = Vec::with_capacity(leader_list.len());
    for (position, &start) in leader_list.iter().enumerate() {
        let end = leader_list
            .get(position + 1)
            .copied()
            .unwrap_or(instructions.len())
            - 1;
        let last = &instructions[end];
        let term = match last.opcode {
            17 => {
                let target = last.offset as isize + isize::from(last.operands[0]);
                if target <= 0 {
                    Term::Ret
                } else {
                    Term::Jmp(find_index(instructions, target as usize))
                }
            }
            15 | 16 => {
                let target = last.offset as isize + isize::from(last.operands[0]);
                if target <= 0 {
                    Term::Ret
                } else {
                    Term::Cond {
                        target: find_index(instructions, target as usize),
                        jump_if_true: last.opcode == 15,
                    }
                }
            }
            119 => Term::Ret,
            122 => Term::Throw,
            _ => Term::Fall,
        };
        blocks.push(Block { start, end, term });
    }
    // Resolve instruction-index targets to block indexes.
    let mut leader_to_block = BTreeMap::new();
    for (index, block) in blocks.iter().enumerate() {
        leader_to_block.insert(block.start, index);
    }
    for block in &mut blocks {
        let resolve = |target: usize| -> usize {
            leader_to_block
                .range(..=target)
                .next_back()
                .map(|(_, block)| *block)
                .unwrap_or(usize::MAX)
        };
        block.term = match block.term {
            Term::Jmp(target) => Term::Jmp(resolve(target)),
            Term::Cond {
                target,
                jump_if_true,
            } => Term::Cond {
                target: resolve(target),
                jump_if_true,
            },
            term => term,
        };
    }
    blocks
}

#[derive(Clone, Copy, Debug)]
struct LoopCtx {
    exit: usize,
    continue_target: usize,
}

#[derive(Clone, Debug, Default)]
struct SeqCtx {
    stop: BTreeSet<usize>,
    loop_ctx: Option<LoopCtx>,
    /// The entry block is itself the header of the enclosing loop (do-while
    /// bodies); do not re-match the loop at the first block.
    suppress_loop_at_entry: bool,
    /// The entry block is a `try` entry already being reconstructed; do not
    /// re-match the try at the first block (avoids infinite recursion when
    /// the try tail is not the entry block).
    suppress_try_at_entry: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SeqEnd {
    StoppedAt(usize),
    Jumped(usize),
    Returned,
}

struct BodyDecompiler<'f> {
    object: &'f CodeObject,
    scanner: Scanner<'f>,
    instructions: Vec<Instruction>,
    blocks: Vec<Block>,
    back_edges: BTreeMap<usize, Vec<usize>>,
    /// Blocks consumed by condition fusion (dead after decompilation).
    dead: BTreeSet<usize>,
    unhandled: usize,
}

impl<'f> BodyDecompiler<'f> {
    fn new(
        file: &'f BytecodeFile,
        object: &'f CodeObject,
        instructions: Vec<Instruction>,
        blocks: Vec<Block>,
    ) -> Self {
        // Natural back-edges: an edge s -> t is a loop back-edge only when
        // t dominates s (otherwise it is a forward-ish jump that merely
        // happens to point at an earlier block in layout order, e.g. dead
        // short-circuit constant blocks).
        let dominators = compute_dominators(&blocks);
        let mut back_edges: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (source, block) in blocks.iter().enumerate() {
            match block.term {
                Term::Jmp(target) | Term::Cond { target, .. }
                    if target != usize::MAX
                        && target < blocks.len()
                        && dominators[source].contains(&target) =>
                {
                    back_edges.entry(target).or_default().push(source);
                }
                _ => {}
            }
        }
        Self {
            object,
            scanner: Scanner::new(file, object),
            instructions,
            blocks,
            back_edges,
            dead: BTreeSet::new(),
            unhandled: 0,
        }
    }

    fn run(mut self) -> BodyOutput {
        let (mut statements, mut end) = self.seq(0, &SeqCtx::default());
        // A trailing forward jump (an empty trampoline between top-level
        // constructs, or a dispatch the constructs handed back) continues
        // the body; only jumps that cannot be followed are reported.
        let mut followed = BTreeSet::new();
        while let SeqEnd::Jumped(target) = end {
            if target == usize::MAX
                || target >= self.blocks.len()
                || target == 0
                || !followed.insert(target)
            {
                self.unhandled += 1;
                statements
                    .push(self.marker(&format!("unexpected trailing jump to block {target}")));
                break;
            }
            let (more, next) = self.seq(target, &SeqCtx::default());
            statements.extend(more);
            end = next;
        }
        self.unhandled += self.scanner.unhandled_count();
        statements.extend(self.scanner.take_out());
        BodyOutput {
            statements,
            unhandled: self.unhandled,
        }
    }

    fn marker(&self, reason: &str) -> Stmt {
        Stmt::new(
            StmtKind::Expr(Expr::new(
                ExprKind::Identifier(Ident::new(super::stmt::unhandled_marker(reason))),
                Span::empty(0),
            )),
            Span::empty(0),
        )
    }

    fn block_insts(&self, index: usize) -> &[Instruction] {
        let block = &self.blocks[index];
        &self.instructions[block.start..=block.end.min(self.instructions.len() - 1)]
    }

    fn block_body(&self, index: usize) -> Vec<Instruction> {
        let mut insts = self.block_insts(index).to_vec();
        match self.blocks[index].term {
            Term::Jmp(_) | Term::Cond { .. } | Term::Ret | Term::Throw => {
                insts.pop();
            }
            Term::Fall => {}
        }
        insts.retain(|inst| !matches!(inst.opcode, 120 | 121));
        insts
    }

    fn skip_trampolines(&self, mut index: usize) -> usize {
        let mut seen = BTreeSet::new();
        while index < self.blocks.len() && seen.insert(index) {
            match self.blocks[index].term {
                Term::Jmp(target)
                    if target != usize::MAX
                        && target > index
                        && self.block_body(index).is_empty() =>
                {
                    index = target;
                }
                _ => break,
            }
        }
        index
    }

    /// Scans a cond block and returns (prelude statements, condition expr,
    /// target, jump_if_true).
    fn cond_info(&mut self, index: usize) -> (Vec<Stmt>, Expr, usize, bool) {
        let (target, jump_if_true) = match self.blocks[index].term {
            Term::Cond {
                target,
                jump_if_true,
            } => (target, jump_if_true),
            _ => unreachable!("cond_info on non-cond block"),
        };
        let body = self.block_body(index);
        let (cond, prelude) = self.scanner.scan_condition_block(&body);
        let cond = cond.unwrap_or_else(|| Expr::new(ExprKind::Bool(true), Span::empty(0)));
        (prelude, cond, target, jump_if_true)
    }

    /// Scans a cond block and returns the raw flag condition plus any
    /// prelude statements.
    fn raw_cond(&mut self, index: usize) -> (Option<Cond>, Vec<Stmt>) {
        let body = self.block_body(index);
        self.scanner.scan_condition_block_raw(&body)
    }

    fn seq(&mut self, entry: usize, ctx: &SeqCtx) -> (Vec<Stmt>, SeqEnd) {
        let mut stmts = Vec::new();
        // Safety bound: pathological control flow must degrade instead of
        // looping forever. The bound is generous (each iteration consumes
        // at least one block on any terminating path).
        let mut iterations = 0usize;
        // A leading jump-only block targeting the loop exit or continue
        // target is a break/continue, not a trampoline (unless the target is
        // this region's natural end, in which case it is the tail jump).
        if let Some(loop_ctx) = ctx.loop_ctx
            && let Term::Jmp(target) = self.blocks[entry].term
            && self.block_body(entry).is_empty()
        {
            if target == loop_ctx.exit {
                stmts.push(Stmt::new(StmtKind::Break, Span::empty(0)));
                return (stmts, SeqEnd::Returned);
            }
            if target == loop_ctx.continue_target && !ctx.stop.contains(&target) {
                stmts.push(Stmt::new(StmtKind::Continue, Span::empty(0)));
                return (stmts, SeqEnd::Returned);
            }
        }
        // A branch whose entry is a loop header itself (a cond block jumped
        // to directly) is a `continue`; scanning it would re-decompile the
        // loop and recurse forever.
        if ctx.loop_ctx.is_some()
            && !ctx.suppress_loop_at_entry
            && self.back_edges.contains_key(&entry)
        {
            stmts.push(Stmt::new(StmtKind::Continue, Span::empty(0)));
            return (stmts, SeqEnd::Returned);
        }
        let mut current = self.skip_trampolines(entry);
        let mut first = true;
        let bound = self.blocks.len().saturating_mul(64).max(256);
        loop {
            iterations += 1;
            if iterations > bound {
                // Degrade instead of looping forever on pathological CFGs.
                self.unhandled += 1;
                stmts.push(self.marker(&format!(
                    "control-flow iteration bound exceeded at block {current}"
                )));
                return (stmts, SeqEnd::Returned);
            }
            while self.dead.contains(&current) {
                current += 1;
            }
            if ctx.stop.contains(&current) || current >= self.blocks.len() {
                return (stmts, SeqEnd::StoppedAt(current));
            }

            if !(first && ctx.suppress_try_at_entry) && self.block_starts_with_entry(current) {
                match self.try_construct(current, ctx) {
                    Ok((try_stmts, next)) => {
                        stmts.extend(try_stmts);
                        current = next;
                        continue;
                    }
                    Err(()) => {
                        self.unhandled += 1;
                        stmts.push(self.marker("unmatched try structure"));
                        return (stmts, SeqEnd::Returned);
                    }
                }
            }

            // Non-cond-headed loop headers (do-while bodies, `for (;;)`).
            let loop_check = !(first && ctx.suppress_loop_at_entry);
            if loop_check
                && self.back_edges.contains_key(&current)
                && !matches!(self.blocks[current].term, Term::Cond { .. })
                && let Some((loop_stmts, next)) = self.loop_construct(current, ctx)
            {
                stmts.extend(loop_stmts);
                current = self.skip_trampolines(next);
                first = false;
                continue;
            }
            first = false;

            match self.blocks[current].term {
                Term::Ret | Term::Throw => {
                    // Keep the `srv`/`ret` pair; only strip try markers.
                    let mut body = self.block_insts(current).to_vec();
                    body.retain(|inst| !matches!(inst.opcode, 120 | 121));
                    self.scanner.scan_linear(&body);
                    stmts.extend(self.scanner.take_out());
                    return (stmts, SeqEnd::Returned);
                }
                Term::Jmp(target) => {
                    let body = self.block_body(current);
                    self.scanner.scan_linear(&body);
                    stmts.extend(self.scanner.take_out());
                    if target == usize::MAX {
                        self.unhandled += 1;
                        stmts.push(self.marker("invalid jump target"));
                        return (stmts, SeqEnd::Returned);
                    }
                    if target <= current {
                        return (stmts, SeqEnd::Jumped(target));
                    }
                    if let Some(loop_ctx) = ctx.loop_ctx {
                        if target == loop_ctx.exit {
                            stmts.push(Stmt::new(StmtKind::Break, Span::empty(0)));
                            return (stmts, SeqEnd::Returned);
                        }
                        // A jump to the continue target that is this
                        // region's natural end is the loop tail, not a
                        // `continue`.
                        if target == loop_ctx.continue_target {
                            if ctx.stop.contains(&target) {
                                return (stmts, SeqEnd::Jumped(target));
                            }
                            stmts.push(Stmt::new(StmtKind::Continue, Span::empty(0)));
                            return (stmts, SeqEnd::Returned);
                        }
                    }
                    // Loop pre-entry: a forward jump straight to a loop
                    // header starts the loop construct here.
                    if self.back_edges.contains_key(&target) {
                        current = target;
                        continue;
                    }
                    // Switch pre-entry: `cp %anchor, %x; jmp <test chain>`.
                    if let Some(anchor) = self.switch_anchor_reg(current, target)
                        && let Some((switch_stmts, next)) =
                            self.switch_construct(target, anchor, ctx)
                    {
                        stmts.extend(switch_stmts);
                        current = next;
                        continue;
                    }
                    return (stmts, SeqEnd::Jumped(target));
                }
                Term::Cond { .. } => {
                    if let Some((construct, next)) = self.cond_construct(current, ctx) {
                        stmts.extend(construct);
                        match next {
                            SeqEnd::StoppedAt(next) => {
                                // A region boundary is not a trampoline to
                                // skip: returning it ends the region here.
                                if ctx.stop.contains(&next) {
                                    return (stmts, SeqEnd::StoppedAt(next));
                                }
                                // Inside a loop body, a construct ending at
                                // the loop header has consumed the continue
                                // jump; re-entering would re-decompile the
                                // loop forever. Outside loops the header is
                                // entered normally.
                                if ctx.loop_ctx.is_some() && self.back_edges.contains_key(&next) {
                                    return (stmts, SeqEnd::StoppedAt(next));
                                }
                                current = self.skip_trampolines(next);
                                continue;
                            }
                            end => return (stmts, end),
                        }
                    }
                    self.unhandled += 1;
                    stmts.push(self.marker("unmatched conditional structure"));
                    return (stmts, SeqEnd::Returned);
                }
                Term::Fall => {
                    let body = self.block_body(current);
                    self.scanner.scan_linear(&body);
                    stmts.extend(self.scanner.take_out());
                    let mut next = current + 1;
                    while self.dead.contains(&next) {
                        next += 1;
                    }
                    // The region boundary wins over trampoline skipping:
                    // a jmp-only block in `stop` (a try tail, a loop tail)
                    // ends the region here.
                    if ctx.stop.contains(&next) {
                        return (stmts, SeqEnd::StoppedAt(next));
                    }
                    // Official switch shape: the anchor block falls into
                    // the test chain.
                    let next_block = self.skip_trampolines(next);
                    if let Some(anchor) = self.switch_anchor_reg(current, next_block)
                        && let Some((switch_stmts, after)) =
                            self.switch_construct(next_block, anchor, ctx)
                    {
                        stmts.extend(switch_stmts);
                        current = after;
                        continue;
                    }
                    current = next_block;
                }
            }
        }
    }

    fn block_starts_with_entry(&self, index: usize) -> bool {
        self.block_insts(index)
            .first()
            .is_some_and(|inst| inst.opcode == 120)
    }

    fn cond_construct(&mut self, index: usize, ctx: &SeqCtx) -> Option<(Vec<Stmt>, SeqEnd)> {
        if self.back_edges.contains_key(&index) {
            if let Some((stmts, next)) = self.loop_construct(index, ctx) {
                return Some((stmts, SeqEnd::StoppedAt(next)));
            }
        }

        let fused = self.fuse_conditions(index)?;
        if fused.is_value {
            // A `&&`/`||` value fusion produces an expression, not an `if`.
            return Some((fused.prelude, SeqEnd::StoppedAt(fused.then_entry)));
        }
        if let Some(merge) = self.ternary_restore(&fused.cond, fused.then_entry, fused.else_entry) {
            // `t = cond ? a : b`: both branches merge into one register, so
            // the construct is a value, not a statement.
            return Some((fused.prelude, SeqEnd::StoppedAt(merge)));
        }
        let (if_stmt, end) = self.if_construct(fused.cond, fused.then_entry, fused.else_entry, ctx);
        let mut stmts = fused.prelude;
        stmts.push(if_stmt);
        Some((stmts, end))
    }

    fn fuse_conditions(&mut self, index: usize) -> Option<FusedCondition> {
        self.fuse_conditions_depth(index, 0)
    }

    fn fuse_conditions_depth(&mut self, index: usize, depth: usize) -> Option<FusedCondition> {
        // The `||` remainder recursion is bounded by the block count, but
        // pathological bytecode can chain conditions cyclically; cap the
        // depth instead of overflowing the stack.
        if depth > self.blocks.len() {
            return None;
        }
        let (mut prelude, mut cond, mut target, mut jump_if_true) = self.cond_info(index);
        let mut last_cond = index;
        loop {
            let next = last_cond + 1;
            if next >= self.blocks.len() {
                break;
            }
            let next_term = self.blocks[next].term;
            let next_ends_setf = self
                .block_body(next)
                .last()
                .is_some_and(|inst| matches!(inst.opcode, 11 | 12));

            // Repo dialect value form: `jf rhs` with a jmp-only trampoline
            // between the condition and the rhs block (dead constant blocks
            // from earlier fusions may also sit in between).
            let rhs_block = if jump_if_true
                && matches!(next_term, Term::Jmp(_))
                && self.block_body(next).is_empty()
                && (target == next + 1 || target > next)
            {
                Some(target)
            } else {
                None
            };
            let is_value_form = rhs_block.is_some()
                && rhs_block
                    .map(|rhs_block| {
                        self.block_body(rhs_block)
                            .last()
                            .is_some_and(|inst| matches!(inst.opcode, 11 | 12))
                            && matches!(
                                self.blocks[rhs_block].term,
                                Term::Jmp(merge) if merge == rhs_block + 1
                            )
                            && self
                                .scanner
                                .is_pure_condition_block(&self.block_body(rhs_block))
                    })
                    .unwrap_or(false);
            if let Some(rhs_block) = rhs_block.filter(|_| is_value_form) {
                let (raw1, prelude1) = self.raw_cond(last_cond);
                let raw1 = raw1?;
                // `last_cond` was already scanned by `cond_info`; its
                // prelude is already in `prelude`, so the re-scan's
                // statements are dropped (they would duplicate side
                // effects).
                let _ = prelude1;
                let lhs = raw_operand_expr(&self.scanner, raw1);
                let op = match raw1 {
                    Cond::Truthy { inv: true, .. } => BinaryOp::LogicalOr,
                    _ => BinaryOp::LogicalAnd,
                };
                let last_inst = self.block_body(rhs_block).last().unwrap().clone();
                let dest_reg = last_inst.operands[0];
                let negate = last_inst.opcode == 12;
                let (raw2, prelude2) = self.raw_cond(rhs_block);
                prelude.extend(prelude2);
                let rhs = raw2
                    .map(|raw| raw_operand_expr(&self.scanner, raw))
                    .unwrap_or_else(|| Expr::new(ExprKind::Bool(true), Span::empty(0)));
                let mut fused_expr = Expr::new(
                    ExprKind::Binary {
                        op,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    },
                    Span::empty(0),
                );
                if negate {
                    fused_expr = Expr::new(
                        ExprKind::Unary {
                            op: syntax::UnaryOp::LogicalNot,
                            expr: Box::new(fused_expr),
                        },
                        Span::empty(0),
                    );
                }
                self.scanner.set_reg(dest_reg, fused_expr);
                let merge = self.skip_trampolines(rhs_block + 1);
                // The cond's fallthrough trampoline and the constant
                // true/false block it leads to are dead after fusion.
                if let Term::Jmp(false_block) = next_term {
                    self.dead.insert(next);
                    if false_block != usize::MAX {
                        self.dead.insert(false_block);
                    }
                }
                return Some(FusedCondition {
                    prelude,
                    cond: self.scanner.reg_expr(dest_reg),
                    then_entry: merge,
                    else_entry: merge,
                    is_value: true,
                });
            }

            // Official dialect value form: `jnf merge` with the rhs block
            // ending in setf/setnf and falling into the merge block.
            if !jump_if_true
                && matches!(next_term, Term::Fall)
                && target == next + 1
                && next_ends_setf
                && self.scanner.is_pure_condition_block(&self.block_body(next))
            {
                let (raw1, prelude1) = self.raw_cond(last_cond);
                let raw1 = raw1?;
                // `last_cond` was already scanned by `cond_info`; dropping
                // the re-scan's prelude avoids duplicated side effects.
                let _ = prelude1;
                let lhs = raw_operand_expr(&self.scanner, raw1);
                let last_inst = self.block_body(next).last().unwrap().clone();
                let dest_reg = last_inst.operands[0];
                let negate = last_inst.opcode == 12;
                let (raw2, prelude2) = self.raw_cond(next);
                prelude.extend(prelude2);
                let rhs = raw2
                    .map(|raw| raw_operand_expr(&self.scanner, raw))
                    .unwrap_or_else(|| Expr::new(ExprKind::Bool(true), Span::empty(0)));
                let mut fused_expr = Expr::new(
                    ExprKind::Binary {
                        op: BinaryOp::LogicalAnd,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    },
                    Span::empty(0),
                );
                if negate {
                    fused_expr = Expr::new(
                        ExprKind::Unary {
                            op: syntax::UnaryOp::LogicalNot,
                            expr: Box::new(fused_expr),
                        },
                        Span::empty(0),
                    );
                }
                self.scanner.set_reg(dest_reg, fused_expr);
                return Some(FusedCondition {
                    prelude,
                    cond: self.scanner.reg_expr(dest_reg),
                    then_entry: target,
                    else_entry: target,
                    is_value: true,
                });
            }

            // `a && b`: jump-if-true to the next condition block (`a` true
            // means `b` still gates the body; `a` false skips to the else).
            if jump_if_true
                && target == next
                && matches!(next_term, Term::Cond { .. })
                && self.scanner.is_pure_condition_block(&self.block_body(next))
            {
                let (prelude2, cond2, target2, jt2) = self.cond_info(next);
                prelude.extend(prelude2);
                cond = Expr::new(
                    ExprKind::Binary {
                        op: BinaryOp::LogicalAnd,
                        lhs: Box::new(cond),
                        rhs: Box::new(cond2),
                    },
                    Span::empty(0),
                );
                last_cond = next;
                target = target2;
                jump_if_true = jt2;
                continue;
            }
            // `a || ...`: jump-if-false to the next condition block with the
            // matched path skipping it via a jmp-only trampoline. The
            // remainder fuses recursively so `a || b && c` keeps TJS2
            // precedence.
            if !jump_if_true
                && target != next
                && matches!(self.blocks[target].term, Term::Cond { .. })
                && matches!(next_term, Term::Jmp(_))
                && self.block_body(next).is_empty()
            {
                let rhs = self.fuse_conditions_depth(target, depth + 1)?;
                if rhs.is_value {
                    return None;
                }
                prelude.extend(rhs.prelude);
                cond = Expr::new(
                    ExprKind::Binary {
                        op: BinaryOp::LogicalOr,
                        lhs: Box::new(cond),
                        rhs: Box::new(rhs.cond),
                    },
                    Span::empty(0),
                );
                return Some(FusedCondition {
                    prelude,
                    cond,
                    then_entry: self.skip_trampolines(next),
                    else_entry: rhs.else_entry,
                    is_value: false,
                });
            }
            // Two consecutive conditions jumping to the same target with the
            // same polarity short-circuit: `jf` pairs (jump if true) fuse
            // with `||`, `jnf` pairs (jump if false) with `&&` (the flag
            // polarity carries the operand negations).
            if let Term::Cond {
                target: t2,
                jump_if_true: jt2,
            } = next_term
            {
                if t2 == target
                    && jt2 == jump_if_true
                    && self.scanner.is_pure_condition_block(&self.block_body(next))
                {
                    let (prelude2, cond2, _, _) = self.cond_info(next);
                    prelude.extend(prelude2);
                    let op = if jump_if_true {
                        BinaryOp::LogicalOr
                    } else {
                        BinaryOp::LogicalAnd
                    };
                    cond = Expr::new(
                        ExprKind::Binary {
                            op,
                            lhs: Box::new(cond),
                            rhs: Box::new(cond2),
                        },
                        Span::empty(0),
                    );
                    last_cond = next;
                    continue;
                }
            }
            break;
        }
        // `jf` jumps to the then branch; `jnf` falls through to it and jumps
        // to the else branch, so the entries swap without negating the
        // condition (the flag polarity already carries any negation).
        let (then_entry, else_entry) = if jump_if_true {
            (
                self.skip_trampolines(target),
                self.skip_trampolines(last_cond + 1),
            )
        } else {
            (
                self.skip_trampolines(last_cond + 1),
                self.skip_trampolines(target),
            )
        };
        Some(FusedCondition {
            prelude,
            cond,
            then_entry,
            else_entry,
            is_value: false,
        })
    }

    /// Restores `t = cond ? a : b` when both branches are single value
    /// definitions of the same positive register merging into one
    /// continuation block. Returns the merge block index on success; the
    /// scanner's register then carries the conditional expression and the
    /// branch blocks are marked dead.
    fn ternary_restore(
        &mut self,
        cond: &Expr,
        then_entry: usize,
        else_entry: usize,
    ) -> Option<usize> {
        let then_body = self.block_body(then_entry);
        let else_body = self.block_body(else_entry);
        if then_body.len() != 1 || else_body.len() != 1 {
            return None;
        }
        let merge = match (self.blocks[then_entry].term, self.blocks[else_entry].term) {
            (Term::Jmp(a), Term::Jmp(b)) if a == b && a > then_entry && a > else_entry => a,
            (Term::Fall, Term::Jmp(m)) if then_entry + 1 == m && m > else_entry => m,
            (Term::Jmp(m), Term::Fall) if else_entry + 1 == m && m > then_entry => m,
            _ => return None,
        };
        // The branch bodies must define the same positive register with a
        // single value-producing instruction and no side statements.
        let saved_materialize = self.scanner.materialize;
        self.scanner.materialize = false;
        let saved_out = self.scanner.take_out();
        let then_inst = then_body[0].clone();
        self.scanner.scan_linear(&then_body);
        let then_clean = self.scanner.out_is_empty();
        let then_reg = then_inst.operands.first().copied();
        let then_expr = then_reg
            .filter(|reg| *reg > 0)
            .map(|reg| self.scanner.reg_expr(reg));
        let else_inst = else_body[0].clone();
        self.scanner.scan_linear(&else_body);
        let else_clean = self.scanner.out_is_empty();
        let else_reg = else_inst.operands.first().copied();
        let else_expr = else_reg
            .filter(|reg| *reg > 0)
            .map(|reg| self.scanner.reg_expr(reg));
        let (Some(then_reg), Some(else_reg)) = (then_reg, else_reg) else {
            self.scanner.restore_out(saved_out);
            self.scanner.materialize = saved_materialize;
            return None;
        };
        let ok = then_clean
            && else_clean
            && then_reg == else_reg
            && then_reg > 0
            && then_expr.is_some()
            && else_expr.is_some();
        if !ok {
            self.scanner.restore_out(saved_out);
            self.scanner.materialize = saved_materialize;
            return None;
        }
        let then_expr = then_expr.unwrap();
        let else_expr = else_expr.unwrap();
        let conditional = Expr::new(
            ExprKind::Conditional {
                condition: Box::new(cond.clone()),
                then_expr: Box::new(then_expr),
                else_expr: Box::new(else_expr),
            },
            Span::empty(0),
        );
        self.scanner.set_reg(then_reg, conditional);
        self.dead.insert(then_entry);
        self.dead.insert(else_entry);
        self.scanner.restore_out(saved_out);
        self.scanner.materialize = saved_materialize;
        Some(merge)
    }

    /// Whether every path out of the else side (the fall-through body) leads
    /// back to `then_entry` (the jump target) — the official compiler's
    /// guarded-statement shape. The walk is bounded so real if-else bodies
    /// (which reach other blocks) fail fast.
    fn guard_body_joins(&self, else_entry: usize, then_entry: usize) -> bool {
        let mut seen = BTreeSet::new();
        let mut frontier = vec![else_entry];
        let mut count = 0usize;
        while let Some(block) = frontier.pop() {
            if block == then_entry || !seen.insert(block) {
                continue;
            }
            count += 1;
            if count > 64 {
                return false;
            }
            match self.blocks[block].term {
                Term::Jmp(target) if target == then_entry => {}
                Term::Fall if block + 1 < self.blocks.len() => frontier.push(block + 1),
                // A nested branch may only skip to the same continuation.
                Term::Cond { target, .. } if target == then_entry => frontier.push(block + 1),
                _ => return false,
            }
        }
        true
    }

    /// Decompiles the guard body (the else side) with the continuation as
    /// the region boundary.
    fn decompile_guard_body(
        &mut self,
        else_entry: usize,
        then_entry: usize,
        ctx: &SeqCtx,
    ) -> Option<Vec<Stmt>> {
        let mut stop = ctx.stop.clone();
        stop.insert(then_entry);
        let (stmts, end) = self.seq(
            else_entry,
            &SeqCtx {
                stop,
                loop_ctx: ctx.loop_ctx,
                suppress_loop_at_entry: false,
                suppress_try_at_entry: false,
            },
        );
        match end {
            SeqEnd::StoppedAt(block) | SeqEnd::Jumped(block) if block == then_entry => Some(stmts),
            _ => None,
        }
    }

    fn if_construct(
        &mut self,
        cond: Expr,
        then_entry: usize,
        else_entry: usize,
        ctx: &SeqCtx,
    ) -> (Stmt, SeqEnd) {
        // Guard form: the official compiler's guarded statements
        // (`cond; jf <continuation>` with the fall-through body jumping
        // back to the continuation) decompile as `if (!cond) { body }`.
        // Reconstructing them as a regular if-else would nest the whole
        // continuation into the then side and blow up exponentially on
        // guard chains.
        if then_entry != else_entry
            && self.guard_body_joins(else_entry, then_entry)
            && let Some(guard_stmts) = self.decompile_guard_body(else_entry, then_entry, ctx)
        {
            let if_stmt = Stmt::new(
                StmtKind::If {
                    condition: negate_condition(cond),
                    then_branch: Box::new(Stmt::new(StmtKind::Block(guard_stmts), Span::empty(0))),
                    else_branch: None,
                },
                Span::empty(0),
            );
            return (if_stmt, SeqEnd::StoppedAt(then_entry));
        }
        // The else side jumping forward to the block right after the then
        // side is an if-else whose then falls into the merge.
        let mirror_merge = match self.blocks[else_entry].term {
            Term::Jmp(m) if m > then_entry && m > else_entry => Some(m),
            _ => None,
        };
        let mut then_stop = ctx.stop.clone();
        then_stop.insert(else_entry);
        if let Some(merge) = mirror_merge {
            then_stop.insert(merge);
        }
        let (mut then_stmts, mut then_end) = self.seq(
            then_entry,
            &SeqCtx {
                stop: then_stop.clone(),
                loop_ctx: ctx.loop_ctx,
                suppress_loop_at_entry: false,
                suppress_try_at_entry: false,
            },
        );
        // The then branch can dispatch forward within its own region
        // (official `typeof` pre-checks, switch-style dispatch): follow
        // strictly-internal forward jumps before matching the branch end. A
        // jump to a jump-only trampoline is always an internal dispatch
        // (merges are real blocks); interleaved layouts can place the then's
        // content past the else side, so trampolines are followed even there.
        let mut hopped = BTreeSet::new();
        while let SeqEnd::Jumped(end) = then_end {
            if end == usize::MAX || end <= then_entry || !hopped.insert(end) {
                break;
            }
            let real = self.skip_trampolines(end);
            if real == end && end >= else_entry {
                break;
            }
            let (more, end2) = self.seq(
                real,
                &SeqCtx {
                    stop: then_stop.clone(),
                    loop_ctx: ctx.loop_ctx,
                    suppress_loop_at_entry: false,
                    suppress_try_at_entry: false,
                },
            );
            then_stmts.extend(more);
            then_end = end2;
        }
        let then_stmt = Stmt::new(StmtKind::Block(then_stmts), Span::empty(0));
        match then_end {
            // The branch jumps straight into the enclosing region's tail
            // (a loop's shared step/tail block): the branch itself is empty
            // and the fall-through side covers the real work, so the whole
            // construct is `if (!cond) { else-side }`.
            SeqEnd::StoppedAt(block)
                if block == then_entry && block != else_entry && ctx.stop.contains(&block) =>
            {
                let (else_stmts, else_end) = self.seq(
                    else_entry,
                    &SeqCtx {
                        stop: ctx.stop.clone(),
                        loop_ctx: ctx.loop_ctx,
                        suppress_loop_at_entry: false,
                        suppress_try_at_entry: false,
                    },
                );
                if !matches!(
                    else_end,
                    SeqEnd::StoppedAt(block) | SeqEnd::Jumped(block) if block == then_entry
                ) && !matches!(else_end, SeqEnd::Returned)
                {
                    self.unhandled += 1;
                    let marker = self.marker("unexpected if tail structure");
                    let if_stmt = Stmt::new(
                        StmtKind::If {
                            condition: negate_condition(cond),
                            then_branch: Box::new(Stmt::new(
                                StmtKind::Block(else_stmts),
                                Span::empty(0),
                            )),
                            else_branch: None,
                        },
                        Span::empty(0),
                    );
                    return (
                        Stmt::new(StmtKind::Block(vec![marker, if_stmt]), Span::empty(0)),
                        SeqEnd::Returned,
                    );
                }
                let if_stmt = Stmt::new(
                    StmtKind::If {
                        condition: negate_condition(cond),
                        then_branch: Box::new(Stmt::new(
                            StmtKind::Block(else_stmts),
                            Span::empty(0),
                        )),
                        else_branch: None,
                    },
                    Span::empty(0),
                );
                (if_stmt, SeqEnd::StoppedAt(then_entry))
            }
            // The else side jumps forward to the merge right after the then
            // side: an if-else whose then falls into the merge.
            SeqEnd::StoppedAt(block) if mirror_merge == Some(block) && block != else_entry => {
                let (else_stmts, else_end) = self.seq(
                    else_entry,
                    &SeqCtx {
                        stop: BTreeSet::from([block]),
                        loop_ctx: ctx.loop_ctx,
                        suppress_loop_at_entry: false,
                        suppress_try_at_entry: false,
                    },
                );
                if !matches!(
                    else_end,
                    SeqEnd::StoppedAt(block2) | SeqEnd::Jumped(block2) if block2 == block
                ) {
                    self.unhandled += 1;
                    let marker = self.marker("unexpected mirrored if structure");
                    let if_stmt = Stmt::new(
                        StmtKind::If {
                            condition: cond,
                            then_branch: Box::new(then_stmt),
                            else_branch: None,
                        },
                        Span::empty(0),
                    );
                    return (
                        Stmt::new(StmtKind::Block(vec![marker, if_stmt]), Span::empty(0)),
                        SeqEnd::Returned,
                    );
                }
                let if_stmt = Stmt::new(
                    StmtKind::If {
                        condition: cond,
                        then_branch: Box::new(then_stmt),
                        else_branch: Some(Box::new(Stmt::new(
                            StmtKind::Block(else_stmts),
                            Span::empty(0),
                        ))),
                    },
                    Span::empty(0),
                );
                (if_stmt, SeqEnd::StoppedAt(block))
            }
            SeqEnd::StoppedAt(block) if block == else_entry => {
                let if_stmt = Stmt::new(
                    StmtKind::If {
                        condition: cond,
                        then_branch: Box::new(then_stmt),
                        else_branch: None,
                    },
                    Span::empty(0),
                );
                (if_stmt, SeqEnd::StoppedAt(else_entry))
            }
            SeqEnd::Jumped(end) if end == else_entry => {
                let if_stmt = Stmt::new(
                    StmtKind::If {
                        condition: cond,
                        then_branch: Box::new(then_stmt),
                        else_branch: None,
                    },
                    Span::empty(0),
                );
                (if_stmt, SeqEnd::StoppedAt(else_entry))
            }
            // The then side ends at (or jumps past) the else side: an
            // if-else whose merge is the then side's end. The else side may
            // itself dispatch forward through several constructs before
            // reaching that merge, so its jumps are followed.
            SeqEnd::StoppedAt(block) | SeqEnd::Jumped(block)
                if block != usize::MAX && block > else_entry && block != then_entry =>
            {
                let merge = block;
                let mut else_stop = BTreeSet::new();
                else_stop.insert(merge);
                let mut else_stmts = Vec::new();
                let mut cursor = else_entry;
                let mut hopped = BTreeSet::new();
                let else_end = loop {
                    let (more, end) = self.seq(
                        cursor,
                        &SeqCtx {
                            stop: else_stop.clone(),
                            loop_ctx: ctx.loop_ctx,
                            suppress_loop_at_entry: false,
                            suppress_try_at_entry: false,
                        },
                    );
                    else_stmts.extend(more);
                    match end {
                        SeqEnd::Jumped(next)
                            if next != merge && next > cursor && hopped.insert(next) =>
                        {
                            cursor = next;
                        }
                        other => break other,
                    }
                };
                let else_stmt = Stmt::new(StmtKind::Block(else_stmts), Span::empty(0));
                let if_stmt = Stmt::new(
                    StmtKind::If {
                        condition: cond,
                        then_branch: Box::new(then_stmt),
                        else_branch: Some(Box::new(else_stmt)),
                    },
                    Span::empty(0),
                );
                match else_end {
                    SeqEnd::StoppedAt(end) | SeqEnd::Jumped(end) if end == merge => {
                        (if_stmt, SeqEnd::StoppedAt(merge))
                    }
                    other => (if_stmt, other),
                }
            }
            SeqEnd::Returned => {
                let (else_stmts, else_end) = self.seq(else_entry, ctx);
                let else_stmt = Stmt::new(StmtKind::Block(else_stmts), Span::empty(0));
                let if_stmt = Stmt::new(
                    StmtKind::If {
                        condition: cond,
                        then_branch: Box::new(then_stmt),
                        else_branch: Some(Box::new(else_stmt)),
                    },
                    Span::empty(0),
                );
                (if_stmt, else_end)
            }
            // A jump to the enclosing loop's continue target is a `continue`
            // (the natural-tail case where `end == else_entry` was handled
            // above): the then branch skips the fall-through side, so the
            // else must be decompiled here.
            SeqEnd::Jumped(end)
                if end != usize::MAX
                    && end < else_entry
                    && ctx
                        .loop_ctx
                        .is_some_and(|loop_ctx| loop_ctx.continue_target == end) =>
            {
                let mut else_stop = ctx.stop.clone();
                else_stop.insert(end);
                let (else_stmts, else_end) = self.seq(
                    else_entry,
                    &SeqCtx {
                        stop: else_stop,
                        loop_ctx: ctx.loop_ctx,
                        suppress_loop_at_entry: false,
                        suppress_try_at_entry: false,
                    },
                );
                let next = match else_end {
                    SeqEnd::StoppedAt(block) | SeqEnd::Jumped(block)
                        if block == end || ctx.stop.contains(&block) =>
                    {
                        block
                    }
                    _ => {
                        self.unhandled += 1;
                        let marker = self.marker("unexpected continue structure");
                        let if_stmt = Stmt::new(
                            StmtKind::If {
                                condition: cond,
                                then_branch: Box::new(then_stmt),
                                else_branch: None,
                            },
                            Span::empty(0),
                        );
                        return (
                            Stmt::new(StmtKind::Block(vec![marker, if_stmt]), Span::empty(0)),
                            SeqEnd::Returned,
                        );
                    }
                };
                let StmtKind::Block(mut then_stmts) = then_stmt.kind else {
                    unreachable!("then branch is always a block");
                };
                then_stmts.push(Stmt::new(StmtKind::Continue, Span::empty(0)));
                let if_stmt = Stmt::new(
                    StmtKind::If {
                        condition: cond,
                        then_branch: Box::new(Stmt::new(
                            StmtKind::Block(then_stmts),
                            Span::empty(0),
                        )),
                        else_branch: Some(Box::new(Stmt::new(
                            StmtKind::Block(else_stmts),
                            Span::empty(0),
                        ))),
                    },
                    Span::empty(0),
                );
                (if_stmt, SeqEnd::StoppedAt(next))
            }
            // The then branch flows into the enclosing region's tail (a try
            // tail, a loop tail): an `if` without else ending the region.
            SeqEnd::StoppedAt(block) | SeqEnd::Jumped(block)
                if ctx.stop.contains(&block) && block != else_entry && block != then_entry =>
            {
                let if_stmt = Stmt::new(
                    StmtKind::If {
                        condition: cond,
                        then_branch: Box::new(then_stmt),
                        else_branch: None,
                    },
                    Span::empty(0),
                );
                (if_stmt, SeqEnd::StoppedAt(block))
            }
            other => {
                self.unhandled += 1;
                let marker = self.marker("unexpected if branch structure");
                let if_stmt = Stmt::new(
                    StmtKind::If {
                        condition: cond,
                        then_branch: Box::new(then_stmt),
                        else_branch: None,
                    },
                    Span::empty(0),
                );
                let _ = other;
                (
                    Stmt::new(StmtKind::Block(vec![marker, if_stmt]), Span::empty(0)),
                    SeqEnd::Returned,
                )
            }
        }
    }

    fn loop_construct(&mut self, index: usize, _ctx: &SeqCtx) -> Option<(Vec<Stmt>, usize)> {
        let header_term = self.blocks[index].term;
        let sources = self.back_edges.get(&index)?.clone();
        let tail = sources.iter().copied().max()?;

        if let Term::Cond {
            target,
            jump_if_true,
        } = self.blocks[tail].term
        {
            if target == index && jump_if_true && !matches!(header_term, Term::Cond { .. }) {
                let (prelude, cond, _, _) = self.cond_info(tail);
                let mut stop = BTreeSet::new();
                stop.insert(tail);
                let (body, _) = self.seq(
                    index,
                    &SeqCtx {
                        stop,
                        loop_ctx: Some(LoopCtx {
                            exit: tail + 1,
                            continue_target: tail,
                        }),
                        suppress_loop_at_entry: true,
                        suppress_try_at_entry: false,
                    },
                );
                let stmt = Stmt::new(
                    StmtKind::DoWhile {
                        body: Box::new(Stmt::new(StmtKind::Block(body), Span::empty(0))),
                        condition: cond,
                    },
                    Span::empty(0),
                );
                let mut stmts = prelude;
                stmts.push(stmt);
                return Some((stmts, tail + 1));
            }
        }

        if !matches!(header_term, Term::Cond { .. }) {
            let mut stop = BTreeSet::new();
            stop.insert(tail);
            let (body, _) = self.seq(
                index,
                &SeqCtx {
                    stop,
                    loop_ctx: Some(LoopCtx {
                        exit: tail + 1,
                        continue_target: index,
                    }),
                    suppress_loop_at_entry: true,
                    suppress_try_at_entry: false,
                },
            );
            let cond = Expr::new(ExprKind::Bool(true), Span::empty(0));
            let stmt = Stmt::new(
                StmtKind::While {
                    condition: cond,
                    body: Box::new(Stmt::new(StmtKind::Block(body), Span::empty(0))),
                },
                Span::empty(0),
            );
            return Some((vec![stmt], tail + 1));
        }

        // Fuse the header's short-circuit chain (`while (a && b)`,
        // `while (a || b)`) like `if` conditions do.
        let fused = self.fuse_conditions(index)?;
        if fused.is_value {
            return None;
        }
        let (prelude, mut cond) = (fused.prelude, fused.cond);
        // The body is the side whose region contains the back-edge tail:
        // the repo dialect's header is `jf body` (jump-if-true into the
        // body), the official dialect's is `jf exit` (jump-if-true out of
        // the loop), which also inverts the loop condition.
        let tail_in_then = fused.then_entry <= tail && tail < fused.else_entry;
        let (body_entry, exit) = if tail_in_then {
            (fused.then_entry, fused.else_entry)
        } else {
            cond = negate_condition(cond);
            (fused.else_entry, fused.then_entry)
        };

        // The post/step block is the loop tail itself (the step shares the
        // back-jump block) when a jump-only block targets it — the
        // `continue` trampoline shape. Non-empty body blocks jumping to the
        // tail are the body's natural end, and short-circuit chain
        // trampolines target their chain body, never the tail.
        let mut post_block = None;
        if tail > index
            && (0..self.blocks.len()).any(|source| {
                source != tail
                    && self.block_body(source).is_empty()
                    && matches!(
                        self.blocks[source].term,
                        Term::Jmp(target) if target == tail
                    )
            })
        {
            post_block = Some(tail);
        }

        let continue_target = post_block.unwrap_or(index);
        let body_end = post_block.unwrap_or(tail);
        let mut stop = BTreeSet::new();
        stop.insert(body_end);
        let (mut body, _) = self.seq(
            body_entry,
            &SeqCtx {
                stop,
                loop_ctx: Some(LoopCtx {
                    exit,
                    continue_target,
                }),
                suppress_loop_at_entry: false,
                suppress_try_at_entry: false,
            },
        );
        if post_block.is_none() {
            let tail_body = self.block_body(tail);
            self.scanner.scan_linear(&tail_body);
            body.extend(self.scanner.take_out());
        }
        let body_stmt = Stmt::new(StmtKind::Block(body), Span::empty(0));

        let stmt = match post_block {
            Some(post) => {
                let post_insts = self.block_body(post);
                let saved = self.scanner.materialize;
                self.scanner.materialize = false;
                self.scanner.scan_linear(&post_insts);
                self.scanner.materialize = saved;
                let post_out = self.scanner.take_out();
                let step = match post_out.as_slice() {
                    [
                        Stmt {
                            kind: StmtKind::Expr(expr),
                            ..
                        },
                    ] => Some(expr.clone()),
                    _ => None,
                };
                Stmt::new(
                    StmtKind::For {
                        init: None,
                        condition: Some(cond),
                        step,
                        body: Box::new(body_stmt),
                    },
                    Span::empty(0),
                )
            }
            None => Stmt::new(
                StmtKind::While {
                    condition: cond,
                    body: Box::new(body_stmt),
                },
                Span::empty(0),
            ),
        };
        let mut stmts = prelude;
        stmts.push(stmt);
        Some((stmts, tail + 1))
    }

    /// The anchor register when `index` is a switch pre-entry block whose
    /// last body instruction defines the anchor (`cp %anchor, %x`, or a
    /// direct `const` for literal discriminants) and the block at
    /// `test_candidate` (following Fall-only constant blocks) begins the
    /// `ceq %anchor, ...` test chain.
    fn switch_anchor_reg(&self, index: usize, test_candidate: usize) -> Option<i16> {
        let body = self.block_body(index);
        let last = body.last()?;
        if !matches!(last.opcode, 1 | 2) || last.operands[0] <= 0 {
            return None;
        }
        let anchor = last.operands[0];
        let mut test = test_candidate;
        loop {
            let first = self
                .block_body(test)
                .into_iter()
                .find(|inst| inst.opcode != 1);
            match first {
                Some(inst) if inst.opcode == 7 && inst.operands[0] == anchor => {
                    return Some(anchor);
                }
                None if matches!(self.blocks[test].term, Term::Fall) => test += 1,
                _ => return None,
            }
        }
    }

    /// Follows jump-only trampoline blocks in either direction (switch test
    /// chains can route through backward trampolines).
    fn skip_switch_trampolines(&self, mut index: usize) -> usize {
        let mut seen = BTreeSet::new();
        while index < self.blocks.len()
            && seen.insert(index)
            && let Term::Jmp(target) = self.blocks[index].term
            && target != usize::MAX
            && self.block_body(index).is_empty()
        {
            index = target;
        }
        index
    }

    /// Matches a `switch` construct starting at the first case test block.
    fn switch_construct(
        &mut self,
        test_block: usize,
        anchor: i16,
        ctx: &SeqCtx,
    ) -> Option<(Vec<Stmt>, usize)> {
        // Walk the test chain, collecting (case value, body entry). Leading
        // constant-only blocks hold the case value evaluations.
        let mut cases: Vec<(Expr, usize)> = Vec::new();
        let mut test = test_block;
        let terminal = loop {
            let mut eval_insts = Vec::new();
            while self.block_body(test).iter().all(|inst| inst.opcode == 1)
                && matches!(self.blocks[test].term, Term::Fall)
            {
                eval_insts.extend(self.block_body(test));
                test += 1;
                if test >= self.blocks.len() {
                    return None;
                }
            }
            let body_insts = self.block_body(test);
            let ceq = body_insts
                .iter()
                .find(|inst| inst.opcode == 7 && inst.operands[0] == anchor)?;
            let ceq = ceq.clone();
            eval_insts.extend(body_insts.iter().filter(|inst| inst.opcode != 8).cloned());
            self.scanner.scan_condition_block(&eval_insts);
            let value = self.scanner.reg_expr(ceq.operands[1]);
            let (body_entry, next_test) = match self.blocks[test].term {
                Term::Cond {
                    target,
                    jump_if_true: true,
                } => {
                    // Repo: `jf body` with a trampoline to the next test.
                    (target, self.skip_switch_trampolines(test + 1))
                }
                Term::Cond {
                    target,
                    jump_if_true: false,
                } => {
                    // Official: `jnf next`, the body falls through.
                    (test + 1, target)
                }
                _ => return None,
            };
            if body_entry >= self.blocks.len() || next_test >= self.blocks.len() {
                return None;
            }
            // The chain ends when the next test block does not compare the
            // anchor: it is the default body (or the exit). Jump-only
            // trampolines and constant-only blocks may sit in between.
            let mut next_real = self.skip_switch_trampolines(next_test);
            while self
                .block_body(next_real)
                .iter()
                .all(|inst| inst.opcode == 1)
                && matches!(self.blocks[next_real].term, Term::Fall)
            {
                next_real += 1;
                if next_real >= self.blocks.len() {
                    return None;
                }
            }
            let next_first = self
                .block_body(next_real)
                .into_iter()
                .find(|inst| inst.opcode != 1);
            if !matches!(next_first, Some(inst) if inst.opcode == 7 && inst.operands[0] == anchor) {
                cases.push((value, body_entry));
                break next_real;
            }
            cases.push((value, body_entry));
            test = next_real;
        };
        // The terminal block is either the default body (ending in a jump to
        // the exit) or the exit itself for a switch without default.
        let (default_body, exit) = match self.blocks[terminal].term {
            Term::Jmp(target) if target != usize::MAX => (Some(terminal), target),
            Term::Ret | Term::Throw => (None, terminal),
            Term::Fall => (Some(terminal), self.skip_switch_trampolines(terminal + 1)),
            _ => return None,
        };

        let discriminant = self.scanner.reg_expr(anchor);
        let mut switch_cases = Vec::with_capacity(cases.len() + 1);
        let entries = cases
            .iter()
            .map(|(_, body)| *body)
            .chain(default_body.into_iter())
            .collect::<Vec<_>>();
        for (index, (value, body_entry)) in cases.iter().enumerate() {
            let next_entry = entries.get(index + 1).copied().unwrap_or(exit);
            let mut stop = BTreeSet::from([next_entry, exit]);
            stop.remove(body_entry);
            let (mut body, end) = self.seq(
                *body_entry,
                &SeqCtx {
                    stop,
                    loop_ctx: ctx.loop_ctx,
                    suppress_loop_at_entry: false,
                    suppress_try_at_entry: false,
                },
            );
            // A jump to the exit is the case's `break`; TJS2 switch falls
            // through by default, so preserve it explicitly.
            match end {
                SeqEnd::StoppedAt(t) | SeqEnd::Jumped(t) if t == exit => {
                    body.push(Stmt::new(StmtKind::Break, Span::empty(0)));
                }
                SeqEnd::Returned => return None,
                _ => {}
            }
            switch_cases.push(syntax::SwitchCase {
                test: Some(value.clone()),
                body,
                span: Span::empty(0),
            });
        }
        if let Some(default_body) = default_body {
            let mut stop = BTreeSet::from([exit]);
            stop.remove(&default_body);
            let (default_stmts, _) = self.seq(
                default_body,
                &SeqCtx {
                    stop,
                    loop_ctx: ctx.loop_ctx,
                    suppress_loop_at_entry: false,
                    suppress_try_at_entry: false,
                },
            );
            switch_cases.push(syntax::SwitchCase {
                test: None,
                body: default_stmts,
                span: Span::empty(0),
            });
        }

        let stmt = Stmt::new(
            StmtKind::Switch {
                discriminant,
                cases: switch_cases,
            },
            Span::empty(0),
        );
        Some((vec![stmt], exit))
    }

    fn try_construct(&mut self, index: usize, ctx: &SeqCtx) -> Result<(Vec<Stmt>, usize), ()> {
        let entry_inst = self.block_insts(index)[0].clone();
        let catch_offset = entry_inst.offset as isize + isize::from(entry_inst.operands[0]);
        if catch_offset <= 0 {
            return Err(());
        }
        let catch_index = find_index(&self.instructions, catch_offset as usize);
        let catch = self
            .blocks
            .iter()
            .position(|block| block.start == catch_index)
            .ok_or(())?;

        // The try body normally sits between the entry and the catch, ending
        // in `extry; jmp end`. Compilers can split the body so the tail
        // block lands after the catch; search the whole rest of the code for
        // the first `extry` block in that case.
        let mut tail = None;
        for block in index..catch {
            if self
                .block_insts(block)
                .iter()
                .any(|inst| inst.opcode == 121)
            {
                tail = Some(block);
            }
        }
        if tail.is_none() {
            for block in catch..self.blocks.len() {
                if self
                    .block_insts(block)
                    .iter()
                    .any(|inst| inst.opcode == 121)
                {
                    tail = Some(block);
                    break;
                }
            }
        }
        let Some(tail) = tail else {
            return Err(());
        };
        let end = match self.blocks[tail].term {
            Term::Jmp(target) if target != usize::MAX && target != tail => target,
            _ => return Err(()),
        };

        let mut stop = BTreeSet::new();
        stop.insert(tail);
        let (mut body, _) = self.seq(
            index,
            &SeqCtx {
                stop,
                loop_ctx: ctx.loop_ctx,
                suppress_loop_at_entry: false,
                suppress_try_at_entry: true,
            },
        );
        let tail_body = self.block_body(tail);
        self.scanner.scan_linear(&tail_body);
        body.extend(self.scanner.take_out());

        // An empty catch collapses into the continuation: the handler address
        // equals the try tail's jump target, so there is no catch body to
        // scan and the construct resumes at `end`.
        if catch == end {
            let stmt = Stmt::new(
                StmtKind::Try {
                    body: Box::new(Stmt::new(StmtKind::Block(body), Span::empty(0))),
                    catch: Some(syntax::CatchClause {
                        binding: Some(Ident::new("e")),
                        body: Box::new(Stmt::new(StmtKind::Block(Vec::new()), Span::empty(0))),
                        span: Span::empty(0),
                    }),
                },
                Span::empty(0),
            );
            return Ok((vec![stmt], end));
        }

        let mut catch_insts = self.block_body(catch);
        let binding = catch_insts
            .first()
            .filter(|inst| inst.opcode == 2 && inst.operands[0] < 0)
            .map(|inst| {
                let reg = inst.operands[0];
                let name = self.scanner_name(reg);
                Ident::new(name)
            })
            .unwrap_or_else(|| Ident::new("e"));
        if catch_insts
            .first()
            .is_some_and(|inst| inst.opcode == 2 && inst.operands[0] < 0)
        {
            catch_insts.remove(0);
        }
        self.scanner.scan_linear(&catch_insts);
        let mut catch_stmts = self.scanner.take_out();

        let mut catch_stop = BTreeSet::new();
        catch_stop.insert(end);
        let (rest, catch_end) = self.seq(
            catch + 1,
            &SeqCtx {
                stop: catch_stop,
                loop_ctx: ctx.loop_ctx,
                suppress_loop_at_entry: false,
                suppress_try_at_entry: false,
            },
        );
        catch_stmts.extend(rest);
        // The catch may end by reaching the continuation (in layout order,
        // via a jump to it, or by returning/throwing); the try body still
        // reaches `end` via its tail jump.
        if !matches!(
            catch_end,
            SeqEnd::StoppedAt(block) | SeqEnd::Jumped(block) if block == end
        ) && !matches!(catch_end, SeqEnd::Returned)
        {
            return Err(());
        }

        let stmt = Stmt::new(
            StmtKind::Try {
                body: Box::new(Stmt::new(StmtKind::Block(body), Span::empty(0))),
                catch: Some(syntax::CatchClause {
                    binding: Some(binding),
                    body: Box::new(Stmt::new(StmtKind::Block(catch_stmts), Span::empty(0))),
                    span: Span::empty(0),
                }),
            },
            Span::empty(0),
        );
        Ok((vec![stmt], end))
    }

    fn scanner_name(&self, reg: i16) -> String {
        match reg {
            -1 | -2 => "this".to_string(),
            0 => "void".to_string(),
            r if r > 0 => format!("t{r}"),
            r => {
                let arg_count = self.object.func_decl_arg_count as usize;
                let frame_index = (-3 - r) as usize;
                if frame_index < arg_count {
                    format!("a{frame_index}")
                } else {
                    format!("l{}", frame_index - arg_count)
                }
            }
        }
    }
}

struct FusedCondition {
    prelude: Vec<Stmt>,
    cond: Expr,
    /// The block the construct's then branch starts at (trampolines skipped).
    then_entry: usize,
    /// The block after the construct (trampolines skipped).
    else_entry: usize,
    /// The fusion produced a boolean value expression (not an `if`).
    is_value: bool,
}

fn raw_operand_expr(scanner: &Scanner<'_>, raw: Cond) -> Expr {
    match raw {
        Cond::Truthy { reg, .. } => scanner.reg_expr(reg),
        Cond::Compare { .. } => cond_expr(raw, false, |reg| scanner.reg_expr(reg)),
    }
}

/// The boolean negation of a condition expression (unwraps an existing
/// `!` instead of double-negating).
fn negate_condition(cond: Expr) -> Expr {
    match cond.kind {
        ExprKind::Unary {
            op: syntax::UnaryOp::LogicalNot,
            expr,
        } => *expr,
        _ => Expr::new(
            ExprKind::Unary {
                op: syntax::UnaryOp::LogicalNot,
                expr: Box::new(cond),
            },
            Span::empty(0),
        ),
    }
}

pub(crate) fn decompile_body(file: &BytecodeFile, object: &CodeObject) -> BodyOutput {
    let instructions = match object.decode_instructions() {
        Ok(instructions) => instructions,
        Err(error) => {
            let _ = error;
            let marker = Stmt::new(
                StmtKind::Expr(Expr::new(
                    ExprKind::Identifier(Ident::new(super::stmt::unhandled_marker(
                        "cannot decode instructions",
                    ))),
                    Span::empty(0),
                )),
                Span::empty(0),
            );
            return BodyOutput {
                statements: vec![marker],
                unhandled: 1,
            };
        }
    };
    if instructions.is_empty() {
        return BodyOutput {
            statements: Vec::new(),
            unhandled: 0,
        };
    }
    let blocks = build_blocks(&instructions);
    BodyDecompiler::new(file, object, instructions, blocks).run()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::compile_source_to_bytecode;
    use crate::frontend::printer::print_statements;

    fn decompile(source: &str) -> (String, usize) {
        let file = compile_source_to_bytecode("control.tjs", source).expect("compile");
        let object = &file.objects[file.top_level.expect("top level")];
        let output = decompile_body(&file, object);
        (print_statements(&output.statements), output.unhandled)
    }

    #[test]
    fn decompiles_if_else() {
        let (text, unhandled) = decompile("if (a) { b(); } else { c(); } d();");
        assert_eq!(unhandled, 0, "{text}");
        assert!(text.contains("if (a) {"), "{text}");
        assert!(text.contains("} else {"), "{text}");
        assert!(text.contains("d();"), "{text}");
    }

    #[test]
    fn decompiles_while_and_do_while() {
        let (text, unhandled) =
            decompile("var i = 0; while (i < 3) { i++; } do { i--; } while (i > 0); return i;");
        assert_eq!(unhandled, 0, "{text}");
        assert!(text.contains("while (i < 3)"), "{text}");
        assert!(text.contains("do {"), "{text}");
    }

    #[test]
    fn decompiles_for_loop() {
        let (text, unhandled) =
            decompile("var s = 0; for (var j = 0; j < 3; j++) { s += j; } return s;");
        assert_eq!(unhandled, 0, "{text}");
        assert!(text.contains("while (j < 3)"), "{text}");
    }

    #[test]
    fn decompiles_short_circuit_conditions() {
        let (text, unhandled) = decompile("if (a && b) { f(); } if (c || d) { g(); }");
        assert_eq!(unhandled, 0, "{text}");
        assert!(text.contains("a && b"), "{text}");
        assert!(text.contains("c || d"), "{text}");
    }

    #[test]
    fn decompiles_short_circuit_values() {
        let (text, unhandled) = decompile("var x = a && b; var y = c || d; return x;");
        assert_eq!(unhandled, 0, "{text}");
        assert!(text.contains("a && b"), "{text}");
        assert!(text.contains("c || d"), "{text}");
    }

    #[test]
    fn decompiles_break_and_continue() {
        let (text, unhandled) =
            decompile("var i = 0; while (i < 9) { i++; if (i > 3) { break; } }");
        assert_eq!(unhandled, 0, "{text}");
        assert!(text.contains("break;"), "{text}");
        let (text, unhandled) = decompile(
            "var s = 0; for (var j = 0; j < 5; j++) { if (j == 2) { continue; } s += j; } return s;",
        );
        assert_eq!(unhandled, 0, "{text}");
        assert!(text.contains("continue;"), "{text}");
    }
}
