//! M3.5 test-only AST oracle + differential harness helpers.
//!
//! Never compiled into non-test builds (`#[cfg(test)] mod oracle` in
//! `lib.rs`); never `pub` outside tests; never used outside tests.
//!
//! The oracle is a direct tree-walk evaluator over `&ParseResult`
//! (AST + span/index tables in lockstep, mirroring lowering) producing
//! plain [`Value`]s for the M2 stable subset. The differential harness
//! (unit tests at the bottom of this file) asserts the typed-IR
//! interpreter agrees with the oracle on every
//! `tests/fixtures/m3/exec/valid/*.aion` fixture, on value *and* on
//! return-ownership behavior (fresh owned call results, no observable
//! aliasing), plus trap code+span agreement on `exec/traps/`.

use super::{find_entry, Config, Env, State, Value};
use aion_error::{Diagnostic, ErrorCode};
use aion_sem_model::ScopeTree;
use aion_span::{ByteSpan, FileId};
use aion_syntax::{
    BinOp, Block, BlockIds, Expr, ExprIds, ExprIdsKind, FnDecl, FnIds, ParseResult, ProgramIds,
    Stmt, StmtIds, UnOp,
};
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// Test-only main-body evaluator over the REAL interpreter state
// ---------------------------------------------------------------------------
// `execute` discards `main`'s body value (program result is always `Unit`).
// The differential harness compares pre-discard body values, so this helper
// drives the same `State`/`Env` path as `execute` but returns the body
// value (and fuel used) instead of discarding it.

/// Evaluate `main`'s body with the real interpreter; return body value.
///
/// `Err` carries the entry fault or the runtime trap, exactly as
/// [`super::execute`] would report it.
#[allow(clippy::result_large_err)]
pub(crate) fn ir_main_value(
    module: &aion_ir::ValidatedModule,
    config: &Config,
) -> Result<(Value, u64), Diagnostic> {
    let entry = find_entry(module)?;
    let main = module.module().functions.get(entry).ok_or_else(|| {
        Diagnostic::new(
            ErrorCode::InternalIrInvalid,
            "internal interpreter error: entry index out of bounds",
        )
    })?;
    let mut st = State {
        fuel: config.fuel,
        used: 0,
        frames: config.frames,
        depth: 0,
    };
    let mut env = Env::new();
    match st.eval_block(&main.body, &mut env, module) {
        Ok(v) => Ok((v, st.used)),
        Err(d) => Err(d),
    }
}

// ---------------------------------------------------------------------------
// AST oracle
// ---------------------------------------------------------------------------

struct Oracle {
    fuel: u64,
    used: u64,
    frames: usize,
    depth: usize,
}

impl Oracle {
    #[allow(clippy::result_large_err)]
    fn charge(&mut self, span: ByteSpan) -> Result<(), Diagnostic> {
        if self.used >= self.fuel {
            return Err(
                Diagnostic::new(ErrorCode::FuelExhausted, "fuel limit exceeded").with_span(span),
            );
        }
        self.used += 1;
        Ok(())
    }
}

/// Test-only block-to-scope map for hand-built models: assigns a fresh
/// [`ScopeId`](aion_sem_model::ScopeId) to every block reachable from
/// function bodies, mirroring the checker's walk (nested `Stmt::Block`
/// recursion; intent bodies excluded). Lets tests that bypass sema
/// satisfy the fail-closed lowering gate with explicit, deterministic
/// scopes. This mirrors Stage 1's published mapping; it is test
/// scaffolding, not a second authority — production models always
/// come from sema.
pub(crate) fn fixture_block_scopes(
    parse: &ParseResult,
    tree: &mut ScopeTree,
    root: aion_sem_model::ScopeId,
) -> HashMap<aion_syntax::NodeId, aion_sem_model::ScopeId> {
    fn go(
        b: &aion_syntax::Block,
        bids: &aion_syntax::BlockIds,
        out: &mut HashMap<aion_syntax::NodeId, aion_sem_model::ScopeId>,
        tree: &mut ScopeTree,
        parent: aion_sem_model::ScopeId,
    ) {
        let scope = tree.fork(parent).expect("fixture block scope");
        out.insert(bids.node, scope);
        for (st, sid) in b.stmts.iter().zip(bids.stmts.iter()) {
            if let (aion_syntax::Stmt::Block(inner), aion_syntax::StmtIds::Block { body, .. }) =
                (st, sid)
            {
                go(inner, body, out, tree, scope);
            }
        }
    }
    let mut out = HashMap::new();
    for (item, iids) in parse.program.items.iter().zip(parse.index.items.iter()) {
        if let (aion_syntax::Item::Fn(f), aion_syntax::ItemIds::Fn(fids)) = (item, iids) {
            let function_scope = tree.fork(root).expect("fixture function scope");
            go(&f.body, &fids.body, &mut out, tree, function_scope);
        }
    }
    out
}

/// Evaluate a whole parsed program: the value of `main`'s body.
///
/// Walks the AST with the parallel id tables in lockstep (same traversal
/// as lowering, so trap spans match the IR interpreter exactly). Move
/// semantics mirror the access-authority rule: a `Str` local moves only at
/// call-argument, `let`-initializer, or trailing-return position; every
/// other use is a borrow (clone).
#[allow(clippy::result_large_err)]
pub(crate) fn eval_program(
    parse: &ParseResult,
    fuel: u64,
    frames: usize,
) -> Result<(Value, u64), Diagnostic> {
    let mut fns: HashMap<&str, (&FnDecl, &FnIds)> = HashMap::new();
    for (item, iids) in parse.program.items.iter().zip(parse.index.items.iter()) {
        if let (aion_syntax::Item::Fn(f), aion_syntax::ItemIds::Fn(fids)) = (item, iids) {
            fns.entry(f.name.as_str()).or_insert((f, fids));
        }
    }
    let (main, main_ids) = fns.get("main").ok_or_else(|| {
        Diagnostic::new(
            ErrorCode::EntryPointInvalid,
            "missing entry: no zero-parameter `fn main`",
        )
    })?;
    if !main.params.is_empty() {
        let mut d = Diagnostic::new(
            ErrorCode::EntryPointInvalid,
            "entry `main` must take no parameters",
        );
        if let Some(sp) = first_span(parse, &main.body, &main_ids.body) {
            d = d.with_span(sp);
        }
        return Err(d);
    }
    let mut ox = Oracle {
        fuel,
        used: 0,
        frames,
        depth: 0,
    };
    let mut env: Vec<HashMap<String, Option<Value>>> = vec![HashMap::new()];
    let v = ox.eval_block(parse, &main.body, &main_ids.body, true, &fns, &mut env)?;
    Ok((v, ox.used))
}

fn first_span(parse: &ParseResult, block: &Block, bids: &BlockIds) -> Option<ByteSpan> {
    for (s, sid) in block.stmts.iter().zip(bids.stmts.iter()) {
        match (s, sid) {
            (Stmt::Let { .. }, StmtIds::Let { expr, .. }) => {
                return parse.spans.get(expr.node);
            }
            (Stmt::Do(e), StmtIds::Do { expr: eids, .. }) => {
                return expr_span(parse, e, eids);
            }
            (Stmt::Block(b), StmtIds::Block { body, .. }) => {
                if let Some(sp) = first_span(parse, b, body) {
                    return Some(sp);
                }
            }
            _ => {}
        }
    }
    None
}

fn expr_span(parse: &ParseResult, e: &Expr, ids: &ExprIds) -> Option<ByteSpan> {
    let _ = e;
    parse.spans.get(ids.node)
}

impl Oracle {
    fn span(&self, parse: &ParseResult, id: aion_syntax::NodeId) -> ByteSpan {
        parse.spans.get(id).unwrap_or(ByteSpan::empty(FileId(0), 0))
    }

    #[allow(clippy::result_large_err)]
    fn eval_block(
        &mut self,
        parse: &ParseResult,
        block: &Block,
        bids: &BlockIds,
        is_fn_body: bool,
        fns: &HashMap<&str, (&FnDecl, &FnIds)>,
        env: &mut Vec<HashMap<String, Option<Value>>>,
    ) -> Result<Value, Diagnostic> {
        env.push(HashMap::new());
        let mut trailing: Option<Value> = None;
        let n = block.stmts.len();
        for (i, (stmt, sid)) in block.stmts.iter().zip(bids.stmts.iter()).enumerate() {
            if block.stmts.len() != bids.stmts.len() {
                break;
            }
            let last = i + 1 == n;
            match (stmt, sid) {
                (Stmt::Let { name, expr }, StmtIds::Let { expr: eids, .. }) => {
                    let (v, _) = self.eval_expr(parse, expr, eids, true, fns, env)?;
                    if let Some(top) = env.last_mut() {
                        top.insert(name.clone(), Some(v));
                    }
                }
                (Stmt::Do(e), StmtIds::Do { expr: eids, .. }) => {
                    let (v, _) = self.eval_expr(parse, e, eids, last && is_fn_body, fns, env)?;
                    if last {
                        trailing = Some(v);
                    }
                }
                (
                    Stmt::Block(inner),
                    StmtIds::Block {
                        body: inner_ids, ..
                    },
                ) => {
                    let v = self.eval_block(parse, inner, inner_ids, false, fns, env)?;
                    if last {
                        trailing = Some(v);
                    }
                }
                (
                    Stmt::Assert {
                        expr: e,
                        provenance,
                    },
                    StmtIds::Assert { node, expr: eids },
                ) => {
                    // M4.4: AST assertion semantics. Evaluate the operand;
                    // `true` succeeds silently, `false` fails through the
                    // oracle failure path with the original statement
                    // span and the assertion's provenance. Separate from
                    // the typed runtime evaluator: no trace recording
                    // happens here.
                    let (v, _) = self.eval_expr(parse, e, eids, false, fns, env)?;
                    match v {
                        Value::Bool(true) => {}
                        Value::Bool(false) => {
                            return Err(Diagnostic::new(
                                ErrorCode::RuntimeTrap,
                                "assertion failed",
                            )
                            .with_span(self.span(parse, *node))
                            .with_provenance(provenance.clone()));
                        }
                        _ => {
                            return Err(oracle_internal(
                                self.span(parse, *node),
                                "non-bool assert operand",
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
        env.pop();
        Ok(trailing.unwrap_or(Value::Unit))
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::result_large_err)]
    fn eval_expr(
        &mut self,
        parse: &ParseResult,
        e: &Expr,
        ids: &ExprIds,
        consuming: bool,
        fns: &HashMap<&str, (&FnDecl, &FnIds)>,
        env: &mut Vec<HashMap<String, Option<Value>>>,
    ) -> Result<(Value, ByteSpan), Diagnostic> {
        let node_sp = self.span(parse, ids.node);
        self.charge(node_sp)?;
        match e {
            Expr::Int(v) => Ok((Value::Int(*v), node_sp)),
            Expr::Str(s) => Ok((Value::Str(s.clone()), node_sp)),
            Expr::Bool(v) => Ok((Value::Bool(*v), node_sp)),
            Expr::Null => Ok((Value::Null, node_sp)),
            Expr::Ident(name) => {
                if name == "null" {
                    return Ok((Value::Null, node_sp));
                }
                for scope in env.iter_mut().rev() {
                    if let Some(slot) = scope.get_mut(name) {
                        match slot {
                            Some(v) => {
                                if consuming && matches!(v, Value::Str(_)) {
                                    let taken = slot.take().unwrap();
                                    return Ok((taken, node_sp));
                                }
                                return Ok((v.clone(), node_sp));
                            }
                            None => {
                                return Err(Diagnostic::new(
                                    ErrorCode::InternalIrInvalid,
                                    format!("internal interpreter error: unusable slot `{name}`"),
                                )
                                .with_span(node_sp));
                            }
                        }
                    }
                }
                Err(Diagnostic::new(
                    ErrorCode::InternalIrInvalid,
                    format!("internal interpreter error: unbound name `{name}`"),
                )
                .with_span(node_sp))
            }
            Expr::Unary { op, rhs } => {
                let rhs_ids = match &ids.kind {
                    ExprIdsKind::Unary { rhs, .. } => rhs.as_ref(),
                    _ => {
                        return Err(Diagnostic::new(
                            ErrorCode::InternalIrInvalid,
                            "internal oracle error: unary/index kind mismatch",
                        )
                        .with_span(node_sp));
                    }
                };
                let op_sp = match &ids.kind {
                    ExprIdsKind::Unary { op, .. } => self.span(parse, *op),
                    _ => node_sp,
                };
                let (rv, rsp) = self.eval_expr(parse, rhs, rhs_ids, false, fns, env)?;
                let span = merge_sp(op_sp, rsp);
                Ok((self.eval_unary(*op, rv, span)?, span))
            }
            Expr::Binary { op, lhs, rhs } => {
                let (l_ids, r_ids) = match &ids.kind {
                    ExprIdsKind::Binary { lhs, rhs, .. } => (lhs.as_ref(), rhs.as_ref()),
                    _ => {
                        return Err(Diagnostic::new(
                            ErrorCode::InternalIrInvalid,
                            "internal oracle error: binary/index kind mismatch",
                        )
                        .with_span(node_sp));
                    }
                };
                match op {
                    BinOp::Or => {
                        let (lv, _) = self.eval_expr(parse, lhs, l_ids, false, fns, env)?;
                        match lv {
                            Value::Bool(true) => {
                                let span = self.binary_span(parse, l_ids, r_ids);
                                Ok((Value::Bool(true), span))
                            }
                            Value::Bool(false) => {
                                let (rv, rsp) =
                                    self.eval_expr(parse, rhs, r_ids, false, fns, env)?;
                                let span = merge_sp(self.span_of(parse, l_ids), rsp);
                                match rv {
                                    Value::Bool(b) => Ok((Value::Bool(b), span)),
                                    _ => Err(oracle_internal(node_sp, "non-bool `||` operand")),
                                }
                            }
                            _ => Err(oracle_internal(node_sp, "non-bool `||` operand")),
                        }
                    }
                    BinOp::And => {
                        let (lv, _) = self.eval_expr(parse, lhs, l_ids, false, fns, env)?;
                        match lv {
                            Value::Bool(false) => {
                                let span = self.binary_span(parse, l_ids, r_ids);
                                Ok((Value::Bool(false), span))
                            }
                            Value::Bool(true) => {
                                let (rv, rsp) =
                                    self.eval_expr(parse, rhs, r_ids, false, fns, env)?;
                                let span = merge_sp(self.span_of(parse, l_ids), rsp);
                                match rv {
                                    Value::Bool(b) => Ok((Value::Bool(b), span)),
                                    _ => Err(oracle_internal(node_sp, "non-bool `&&` operand")),
                                }
                            }
                            _ => Err(oracle_internal(node_sp, "non-bool `&&` operand")),
                        }
                    }
                    _ => {
                        let (lv, lsp) = self.eval_expr(parse, lhs, l_ids, false, fns, env)?;
                        let (rv, rsp) = self.eval_expr(parse, rhs, r_ids, false, fns, env)?;
                        let span = merge_sp(lsp, rsp);
                        Ok((self.eval_binary(*op, lv, rv, span)?, span))
                    }
                }
            }
            Expr::Call { callee, args } => {
                let (callee_id, arg_list_id, arg_ids) = match &ids.kind {
                    ExprIdsKind::Call {
                        callee,
                        arg_list,
                        args,
                    } => (*callee, *arg_list, args.clone()),
                    _ => {
                        return Err(Diagnostic::new(
                            ErrorCode::InternalIrInvalid,
                            "internal oracle error: call/index kind mismatch",
                        )
                        .with_span(node_sp));
                    }
                };
                let mut vals = Vec::with_capacity(args.len());
                for (a, a_ids) in args.iter().zip(arg_ids.iter()) {
                    let (v, _) = self.eval_expr(parse, a, a_ids, true, fns, env)?;
                    vals.push(v);
                }
                let span = merge_sp(self.span(parse, callee_id), self.span(parse, arg_list_id));
                let (fdecl, fids) = fns.get(callee.as_str()).ok_or_else(|| {
                    Diagnostic::new(
                        ErrorCode::InternalIrInvalid,
                        format!("internal oracle error: unknown callee `{callee}`"),
                    )
                    .with_span(span)
                })?;
                if vals.len() != fdecl.params.len() {
                    return Err(Diagnostic::new(
                        ErrorCode::InternalIrInvalid,
                        format!("internal oracle error: arity mismatch calling `{callee}`"),
                    )
                    .with_span(span));
                }
                if self.depth + 1 > self.frames {
                    return Err(Diagnostic::new(
                        ErrorCode::StackOverflow,
                        "call-frame limit exceeded",
                    )
                    .with_span(span));
                }
                self.depth += 1;
                let mut call_env: Vec<HashMap<String, Option<Value>>> = vec![HashMap::new()];
                for (p, v) in fdecl.params.iter().zip(vals.into_iter()) {
                    call_env[0].insert(p.clone(), Some(v));
                }
                let r = self.eval_block(parse, &fdecl.body, &fids.body, true, fns, &mut call_env);
                self.depth -= 1;
                Ok((r?, span))
            }
        }
    }

    fn span_of(&self, parse: &ParseResult, ids: &ExprIds) -> ByteSpan {
        self.span(parse, ids.node)
    }

    fn binary_span(&self, parse: &ParseResult, l: &ExprIds, r: &ExprIds) -> ByteSpan {
        merge_sp(self.span_of(parse, l), self.span_of(parse, r))
    }

    #[allow(clippy::result_large_err)]
    fn eval_unary(&self, op: UnOp, v: Value, span: ByteSpan) -> Result<Value, Diagnostic> {
        match (op, v) {
            (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
            (UnOp::Neg, Value::Int(i)) => match i.checked_neg() {
                Some(n) => Ok(Value::Int(n)),
                None => Err(oracle_overflow(span)),
            },
            _ => Err(oracle_internal(span, "bad unary operand")),
        }
    }

    #[allow(clippy::result_large_err)]
    fn eval_binary(
        &self,
        op: BinOp,
        l: Value,
        r: Value,
        span: ByteSpan,
    ) -> Result<Value, Diagnostic> {
        match (op, l, r) {
            (BinOp::Add, Value::Int(a), Value::Int(b)) => match a.checked_add(b) {
                Some(n) => Ok(Value::Int(n)),
                None => Err(oracle_overflow(span)),
            },
            (BinOp::Sub, Value::Int(a), Value::Int(b)) => match a.checked_sub(b) {
                Some(n) => Ok(Value::Int(n)),
                None => Err(oracle_overflow(span)),
            },
            (BinOp::Mul, Value::Int(a), Value::Int(b)) => match a.checked_mul(b) {
                Some(n) => Ok(Value::Int(n)),
                None => Err(oracle_overflow(span)),
            },
            (BinOp::Div, Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    return Err(oracle_divzero(span));
                }
                match a.checked_div(b) {
                    Some(n) => Ok(Value::Int(n)),
                    None => Err(oracle_overflow(span)),
                }
            }
            (BinOp::Mod, Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    return Err(oracle_divzero(span));
                }
                match a.checked_rem(b) {
                    Some(n) => Ok(Value::Int(n)),
                    None => Err(oracle_overflow(span)),
                }
            }
            (BinOp::Lt, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a < b)),
            (BinOp::Le, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a <= b)),
            (BinOp::Gt, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a > b)),
            (BinOp::Ge, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a >= b)),
            (BinOp::Eq, a, b) => Ok(Value::Bool(oracle_eq(&a, &b, span)?)),
            (BinOp::Ne, a, b) => Ok(Value::Bool(!oracle_eq(&a, &b, span)?)),
            _ => Err(oracle_internal(span, "bad binary operands")),
        }
    }
}

#[allow(clippy::result_large_err)]
fn oracle_eq(a: &Value, b: &Value, span: ByteSpan) -> Result<bool, Diagnostic> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Ok(x == y),
        (Value::Str(x), Value::Str(y)) => Ok(x == y),
        (Value::Bool(x), Value::Bool(y)) => Ok(x == y),
        (Value::Null, Value::Null) => Ok(true),
        (Value::Unit, Value::Unit) => Ok(true),
        _ => Err(oracle_internal(span, "`==` on mismatched values")),
    }
}

fn merge_sp(a: ByteSpan, b: ByteSpan) -> ByteSpan {
    ByteSpan {
        file: a.file,
        start: a.start.min(b.start),
        end: a.end.max(b.end),
    }
}

fn oracle_divzero(span: ByteSpan) -> Diagnostic {
    Diagnostic::new(ErrorCode::DivByZero, "integer division by zero").with_span(span)
}

fn oracle_overflow(span: ByteSpan) -> Diagnostic {
    Diagnostic::new(ErrorCode::IntOverflow, "integer overflow").with_span(span)
}

fn oracle_internal(span: ByteSpan, msg: &str) -> Diagnostic {
    Diagnostic::new(
        ErrorCode::InternalIrInvalid,
        format!("internal oracle error: {msg}"),
    )
    .with_span(span)
}

// ---------------------------------------------------------------------------
// Index walk: collect every NodeId for the span-chain test
// ---------------------------------------------------------------------------

/// Every `NodeId` in the parse index (mirrors the AST shape).
pub(crate) fn all_node_ids(index: &ProgramIds) -> Vec<aion_syntax::NodeId> {
    let mut out = Vec::new();
    out.push(index.node);
    for item in &index.items {
        match item {
            aion_syntax::ItemIds::Fn(f) => {
                out.push(f.node);
                out.extend(f.params.iter().copied());
                block_ids(&f.body, &mut out);
            }
            aion_syntax::ItemIds::Intent(i) => {
                out.push(i.node);
                block_ids(&i.body, &mut out);
            }
        }
    }
    out
}

fn block_ids(b: &BlockIds, out: &mut Vec<aion_syntax::NodeId>) {
    out.push(b.node);
    for s in &b.stmts {
        match s {
            StmtIds::Let { node, name, expr } => {
                out.push(*node);
                out.push(*name);
                expr_ids(expr, out);
            }
            StmtIds::Do { node, expr } => {
                out.push(*node);
                expr_ids(expr, out);
            }
            StmtIds::Block { node, body } => {
                out.push(*node);
                block_ids(body, out);
            }
            StmtIds::Goal { node: n, .. } => out.push(*n),
            StmtIds::Assert { node, expr } => {
                out.push(*node);
                expr_ids(expr, out);
            }
            StmtIds::Empty(n) => out.push(*n),
        }
    }
}

fn expr_ids(e: &ExprIds, out: &mut Vec<aion_syntax::NodeId>) {
    out.push(e.node);
    match &e.kind {
        ExprIdsKind::Leaf => {}
        ExprIdsKind::Unary { op, rhs } => {
            out.push(*op);
            expr_ids(rhs, out);
        }
        ExprIdsKind::Binary { op, lhs, rhs } => {
            out.push(*op);
            expr_ids(lhs, out);
            expr_ids(rhs, out);
        }
        ExprIdsKind::Call {
            callee,
            arg_list,
            args,
        } => {
            out.push(*callee);
            out.push(*arg_list);
            for a in args {
                expr_ids(a, out);
            }
        }
    }
}

/// The set of parser-recorded spans (for the span-chain test: lowering
/// must never invent a span).
pub(crate) fn recorded_spans(parse: &ParseResult) -> HashSet<(u32, u32)> {
    let mut set = HashSet::new();
    for id in all_node_ids(&parse.index) {
        if let Some(sp) = parse.spans.get(id) {
            set.insert((sp.start, sp.end));
        }
    }
    set
}

#[cfg(test)]
mod harness {
    //! Differential harness: `execute`-IR vs AST-oracle over the m3 corpus.
    use super::*;
    use aion_sem_model::{ScopeTree, Symbol, SymbolKind, Ty};
    use aion_span::FileId;

    fn fixture(rel: &str) -> Vec<u8> {
        let path = format!(
            "{}/../../tests/fixtures/m3/{rel}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
    }

    fn parse_clean(rel: &str) -> (String, ParseResult) {
        let src = fixture(rel);
        let text = String::from_utf8_lossy(&src).into_owned();
        let r = aion_syntax::parse(FileId(0), &src);
        assert!(
            r.errors.is_empty(),
            "{rel} must parse clean: {:?}",
            r.errors
        );
        (text, r)
    }

    /// Hand-built `SemModel`: the scope-tree function signatures the
    /// lowering pre-pass needs (`name -> Ty::Fn`). Move facts are not
    /// stored here; `lowered` copies them from semantic analysis.
    fn model_for(sigs: &[(&str, Vec<Ty>, Ty)]) -> aion_sem_model::SemModel {
        let mut tree = ScopeTree::new();
        let root = tree.root().expect("fixture root allocation");
        for (name, params, ret) in sigs {
            tree.define(
                root,
                Symbol {
                    kind: SymbolKind::Fn,
                    name: name.to_string(),
                    ty: Ty::Fn {
                        params: params.clone(),
                        ret: Box::new(ret.clone()),
                    },
                    span: ByteSpan::empty(FileId(0), 0),
                    moved: false,
                },
            );
        }
        aion_sem_model::SemModel {
            items: Vec::new(),
            scope_tree: tree,
            types: HashMap::new(),
            moved_uses: HashSet::new(),
            ..Default::default()
        }
    }

    /// Lower + validate; both must be clean for valid fixtures.
    fn lowered(
        rel: &str,
        sigs: &[(&str, Vec<Ty>, Ty)],
    ) -> (String, ParseResult, aion_ir::ValidatedModule) {
        let (text, r) = parse_clean(rel);
        let mut model = model_for(sigs);
        // Signatures stay hand-built. Block scopes stay the deterministic
        // test map. Move facts come only from semantic analysis on this
        // same parse.
        let root = model
            .scope_tree
            .scopes
            .iter()
            .position(|s| s.parent.is_none())
            .map(|i| aion_sem_model::ScopeId(i as u32))
            .expect("fixture module root");
        model.block_scopes = super::fixture_block_scopes(&r, &mut model.scope_tree, root);
        let (sema_diags, sema_model) = aion_sema::check_with_model(&r, FileId(0), text.as_bytes());
        assert!(
            sema_diags.is_empty(),
            "{rel} sema must be clean: {:?}",
            sema_diags
                .iter()
                .map(|d| d.render_human())
                .collect::<Vec<_>>()
        );
        model.moved_uses = sema_model.moved_uses;
        let module = aion_ir::lower(&r, &model, FileId(0), text.len() as u32)
            .unwrap_or_else(|diags| panic!("{rel} lower must be clean: {diags:?}"));
        let module = aion_ir::validate(&module).unwrap_or_else(|vdiags| {
            panic!(
                "{rel} validate must be clean: {:?}",
                vdiags.iter().map(|d| d.render_human()).collect::<Vec<_>>()
            )
        });
        (text, r, module)
    }

    /// Minimal reader for the golden `{"value": ...}` sidecar shape
    /// documented in `tests/fixtures/m3/README.md`.
    fn golden(rel: &str) -> Value {
        let raw = fixture(&format!("{rel}.expected.json"));
        let text = String::from_utf8_lossy(&raw).into_owned();
        let t = text.trim();
        assert!(t.starts_with("{\"value\":"), "{rel} golden shape: {t:?}");
        let inner = t["{\"value\":".len()..].trim().trim_end_matches('}').trim();
        if let Some(rest) = inner.strip_prefix("{\"Int\":") {
            return Value::Int(rest.trim().trim_end_matches('}').trim().parse().unwrap());
        }
        if let Some(rest) = inner.strip_prefix("{\"Str\":") {
            let s = rest.trim().trim_end_matches('}').trim();
            assert!(
                s.starts_with('"') && s.ends_with('"'),
                "{rel} golden: {t:?}"
            );
            return Value::Str(unesc(&s[1..s.len() - 1]));
        }
        if let Some(rest) = inner.strip_prefix("{\"Bool\":") {
            let b = rest.trim().trim_end_matches('}').trim();
            return Value::Bool(b == "true");
        }
        if inner == "\"Null\"" {
            return Value::Null;
        }
        if inner == "\"Unit\"" {
            return Value::Unit;
        }
        panic!("{rel} golden: unrecognized shape {t:?}");
    }

    fn unesc(s: &str) -> String {
        let mut out = String::new();
        let mut it = s.chars();
        while let Some(c) = it.next() {
            if c == '\\' {
                match it.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some(x) => out.push(x),
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    const I: Ty = Ty::Int;
    const S: Ty = Ty::Str;
    const B: Ty = Ty::Bool;
    const U: Ty = Ty::Unit;

    type Sig<'a> = (&'a str, Vec<Ty>, Ty);

    fn sig_main(ret: Ty) -> (&'static str, Vec<Ty>, Ty) {
        ("main", vec![], ret)
    }

    /// (fixture, signatures, golden source for failure messages).
    fn valid_cases() -> Vec<(&'static str, Vec<Sig<'static>>)> {
        vec![
            ("exec/valid/arith.aion", vec![sig_main(I)]),
            ("exec/valid/precedence.aion", vec![sig_main(B)]),
            (
                "exec/valid/calls.aion",
                vec![("add", vec![I, I], I), ("neg", vec![I], I), sig_main(I)],
            ),
            ("exec/valid/shadowing.aion", vec![sig_main(B)]),
            (
                "exec/valid/moves.aion",
                vec![("id", vec![S], S), sig_main(S)],
            ),
            (
                "exec/valid/return_str.aion",
                vec![("mk", vec![], S), sig_main(S)],
            ),
            ("exec/valid/nested.aion", vec![sig_main(I)]),
            ("exec/valid/unary_bool.aion", vec![sig_main(B)]),
            ("exec/valid/cmp_eq.aion", vec![sig_main(B)]),
            (
                "exec/valid/fresh_calls.aion",
                vec![("mk", vec![], S), sig_main(B)],
            ),
        ]
    }

    #[test]
    fn differential_value_agrees_with_golden() {
        for (rel, sigs) in valid_cases() {
            let (_text, parse, module) = lowered(rel, &sigs);
            let (ir_v, _ir_steps) = ir_main_value(&module, &Config::default())
                .unwrap_or_else(|d| panic!("{rel} IR trapped: {}", d.render_human()));
            let (ox_v, _ox_steps) = eval_program(&parse, 1_000_000, 1024)
                .unwrap_or_else(|d| panic!("{rel} oracle trapped: {}", d.render_human()));
            assert_eq!(ir_v, ox_v, "{rel}: IR vs oracle value mismatch");
            assert_eq!(ir_v, golden(rel), "{rel}: value vs golden mismatch");
        }
    }

    #[test]
    fn differential_return_ownership_fresh_no_aliasing() {
        // `fresh_calls`: two calls to the same `Str`-returning fn must
        // yield equal but independent owned values; `==` borrows both.
        let sigs = vec![("mk", vec![], S), sig_main(B)];
        let (_text, parse, module) = lowered("exec/valid/fresh_calls.aion", &sigs);
        let (ir_v, _) = ir_main_value(&module, &Config::default()).expect("IR");
        let (ox_v, _) = eval_program(&parse, 1_000_000, 1024).expect("oracle");
        assert_eq!(ir_v, Value::Bool(true));
        assert_eq!(ox_v, Value::Bool(true));
        // Run twice: fresh owned results each time, no aliasing observable.
        let (ir_v2, _) = ir_main_value(&module, &Config::default()).expect("IR rerun");
        assert_eq!(
            ir_v, ir_v2,
            "call results must be fresh and equal, never aliased"
        );
        // `moves`: the moved-from slot is unusable afterwards — both
        // sides agree the program still yields the moved value once.
        let msigs = vec![("id", vec![S], S), sig_main(S)];
        let (_t, p2, m2) = lowered("exec/valid/moves.aion", &msigs);
        let (a, _) = ir_main_value(&m2, &Config::default()).expect("IR moves");
        let (b, _) = eval_program(&p2, 1_000_000, 1024).expect("oracle moves");
        assert_eq!((a, b), (Value::Str("hi".into()), Value::Str("hi".into())));
    }

    #[test]
    fn differential_traps_code_and_span() {
        let cases: &[(&str, Vec<Sig<'_>>, ErrorCode, &str, Config)] = &[
            (
                "exec/traps/divzero.aion",
                vec![sig_main(I)],
                ErrorCode::DivByZero,
                "1 / 0",
                Config::default(),
            ),
            (
                "exec/traps/overflow.aion",
                vec![sig_main(I)],
                ErrorCode::IntOverflow,
                "9223372036854775807 + 1",
                Config::default(),
            ),
            (
                "exec/traps/fuel.aion",
                vec![sig_main(I)],
                ErrorCode::FuelExhausted,
                "1",
                Config {
                    fuel: 3,
                    frames: 1024,
                },
            ),
            (
                "exec/traps/recursion.aion",
                vec![("ping", vec![], U), sig_main(U)],
                ErrorCode::StackOverflow,
                "ping()",
                Config {
                    fuel: 1_000_000,
                    frames: 16,
                },
            ),
        ];
        for (rel, sigs, code, anchor, cfg) in cases {
            let (text, parse, module) = lowered(rel, sigs);
            let ir_d = ir_main_value(&module, cfg)
                .err()
                .unwrap_or_else(|| panic!("{rel}: expected trap {}", code.code_str()));
            let ox_d = eval_program(&parse, cfg.fuel, cfg.frames)
                .err()
                .unwrap_or_else(|| panic!("{rel}: oracle expected trap {}", code.code_str()));
            assert_eq!(ir_d.code, *code, "{rel}: IR code");
            assert_eq!(ox_d.code, *code, "{rel}: oracle code");
            assert_eq!(ir_d.code, ox_d.code, "{rel}: code agreement");
            let isp = ir_d.span.expect("IR trap span");
            let osp = ox_d.span.expect("oracle trap span");
            assert_eq!(
                (isp.start, isp.end),
                (osp.start, osp.end),
                "{rel}: IR vs oracle span"
            );
            let slice = &text[isp.start as usize..isp.end as usize];
            assert_eq!(slice, *anchor, "{rel}: trap span slices {slice:?}");
        }
    }

    #[test]
    fn determinism_consecutive_runs_byte_identical() {
        for (rel, sigs) in valid_cases() {
            let (_text, parse, module) = lowered(rel, &sigs);
            let mut seen = Vec::new();
            for _ in 0..4 {
                let (v, steps) = ir_main_value(&module, &Config::default())
                    .unwrap_or_else(|d| panic!("{rel}: {}", d.render_human()));
                let (ov, osteps) = eval_program(&parse, 1_000_000, 1024).expect("oracle");
                seen.push(format!("{v:?}|{steps}|{ov:?}|{osteps}"));
            }
            for w in seen.windows(2) {
                assert_eq!(w[0], w[1], "{rel}: runs differ");
            }
        }
    }

    #[test]
    fn span_chain_lowering_invents_no_spans() {
        let mut rels: Vec<(&str, Vec<Sig<'_>>)> = valid_cases();
        rels.push(("lower_valid/empty_main.aion", vec![sig_main(U)]));
        rels.push(("lower_valid/nested_blocks.aion", vec![sig_main(I)]));
        rels.push(("lower_valid/shadow_shape.aion", vec![sig_main(S)]));
        rels.push(("exec/traps/divzero.aion", vec![sig_main(I)]));
        rels.push(("exec/traps/overflow.aion", vec![sig_main(I)]));
        rels.push((
            "exec/traps/recursion.aion",
            vec![("ping", vec![], U), sig_main(U)],
        ));
        for (rel, sigs) in rels {
            let (text, parse, module) = lowered(rel, &sigs);
            let recorded = recorded_spans(&parse);
            assert!(!recorded.is_empty(), "{rel}: parser recorded no spans");
            let mut count = 0;
            for f in &module.module().functions {
                check_block_spans(&f.body, &text, &recorded, rel, &mut count);
            }
            // Every expression span slices real source (span equality
            // against the source bytes, stage by stage).
            let _ = count;
        }
    }

    fn check_block_spans(
        block: &aion_ir::TypedBlock,
        text: &str,
        recorded: &HashSet<(u32, u32)>,
        rel: &str,
        count: &mut usize,
    ) {
        for stmt in &block.stmts {
            match stmt {
                aion_ir::TypedStmt::Let { init, .. } => {
                    check_expr_spans(init, text, recorded, rel, count)
                }
                aion_ir::TypedStmt::Do { expr } => {
                    check_expr_spans(expr, text, recorded, rel, count)
                }
                aion_ir::TypedStmt::Assert { expr, .. } => {
                    check_expr_spans(expr, text, recorded, rel, count)
                }
                // Folded false carries no operand expression.
                aion_ir::TypedStmt::AssertFalse { .. } => {}
                aion_ir::TypedStmt::Block { body } => {
                    check_block_spans(body, text, recorded, rel, count)
                }
                aion_ir::TypedStmt::Empty => {}
            }
        }
    }

    fn check_expr_spans(
        e: &aion_ir::TypedExpr,
        text: &str,
        recorded: &HashSet<(u32, u32)>,
        rel: &str,
        count: &mut usize,
    ) {
        *count += 1;
        let sp = e.span;
        assert!(
            (sp.start as usize) <= (sp.end as usize) && (sp.end as usize) <= text.len(),
            "{rel}: span out of bounds {sp:?}"
        );
        assert!(
            recorded.contains(&(sp.start, sp.end)),
            "{rel}: IR span {sp:?} not parser-recorded (slice {:?})",
            &text[sp.start as usize..sp.end as usize]
        );
        assert!(
            !text[sp.start as usize..sp.end as usize].is_empty(),
            "{rel}: empty slice at {sp:?}"
        );
        match &e.kind {
            aion_ir::ExprKind::Unary { rhs, .. } => {
                check_expr_spans(rhs, text, recorded, rel, count)
            }
            aion_ir::ExprKind::Binary { lhs, rhs, .. } => {
                check_expr_spans(lhs, text, recorded, rel, count);
                check_expr_spans(rhs, text, recorded, rel, count);
            }
            aion_ir::ExprKind::Call { args, .. } => {
                for a in args {
                    check_expr_spans(a, text, recorded, rel, count);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn lower_valid_shapes_execute_unit() {
        for (rel, sigs) in [
            ("lower_valid/empty_main.aion", vec![sig_main(U)]),
            ("lower_valid/nested_blocks.aion", vec![sig_main(I)]),
            ("lower_valid/shadow_shape.aion", vec![sig_main(S)]),
        ] {
            let (_text, _parse, module) = lowered(rel, &sigs);
            match super::super::execute(&module, &Config::default()) {
                super::super::RunResult::Ok(o) => assert_eq!(o.value, Value::Unit, "{rel}"),
                super::super::RunResult::Trap(d) => panic!("{rel} trapped: {}", d.render_human()),
            }
        }
    }

    /// Parse inline source with assertions enabled (gated syntax).
    fn parse_assert(src: &str) -> aion_syntax::ParseResult {
        let r = aion_syntax::parse_with_flags(
            FileId(0),
            src.as_bytes(),
            aion_syntax::FeatureFlags::all_experimental(),
        );
        assert!(
            r.errors.is_empty(),
            "assert source must parse clean: {src:?}: {:?}",
            r.errors
                .iter()
                .map(|d| d.render_human())
                .collect::<Vec<_>>()
        );
        r
    }

    /// Recorded statement span of the first assert in the program.
    fn first_assert_span(parse: &aion_syntax::ParseResult) -> aion_span::ByteSpan {
        for item in &parse.index.items {
            if let aion_syntax::ItemIds::Fn(fids) = item {
                for sid in &fids.body.stmts {
                    if let aion_syntax::StmtIds::Assert { node, .. } = sid {
                        return parse.spans.get(*node).expect("assert stmt span recorded");
                    }
                }
            }
        }
        panic!("no assert statement in index");
    }

    #[test]
    fn oracle_assert_true_succeeds() {
        for src in ["fn main() { assert true; }", "fn main() { assert 1 == 1; }"] {
            let parse = parse_assert(src);
            let (v, _) = eval_program(&parse, 1_000_000, 1024)
                .unwrap_or_else(|d| panic!("{src}: oracle trapped: {}", d.render_human()));
            assert_eq!(v, Value::Unit, "{src}: assert success yields nothing");
        }
    }

    #[test]
    fn oracle_assert_false_fails_with_statement_span() {
        for src in [
            "fn main() { assert false; }",
            "fn main() { assert 1 == 2; }",
        ] {
            let parse = parse_assert(src);
            let expected = first_assert_span(&parse);
            let d = eval_program(&parse, 1_000_000, 1024)
                .err()
                .unwrap_or_else(|| panic!("{src}: oracle must fail"));
            assert_eq!(d.code, ErrorCode::RuntimeTrap, "{src}: failure code");
            assert_eq!(d.code.code_str(), "AION-E0044", "{src}");
            assert_eq!(d.message, "assertion failed", "{src}");
            // Exact recorded statement span — not the operand span
            // and not a reconstructed span.
            assert_eq!(d.span, Some(expected), "{src}: statement span");
            // Parsed assertions carry HumanAuthored provenance.
            assert_eq!(
                d.provenance,
                Some(aion_error::AssertProvenance::human_authored()),
                "{src}: provenance preserved"
            );
        }
    }

    #[test]
    fn oracle_assert_false_preserves_tool_provenance() {
        // Non-default provenance set by tooling must survive to the
        // oracle E0044 for both literal and dynamic false operands.
        for src in [
            "fn main() { assert false; }",
            "fn main() { assert 1 == 2; }",
        ] {
            let mut parse = parse_assert(src);
            let expected = first_assert_span(&parse);
            let tool = aion_error::AssertProvenance::ai_generated(Some("agent-7"));
            for item in &mut parse.program.items {
                if let aion_syntax::Item::Fn(f) = item {
                    for st in &mut f.body.stmts {
                        if let aion_syntax::Stmt::Assert { provenance, .. } = st {
                            *provenance = tool.clone();
                        }
                    }
                }
            }
            let d = eval_program(&parse, 1_000_000, 1024)
                .err()
                .unwrap_or_else(|| panic!("{src}: oracle must fail"));
            assert_eq!(d.code.code_str(), "AION-E0044", "{src}");
            assert_eq!(d.span, Some(expected), "{src}: statement span");
            assert_eq!(d.provenance, Some(tool.clone()), "{src}: tool provenance");
        }
    }
}
