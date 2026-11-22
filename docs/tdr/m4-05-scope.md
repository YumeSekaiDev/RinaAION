# TDR-M4-5 — M4 scope and ordering

## Context

M4 bundles versioning, determinism, query surface, and assertions.
Order and deferrals need writing down so M4.3/M4.4 land on the gate.

## Options

1. Ordered: M4.1 (determinism/canonical-form gate + versioning) before
   M4.3 (query surface) / M4.4 (asserts).
2. Parallel tracks with late integration.
3. All-in-one milestone.

## Decision

Option 1. Ordered milestones, all four required:
M4.1 Verification, determinism, and versioning (TDR-M4-1, TDR-M4-2);
M4.2 AI-facing JSON interfaces (per the versioned envelopes);
M4.3 Intent and goal exposure (TDR-M4-3); M4.4 Assertion semantics
(TDR-M4-4). M4.1 gates M4.3/M4.4. Deferrals: no M5 work, no
implementation in this round (docs only), no solver-backed assertion
discharge, no `Debug` stability promise. Acceptance: non-interference
tests (each executable program P vs the otherwise identical P+metadata:
byte-identical results, traces, fuel, and traps), golden JSON tests
per envelope version, shared-model consistency tests.

## Consequences

- M4.3/M4.4 tests run against a frozen canonical baseline.
- Docs-only round keeps `crates/`, `tests/`, `scripts/`, `syntax/`, CI
  untouched.
- SPEC Sec 5 touched.
