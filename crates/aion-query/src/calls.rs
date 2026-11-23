//! Direct call-edge extraction (read-only walk of the AST).
//!
//! Edges are direct syntactic calls only: a call `f(args)` appearing
//! anywhere inside the body of `g` yields one edge `g -> f`. No
//! transitive closure, no resolution beyond the callee name.

use aion_syntax::{Block, Expr, Item, ItemIds, Stmt};
use std::collections::HashMap;

/// Direct call graph: caller name -> callee names in definition order.
///
/// Callee lists preserve first-occurrence (source) order with duplicates
/// removed. Caller lists are derived in function-definition order.
#[derive(Debug, Clone, Default)]
pub struct CallGraph {
    /// Function definitions in source order.
    pub order: Vec<String>,
    /// Caller -> callees (first-occurrence order, deduplicated).
    edges: HashMap<String, Vec<String>>,
}

impl CallGraph {
    /// Build the graph from a program and its parallel id index.
    ///
    /// The AST is walked read-only; `index` is accepted for signature
    /// symmetry with sema and deliberately unused.
    pub fn build(program: &[Item], _index: &[ItemIds]) -> Self {
        let mut graph = Self::default();
        for item in program {
            if let Item::Fn(f) = item {
                graph.order.push(f.name.clone());
                let mut callees = Vec::new();
                collect_block(&f.body, &mut callees);
                let mut seen = std::collections::HashSet::new();
                let deduped: Vec<String> = callees
                    .into_iter()
                    .filter(|c| seen.insert(c.clone()))
                    .collect();
                graph.edges.insert(f.name.clone(), deduped);
            }
        }
        graph
    }

    /// Direct callees of `name` in source order, or `None` if unknown.
    pub fn callees(&self, name: &str) -> Option<&[String]> {
        self.edges.get(name).map(Vec::as_slice)
    }

    /// Direct callers of `name` in function-definition order.
    pub fn callers(&self, name: &str) -> Vec<String> {
        let mut out = Vec::new();
        for caller in &self.order {
            if let Some(callees) = self.edges.get(caller) {
                if callees.iter().any(|c| c == name) {
                    out.push(caller.clone());
                }
            }
        }
        out
    }
}

fn collect_block(block: &Block, out: &mut Vec<String>) {
    for stmt in &block.stmts {
        collect_stmt(stmt, out);
    }
}

fn collect_stmt(stmt: &Stmt, out: &mut Vec<String>) {
    match stmt {
        Stmt::Let { expr, .. } => collect_expr(expr, out),
        Stmt::Do(e) => collect_expr(e, out),
        Stmt::Block(b) => collect_block(b, out),
        Stmt::Assert { expr: e, .. } => collect_expr(e, out),
        Stmt::Goal(_) | Stmt::Empty => {}
    }
}

fn collect_expr(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::Call { callee, args } => {
            out.push(callee.clone());
            for a in args {
                collect_expr(a, out);
            }
        }
        Expr::Unary { rhs, .. } => collect_expr(rhs, out),
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr(lhs, out);
            collect_expr(rhs, out);
        }
        _ => {}
    }
}
