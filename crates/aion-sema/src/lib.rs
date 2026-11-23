//! `aion-sema`: M2 type checking, name resolution, and move analysis.
//!
//! Bidirectional monomorphic inference over the M1 syntax (SPEC Sec 3).
//! No annotations exist in M2: every type is inferred. Entry [`check`]
//! takes a [`ParseResult`]; when parsing produced errors the result is
//! returned untouched (empty diagnostics), mirroring the driver contract.
//! This module is total: it never panics on any input.
//!
//! Spans come ONLY from the parser-recorded [`SpanTable`], joined to the
//! span-free AST through the parallel index tree (`ProgramIds` and friends
//! in `aion-syntax`): sema walks `program` and `index` in lockstep and
//! never searches source text.

use aion_error::{Diagnostic, ErrorCode};
use aion_sem_model::{
    GoalEntry, GoalRef, IntentEntry, ScopeId, ScopeTree, Symbol, SymbolKind, Ty as PubTy,
};
use aion_span::ByteSpan;
use aion_syntax::{
    Block, BlockIds, Expr, ExprIds, ExprIdsKind, FnIds, ItemIds, NodeId, ParseResult, Stmt, StmtIds,
};
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// Internal types (may contain inference variables)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Ity {
    Int,
    Str,
    Bool,
    Null,
    Unit,
    Fn { params: Vec<Ity>, ret: Box<Ity> },
    Var(u32),
    Error,
}

impl Ity {
    fn to_pub(&self, ck: &Checker<'_>) -> PubTy {
        match self {
            Ity::Int => PubTy::Int,
            Ity::Str => PubTy::Str,
            Ity::Bool => PubTy::Bool,
            Ity::Null => PubTy::Null,
            Ity::Unit => PubTy::Unit,
            Ity::Error => PubTy::Error,
            Ity::Var(v) => match ck.resolve_var(*v) {
                Ity::Var(w) => PubTy::Var(w),
                other => other.to_pub(ck),
            },
            Ity::Fn { params, ret } => PubTy::Fn {
                params: params.iter().map(|p| p.to_pub(ck)).collect(),
                ret: Box::new(ret.to_pub(ck)),
            },
        }
    }
}

fn merge(a: ByteSpan, b: ByteSpan) -> ByteSpan {
    ByteSpan {
        file: a.file,
        start: a.start.min(b.start),
        end: a.end.max(b.end),
    }
}

// ---------------------------------------------------------------------------
// Bindings and union-find
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Bind {
    ty: Ity,
    kind: SymbolKind,
    def_span: ByteSpan,
    decl: NodeId,
    moved: bool,
    move_span: Option<ByteSpan>,
}

#[derive(Debug, Clone)]
struct FnSig {
    param_vars: Vec<Ity>,
    ret_var: Ity,
}

struct Checker<'p> {
    spans: &'p aion_syntax::SpanTable,
    fallback: ByteSpan,
    diags: Vec<Diagnostic>,
    tree: ScopeTree,
    // Parallel stacks: scope ids in the tree and checker-local bindings
    // (which additionally track move state).
    scope_ids: Vec<ScopeId>,
    bindings: Vec<HashMap<String, Bind>>,
    /// (`diag_index`, moved-value type snapshot) for each `UseAfterMove`
    /// diagnostic, used by the post-inference soundness filter.
    move_tys: Vec<(usize, Ity)>,
    /// Candidate move sites: (use-site [`NodeId`], moved-value type
    /// snapshot) recorded at exactly the sites that feed move behavior
    /// (call argument, `let` initializer, trailing return). The
    /// post-inference soundness filter drops `Copy`-resolved entries;
    /// survivors populate [`aion_sem_model::SemModel::moved_uses`].
    move_sites: Vec<(NodeId, Ity)>,
    /// Raw (possibly var-containing) type snapshot per expression node,
    /// plus block-value/stmt node entries. Resolved with the final
    /// substitution post-inference before publishing on the model.
    expr_tys: HashMap<NodeId, Ity>,
    /// Every `(scope, name, type)` ever mirrored into the scope tree, so
    /// symbol `.ty` fields can be re-resolved post-inference (touching
    /// only `.ty`, never kind/name/span/moved).
    mirrored: Vec<(ScopeId, String, Ity)>,
    // Union-find over inference variables; `subst[v]` is the bound
    // ground type (or another `Ity::Var`) when resolved.
    subst: Vec<Option<Ity>>,
    fn_sigs: HashMap<String, FnSig>,
    fn_first_span: HashMap<String, ByteSpan>,
    /// Goals collected from the function body currently being checked
    /// (resolution only; drained by `check_clean` per `fn` item).
    cur_fn_goals: Vec<GoalEntry>,
    /// Retired (popped) scopes: scope id -> bindings snapshot. Purely
    /// additive retention so owned intent-block goals can resolve in
    /// their owner fn's lexical environment after the body is checked.
    /// Never consulted by normal checking paths.
    retired: HashMap<ScopeId, HashMap<String, Bind>>,
    /// Block node -> scope id for every block entered during checking.
    block_scopes: HashMap<NodeId, ScopeId>,
}

impl<'p> Checker<'p> {
    /// Parser-recorded span for `id`; total (empty fallback, never panics).
    fn span(&self, id: NodeId) -> ByteSpan {
        self.spans.get(id).unwrap_or(self.fallback)
    }

    fn fresh_var(&mut self) -> Ity {
        let v = self.subst.len() as u32;
        self.subst.push(None);
        Ity::Var(v)
    }

    fn resolve_var(&self, v: u32) -> Ity {
        let mut cur = v;
        let mut seen = 0usize;
        loop {
            if seen > self.subst.len() + 8 {
                return Ity::Error; // cycle guard; total, never hangs
            }
            seen += 1;
            match self.subst.get(cur as usize).and_then(|s| s.clone()) {
                None => return Ity::Var(cur),
                Some(Ity::Var(w)) => {
                    if w == cur {
                        return Ity::Var(cur);
                    }
                    cur = w;
                }
                Some(t) => return t,
            }
        }
    }

    fn resolve(&self, t: &Ity) -> Ity {
        match t {
            Ity::Var(v) => self.resolve_var(*v),
            Ity::Fn { params, ret } => Ity::Fn {
                params: params.iter().map(|p| self.resolve(p)).collect(),
                ret: Box::new(self.resolve(ret)),
            },
            other => other.clone(),
        }
    }

    /// Bind variable `v` to type `t`. Returns false on conflicting grounds.
    fn bind_var(&mut self, v: u32, t: &Ity) -> bool {
        let r = self.resolve(t);
        match r {
            Ity::Var(w) => {
                if w != v {
                    if let Some(slot) = self.subst.get_mut(v as usize) {
                        *slot = Some(Ity::Var(w));
                    }
                }
                true
            }
            Ity::Error => true, // error absorbs: no cascade
            ground => {
                if let Some(slot) = self.subst.get_mut(v as usize) {
                    *slot = Some(ground);
                }
                true
            }
        }
    }

    /// Unify `a` with `b`; false when two distinct grounds collide.
    fn unify(&mut self, a: &Ity, b: &Ity) -> bool {
        let ra = self.resolve(a);
        let rb = self.resolve(b);
        match (ra, rb) {
            (Ity::Error, _) | (_, Ity::Error) => true,
            (Ity::Var(v), t) | (t, Ity::Var(v)) => self.bind_var(v, &t),
            (Ity::Fn { .. }, _) | (_, Ity::Fn { .. }) => {
                // Fn-vs-Fn structural compare would go here; M2 never
                // equates function types, so any Fn involvement with a
                // non-var is left alone (arity is checked separately).
                true
            }
            (x, y) => x == y,
        }
    }

    /// Copy-ness on the resolved type. A variable still unresolved after
    /// inference is genuinely unknown (never constrained to a `Copy` ground
    /// type) and is treated as OWNED: moves are never missed (sound
    /// direction), and false positives are filtered post-inference once
    /// late constraints resolve the variable to a `Copy` ground type.
    fn is_copy(&self, t: &Ity) -> bool {
        match self.resolve(t) {
            Ity::Int | Ity::Bool | Ity::Null | Ity::Unit | Ity::Error => true,
            Ity::Str => false,
            Ity::Var(_) => false, // unknown: assume owned, never miss a move
            Ity::Fn { .. } => true,
        }
    }

    /// Post-inference filter for `UseAfterMove` diagnostics: drop the ones
    /// whose moved value turned out `Copy` once all constraints applied.
    /// Unresolved variables stay owned (kept). Total.
    ///
    /// Returns the surviving recorded move facts: the use-site [`NodeId`]s
    /// whose moved value is still owned after final resolution. This is
    /// the single authoritative move set published on the model.
    fn retain_sound_moves(&mut self) -> HashSet<NodeId> {
        let subst_len = self.subst.len();
        let resolve_final = |t: &Ity, subst: &[Option<Ity>]| -> Ity {
            let mut cur = t.clone();
            let mut seen = 0usize;
            loop {
                let Ity::Var(v) = cur else {
                    return cur;
                };
                if seen > subst_len + 8 {
                    return Ity::Error;
                }
                seen += 1;
                match subst.get(v as usize).and_then(|s| s.clone()) {
                    None => return Ity::Var(v),
                    Some(Ity::Var(w)) if w == v => return Ity::Var(v),
                    Some(Ity::Var(w)) => cur = Ity::Var(w),
                    Some(g) => return g,
                }
            }
        };
        let ty_of: HashMap<usize, Ity> = self.move_tys.iter().cloned().collect();
        let mut drop_idx = HashSet::new();
        for (i, d) in self.diags.iter().enumerate() {
            if d.code != ErrorCode::UseAfterMove {
                continue;
            }
            if let Some(ty) = ty_of.get(&i) {
                match resolve_final(ty, &self.subst) {
                    Ity::Int | Ity::Bool | Ity::Null | Ity::Unit | Ity::Error | Ity::Fn { .. } => {
                        drop_idx.insert(i);
                    }
                    _ => {} // Str or still-unknown Var: owned, keep
                }
            }
        }
        if !drop_idx.is_empty() {
            let mut diags = Vec::with_capacity(self.diags.len());
            for (i, d) in self.diags.drain(..).enumerate() {
                if !drop_idx.contains(&i) {
                    diags.push(d);
                }
            }
            self.diags = diags;
        }
        self.move_tys.clear();
        let sites = std::mem::take(&mut self.move_sites);
        let mut kept = HashSet::new();
        for (id, ty) in sites {
            match resolve_final(&ty, &self.subst) {
                Ity::Int | Ity::Bool | Ity::Null | Ity::Unit | Ity::Error | Ity::Fn { .. } => {}
                _ => {
                    kept.insert(id);
                } // Str or still-unknown Var: owned, keep
            }
        }
        kept
    }

    /// Controlled diagnostic for AST/index shape mismatches (stale or
    /// adversarial inputs). Never panics; the checker stays total.
    fn index_mismatch(&mut self, what: &str) {
        self.diags.push(
            Diagnostic::new(
                ErrorCode::ParseError,
                format!("internal index mismatch: {what}"),
            )
            .with_span(self.fallback),
        );
    }

    fn lookup(&self, name: &str) -> Option<(usize, Bind)> {
        for (depth, scope) in self.bindings.iter().enumerate().rev() {
            if let Some(b) = scope.get(name) {
                return Some((depth, b.clone()));
            }
        }
        None
    }

    /// Resolve `name` starting from an arbitrary scope id, walking the
    /// scope tree innermost-first. Consults retired (popped) scope
    /// snapshots first, falling back to still-live scopes. Used only
    /// for owned intent-goal resolution; normal checking uses
    /// [`Self::lookup`].
    fn lookup_at(&self, mut scope: ScopeId, name: &str) -> Option<Bind> {
        loop {
            if let Some(m) = self.retired.get(&scope) {
                if let Some(b) = m.get(name) {
                    return Some(b.clone());
                }
            } else if let Some(pos) = self.scope_ids.iter().position(|&s| s == scope) {
                if let Some(b) = self.bindings.get(pos).and_then(|m| m.get(name)) {
                    return Some(b.clone());
                }
            }
            match self
                .tree
                .scopes
                .get(scope.0 as usize)
                .and_then(|s| s.parent)
            {
                Some(parent) => scope = parent,
                None => return None,
            }
        }
    }

    fn current(&mut self) -> &mut HashMap<String, Bind> {
        let n = self.bindings.len().saturating_sub(1);
        &mut self.bindings[n]
    }

    fn push_scope(&mut self, parent: ScopeId) -> ScopeId {
        let id = self.tree.fork(parent);
        self.scope_ids.push(id);
        self.bindings.push(HashMap::new());
        id
    }

    fn pop_scope(&mut self) {
        // Retain the popped bindings snapshot (keyed by scope id) for
        // owned intent-goal resolution; normal checking never reads it.
        if let (Some(id), Some(map)) = (self.scope_ids.pop(), self.bindings.pop()) {
            self.retired.insert(id, map);
        }
    }

    fn mirror(&mut self, scope: ScopeId, name: &str, b: &Bind) {
        self.mirrored.push((scope, name.to_string(), b.ty.clone()));
        let pub_ty = b.ty.to_pub(self);
        if let Some(s) = self.tree.scopes.get_mut(scope.0 as usize) {
            s.symbols.insert(
                name.to_string(),
                Symbol {
                    kind: b.kind,
                    name: name.to_string(),
                    ty: pub_ty,
                    span: b.def_span,
                    moved: b.moved,
                },
            );
        }
    }

    /// Record a move of binding `name` at use site `use_span`.
    /// `use_site` is the [`NodeId`] of the consuming identifier
    /// expression and `ty` its type snapshot; on the first (and only
    /// recorded) move the site is pushed to `move_sites` for the
    /// post-inference soundness filter. Repeat consumes only diagnose.
    fn mark_moved(&mut self, name: &str, use_span: ByteSpan, use_site: NodeId, ty: Ity) {
        let hit = self.lookup(name);
        if let Some((depth, mut b)) = hit {
            if !b.moved {
                b.moved = true;
                b.move_span = Some(use_span);
                self.move_sites.push((use_site, ty));
                if let Some(scope) = self.bindings.get_mut(depth) {
                    scope.insert(name.to_string(), b.clone());
                }
                if let Some(id) = self.scope_ids.get(depth).copied() {
                    self.mirror(id, name, &b);
                }
            }
        }
    }

    // -- expressions: returns (type, span) -------------------------------

    fn check_expr(&mut self, e: &Expr, ids: &ExprIds) -> (Ity, ByteSpan) {
        let (ty, sp) = self.check_expr_inner(e, ids);
        self.expr_tys.insert(ids.node, ty.clone());
        (ty, sp)
    }

    fn check_expr_inner(&mut self, e: &Expr, ids: &ExprIds) -> (Ity, ByteSpan) {
        let node_sp = self.span(ids.node);
        match e {
            Expr::Int(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Null => {
                let ty = match e {
                    Expr::Int(_) => Ity::Int,
                    Expr::Str(_) => Ity::Str,
                    Expr::Bool(_) => Ity::Bool,
                    _ => Ity::Null,
                };
                (ty, node_sp)
            }
            Expr::Ident(name) => {
                let sp = node_sp;
                if name == "null" {
                    // Defensive: SPEC notes the M1 parser once read `null`
                    // as an identifier; treat it as the Null literal.
                    return (Ity::Null, sp);
                }
                match self.lookup(name) {
                    None => {
                        self.diags.push(
                            Diagnostic::new(
                                ErrorCode::UnboundName,
                                format!("unbound name `{name}`"),
                            )
                            .with_span(sp),
                        );
                        (Ity::Error, sp)
                    }
                    Some((_, b)) => {
                        if b.moved && !self.is_copy(&b.ty) {
                            let mut d = Diagnostic::new(
                                ErrorCode::UseAfterMove,
                                format!("use of moved value `{name}`"),
                            )
                            .with_span(sp);
                            if let Some(ms) = b.move_span {
                                d = d.with_related(ms, "moved here");
                            }
                            self.move_tys.push((self.diags.len(), b.ty.clone()));
                            self.diags.push(d);
                        }
                        (b.ty.clone(), sp)
                    }
                }
            }
            Expr::Unary { op, rhs } => {
                let osp = match &ids.kind {
                    ExprIdsKind::Unary { op, .. } => self.span(*op),
                    _ => node_sp,
                };
                let (rt, rsp) = self.check_expr(rhs, Self::unary_rhs(ids));
                let sp = merge(osp, rsp);
                let want = match op {
                    aion_syntax::UnOp::Neg => Ity::Int,
                    aion_syntax::UnOp::Not => Ity::Bool,
                };
                match self.resolve(&rt) {
                    Ity::Error => (Ity::Error, sp),
                    Ity::Var(v) => {
                        self.bind_var(v, &want);
                        (want, sp)
                    }
                    t if t == want => (want, sp),
                    _ => {
                        self.diags.push(
                            Diagnostic::new(
                                ErrorCode::TypeMismatch,
                                "operand type mismatch".to_string(),
                            )
                            .with_span(sp),
                        );
                        (Ity::Error, sp)
                    }
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                let (l_ids, r_ids) = Self::binary_parts(ids);
                self.check_binary(*op, lhs, rhs, l_ids, r_ids)
            }
            Expr::Call { callee, args } => {
                let (callee_id, arg_list_id, arg_ids) = Self::call_parts(ids);
                let csp = self.span(callee_id);
                let list_span = self.span(arg_list_id);
                let mut arg_tys = Vec::with_capacity(args.len());
                let mut arg_spans = Vec::with_capacity(args.len());
                for (a, a_ids) in args.iter().zip(arg_ids.iter()) {
                    let (t, s) = self.check_expr(a, a_ids);
                    arg_tys.push(t);
                    arg_spans.push(s);
                }
                let call_span = merge(csp, list_span);
                let _ = node_sp;

                // Ownership: passing a `Str` as a bare identifier moves it.
                for ((a, a_ids), (t, s)) in args
                    .iter()
                    .zip(arg_ids.iter())
                    .zip(arg_tys.iter().zip(arg_spans.iter()))
                {
                    if matches!(a, Expr::Ident(_)) && !self.is_copy(t) {
                        if let Expr::Ident(nm) = a {
                            self.mark_moved(nm, *s, a_ids.node, t.clone());
                        }
                    }
                }

                match self.lookup(callee) {
                    None => {
                        self.diags.push(
                            Diagnostic::new(
                                ErrorCode::UnboundName,
                                format!("unbound name `{callee}`"),
                            )
                            .with_span(csp),
                        );
                        (Ity::Error, call_span)
                    }
                    Some((_, b)) => match self.resolve(&b.ty) {
                        Ity::Error => (Ity::Error, call_span),
                        Ity::Var(v) => {
                            let ret = self.fresh_var();
                            let fty = Ity::Fn {
                                params: arg_tys.clone(),
                                ret: Box::new(ret.clone()),
                            };
                            self.bind_var(v, &fty);
                            (ret, call_span)
                        }
                        Ity::Fn { params, ret } => {
                            if params.len() != arg_tys.len() {
                                self.diags.push(
                                    Diagnostic::new(
                                        ErrorCode::ArityMismatch,
                                        format!(
                                            "arity mismatch: expected {} argument(s), found {}",
                                            params.len(),
                                            arg_tys.len()
                                        ),
                                    )
                                    .with_span(list_span),
                                );
                                (Ity::Error, call_span)
                            } else {
                                for (p, (a, s)) in
                                    params.iter().zip(arg_tys.iter().zip(arg_spans.iter()))
                                {
                                    if !self.unify(p, a) {
                                        self.diags.push(
                                            Diagnostic::new(
                                                ErrorCode::TypeMismatch,
                                                "argument type mismatch".to_string(),
                                            )
                                            .with_span(*s),
                                        );
                                    }
                                }
                                ((*ret).clone(), call_span)
                            }
                        }
                        _ => {
                            self.diags.push(
                                Diagnostic::new(
                                    ErrorCode::TypeMismatch,
                                    format!("`{callee}` is not callable"),
                                )
                                .with_span(csp),
                            );
                            (Ity::Error, call_span)
                        }
                    },
                }
            }
        }
    }

    fn unary_rhs(ids: &ExprIds) -> &ExprIds {
        match &ids.kind {
            ExprIdsKind::Unary { rhs, .. } => rhs,
            _ => ids,
        }
    }

    fn binary_parts(ids: &ExprIds) -> (&ExprIds, &ExprIds) {
        match &ids.kind {
            ExprIdsKind::Binary { lhs, rhs, .. } => (lhs, rhs),
            _ => (ids, ids),
        }
    }

    fn call_parts(ids: &ExprIds) -> (NodeId, NodeId, &[ExprIds]) {
        match &ids.kind {
            ExprIdsKind::Call {
                callee,
                arg_list,
                args,
            } => (*callee, *arg_list, args),
            _ => (ids.node, ids.node, &[]),
        }
    }

    fn check_binary(
        &mut self,
        op: aion_syntax::BinOp,
        lhs: &Expr,
        rhs: &Expr,
        l_ids: &ExprIds,
        r_ids: &ExprIds,
    ) -> (Ity, ByteSpan) {
        use aion_syntax::BinOp as O;
        let (lt, lsp) = self.check_expr(lhs, l_ids);
        let (rt, rsp) = self.check_expr(rhs, r_ids);
        let sp = merge(lsp, rsp);
        if matches!(self.resolve(&lt), Ity::Error) || matches!(self.resolve(&rt), Ity::Error) {
            return (Ity::Error, sp);
        }
        match op {
            O::Add | O::Sub | O::Mul | O::Div | O::Mod => {
                let mut ok = true;
                for t in [&lt, &rt] {
                    match self.resolve(t) {
                        Ity::Var(v) => {
                            self.bind_var(v, &Ity::Int);
                        }
                        Ity::Int => {}
                        _ => ok = false,
                    }
                }
                if ok {
                    (Ity::Int, sp)
                } else {
                    self.type_mismatch(sp);
                    (Ity::Error, sp)
                }
            }
            O::Lt | O::Le | O::Gt | O::Ge => {
                let mut ok = true;
                for t in [&lt, &rt] {
                    match self.resolve(t) {
                        Ity::Var(v) => {
                            self.bind_var(v, &Ity::Int);
                        }
                        Ity::Int => {}
                        _ => ok = false,
                    }
                }
                if ok {
                    (Ity::Bool, sp)
                } else {
                    self.type_mismatch(sp);
                    (Ity::Error, sp)
                }
            }
            O::Or | O::And => {
                let mut ok = true;
                for t in [&lt, &rt] {
                    match self.resolve(t) {
                        Ity::Var(v) => {
                            self.bind_var(v, &Ity::Bool);
                        }
                        Ity::Bool => {}
                        _ => ok = false,
                    }
                }
                if ok {
                    (Ity::Bool, sp)
                } else {
                    self.type_mismatch(sp);
                    (Ity::Error, sp)
                }
            }
            O::Eq | O::Ne => {
                let r = self.resolve(&lt);
                let s = self.resolve(&rt);
                match (r, s) {
                    (Ity::Var(v), Ity::Var(w)) => {
                        self.bind_var(v, &Ity::Var(w));
                        (Ity::Bool, sp)
                    }
                    (Ity::Var(v), t) | (t, Ity::Var(v)) => {
                        self.bind_var(v, &t);
                        (Ity::Bool, sp)
                    }
                    (x, y) if x == y => (Ity::Bool, sp),
                    _ => {
                        self.type_mismatch(sp);
                        (Ity::Error, sp)
                    }
                }
            }
        }
    }

    // -- statements, blocks, functions ---------------------------------

    fn type_mismatch(&mut self, sp: ByteSpan) {
        self.diags.push(
            Diagnostic::new(ErrorCode::TypeMismatch, "operand type mismatch".to_string())
                .with_span(sp),
        );
    }

    /// Resolution-only goal check: walk `e`/`ids` in parallel, resolving
    /// every name occurrence (including call callees) with [`Self::lookup`]
    /// against the live scopes. Unbound names yield `E0011` at the
    /// occurrence span. Records `(occurrence, decl)` per occurrence.
    /// No inference, no moves, no scope changes.
    /// Resolution-only goal check. With `base: None`, resolves against
    /// the live scopes (function-body goals). With `base: Some(scope)`,
    /// resolves against that scope id through live + retired scopes
    /// (owned intent-block goals resolve in their owner fn's lexical
    /// environment, including parameters and body locals).
    fn check_goal(
        &mut self,
        e: &Expr,
        ids: &ExprIds,
        refs: &mut Vec<GoalRef>,
        base: Option<ScopeId>,
    ) {
        // Single resolution point: live stack for body goals, scope-id
        // walk (live + retired snapshots) for owned intent goals.
        let resolve = |ck: &Self, name: &str| -> Option<Bind> {
            match base {
                None => ck.lookup(name).map(|(_, b)| b),
                Some(scope) => ck.lookup_at(scope, name),
            }
        };
        match e {
            Expr::Ident(name) => {
                if name == "null" {
                    return; // `null` reads as the Null literal
                }
                let sp = self.span(ids.node);
                match resolve(self, name) {
                    None => {
                        self.diags.push(
                            Diagnostic::new(
                                ErrorCode::UnboundName,
                                format!("unbound name `{name}`"),
                            )
                            .with_span(sp),
                        );
                        refs.push(GoalRef {
                            use_site: ids.node,
                            decl: None,
                        });
                    }
                    Some(b) => refs.push(GoalRef {
                        use_site: ids.node,
                        decl: Some(b.decl),
                    }),
                }
            }
            Expr::Call { callee, args } => {
                let (callee_id, _, arg_ids) = Self::call_parts(ids);
                let csp = self.span(callee_id);
                match resolve(self, callee) {
                    None => {
                        self.diags.push(
                            Diagnostic::new(
                                ErrorCode::UnboundName,
                                format!("unbound name `{callee}`"),
                            )
                            .with_span(csp),
                        );
                        refs.push(GoalRef {
                            use_site: callee_id,
                            decl: None,
                        });
                    }
                    Some(b) => refs.push(GoalRef {
                        use_site: callee_id,
                        decl: Some(b.decl),
                    }),
                }
                for (a, a_ids) in args.iter().zip(arg_ids.iter()) {
                    self.check_goal(a, a_ids, refs, base);
                }
            }
            Expr::Unary { rhs, .. } => {
                self.check_goal(rhs, Self::unary_rhs(ids), refs, base);
            }
            Expr::Binary { lhs, rhs, .. } => {
                let (l_ids, r_ids) = Self::binary_parts(ids);
                self.check_goal(lhs, l_ids, refs, base);
                self.check_goal(rhs, r_ids, refs, base);
            }
            _ => {}
        }
    }

    /// Check a block in a fresh child scope. Value = type of the
    /// trailing `do` expression, else `Unit`. The trailing value (if any)
    /// is also returned as `(name, use_span, use_site)` when it is a bare
    /// identifier, so the caller can apply return-move semantics (SPEC
    /// Sec 3). `use_site` is the trailing identifier's [`NodeId`].
    fn check_block(
        &mut self,
        b: &Block,
        bids: &BlockIds,
    ) -> (Ity, Option<(String, ByteSpan, NodeId)>) {
        let parent = *self.scope_ids.last().unwrap_or(&ScopeId(0));
        let scope = self.push_scope(parent);
        self.block_scopes.insert(bids.node, scope);
        let mut value = Ity::Unit;
        let mut trailing: Option<(String, ByteSpan, NodeId)> = None;
        let n = b.stmts.len();
        if n != bids.stmts.len() {
            self.index_mismatch("block statement count differs from index");
            return (Ity::Error, None);
        }
        for (i, (st, sid)) in b.stmts.iter().zip(bids.stmts.iter()).enumerate() {
            match (st, sid) {
                (
                    Stmt::Let { name, expr },
                    StmtIds::Let {
                        name: name_id,
                        expr: expr_ids,
                        ..
                    },
                ) => {
                    let def_span = self.span(*name_id);
                    let (init_ty, init_span) = self.check_expr(expr, expr_ids);
                    // A `let` of a `Str` by bare identifier moves it.
                    if matches!(expr, Expr::Ident(_)) && !self.is_copy(&init_ty) {
                        if let Expr::Ident(nm) = expr {
                            self.mark_moved(nm, init_span, expr_ids.node, init_ty.clone());
                        }
                    }
                    // Shadowing re-lets are fresh and legal: overwrite.
                    let v = self.fresh_var();
                    let var = v.clone();
                    self.unify(&var, &init_ty);
                    let bind = Bind {
                        ty: v,
                        kind: SymbolKind::Let,
                        def_span,
                        decl: *name_id,
                        moved: false,
                        move_span: None,
                    };
                    self.current().insert(name.clone(), bind.clone());
                    self.mirror(scope, name, &bind);
                    if let StmtIds::Let { node, .. } = sid {
                        self.expr_tys.insert(*node, init_ty.clone());
                    }
                    let _ = i;
                }
                (
                    Stmt::Do(e),
                    StmtIds::Do {
                        node,
                        expr: expr_ids,
                    },
                ) => {
                    let (t, s) = self.check_expr(e, expr_ids);
                    self.expr_tys.insert(*node, t.clone());
                    if i + 1 == n {
                        value = t;
                        if let Expr::Ident(nm) = e {
                            trailing = Some((nm.clone(), s, expr_ids.node));
                        }
                    }
                }
                (Stmt::Block(b), StmtIds::Block { node, body: bids }) => {
                    let (t, _) = self.check_block(b, bids);
                    self.expr_tys.insert(*node, t.clone());
                    if i + 1 == n {
                        value = t;
                    }
                }
                (Stmt::Empty, StmtIds::Empty(nid)) => {
                    self.expr_tys.insert(*nid, Ity::Unit);
                }
                (
                    Stmt::Goal(e),
                    StmtIds::Goal {
                        node,
                        expr: expr_ids,
                    },
                ) => {
                    // Resolution only: no unify/bind_var/fresh_var, no
                    // mark_moved, no define/mirror/push/pop, no
                    // block-value contribution; the statement keeps
                    // `Unit` type. Called with the enclosing block
                    // scope live.
                    let mut refs = Vec::new();
                    self.check_goal(e, expr_ids, &mut refs, None);
                    self.cur_fn_goals.push(GoalEntry {
                        stmt: *node,
                        expr: expr_ids.clone(),
                        refs,
                    });
                    self.expr_tys.insert(*node, Ity::Unit);
                }
                (
                    Stmt::Assert(e),
                    StmtIds::Assert {
                        node,
                        expr: expr_ids,
                    },
                ) => {
                    let (t, s) = self.check_expr(e, expr_ids);
                    self.expr_tys.insert(*node, Ity::Unit);
                    if !matches!(self.resolve(&t), Ity::Error | Ity::Var(_))
                        && self.resolve(&t) != Ity::Bool
                    {
                        self.type_mismatch(s);
                    }
                    let _ = i;
                }
                _ => {
                    // Total: AST/index kind mismatch (stale index) emits a
                    // controlled diagnostic and skips the statement.
                    self.index_mismatch("statement/index kind mismatch");
                }
            }
        }
        let _ = n;
        self.expr_tys.insert(bids.node, value.clone());
        self.pop_scope();
        (value, trailing)
    }

    fn check_fn(&mut self, f: &aion_syntax::FnDecl, fids: &FnIds, global: ScopeId) {
        // Fresh per-function goal buffer; drained by `check_clean`.
        self.cur_fn_goals = Vec::new();
        // Duplicate definitions still get their bodies checked (in a
        // fresh scope) so one error does not hide others.
        let name_span = self.span(fids.name);
        if let Some(first) = self.fn_first_span.get(&f.name).copied() {
            if first != name_span {
                self.diags.push(
                    Diagnostic::new(
                        ErrorCode::DuplicateDefinition,
                        format!("duplicate definition of `{}`", f.name),
                    )
                    .with_span(name_span)
                    .with_related(first, "first defined here"),
                );
            }
        } else {
            self.fn_first_span.insert(f.name.clone(), name_span);
        }

        let sig = self.fn_sigs.get(&f.name);
        let (param_vars, ret_var) = match sig {
            Some(s) => (s.param_vars.clone(), s.ret_var.clone()),
            None => (vec![], self.fresh_var()),
        };

        let scope = self.push_scope(global);
        if f.params.len() != fids.params.len() {
            self.index_mismatch("function parameter count differs from index");
            self.pop_scope();
            return;
        }
        let mut seen_params: HashMap<String, ByteSpan> = HashMap::new();
        for (i, (p, pid)) in f.params.iter().zip(fids.params.iter()).enumerate() {
            let pspan = self.span(*pid);
            if let Some(first) = seen_params.get(p) {
                self.diags.push(
                    Diagnostic::new(
                        ErrorCode::DuplicateDefinition,
                        format!("duplicate parameter `{p}`"),
                    )
                    .with_span(pspan)
                    .with_related(*first, "first declared here"),
                );
                continue;
            }
            seen_params.insert(p.clone(), pspan);
            let ty = param_vars
                .get(i)
                .cloned()
                .unwrap_or_else(|| self.fresh_var());
            let bind = Bind {
                ty,
                kind: SymbolKind::Param,
                def_span: pspan,
                decl: *pid,
                moved: false,
                move_span: None,
            };
            self.current().insert(p.clone(), bind.clone());
            self.mirror(scope, p, &bind);
        }

        let (body_ty, trailing) = self.check_block(&f.body, &fids.body);
        // Return-move (SPEC Sec 3): a `Str`-typed block value is owned by
        // the caller on return. Returning a local owned `Str` transfers it
        // and returning a `Str` parameter moves it; the source binding is
        // marked moved at the trailing use site. `Copy` returns never move.
        if let Some((nm, use_span, use_site)) = trailing {
            if !self.is_copy(&body_ty) {
                // The block scope already popped, so a block-local
                // trailing value has no live binding to mark (parameters
                // still do). The use-site moves either way — this is the
                // historical return-move decision — so record it even
                // when no binding is marked. Already-moved bindings stay
                // unrecorded (repeat consumes only diagnose).
                let live = self.lookup(&nm).is_some();
                self.mark_moved(&nm, use_span, use_site, body_ty.clone());
                if !live {
                    self.move_sites.push((use_site, body_ty.clone()));
                }
            }
        }
        self.unify(&ret_var, &body_ty);
        // Publish the inferred function type in the global scope.
        let fty = Ity::Fn {
            params: param_vars,
            ret: Box::new(ret_var),
        };
        if !self.bindings.is_empty() {
            let b = Bind {
                ty: fty.clone(),
                kind: SymbolKind::Fn,
                def_span: name_span,
                decl: fids.name,
                moved: false,
                move_span: None,
            };
            if let Some(g) = self.bindings.first_mut() {
                g.entry(f.name.clone()).or_insert_with(|| b.clone());
            }
            self.mirror(global, &f.name, &b);
        }
        self.pop_scope();
    }
}

// ---------------------------------------------------------------------------
// Public entry
// ---------------------------------------------------------------------------

/// Check a parsed program. When parsing produced errors the program is
/// returned unchecked (empty diagnostics); the driver enforces this by
/// only calling into sema on clean parses. Total: never panics.
pub fn check(parse: &ParseResult, file: aion_span::FileId, bytes: &[u8]) -> Vec<Diagnostic> {
    if !parse.errors.is_empty() {
        return Vec::new();
    }
    check_clean(parse, file, bytes).diags
}

/// Check plus the populated scope tree (visibility for tooling/tests).
pub fn check_with_model(
    parse: &ParseResult,
    file: aion_span::FileId,
    bytes: &[u8],
) -> (Vec<Diagnostic>, aion_sem_model::SemModel) {
    if !parse.errors.is_empty() {
        return (Vec::new(), aion_sem_model::SemModel::new());
    }
    let out = check_clean(parse, file, bytes);
    let mut model = aion_sem_model::SemModel::new();
    model.scope_tree = out.tree;
    model.moved_uses = out.moved_uses;
    model.items = out.items;
    model.types = out.types;
    let mut intents_by_owner: HashMap<NodeId, Vec<usize>> = HashMap::new();
    let mut orphans: Vec<usize> = Vec::new();
    for (i, e) in out.intents.iter().enumerate() {
        match e.owner {
            Some(o) => intents_by_owner.entry(o).or_default().push(i),
            None => orphans.push(i),
        }
    }
    model.intents = out.intents;
    model.intents_by_owner = intents_by_owner;
    model.orphans = orphans;
    (out.diags, model)
}

/// Output of [`check_clean`]: diagnostics plus the published model parts.
/// Anchor sort key for an intent entry: the intent node span for
/// explicit entries, the first body-goal stmt span for synthesized
/// entries. `None` (no anchor) sorts after every `Some` key, so
/// anchorless entries land deterministically last. Pure query over the
/// parse spans; never panics, never fabricates.
fn anchor_key(e: &IntentEntry, ck: &Checker) -> Option<(u32, u32, u32)> {
    let sp = match e.intent {
        Some(n) => ck.span(n),
        None => {
            let g = e.goals.first()?;
            ck.span(g.stmt)
        }
    };
    Some((sp.file.0, sp.start, sp.end))
}

/// Collect `goal : <expr>;` statements from an intent body at any
/// nesting depth. Non-goal statements contribute nothing.
fn collect_intent_goals(
    ck: &mut Checker,
    b: &aion_syntax::Block,
    bids: &BlockIds,
    out: &mut Vec<GoalEntry>,
    base: Option<ScopeId>,
) {
    if b.stmts.len() != bids.stmts.len() {
        ck.index_mismatch("intent block statement count differs from index");
        return;
    }
    for (st, sid) in b.stmts.iter().zip(bids.stmts.iter()) {
        match (st, sid) {
            (
                Stmt::Goal(e),
                StmtIds::Goal {
                    node,
                    expr: expr_ids,
                },
            ) => {
                let mut refs = Vec::new();
                ck.check_goal(e, expr_ids, &mut refs, base);
                out.push(GoalEntry {
                    stmt: *node,
                    expr: expr_ids.clone(),
                    refs,
                });
            }
            (Stmt::Block(inner), StmtIds::Block { body, .. }) => {
                collect_intent_goals(ck, inner, body, out, base);
            }
            _ => {}
        }
    }
}

struct CleanOutput {
    diags: Vec<Diagnostic>,
    tree: ScopeTree,
    moved_uses: HashSet<NodeId>,
    items: Vec<aion_sem_model::Item>,
    types: HashMap<NodeId, PubTy>,
    intents: Vec<IntentEntry>,
}

fn check_clean(parse: &ParseResult, file: aion_span::FileId, _bytes: &[u8]) -> CleanOutput {
    let _ = file;
    let fallback = aion_span::ByteSpan::empty(file, 0);
    // Pre-pass: one fresh variable per parameter and return slot, so
    // calls resolve even to functions defined later in the file.
    let mut ck = Checker {
        spans: &parse.spans,
        fallback,
        diags: Vec::new(),
        tree: ScopeTree::new(),
        scope_ids: Vec::new(),
        bindings: Vec::new(),
        subst: Vec::new(),
        fn_sigs: HashMap::new(),
        fn_first_span: HashMap::new(),
        move_tys: Vec::new(),
        move_sites: Vec::new(),
        expr_tys: HashMap::new(),
        mirrored: Vec::new(),
        cur_fn_goals: Vec::new(),
        retired: HashMap::new(),
        block_scopes: HashMap::new(),
    };
    if parse.program.items.len() != parse.index.items.len() {
        ck.index_mismatch("top-level item count differs from index");
        return CleanOutput {
            diags: ck.diags,
            tree: ck.tree,
            moved_uses: HashSet::new(),
            items: Vec::new(),
            types: HashMap::new(),
            intents: Vec::new(),
        };
    }
    for item in &parse.program.items {
        if let aion_syntax::Item::Fn(f) = item {
            let mut params = Vec::with_capacity(f.params.len());
            for _ in &f.params {
                params.push(ck.fresh_var());
            }
            let ret = ck.fresh_var();
            // Keep the first signature on duplicates; the duplicate
            // itself is reported during the main pass.
            ck.fn_sigs.entry(f.name.clone()).or_insert(FnSig {
                param_vars: params,
                ret_var: ret,
            });
        }
    }

    let global = ck.tree.root();
    ck.scope_ids.push(global);
    ck.bindings.push(HashMap::new());
    // Publish every function name up front so calls resolve regardless
    // of definition order (forward references included).
    for (item, iids) in parse.program.items.iter().zip(parse.index.items.iter()) {
        if let (aion_syntax::Item::Fn(f), ItemIds::Fn(fids)) = (item, iids) {
            if let Some(sig) = ck.fn_sigs.get(&f.name).cloned() {
                let fty = Ity::Fn {
                    params: sig.param_vars,
                    ret: Box::new(sig.ret_var),
                };
                let b = Bind {
                    ty: fty,
                    kind: SymbolKind::Fn,
                    def_span: ck.span(fids.name),
                    decl: fids.name,
                    moved: false,
                    move_span: None,
                };
                ck.bindings[0].entry(f.name.clone()).or_insert(b);
            }
        }
    }
    // Pending intent blocks: (intent node, intent body + index). Each is
    // claimed by the nearest following `fn` (R-FOLLOWING); leftovers
    // become retained orphans. Bodies resolve only at claim time, in the
    // owner fn's lexical environment (top-level for orphans).
    let mut pending: Vec<(NodeId, aion_syntax::Block, BlockIds)> = Vec::new();
    let mut intents: Vec<IntentEntry> = Vec::new();
    for (item, iids) in parse.program.items.iter().zip(parse.index.items.iter()) {
        match (item, iids) {
            (aion_syntax::Item::Fn(f), ItemIds::Fn(fids)) => {
                ck.check_fn(f, fids, global);
                let fn_goals = std::mem::take(&mut ck.cur_fn_goals);
                // Owner environment: the fn body block scope recorded
                // during checking (params + body locals via retired
                // snapshots). Falls back to live scopes only if the body
                // was never entered (already-diagnosed index mismatch).
                let env = ck.block_scopes.get(&fids.body.node).copied();
                for (intent_node, ibody, ibids) in pending.drain(..) {
                    let mut igoals = Vec::new();
                    collect_intent_goals(&mut ck, &ibody, &ibids, &mut igoals, env);
                    intents.push(IntentEntry {
                        owner: Some(fids.node),
                        intent: Some(intent_node),
                        goals: igoals,
                    });
                }
                if !fn_goals.is_empty() {
                    intents.push(IntentEntry {
                        owner: Some(fids.node),
                        intent: None,
                        goals: fn_goals,
                    });
                }
            }
            (aion_syntax::Item::Intent(b), ItemIds::Intent(ii)) => {
                // Deferred: resolved at claim time in the owner fn's
                // environment, or here in the top-level environment if
                // the block is orphaned (no following `fn`).
                pending.push((ii.node, b.body.clone(), ii.body.clone()));
            }
            _ => {
                ck.index_mismatch("item/index kind mismatch");
            }
        }
    }
    for (intent_node, ibody, ibids) in pending.drain(..) {
        // Orphan-position resolution: only the top-level scope is live
        // here, so orphan goals resolve in the top-level environment.
        let mut goals = Vec::new();
        collect_intent_goals(&mut ck, &ibody, &ibids, &mut goals, None);
        intents.push(IntentEntry {
            owner: None,
            intent: Some(intent_node),
            goals,
        });
    }
    // Anchor ordering: sort by anchor span (file, start, end). Anchor is
    // the intent node span for explicit entries, the first body-goal
    // stmt span for synthesized entries; sort key only, never stored.
    // The key is explicit `Option`: entries without an anchor sort
    // deterministically last (`None` sorts after `Some`). No spans are
    // fabricated and nothing here can panic.
    intents.sort_by_key(|a| anchor_key(a, &ck));
    // Post-inference pass: move analysis runs on final resolved types, so
    // late constraints (e.g. a callee pinning a parameter to `Int`) filter
    // out `Copy` false positives; still-unresolved variables stay owned.
    let moved_uses = ck.retain_sound_moves();
    // Final resolve pass over all recorded type snapshots with the final
    // substitution (mirrors the retain_sound_moves resolve_final pattern,
    // but deep through Fn types via resolve/to_pub) so no grounded-later
    // Var leaks into the published model.
    let mut types: HashMap<NodeId, PubTy> = HashMap::with_capacity(ck.expr_tys.len());
    for (id, raw) in &ck.expr_tys {
        let resolved = ck.resolve(raw);
        types.insert(*id, resolved.to_pub(&ck));
    }
    // Refresh scope_tree symbol .ty post-inference: re-mirror each recorded
    // Bind with the final substitution, touching ONLY .ty (kind/name/span
    // /moved are preserved from the live symbol).
    for (scope, name, raw) in std::mem::take(&mut ck.mirrored) {
        let pub_ty = ck.resolve(&raw).to_pub(&ck);
        if let Some(s) = ck.tree.scopes.get_mut(scope.0 as usize) {
            if let Some(sym) = s.symbols.get_mut(&name) {
                sym.ty = pub_ty;
            }
        }
    }
    // One Item per top-level fn; span is the fn name span.
    let mut items = Vec::new();
    for (item, iids) in parse.program.items.iter().zip(parse.index.items.iter()) {
        if let (aion_syntax::Item::Fn(f), ItemIds::Fn(fids)) = (item, iids) {
            items.push(aion_sem_model::Item {
                name: f.name.clone(),
                span: ck.span(fids.name),
            });
        }
    }
    CleanOutput {
        diags: ck.diags,
        tree: ck.tree,
        moved_uses,
        items,
        types,
        intents,
    }
}
