//! Entry-anchored intent/goal views over a [`SemanticView`].
//!
//! Single public entry point: [`SemanticView::entry_intents`]. Reachability
//! stays a private helper; there is no unfiltered whole-program surface.

use crate::queries::SemanticView;
use aion_sem_model::SemModel;
use aion_span::ByteSpan;
use aion_syntax::{
    Block, BlockIds, Expr, ExprIds, ExprIdsKind, Item, ItemIds, NodeId, ParseResult, Stmt, StmtIds,
};
use std::collections::{HashMap, HashSet};

/// One intent entry anchored at the entry point: owning function, the
/// intent block (or `None` for a synthesized body-goal entry), the
/// parser-provided description (`""` when absent), and its goals in
/// source order.
#[derive(Debug, Clone)]
pub struct IntentView {
    pub owner: String,
    pub owner_span: Option<ByteSpan>,
    pub span: Option<ByteSpan>,
    pub desc: String,
    pub goals: Vec<GoalView>,
}

/// One `goal : <expr>;` statement: the goal expression, its statement
/// span, and its resolved-or-unbound name references sorted by
/// occurrence span.
#[derive(Debug, Clone)]
pub struct GoalView {
    pub expr: Expr,
    pub span: Option<ByteSpan>,
    pub refs: Vec<GoalRefView>,
}

/// One name occurrence inside a goal expression.
#[derive(Debug, Clone)]
pub struct GoalRefView {
    pub name: String,
    pub bound: bool,
    pub occurrence: Option<ByteSpan>,
    pub decl_name: Option<String>,
    pub decl_span: Option<ByteSpan>,
}

impl<'a> SemanticView<'a> {
    /// Intents owned by `fn main` (zero-parameter) and its transitively
    /// reachable callees, in model anchor order. Missing entry yields an
    /// empty vector, never an error. Orphan entries and entries owned by
    /// unreachable items are excluded.
    pub fn entry_intents(&self) -> Vec<IntentView> {
        let (parse, model) = self.parse_and_model();
        let reachable = reachable_names(parse);
        let Some(reachable) = reachable else {
            return Vec::new();
        };
        let decls = decl_table(parse);
        let mut out = Vec::new();
        for entry in &model.intents {
            let Some(owner_id) = entry.owner else {
                continue;
            };
            let Some((fn_name, owner_span)) = fn_owner(parse, owner_id) else {
                continue;
            };
            if !reachable.contains(fn_name.as_str()) {
                continue;
            }
            let (intent_span, desc) = match entry.intent {
                None => (None, String::new()),
                Some(n) => (parse.spans.get(n), intent_desc(parse, n)),
            };
            let mut goals = Vec::with_capacity(entry.goals.len());
            for g in &entry.goals {
                let Some(expr) = find_goal_expr(parse, g.stmt) else {
                    continue;
                };
                let refs = goal_refs(parse, &expr, &g.expr, &g.refs, &decls);
                goals.push(GoalView {
                    expr,
                    span: parse.spans.get(g.stmt),
                    refs,
                });
            }
            out.push(IntentView {
                owner: fn_name.clone(),
                owner_span,
                span: intent_span,
                desc,
                goals,
            });
        }
        out
    }
}

/// Names of `fn main` (zero-parameter) plus its transitively reachable
/// callees. `None` when no zero-parameter `fn main` exists.
fn reachable_names(parse: &ParseResult) -> Option<HashSet<String>> {
    let mut has_main = false;
    for item in &parse.program.items {
        if let Item::Fn(f) = item {
            if f.name == "main" && f.params.is_empty() {
                has_main = true;
                break;
            }
        }
    }
    if !has_main {
        return None;
    }
    let graph = crate::calls::CallGraph::build(&parse.program.items, &parse.index.items);
    let mut seen = HashSet::new();
    let mut stack = vec!["main".to_string()];
    seen.insert("main".to_string());
    while let Some(name) = stack.pop() {
        if let Some(callees) = graph.callees(&name) {
            for c in callees {
                if seen.insert(c.clone()) {
                    stack.push(c.clone());
                }
            }
        }
    }
    Some(seen)
}

/// Owner [`NodeId`] -> (function name, whole-item span) via the index
/// `ItemIds::Fn` node match. `None` when the owner is not a `fn` item.
fn fn_owner(parse: &ParseResult, owner: NodeId) -> Option<(&String, Option<ByteSpan>)> {
    for (item, iids) in parse.program.items.iter().zip(parse.index.items.iter()) {
        if let (Item::Fn(f), ItemIds::Fn(fids)) = (item, iids) {
            if fids.node == owner {
                return Some((&f.name, parse.spans.get(fids.node)));
            }
        }
    }
    None
}

/// Description of the intent block with index node `n` (`""` when the
/// block cannot be located; parser default when absent).
fn intent_desc(parse: &ParseResult, n: NodeId) -> String {
    for (item, iids) in parse.program.items.iter().zip(parse.index.items.iter()) {
        if let (Item::Intent(b), ItemIds::Intent(ii)) = (item, iids) {
            if ii.node == n {
                return b.desc.clone();
            }
        }
    }
    String::new()
}

/// Declaration table: [`NodeId`] -> (declared name, name span), walked
/// from the index (`FnIds.name`/`params`, `StmtIds::Let` names) plus
/// every top-level `fn` name.
fn decl_table(parse: &ParseResult) -> HashMap<NodeId, (String, ByteSpan)> {
    let mut out = HashMap::new();
    for (item, iids) in parse.program.items.iter().zip(parse.index.items.iter()) {
        if let (Item::Fn(f), ItemIds::Fn(fids)) = (item, iids) {
            if let Some(sp) = parse.spans.get(fids.name) {
                out.insert(fids.name, (f.name.clone(), sp));
            }
            for (p, pid) in f.params.iter().zip(fids.params.iter()) {
                if let Some(sp) = parse.spans.get(*pid) {
                    out.insert(*pid, (p.clone(), sp));
                }
            }
            collect_let_names(&f.body, &fids.body, parse, &mut out);
        }
    }
    out
}

fn collect_let_names(
    b: &Block,
    bids: &BlockIds,
    parse: &ParseResult,
    out: &mut HashMap<NodeId, (String, ByteSpan)>,
) {
    if b.stmts.len() != bids.stmts.len() {
        return;
    }
    for (st, sid) in b.stmts.iter().zip(bids.stmts.iter()) {
        match (st, sid) {
            (Stmt::Let { name, .. }, StmtIds::Let { name: nid, .. }) => {
                if let Some(sp) = parse.spans.get(*nid) {
                    out.insert(*nid, (name.clone(), sp));
                }
            }
            (Stmt::Block(inner), StmtIds::Block { body, .. }) => {
                collect_let_names(inner, body, parse, out);
            }
            _ => {}
        }
    }
}

/// Locate the goal [`Expr`] for the goal statement [`NodeId`] by
/// walking the program AST and its parallel index in lockstep (fn bodies
/// and intent bodies alike). `None` on any shape mismatch.
fn find_goal_expr(parse: &ParseResult, stmt: NodeId) -> Option<Expr> {
    if parse.program.items.len() != parse.index.items.len() {
        return None;
    }
    for (item, iids) in parse.program.items.iter().zip(parse.index.items.iter()) {
        match (item, iids) {
            (Item::Fn(f), ItemIds::Fn(fids)) => {
                if let Some(e) = find_goal_in_block(&f.body, &fids.body, stmt) {
                    return Some(e);
                }
            }
            (Item::Intent(b), ItemIds::Intent(ii)) => {
                if let Some(e) = find_goal_in_block(&b.body, &ii.body, stmt) {
                    return Some(e);
                }
            }
            _ => {}
        }
    }
    None
}

fn find_goal_in_block(b: &Block, bids: &BlockIds, stmt: NodeId) -> Option<Expr> {
    if b.stmts.len() != bids.stmts.len() {
        return None;
    }
    for (st, sid) in b.stmts.iter().zip(bids.stmts.iter()) {
        match (st, sid) {
            (Stmt::Goal(e), StmtIds::Goal { node, .. }) if *node == stmt => {
                return Some(e.clone());
            }
            (Stmt::Block(inner), StmtIds::Block { body, .. }) => {
                if let Some(e) = find_goal_in_block(inner, body, stmt) {
                    return Some(e);
                }
            }
            _ => {}
        }
    }
    None
}

/// Name occurrences in a goal expression in source order: (name,
/// use-site [`NodeId`]), callees included. Bare `null` contributes no
/// occurrence (it reads as the `Null` literal).
fn occurrences(e: &Expr, ids: &ExprIds, out: &mut Vec<(String, NodeId)>) {
    match e {
        Expr::Ident(name) => {
            if name != "null" {
                out.push((name.clone(), ids.node));
            }
        }
        Expr::Call { callee, args } => {
            if let ExprIdsKind::Call {
                callee: cid,
                args: aids,
                ..
            } = &ids.kind
            {
                out.push((callee.clone(), *cid));
                for (a, aid) in args.iter().zip(aids.iter()) {
                    occurrences(a, aid, out);
                }
            }
        }
        Expr::Unary { rhs, .. } => {
            if let ExprIdsKind::Unary { rhs: rid, .. } = &ids.kind {
                occurrences(rhs, rid, out);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            if let ExprIdsKind::Binary {
                lhs: lid, rhs: rid, ..
            } = &ids.kind
            {
                occurrences(lhs, lid, out);
                occurrences(rhs, rid, out);
            }
        }
        _ => {}
    }
}

fn goal_refs(
    parse: &ParseResult,
    expr: &Expr,
    ids: &ExprIds,
    refs: &[aion_sem_model::GoalRef],
    decls: &HashMap<NodeId, (String, ByteSpan)>,
) -> Vec<GoalRefView> {
    let mut occ = Vec::new();
    occurrences(expr, ids, &mut occ);
    let by_site: HashMap<NodeId, Option<NodeId>> =
        refs.iter().map(|r| (r.use_site, r.decl)).collect();
    let mut out: Vec<GoalRefView> = occ
        .into_iter()
        .map(|(name, site)| {
            let occurrence = parse.spans.get(site);
            let decl = by_site.get(&site).copied().flatten();
            let (decl_name, decl_span) = decl
                .and_then(|d| decls.get(&d))
                .map(|(n, s)| (Some(n.clone()), Some(*s)))
                .unwrap_or((None, None));
            GoalRefView {
                name,
                bound: decl.is_some(),
                occurrence,
                decl_name,
                decl_span,
            }
        })
        .collect();
    out.sort_by_key(|r| {
        r.occurrence
            .map(|s| (s.file.0, s.start, s.end))
            .unwrap_or((u32::MAX, u32::MAX, u32::MAX))
    });
    out
}

#[allow(dead_code)]
fn unused_model(_: &SemModel) {}
