# TDR-M4-2 — Determinism and canonical form

## Context

M4.3 (query surface) and M4.4 (assert execution) need a shared
determinism baseline, or their tests flake on seed/locale drift.

## Options

1. Determinism gate first (M4.1), then M4.3/M4.4.
2. Interleave determinism with feature work.
3. Best-effort determinism, no gate.

## Decision

Option 1. M4.1 defines the canonical trace form and canonical
diagnostic form and gates M4.3/M4.4: identical input plus configuration
yields byte-identical canonical forms. Seed independence (no RNG input)
and locale independence (no locale-dependent rendering) are required.
`Debug` formatting is explicitly unstable and excluded from comparison.

## Consequences

- M4.3/M4.4 tests compare canonical forms only.
- Locale/seed bugs surface at the gate, not in feature tests.
- SPEC Sec 5 (Determinism) touched.
