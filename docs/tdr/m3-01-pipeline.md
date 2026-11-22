# TDR-M3-1 — M3 pipeline (sema-clean gate, lowering, execution)

## Context

M3 adds execution over the M1 syntax (Sec 2) with M2 static checks
(Sec 3). The pipeline order needed a written rule: when lowering and
execution may run, and what happens when earlier stages fail.

## Decision

Pipeline is strictly phased: parse → M2 sema → lower to IR → execute.
Sema must be fully clean before lowering; any residual M2 diagnostic
blocks execution and no IR is emitted. Lowering faults (E0014/E0015)
likewise emit no IR and never reach execution.

## Consequences

- Executors never see ill-typed or ambiguous programs; all runtime
  invariants rest on checked input.
- A single clean gate simplifies diagnostics: stage of failure tells
  the user where to look.
- No partial execution or partial IR on failure; tooling must surface
  the blocking diagnostic instead.
