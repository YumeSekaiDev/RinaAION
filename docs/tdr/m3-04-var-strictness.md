# TDR-M3-4 — Var strictness (no IR on residual Var)

## Context

M2 inference can leave a genuinely unconstrained type variable
(`Var`). Lowering such a program would require guessing a runtime
representation for an unknown type.

## Decision

Emission is strict: any residual `Var` at lowering is E0014
(NotLowerable) and no IR is emitted — not even partial IR. There is no
default-type fallback and no execution past this point.

## Consequences

- Executor only handles ground types; no generic or dynamic fallback
  needed in M3.
- Programs that are well-formed but under-constrained must gain
  constraints (usage that pins the type) before they can run.
- Partial IR is never inspected, so tooling cannot accidentally run a
  half-lowered program.
