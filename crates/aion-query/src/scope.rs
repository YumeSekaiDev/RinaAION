//! Scope views: innermost-first chains with sorted symbols.

use aion_sem_model::{ScopeId, ScopeTree, SymbolKind};

/// One scope level with its symbols, sorted by name for determinism.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeInfo {
    pub id: u32,
    pub symbols: Vec<SymbolInfo>,
}

/// Stable rendering of a [`SymbolKind`].
///
/// This is an explicit mapping, never `Debug` formatting, so the
/// emitted strings are a stable contract.
pub fn symbol_kind_str(kind: &SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Fn => "Fn",
        SymbolKind::Param => "Param",
        SymbolKind::Let => "Let",
    }
}

/// A single symbol snapshot (read from the model, never derived).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolInfo {
    pub name: String,
    pub kind: String,
    pub ty: String,
    pub moved: bool,
}

/// Innermost-first chain starting at `scope`, following parent links.
///
/// Symbols within each scope are sorted by name. Unknown scope ids
/// yield an empty chain.
pub fn scope_chain(tree: &ScopeTree, scope: ScopeId) -> Vec<ScopeInfo> {
    let mut out = Vec::new();
    let mut cur = Some(scope);
    while let Some(id) = cur {
        let idx = id.0 as usize;
        let Some(s) = tree.scopes.get(idx) else {
            break;
        };
        let mut symbols: Vec<SymbolInfo> = s
            .symbols
            .values()
            .map(|sym| SymbolInfo {
                name: sym.name.clone(),
                kind: symbol_kind_str(&sym.kind).to_string(),
                ty: sym.ty.to_string(),
                moved: sym.moved,
            })
            .collect();
        symbols.sort_by(|a, b| a.name.cmp(&b.name));
        out.push(ScopeInfo { id: id.0, symbols });
        cur = s.parent;
    }
    out
}

/// All scopes innermost-first (highest id first), each with sorted symbols.
pub fn all_scopes_innermost_first(tree: &ScopeTree) -> Vec<ScopeInfo> {
    let mut out: Vec<ScopeInfo> = (0..tree.scopes.len() as u32)
        .map(|id| {
            let s = &tree.scopes[id as usize];
            let mut symbols: Vec<SymbolInfo> = s
                .symbols
                .values()
                .map(|sym| SymbolInfo {
                    name: sym.name.clone(),
                    kind: symbol_kind_str(&sym.kind).to_string(),
                    ty: sym.ty.to_string(),
                    moved: sym.moved,
                })
                .collect();
            symbols.sort_by(|a, b| a.name.cmp(&b.name));
            ScopeInfo { id, symbols }
        })
        .collect();
    out.reverse();
    out
}
