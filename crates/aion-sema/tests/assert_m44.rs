//! M4.4 milestone M2: assertion-specific E0010 provenance, E0016
//! intent-block rejection, and non-moving assertion operand reads.

use aion_error::{AssertProvenance, Diagnostic, ErrorCode, Severity};
use aion_span::FileId;

fn check(src: &str) -> Vec<Diagnostic> {
    let r = aion_syntax::parse_with_flags(
        FileId(0),
        src.as_bytes(),
        aion_syntax::FeatureFlags::all_experimental(),
    );
    assert!(
        r.errors.is_empty(),
        "source must parse clean: {src:?}: {:?}",
        r.errors.iter().map(|d| d.code).collect::<Vec<_>>()
    );
    let (diags, _) = aion_sema::check_with_model(&r, FileId(0), src.as_bytes());
    diags
}

/// Byte span of the first `assert … ;` statement in `src`.
fn assert_stmt_span(src: &str) -> (u32, u32) {
    let start = src
        .find("assert")
        .unwrap_or_else(|| panic!("no assert in {src:?}")) as u32;
    let semi = src[start as usize..]
        .find(';')
        .unwrap_or_else(|| panic!("no `;` in {src:?}"));
    (start, start + semi as u32 + 1)
}

#[test]
fn assert_e0010_carries_provenance() {
    let src = "fn main() { assert 1; }";
    let diags = check(src);
    assert_eq!(diags.len(), 1, "expected exactly one diagnostic");
    let d = &diags[0];
    assert_eq!(d.code, ErrorCode::TypeMismatch);
    assert_eq!(d.code.code_str(), "AION-E0010");
    assert_eq!(d.message, "operand type mismatch");
    assert_eq!(d.severity, Severity::Error);
    assert_eq!(d.provenance, Some(AssertProvenance::human_authored()));
    // Span is the operand span, unchanged from M1 behavior.
    let op = src.find(" 1;").unwrap() as u32 + 1;
    assert_eq!(d.span.map(|sp| (sp.start, sp.end)), Some((op, op + 1)));
}

#[test]
fn unrelated_e0010_has_no_provenance() {
    let diags = check("fn main() { do 1 + true; }");
    assert_eq!(diags.len(), 1, "expected exactly one diagnostic");
    let d = &diags[0];
    assert_eq!(d.code, ErrorCode::TypeMismatch);
    assert_eq!(d.provenance, None);
}

#[test]
fn e0016_top_level_intent_assert() {
    let src = "intent \"d\" { assert true; } fn main() {}";
    let diags = check(src);
    assert_eq!(diags.len(), 1, "exactly one diagnostic, no duplicates");
    let d = &diags[0];
    assert_eq!(d.code, ErrorCode::AssertInIntent);
    assert_eq!(d.code.code_str(), "AION-E0016");
    assert_eq!(
        d.message,
        "assert statements are not allowed in intent blocks"
    );
    assert_eq!(d.severity, Severity::Error);
    assert_eq!(d.provenance, Some(AssertProvenance::human_authored()));
    let (start, end) = assert_stmt_span(src);
    assert_eq!(
        d.span.map(|sp| (sp.start, sp.end)),
        Some((start, end)),
        "E0016 must point at the offending assert statement"
    );
}

#[test]
fn e0016_nested_intent_assert() {
    let src = "intent \"d\" { do 1; { assert false; } } fn main() {}";
    let diags = check(src);
    assert_eq!(diags.len(), 1, "exactly one diagnostic, no duplicates");
    let d = &diags[0];
    assert_eq!(d.code, ErrorCode::AssertInIntent);
    assert_eq!(d.provenance, Some(AssertProvenance::human_authored()));
    let (start, end) = assert_stmt_span(src);
    assert_eq!(
        d.span.map(|sp| (sp.start, sp.end)),
        Some((start, end)),
        "nested E0016 must point at the inner assert statement"
    );
}

#[test]
fn e0016_let_and_do_unaffected() {
    let diags = check("intent \"d\" { let x = 1; do x; } fn main() {}");
    assert!(
        diags.iter().all(|d| d.code != ErrorCode::AssertInIntent),
        "let/do must not trigger E0016"
    );
    assert!(diags.is_empty(), "intent let/do are unchecked: {diags:?}");
}

#[test]
fn assert_operand_reads_do_not_move() {
    // `a` is read (not moved) by the assert operand, so the later
    // `do id(a)` is a legal first move.
    let diags =
        check("fn id(s) { do s; } fn main() { let a = \"hi\"; assert id(a) == \"x\"; do id(a); }");
    assert!(
        diags.is_empty(),
        "assert operand must not move: {:?}",
        diags.iter().map(|d| d.render_human()).collect::<Vec<_>>()
    );
}

#[test]
fn assert_operand_use_after_move_still_fires() {
    // `a` was genuinely moved before the assert; reading it inside the
    // operand must still diagnose E0020.
    let diags =
        check("fn id(s) { do s; } fn main() { let a = \"hi\"; do id(a); assert id(a) == \"x\"; }");
    assert_eq!(diags.len(), 1, "expected exactly one diagnostic");
    assert_eq!(diags[0].code, ErrorCode::UseAfterMove);
}

#[test]
fn call_arg_double_use_fires_e0020_without_assert() {
    // Control: without any assert, a second call-arg use of a moved
    // `Str` is E0020. Proves the harness detects call-arg moves.
    let diags = check("fn id(s) { do s; } fn main() { let a = \"hi\"; do id(a); do id(a); }");
    assert_eq!(diags.len(), 1, "expected exactly one diagnostic");
    assert_eq!(diags[0].code, ErrorCode::UseAfterMove);
}
