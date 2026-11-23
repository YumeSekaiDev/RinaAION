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

## Amendment — M4.4 consolidated scope (Product Owner approved)

AST breaking change: `Stmt::Assert(Expr)` becomes the struct variant
`Stmt::Assert { expr, provenance }` (compiler-enumerated migration of
all in-workspace matches; acceptable pre-stability break, no external
stability promise exists). Provenance integration: `AssertProvenance`
(origin + optional identifiers, private fields, public constructors,
read-only accessors, boundary normalization) carried on the AST node,
preserved through lowering/folding into `E0010`/`E0044` diagnostics.
Folding scope: bare literals only. Unconditional-`false` ignores fuel
(nothing to evaluate) and still traverses the normal runtime trap
path including `record_trap`; unreached traps emit nothing.
Recorded in full in TDR-M4-8; this TDR's original decision (Option 1)
stands unchanged. `source_ref` and `transform_id` are optional
tool-supplied identifiers; ordinary lowering and mandated folding
preserve existing provenance unchanged and never set `Transformed` or
invent identifiers. Each identifier is independently
optional: a `Transformed` value with neither, either, or both present
is complete and valid, and absence never triggers inference, defaults,
or reclassification. No envelope or subsystem beyond `aion.diagnostic/v1`
diagnostic objects gains provenance serialization: not
`aion.trace/v1`, not `aion.intent/v1`, not source rendering,
`SemModel`, or IR. Evaluation precedence: operand traps and
fuel exhaustion during evaluation precede `E0044`; literal-`false`
traps unconditionally without operand evaluation or fuel check.
