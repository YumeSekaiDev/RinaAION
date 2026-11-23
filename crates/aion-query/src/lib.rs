//! `aion-query`: read-only semantic queries over [`SemModel`].
//!
//! Single-truth rule: this crate only reads the model produced by
//! `aion-sema`. It performs no type inference, no unification, and no
//! move derivation of its own.

pub mod calls;
pub mod intent_json;
pub mod intents;
pub mod queries;
pub mod scope;

pub use calls::CallGraph;
pub use intent_json::{intent_canonical_bytes, parse_intent_envelope, ParsedIntent};
pub use intents::{GoalRefView, GoalView, IntentView};
pub use queries::{view, FnHandle, FnSig, MoveSite, QueryError, ResolvedTy, SemanticView};
pub use scope::{ScopeInfo, SymbolInfo};
