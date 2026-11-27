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
//!   values match their recursively derived result type (`Unit` when the
//!   final surviving statement is not a value-producing `do`/block).
//! - WF5 span coverage: every expression carries a real span. The
//!   lowering fallback span (`ByteSpan::empty(FileId(0), 0)`, emitted
//!   only alongside `AION-E0030` index-mismatch diagnostics) is treated
//!   as a dummy/missing span.
//! - WF6 symbol closure: every call callee names a function in this
//!   module. (Local-name closure is WF1.)
//! - WF7 access-kind consistency: `Move` requires a `Str`-typed
//!   (owned) slot not previously moved on the linear path; `Move` on a
//!   non-`Str` slot is a violation, as is `Copy` of a `Str` slot that
//!   was previously moved (lowering never emits either shape).
//! - WF9 move attribution: a live `Local` stamp must match the
//!   structural expectation. `Move` is expected only for a `Str` slot
//!   outside an assertion operand whose root position is a direct call
//!   argument, a `let` initializer, or the function body's last `do`
//!   when that expression's type equals [`TypedFn::ret`]. Every other
//!   live use expects `Copy`. The comparison reads the stamp; it does
//!   not rewrite the module. Precedence on one node is WF1, then WF2,
//!   then WF7, then WF9. A `Move` stamp on a live `Str` still marks the
//!   checker's private slot moved when WF9 also fires, so a later use
//!   at a different span is WF2.
//!
//! Total: the validator never panics on hand-built or adversarial IR.
//! At most one diagnostic is emitted per source span; the first firing
//! rule for that span wins. A single isolated violation yields exactly
//! one `AION-E0030`.
//!
//! - WF8 module scope: function names are unique within the module.
//!   Sema already rejects duplicates on the real pipeline (`AION-E0013`),
//!   so this rule fires only for hand-built or adversarial IR — it is
//!   what stops two `main` definitions from silently executing as one.
//! - WF14 scope identity: module, function, and block scopes are
//!   reconstructed from typed IR structure, and a `ScopeId` may identify
//!   only one scope in the module. WF14 runs after WF1-WF9.
//!
//! Only [`ValidatedModule`] — constructible solely by successful
//! validation — reaches the interpreter (`aion_run::execute`); there
//! is no path that executes unvalidated IR.

use crate::{
    AllocKind, ArenaId, ExprKind, LocalAccess, TypedBlock, TypedExpr, TypedFn, TypedModule,
    TypedStmt,
};
use aion_error::{Diagnostic, ErrorCode};
use aion_sem_model::{ScopeId, Ty};
use aion_span::{ByteSpan, FileId};
use aion_syntax::{BinOp, UnOp};
use std::collections::{hash_map::Entry, HashMap, HashSet};

/// Dummy span: the lowering fallback emitted only with `AION-E0030`.
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

/// Binding provenance tracked alongside [`Slot`] during the Stage 4
/// check traversal (Phase A infrastructure, plan I3).
///
/// Carries only `alloc`: WF11c compares a local read with its binding
/// provenance, and the plan's `ty` and `declaring_scope` fields remain
/// unnecessary. Never consulted by Move/Copy attribution.
#[derive(Debug, Clone, Copy)]
struct ProvenanceSlot {
    alloc: AllocKind,
}

/// A block's derived result: value type, result-expression span when one
/// exists, and storage provenance. Produced once per block by
/// [`derive_block_provenance`]; consumed by WF3/WF4 in the same traversal
/// and retained for the later provenance pass (WF13).
type DerivedTriple = (Ty, Option<ByteSpan>, AllocKind);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScopeKind {
    Module,
    Function,
    Block,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct OccurrenceId(u64);

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum CandidatePhase {
    Expression,
    Table,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum CandidateOrder {
    Expression { sequence: u64 },
    Table { index: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CandidateDependency {
    None,
    CanonicalScope {
        scope: ScopeId,
        occurrence: OccurrenceId,
    },
    ArenaOwnerAndScope {
        arena: ArenaId,
        scope: ScopeId,
        occurrence: OccurrenceId,
    },
}

#[derive(Clone, Debug)]
struct DiagnosticCandidate {
    phase: CandidatePhase,
    order: CandidateOrder,
    occurrence: OccurrenceId,
    dependency: CandidateDependency,
    span: ByteSpan,
    message: String,
    rule: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ExpressionFactKind {
    Literal,
    StringLiteral,
    Local {
        name: String,
        access: LocalAccess,
        resolved_ty: Option<Ty>,
        resolved_alloc: Option<AllocKind>,
    },
    Unary,
    Binary,
    CallResult {
        callee: String,
        argument_count: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ExpressionFact {
    occurrence: OccurrenceId,
    sequence: u64,
    span: ByteSpan,
    scope: ScopeId,
    scope_occurrence: OccurrenceId,
    ty: Ty,
    alloc: AllocKind,
    use_pos: UsePos,
    kind: ExpressionFactKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BindingFact {
    occurrence: OccurrenceId,
    sequence: u64,
    span: ByteSpan,
    scope: ScopeId,
    scope_occurrence: OccurrenceId,
    name: String,
    ty: Ty,
    alloc: AllocKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ArenaReferenceFact {
    occurrence: OccurrenceId,
    sequence: u64,
    span: ByteSpan,
    scope: ScopeId,
    scope_occurrence: OccurrenceId,
    expression: OccurrenceId,
    arena: ArenaId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FunctionReturnFact {
    function_occurrence: OccurrenceId,
    function_scope: ScopeId,
    body_occurrence: OccurrenceId,
    body_scope: ScopeId,
    return_ty: Ty,
    result_sequence: Option<u64>,
    result_occurrence: Option<OccurrenceId>,
    result: Option<DerivedTriple>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BlockResultFact {
    occurrence: OccurrenceId,
    sequence: u64,
    scope: ScopeId,
    scope_occurrence: OccurrenceId,
    span: ByteSpan,
    arena: ArenaId,
}

/// Reconstructed solely from typed IR shape; never populated from sema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScopeNode {
    id: ScopeId,
    kind: ScopeKind,
    parent: Option<ScopeId>,
    function_owner: Option<ScopeId>,
    representative_span: ByteSpan,
    occurrence: OccurrenceId,
}

/// Ordered scope records preserve deterministic traversal and retain
/// duplicate entries for diagnosis. `first_by_id` always points to the
/// first occurrence; collisions never overwrite it.
#[derive(Default)]
struct ReconstructedScopes {
    nodes: Vec<ScopeNode>,
    first_by_id: HashMap<ScopeId, usize>,
    seen_by_id: HashMap<ScopeId, usize>,
}

impl ReconstructedScopes {
    fn record(&mut self, node: ScopeNode, checker: &mut Checker<'_>) {
        let ordinal = *self.seen_by_id.entry(node.id).or_insert(0);
        let occurrence = checker.canonical_scope_occurrence(node.id, ordinal);
        let node = ScopeNode { occurrence, ..node };
        let index = self.nodes.len();
        match self.first_by_id.entry(node.id) {
            Entry::Vacant(entry) => {
                entry.insert(index);
            }
            Entry::Occupied(_) => {
                // Keep the original assignment as the identity owner, while
                // locating the diagnostic at this later conflicting assignment.
                checker.emit_wf14(
                    node.representative_span,
                    format!("scope id {} is assigned to more than one scope", node.id.0),
                );
            }
        }
        *self
            .seen_by_id
            .get_mut(&node.id)
            .expect("scope occurrence ordinal was just inserted") += 1;
        self.nodes.push(node);
    }
}

fn first_block_expression_span(block: &TypedBlock) -> Option<ByteSpan> {
    for stmt in &block.stmts {
        match stmt {
            TypedStmt::Let { init, .. } | TypedStmt::Do { expr: init } => return Some(init.span),
            TypedStmt::Assert { expr, .. } => return Some(expr.span),
            // Folded assertions retain their source expression span.
            TypedStmt::AssertFalse { span, .. } => return Some(*span),
            TypedStmt::Block { body } => {
                if let Some(span) = first_block_expression_span(body) {
                    return Some(span);
                }
            }
            TypedStmt::Empty => {}
        }
    }
    None
}

/// Structural position of one expression node. Only the root of a move
/// site carries a consuming position; nested operands stay [`UsePos::Normal`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UsePos {
    Normal,
    /// Root expression of a direct call argument.
    DirectCallArg,
    /// Root expression of a `let` initializer.
    LetInit,
    /// Root expression of the function body's last `do`.
    FnTrailingDo,
}

/// Single-site recursive block-result derivation (Phase A, plan I3).
///
/// Pure: reads only node fields, binding provenance from the immutable
/// `env` snapshot, and the already-derived child triple supplied by the
/// caller. Mutates nothing, emits nothing, never re-derives: the caller
/// ([`Checker::check_block`]) invokes this exactly once per block during
/// its statement walk, after the final child (if any) has been checked.
/// `child` carries that final child's triple; it is `Some` whenever the
/// final statement is a `Block` (the traversal guarantees the child was
/// checked first). A `None` child with a final `Block` is an invariant
/// violation (D-B): return `None` rather than fabricating a result.
fn derive_block_provenance(
    b: &TypedBlock,
    env: &[HashMap<String, ProvenanceSlot>],
    child: Option<DerivedTriple>,
) -> Option<DerivedTriple> {
    match b.stmts.last() {
        Some(TypedStmt::Do { expr }) => {
            // Copy and Move preserve provenance: a local read carries
            // its binding's provenance. Unbound names (already WF1
            // violations) fall back to Refcounted to stay total.
            let alloc = match &expr.kind {
                ExprKind::Local { name, .. } => env
                    .iter()
                    .rev()
                    .find_map(|scope| scope.get(name))
                    .map(|slot| slot.alloc)
                    .unwrap_or(AllocKind::Refcounted),
                _ => expr.alloc,
            };
            Some((expr.ty.clone(), Some(expr.span), alloc))
        }
        // D-B: a final nested block must reuse its already-derived child.
        // Missing child state is never replaced with a fabricated triple.
        Some(TypedStmt::Block { .. }) => child,
        _ => Some((Ty::Unit, None, AllocKind::Refcounted)),
    }
}

struct Checker<'a> {
    module: &'a TypedModule,
    /// Function signatures in this module (`name -> (params, ret)`).
    fns: HashMap<&'a str, FnSig<'a>>,
    scopes: Vec<HashMap<String, Slot>>,
    /// Parallel provenance environments, pushed/popped in lockstep with
    /// `scopes`. Read by [`derive_block_provenance`]; never consulted
    /// by Move/Copy attribution.
    prov_scopes: Vec<HashMap<String, ProvenanceSlot>>,
    /// Per-block derived results retained for the later provenance pass
    /// (WF13), keyed by block `ScopeId` (unique per module). Written
    /// once per block during the Stage 4 traversal; read by
    /// `check_provenance`.
    retained: HashMap<ScopeId, DerivedTriple>,
    /// Persistent scope-ancestry map (plan I2): first occurrence wins,
    /// mirroring `ReconstructedScopes::first_by_id`. Read by later
    /// Stage 5 provenance checks; populated here so no second
    /// reconstruction is ever needed.
    scope_map: HashMap<ScopeId, ScopeNode>,
    scope_stack: Vec<(ScopeId, OccurrenceId)>,
    scope_occurrences: HashMap<ScopeId, Vec<OccurrenceId>>,
    next_occurrence: u64,
    next_sequence: u64,
    binding_facts: Vec<BindingFact>,
    expression_facts: Vec<ExpressionFact>,
    arena_reference_facts: Vec<ArenaReferenceFact>,
    function_return_facts: Vec<FunctionReturnFact>,
    block_result_facts: Vec<BlockResultFact>,
    candidates: Vec<DiagnosticCandidate>,
    diags: Vec<Diagnostic>,
    /// Source spans already diagnosed (one diagnostic per source span).
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
        let module_occurrence = OccurrenceId(0);
        let mut scope_occurrences = HashMap::new();
        scope_occurrences.insert(module.scope, vec![module_occurrence]);
        Self {
            module,
            fns,
            scopes: Vec::new(),
            prov_scopes: Vec::new(),
            retained: HashMap::new(),
            scope_map: HashMap::new(),
            scope_stack: vec![(module.scope, module_occurrence)],
            scope_occurrences,
            next_occurrence: 1,
            next_sequence: 0,
            binding_facts: Vec::new(),
            expression_facts: Vec::new(),
            arena_reference_facts: Vec::new(),
            function_return_facts: Vec::new(),
            block_result_facts: Vec::new(),
            candidates: Vec::new(),
            diags: Vec::new(),
            flagged: HashSet::new(),
        }
    }

    fn allocate_occurrence(&mut self) -> OccurrenceId {
        let occurrence = OccurrenceId(self.next_occurrence);
        self.next_occurrence += 1;
        occurrence
    }

    fn allocate_sequence(&mut self) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        sequence
    }

    fn record_scope_occurrence(&mut self, scope: ScopeId, occurrence: OccurrenceId) {
        self.scope_occurrences
            .entry(scope)
            .or_default()
            .push(occurrence);
    }

    fn scope_occurrence(&self, scope: ScopeId, ordinal: usize) -> Option<OccurrenceId> {
        self.scope_occurrences
            .get(&scope)
            .and_then(|occurrences| occurrences.get(ordinal))
            .copied()
    }

    fn canonical_scope_occurrence(&mut self, scope: ScopeId, ordinal: usize) -> OccurrenceId {
        if let Some(occurrence) = self.scope_occurrence(scope, ordinal) {
            return occurrence;
        }
        let occurrence = self.allocate_occurrence();
        self.record_scope_occurrence(scope, occurrence);
        occurrence
    }

    fn current_scope(&self) -> ScopeId {
        self.scope_stack
            .last()
            .map(|(scope, _)| *scope)
            .unwrap_or(self.module.scope)
    }

    fn current_scope_occurrence(&self) -> OccurrenceId {
        self.scope_stack
            .last()
            .map(|(_, occurrence)| *occurrence)
            .unwrap_or(OccurrenceId(0))
    }

    fn lookup_provenance(&self, name: &str) -> Option<AllocKind> {
        self.prov_scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).map(|slot| slot.alloc))
    }

    fn capture_binding_fact(&mut self, name: &str, ty: Ty, alloc: AllocKind, span: ByteSpan) {
        let occurrence = self.allocate_occurrence();
        let sequence = self.allocate_sequence();
        self.binding_facts.push(BindingFact {
            occurrence,
            sequence,
            span,
            scope: self.current_scope(),
            scope_occurrence: self.current_scope_occurrence(),
            name: name.to_string(),
            ty,
            alloc,
        });
    }

    fn capture_arena_reference(
        &mut self,
        arena: ArenaId,
        span: ByteSpan,
        expression: OccurrenceId,
    ) {
        let occurrence = self.allocate_occurrence();
        let sequence = self.allocate_sequence();
        self.arena_reference_facts.push(ArenaReferenceFact {
            occurrence,
            sequence,
            span,
            scope: self.current_scope(),
            scope_occurrence: self.current_scope_occurrence(),
            expression,
            arena,
        });
    }

    fn capture_expression_fact(
        &mut self,
        e: &TypedExpr,
        use_pos: UsePos,
        kind: ExpressionFactKind,
    ) {
        let occurrence = self.allocate_occurrence();
        let sequence = self.allocate_sequence();
        let resolved_alloc = match &kind {
            ExpressionFactKind::Local { resolved_alloc, .. } => *resolved_alloc,
            _ => None,
        };
        self.expression_facts.push(ExpressionFact {
            occurrence,
            sequence,
            span: e.span,
            scope: self.current_scope(),
            scope_occurrence: self.current_scope_occurrence(),
            ty: e.ty.clone(),
            alloc: e.alloc,
            use_pos,
            kind,
        });
        if let AllocKind::Arena(arena) = e.alloc {
            self.capture_arena_reference(arena, e.span, occurrence);
        } else if let Some(AllocKind::Arena(arena)) = resolved_alloc {
            self.capture_arena_reference(arena, e.span, occurrence);
        }
    }

    fn capture_function_return(
        &mut self,
        f: &TypedFn,
        function_occurrence: OccurrenceId,
        body_occurrence: OccurrenceId,
        body_start_sequence: u64,
        result: Option<DerivedTriple>,
    ) {
        let result_fact = result
            .as_ref()
            .and_then(|(_, result_span, _)| *result_span)
            .and_then(|result_span| {
                self.expression_facts
                    .iter()
                    .rev()
                    .find(|fact| fact.sequence >= body_start_sequence && fact.span == result_span)
            });
        let result_sequence = result_fact.map(|fact| fact.sequence);
        let result_occurrence = result_fact.map(|fact| fact.occurrence);
        self.function_return_facts.push(FunctionReturnFact {
            function_occurrence,
            function_scope: f.scope,
            body_occurrence,
            body_scope: f.body.scope,
            return_ty: f.ret.clone(),
            result_sequence,
            result_occurrence,
            result,
        });
    }

    fn push_candidate(&mut self, candidate: DiagnosticCandidate) {
        self.candidates.push(candidate);
    }

    fn expression_candidate_from_fact(
        &self,
        fact: &ExpressionFact,
        dependency: CandidateDependency,
        message: &str,
        rule: &str,
    ) -> DiagnosticCandidate {
        DiagnosticCandidate {
            phase: CandidatePhase::Expression,
            order: CandidateOrder::Expression {
                sequence: fact.sequence,
            },
            occurrence: fact.occurrence,
            dependency,
            span: fact.span,
            message: message.to_string(),
            rule: rule.to_string(),
        }
    }

    #[allow(dead_code)]
    fn arena_reference_candidate_from_fact(
        &self,
        fact: &ArenaReferenceFact,
        dependency: CandidateDependency,
        message: &str,
        rule: &str,
    ) -> DiagnosticCandidate {
        DiagnosticCandidate {
            phase: CandidatePhase::Expression,
            order: CandidateOrder::Expression {
                sequence: fact.sequence,
            },
            occurrence: fact.occurrence,
            dependency,
            span: fact.span,
            message: message.to_string(),
            rule: rule.to_string(),
        }
    }

    #[allow(dead_code)]
    fn table_candidate_from_arena(
        &mut self,
        arena: ArenaId,
        dependency: CandidateDependency,
        message: &str,
        rule: &str,
    ) -> Option<DiagnosticCandidate> {
        let span = self.module.arenas.get(arena)?.span;
        self.table_candidate_with_span(arena, span, dependency, message, rule)
    }

    fn table_candidate_with_span(
        &mut self,
        arena: ArenaId,
        span: ByteSpan,
        dependency: CandidateDependency,
        message: &str,
        rule: &str,
    ) -> Option<DiagnosticCandidate> {
        let index = usize::try_from(arena.0).ok()?;
        let occurrence = self.allocate_occurrence();
        Some(DiagnosticCandidate {
            phase: CandidatePhase::Table,
            order: CandidateOrder::Table { index },
            occurrence,
            dependency,
            span,
            message: message.to_string(),
            rule: rule.to_string(),
        })
    }

    fn canonical_scope(&self, scope: ScopeId, occurrence: OccurrenceId) -> bool {
        self.scope_map
            .get(&scope)
            .is_some_and(|node| node.occurrence == occurrence)
    }

    fn arena_owner_is_valid(&self, arena: ArenaId) -> bool {
        let Some(entry) = self.module.arenas.get(arena) else {
            return false;
        };
        let Some(owner) = self.scope_map.get(&entry.owner) else {
            return false;
        };
        if owner.kind != ScopeKind::Block {
            return false;
        }
        owner
            .function_owner
            .and_then(|function| self.scope_map.get(&function))
            .is_some_and(|function| function.kind == ScopeKind::Function)
    }

    fn scope_function(&self, scope: ScopeId) -> Option<ScopeId> {
        let mut current = scope;
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(current) {
                return None;
            }
            let node = self.scope_map.get(&current)?;
            match node.kind {
                ScopeKind::Function => return Some(current),
                ScopeKind::Module => return node.function_owner,
                ScopeKind::Block => current = node.parent?,
            }
        }
    }

    fn scope_contains(&self, scope: ScopeId, ancestor: ScopeId) -> Option<bool> {
        let mut current = Some(scope);
        let mut seen = HashSet::new();
        while let Some(id) = current {
            if !seen.insert(id) {
                return None;
            }
            let node = self.scope_map.get(&id)?;
            if id == ancestor {
                return Some(true);
            }
            current = node.parent;
        }
        Some(false)
    }

    fn candidate_eligible(&self, candidate: &DiagnosticCandidate) -> bool {
        match candidate.dependency {
            CandidateDependency::None => true,
            CandidateDependency::CanonicalScope { scope, occurrence } => {
                self.canonical_scope(scope, occurrence)
            }
            CandidateDependency::ArenaOwnerAndScope {
                arena,
                scope,
                occurrence,
            } => self.arena_owner_is_valid(arena) && self.canonical_scope(scope, occurrence),
        }
    }

    /// Emit one diagnostic for `sp` unless that source span already has one.
    fn emit(&mut self, sp: ByteSpan, msg: String, rule: &str) {
        if self.flagged.insert(span_key(sp)) {
            self.diags.push(e0030(sp, msg, rule));
        }
    }

    /// Emit WF14 after the earlier WF rules and honor spans they have claimed.
    fn emit_wf14(&mut self, sp: ByteSpan, msg: String) {
        let key = span_key(sp);
        if self.flagged.contains(&key) || self.diags.iter().any(|diag| diag.span == Some(sp)) {
            return;
        }
        self.flagged.insert(key);
        self.diags.push(e0030(sp, msg, "WF14"));
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
        let mut wf8_candidates = Vec::new();
        let n = self.module.functions.len();
        for i in 0..n {
            // Clone to avoid aliasing `&mut self` with `&self.module`.
            let f = self.module.functions[i].clone();
            if !seen.insert(f.name.clone()) {
                // Defer WF8 emission until every function has passed
                // through WF1-WF9, so an earlier rule wins a shared span.
                wf8_candidates.push((f.decl_span, format!("duplicate function `{}`", f.name)));
            }
            self.check_fn(&f);
        }
        for (span, message) in wf8_candidates {
            self.emit(span, message, "WF8");
        }
    }

    /// Reconstruct scope ancestry from TypedModule/TypedFn/TypedBlock
    /// structure after WF1-WF9, so WF14 diagnostics follow those rules.
    fn check_scope_structure(&mut self) {
        let mut scopes = ReconstructedScopes::default();
        scopes.record(
            ScopeNode {
                id: self.module.scope,
                kind: ScopeKind::Module,
                parent: None,
                function_owner: None,
                representative_span: self.module.eof_span,
                occurrence: OccurrenceId(0),
            },
            self,
        );

        // Clone the immutable IR slice to allow diagnostic emission on
        // `self` while traversing functions in source order.
        let functions = self.module.functions.clone();
        for function in &functions {
            scopes.record(
                ScopeNode {
                    id: function.scope,
                    kind: ScopeKind::Function,
                    parent: Some(self.module.scope),
                    function_owner: Some(function.scope),
                    representative_span: function.decl_span,
                    occurrence: OccurrenceId(0),
                },
                self,
            );
            record_block_scopes(
                &function.body,
                function.scope,
                Some(function.scope),
                function.decl_span,
                &mut scopes,
                self,
            );
        }
        // I2: persist the ancestry map for later Stage 5 passes. First
        // occurrence wins, mirroring `first_by_id`; duplicates keep
        // their original identity (collisions are still diagnosed).
        for node in &scopes.nodes {
            self.scope_map.entry(node.id).or_insert(*node);
        }
        // Keep the complete reconstruction alive through the pass. The
        // ordered records encode kind, parent, and function ownership;
        // `first_by_id` is only an index and never replaces a collision.
        let _reconstructed = scopes;
    }

    fn check_fn(&mut self, f: &TypedFn) {
        let function_occurrence = self.allocate_occurrence();
        let body_occurrence = self.allocate_occurrence();
        self.record_scope_occurrence(f.scope, function_occurrence);
        self.scope_stack.push((f.scope, function_occurrence));
        self.scopes.push(HashMap::new());
        // Mirror the parameter scope for provenance: every parameter
        // enters as Refcounted (Stage 5 function-boundary policy), keyed
        // under the function scope. Never read by Move/Copy attribution.
        self.prov_scopes.push(HashMap::new());
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
        if let Some(ptop) = self.prov_scopes.last_mut() {
            for (name, _) in &f.params {
                ptop.insert(
                    name.clone(),
                    ProvenanceSlot {
                        alloc: AllocKind::Refcounted,
                    },
                );
            }
        }
        let body_start_sequence = self.next_sequence;
        let body_result = self.check_block_at(&f.body, true, Some(&f.ret), body_occurrence);
        self.capture_function_return(
            f,
            function_occurrence,
            body_occurrence,
            body_start_sequence,
            body_result,
        );
        self.scopes.pop();
        self.prov_scopes.pop();
        self.scope_stack.pop();
    }

    /// Check a block. `is_fn_body`/`ret` enforce WF3 at function level.
    ///
    /// Bottom-up single derivation (Phase 4, per the approved Stage 5
    /// contract): this block's result is derived exactly once here, via
    /// one [`derive_block_provenance`] call, and the outcome is returned
    /// so the parent reuses it for its own derivation. The child's own
    /// invocation verifies its recorded value (approved decision D-A);
    /// this parent never re-checks a child's value. No recursive
    /// traversal of child blocks ever occurs here; a child's result
    /// reaches its parent only through that return. `None` means the
    /// derivation failed closed (D-B): nothing is retained or propagated.
    /// Independent from Move attribution.
    fn check_block(
        &mut self,
        b: &TypedBlock,
        is_fn_body: bool,
        ret: Option<&Ty>,
    ) -> Option<DerivedTriple> {
        let occurrence = self.allocate_occurrence();
        self.check_block_at(b, is_fn_body, ret, occurrence)
    }

    fn check_block_at(
        &mut self,
        b: &TypedBlock,
        is_fn_body: bool,
        ret: Option<&Ty>,
        occurrence: OccurrenceId,
    ) -> Option<DerivedTriple> {
        self.record_scope_occurrence(b.scope, occurrence);
        self.scope_stack.push((b.scope, occurrence));
        self.scopes.push(HashMap::new());
        // Lockstep provenance scope; popped alongside `scopes` below.
        // Never consulted by Move/Copy attribution.
        self.prov_scopes.push(HashMap::new());
        let mut i = 0;
        // Clone statements up front to avoid aliasing `&mut self`.
        let stmts = b.stmts.clone();
        // Triple returned by the final nested `Block` child, if the final
        // statement is one. Only the final child's triple is consumed
        // (for this block's own derivation); earlier children are
        // already verified by their own invocations (D-A).
        let mut last_child: Option<DerivedTriple> = None;
        let mut child_failed = false;
        while i < stmts.len() {
            let st = &stmts[i];
            match st {
                TypedStmt::Let { name, ty, init } => {
                    self.check_expr(init, UsePos::LetInit, false, ret);
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
                    // Mirror the binding for provenance: a `let` keeps the
                    // initializer's provenance (Copy/Move preserve it).
                    // Never read by Move/Copy attribution.
                    if let Some(ptop) = self.prov_scopes.last_mut() {
                        ptop.insert(name.clone(), ProvenanceSlot { alloc: init.alloc });
                    }
                    self.capture_binding_fact(name, ty.clone(), init.alloc, init.span);
                }
                TypedStmt::Do { expr } => {
                    let pos = if is_fn_body && i + 1 == stmts.len() {
                        UsePos::FnTrailingDo
                    } else {
                        UsePos::Normal
                    };
                    self.check_expr(expr, pos, false, ret);
                }
                TypedStmt::Assert { expr, .. } => {
                    // Assertions contribute no block value. Operand reads
                    // are non-moving, including a direct call argument.
                    self.check_expr(expr, UsePos::Normal, true, ret);
                }
                TypedStmt::AssertFalse { .. } => {
                    // Folded false carries no operand; nothing to check
                    // and no block value.
                }
                TypedStmt::Block { body } => {
                    // The child's own invocation verifies its recorded value
                    // (approved decision D-A). Capture the returned triple
                    // only when this child is the final surviving statement;
                    // earlier children are already verified and need no
                    // further handling here.
                    let child_derived = self.check_block(body, false, None);
                    if i + 1 == stmts.len() {
                        child_failed = child_derived.is_none();
                        last_child = child_derived;
                    }
                }
                TypedStmt::Empty => {
                    // A final `Empty` yields Unit per the contract; it
                    // contributes no value. Derivation reads the final
                    // statement literally, so no handling is needed here.
                }
            }
            i += 1;
        }
        let derived = self.finish_block(b, is_fn_body, ret, last_child, child_failed);
        if let Some((_, Some(span), AllocKind::Arena(arena))) = derived.clone() {
            let fact_occurrence = self.allocate_occurrence();
            let sequence = self.allocate_sequence();
            self.block_result_facts.push(BlockResultFact {
                occurrence: fact_occurrence,
                sequence,
                scope: b.scope,
                scope_occurrence: occurrence,
                span,
                arena,
            });
        }
        self.scopes.pop();
        self.prov_scopes.pop();
        self.scope_stack.pop();
        derived
    }

    fn finish_block(
        &mut self,
        b: &TypedBlock,
        is_fn_body: bool,
        ret: Option<&Ty>,
        last_child: Option<DerivedTriple>,
        child_failed: bool,
    ) -> Option<DerivedTriple> {
        let derived = if child_failed {
            None
        } else {
            match derive_block_provenance(b, &self.prov_scopes, last_child) {
                Some(triple) => Some(triple),
                None => {
                    let sp = last_span(b).unwrap_or(ByteSpan::empty(FileId(0), 0));
                    self.emit(
                        sp,
                        "internal block mismatch: missing derived result for final nested block"
                            .to_string(),
                        "internal",
                    );
                    None
                }
            }
        };
        if let Some((expect_ty, expect_sp, expect_alloc)) = derived.clone() {
            // Retain the derived triple for the later provenance pass (WF13),
            // keyed by this block's `ScopeId` (unique per module per WF14).
            self.retained
                .entry(b.scope)
                .or_insert((expect_ty.clone(), expect_sp, expect_alloc));
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
                        // Point at the derived result expression when there is
                        // one; otherwise there is no offending node to span.
                        if let Some(sp) = expect_sp {
                            self.emit(
                                sp,
                                format!(
                                    "function returns `{}` but body value is `{}`",
                                    ret, b.value
                                ),
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
        }
        derived
    }

    fn check_expr(&mut self, e: &TypedExpr, pos: UsePos, in_assert: bool, ret: Option<&Ty>) {
        // WF5 first so a dummy span is reported even if the node is
        // otherwise fine; `emit` dedupes later rules for this source span.
        if is_dummy(e.span) {
            self.emit(e.span, "expression has no recorded span".to_string(), "WF5");
        }
        let fact_kind = match &e.kind {
            ExprKind::Int(_) => {
                if e.ty != Ty::Int {
                    self.emit(e.span, format!("int literal has `{}`", e.ty), "WF4");
                }
                ExpressionFactKind::Literal
            }
            ExprKind::Str(_) => {
                if e.ty != Ty::Str {
                    self.emit(e.span, format!("string literal has `{}`", e.ty), "WF4");
                }
                ExpressionFactKind::StringLiteral
            }
            ExprKind::Bool(_) => {
                if e.ty != Ty::Bool {
                    self.emit(e.span, format!("bool literal has `{}`", e.ty), "WF4");
                }
                ExpressionFactKind::Literal
            }
            ExprKind::Null => {
                if e.ty != Ty::Null {
                    self.emit(e.span, format!("null literal has `{}`", e.ty), "WF4");
                }
                ExpressionFactKind::Literal
            }
            ExprKind::Local { name, access } => {
                let stamped_move = *access == LocalAccess::Move;
                let resolved_slot = self.lookup(name);
                let resolved_ty = resolved_slot.as_ref().map(|slot| slot.ty.clone());
                let resolved_alloc = self.lookup_provenance(name);
                match resolved_slot {
                    None => {
                        self.emit(e.span, format!("use of unbound slot `{name}`"), "WF1");
                    }
                    Some(slot) if slot.moved => {
                        // WF2: any use after a prior move. Precedence stops
                        // here, so a stamp mismatch is not also WF9.
                        self.emit(e.span, format!("use of `{name}` after it was moved"), "WF2");
                    }
                    Some(slot) if stamped_move && slot.ty != Ty::Str => {
                        // WF7: Move of a non-Str slot. Checked before WF9.
                        self.emit(
                            e.span,
                            format!("move of non-owned slot `{name}` of type `{}`", slot.ty),
                            "WF7",
                        );
                    }
                    Some(slot) => {
                        let expect_move = slot.ty == Ty::Str
                            && !in_assert
                            && match pos {
                                UsePos::DirectCallArg | UsePos::LetInit => true,
                                UsePos::FnTrailingDo => ret.is_some_and(|r| e.ty == *r),
                                UsePos::Normal => false,
                            };
                        if stamped_move != expect_move {
                            let stamped = if stamped_move { "Move" } else { "Copy" };
                            let expected = if expect_move { "Move" } else { "Copy" };
                            self.emit(
                                e.span,
                                format!(
                                    "move attribution of `{name}` is {stamped} but this position expects {expected}"
                                ),
                                "WF9",
                            );
                        }
                        // A Move stamp on a live Str consumes the slot even
                        // when the stamp itself is the WF9 violation.
                        if stamped_move && slot.ty == Ty::Str {
                            if let Some(s) = self.lookup_mut(name) {
                                s.moved = true;
                            }
                        }
                    }
                }
                ExpressionFactKind::Local {
                    name: name.clone(),
                    access: *access,
                    resolved_ty,
                    resolved_alloc,
                }
            }
            ExprKind::Unary { op, rhs } => {
                self.check_expr(rhs, UsePos::Normal, in_assert, ret);
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
                ExpressionFactKind::Unary
            }
            ExprKind::Binary { op, lhs, rhs } => {
                self.check_expr(lhs, UsePos::Normal, in_assert, ret);
                self.check_expr(rhs, UsePos::Normal, in_assert, ret);
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
                ExpressionFactKind::Binary
            }
            ExprKind::Call { callee, args } => {
                for a in args {
                    self.check_expr(a, UsePos::DirectCallArg, in_assert, ret);
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
                ExpressionFactKind::CallResult {
                    callee: callee.clone(),
                    argument_count: args.len(),
                }
            }
        };
        self.capture_expression_fact(e, pos, fact_kind);
    }
}

fn record_block_scopes(
    block: &TypedBlock,
    parent: ScopeId,
    function_owner: Option<ScopeId>,
    function_decl_span: ByteSpan,
    scopes: &mut ReconstructedScopes,
    checker: &mut Checker<'_>,
) {
    scopes.record(
        ScopeNode {
            id: block.scope,
            kind: ScopeKind::Block,
            parent: Some(parent),
            function_owner,
            representative_span: first_block_expression_span(block).unwrap_or(function_decl_span),
            occurrence: OccurrenceId(0),
        },
        checker,
    );
    for stmt in &block.stmts {
        if let TypedStmt::Block { body } = stmt {
            record_block_scopes(
                body,
                block.scope,
                function_owner,
                function_decl_span,
                scopes,
                checker,
            );
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

fn collect_wf10_wf13_candidates(checker: &mut Checker<'_>) {
    let expression_facts = checker.expression_facts.clone();
    for fact in expression_facts {
        if fact.ty != Ty::Str && fact.alloc != AllocKind::Refcounted {
            let candidate = checker.expression_candidate_from_fact(
                &fact,
                CandidateDependency::None,
                "non-string expression must use Refcounted provenance",
                "WF10",
            );
            checker.push_candidate(candidate);
        }
        if matches!(&fact.kind, ExpressionFactKind::StringLiteral)
            && fact.alloc != AllocKind::Refcounted
        {
            let candidate = checker.expression_candidate_from_fact(
                &fact,
                CandidateDependency::None,
                "string literal must use Refcounted provenance",
                "WF11",
            );
            checker.push_candidate(candidate);
        }
        if let ExpressionFactKind::Local {
            name,
            resolved_ty,
            resolved_alloc,
            ..
        } = &fact.kind
        {
            if resolved_ty.as_ref() == Some(&Ty::Str) {
                if let Some(binding_alloc) = resolved_alloc {
                    if fact.alloc != *binding_alloc {
                        let candidate = checker.expression_candidate_from_fact(
                            &fact,
                            CandidateDependency::None,
                            &format!("provenance of local '{name}' does not match its binding"),
                            "WF11",
                        );
                        checker.push_candidate(candidate);
                    }
                }
            }
        }
        if fact.use_pos == UsePos::DirectCallArg
            && fact.ty == Ty::Str
            && fact.alloc != AllocKind::Refcounted
        {
            let candidate = checker.expression_candidate_from_fact(
                &fact,
                CandidateDependency::None,
                "arena-backed string cannot be passed as a call argument",
                "WF13",
            );
            checker.push_candidate(candidate);
        }
        if matches!(&fact.kind, ExpressionFactKind::CallResult { .. })
            && fact.ty == Ty::Str
            && fact.alloc != AllocKind::Refcounted
        {
            let candidate = checker.expression_candidate_from_fact(
                &fact,
                CandidateDependency::None,
                "string call result must use Refcounted provenance",
                "WF13",
            );
            checker.push_candidate(candidate);
        }
    }

    let function_return_facts = checker.function_return_facts.clone();
    for fact in function_return_facts {
        if fact.return_ty != Ty::Str {
            continue;
        }
        let (result_span, result_alloc) = match fact.result {
            Some((_, Some(span), alloc)) => (span, alloc),
            _ => continue,
        };
        if result_alloc == AllocKind::Refcounted {
            continue;
        }
        let candidate = DiagnosticCandidate {
            phase: CandidatePhase::Expression,
            order: CandidateOrder::Expression {
                sequence: fact.result_sequence.unwrap_or(u64::MAX),
            },
            occurrence: fact.result_occurrence.unwrap_or(fact.body_occurrence),
            dependency: CandidateDependency::CanonicalScope {
                scope: fact.body_scope,
                occurrence: fact.body_occurrence,
            },
            span: result_span,
            message: "arena-backed string cannot be returned from a function".to_string(),
            rule: "WF13".to_string(),
        };
        checker.push_candidate(candidate);
    }
}

fn collect_wf11b_wf12_candidates(checker: &mut Checker<'_>) {
    let binding_facts = checker.binding_facts.clone();
    for fact in binding_facts {
        let AllocKind::Arena(arena) = fact.alloc else {
            continue;
        };
        if !checker.arena_owner_is_valid(arena)
            || !checker.canonical_scope(fact.scope, fact.scope_occurrence)
        {
            continue;
        }
        let Some(entry) = checker.module.arenas.get(arena) else {
            continue;
        };
        let owner = entry.owner;
        let Some(binding_function) = checker.scope_function(fact.scope) else {
            continue;
        };
        let owner_function = checker
            .scope_map
            .get(&owner)
            .and_then(|node| node.function_owner);
        if owner_function != Some(binding_function) {
            continue;
        }
        match checker.scope_contains(fact.scope, owner) {
            Some(true) | None => continue,
            Some(false) => {}
        }
        let candidate = DiagnosticCandidate {
            phase: CandidatePhase::Expression,
            order: CandidateOrder::Expression {
                sequence: fact.sequence,
            },
            occurrence: fact.occurrence,
            dependency: CandidateDependency::ArenaOwnerAndScope {
                arena,
                scope: fact.scope,
                occurrence: fact.scope_occurrence,
            },
            span: fact.span,
            message: format!("binding '{}' would outlive arena {}", fact.name, arena.0),
            rule: "WF11".to_string(),
        };
        checker.push_candidate(candidate);
    }

    let arena_reference_facts = checker.arena_reference_facts.clone();
    let mut unknown_seen = HashSet::new();
    for fact in &arena_reference_facts {
        if checker.module.arenas.get(fact.arena).is_some() {
            continue;
        }
        if !unknown_seen.insert(fact.arena) {
            continue;
        }
        let candidate = checker.arena_reference_candidate_from_fact(
            fact,
            CandidateDependency::None,
            &format!("unknown arena id {}", fact.arena.0),
            "WF12",
        );
        checker.push_candidate(candidate);
    }

    let mut reference_counts: HashMap<ArenaId, usize> = HashMap::new();
    let mut first_reference_span: HashMap<ArenaId, ByteSpan> = HashMap::new();
    for fact in &arena_reference_facts {
        *reference_counts.entry(fact.arena).or_insert(0) += 1;
        first_reference_span.entry(fact.arena).or_insert(fact.span);
    }

    for index in 0..checker.module.arenas.len() {
        let Ok(id) = u32::try_from(index) else {
            continue;
        };
        let arena = ArenaId(id);
        let Some(entry) = checker.module.arenas.get(arena).cloned() else {
            continue;
        };
        if !checker.arena_owner_is_valid(arena) {
            let span = first_reference_span
                .get(&arena)
                .copied()
                .unwrap_or(entry.span);
            let Some(candidate) = checker.table_candidate_with_span(
                arena,
                span,
                CandidateDependency::None,
                &format!("arena {} has an invalid owner scope", arena.0),
                "WF12",
            ) else {
                continue;
            };
            checker.push_candidate(candidate);
            continue;
        }
        if reference_counts.get(&arena).copied().unwrap_or(0) == 0 {
            let Some(candidate) = checker.table_candidate_with_span(
                arena,
                entry.span,
                CandidateDependency::None,
                &format!("arena {} has no provenance references", arena.0),
                "WF12",
            ) else {
                continue;
            };
            checker.push_candidate(candidate);
        }
    }

    for fact in &arena_reference_facts {
        if !checker.arena_owner_is_valid(fact.arena)
            || !checker.canonical_scope(fact.scope, fact.scope_occurrence)
        {
            continue;
        }
        let Some(entry) = checker.module.arenas.get(fact.arena) else {
            continue;
        };
        let owner = entry.owner;
        let Some(expression_function) = checker.scope_function(fact.scope) else {
            continue;
        };
        let owner_function = checker
            .scope_map
            .get(&owner)
            .and_then(|node| node.function_owner);
        let violates = if owner_function != Some(expression_function) {
            true
        } else {
            match checker.scope_contains(fact.scope, owner) {
                Some(true) => false,
                Some(false) => true,
                None => continue,
            }
        };
        if !violates {
            continue;
        }
        let candidate = checker.arena_reference_candidate_from_fact(
            fact,
            CandidateDependency::ArenaOwnerAndScope {
                arena: fact.arena,
                scope: fact.scope,
                occurrence: fact.scope_occurrence,
            },
            &format!(
                "arena-backed value is used outside owner scope {}",
                fact.arena.0
            ),
            "WF12",
        );
        checker.push_candidate(candidate);
    }

    let block_result_facts = checker.block_result_facts.clone();
    for fact in block_result_facts {
        if !checker.arena_owner_is_valid(fact.arena)
            || !checker.canonical_scope(fact.scope, fact.scope_occurrence)
        {
            continue;
        }
        let Some(entry) = checker.module.arenas.get(fact.arena) else {
            continue;
        };
        let owner = entry.owner;
        if owner != fact.scope {
            let Some(block_function) = checker.scope_function(fact.scope) else {
                continue;
            };
            let owner_function = checker
                .scope_map
                .get(&owner)
                .and_then(|node| node.function_owner);
            if owner_function != Some(block_function) {
                continue;
            }
            if checker.scope_contains(fact.scope, owner) == Some(true) {
                continue;
            }
            if checker.scope_contains(fact.scope, owner).is_none() {
                continue;
            }
        }
        let candidate = DiagnosticCandidate {
            phase: CandidatePhase::Expression,
            order: CandidateOrder::Expression {
                sequence: fact.sequence,
            },
            occurrence: fact.occurrence,
            dependency: CandidateDependency::ArenaOwnerAndScope {
                arena: fact.arena,
                scope: fact.scope,
                occurrence: fact.scope_occurrence,
            },
            span: fact.span,
            message: format!(
                "arena-backed block result escapes owner scope {}",
                fact.arena.0
            ),
            rule: "WF12".to_string(),
        };
        checker.push_candidate(candidate);
    }
}

fn check_provenance(checker: &mut Checker<'_>) {
    let mut selected: Vec<DiagnosticCandidate> = checker
        .candidates
        .iter()
        .filter(|candidate| checker.candidate_eligible(candidate))
        .cloned()
        .collect();
    selected.sort_by_key(|candidate| (candidate.phase, candidate.order, candidate.occurrence.0));
    for candidate in selected {
        checker.emit(candidate.span, candidate.message, &candidate.rule);
    }
}

pub fn validate(module: &TypedModule) -> Result<ValidatedModule, Vec<Diagnostic>> {
    let mut c = Checker::new(module);
    c.check_module();
    c.check_scope_structure();
    collect_wf10_wf13_candidates(&mut c);
    collect_wf11b_wf12_candidates(&mut c);
    check_provenance(&mut c);
    if c.diags.is_empty() {
        Ok(ValidatedModule(module.clone()))
    } else {
        Err(c.diags)
    }
}

#[cfg(test)]
mod phase_a_tests {
    //! Validator tests with hand-built IR (no lowering involved), covering
    //! ancestry/derivation infrastructure and the authorized WF10–WF13
    //! rules.
    use super::*;
    use crate::ArenaId;

    fn sp(start: u32, end: u32) -> ByteSpan {
        ByteSpan::new(FileId(0), start, end).expect("valid span")
    }

    fn int_lit(v: i64, span: ByteSpan) -> TypedExpr {
        TypedExpr {
            ty: Ty::Int,
            span,
            kind: ExprKind::Int(v),
            alloc: AllocKind::Refcounted,
        }
    }

    fn str_lit_arena(span: ByteSpan, id: u32) -> TypedExpr {
        TypedExpr {
            ty: Ty::Str,
            span,
            kind: ExprKind::Str("hi".to_string()),
            alloc: AllocKind::Arena(ArenaId(id)),
        }
    }

    fn local(name: &str, ty: Ty, access: LocalAccess, span: ByteSpan) -> TypedExpr {
        TypedExpr {
            ty,
            span,
            kind: ExprKind::Local {
                name: name.to_string(),
                access,
            },
            alloc: AllocKind::Refcounted,
        }
    }

    fn bool_lit(v: bool, span: ByteSpan) -> TypedExpr {
        TypedExpr {
            ty: Ty::Bool,
            span,
            kind: ExprKind::Bool(v),
            alloc: AllocKind::Refcounted,
        }
    }

    fn call(callee: &str, args: Vec<TypedExpr>, ty: Ty, span: ByteSpan) -> TypedExpr {
        TypedExpr {
            ty,
            span,
            kind: ExprKind::Call {
                callee: callee.to_string(),
                args,
            },
            alloc: AllocKind::Refcounted,
        }
    }

    fn expression_candidate(
        checker: &Checker<'_>,
        sequence: u64,
        occurrence: OccurrenceId,
        dependency: CandidateDependency,
        span: ByteSpan,
        message: &str,
        rule: &str,
    ) -> DiagnosticCandidate {
        let fact = ExpressionFact {
            occurrence,
            sequence,
            span,
            scope: ScopeId(1000),
            scope_occurrence: OccurrenceId(0),
            ty: Ty::Unit,
            alloc: AllocKind::Refcounted,
            use_pos: UsePos::Normal,
            kind: ExpressionFactKind::Literal,
        };
        checker.expression_candidate_from_fact(&fact, dependency, message, rule)
    }

    fn lower_clean(src: &str) -> TypedModule {
        let file = FileId(0);
        let parse = aion_syntax::parse(file, src.as_bytes());
        assert!(parse.errors.is_empty(), "parse errors: {:?}", parse.errors);
        let (diags, model) = aion_sema::check_with_model(&parse, file, src.as_bytes());
        assert!(diags.is_empty(), "sema diags: {:?}", diags);
        crate::lower(&parse, &model, file, src.len() as u32)
            .unwrap_or_else(|diags| panic!("lower failed: {diags:?}"))
    }

    fn env_with(bindings: &[(&str, AllocKind)]) -> Vec<HashMap<String, ProvenanceSlot>> {
        let mut scope = HashMap::new();
        for (name, alloc) in bindings {
            scope.insert(name.to_string(), ProvenanceSlot { alloc: *alloc });
        }
        vec![scope]
    }

    fn block(stmts: Vec<TypedStmt>, value: Ty, scope: u32) -> TypedBlock {
        TypedBlock {
            stmts,
            value,
            scope: ScopeId(scope),
        }
    }

    fn do_stmt(expr: TypedExpr) -> TypedStmt {
        TypedStmt::Do { expr }
    }

    #[test]
    fn derivation_do_int_is_refcounted() {
        let b = block(vec![do_stmt(int_lit(1, sp(4, 5)))], Ty::Int, 7);
        assert_eq!(
            derive_block_provenance(&b, &[], None),
            Some((Ty::Int, Some(sp(4, 5)), AllocKind::Refcounted))
        );
    }

    #[test]
    fn derivation_final_block_reuses_child_triple() {
        // The helper never recurses: the caller supplies the child's
        // already-derived triple, which is returned verbatim.
        let child_body = block(vec![do_stmt(int_lit(1, sp(4, 5)))], Ty::Int, 8);
        let b = block(vec![TypedStmt::Block { body: child_body }], Ty::Int, 7);
        let child = (Ty::Int, Some(sp(4, 5)), AllocKind::Refcounted);
        assert_eq!(
            derive_block_provenance(&b, &[], Some(child.clone())),
            Some(child)
        );
        // Arena provenance rides along unchanged.
        let arena_child = (Ty::Str, Some(sp(4, 5)), AllocKind::Arena(ArenaId(9)));
        let sb = block(
            vec![TypedStmt::Block {
                body: block(vec![], Ty::Str, 8),
            }],
            Ty::Str,
            7,
        );
        assert_eq!(
            derive_block_provenance(&sb, &[], Some(arena_child.clone())),
            Some(arena_child)
        );
    }

    #[test]
    fn derivation_non_do_final_yields_unit() {
        for stmts in [
            vec![TypedStmt::Let {
                name: "x".to_string(),
                ty: Ty::Int,
                init: int_lit(1, sp(10, 11)),
            }],
            vec![TypedStmt::AssertFalse {
                span: sp(10, 12),
                provenance: aion_error::AssertProvenance::human_authored(),
            }],
            vec![TypedStmt::Empty],
            vec![],
        ] {
            let b = block(stmts, Ty::Unit, 7);
            assert_eq!(
                derive_block_provenance(&b, &[], None),
                Some((Ty::Unit, None, AllocKind::Refcounted))
            );
        }
    }

    #[test]
    fn derivation_local_read_uses_binding_provenance() {
        let read = local("s", Ty::Str, LocalAccess::Copy, sp(6, 7));
        let b = block(vec![do_stmt(read)], Ty::Str, 7);
        let env = env_with(&[("s", AllocKind::Arena(ArenaId(3)))]);
        assert_eq!(
            derive_block_provenance(&b, &env, None),
            Some((Ty::Str, Some(sp(6, 7)), AllocKind::Arena(ArenaId(3))))
        );
    }

    #[test]
    fn derivation_unbound_local_falls_back_to_refcounted() {
        // Totality: an unbound name (already a WF1 violation elsewhere)
        // must not panic derivation; it falls back to Refcounted.
        let read = local("missing", Ty::Str, LocalAccess::Copy, sp(6, 7));
        let b = block(vec![do_stmt(read)], Ty::Str, 7);
        assert_eq!(
            derive_block_provenance(&b, &[], None),
            Some((Ty::Str, Some(sp(6, 7)), AllocKind::Refcounted))
        );
    }

    #[test]
    fn derivation_missing_final_child_yields_no_triple() {
        let b = block(
            vec![TypedStmt::Block {
                body: block(vec![], Ty::Int, 8),
            }],
            Ty::Int,
            7,
        );
        assert_eq!(derive_block_provenance(&b, &[], None), None);
    }

    fn nested_module() -> TypedModule {
        // fn main() -> Str { let a: Str = <Arena(5)>; { do a; } do a; }
        // Body and nested block both claim Str; the trailing `do a`
        // is a return Move.
        let mut m = TypedModule::new("t", ScopeId(1000));
        let inner = block(
            vec![do_stmt(local("a", Ty::Str, LocalAccess::Copy, sp(12, 13)))],
            Ty::Str,
            2001,
        );
        m.functions.push(TypedFn {
            name: "main".to_string(),
            scope: ScopeId(1001),
            decl_span: sp(0, 9),
            params: vec![],
            ret: Ty::Str,
            body: block(
                vec![
                    TypedStmt::Let {
                        name: "a".to_string(),
                        ty: Ty::Str,
                        init: str_lit_arena(sp(10, 11), 5),
                    },
                    TypedStmt::Block { body: inner },
                    do_stmt(local("a", Ty::Str, LocalAccess::Move, sp(14, 15))),
                ],
                Ty::Str,
                1002,
            ),
        });
        m
    }

    fn call_module() -> TypedModule {
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions.push(TypedFn {
            name: "id".to_string(),
            scope: ScopeId(1001),
            decl_span: sp(0, 3),
            params: vec![("s".to_string(), Ty::Str)],
            ret: Ty::Str,
            body: block(
                vec![do_stmt(local("s", Ty::Str, LocalAccess::Move, sp(4, 5)))],
                Ty::Str,
                1002,
            ),
        });
        m.functions.push(TypedFn {
            name: "main".to_string(),
            scope: ScopeId(1003),
            decl_span: sp(6, 10),
            params: vec![],
            ret: Ty::Str,
            body: block(
                vec![
                    TypedStmt::Let {
                        name: "s".to_string(),
                        ty: Ty::Str,
                        init: str_lit_arena(sp(11, 12), 5),
                    },
                    do_stmt(call(
                        "id",
                        vec![local("s", Ty::Str, LocalAccess::Move, sp(17, 18))],
                        Ty::Str,
                        sp(16, 19),
                    )),
                ],
                Ty::Str,
                1004,
            ),
        });
        m
    }

    fn duplicate_body_module() -> TypedModule {
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions.push(TypedFn {
            name: "first".to_string(),
            scope: ScopeId(1001),
            decl_span: sp(0, 2),
            params: vec![],
            ret: Ty::Int,
            body: block(vec![do_stmt(int_lit(1, sp(4, 5)))], Ty::Int, 2001),
        });
        m.functions.push(TypedFn {
            name: "second".to_string(),
            scope: ScopeId(1002),
            decl_span: sp(6, 8),
            params: vec![],
            ret: Ty::Bool,
            body: block(vec![do_stmt(bool_lit(true, sp(8, 9)))], Ty::Bool, 2001),
        });
        m
    }

    fn nested_duplicate_module(nested_scope: ScopeId) -> TypedModule {
        let mut m = TypedModule::new("t", ScopeId(1000));
        let inner = block(
            vec![do_stmt(local("a", Ty::Str, LocalAccess::Copy, sp(12, 13)))],
            Ty::Str,
            nested_scope.0,
        );
        m.functions.push(TypedFn {
            name: "main".to_string(),
            scope: ScopeId(1001),
            decl_span: sp(0, 9),
            params: vec![],
            ret: Ty::Str,
            body: block(
                vec![
                    TypedStmt::Let {
                        name: "a".to_string(),
                        ty: Ty::Str,
                        init: str_lit_arena(sp(10, 11), 5),
                    },
                    TypedStmt::Block { body: inner },
                    TypedStmt::Let {
                        name: "b".to_string(),
                        ty: Ty::Str,
                        init: str_lit_arena(sp(16, 17), 6),
                    },
                    do_stmt(local("a", Ty::Str, LocalAccess::Move, sp(18, 19))),
                ],
                Ty::Str,
                1002,
            ),
        });
        m
    }

    #[test]
    fn ancestry_map_records_kinds_parents_and_owners() {
        let m = nested_module();
        let mut c = Checker::new(&m);
        c.check_scope_structure();
        assert!(c.diags.is_empty(), "unexpected diags: {:?}", c.diags);
        let module = c.scope_map.get(&ScopeId(1000)).expect("module");
        assert_eq!(module.kind, ScopeKind::Module);
        assert_eq!(module.parent, None);
        let func = c.scope_map.get(&ScopeId(1001)).expect("function");
        assert_eq!(func.kind, ScopeKind::Function);
        assert_eq!(func.parent, Some(ScopeId(1000)));
        assert_eq!(func.function_owner, Some(ScopeId(1001)));
        let body = c.scope_map.get(&ScopeId(1002)).expect("body");
        assert_eq!(body.kind, ScopeKind::Block);
        assert_eq!(body.parent, Some(ScopeId(1001)));
        assert_eq!(body.function_owner, Some(ScopeId(1001)));
        let nested = c.scope_map.get(&ScopeId(2001)).expect("nested");
        assert_eq!(nested.kind, ScopeKind::Block);
        assert_eq!(nested.parent, Some(ScopeId(1002)));
    }

    #[test]
    fn traversal_retains_binding_provenance_per_block() {
        // End-to-end through the real traversal: the Arena provenance of
        // `let a` flows into the nested block's and body's retained
        // triples, proving the lockstep env works during checking.
        let m = nested_module();
        let mut c = Checker::new(&m);
        c.check_module();
        c.check_scope_structure();
        assert!(c.diags.is_empty(), "unexpected diags: {:?}", c.diags);
        assert_eq!(
            c.retained.get(&ScopeId(2001)),
            Some(&(Ty::Str, Some(sp(12, 13)), AllocKind::Arena(ArenaId(5)))),
            "nested block retains Arena provenance via its binding"
        );
        assert_eq!(
            c.retained.get(&ScopeId(1002)),
            Some(&(Ty::Str, Some(sp(14, 15)), AllocKind::Arena(ArenaId(5)))),
            "body retains Arena provenance via its trailing Move"
        );
    }

    #[test]
    fn duplicate_scope_id_keeps_first_retained_result() {
        let mut m = TypedModule::new("t", ScopeId(1000));
        let first = block(vec![do_stmt(int_lit(1, sp(4, 5)))], Ty::Int, 2001);
        let second = block(vec![do_stmt(int_lit(2, sp(8, 9)))], Ty::Int, 2001);
        m.functions.push(TypedFn {
            name: "main".to_string(),
            scope: ScopeId(1001),
            decl_span: sp(0, 2),
            params: vec![],
            ret: Ty::Unit,
            body: block(
                vec![
                    TypedStmt::Block { body: first },
                    TypedStmt::Block { body: second },
                    TypedStmt::Empty,
                ],
                Ty::Unit,
                1002,
            ),
        });
        let mut c = Checker::new(&m);
        c.check_module();
        c.check_scope_structure();
        assert_eq!(
            c.retained.get(&ScopeId(2001)),
            Some(&(Ty::Int, Some(sp(4, 5)), AllocKind::Refcounted))
        );
        assert_eq!(c.diags.len(), 1, "unexpected diags: {:?}", c.diags);
        assert_eq!(c.diags[0].notes, vec!["ir validator: WF14"]);
        assert_eq!(c.diags[0].span, Some(sp(8, 9)));
    }

    #[test]
    fn nested_value_mismatch_reports_single_wf4() {
        // D-A: the child's own invocation reports the mismatch once; the
        // parent must not repeat it (first-diagnostic-wins would hide a
        // duplicate, so assert exact singularity, span, and rule).
        let inner = block(vec![do_stmt(int_lit(1, sp(4, 5)))], Ty::Bool, 2002);
        let mid = block(vec![TypedStmt::Block { body: inner }], Ty::Int, 2001);
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions.push(TypedFn {
            name: "main".to_string(),
            scope: ScopeId(1001),
            decl_span: sp(0, 9),
            params: vec![],
            ret: Ty::Int,
            body: block(vec![TypedStmt::Block { body: mid }], Ty::Int, 1002),
        });
        let ds = match validate(&m) {
            Ok(_) => panic!("mismatch must not validate"),
            Err(ds) => ds,
        };
        assert_eq!(ds.len(), 1, "exactly one diagnostic: {ds:?}");
        assert_eq!(ds[0].code, ErrorCode::InternalIrInvalid);
        assert_eq!(ds[0].span, Some(sp(4, 5)));
        assert_eq!(ds[0].notes, vec!["ir validator: WF4"]);
    }

    #[test]
    fn missing_final_child_fails_closed_without_fabricated_state() {
        let m = TypedModule::new("t", ScopeId(1000));
        let mut c = Checker::new(&m);
        let b = block(
            vec![TypedStmt::Block {
                body: block(vec![], Ty::Int, 8),
            }],
            Ty::Str,
            7,
        );
        let derived = c.finish_block(&b, true, Some(&Ty::Str), None, false);
        assert_eq!(derived, None, "no derived triple may escape D-B");
        assert!(
            !c.retained.contains_key(&ScopeId(7)),
            "no fabricated triple may be retained"
        );
        assert_eq!(
            c.diags.len(),
            1,
            "only the internal diagnostic may fire: {:?}",
            c.diags
        );
        assert_eq!(c.diags[0].code, ErrorCode::InternalIrInvalid);
        assert_eq!(c.diags[0].notes, vec!["ir validator: internal"]);
        assert!(
            c.diags[0].message.contains("internal block mismatch"),
            "unexpected message: {}",
            c.diags[0].message
        );
    }

    #[test]
    fn failed_final_child_does_not_emit_second_diagnostic() {
        let m = TypedModule::new("t", ScopeId(1000));
        let mut c = Checker::new(&m);
        let b = block(
            vec![TypedStmt::Block {
                body: block(vec![], Ty::Int, 8),
            }],
            Ty::Str,
            7,
        );
        assert_eq!(c.finish_block(&b, true, Some(&Ty::Str), None, true), None);
        assert!(c.diags.is_empty(), "the child already reported the failure");
        assert!(!c.retained.contains_key(&ScopeId(7)));
    }

    #[test]
    fn p1_collects_call_arguments_before_call_result() {
        let m = call_module();
        let mut c = Checker::new(&m);
        c.check_module();
        c.check_scope_structure();
        assert!(c.diags.is_empty(), "unexpected diags: {:?}", c.diags);
        let call = c
            .expression_facts
            .iter()
            .find(|fact| {
                fact.scope == ScopeId(1004)
                    && matches!(
                        &fact.kind,
                        ExpressionFactKind::CallResult { callee, .. } if callee.as_str() == "id"
                    )
            })
            .expect("call-result fact");
        let arg = c
            .expression_facts
            .iter()
            .find(|fact| {
                fact.scope == ScopeId(1004)
                    && matches!(
                        &fact.kind,
                        ExpressionFactKind::Local { name, .. } if name.as_str() == "s"
                    )
            })
            .expect("call-argument fact");
        assert!(arg.sequence < call.sequence);
        assert_ne!(arg.occurrence, call.occurrence);
    }

    #[test]
    fn p1_unary_and_binary_children_precede_parent() {
        let m = TypedModule::new("t", ScopeId(1000));
        let mut c = Checker::new(&m);
        let binary = TypedExpr {
            ty: Ty::Int,
            span: sp(10, 11),
            kind: ExprKind::Binary {
                op: BinOp::Add,
                lhs: Box::new(int_lit(1, sp(2, 3))),
                rhs: Box::new(int_lit(2, sp(4, 5))),
            },
            alloc: AllocKind::Refcounted,
        };
        let unary = TypedExpr {
            ty: Ty::Int,
            span: sp(14, 15),
            kind: ExprKind::Unary {
                op: UnOp::Neg,
                rhs: Box::new(int_lit(3, sp(12, 13))),
            },
            alloc: AllocKind::Refcounted,
        };
        c.check_expr(&binary, UsePos::Normal, false, None);
        c.check_expr(&unary, UsePos::Normal, false, None);
        let seq = |start: u32| {
            c.expression_facts
                .iter()
                .find(|fact| fact.span.start == start)
                .expect("expression fact")
                .sequence
        };
        assert!(seq(2) < seq(4));
        assert!(seq(4) < seq(10));
        assert!(seq(12) < seq(14));
        assert!(seq(10) < seq(12));
    }

    #[test]
    fn p1_captures_binding_and_arena_reference_facts() {
        let m = nested_module();
        let mut c = Checker::new(&m);
        c.check_module();
        assert!(c.diags.is_empty(), "unexpected diags: {:?}", c.diags);
        assert!(c
            .binding_facts
            .iter()
            .any(|fact| { fact.name == "a" && fact.alloc == AllocKind::Arena(ArenaId(5)) }));
        assert!(c
            .arena_reference_facts
            .iter()
            .any(|fact| { fact.arena == ArenaId(5) && fact.span == sp(10, 11) }));
        assert!(c.expression_facts.iter().any(|fact| {
            matches!(
                &fact.kind,
                ExpressionFactKind::Local { name, resolved_alloc: Some(AllocKind::Arena(ArenaId(5))), .. }
                    if name.as_str() == "a"
            )
        }));
    }

    #[test]
    fn p1_function_return_fact_keeps_each_body_occurrence_and_result() {
        let m = duplicate_body_module();
        let mut c = Checker::new(&m);
        c.check_module();
        assert_eq!(c.function_return_facts.len(), 2);
        let first = &c.function_return_facts[0];
        let second = &c.function_return_facts[1];
        assert_eq!(first.function_scope, ScopeId(1001));
        assert_eq!(first.body_scope, ScopeId(2001));
        assert_eq!(
            first.result,
            Some((Ty::Int, Some(sp(4, 5)), AllocKind::Refcounted))
        );
        assert_eq!(second.function_scope, ScopeId(1002));
        assert_eq!(second.body_scope, ScopeId(2001));
        assert_eq!(
            second.result,
            Some((Ty::Bool, Some(sp(8, 9)), AllocKind::Refcounted))
        );
        assert_ne!(first.body_occurrence, second.body_occurrence);
    }

    #[test]
    fn p1_poisoned_function_return_fact_is_not_fabricated() {
        let m = TypedModule::new("t", ScopeId(1000));
        let mut c = Checker::new(&m);
        let f = TypedFn {
            name: "f".to_string(),
            scope: ScopeId(1001),
            decl_span: sp(0, 1),
            params: vec![],
            ret: Ty::Str,
            body: block(vec![], Ty::Str, 1002),
        };
        let function_occurrence = c.allocate_occurrence();
        let body_occurrence = c.allocate_occurrence();
        c.capture_function_return(
            &f,
            function_occurrence,
            body_occurrence,
            c.next_sequence,
            None,
        );
        assert_eq!(c.function_return_facts.len(), 1);
        assert_eq!(
            c.function_return_facts[0].function_occurrence,
            function_occurrence
        );
        assert_eq!(c.function_return_facts[0].body_occurrence, body_occurrence);
        assert_eq!(c.function_return_facts[0].result, None);
        assert!(c.retained.is_empty());
        assert!(c.diags.is_empty());
    }

    #[test]
    fn p1_function_scope_collision_keeps_exact_function_occurrence() {
        let m = nested_duplicate_module(ScopeId(1001));
        let mut c = Checker::new(&m);
        c.check_module();
        c.check_scope_structure();
        let function_occurrences = c
            .scope_occurrences
            .get(&ScopeId(1001))
            .expect("function scope occurrences")
            .clone();
        assert_eq!(function_occurrences.len(), 2);
        let fact = c
            .function_return_facts
            .first()
            .expect("function return fact");
        assert_eq!(fact.function_occurrence, function_occurrences[0]);
        assert_ne!(fact.function_occurrence, function_occurrences[1]);
        assert_eq!(
            fact.body_occurrence,
            c.scope_occurrences
                .get(&ScopeId(1002))
                .expect("body scope occurrences")[0]
        );
    }

    #[test]
    fn p1_body_nested_scope_collision_keeps_exact_body_occurrence() {
        let m = nested_duplicate_module(ScopeId(1002));
        let mut c = Checker::new(&m);
        c.check_module();
        c.check_scope_structure();
        let body_occurrences = c
            .scope_occurrences
            .get(&ScopeId(1002))
            .expect("body scope occurrences")
            .clone();
        assert_eq!(body_occurrences.len(), 2);
        let fact = c
            .function_return_facts
            .first()
            .expect("function return fact");
        assert_eq!(fact.body_occurrence, body_occurrences[0]);
        assert_ne!(fact.body_occurrence, body_occurrences[1]);
        assert!(c.candidate_eligible(&expression_candidate(
            &c,
            10,
            body_occurrences[0],
            CandidateDependency::CanonicalScope {
                scope: ScopeId(1002),
                occurrence: body_occurrences[0],
            },
            sp(18, 19),
            "future function boundary",
            "future-expression",
        )));
    }

    #[test]
    fn p2_facts_keep_exact_containing_scope_occurrence_after_nested_collision() {
        let m = nested_duplicate_module(ScopeId(1002));
        let mut c = Checker::new(&m);
        c.check_module();
        let occurrences = c
            .scope_occurrences
            .get(&ScopeId(1002))
            .expect("body scope occurrences")
            .clone();
        assert_eq!(occurrences.len(), 2);
        let nested = c
            .expression_facts
            .iter()
            .find(|fact| fact.span == sp(12, 13))
            .expect("nested expression fact");
        assert_eq!(nested.scope_occurrence, occurrences[1]);
        let parent_binding = c
            .binding_facts
            .iter()
            .find(|fact| fact.name == "b")
            .expect("parent binding fact");
        assert_eq!(parent_binding.scope, ScopeId(1002));
        assert_eq!(parent_binding.scope_occurrence, occurrences[0]);
        let parent_expression = c
            .expression_facts
            .iter()
            .find(|fact| fact.span == sp(16, 17))
            .expect("parent expression fact");
        assert_eq!(parent_expression.scope, ScopeId(1002));
        assert_eq!(parent_expression.scope_occurrence, occurrences[0]);
        assert_ne!(
            parent_expression.occurrence,
            parent_expression.scope_occurrence
        );
        let parent_reference = c
            .arena_reference_facts
            .iter()
            .find(|fact| fact.arena == ArenaId(6) && fact.span == sp(16, 17))
            .expect("parent arena reference fact");
        assert_eq!(parent_reference.scope, ScopeId(1002));
        assert_eq!(parent_reference.scope_occurrence, occurrences[0]);
    }

    #[test]
    fn p1_duplicate_occurrences_stay_distinct_and_eligibility_is_canonical() {
        let m = duplicate_body_module();
        let mut c = Checker::new(&m);
        c.check_module();
        c.check_scope_structure();
        let occurrences = c
            .scope_occurrences
            .get(&ScopeId(2001))
            .expect("duplicate scope occurrences")
            .clone();
        assert_eq!(occurrences.len(), 2);
        assert_ne!(occurrences[0], occurrences[1]);
        assert_eq!(
            c.scope_map
                .get(&ScopeId(2001))
                .expect("scope map entry")
                .occurrence,
            occurrences[0]
        );
        assert!(c.candidate_eligible(&expression_candidate(
            &c,
            1,
            occurrences[0],
            CandidateDependency::CanonicalScope {
                scope: ScopeId(2001),
                occurrence: occurrences[0],
            },
            sp(20, 21),
            "canonical",
            "future-expression",
        )));
        assert!(!c.candidate_eligible(&expression_candidate(
            &c,
            2,
            occurrences[1],
            CandidateDependency::CanonicalScope {
                scope: ScopeId(2001),
                occurrence: occurrences[1],
            },
            sp(22, 23),
            "duplicate",
            "future-expression",
        )));
    }

    #[test]
    fn p1_collection_creates_no_rule_candidates() {
        let m = nested_module();
        let mut c = Checker::new(&m);
        c.check_module();
        c.check_scope_structure();
        check_provenance(&mut c);
        assert!(c.candidates.is_empty());
        assert!(c.diags.is_empty(), "unexpected diags: {:?}", c.diags);
    }

    #[test]
    fn p2_phase_origin_precedes_span_and_first_span_wins() {
        let mut m = TypedModule::new("t", ScopeId(1000));
        let arena = m
            .arenas
            .alloc(ScopeId(1000), sp(3, 4))
            .expect("arena entry");
        let mut c = Checker::new(&m);
        let later = expression_candidate(
            &c,
            1,
            OccurrenceId(2),
            CandidateDependency::None,
            sp(3, 4),
            "later same span",
            "future-expression-2",
        );
        let first = expression_candidate(
            &c,
            0,
            OccurrenceId(1),
            CandidateDependency::None,
            sp(3, 4),
            "expression phase",
            "future-expression",
        );
        c.push_candidate(later);
        c.push_candidate(first);
        let table = c
            .table_candidate_from_arena(
                arena,
                CandidateDependency::None,
                "table same span",
                "future-table",
            )
            .expect("table candidate");
        c.push_candidate(table);
        check_provenance(&mut c);
        assert_eq!(
            c.diags
                .iter()
                .map(|diag| diag.message.clone())
                .collect::<Vec<_>>(),
            vec!["expression phase"]
        );
        assert_eq!(c.diags[0].notes, vec!["ir validator: future-expression"]);
    }

    #[test]
    fn p2_expression_candidates_follow_captured_sequence_order() {
        let m = call_module();
        let mut c = Checker::new(&m);
        c.check_module();
        let facts = c.expression_facts.clone();
        let call = facts
            .iter()
            .find(|fact| matches!(&fact.kind, ExpressionFactKind::CallResult { .. }))
            .expect("call-result fact");
        let argument = facts
            .iter()
            .find(|fact| fact.span == sp(17, 18))
            .expect("call-argument fact");
        let call_candidate = c.expression_candidate_from_fact(
            call,
            CandidateDependency::None,
            "call result",
            "future-expression",
        );
        let argument_candidate = c.expression_candidate_from_fact(
            argument,
            CandidateDependency::None,
            "argument",
            "future-expression",
        );
        c.push_candidate(call_candidate);
        c.push_candidate(argument_candidate);
        check_provenance(&mut c);
        assert_eq!(
            c.diags
                .iter()
                .map(|diag| diag.message.clone())
                .collect::<Vec<_>>(),
            vec!["argument", "call result"]
        );
    }

    #[test]
    fn p2_arena_reference_candidates_follow_captured_sequence_order() {
        let m = nested_module();
        let mut c = Checker::new(&m);
        c.check_module();
        let facts = c.arena_reference_facts.clone();
        let first = facts
            .iter()
            .find(|fact| fact.span == sp(10, 11))
            .expect("earlier arena reference fact");
        let later = facts
            .iter()
            .find(|fact| fact.span == sp(12, 13))
            .expect("later arena reference fact");
        let later_candidate = c.arena_reference_candidate_from_fact(
            later,
            CandidateDependency::None,
            "later reference",
            "future-table",
        );
        let first_candidate = c.arena_reference_candidate_from_fact(
            first,
            CandidateDependency::None,
            "first reference",
            "future-table",
        );
        c.push_candidate(later_candidate);
        c.push_candidate(first_candidate);
        check_provenance(&mut c);
        assert_eq!(
            c.diags
                .iter()
                .map(|diag| diag.message.clone())
                .collect::<Vec<_>>(),
            vec!["first reference", "later reference"]
        );
    }

    #[test]
    fn p2_table_candidates_follow_arena_table_index_order() {
        let mut m = TypedModule::new("t", ScopeId(1000));
        let entries = [
            (
                m.arenas
                    .alloc(ScopeId(1000), sp(5, 6))
                    .expect("arena entry 0"),
                "table 0",
            ),
            (
                m.arenas
                    .alloc(ScopeId(1000), sp(7, 8))
                    .expect("arena entry 1"),
                "table 1",
            ),
            (
                m.arenas
                    .alloc(ScopeId(1000), sp(9, 10))
                    .expect("arena entry 2"),
                "table 2",
            ),
        ];
        let mut c = Checker::new(&m);
        for (arena, label) in [
            (entries[2].0, entries[2].1),
            (entries[0].0, entries[0].1),
            (entries[1].0, entries[1].1),
        ] {
            let candidate = c
                .table_candidate_from_arena(arena, CandidateDependency::None, label, "future-table")
                .expect("table candidate");
            c.push_candidate(candidate);
        }
        check_provenance(&mut c);
        assert_eq!(
            c.diags
                .iter()
                .map(|diag| diag.message.clone())
                .collect::<Vec<_>>(),
            vec!["table 0", "table 1", "table 2"]
        );
    }

    #[test]
    fn p2_same_span_table_candidates_follow_arena_table_index() {
        let mut m = TypedModule::new("t", ScopeId(1000));
        let entries = [
            m.arenas
                .alloc(ScopeId(1000), sp(30, 31))
                .expect("arena entry 0"),
            m.arenas
                .alloc(ScopeId(1000), sp(30, 31))
                .expect("arena entry 1"),
            m.arenas
                .alloc(ScopeId(1000), sp(30, 31))
                .expect("arena entry 2"),
        ];
        let mut c = Checker::new(&m);
        for (arena, label) in [
            (entries[2], "table 2"),
            (entries[1], "table 1"),
            (entries[0], "table 0"),
        ] {
            let candidate = c
                .table_candidate_from_arena(arena, CandidateDependency::None, label, "future-table")
                .expect("table candidate");
            c.push_candidate(candidate);
        }
        check_provenance(&mut c);
        assert_eq!(
            c.diags
                .iter()
                .map(|diag| diag.message.clone())
                .collect::<Vec<_>>(),
            vec!["table 0"]
        );
    }

    #[test]
    fn p2_emission_is_deterministic_under_candidate_insertion_order() {
        let mut m = TypedModule::new("t", ScopeId(1000));
        let entries = [
            (
                m.arenas
                    .alloc(ScopeId(1000), sp(40, 41))
                    .expect("arena entry 0"),
                "table 0",
            ),
            (
                m.arenas
                    .alloc(ScopeId(1000), sp(42, 43))
                    .expect("arena entry 1"),
                "table 1",
            ),
            (
                m.arenas
                    .alloc(ScopeId(1000), sp(44, 45))
                    .expect("arena entry 2"),
                "table 2",
            ),
        ];
        let emitted = |reverse: bool| {
            let mut c = Checker::new(&m);
            let order: Vec<usize> = if reverse {
                vec![2, 1, 0]
            } else {
                vec![0, 1, 2]
            };
            for idx in order {
                let (arena, label) = entries[idx];
                let candidate = c
                    .table_candidate_from_arena(
                        arena,
                        CandidateDependency::None,
                        label,
                        "future-table",
                    )
                    .expect("table candidate");
                c.push_candidate(candidate);
            }
            check_provenance(&mut c);
            c.diags
                .iter()
                .map(|diag| diag.message.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(emitted(false), emitted(true));
    }

    #[test]
    fn p2_missing_and_duplicate_ancestry_fail_closed() {
        let m = duplicate_body_module();
        let mut c = Checker::new(&m);
        c.check_module();
        c.check_scope_structure();
        let occurrences = c
            .scope_occurrences
            .get(&ScopeId(2001))
            .expect("duplicate scope occurrences")
            .clone();
        let duplicate = expression_candidate(
            &c,
            4,
            occurrences[1],
            CandidateDependency::CanonicalScope {
                scope: ScopeId(2001),
                occurrence: occurrences[1],
            },
            sp(22, 23),
            "duplicate ancestry",
            "future-expression",
        );
        let missing = expression_candidate(
            &c,
            3,
            OccurrenceId(99),
            CandidateDependency::CanonicalScope {
                scope: ScopeId(9999),
                occurrence: OccurrenceId(99),
            },
            sp(24, 25),
            "missing ancestry",
            "future-expression",
        );
        let independent = expression_candidate(
            &c,
            2,
            OccurrenceId(4),
            CandidateDependency::None,
            sp(26, 27),
            "independent",
            "future-expression",
        );
        let canonical = expression_candidate(
            &c,
            1,
            occurrences[0],
            CandidateDependency::CanonicalScope {
                scope: ScopeId(2001),
                occurrence: occurrences[0],
            },
            sp(28, 29),
            "canonical",
            "future-expression",
        );
        c.push_candidate(duplicate);
        c.push_candidate(missing);
        c.push_candidate(independent);
        c.push_candidate(canonical);
        check_provenance(&mut c);
        let emitted = c
            .diags
            .iter()
            .filter(|diag| diag.message != "scope id 2001 is assigned to more than one scope")
            .map(|diag| diag.message.clone())
            .collect::<Vec<_>>();
        assert_eq!(emitted, vec!["canonical", "independent"]);
    }

    #[test]
    fn p2_inconsistent_present_ancestry_fails_closed() {
        let m = duplicate_body_module();
        let mut c = Checker::new(&m);
        c.check_module();
        c.check_scope_structure();
        let captured = c
            .scope_map
            .get(&ScopeId(2001))
            .expect("scope map entry")
            .occurrence;
        if let Some(node) = c.scope_map.get_mut(&ScopeId(2001)) {
            node.occurrence = OccurrenceId(777);
        }
        assert!(!c.candidate_eligible(&expression_candidate(
            &c,
            1,
            captured,
            CandidateDependency::CanonicalScope {
                scope: ScopeId(2001),
                occurrence: captured,
            },
            sp(30, 31),
            "inconsistent ancestry",
            "future-expression",
        )));
        assert!(c.candidate_eligible(&expression_candidate(
            &c,
            2,
            OccurrenceId(8),
            CandidateDependency::None,
            sp(32, 33),
            "independent",
            "future-expression",
        )));
    }

    #[test]
    fn lowered_module_captures_published_scope_and_provenance_facts() {
        let m = lower_clean(
            "fn id(s) { do s == \"x\"; do s; } fn main() { let a = \"hi\"; do id(a); }",
        );
        let mut c = Checker::new(&m);
        c.check_module();
        c.check_scope_structure();
        check_provenance(&mut c);
        assert!(c.diags.is_empty(), "unexpected diags: {:?}", c.diags);
        assert_eq!(c.function_return_facts.len(), 2);
        assert!(c.binding_facts.iter().any(|fact| fact.name == "a"));
        assert!(c.expression_facts.iter().any(|fact| {
            matches!(
                &fact.kind,
                ExpressionFactKind::Local {
                    resolved_alloc: Some(_),
                    ..
                }
            )
        }));
        assert!(c.expression_facts.iter().all(|fact| c
            .scope_map
            .get(&fact.scope)
            .is_some_and(|node| node.occurrence == fact.scope_occurrence)));
        assert!(c.binding_facts.iter().all(|fact| c
            .scope_map
            .get(&fact.scope)
            .is_some_and(|node| node.occurrence == fact.scope_occurrence)));
        assert!(c.arena_reference_facts.iter().all(|fact| c
            .scope_map
            .get(&fact.scope)
            .is_some_and(|node| node.occurrence == fact.scope_occurrence)));
    }

    fn str_lit_refcounted(span: ByteSpan) -> TypedExpr {
        TypedExpr {
            ty: Ty::Str,
            span,
            kind: ExprKind::Str("hi".to_string()),
            alloc: AllocKind::Refcounted,
        }
    }

    fn int_lit_arena(span: ByteSpan, id: u32) -> TypedExpr {
        TypedExpr {
            ty: Ty::Int,
            span,
            kind: ExprKind::Int(1),
            alloc: AllocKind::Arena(ArenaId(id)),
        }
    }

    fn local_with_alloc(
        name: &str,
        ty: Ty,
        access: LocalAccess,
        span: ByteSpan,
        alloc: AllocKind,
    ) -> TypedExpr {
        TypedExpr {
            ty,
            span,
            kind: ExprKind::Local {
                name: name.to_string(),
                access,
            },
            alloc,
        }
    }

    fn call_with_alloc(
        callee: &str,
        args: Vec<TypedExpr>,
        ty: Ty,
        span: ByteSpan,
        alloc: AllocKind,
    ) -> TypedExpr {
        TypedExpr {
            ty,
            span,
            kind: ExprKind::Call {
                callee: callee.to_string(),
                args,
            },
            alloc,
        }
    }

    fn fn_def(
        name: &str,
        scope: u32,
        decl_span: ByteSpan,
        params: Vec<(String, Ty)>,
        ret: Ty,
        body: TypedBlock,
    ) -> TypedFn {
        TypedFn {
            name: name.to_string(),
            scope: ScopeId(scope),
            decl_span,
            params,
            ret,
            body,
        }
    }

    fn rule_diags(m: &TypedModule) -> Vec<Diagnostic> {
        match validate(m) {
            Ok(_) => Vec::new(),
            Err(diags) => diags,
        }
    }

    fn assert_diag(diags: &[Diagnostic], message: &str, note: &str, span: ByteSpan) {
        assert!(
            diags.iter().any(|diag| {
                diag.code == ErrorCode::InternalIrInvalid
                    && diag.message == message
                    && diag.notes == vec![note.to_string()]
                    && diag.span == Some(span)
            }),
            "missing diagnostic {message:?} at {span:?}: {diags:?}"
        );
    }

    #[test]
    fn wf10_non_string_arena_provenance_is_rejected() {
        let mut m = TypedModule::new("t", ScopeId(3000));
        m.functions.push(fn_def(
            "main",
            3001,
            sp(0, 4),
            vec![],
            Ty::Int,
            block(vec![do_stmt(int_lit_arena(sp(5, 6), 7))], Ty::Int, 3002),
        ));
        let diags = rule_diags(&m);
        assert_eq!(diags.len(), 1, "unexpected diagnostics: {diags:?}");
        assert_diag(
            &diags,
            "non-string expression must use Refcounted provenance",
            "ir validator: WF10",
            sp(5, 6),
        );
    }

    #[test]
    fn wf11a_arena_string_literal_is_rejected() {
        let mut m = TypedModule::new("t", ScopeId(3000));
        m.functions.push(fn_def(
            "main",
            3001,
            sp(0, 4),
            vec![],
            Ty::Str,
            block(vec![do_stmt(str_lit_arena(sp(5, 6), 7))], Ty::Str, 3002),
        ));
        let diags = rule_diags(&m);
        assert_eq!(diags.len(), 1, "unexpected diagnostics: {diags:?}");
        assert_diag(
            &diags,
            "string literal must use Refcounted provenance",
            "ir validator: WF11",
            sp(5, 6),
        );
    }

    #[test]
    fn wf11c_local_provenance_mismatch_is_rejected() {
        let mut m = TypedModule::new("t", ScopeId(3000));
        m.functions.push(fn_def(
            "main",
            3001,
            sp(0, 4),
            vec![],
            Ty::Str,
            block(
                vec![
                    TypedStmt::Let {
                        name: "s".to_string(),
                        ty: Ty::Str,
                        init: str_lit_refcounted(sp(5, 6)),
                    },
                    do_stmt(local_with_alloc(
                        "s",
                        Ty::Str,
                        LocalAccess::Move,
                        sp(7, 8),
                        AllocKind::Arena(ArenaId(7)),
                    )),
                ],
                Ty::Str,
                3002,
            ),
        ));
        let diags = rule_diags(&m);
        assert_eq!(diags.len(), 1, "unexpected diagnostics: {diags:?}");
        assert_diag(
            &diags,
            "provenance of local 's' does not match its binding",
            "ir validator: WF11",
            sp(7, 8),
        );
    }

    #[test]
    fn wf13a_arena_string_call_argument_is_rejected() {
        let mut m = TypedModule::new("t", ScopeId(3000));
        m.functions.push(fn_def(
            "id",
            3001,
            sp(0, 3),
            vec![("s".to_string(), Ty::Str)],
            Ty::Str,
            block(
                vec![do_stmt(local("s", Ty::Str, LocalAccess::Move, sp(4, 5)))],
                Ty::Str,
                3002,
            ),
        ));
        m.functions.push(fn_def(
            "main",
            3003,
            sp(6, 10),
            vec![],
            Ty::Str,
            block(
                vec![
                    TypedStmt::Let {
                        name: "s".to_string(),
                        ty: Ty::Str,
                        init: str_lit_arena(sp(11, 12), 5),
                    },
                    do_stmt(call_with_alloc(
                        "id",
                        vec![local_with_alloc(
                            "s",
                            Ty::Str,
                            LocalAccess::Move,
                            sp(17, 18),
                            AllocKind::Arena(ArenaId(5)),
                        )],
                        Ty::Str,
                        sp(16, 19),
                        AllocKind::Refcounted,
                    )),
                ],
                Ty::Str,
                3004,
            ),
        ));
        let diags = rule_diags(&m);
        assert_eq!(diags.len(), 2, "unexpected diagnostics: {diags:?}");
        assert_diag(
            &diags,
            "string literal must use Refcounted provenance",
            "ir validator: WF11",
            sp(11, 12),
        );
        assert_diag(
            &diags,
            "arena-backed string cannot be passed as a call argument",
            "ir validator: WF13",
            sp(17, 18),
        );
    }

    #[test]
    fn wf13b_arena_string_call_result_is_rejected() {
        let mut m = TypedModule::new("t", ScopeId(3000));
        m.functions.push(fn_def(
            "id",
            3001,
            sp(0, 3),
            vec![("s".to_string(), Ty::Str)],
            Ty::Str,
            block(
                vec![do_stmt(local("s", Ty::Str, LocalAccess::Move, sp(4, 5)))],
                Ty::Str,
                3002,
            ),
        ));
        m.functions.push(fn_def(
            "main",
            3003,
            sp(6, 10),
            vec![],
            Ty::Unit,
            block(
                vec![
                    TypedStmt::Let {
                        name: "s".to_string(),
                        ty: Ty::Str,
                        init: str_lit_refcounted(sp(11, 12)),
                    },
                    do_stmt(call_with_alloc(
                        "id",
                        vec![local("s", Ty::Str, LocalAccess::Move, sp(17, 18))],
                        Ty::Str,
                        sp(16, 19),
                        AllocKind::Arena(ArenaId(5)),
                    )),
                    TypedStmt::Empty,
                ],
                Ty::Unit,
                3004,
            ),
        ));
        let diags = rule_diags(&m);
        assert_eq!(diags.len(), 1, "unexpected diagnostics: {diags:?}");
        assert_diag(
            &diags,
            "string call result must use Refcounted provenance",
            "ir validator: WF13",
            sp(16, 19),
        );
    }

    #[test]
    fn wf13b_arena_string_function_return_is_rejected() {
        let mut m = TypedModule::new("t", ScopeId(3000));
        m.functions.push(fn_def(
            "f",
            3001,
            sp(0, 2),
            vec![],
            Ty::Str,
            block(
                vec![
                    TypedStmt::Let {
                        name: "s".to_string(),
                        ty: Ty::Str,
                        init: str_lit_arena(sp(3, 4), 5),
                    },
                    do_stmt(local_with_alloc(
                        "s",
                        Ty::Str,
                        LocalAccess::Move,
                        sp(5, 6),
                        AllocKind::Arena(ArenaId(5)),
                    )),
                ],
                Ty::Str,
                3002,
            ),
        ));
        let diags = rule_diags(&m);
        assert_eq!(diags.len(), 2, "unexpected diagnostics: {diags:?}");
        assert_diag(
            &diags,
            "string literal must use Refcounted provenance",
            "ir validator: WF11",
            sp(3, 4),
        );
        assert_diag(
            &diags,
            "arena-backed string cannot be returned from a function",
            "ir validator: WF13",
            sp(5, 6),
        );
    }

    #[test]
    fn wf13_function_return_uses_canonical_body_occurrence() {
        let mut m = TypedModule::new("t", ScopeId(3000));
        for (name, scope, decl_span, init_span, result_span) in [
            ("first", 3001, sp(0, 2), sp(3, 4), sp(5, 6)),
            ("second", 3003, sp(10, 12), sp(13, 14), sp(15, 16)),
        ] {
            m.functions.push(fn_def(
                name,
                scope,
                decl_span,
                vec![],
                Ty::Str,
                block(
                    vec![
                        TypedStmt::Let {
                            name: "s".to_string(),
                            ty: Ty::Str,
                            init: str_lit_arena(init_span, 5),
                        },
                        do_stmt(local_with_alloc(
                            "s",
                            Ty::Str,
                            LocalAccess::Move,
                            result_span,
                            AllocKind::Arena(ArenaId(5)),
                        )),
                    ],
                    Ty::Str,
                    3002,
                ),
            ));
        }
        let mut c = Checker::new(&m);
        c.check_module();
        c.check_scope_structure();
        collect_wf10_wf13_candidates(&mut c);
        check_provenance(&mut c);
        let return_diags: Vec<_> = c
            .diags
            .iter()
            .filter(|diag| diag.message == "arena-backed string cannot be returned from a function")
            .collect();
        assert_eq!(
            return_diags.len(),
            1,
            "only the canonical body occurrence may emit a return diagnostic: {:?}",
            c.diags
        );
        assert_eq!(return_diags[0].span, Some(sp(5, 6)), ": {:?}", c.diags);
    }

    #[test]
    fn wf11c_copy_mismatch_is_rejected() {
        let mut m = TypedModule::new("t", ScopeId(3000));
        m.functions.push(fn_def(
            "main",
            3001,
            sp(0, 4),
            vec![],
            Ty::Unit,
            block(
                vec![
                    TypedStmt::Let {
                        name: "s".to_string(),
                        ty: Ty::Str,
                        init: str_lit_refcounted(sp(5, 6)),
                    },
                    do_stmt(local_with_alloc(
                        "s",
                        Ty::Str,
                        LocalAccess::Copy,
                        sp(7, 8),
                        AllocKind::Arena(ArenaId(7)),
                    )),
                    TypedStmt::Empty,
                ],
                Ty::Unit,
                3002,
            ),
        ));
        let diags = rule_diags(&m);
        assert_eq!(diags.len(), 1, "unexpected diagnostics: {diags:?}");
        assert_diag(
            &diags,
            "provenance of local 's' does not match its binding",
            "ir validator: WF11",
            sp(7, 8),
        );
    }

    #[test]
    fn wf11c_matching_copy_and_move_reads_validate() {
        let mut m = TypedModule::new("t", ScopeId(3000));
        m.functions.push(fn_def(
            "main",
            3001,
            sp(0, 4),
            vec![],
            Ty::Str,
            block(
                vec![
                    TypedStmt::Let {
                        name: "s".to_string(),
                        ty: Ty::Str,
                        init: str_lit_refcounted(sp(5, 6)),
                    },
                    do_stmt(local("s", Ty::Str, LocalAccess::Copy, sp(7, 8))),
                    do_stmt(local("s", Ty::Str, LocalAccess::Move, sp(9, 10))),
                ],
                Ty::Str,
                3002,
            ),
        ));
        assert!(rule_diags(&m).is_empty(), "unexpected diagnostics");
    }

    #[test]
    fn wf13_call_argument_precedes_call_result() {
        let mut m = TypedModule::new("t", ScopeId(3000));
        m.functions.push(fn_def(
            "id",
            3001,
            sp(0, 3),
            vec![("s".to_string(), Ty::Str)],
            Ty::Str,
            block(
                vec![do_stmt(local("s", Ty::Str, LocalAccess::Move, sp(4, 5)))],
                Ty::Str,
                3002,
            ),
        ));
        m.functions.push(fn_def(
            "main",
            3003,
            sp(6, 10),
            vec![],
            Ty::Unit,
            block(
                vec![
                    TypedStmt::Let {
                        name: "s".to_string(),
                        ty: Ty::Str,
                        init: str_lit_arena(sp(11, 12), 5),
                    },
                    do_stmt(call_with_alloc(
                        "id",
                        vec![local_with_alloc(
                            "s",
                            Ty::Str,
                            LocalAccess::Move,
                            sp(17, 18),
                            AllocKind::Arena(ArenaId(5)),
                        )],
                        Ty::Str,
                        sp(16, 19),
                        AllocKind::Arena(ArenaId(5)),
                    )),
                    TypedStmt::Empty,
                ],
                Ty::Unit,
                3004,
            ),
        ));
        let diags = rule_diags(&m);
        assert_eq!(diags.len(), 3, "unexpected diagnostics: {diags:?}");
        let messages: Vec<_> = diags.iter().map(|diag| diag.message.as_str()).collect();
        assert_eq!(
            messages,
            vec![
                "string literal must use Refcounted provenance",
                "arena-backed string cannot be passed as a call argument",
                "string call result must use Refcounted provenance",
            ]
        );
        assert_diag(
            &diags,
            "string literal must use Refcounted provenance",
            "ir validator: WF11",
            sp(11, 12),
        );
        assert_diag(
            &diags,
            "arena-backed string cannot be passed as a call argument",
            "ir validator: WF13",
            sp(17, 18),
        );
        assert_diag(
            &diags,
            "string call result must use Refcounted provenance",
            "ir validator: WF13",
            sp(16, 19),
        );
    }

    #[test]
    fn wf14_precedes_expression_rules_at_shared_span() {
        let mut m = TypedModule::new("t", ScopeId(3000));
        m.functions.push(fn_def(
            "main",
            3001,
            sp(0, 2),
            vec![],
            Ty::Unit,
            block(
                vec![
                    TypedStmt::Block {
                        body: block(vec![do_stmt(int_lit_arena(sp(3, 4), 7))], Ty::Int, 3100),
                    },
                    TypedStmt::Block {
                        body: block(vec![do_stmt(int_lit_arena(sp(5, 6), 8))], Ty::Int, 3100),
                    },
                    TypedStmt::Empty,
                ],
                Ty::Unit,
                3002,
            ),
        ));
        let diags = rule_diags(&m);
        assert_eq!(diags.len(), 2, "unexpected diagnostics: {diags:?}");
        assert_diag(
            &diags,
            "scope id 3100 is assigned to more than one scope",
            "ir validator: WF14",
            sp(5, 6),
        );
        assert!(
            !diags.iter().any(|diag| {
                diag.span == Some(sp(5, 6))
                    && diag.message == "non-string expression must use Refcounted provenance"
            }),
            "WF10 must not overwrite WF14 at the same span: {diags:?}"
        );
    }

    fn empty_module() -> TypedModule {
        let mut m = TypedModule::new("t", ScopeId(5000));
        m.functions.push(fn_def(
            "main",
            5001,
            sp(0, 4),
            vec![],
            Ty::Unit,
            block(vec![TypedStmt::Empty], Ty::Unit, 5002),
        ));
        m
    }

    fn nested_arena_module() -> TypedModule {
        let inner = block(vec![do_stmt(int_lit(1, sp(10, 11)))], Ty::Int, 5003);
        let mut m = TypedModule::new("t", ScopeId(5000));
        m.functions.push(fn_def(
            "main",
            5001,
            sp(0, 4),
            vec![],
            Ty::Unit,
            block(
                vec![TypedStmt::Block { body: inner }, TypedStmt::Empty],
                Ty::Unit,
                5002,
            ),
        ));
        m
    }

    fn two_fn_module() -> TypedModule {
        let mut m = TypedModule::new("t", ScopeId(5100));
        m.functions.push(fn_def(
            "owner",
            5101,
            sp(0, 5),
            vec![],
            Ty::Int,
            block(vec![do_stmt(int_lit(1, sp(2, 3)))], Ty::Int, 5102),
        ));
        m.functions.push(fn_def(
            "user",
            5103,
            sp(6, 10),
            vec![],
            Ty::Int,
            block(vec![do_stmt(int_lit(2, sp(11, 12)))], Ty::Int, 5104),
        ));
        m
    }

    fn duplicate_owner_module() -> TypedModule {
        let owner = block(vec![do_stmt(int_lit(1, sp(20, 21)))], Ty::Int, 5204);
        let mut m = TypedModule::new("t", ScopeId(5200));
        m.functions.push(fn_def(
            "first",
            5201,
            sp(0, 5),
            vec![],
            Ty::Unit,
            block(
                vec![TypedStmt::Block { body: owner }, TypedStmt::Empty],
                Ty::Unit,
                5202,
            ),
        ));
        m.functions.push(fn_def(
            "second",
            5203,
            sp(6, 11),
            vec![],
            Ty::Int,
            block(vec![do_stmt(int_lit(2, sp(30, 31)))], Ty::Int, 5202),
        ));
        m
    }

    fn prepared_checker<'a>(m: &'a TypedModule) -> Checker<'a> {
        let mut c = Checker::new(m);
        c.check_module();
        c.check_scope_structure();
        c.diags.clear();
        c.flagged.clear();
        c.candidates.clear();
        c.binding_facts.clear();
        c.expression_facts.clear();
        c.arena_reference_facts.clear();
        c.function_return_facts.clear();
        c.block_result_facts.clear();
        c
    }

    fn canonical_occurrence(c: &Checker<'_>, scope: ScopeId) -> OccurrenceId {
        c.scope_map
            .get(&scope)
            .expect("scope in canonical map")
            .occurrence
    }

    fn inject_binding(
        c: &mut Checker<'_>,
        scope: ScopeId,
        span: ByteSpan,
        name: &str,
        arena: ArenaId,
    ) {
        let occurrence = c.allocate_occurrence();
        let sequence = c.allocate_sequence();
        let scope_occurrence = canonical_occurrence(c, scope);
        c.binding_facts.push(BindingFact {
            occurrence,
            sequence,
            span,
            scope,
            scope_occurrence,
            name: name.to_string(),
            ty: Ty::Str,
            alloc: AllocKind::Arena(arena),
        });
    }

    fn inject_arena_ref(
        c: &mut Checker<'_>,
        scope: ScopeId,
        scope_occurrence: Option<OccurrenceId>,
        span: ByteSpan,
        arena: ArenaId,
    ) -> OccurrenceId {
        let occurrence = c.allocate_occurrence();
        let sequence = c.allocate_sequence();
        let scope_occurrence = scope_occurrence.unwrap_or_else(|| canonical_occurrence(c, scope));
        c.arena_reference_facts.push(ArenaReferenceFact {
            occurrence,
            sequence,
            span,
            scope,
            scope_occurrence,
            expression: occurrence,
            arena,
        });
        occurrence
    }

    fn inject_block_result(c: &mut Checker<'_>, scope: ScopeId, span: ByteSpan, arena: ArenaId) {
        let occurrence = c.allocate_occurrence();
        let sequence = c.allocate_sequence();
        let scope_occurrence = canonical_occurrence(c, scope);
        c.block_result_facts.push(BlockResultFact {
            occurrence,
            sequence,
            scope,
            scope_occurrence,
            span,
            arena,
        });
    }

    fn collect_and_emit(c: &mut Checker<'_>) {
        collect_wf11b_wf12_candidates(c);
        check_provenance(c);
    }

    fn diagnostic_messages(c: &Checker<'_>) -> Vec<String> {
        c.diags
            .iter()
            .map(|diag| diag.message.clone())
            .collect::<Vec<_>>()
    }

    #[test]
    fn wf12a_valid_same_function_arena_reference_is_clean() {
        let mut m = empty_module();
        let arena = m
            .arenas
            .alloc(ScopeId(5002), sp(20, 21))
            .expect("arena entry");
        let mut c = prepared_checker(&m);
        inject_arena_ref(&mut c, ScopeId(5002), None, sp(30, 31), arena);
        collect_and_emit(&mut c);
        assert!(c.diags.is_empty(), "unexpected diagnostics: {:?}", c.diags);
        assert_eq!(c.arena_reference_facts.len(), 1);
    }

    #[test]
    fn wf12a_unknown_arena_id_is_rejected_at_first_reference() {
        let m = empty_module();
        let mut c = prepared_checker(&m);
        inject_arena_ref(&mut c, ScopeId(5002), None, sp(30, 31), ArenaId(99));
        inject_arena_ref(&mut c, ScopeId(5002), None, sp(32, 33), ArenaId(99));
        collect_and_emit(&mut c);
        assert_eq!(c.diags.len(), 1, "unexpected diagnostics: {:?}", c.diags);
        assert_diag(
            &c.diags,
            "unknown arena id 99",
            "ir validator: WF12",
            sp(30, 31),
        );
    }

    #[test]
    fn wf12a_owner_validity_rejects_missing_nonblock_and_unresolved_function_owners() {
        for owner in [ScopeId(9999), ScopeId(5001), ScopeId(5000)] {
            let mut m = empty_module();
            let arena = m.arenas.alloc(owner, sp(20, 21)).expect("arena entry");
            let mut c = prepared_checker(&m);
            inject_arena_ref(&mut c, ScopeId(5002), None, sp(30, 31), arena);
            collect_and_emit(&mut c);
            assert_eq!(c.diags.len(), 1, "owner {owner:?}: {:?}", c.diags);
            assert_diag(
                &c.diags,
                &format!("arena {} has an invalid owner scope", arena.0),
                "ir validator: WF12",
                sp(30, 31),
            );
        }
    }

    #[test]
    fn wf12a_invalid_owner_uses_entry_span_without_reference() {
        let mut m = empty_module();
        let arena = m
            .arenas
            .alloc(ScopeId(9999), sp(20, 21))
            .expect("arena entry");
        let mut c = prepared_checker(&m);
        collect_and_emit(&mut c);
        assert_eq!(c.diags.len(), 1, "unexpected diagnostics: {:?}", c.diags);
        assert_diag(
            &c.diags,
            &format!("arena {} has an invalid owner scope", arena.0),
            "ir validator: WF12",
            sp(20, 21),
        );
    }

    #[test]
    fn wf12b_cross_function_reference_uses_expression_span_and_keeps_entry_valid() {
        let mut m = two_fn_module();
        let arena = m
            .arenas
            .alloc(ScopeId(5102), sp(20, 21))
            .expect("arena entry");
        let mut c = prepared_checker(&m);
        inject_arena_ref(&mut c, ScopeId(5104), None, sp(40, 41), arena);
        collect_and_emit(&mut c);
        assert_eq!(c.diags.len(), 1, "unexpected diagnostics: {:?}", c.diags);
        assert_diag(
            &c.diags,
            &format!("arena-backed value is used outside owner scope {}", arena.0),
            "ir validator: WF12",
            sp(40, 41),
        );
        assert!(
            !c.diags
                .iter()
                .any(|diag| diag.message.contains("has an invalid owner scope")),
            "cross-function reference must not invalidate the entry: {:?}",
            c.diags
        );
    }

    #[test]
    fn wf12b_nested_block_reference_within_owner_is_clean() {
        let mut m = nested_arena_module();
        let arena = m
            .arenas
            .alloc(ScopeId(5002), sp(20, 21))
            .expect("arena entry");
        let mut c = prepared_checker(&m);
        inject_arena_ref(&mut c, ScopeId(5003), None, sp(12, 13), arena);
        collect_and_emit(&mut c);
        assert!(c.diags.is_empty(), "unexpected diagnostics: {:?}", c.diags);
    }

    #[test]
    fn wf12b_parent_reference_to_child_owner_is_rejected() {
        let mut m = nested_arena_module();
        let arena = m
            .arenas
            .alloc(ScopeId(5003), sp(20, 21))
            .expect("arena entry");
        let mut c = prepared_checker(&m);
        inject_arena_ref(&mut c, ScopeId(5002), None, sp(40, 41), arena);
        collect_and_emit(&mut c);
        assert_eq!(c.diags.len(), 1, "unexpected diagnostics: {:?}", c.diags);
        assert_diag(
            &c.diags,
            &format!("arena-backed value is used outside owner scope {}", arena.0),
            "ir validator: WF12",
            sp(40, 41),
        );
    }

    #[test]
    fn wf11b_binding_outlive_is_rejected_at_initializer_span() {
        let mut m = nested_arena_module();
        let arena = m
            .arenas
            .alloc(ScopeId(5003), sp(20, 21))
            .expect("arena entry");
        let mut c = prepared_checker(&m);
        inject_binding(&mut c, ScopeId(5002), sp(8, 9), "a", arena);
        inject_arena_ref(&mut c, ScopeId(5003), None, sp(12, 13), arena);
        collect_and_emit(&mut c);
        assert_eq!(c.diags.len(), 1, "unexpected diagnostics: {:?}", c.diags);
        assert_diag(
            &c.diags,
            &format!("binding 'a' would outlive arena {}", arena.0),
            "ir validator: WF11",
            sp(8, 9),
        );
    }

    #[test]
    fn wf11b_ancestor_owned_binding_is_valid() {
        let mut m = nested_arena_module();
        let arena = m
            .arenas
            .alloc(ScopeId(5002), sp(20, 21))
            .expect("arena entry");
        let mut c = prepared_checker(&m);
        inject_binding(&mut c, ScopeId(5003), sp(8, 9), "a", arena);
        inject_arena_ref(&mut c, ScopeId(5003), None, sp(12, 13), arena);
        collect_and_emit(&mut c);
        assert!(c.diags.is_empty(), "unexpected diagnostics: {:?}", c.diags);
    }

    #[test]
    fn wf12c_equal_scope_block_result_escape_is_rejected() {
        let mut m = empty_module();
        let arena = m
            .arenas
            .alloc(ScopeId(5002), sp(20, 21))
            .expect("arena entry");
        let mut c = prepared_checker(&m);
        inject_arena_ref(&mut c, ScopeId(5002), None, sp(30, 31), arena);
        inject_block_result(&mut c, ScopeId(5002), sp(30, 31), arena);
        collect_and_emit(&mut c);
        assert_eq!(c.diags.len(), 1, "unexpected diagnostics: {:?}", c.diags);
        assert_diag(
            &c.diags,
            &format!("arena-backed block result escapes owner scope {}", arena.0),
            "ir validator: WF12",
            sp(30, 31),
        );
    }

    #[test]
    fn wf12c_child_owned_block_result_candidate_exists_and_containment_wins() {
        let mut m = nested_arena_module();
        let arena = m
            .arenas
            .alloc(ScopeId(5003), sp(20, 21))
            .expect("arena entry");
        let mut c = prepared_checker(&m);
        inject_arena_ref(&mut c, ScopeId(5002), None, sp(30, 31), arena);
        inject_block_result(&mut c, ScopeId(5002), sp(30, 31), arena);
        collect_wf11b_wf12_candidates(&mut c);
        assert!(
            c.candidates.iter().any(|candidate| {
                candidate.rule == "WF12"
                    && candidate.message
                        == format!("arena-backed block result escapes owner scope {}", arena.0)
            }),
            "WF12c candidate must be captured: {:?}",
            c.candidates
        );
        check_provenance(&mut c);
        assert_eq!(c.diags.len(), 1, "unexpected diagnostics: {:?}", c.diags);
        assert_diag(
            &c.diags,
            &format!("arena-backed value is used outside owner scope {}", arena.0),
            "ir validator: WF12",
            sp(30, 31),
        );
    }

    #[test]
    fn wf12d_valid_owner_orphan_is_rejected_at_entry_span() {
        let mut m = empty_module();
        let arena = m
            .arenas
            .alloc(ScopeId(5002), sp(20, 21))
            .expect("arena entry");
        let mut c = prepared_checker(&m);
        collect_and_emit(&mut c);
        assert_eq!(c.diags.len(), 1, "unexpected diagnostics: {:?}", c.diags);
        assert_diag(
            &c.diags,
            &format!("arena {} has no provenance references", arena.0),
            "ir validator: WF12",
            sp(20, 21),
        );
    }

    #[test]
    fn d1_invalid_owner_suppresses_orphan_but_keeps_invalid_owner() {
        let mut m = empty_module();
        let arena = m
            .arenas
            .alloc(ScopeId(9999), sp(20, 21))
            .expect("arena entry");
        let mut c = prepared_checker(&m);
        collect_and_emit(&mut c);
        assert_eq!(c.diags.len(), 1, "unexpected diagnostics: {:?}", c.diags);
        assert_eq!(
            diagnostic_messages(&c),
            vec![format!("arena {} has an invalid owner scope", arena.0)]
        );
        assert!(
            !c.diags
                .iter()
                .any(|diag| diag.message.contains("has no provenance references")),
            "D1 must suppress the orphan: {:?}",
            c.diags
        );
    }

    #[test]
    fn d1_invalid_owner_reference_counting_uses_first_reference_span() {
        let mut m = empty_module();
        let arena = m
            .arenas
            .alloc(ScopeId(9999), sp(20, 21))
            .expect("arena entry");
        let mut c = prepared_checker(&m);
        inject_arena_ref(&mut c, ScopeId(5002), None, sp(30, 31), arena);
        inject_arena_ref(&mut c, ScopeId(5002), None, sp(32, 33), arena);
        collect_and_emit(&mut c);
        assert_eq!(c.diags.len(), 1, "unexpected diagnostics: {:?}", c.diags);
        assert_diag(
            &c.diags,
            &format!("arena {} has an invalid owner scope", arena.0),
            "ir validator: WF12",
            sp(30, 31),
        );
    }

    #[test]
    fn d_d_suppresses_owner_dependent_checks_for_invalid_owner() {
        let mut m = nested_arena_module();
        let arena = m
            .arenas
            .alloc(ScopeId(9999), sp(20, 21))
            .expect("arena entry");
        let mut c = prepared_checker(&m);
        inject_binding(&mut c, ScopeId(5002), sp(8, 9), "a", arena);
        inject_arena_ref(&mut c, ScopeId(5002), None, sp(30, 31), arena);
        inject_block_result(&mut c, ScopeId(5002), sp(30, 31), arena);
        collect_and_emit(&mut c);
        assert_eq!(
            diagnostic_messages(&c),
            vec![format!("arena {} has an invalid owner scope", arena.0)],
            "unexpected diagnostics: {:?}",
            c.diags
        );
    }

    #[test]
    fn d_arch4_invalid_owner_does_not_suppress_wf13_boundary_checks() {
        let mut m = empty_module();
        let arena = m
            .arenas
            .alloc(ScopeId(9999), sp(20, 21))
            .expect("arena entry");
        let mut c = prepared_checker(&m);
        inject_arena_ref(&mut c, ScopeId(5002), None, sp(50, 51), arena);
        let wf13 = expression_candidate(
            &c,
            0,
            OccurrenceId(2),
            CandidateDependency::None,
            sp(30, 31),
            "arena-backed string cannot be returned from a function",
            "WF13",
        );
        c.push_candidate(wf13);
        collect_and_emit(&mut c);
        assert_eq!(c.diags.len(), 2, "unexpected diagnostics: {:?}", c.diags);
        assert_diag(
            &c.diags,
            "arena-backed string cannot be returned from a function",
            "ir validator: WF13",
            sp(30, 31),
        );
        assert_diag(
            &c.diags,
            &format!("arena {} has an invalid owner scope", arena.0),
            "ir validator: WF12",
            sp(50, 51),
        );
    }

    #[test]
    fn d2_same_span_table_entries_use_lowest_index() {
        let mut m = empty_module();
        let first = m
            .arenas
            .alloc(ScopeId(5002), sp(30, 31))
            .expect("arena entry 0");
        let second = m
            .arenas
            .alloc(ScopeId(5002), sp(30, 31))
            .expect("arena entry 1");
        assert_eq!(first, ArenaId(0));
        assert_eq!(second, ArenaId(1));
        let diags = rule_diags(&m);
        assert_eq!(diags.len(), 1, "unexpected diagnostics: {diags:?}");
        assert_diag(
            &diags,
            "arena 0 has no provenance references",
            "ir validator: WF12",
            sp(30, 31),
        );
    }

    #[test]
    fn expression_phase_precedes_table_phase_at_same_span() {
        let mut m = empty_module();
        m.arenas
            .alloc(ScopeId(5002), sp(30, 31))
            .expect("arena entry");
        let mut c = prepared_checker(&m);
        let expression = expression_candidate(
            &c,
            0,
            OccurrenceId(2),
            CandidateDependency::None,
            sp(30, 31),
            "future expression",
            "future-expression",
        );
        c.push_candidate(expression);
        collect_and_emit(&mut c);
        assert_eq!(c.diags.len(), 1, "unexpected diagnostics: {:?}", c.diags);
        assert_diag(
            &c.diags,
            "future expression",
            "ir validator: future-expression",
            sp(30, 31),
        );
    }

    #[test]
    fn duplicate_occurrence_owner_dependent_checks_fail_closed() {
        let mut m = duplicate_owner_module();
        let arena = m
            .arenas
            .alloc(ScopeId(5204), sp(20, 21))
            .expect("arena entry");
        let mut c = prepared_checker(&m);
        let occurrences = c
            .scope_occurrences
            .get(&ScopeId(5202))
            .expect("duplicate body occurrences")
            .clone();
        assert_eq!(occurrences.len(), 2);
        inject_arena_ref(
            &mut c,
            ScopeId(5202),
            Some(occurrences[1]),
            sp(30, 31),
            arena,
        );
        inject_arena_ref(
            &mut c,
            ScopeId(5202),
            Some(occurrences[0]),
            sp(40, 41),
            arena,
        );
        collect_and_emit(&mut c);
        assert_eq!(
            diagnostic_messages(&c),
            vec![format!(
                "arena-backed value is used outside owner scope {}",
                arena.0
            )],
            "unexpected diagnostics: {:?}",
            c.diags
        );
        assert_diag(
            &c.diags,
            &format!("arena-backed value is used outside owner scope {}", arena.0),
            "ir validator: WF12",
            sp(40, 41),
        );
    }

    #[test]
    fn ambiguous_ancestry_suppresses_owner_dependent_expression_checks() {
        let mut m = empty_module();
        let arena = m
            .arenas
            .alloc(ScopeId(5004), sp(20, 21))
            .expect("arena entry");
        let mut c = prepared_checker(&m);
        let function = c.scope_map.get_mut(&ScopeId(5001)).expect("function scope");
        function.parent = Some(ScopeId(5002));
        let block = c.scope_map.get_mut(&ScopeId(5002)).expect("block scope");
        block.parent = Some(ScopeId(5001));
        c.scope_map.insert(
            ScopeId(5004),
            ScopeNode {
                id: ScopeId(5004),
                kind: ScopeKind::Block,
                parent: Some(ScopeId(5001)),
                function_owner: Some(ScopeId(5001)),
                representative_span: sp(20, 21),
                occurrence: OccurrenceId(900),
            },
        );
        inject_binding(&mut c, ScopeId(5001), sp(30, 31), "v", arena);
        inject_arena_ref(&mut c, ScopeId(5001), None, sp(40, 41), arena);
        collect_and_emit(&mut c);
        assert!(c.diags.is_empty(), "unexpected diagnostics: {:?}", c.diags);
    }

    #[test]
    fn inconsistent_ancestry_fails_closed_but_independent_table_checks_continue() {
        let mut m = empty_module();
        let referenced = m
            .arenas
            .alloc(ScopeId(5002), sp(20, 21))
            .expect("referenced arena");
        m.arenas
            .alloc(ScopeId(5002), sp(40, 41))
            .expect("orphan arena");
        let mut c = prepared_checker(&m);
        let captured = canonical_occurrence(&c, ScopeId(5002));
        if let Some(node) = c.scope_map.get_mut(&ScopeId(5002)) {
            node.occurrence = OccurrenceId(777);
        }
        inject_arena_ref(
            &mut c,
            ScopeId(5002),
            Some(captured),
            sp(30, 31),
            referenced,
        );
        collect_and_emit(&mut c);
        assert_eq!(
            diagnostic_messages(&c),
            vec!["arena 1 has no provenance references".to_string()],
            "unexpected diagnostics: {:?}",
            c.diags
        );
    }

    #[test]
    fn deterministic_repeated_validation_is_byte_stable() {
        let mut m = empty_module();
        m.arenas
            .alloc(ScopeId(9999), sp(20, 21))
            .expect("invalid owner entry");
        m.arenas
            .alloc(ScopeId(5002), sp(30, 31))
            .expect("orphan entry");
        let summary = |diags: &[Diagnostic]| {
            diags
                .iter()
                .map(|diag| (diag.message.clone(), diag.span, diag.notes.clone()))
                .collect::<Vec<_>>()
        };
        let first = summary(&rule_diags(&m));
        let second = summary(&rule_diags(&m));
        assert_eq!(first, second);
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].0, "arena 0 has an invalid owner scope");
        assert_eq!(first[1].0, "arena 1 has no provenance references");
    }
}
