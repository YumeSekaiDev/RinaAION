# TDR-M4-4 — Assertion semantics

## Context

M4 adds `assert` statements: static checking plus a runtime trap on
false, with precise fuel/span/ownership rules.

## Options

1. `TypedStmt::Assert` as its own node, never via `Do`, with
   non-moving reads and literal folding.
2. Desugar `assert e` to `do e` plus a check: rejected — conflates
   assertion failures with value yields and breaks the no-output rule.
3. Solver-backed static discharge: rejected — out of scope; Bool
   checking + literal folding only.

## Decision

Option 1. Shape: `TypedStmt::Assert` is a distinct statement node,
never routed via `Do`; a 0-cost wrapper (success yields nothing).
Static scope: Bool checking + literal folding only, no solver
(non-`Bool` operand is `AION-E0010`). Fuel: operand nodes cost normal
fuel; the wrapper adds none. Span: false asserts trap `AION-E0044`
(AssertFailed) at the statement span. Reads: assert-operand reads are
non-moving reads that produce no ownership move facts (no move facts
recorded). Literals: literal-`true` eliminated (no node, no fuel);
literal-`false` becomes an unconditional `AION-E0044` without operand
evaluation. Provenance: every assertion carries an origin —
`human-authored`, `ai-generated`, or `transformed` (preserving the
source assertion identity plus the transformation identity). Provenance
is preserved into diagnostics but grants no authority and changes no
rule: all assertions follow identical static and execution rules
regardless of origin, and provenance never counts as proof.

## Consequences

- Assert success is unobservable (no-output rule holds).
- `E0044` joins the `E0040`+ runtime-trap class at statement span.
- SPEC Sec 5 (Assertions, code table, no-output interaction) touched.
