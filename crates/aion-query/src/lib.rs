//! `aion-query`: read-only semantic queries over [`SemModel`].
//!
//! Single-truth rule: this crate only reads the model produced by
//! `aion-sema`. It performs no type inference, no unification, and no
//! move derivation of its own.

pub mod calls;
pub mod queries;
pub mod scope;

pub use calls::CallGraph;
pub use queries::{view, FnHandle, FnSig, MoveSite, QueryError, ResolvedTy, SemanticView};
pub use scope::{ScopeInfo, SymbolInfo};
