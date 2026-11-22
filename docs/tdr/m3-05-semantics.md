# TDR-M3-5 — Semantics (body value, checked arithmetic, no output)

## Context

M3 needed exact value semantics: what a block yields, how arithmetic
fails, and whether `do` or anything else can be observed outside the
program.

## Decision

Body-value: trailing `do` Expr is the block value, else `Unit`;
recursive for nested blocks; `main` discards its value (program result
always `Unit`). Arithmetic is checked: overflow, div-by-zero, and
mod-by-zero trap with no wrapping or recovery. `do` has no observable
effect and M3 has no print/emit/host-call: programs produce no output.

## Consequences

- Value semantics compose: any block can be reasoned about locally.
- Checked traps make arithmetic total-safe at the cost of aborting on
  overflow/div-zero instead of wrapping.
- No-output keeps M3 pure and testable by result value and traps only;
  I/O arrives in a later milestone.
