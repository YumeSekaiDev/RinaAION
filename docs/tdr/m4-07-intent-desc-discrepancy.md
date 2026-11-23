# Specification issue — intent description slot

Status: recorded, out of scope for M4.3. No action taken.

## Discrepancy

SPEC Appendix A defines `GatedIntent = 'intent' Block`, with no
description slot. The parser accepts an optional string description
(`intent "desc" { ... }`), defaulting to empty.

## Disposition

This file records the discrepancy only. The M4.3 schema carries the
parser-provided description (`""` when absent, omitted from JSON when
empty) without legitimizing new syntax. Amending the grammar row or
restricting the parser is explicitly outside the M4.3 scope and
requires a separate Product Owner decision.
