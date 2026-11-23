//! M3.3 IR validator: well-formedness checks over [`TypedModule`].
//!
//! Rules (violations yield `AION-E0030`, never any other code):
//!
//! - WF1 slots-defined-before-use: every `Local` name must be bound by a
//!   parameter or a preceding `let` visible at the use.
//! - WF2 single-move: no use (`Copy` or `Move`) of a slot after a prior
//!   `Move` of the same slot. Straight-line IR, so the path is the
//!   statement sequence (nested blocks walked inline, in order).
//! - WF3 structural completeness: every function body value type must
//!   equal the declared return type.
//! - WF4 type consistency: rechecks each node against the SPEC Sec 4
//!   operator table (unary/binary operand types imply the result type),
//!   `let` initializer types match the declared slot type, call arity
//!   matches the callee's `Fn` type in this module, and nested block
//!   values match their trailing expression type (`Unit` otherwise).
//! - WF5 span coverage: every expression carries a real span. The
//!   lowering fallback span (`ByteSpan::empty(FileId(0), 0)`, emitted
//!   only alongside `AION-E0004` index-mismatch diagnostics) is treated
//!   as a dummy/missing span.
//! - WF6 symbol closure: every call callee names a function in this
//!   module. (Local-name closure is WF1.)
//! - WF7 access-kind consistency: `Move` requires a `Str`-typed
//!   (owned) slot not previously moved on the linear path; `Move` on a
//!   non-`Str` slot is a violation, as is `Copy` of a `Str` slot that
//!   was previously moved (lowering never emits either shape).
//!
//! Total: the validator never panics on hand-built or adversarial IR.
//! At most one diagnostic is emitted per expression node (the first
//! firing rule wins), so a single isolated violation yields exactly one
//! `AION-E0030`.
//!
//! - WF8 module scope: function names are unique within the module.
//!   Sema already rejects duplicates on the real pipeline (`AION-E0013`),
//!   so this rule fires only for hand-built or adversarial IR — it is
//!   what stops two `main` definitions from silently executing as one.
//!
//! Only [`ValidatedModule`] — constructible solely by successful
//! validation — reaches the interpreter (`aion_run::execute`); there
//! is no path that executes unvalidated IR.

use crate::{ExprKind, LocalAccess, TypedBlock, TypedExpr, TypedFn, TypedModule, TypedStmt};
use aion_error::{Diagnostic, ErrorCode};
use aion_sem_model::Ty;
use aion_span::{ByteSpan, FileId};
use aion_syntax::{BinOp, UnOp};
use std::collections::{HashMap, HashSet};

/// Dummy span: the lowering fallback emitted only with `AION-E0004`.
/// The validator treats it as missing span coverage (WF5).
fn is_dummy(sp: ByteSpan) -> bool {
    sp == ByteSpan::empty(FileId(0), 0)
}

/// Expected result type of a unary operator, mirroring lowering.
fn unary_ty(op: UnOp, rhs: &Ty) -> Ty {
    match op {
        UnOp::Neg => {
            if *rhs == Ty::Int {
                Ty::Int
            } else {
                Ty::Error
            }
        }
        UnOp::Not => {
            if *rhs == Ty::Bool {
                Ty::Bool
            } else {
                Ty::Error
            }
        }
    }
}

/// Expected result type of a binary operator, mirroring lowering.
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

#[derive(Debug, Clone)]
struct Slot {
    ty: Ty,
    moved: bool,
}

/// A function signature in the module under validation.
type FnSig<'a> = (&'a Vec<(String, Ty)>, &'a Ty);

struct Checker<'a> {
    module: &'a TypedModule,
    /// Function signatures in this module (`name -> (params, ret)`).
    fns: HashMap<&'a str, FnSig<'a>>,
    scopes: Vec<HashMap<String, Slot>>,
    diags: Vec<Diagnostic>,
    /// Expression spans already diagnosed (one diagnostic per node).
    flagged: HashSet<(u32, u32, u32)>,
}

fn span_key(sp: ByteSpan) -> (u32, u32, u32) {
    (sp.file.0, sp.start, sp.end)
}

fn e0030(sp: ByteSpan, msg: String, rule: &str) -> Diagnostic {
    Diagnostic::new(ErrorCode::InternalIrInvalid, msg)
        .with_span(sp)
        .with_note(format!("ir validator: {rule}"))
}

impl<'a> Checker<'a> {
    fn new(module: &'a TypedModule) -> Self {
        let mut fns = HashMap::new();
        for f in &module.functions {
            // First definition wins; duplicates are still closed names.
            fns.entry(f.name.as_str()).or_insert((&f.params, &f.ret));
        }
        Self {
            module,
            fns,
            scopes: Vec::new(),
            diags: Vec::new(),
            flagged: HashSet::new(),
        }
    }

    /// Emit one diagnostic for `sp` unless that node already has one.
    fn emit(&mut self, sp: ByteSpan, msg: String, rule: &str) {
        if self.flagged.insert(span_key(sp)) {
            self.diags.push(e0030(sp, msg, rule));
        }
    }

    fn lookup(&self, name: &str) -> Option<Slot> {
        for scope in self.scopes.iter().rev() {
            if let Some(s) = scope.get(name) {
                return Some(s.clone());
            }
        }
        None
    }

    fn lookup_mut(&mut self, name: &str) -> Option<&mut Slot> {
        for scope in self.scopes.iter_mut().rev() {
            if scope.contains_key(name) {
                return scope.get_mut(name);
            }
        }
        None
    }

    fn check_module(&mut self) {
        // WF8: function names are unique. Sema enforces this on the real
        // pipeline (AION-E0013), so a violation here means hand-built or
        // adversarial IR — e.g. two `main`s, where executing the first
        // silently would be wrong. Span: the second definition's decl.
        let mut seen: HashSet<String> = HashSet::new();
        let n = self.module.functions.len();
        for i in 0..n {
            // Clone to avoid aliasing `&mut self` with `&self.module`.
            let f = self.module.functions[i].clone();
            if !seen.insert(f.name.clone()) {
                self.diags.push(e0030(
                    f.decl_span,
                    format!("duplicate function `{}`", f.name),
                    "WF8",
                ));
            }
            self.check_fn(&f);
        }
    }

    fn check_fn(&mut self, f: &TypedFn) {
        self.scopes.push(HashMap::new());
        if let Some(top) = self.scopes.last_mut() {
            for (name, ty) in &f.params {
                top.insert(
                    name.clone(),
                    Slot {
                        ty: ty.clone(),
                        moved: false,
                    },
                );
            }
        }
        self.check_block(&f.body, true, Some(&f.ret));
        self.scopes.pop();
    }

    /// Check a block. `is_fn_body`/`ret` enforce WF3 at function level.
    fn check_block(&mut self, b: &TypedBlock, is_fn_body: bool, ret: Option<&Ty>) {
        self.scopes.push(HashMap::new());
        let mut trailing_ty: Option<(Ty, ByteSpan)> = None;
        let mut trailing_is_do = false;
        let mut i = 0;
        // Clone statements up front to avoid aliasing `&mut self`.
        let stmts = b.stmts.clone();
        while i < stmts.len() {
            let st = &stmts[i];
            match st {
                TypedStmt::Let { name, ty, init } => {
                    self.check_expr(init);
                    if init.ty != *ty {
                        self.emit(
                            init.span,
                            format!(
                                "let `{name}` declares `{ty}` but initializer has `{}`",
                                init.ty
                            ),
                            "WF4",
                        );
                    }
                    if let Some(top) = self.scopes.last_mut() {
                        top.insert(
                            name.clone(),
                            Slot {
                                ty: ty.clone(),
                                moved: false,
                            },
                        );
                    }
                    trailing_ty = None;
                    trailing_is_do = false;
                }
                TypedStmt::Do { expr } => {
                    self.check_expr(expr);
                    trailing_ty = Some((expr.ty.clone(), expr.span));
                    trailing_is_do = i + 1 == stmts.len();
                }
                TypedStmt::Assert { expr, .. } => {
                    // Assertions contribute no block value.
                    self.check_expr(expr);
                    trailing_ty = None;
                    trailing_is_do = false;
                }
                TypedStmt::AssertFalse { .. } => {
                    // Folded false carries no operand; nothing to check
                    // and no block value.
                    trailing_ty = None;
                    trailing_is_do = false;
                }
                TypedStmt::Block { body } => {
                    self.check_block(body, false, None);
                    trailing_ty = None;
                    trailing_is_do = false;
                    // WF4: nested block value consistency, keyed on the
                    // trailing expression span when one exists.
                    let (expect_ty, expect_sp) = match body.stmts.last() {
                        Some(TypedStmt::Do { expr }) => (expr.ty.clone(), Some(expr.span)),
                        _ => (Ty::Unit, None),
                    };
                    if body.value != expect_ty {
                        if let Some(sp) = expect_sp {
                            self.emit(
                                sp,
                                format!(
                                    "block value `{}` does not match trailing type `{expect_ty}`",
                                    body.value
                                ),
                                "WF4",
                            );
                        } else if body.value != Ty::Unit {
                            // No trailing node to blame; use the last
                            // statement span when available, else skip
                            // (nothing to point at — still total).
                            if let Some(sp) = last_span(body) {
                                self.emit(
                                    sp,
                                    format!(
                                        "block value `{}` must be `()` without trailing `do`",
                                        body.value
                                    ),
                                    "WF4",
                                );
                            }
                        }
                    }
                }
                TypedStmt::Empty => {
                    if i + 1 == stmts.len() {
                        trailing_ty = None;
                        trailing_is_do = false;
                    }
                }
            }
            i += 1;
        }
        // WF3/WF4: block value vs trailing type and (fn body) vs ret.
        let (expect_ty, expect_sp) = match trailing_ty.clone() {
            Some((t, sp)) if trailing_is_do => (t, Some(sp)),
            _ => (Ty::Unit, None),
        };
        if b.value != expect_ty {
            if let Some(sp) = expect_sp {
                let rule = if is_fn_body { "WF3" } else { "WF4" };
                self.emit(
                    sp,
                    format!(
                        "block value `{}` does not match trailing type `{expect_ty}`",
                        b.value
                    ),
                    rule,
                );
            } else if b.value != Ty::Unit {
                if let Some(sp) = last_span(b) {
                    let rule = if is_fn_body { "WF3" } else { "WF4" };
                    self.emit(
                        sp,
                        format!(
                            "block value `{}` must be `()` without trailing `do`",
                            b.value
                        ),
                        rule,
                    );
                }
            }
        }
        if is_fn_body {
            if let Some(ret) = ret {
                if b.value != *ret {
                    // Point at the trailing expression when there is one;
                    // otherwise there is no offending node to span.
                    if let Some((_, sp)) = trailing_ty.clone() {
                        self.emit(
                            sp,
                            format!("function returns `{}` but body value is `{}`", ret, b.value),
                            "WF3",
                        );
                    } else if let Some(sp) = last_span(b) {
                        self.emit(
                            sp,
                            format!("function returns `{ret}` but body has no trailing value"),
                            "WF3",
                        );
                    } else if b.value != *ret {
                        // Empty body, no spans at all: use the dummy span
                        // so the violation is still reported (total).
                        self.emit(
                            ByteSpan::empty(FileId(0), 0),
                            format!("function returns `{ret}` but body has no trailing value"),
                            "WF3",
                        );
                    }
                }
            }
        }
        self.scopes.pop();
    }

    fn check_expr(&mut self, e: &TypedExpr) {
        // WF5 first so a dummy span is reported even if the node is
        // otherwise fine; `emit` dedupes later rules on this node.
        if is_dummy(e.span) {
            self.emit(e.span, "expression has no recorded span".to_string(), "WF5");
        }
        match &e.kind {
            ExprKind::Int(_) => {
                if e.ty != Ty::Int {
                    self.emit(e.span, format!("int literal has `{}`", e.ty), "WF4");
                }
            }
            ExprKind::Str(_) => {
                if e.ty != Ty::Str {
                    self.emit(e.span, format!("string literal has `{}`", e.ty), "WF4");
                }
            }
            ExprKind::Bool(_) => {
                if e.ty != Ty::Bool {
                    self.emit(e.span, format!("bool literal has `{}`", e.ty), "WF4");
                }
            }
            ExprKind::Null => {
                if e.ty != Ty::Null {
                    self.emit(e.span, format!("null literal has `{}`", e.ty), "WF4");
                }
            }
            ExprKind::Local { name, access } => {
                match self.lookup(name) {
                    None => {
                        self.emit(e.span, format!("use of unbound slot `{name}`"), "WF1");
                    }
                    Some(slot) => {
                        if slot.moved {
                            // WF2: any use after a prior move.
                            self.emit(e.span, format!("use of `{name}` after it was moved"), "WF2");
                        } else {
                            match access {
                                LocalAccess::Move => {
                                    if slot.ty != Ty::Str {
                                        // WF7: lowering never emits Move
                                        // of a Copy-typed slot.
                                        self.emit(
                                            e.span,
                                            format!(
                                                "move of non-owned slot `{name}` of type `{}`",
                                                slot.ty
                                            ),
                                            "WF7",
                                        );
                                    } else if let Some(s) = self.lookup_mut(name) {
                                        s.moved = true;
                                    }
                                }
                                LocalAccess::Copy => {
                                    // Copy of never-moved Str is fine
                                    // (a borrow); Copy after move is
                                    // already reported as WF2 above.
                                }
                            }
                        }
                    }
                }
            }
            ExprKind::Unary { op, rhs } => {
                self.check_expr(rhs);
                let expect = unary_ty(*op, &rhs.ty);
                if e.ty != expect {
                    self.emit(
                        e.span,
                        format!(
                            "unary `{op:?}` on `{}` yields `{expect}` but node has `{}`",
                            rhs.ty, e.ty
                        ),
                        "WF4",
                    );
                }
            }
            ExprKind::Binary { op, lhs, rhs } => {
                self.check_expr(lhs);
                self.check_expr(rhs);
                let expect = binary_ty(*op, &lhs.ty, &rhs.ty);
                if e.ty != expect {
                    self.emit(
                        e.span,
                        format!(
                            "binary `{op:?}` on `{}` and `{}` yields `{expect}` but node has `{}`",
                            lhs.ty, rhs.ty, e.ty
                        ),
                        "WF4",
                    );
                }
            }
            ExprKind::Call { callee, args } => {
                for a in args {
                    self.check_expr(a);
                }
                match self.fns.get(callee.as_str()) {
                    None => {
                        self.emit(
                            e.span,
                            format!("call to undefined function `{callee}`"),
                            "WF6",
                        );
                    }
                    Some((params, _)) => {
                        if args.len() != params.len() {
                            self.emit(
                                e.span,
                                format!(
                                    "call to `{callee}` takes {} args but got {}",
                                    params.len(),
                                    args.len()
                                ),
                                "WF4",
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Last expression span in a block, if any (for pointing block-level
/// violations at an offending node).
fn last_span(b: &TypedBlock) -> Option<ByteSpan> {
    for st in b.stmts.iter().rev() {
        match st {
            TypedStmt::Do { expr } => return Some(expr.span),
            TypedStmt::Assert { span, .. } => return Some(*span),
            TypedStmt::AssertFalse { span, .. } => return Some(*span),
            TypedStmt::Let { init, .. } => return Some(init.span),
            TypedStmt::Block { body } => {
                if let Some(sp) = last_span(body) {
                    return Some(sp);
                }
            }
            TypedStmt::Empty => {}
        }
    }
    None
}

/// A typed module that passed validation. The field is private so the
/// only construction path is successful [`validate`]; the interpreter
/// accepts `&ValidatedModule`, which makes executing unvalidated IR a
/// compile-time impossibility (there is no `From<TypedModule>` impl and
/// no other constructor).
#[derive(Debug, Clone)]
pub struct ValidatedModule(TypedModule);

impl ValidatedModule {
    /// Borrow the validated inner module.
    pub fn module(&self) -> &TypedModule {
        &self.0
    }
}

/// Validate a typed module, returning the [`ValidatedModule`] on success
/// or the `AION-E0030` diagnostics on failure.
///
/// Total: never panics, even on hand-built or adversarial IR.
pub fn validate(module: &TypedModule) -> Result<ValidatedModule, Vec<Diagnostic>> {
    let mut c = Checker::new(module);
    c.check_module();
    if c.diags.is_empty() {
        Ok(ValidatedModule(module.clone()))
    } else {
        Err(c.diags)
    }
}
