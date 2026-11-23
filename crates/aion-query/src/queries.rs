//! Read-only semantic queries over a parse result plus [`SemModel`].
//!
//! Every query reads already-computed model data. This module never
//! unifies types, never derives move facts, and never synthesizes
//! spans: unknown entities are reported as [`QueryError`].

use crate::calls::CallGraph;
use crate::scope::{self, ScopeInfo};
use aion_sem_model::{SemModel, Ty};
use aion_span::ByteSpan;
use aion_syntax::{Item, NodeId, ParseResult};

/// A borrowed parse + model pair.
#[derive(Debug)]
pub struct SemanticView<'a> {
    parse: &'a ParseResult,
    model: &'a SemModel,
}

/// Build a [`SemanticView`] over borrowed artifacts.
pub fn view<'a>(parse: &'a ParseResult, model: &'a SemModel) -> SemanticView<'a> {
    SemanticView { parse, model }
}

impl<'a> SemanticView<'a> {
    /// Read-only access to the borrowed parse + model for crate-internal
    /// intent views. Not part of the public query surface.
    pub(crate) fn parse_and_model(&self) -> (&'a ParseResult, &'a SemModel) {
        (self.parse, self.model)
    }
}

/// Plain query failure; deliberately free of diagnostic codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryError {
    /// No zero-parameter `fn main` is defined.
    NoEntry,
    /// No function with this name is defined.
    UnknownFunction(String),
    /// No type is recorded for this node.
    UnknownNode(NodeId),
    /// More than one function shares this name.
    AmbiguousFunction(String),
}

/// Resolved-type answer: ground types carry their display string,
/// unresolved inference variables report [`ResolvedTy::Unknown`],
/// and error types report [`ResolvedTy::Error`].
/// Inference-variable identities are never exposed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedTy {
    Ground { ty: String },
    Unknown,
    Error,
}

/// A function handle: name plus declaration span, if recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnHandle {
    pub name: String,
    pub span: Option<ByteSpan>,
}

/// A function signature with its declaration span, if recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnSig {
    pub name: String,
    pub params: Vec<String>,
    pub span: Option<ByteSpan>,
}

/// A recorded move site joined to its source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveSite {
    pub node: NodeId,
    pub span: Option<ByteSpan>,
}

impl<'a> SemanticView<'a> {
    /// Zero-parameter `fn main`, or [`QueryError::NoEntry`].
    pub fn entry_point(&self) -> Result<FnHandle, QueryError> {
        for item in &self.parse.program.items {
            if let Item::Fn(f) = item {
                if f.name == "main" && f.params.is_empty() {
                    return Ok(FnHandle {
                        name: f.name.clone(),
                        span: self.fn_span(&f.name),
                    });
                }
            }
        }
        Err(QueryError::NoEntry)
    }

    /// Exact-name function lookup in definition order.
    pub fn fn_lookup(&self, name: &str) -> Result<FnSig, QueryError> {
        let mut found: Option<FnSig> = None;
        for item in &self.parse.program.items {
            if let Item::Fn(f) = item {
                if f.name == name {
                    let sig = FnSig {
                        name: f.name.clone(),
                        params: f.params.clone(),
                        span: self.fn_span(&f.name),
                    };
                    if found.is_some() {
                        return Err(QueryError::AmbiguousFunction(name.to_string()));
                    }
                    found = Some(sig);
                }
            }
        }
        found.ok_or_else(|| QueryError::UnknownFunction(name.to_string()))
    }

    /// Function list with signatures and spans, in definition order.
    pub fn program_structure(&self) -> Vec<FnSig> {
        let mut out = Vec::new();
        for item in &self.parse.program.items {
            if let Item::Fn(f) = item {
                out.push(FnSig {
                    name: f.name.clone(),
                    params: f.params.clone(),
                    span: self.fn_span(&f.name),
                });
            }
        }
        out
    }

    /// Direct callees of `name` in source order.
    pub fn callees(&self, name: &str) -> Result<Vec<String>, QueryError> {
        self.fn_lookup(name)?;
        Ok(self.call_graph().callees(name).unwrap_or(&[]).to_vec())
    }

    /// Direct callers of `name` in function-definition order.
    pub fn callers(&self, name: &str) -> Result<Vec<String>, QueryError> {
        self.fn_lookup(name)?;
        Ok(self.call_graph().callers(name))
    }

    /// Recorded type of a node: unresolved inference variables report
    /// [`ResolvedTy::Unknown`], error types report [`ResolvedTy::Error`],
    /// and ground types carry their display string. Raw `Var` identities
    /// are never leaked.
    pub fn type_at(&self, node: NodeId) -> Result<ResolvedTy, QueryError> {
        match self.model.types.get(&node) {
            None => Err(QueryError::UnknownNode(node)),
            Some(Ty::Var(_)) => Ok(ResolvedTy::Unknown),
            Some(Ty::Error) => Ok(ResolvedTy::Error),
            Some(ty) => Ok(ResolvedTy::Ground { ty: ty.to_string() }),
        }
    }

    /// Recorded move sites joined to spans, sorted by node id.
    pub fn moves(&self) -> Vec<MoveSite> {
        let mut nodes: Vec<NodeId> = self.model.moved_uses.iter().copied().collect();
        nodes.sort_by_key(|n| n.0);
        nodes
            .into_iter()
            .map(|node| MoveSite {
                node,
                span: self.parse.spans.get(node),
            })
            .collect()
    }

    /// All scopes innermost-first, each with name-sorted symbols.
    pub fn scope_chain(&self) -> Vec<ScopeInfo> {
        scope::all_scopes_innermost_first(&self.model.scope_tree)
    }

    fn call_graph(&self) -> CallGraph {
        CallGraph::build(&self.parse.program.items, &self.parse.index.items)
    }

    /// Declaration span for `name`, or `None` when the model records
    /// no item under that name. Never synthesizes a span.
    fn fn_span(&self, name: &str) -> Option<ByteSpan> {
        self.model
            .items
            .iter()
            .find(|i| i.name == name)
            .map(|i| i.span)
    }
}
