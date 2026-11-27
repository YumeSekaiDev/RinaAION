//! Stage 1 (arena containment): the published block-to-scope map is
//! complete for every reachable function-body block.

use aion_sem_model::SemModel;
use aion_span::FileId;
use aion_syntax::{Block, BlockIds, ItemIds, NodeId, Stmt, StmtIds};

fn collect_blocks(b: &Block, bids: &BlockIds, out: &mut Vec<NodeId>) {
    out.push(bids.node);
    for (st, sid) in b.stmts.iter().zip(bids.stmts.iter()) {
        if let (Stmt::Block(inner), StmtIds::Block { body, .. }) = (st, sid) {
            collect_blocks(inner, body, out);
        }
    }
}

fn reachable_block_nodes(src: &str) -> (Vec<NodeId>, SemModel, Vec<aion_error::Diagnostic>) {
    let r = aion_syntax::parse(FileId(0), src.as_bytes());
    assert!(r.errors.is_empty(), "parse must be clean: {src:?}");
    let (diags, model) = aion_sema::check_with_model(&r, FileId(0), src.as_bytes());
    let mut nodes = Vec::new();
    for (item, iids) in r.program.items.iter().zip(r.index.items.iter()) {
        if let (aion_syntax::Item::Fn(f), ItemIds::Fn(fids)) = (item, iids) {
            collect_blocks(&f.body, &fids.body, &mut nodes);
        }
    }
    (nodes, model, diags)
}

#[test]
fn nested_blocks_all_have_scope_entries() {
    let src = "fn main() { let a = 1; { do a; { do a; } } do a; }";
    let (nodes, model, diags) = reachable_block_nodes(src);
    assert!(diags.is_empty(), "expected clean sema: {diags:?}");
    assert_eq!(nodes.len(), 3, "fn body plus two nested blocks");
    for n in &nodes {
        assert!(
            model.block_scopes.contains_key(n),
            "block {n:?} missing from published scope map"
        );
    }
    // Distinct scopes in ancestry order (inner blocks nest inside outer).
    let scopes: Vec<_> = nodes.iter().map(|n| model.block_scopes[n]).collect();
    assert_eq!(
        scopes.len(),
        scopes
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        "each block gets its own scope"
    );
}

#[test]
fn shadowing_blocks_keep_distinct_scopes() {
    let src = "fn main() { let x = 1; { let x = 2; do x; } do x; }";
    let (nodes, model, diags) = reachable_block_nodes(src);
    assert!(diags.is_empty(), "expected clean sema: {diags:?}");
    assert_eq!(nodes.len(), 2);
    for n in &nodes {
        assert!(
            model.block_scopes.contains_key(n),
            "block {n:?} missing from published scope map"
        );
    }
}

#[test]
fn intent_bodies_need_no_scope_entries() {
    // Intent bodies are never entered by the statement checker, so the
    // completeness check must not require (or create) entries for them —
    // including nested blocks — while the owning function body must be
    // present.
    let src = "intent \"d\" { do 1; { do 1; } } fn main() { do 1; }";
    let r = aion_syntax::parse_with_flags(
        FileId(0),
        src.as_bytes(),
        aion_syntax::FeatureFlags::all_experimental(),
    );
    assert!(r.errors.is_empty(), "must parse clean");
    let (diags, model) = aion_sema::check_with_model(&r, FileId(0), src.as_bytes());
    assert!(
        diags.iter().all(|d| !d.message.contains("block scope")),
        "no internal scope diagnostic expected: {diags:?}"
    );
    let mut intent_nodes = Vec::new();
    let mut fn_body_nodes = Vec::new();
    for (item, iids) in r.program.items.iter().zip(r.index.items.iter()) {
        match (item, iids) {
            (aion_syntax::Item::Intent(b), ItemIds::Intent(ii)) => {
                collect_blocks(&b.body, &ii.body, &mut intent_nodes)
            }
            (aion_syntax::Item::Fn(f), ItemIds::Fn(fids)) => {
                collect_blocks(&f.body, &fids.body, &mut fn_body_nodes)
            }
            _ => {}
        }
    }
    assert_eq!(
        intent_nodes.len(),
        2,
        "top-level and nested intent-body blocks must both be examined"
    );
    for n in &intent_nodes {
        assert!(
            !model.block_scopes.contains_key(n),
            "intent-body block {n:?} must have no scope entry"
        );
    }
    assert_eq!(fn_body_nodes.len(), 1);
    for n in &fn_body_nodes {
        assert!(
            model.block_scopes.contains_key(n),
            "function-body block {n:?} must have a scope entry"
        );
    }
}
