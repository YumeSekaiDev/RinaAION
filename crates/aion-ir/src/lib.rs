//! `aion-ir`: M3.2 typed IR + lowering.
//!
//! The IR is a typed decoration of the M1 AST: same shape, every node
//! carrying its inferred [`Ty`]. Lowering ([`lower`]) runs only on clean
//! sema output; it performs no independent move inference — a `Local`
//! use is [`LocalAccess::Move`] exactly where M2 move analysis recorded
//! a move (call argument, `let` binding, return local, or return
//! parameter, per SPEC Sec 4), otherwise [`LocalAccess::Copy`].
//!
//! Note on the return sites: the fn-level return-move records the
//! trailing use-site through one of two branches — the live-binding
//! mark when the returned name is still in scope (a returned parameter),
//! or an explicit push when the block scope already popped (a returned
//! local). Because the published move set is a `HashSet<NodeId>`, any
//! repeated recording of the same use-site [`NodeId`] collapses to a
//! single fact before lowering ever sees it.
//!
//! Total: lowering never panics. Structural AST/index mismatches (stale
//! or adversarial hand-off) yield `AION-E0004` diagnostics; any residual
//! type variable yields `AION-E0014` and no IR is emitted.

use aion_error::{AssertProvenance, Diagnostic, ErrorCode};
use aion_sem_model::Ty;
use aion_span::{ByteSpan, FileId};
use aion_syntax::{
    BinOp, Block, BlockIds, Expr, ExprIds, ExprIdsKind, FnIds, ItemIds, NodeId, ParseResult, Stmt,
    StmtIds, UnOp,
};
use std::collections::{HashMap, HashSet};

pub mod validate;
pub use validate::{validate, ValidatedModule};

// ---------------------------------------------------------------------------
// Typed IR
// ---------------------------------------------------------------------------

/// A lowered module (one source file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedModule {
    pub name: String,
    pub functions: Vec<TypedFn>,
    /// Empty span at the end of input, recorded at lowering time for
    /// entry-point diagnostics (SPEC Sec 4: missing `main` points at the
    /// end of input).
    pub eof_span: ByteSpan,
}

impl TypedModule {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            functions: Vec::new(),
            eof_span: ByteSpan::empty(FileId(0), 0),
        }
    }
}

/// Back-compat alias for the M0 `Module` name (`Module::new` keeps working).
pub type Module = TypedModule;
/// Back-compat alias for lowering output.
pub type IrModule = TypedModule;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedFn {
    pub name: String,
    /// Declaration header span (`fn` keyword through the `)` closer),
    /// threaded from the parser index for entry-point diagnostics
    /// (SPEC Sec 4: the offending `main` declaration span).
    pub decl_span: ByteSpan,
    pub params: Vec<(String, Ty)>,
    pub ret: Ty,
    pub body: TypedBlock,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedBlock {
    pub stmts: Vec<TypedStmt>,
    pub value: Ty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypedStmt {
    Let {
        name: String,
        ty: Ty,
        init: TypedExpr,
    },
    Do {
        expr: TypedExpr,
    },
    Block {
        body: TypedBlock,
    },
    /// Distinct assertion node (M4.4): never routed via `Do`.
    /// `span` is the original assertion statement span; `provenance`
    /// is the assertion's provenance for diagnostic attribution.
    Assert {
        expr: TypedExpr,
        span: ByteSpan,
        provenance: AssertProvenance,
    },
    /// Folded `assert false` (M4.4 M3): unconditional `E0044` trap.
    /// No operand to evaluate and no fuel to consult; carries the
    /// original statement span and provenance.
    AssertFalse {
        span: ByteSpan,
        provenance: AssertProvenance,
    },
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedExpr {
    pub ty: Ty,
    pub span: ByteSpan,
    pub kind: ExprKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExprKind {
    Int(i64),
    Str(String),
    Bool(bool),
    Null,
    Local {
        name: String,
        access: LocalAccess,
    },
    Unary {
        op: UnOp,
        rhs: Box<TypedExpr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<TypedExpr>,
        rhs: Box<TypedExpr>,
    },
    Call {
        callee: String,
        args: Vec<TypedExpr>,
    },
}

/// How a `Local` use interacts with ownership. `Move` appears exactly at
/// the M2 move sites (call argument, `let` binding, return local,
/// return parameter, per SPEC Sec 4); every other use is `Copy`
/// (a borrow/read).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalAccess {
    Copy,
    Move,
}

// ---------------------------------------------------------------------------
// Lowering
// ---------------------------------------------------------------------------

/// Lower a clean-sema parse to typed IR.
///
/// `file`/`source_len` locate the end-of-input span recorded on the
/// module for entry-point diagnostics. Returns the module plus
/// diagnostics. Any residual `Var` anywhere in the model types yields
/// `AION-E0014` and NO IR (empty module). Total: never panics.
pub fn lower(
    parse: &ParseResult,
    model: &aion_sem_model::SemModel,
    file: FileId,
    source_len: u32,
) -> (TypedModule, Vec<Diagnostic>) {
    let fallback = ByteSpan::empty(FileId(0), 0);
    // Note: first-wins on duplicate fn names iterates scope `symbols`
    // (a HashMap), but duplicates are unreachable on clean programs —
    // sema rejects redefinitions before lowering runs — so the winner
    // is unique and this map is deterministic in practice. Lookups by
    // key below are order-independent.
    let mut fns: HashMap<String, Ty> = HashMap::new();
    for scope in &model.scope_tree.scopes {
        for sym in scope.symbols.values() {
            if matches!(sym.ty, Ty::Fn { .. }) {
                fns.entry(sym.name.clone())
                    .or_insert_with(|| sym.ty.clone());
            }
        }
    }
    let mut lx = Lower {
        parse,
        fallback,
        diags: Vec::new(),
        scopes: Vec::new(),
        fns,
        // Sole move authority: the use-site set recorded by sema.
        moved_uses: model.moved_uses.clone(),
    };

    // E0014 gate first: walk every reachable Ty (model types + every
    // scope-tree symbol type, recursing into nested Fn params/ret).
    // Determinism: `model.types` and scope `symbols` are HashMaps, so
    // collect entries in sorted key order and sort the gate by
    // (span start, span end, message) before emitting diagnostics.
    let mut gate: Vec<(ByteSpan, Ty)> = Vec::new();
    let mut type_ids: Vec<_> = model.types.keys().copied().collect();
    type_ids.sort_by_key(|id| id.0);
    for id in type_ids {
        if let Some(ty) = model.types.get(&id) {
            if contains_var(ty) {
                gate.push((parse.spans.get(id).unwrap_or(fallback), ty.clone()));
            }
        }
    }
    for scope in &model.scope_tree.scopes {
        let mut names: Vec<_> = scope.symbols.keys().collect();
        names.sort();
        for name in names {
            if let Some(sym) = scope.symbols.get(name) {
                if contains_var(&sym.ty) {
                    gate.push((sym.span, sym.ty.clone()));
                }
            }
        }
    }
    if !gate.is_empty() {
        gate.sort_by(|a, b| {
            (a.0.start, a.0.end, format!("{}", a.1)).cmp(&(b.0.start, b.0.end, format!("{}", b.1)))
        });
        for (sp, ty) in gate {
            lx.diags.push(
                Diagnostic::new(
                    ErrorCode::UnresolvedType,
                    format!("unresolved type `{ty}`; cannot lower to IR"),
                )
                .with_span(sp),
            );
        }
        let mut empty = TypedModule::new(String::new());
        empty.eof_span = ByteSpan::empty(file, source_len);
        return (empty, lx.diags);
    }

    let mut module = TypedModule::new(String::new());
    module.eof_span = ByteSpan::empty(file, source_len);
    if parse.program.items.len() != parse.index.items.len() {
        lx.diags.push(
            Diagnostic::new(
                ErrorCode::ParseError,
                "internal index mismatch: top-level item count differs from index",
            )
            .with_span(fallback),
        );
        return (module, lx.diags);
    }
    for (item, iids) in parse.program.items.iter().zip(parse.index.items.iter()) {
        match (item, iids) {
            (aion_syntax::Item::Fn(f), ItemIds::Fn(fids)) => {
                if let Some(tf) = lx.lower_fn(f, fids) {
                    module.functions.push(tf);
                }
            }
            _ => {
                lx.diags.push(
                    Diagnostic::new(
                        ErrorCode::ParseError,
                        "internal index mismatch: item/index kind mismatch",
                    )
                    .with_span(fallback),
                );
            }
        }
    }
    (module, lx.diags)
}

/// True when `ty` (including nested `Fn` params/ret) contains a `Var`.
fn contains_var(ty: &Ty) -> bool {
    match ty {
        Ty::Var(_) => true,
        Ty::Fn { params, ret } => params.iter().any(contains_var) || contains_var(ret),
        _ => false,
    }
}

#[derive(Debug, Clone)]
struct Binding {
    ty: Ty,
}

struct Lower<'a> {
    parse: &'a ParseResult,
    fallback: ByteSpan,
    diags: Vec<Diagnostic>,
    scopes: Vec<HashMap<String, Binding>>,
    /// Function signatures (`name -> Ty::Fn`) from scope-tree symbols.
    fns: HashMap<String, Ty>,
    /// The recorded sema move facts: a `Local` use is [`LocalAccess::Move`]
    /// iff its use-site [`NodeId`] is in this set, otherwise `Copy`.
    moved_uses: HashSet<NodeId>,
}

impl Lower<'_> {
    fn span(&mut self, id: NodeId) -> ByteSpan {
        match self.parse.spans.get(id) {
            Some(sp) => sp,
            None => {
                let fb = self.fallback;
                self.diags.push(
                    Diagnostic::new(
                        ErrorCode::ParseError,
                        "internal index mismatch: missing span for node",
                    )
                    .with_span(fb),
                );
                fb
            }
        }
    }

    fn lookup(&self, name: &str) -> Option<Binding> {
        for scope in self.scopes.iter().rev() {
            if let Some(b) = scope.get(name) {
                return Some(b.clone());
            }
        }
        None
    }

    fn lower_fn(&mut self, f: &aion_syntax::FnDecl, fids: &FnIds) -> Option<TypedFn> {
        // Function signature from the scope-tree symbols collected up
        // front (sema mirrors every function there); a missing symbol is
        // hand-off corruption.
        let (mut param_tys, mut ret_ty) = (vec![Ty::Error; f.params.len()], Ty::Error);
        let mut have_sig = false;
        if let Some(Ty::Fn { params, ret }) = self.fns.get(&f.name) {
            param_tys = params.clone();
            ret_ty = (**ret).clone();
            have_sig = true;
        }
        if !have_sig {
            let fb = self.fallback;
            self.diags.push(
                Diagnostic::new(
                    ErrorCode::ParseError,
                    format!("internal index mismatch: no scope symbol for `{}`", f.name),
                )
                .with_span(fb),
            );
        }
        if f.params.len() != fids.params.len() {
            let fb = self.fallback;
            self.diags.push(
                Diagnostic::new(
                    ErrorCode::ParseError,
                    "internal index mismatch: function parameter count differs from index",
                )
                .with_span(fb),
            );
            return None;
        }
        self.scopes.push(HashMap::new());
        for (p, pt) in f.params.iter().zip(param_tys.iter()) {
            if let Some(top) = self.scopes.last_mut() {
                top.insert(p.clone(), Binding { ty: pt.clone() });
            }
        }
        let body = self.lower_block(&f.body, &fids.body);
        self.scopes.pop();
        // Declared params keep model order even if AST/index were ragged.
        let params: Vec<(String, Ty)> = f
            .params
            .iter()
            .zip(param_tys.iter())
            .map(|(n, t)| (n.clone(), t.clone()))
            .collect();
        // Declaration header for entry-point diagnostics; a missing
        // index entry is hand-off corruption (diagnosed, then fallback).
        let decl_span = self.span(fids.decl);
        Some(TypedFn {
            name: f.name.clone(),
            decl_span,
            params,
            ret: ret_ty,
            body,
        })
    }

    /// Lower a block. Move decisions come only from the recorded sema
    /// set (`moved_uses`); block position plays no role.
    fn lower_block(&mut self, b: &Block, bids: &BlockIds) -> TypedBlock {
        if b.stmts.len() != bids.stmts.len() {
            let fb = self.fallback;
            self.diags.push(
                Diagnostic::new(
                    ErrorCode::ParseError,
                    "internal index mismatch: block statement count differs from index",
                )
                .with_span(fb),
            );
            return TypedBlock {
                stmts: Vec::new(),
                value: Ty::Unit,
            };
        }
        self.scopes.push(HashMap::new());
        let n = b.stmts.len();
        let mut stmts = Vec::with_capacity(n);
        let mut value = Ty::Unit;
        // M4.4 M3: eliminated `assert true` statements do not exist for
        // trailing-value purposes; the block value is determined by the
        // last surviving statement, as if eliminated nodes were absent.
        let last_live = b.stmts.iter().rposition(|st| {
            !matches!(
                st,
                Stmt::Assert {
                    expr: Expr::Bool(true),
                    ..
                }
            )
        });
        for (i, (st, sid)) in b.stmts.iter().zip(bids.stmts.iter()).enumerate() {
            let trailing = Some(i) == last_live;
            match (st, sid) {
                (
                    Stmt::Let { name, expr },
                    StmtIds::Let {
                        node: _,
                        name: _,
                        expr: expr_ids,
                    },
                ) => {
                    let init = self.lower_expr(expr, expr_ids);
                    let ty = init.ty.clone();
                    if let Some(top) = self.scopes.last_mut() {
                        // Shadowing re-lets are fresh and legal.
                        top.insert(name.clone(), Binding { ty: ty.clone() });
                    }
                    stmts.push(TypedStmt::Let {
                        name: name.clone(),
                        ty,
                        init,
                    });
                }
                (Stmt::Do(e), StmtIds::Do { expr: expr_ids, .. }) => {
                    let te = self.lower_expr(e, expr_ids);
                    if trailing {
                        value = te.ty.clone();
                    }
                    stmts.push(TypedStmt::Do { expr: te });
                }
                (
                    Stmt::Block(inner),
                    StmtIds::Block {
                        body: inner_ids, ..
                    },
                ) => {
                    // Nested blocks recurse: value is the inner trailing
                    // `do` type, else `Unit`.
                    let tb = self.lower_block(inner, inner_ids);
                    if trailing {
                        value = tb.value.clone();
                    }
                    stmts.push(TypedStmt::Block { body: tb });
                }
                (Stmt::Empty, StmtIds::Empty(_)) => {
                    stmts.push(TypedStmt::Empty);
                }
                (
                    Stmt::Assert {
                        expr: e,
                        provenance,
                    },
                    StmtIds::Assert {
                        node,
                        expr: expr_ids,
                    },
                ) => {
                    let span = self.span(*node);
                    // M4.4 M3 literal-only folding: bare literals only.
                    // Anything else keeps dynamic operand-first behavior.
                    match e {
                        Expr::Bool(true) => {
                            // Eliminated completely: no node, no fuel,
                            // no diagnostic or trap.
                        }
                        Expr::Bool(false) => {
                            stmts.push(TypedStmt::AssertFalse {
                                span,
                                provenance: provenance.clone(),
                            });
                        }
                        _ => {
                            let te = self.lower_expr(e, expr_ids);
                            stmts.push(TypedStmt::Assert {
                                expr: te,
                                span,
                                provenance: provenance.clone(),
                            });
                        }
                    }
                }
                _ => {
                    let fb = self.fallback;
                    self.diags.push(
                        Diagnostic::new(
                            ErrorCode::ParseError,
                            "internal index mismatch: statement/index kind mismatch",
                        )
                        .with_span(fb),
                    );
                }
            }
        }
        self.scopes.pop();
        TypedBlock { stmts, value }
    }

    /// Lower an expression. A `Local` use is [`LocalAccess::Move`] iff
    /// its use-site [`NodeId`] is in the recorded sema move set
    /// (`moved_uses`); every other use is [`LocalAccess::Copy`].
    /// Lowering performs no independent move inference.
    fn lower_expr(&mut self, e: &Expr, ids: &ExprIds) -> TypedExpr {
        let node_sp = self.span(ids.node);
        match e {
            Expr::Int(v) => TypedExpr {
                ty: Ty::Int,
                span: node_sp,
                kind: ExprKind::Int(*v),
            },
            Expr::Str(s) => TypedExpr {
                ty: Ty::Str,
                span: node_sp,
                kind: ExprKind::Str(s.clone()),
            },
            Expr::Bool(v) => TypedExpr {
                ty: Ty::Bool,
                span: node_sp,
                kind: ExprKind::Bool(*v),
            },
            Expr::Null => TypedExpr {
                ty: Ty::Null,
                span: node_sp,
                kind: ExprKind::Null,
            },
            Expr::Ident(name) => {
                if name == "null" {
                    // Defensive: agree with the parser on the `null` literal.
                    return TypedExpr {
                        ty: Ty::Null,
                        span: node_sp,
                        kind: ExprKind::Null,
                    };
                }
                match self.lookup(name) {
                    None => {
                        let fb = self.fallback;
                        self.diags.push(
                            Diagnostic::new(
                                ErrorCode::ParseError,
                                format!("internal index mismatch: unbound name `{name}`"),
                            )
                            .with_span(fb),
                        );
                        TypedExpr {
                            ty: Ty::Error,
                            span: node_sp,
                            kind: ExprKind::Local {
                                name: name.clone(),
                                access: LocalAccess::Copy,
                            },
                        }
                    }
                    Some(b) => {
                        let access = if self.moved_uses.contains(&ids.node) {
                            LocalAccess::Move
                        } else {
                            LocalAccess::Copy
                        };
                        TypedExpr {
                            ty: b.ty.clone(),
                            span: node_sp,
                            kind: ExprKind::Local {
                                name: name.clone(),
                                access,
                            },
                        }
                    }
                }
            }
            Expr::Unary { op, rhs } => {
                let rhs_ids = match &ids.kind {
                    ExprIdsKind::Unary { rhs, .. } => rhs.clone(),
                    _ => {
                        let fb = self.fallback;
                        self.diags.push(
                            Diagnostic::new(
                                ErrorCode::ParseError,
                                "internal index mismatch: unary/index kind mismatch",
                            )
                            .with_span(fb),
                        );
                        Box::new(ExprIds {
                            node: ids.node,
                            kind: ExprIdsKind::Leaf,
                        })
                    }
                };
                let op_sp = match &ids.kind {
                    ExprIdsKind::Unary { op, .. } => self.span(*op),
                    _ => node_sp,
                };
                let r = self.lower_expr(rhs, &rhs_ids);
                // `Error` propagates without new diagnostics (clean-sema
                // precondition means operand mismatches cannot occur).
                let ty = match op {
                    UnOp::Neg => {
                        if r.ty == Ty::Int {
                            Ty::Int
                        } else {
                            Ty::Error
                        }
                    }
                    UnOp::Not => {
                        if r.ty == Ty::Bool {
                            Ty::Bool
                        } else {
                            Ty::Error
                        }
                    }
                };
                let span = merge_sp(op_sp, r.span);
                TypedExpr {
                    ty,
                    span,
                    kind: ExprKind::Unary {
                        op: *op,
                        rhs: Box::new(r),
                    },
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                let (l_ids, r_ids) = match &ids.kind {
                    ExprIdsKind::Binary { lhs, rhs, .. } => (lhs.clone(), rhs.clone()),
                    _ => {
                        let fb = self.fallback;
                        self.diags.push(
                            Diagnostic::new(
                                ErrorCode::ParseError,
                                "internal index mismatch: binary/index kind mismatch",
                            )
                            .with_span(fb),
                        );
                        (
                            Box::new(ExprIds {
                                node: ids.node,
                                kind: ExprIdsKind::Leaf,
                            }),
                            Box::new(ExprIds {
                                node: ids.node,
                                kind: ExprIdsKind::Leaf,
                            }),
                        )
                    }
                };
                let l = self.lower_expr(lhs, &l_ids);
                let r = self.lower_expr(rhs, &r_ids);
                let ty = binary_ty(*op, &l.ty, &r.ty);
                let span = merge_sp(l.span, r.span);
                TypedExpr {
                    ty,
                    span,
                    kind: ExprKind::Binary {
                        op: *op,
                        lhs: Box::new(l),
                        rhs: Box::new(r),
                    },
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
                        let fb = self.fallback;
                        self.diags.push(
                            Diagnostic::new(
                                ErrorCode::ParseError,
                                "internal index mismatch: call/index kind mismatch",
                            )
                            .with_span(fb),
                        );
                        (ids.node, ids.node, Vec::new())
                    }
                };
                let csp = self.span(callee_id);
                let list_span = self.span(arg_list_id);
                let mut lowered = Vec::with_capacity(args.len());
                if args.len() != arg_ids.len() {
                    let fb = self.fallback;
                    self.diags.push(
                        Diagnostic::new(
                            ErrorCode::ParseError,
                            "internal index mismatch: call argument count differs from index",
                        )
                        .with_span(fb),
                    );
                }
                for (a, a_ids) in args.iter().zip(arg_ids.iter()) {
                    lowered.push(self.lower_expr(a, a_ids));
                }
                // Callee return type from the scope-tree signature.
                let mut ret = Ty::Error;
                if let Some(Ty::Fn { ret: r, .. }) = self.fns.get(callee) {
                    ret = (**r).clone();
                }
                let span = merge_sp(csp, list_span);
                TypedExpr {
                    ty: ret,
                    span,
                    kind: ExprKind::Call {
                        callee: callee.clone(),
                        args: lowered,
                    },
                }
            }
        }
    }
}

fn merge_sp(a: ByteSpan, b: ByteSpan) -> ByteSpan {
    ByteSpan {
        file: a.file,
        start: a.start.min(b.start),
        end: a.end.max(b.end),
    }
}

/// Result type of a binary operator on already-lowered operand types.
/// Mirrors the sema operator table; `Error` propagates without new
/// diagnostics (clean-sema precondition means mismatches cannot occur).
fn binary_ty(op: BinOp, l: &Ty, r: &Ty) -> Ty {
    if *l == Ty::Error || *r == Ty::Error {
        return Ty::Error;
    }
    match op {
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
            if *l == Ty::Int && *r == Ty::Int {
                Ty::Int
            } else {
                Ty::Error
            }
        }
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            if *l == Ty::Int && *r == Ty::Int {
                Ty::Bool
            } else {
                Ty::Error
            }
        }
        BinOp::Or | BinOp::And => {
            if *l == Ty::Bool && *r == Ty::Bool {
                Ty::Bool
            } else {
                Ty::Error
            }
        }
        BinOp::Eq | BinOp::Ne => {
            if l == r {
                Ty::Bool
            } else {
                Ty::Error
            }
        }
    }
}

#[cfg(test)]
mod tests;
