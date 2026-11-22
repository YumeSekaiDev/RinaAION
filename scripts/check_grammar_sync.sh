#!/usr/bin/env bash
set -euo pipefail
# Gate: syntax/grammar.ungram must stay derived from SPEC.md Appendix A.
# M0: check derived-header marker + rule-name parity between the
# ```ungram fenced block in SPEC.md and syntax/grammar.ungram body.
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SPEC="$ROOT/SPEC.md"
GRAMMAR="$ROOT/syntax/grammar.ungram"

fail() { echo "spec-grammar-sync: FAIL: $*" >&2; exit 1; }

grep -q "DERIVED FILE" "$GRAMMAR" || fail "$GRAMMAR missing DERIVED header"
grep -q "SPEC.md Appendix A" "$GRAMMAR" || fail "$GRAMMAR header must cite SPEC Appendix A"

# Extract rule names (LHS before '=') from both sources.
spec_rules=$(sed -n '/```ungram/,/```/p' "$SPEC" | grep -E '^[A-Za-z_]+ *=' | cut -d= -f1 | tr -d ' ' | sort)
grammar_rules=$(grep -E '^[A-Za-z_]+ *=' "$GRAMMAR" | cut -d= -f1 | tr -d ' ' | sort)
[ -n "$spec_rules" ] || fail "no ungram rules found in $SPEC"
[ "$spec_rules" = "$grammar_rules" ] || fail "rule mismatch:
SPEC: $(echo "$spec_rules" | tr '\n' ' ')
GRAMMAR: $(echo "$grammar_rules" | tr '\n' ' ')"
echo "spec-grammar-sync: OK"
