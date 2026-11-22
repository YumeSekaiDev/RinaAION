# TDR-M3-6 — Scope (entry, moves, null, grammar)

## Context

Four boundary questions recurred: which `main` counts as entry, who
owns move authority, who owns the `null` literal reading, and whether
the grammar needs changes for nested blocks.

## Decision

Entry: exactly one zero-param `fn main`; missing/mismatched arity is
E0015, duplicate `main` stays E0013. Moves: executor has no inference;
a move executes iff M2 recorded a move fact, at exactly the three Sec 4
sites. `null`: parser is authoritative (`Lit` node); sema fallback is
defensive. Grammar: `Stmt = LetStmt | DoStmt | Block` already covers
nested blocks in both Appendix A and `grammar.ungram` — kept as is,
no restructure.

## Consequences

- Entry disputes resolve to two codes with no overlap.
- Move behavior cannot drift between analysis and execution.
- Parser/sema `null` readings cannot conflict silently.
- Grammar sync gate stays green with zero derived-file churn.
