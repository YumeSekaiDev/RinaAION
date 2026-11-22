# TDR-M4-3 — Intent/goal query surface

## Context

Intent blocks and goal fields are metadata, not executable code. They
need a queryable shape anchored at the entry point without affecting
execution.

## Options

1. Queryable metadata with entry-point query + joins.
2. Executable intent (affects runs): rejected — violates
   non-interference.
3. Ad-hoc per-tool scraping: rejected — no consistent shape.

## Decision

Option 1. Intent/goal shape: goals are `Goal(Expr)` — full expression
plus source span and referenced binding ids (reconciles Appendix A:
the `GatedGoal` row is already the Expr form; no bare-identifier form).
Goal checking is name resolution only: unbound names yield `E0011`
(UnboundName); no type constraints, no move facts, no block-value or
scope contribution. Entry-point query: read-only queries anchored at
`fn main`, returning attached intent/goals. Joins: shared-model
consistency tests join query results against the same underlying model
(one model, many views — no divergent copies).

## Consequences

- Non-interference theorem (tested): for any executable program P and
  the otherwise identical program P+metadata, execution results,
  semantic traces, fuel consumption, and trap identity/payload must be
  identical. Source spans, line/column positions, and source excerpts
  are provenance/presentation data and may differ with layout;
  canonical diagnostic comparisons separate semantic fields from
  source-location fields. Spans stay deterministic within each variant.
- Default-off: queries on gate-off programs return empty, execution
  identical to M3.
- SPEC Sec 5 (Intent/goal exposure, Entry-point query) touched.
