//! Arena identity and allocation provenance (arena-containment Stage 2).
//!
//! Foundational types only: [`ArenaId`], [`AllocKind`], [`ArenaEntry`],
//! and the append-only [`ArenaTable`]. No lowering, validation, move,
//! or runtime behavior lives here, and nothing in this module consumes
//! Stage 1's `block_scopes` mapping yet.
//!
//! Safety invariants enforced by construction:
//! - `AllocKind` has no `Default` implementation: every string-typed IR
//!   node must name its variant explicitly once Stage 3 attaches the
//!   field. No missing provenance may silently become `Refcounted`.
//! - `ArenaTable::alloc` checks the `u32` boundary *before* mutating:
//!   exhaustion returns `None` with the table unchanged, never
//!   truncating or reusing ids.

use aion_sem_model::ScopeId;
use aion_span::ByteSpan;

/// Arena identity: index into the per-module [`ArenaTable`].
/// Deliberately distinct from `NodeId`/`ScopeId` (no `From` impls in
/// either direction) so ids can never be confused across domains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArenaId(pub u32);

/// Allocation provenance for string-typed IR nodes.
/// Intentionally no `Default` implementation: see module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocKind {
    Refcounted,
    Arena(ArenaId),
}

/// One arena scope: owning scope plus its span (span for diagnostics
/// and source mapping only — never for the containment proof).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArenaEntry {
    pub owner: ScopeId,
    pub span: ByteSpan,
}

/// Dense per-module arena table. Ids are append-only: no removal API
/// exists, so ids can never dangle or be reused.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArenaTable {
    entries: Vec<ArenaEntry>,
}

/// Checked conversion of the next table index to an [`ArenaId`].
/// Pure and total: returns `None` when `len` is not representable as
/// `u32`, without touching any table.
fn checked_next_id(len: usize) -> Option<ArenaId> {
    u32::try_from(len).ok().map(ArenaId)
}

impl ArenaTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate the next id for `owner`. The `u32` boundary is checked
    /// *before* mutation: exhaustion returns `None` with the table
    /// unchanged — no truncation, no id reuse, no partial state.
    pub fn alloc(&mut self, owner: ScopeId, span: ByteSpan) -> Option<ArenaId> {
        let id = checked_next_id(self.entries.len())?;
        self.entries.push(ArenaEntry { owner, span });
        Some(id)
    }

    /// Fail-closed lookup: `None` for unknown ids. The `u32`-to-`usize`
    /// conversion is checked: a narrowing failure returns `None`
    /// instead of indexing with a truncated value.
    pub fn get(&self, id: ArenaId) -> Option<&ArenaEntry> {
        let idx = usize::try_from(id.0).ok()?;
        self.entries.get(idx)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_span::FileId;

    fn sp(start: u32, end: u32) -> ByteSpan {
        ByteSpan {
            file: FileId(0),
            start,
            end,
        }
    }

    #[test]
    fn zero_produces_arena_id_zero() {
        assert_eq!(checked_next_id(0), Some(ArenaId(0)));
    }

    #[test]
    fn u32_max_is_final_representable_id() {
        let max = usize::try_from(u32::MAX).expect("usize holds u32::MAX on supported targets");
        assert_eq!(checked_next_id(max), Some(ArenaId(u32::MAX)));
    }

    #[test]
    fn next_index_rejected_when_representable() {
        // Portable: only assert rejection where the next index fits in
        // `usize` at all. Never writes `u32::MAX as usize + 1`, which
        // would overflow on 32-bit targets.
        let next = u32::MAX as u64 + 1;
        if let Ok(n) = usize::try_from(next) {
            assert_eq!(checked_next_id(n), None, "index past u32::MAX rejected");
        }
    }

    #[test]
    fn usize_max_rejected_unless_usize_is_32_bit() {
        // On targets where `usize` is wider than `u32`, `usize::MAX`
        // cannot be an arena id. On 32-bit targets it *is* `u32::MAX`
        // and therefore valid.
        if usize::BITS > u32::BITS {
            assert_eq!(checked_next_id(usize::MAX), None);
        } else {
            assert_eq!(checked_next_id(usize::MAX), Some(ArenaId(u32::MAX)));
        }
    }

    #[test]
    fn sequential_allocations_produce_dense_ids() {
        let mut t = ArenaTable::new();
        assert!(t.is_empty());
        for (i, expected) in [0u32, 1, 2].iter().enumerate() {
            let id = t
                .alloc(ScopeId(i as u32), sp(i as u32, i as u32 + 1))
                .expect("plenty of id space");
            assert_eq!(id, ArenaId(*expected));
        }
        assert_eq!(t.len(), 3);
        assert!(!t.is_empty());
    }

    #[test]
    fn invalid_ids_return_none() {
        let t = ArenaTable::new();
        assert_eq!(t.get(ArenaId(0)), None);
        assert_eq!(t.get(ArenaId(u32::MAX)), None);
        let mut t = ArenaTable::new();
        t.alloc(ScopeId(7), sp(0, 1));
        assert_eq!(t.get(ArenaId(1)), None, "unallocated id rejected");
        assert_eq!(t.get(ArenaId(u32::MAX)), None);
    }

    #[test]
    fn table_lookup_returns_correct_entries() {
        let mut t = ArenaTable::new();
        let a = t.alloc(ScopeId(3), sp(10, 20)).expect("id space");
        let b = t.alloc(ScopeId(5), sp(30, 40)).expect("id space");
        assert_eq!(
            t.get(a),
            Some(&ArenaEntry {
                owner: ScopeId(3),
                span: sp(10, 20),
            })
        );
        assert_eq!(
            t.get(b),
            Some(&ArenaEntry {
                owner: ScopeId(5),
                span: sp(30, 40),
            })
        );
    }

    #[test]
    fn both_alloc_kind_variants_construct_explicitly() {
        let r = AllocKind::Refcounted;
        let a = AllocKind::Arena(ArenaId(9));
        assert_ne!(r, a);
        assert!(matches!(a, AllocKind::Arena(ArenaId(9))));
    }
}
