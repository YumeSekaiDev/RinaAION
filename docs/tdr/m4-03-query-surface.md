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

- Non-interference theorem (accepted as specified; execution
  equivalence tested, per-path trace verification deferred): for any
  executable program P and the otherwise identical program P+metadata,
  execution results, semantic traces, fuel consumption, and trap
  identity/payload must be identical. Suite A verifies result value,
  fuel, and full trap identity+payload across both executable paths;
  the trace channel is covered only by the public empty-event
  serializer golden test — per-path trace equivalence awaits a
  traced-execution interface. Source spans, line/column positions, and
  source excerpts are provenance/presentation data and may differ with
  layout; canonical diagnostic comparisons separate semantic fields
  from source-location fields. Spans stay deterministic within each
  variant.
- Default-off: queries on gate-off programs return empty, execution
  identical to M3.
- SPEC Sec 5 (Intent/goal exposure, Entry-point query) touched.

## Amendment — M4.3 decisions D1–D10 (Product Owner approved)

D1 Ownership direction: R-FOLLOWING — each top-level `intent` block is
owned by the nearest following `fn` item in source order.
D2 Multiple intents per item accumulate independently, in source order;
no merging.
D3 Orphan intents (no following `fn`) are retained in the semantic
model but excluded from entry-point queries and the normal
`aion.intent/v1` envelope.
D4 Nested model shape (`IntentEntry` with per-block goals) with a
nested JSON projection; the earlier flat-goals alternative is
withdrawn.
D5 References carry occurrence span plus declaration name and span, or
explicit `decl: null` with `status: "unresolved"` and the diagnostic
when unbound.
D6 Goal E0011 is blocking: an unbound goal name fails semantic
checking and the program is not executed.
D7 Queries are entry-point-anchored, covering `fn main` and its
transitively reachable callees; no unfiltered whole-program query
surface.
D8 Owner spans are whole-item spans.
D9 Non-interference compares full trap identity and payload.
D10 The eight-kind expression vocabulary and the complete
`aion.intent/v1` field list are frozen in SPEC Sec 5 before any
golden tests are written.

Frozen alongside: `Goal(Expr)` representation, per-item attachment,
new `aion.intent/v1` envelope, provenance deferred, exposure-only
scope, no M4.4/M5.

D11 Enabled analysis: additive `analyze_with_flags(file, bytes,
flags)` composing `parse_with_flags()` with `check_with_model()`;
existing `analyze()` behavior and the M4.2 contract unchanged;
default-off preserved.
D12 Intent descriptions: parser-provided optional description kept,
`""` default, omitted from JSON when empty; no grammar change; gap
logged separately (TDR-M4-7).

## Normative testing (M4.3 acceptance)

Test suites: `crates/aion-driver/tests/intent_noninterference.rs`
(valid-metadata runtime equivalence), `crates/aion-sema/tests/intent_goals.rs`
(invalid-goal diagnostic isolation), `crates/aion-query/tests/query_m43.rs`
(entry-anchored intent/goal queries).
Fixtures: `tests/fixtures/m4_3/` (`valid_equiv/` pairs `p.aion` +
`p_meta.aion`; `invalid_goal/` unbound-reference cases).
Goldens: `tests/golden/m4/intent/` (canonical plus compat triplets,
mirroring the M4.2 golden pattern); no other golden directory is
touched.
Cleanliness-gate-then-compare protocol: for each P vs P+metadata pair,
first require `analyze()` (or `analyze_with_flags()` with enabled
flags) to succeed on both variants — a dirty variant invalidates the
pair, it does not count as equivalence. Only then compare three
runtime channels independently across the two executable paths:
execution result value, fuel consumption, and full trap identity plus
payload. Trace-equivalence verification is explicitly deferred until
AION has a traced-execution interface: the suite additionally checks
the public empty-event trace serializer output against its golden
file, which does not establish equivalence between execution paths.
Never compare `Debug` output; never compare spans across variants.
Invalid-goal isolation tests (E0011 per occurrence, diagnostic-set
diff, masked model-identity, no lowering on dirty programs) are kept
strictly separate from runtime equivalence tests; no shared helpers
between the two suites.
D13 Synthesized entries: `IntentEntry.intent` is `Option<NodeId>`
(`None` = synthesized, no intent block); created only when body
goals exist; explicit entries hold only their block-body goals;
empty description omitted; never orphans.
D14 Anchor ordering: explicit entries anchor at the intent-block
span, synthesized entries at the first body-goal span; all entries
sort ascending by anchor `(file, start, end)`; goals within an
entry stay in source order.

Implementation note — pre-pass declaration spans (M4.3): the forward
pre-pass `Bind` now carries the real `FnIds.name` span and declaration
`NodeId` instead of a synthetic zero span. Verified impact: the only
consumer is `Symbol.span` in the scope tree (never exposed by queries,
diagnostics, or JSON envelopes); all existing suites pass with no
golden drift. Classified as an intended correction, not a regression.
