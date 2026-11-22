# TDR-M3-3 — Diagnostics (E0014/E0015/E0030/E0040+)

## Context

M2 codes (E0010–E0013, E0020) do not cover entry, lowering, or runtime
faults. New codes must slot into the stable `AION-E00xx` scheme without
renumbering existing codes.

## Decision

Add E0014 (NotLowerable), E0015 (MissingEntry), E0030 (OutOfFuel, with
E0031–39 reserved for fuel sub-cases), and E0040+ (StackOverflow and
future runtime faults). E0014/E0015 are pre-execution (no IR, no run);
E0030/E0040+ are runtime traps. Spans: E0014 at the carrying
expression/binding, E0015 at `main` or end of input, runtime traps at
the operator/call span.

## Consequences

- Existing codes untouched; stability guarantee holds.
- Reserved E0031–39 keeps fuel extensions compatible.
- E0040+ leaves room above for future runtime faults without
  collisions.
