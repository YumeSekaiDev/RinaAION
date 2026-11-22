# TDR-M4-1 — JSON versioning policy

## Context

M4 introduces versioned JSON envelopes (canonical trace/diagnostic
forms). Consumers need compat rules and a stewardship owner before
M4.3/M4.4 ship golden artifacts.

## Options

1. Unversioned JSON (status quo): breaks consumers silently on change.
2. Versioned envelopes with steward + compat rules + JSON goldens.
3. Per-endpoint version negotiation: overkill for file-form outputs.

## Decision

Option 2. Every JSON envelope carries `"schema":
"aion.<artifact>/v<MAJOR>"` and `"version": <MAJOR>` (integer, starting
at 1), checked before parsing. Stewardship: SPEC owns the version
number; bumps ship with a SPEC note. Compat rules: additive fields only
within a major; renames, removals, retypings, and semantic changes
require a major bump. Readers must reject unknown major versions and
must ignore unknown fields. Golden strategy: tested JSON goldens per
envelope version (canonical forms only; `Debug` output is explicitly
unstable and never golden-tested).

## Consequences

- Consumers pin on the version field, not on incidental formatting.
- Omit-when-empty stays preserved across versions (no null/empty-list
  drift without a major bump).
- SPEC Sec 5 (JSON contract pointers) touched.
