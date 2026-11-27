//! Semantic model: queryable data model (no checking logic).
use aion_span::ByteSpan;
use std::collections::{HashMap, HashSet};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub span: ByteSpan,
}

/// Semantic type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    Int,
    Str,
    Bool,
    Null,
    Unit,
    Fn { params: Vec<Ty>, ret: Box<Ty> },
    Var(u32),
    Error,
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Int => write!(f, "int"),
            Ty::Str => write!(f, "string"),
            Ty::Bool => write!(f, "bool"),
            Ty::Null => write!(f, "null"),
            Ty::Unit => write!(f, "()"),
            Ty::Fn { params, ret } => {
                write!(f, "fn(")?;
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{p}")?;
                }
                write!(f, ") -> {ret}")
            }
            Ty::Var(v) => write!(f, "^{v}"),
            Ty::Error => write!(f, "<error>"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Fn,
    Param,
    Let,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub kind: SymbolKind,
    pub name: String,
    pub ty: Ty,
    pub span: ByteSpan,
    pub moved: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScopeId(pub u32);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope {
    pub parent: Option<ScopeId>,
    pub symbols: HashMap<String, Symbol>,
}

#[derive(Debug, Clone)]
pub struct ScopeTree {
    pub scopes: Vec<Scope>,
    max_id: u32,
}

impl Default for ScopeTree {
    fn default() -> Self {
        Self {
            scopes: Vec::new(),
            max_id: u32::MAX,
        }
    }
}

impl ScopeTree {
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a tree with a bounded ID range. This supports exercising
    /// exhaustion handling without allocating a huge scope vector.
    #[doc(hidden)]
    pub fn with_max_id(max_id: u32) -> Self {
        Self {
            scopes: Vec::new(),
            max_id,
        }
    }
    /// Create the unique module/root scope. Fails if a root already
    /// exists or the ScopeId space is exhausted.
    pub fn root(&mut self) -> Result<ScopeId, ScopeTreeError> {
        if !self.scopes.is_empty() {
            return Err(ScopeTreeError::RootAlreadyExists);
        }
        self.push(None)
    }

    /// Allocate a child scope. The parent must already exist in this tree.
    pub fn fork(&mut self, parent: ScopeId) -> Result<ScopeId, ScopeTreeError> {
        if self.scopes.get(parent.0 as usize).is_none() {
            return Err(ScopeTreeError::InvalidParent(parent));
        }
        self.push(Some(parent))
    }

    fn push(&mut self, parent: Option<ScopeId>) -> Result<ScopeId, ScopeTreeError> {
        let id =
            checked_scope_id(self.scopes.len(), self.max_id).ok_or(ScopeTreeError::IdExhausted)?;
        self.scopes.push(Scope {
            parent,
            symbols: HashMap::new(),
        });
        Ok(id)
    }
    pub fn define(&mut self, scope: ScopeId, sym: Symbol) -> bool {
        match self.scopes.get_mut(scope.0 as usize) {
            Some(s) => {
                if s.symbols.contains_key(&sym.name) {
                    false
                } else {
                    s.symbols.insert(sym.name.clone(), sym);
                    true
                }
            }
            None => false,
        }
    }
    pub fn lookup(&self, scope: ScopeId, name: &str) -> Option<&Symbol> {
        let mut cur = Some(scope);
        while let Some(id) = cur {
            let s = self.scopes.get(id.0 as usize)?;
            if let Some(sym) = s.symbols.get(name) {
                return Some(sym);
            }
            cur = s.parent;
        }
        None
    }
}

/// Why a scope could not be added to a [`ScopeTree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeTreeError {
    RootAlreadyExists,
    InvalidParent(ScopeId),
    IdExhausted,
}

/// Convert the next vector index to a ScopeId without truncation.
fn checked_scope_id(index: usize, max_id: u32) -> Option<ScopeId> {
    let index = u32::try_from(index).ok()?;
    (index <= max_id).then_some(ScopeId(index))
}

#[derive(Debug, Clone, Default)]
pub struct SemModel {
    pub items: Vec<Item>,
    pub scope_tree: ScopeTree,
    pub types: HashMap<aion_syntax::NodeId, Ty>,
    /// M2-recorded move facts: use-site [`NodeId`]s that moved a value
    /// (call argument, `let` initializer, or trailing-return position of
    /// an owned value, after the post-inference soundness filter).
    /// Single writer is `aion-sema`; `aion-ir` lowering consults this set
    /// and performs no independent move inference.
    pub moved_uses: HashSet<aion_syntax::NodeId>,
    /// M4 intent/goal metadata (resolution only; never affects execution).
    pub intents: Vec<IntentEntry>,
    /// Owner [`NodeId`] -> indices into [`SemModel::intents`].
    pub intents_by_owner: HashMap<aion_syntax::NodeId, Vec<usize>>,
    /// Indices into [`SemModel::intents`] with `owner: None` (orphans).
    pub orphans: Vec<usize>,
    /// Block [`NodeId`] -> [`ScopeId`] for every block entered during
    /// checking (arena-containment Stage 1). Single writer is
    /// `aion-sema`; populated only for blocks the statement checker
    /// actually visits (function bodies, never intent bodies). A clean
    /// *checked* result always carries the complete mapping (checked
    /// before publication; parse failures return an empty model with
    /// empty diagnostics, which the driver rejects on parse errors
    /// before touching the model). Dirty checked results may stay
    /// partial because the driver never lowers them.
    pub block_scopes: HashMap<aion_syntax::NodeId, ScopeId>,
}

/// One name occurrence inside a goal expression: the use-site [`NodeId`]
/// plus the declaration [`NodeId`] it resolved to (`None` when unbound).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoalRef {
    pub use_site: aion_syntax::NodeId,
    pub decl: Option<aion_syntax::NodeId>,
}

/// One `goal : <expr>;` statement: its statement [`NodeId`], its
/// expression index, and every name occurrence resolved within it.
#[derive(Debug, Clone)]
pub struct GoalEntry {
    pub stmt: aion_syntax::NodeId,
    pub expr: aion_syntax::ExprIds,
    pub refs: Vec<GoalRef>,
}

/// One explicit top-level `intent` block attached to its owning `fn`
/// item (R-FOLLOWING; `owner: None` marks a retained orphan), carrying
/// only that block's goals — or one synthesized entry per `fn` with
/// body goals (`intent: None`), carrying only the body goals.
#[derive(Debug, Clone)]
pub struct IntentEntry {
    pub owner: Option<aion_syntax::NodeId>,
    pub intent: Option<aion_syntax::NodeId>,
    pub goals: Vec<GoalEntry>,
}

impl SemModel {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn span(s: u32, e: u32) -> ByteSpan {
        ByteSpan {
            file: aion_span::FileId(0),
            start: s,
            end: e,
        }
    }
    fn sym(kind: SymbolKind, name: &str) -> Symbol {
        Symbol {
            kind,
            name: name.into(),
            ty: Ty::Int,
            span: span(0, 1),
            moved: false,
        }
    }
    #[test]
    fn lookup_current_scope() {
        let mut t = ScopeTree::new();
        let r = t.root().unwrap();
        assert!(t.define(r, sym(SymbolKind::Let, "x")));
        assert_eq!(t.lookup(r, "x").unwrap().name, "x");
        assert!(t.lookup(r, "missing").is_none());
    }
    #[test]
    fn child_sees_parent() {
        let mut t = ScopeTree::new();
        let r = t.root().unwrap();
        t.define(r, sym(SymbolKind::Let, "x"));
        let c = t.fork(r).unwrap();
        assert!(t.lookup(c, "x").is_some());
    }
    #[test]
    fn shadowing_prefers_inner() {
        let mut t = ScopeTree::new();
        let r = t.root().unwrap();
        t.define(r, sym(SymbolKind::Let, "x"));
        let c = t.fork(r).unwrap();
        let mut inner = sym(SymbolKind::Let, "x");
        inner.ty = Ty::Bool;
        t.define(c, inner);
        assert_eq!(t.lookup(c, "x").unwrap().ty, Ty::Bool);
        assert_eq!(t.lookup(r, "x").unwrap().ty, Ty::Int);
    }
    #[test]
    fn define_rejects_duplicate() {
        let mut t = ScopeTree::new();
        let r = t.root().unwrap();
        assert!(t.define(r, sym(SymbolKind::Let, "x")));
        assert!(!t.define(r, sym(SymbolKind::Let, "x")));
    }

    #[test]
    fn scope_id_allocation_checks_final_id_and_exhaustion() {
        let final_index = usize::try_from(u32::MAX).unwrap();
        assert_eq!(
            checked_scope_id(final_index, u32::MAX),
            Some(ScopeId(u32::MAX))
        );
        assert_eq!(checked_scope_id(final_index, u32::MAX - 1), None);
        if let Some(next_index) = final_index.checked_add(1) {
            assert_eq!(checked_scope_id(next_index, u32::MAX), None);
        }
    }

    #[test]
    fn exhausted_scope_tree_is_unchanged_and_never_reuses_an_id() {
        let mut t = ScopeTree::with_max_id(0);
        let root = t.root().unwrap();
        assert_eq!(root, ScopeId(0));
        let before = t.scopes.clone();
        assert_eq!(t.fork(root), Err(ScopeTreeError::IdExhausted));
        assert_eq!(t.scopes, before, "failed allocation must not mutate tree");
        assert_eq!(
            t.fork(root),
            Err(ScopeTreeError::IdExhausted),
            "exhaustion must not wrap or return a duplicate id"
        );
        assert_eq!(t.scopes, before);
    }

    #[test]
    fn root_and_fork_reject_duplicate_or_invalid_relationships() {
        let mut t = ScopeTree::new();
        let root = t.root().unwrap();
        let before = t.scopes.clone();
        assert_eq!(t.root(), Err(ScopeTreeError::RootAlreadyExists));
        assert_eq!(
            t.fork(ScopeId(u32::MAX)),
            Err(ScopeTreeError::InvalidParent(ScopeId(u32::MAX)))
        );
        assert_eq!(t.scopes, before, "rejected operations must not mutate tree");
        assert_eq!(root, ScopeId(0));
    }
    #[test]
    fn ty_display() {
        assert_eq!(Ty::Int.to_string(), "int");
        assert_eq!(Ty::Str.to_string(), "string");
        assert_eq!(Ty::Bool.to_string(), "bool");
        assert_eq!(Ty::Null.to_string(), "null");
        assert_eq!(Ty::Unit.to_string(), "()");
        assert!(Ty::Fn {
            params: vec![Ty::Int],
            ret: Box::new(Ty::Bool)
        }
        .to_string()
        .contains("fn"));
    }
}
