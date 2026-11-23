# M4.3 Closure Record — Intent and Goal Semantics (ACCEPTED, FROZEN)

Status: complete and Product Owner accepted. No commit. No push. M4.4 not started.

## Completion and acceptance

M4.3 implements AI-facing intent/goal exposure exactly per the approved
`SPEC.md` Sec 5, TDR-M4-3 (decisions D1–D14), and the four frozen
Product Owner decisions (`Goal(Expr)`; per-item attachment; new
`aion.intent/v1` envelope; provenance deferred) plus the adopted
R-FOLLOWING ownership rule. Implementation spans syntax (`goal :
Expr ;`), semantic model + sema (`IntentEntry`/`GoalEntry`/`GoalRef`,
resolution-only `check_goal`, synthesized entries, anchor sorting),
query (`entry_intents()` only), JSON (`aion.intent/v1` emitter/reader),
and driver (`analyze_with_flags`, `run_intent_json`). Independent QA
verdict: PASS, no remaining flags.

## Final verification results (pinned Rust 1.89.0, `--locked --offline`)

- `cargo test --lib --tests`: 227 passed, 0 failed (167 M4.2 baseline + 60 new).
- `cargo fmt --check`: clean. `cargo clippy --all-targets -- -D warnings`: clean.
- `scripts/check_grammar_sync.sh`: `spec-grammar-sync: OK`.

## Deferred technical debt

1. Metadata-aware lowering: `aion-ir` rejects `Item::Intent`/`Stmt::Goal`,
   so Suite A executes the metadata variant from a metadata-stripped
   parse with `shape()`-equality proof. Future work: tolerate/ignore
   metadata items in lowering so a later suite executes P+metadata
   directly. Test-only quarantine holds; no production impact.
2. Trace-equivalence verification deferred: Suite A compares result
   value, fuel, and full trap identity+payload across the two
   executable paths, and checks the public empty-event trace
   serializer output against its golden file. Cross-path trace
   equivalence is NOT established and awaits a traced-execution
   interface.
3. Recorded specification gaps (out of scope, no action taken):
   `docs/tdr/m4-06-call-args-discrepancy.md` (zero-arg calls),
   `docs/tdr/m4-07-intent-desc-discrepancy.md` (intent description slot).
4. Deferred per approval: provenance, solver-backed discharge, orphan
   query surface, M4.4 assertion semantics, M5.

## Current repository state

HEAD `ba25030` (M4.2). M4.3 complete in the working tree, uncommitted:
16 modified tracked files, 12 created M4.3 paths (sources, tests,
fixtures, goldens, closure record, 2 TDR records), plus authorized
SPEC/TDR updates.
Pre-existing untracked non-M4.3 files (`AGENTS.md`, `SESSION_HANDOFF.md`,
`Codex Report.txt`, `Muse Code Replies.txt`) untouched.
Nothing committed, nothing pushed, nothing stashed.
