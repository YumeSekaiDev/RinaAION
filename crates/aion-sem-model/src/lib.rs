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

#[derive(Debug, Clone, Default)]
pub struct ScopeTree {
    pub scopes: Vec<Scope>,
}

impl ScopeTree {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn root(&mut self) -> ScopeId {
        self.push(None)
    }
    fn push(&mut self, parent: Option<ScopeId>) -> ScopeId {
        let id = ScopeId(self.scopes.len() as u32);
        self.scopes.push(Scope {
            parent,
            symbols: HashMap::new(),
        });
        id
    }
    pub fn fork(&mut self, parent: ScopeId) -> ScopeId {
        self.push(Some(parent))
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
        let r = t.root();
        assert!(t.define(r, sym(SymbolKind::Let, "x")));
        assert_eq!(t.lookup(r, "x").unwrap().name, "x");
        assert!(t.lookup(r, "missing").is_none());
    }
    #[test]
    fn child_sees_parent() {
        let mut t = ScopeTree::new();
        let r = t.root();
        t.define(r, sym(SymbolKind::Let, "x"));
        let c = t.fork(r);
        assert!(t.lookup(c, "x").is_some());
    }
    #[test]
    fn shadowing_prefers_inner() {
        let mut t = ScopeTree::new();
        let r = t.root();
        t.define(r, sym(SymbolKind::Let, "x"));
        let c = t.fork(r);
        let mut inner = sym(SymbolKind::Let, "x");
        inner.ty = Ty::Bool;
        t.define(c, inner);
        assert_eq!(t.lookup(c, "x").unwrap().ty, Ty::Bool);
        assert_eq!(t.lookup(r, "x").unwrap().ty, Ty::Int);
    }
    #[test]
    fn define_rejects_duplicate() {
        let mut t = ScopeTree::new();
        let r = t.root();
        assert!(t.define(r, sym(SymbolKind::Let, "x")));
        assert!(!t.define(r, sym(SymbolKind::Let, "x")));
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
