//! M1 golden corpus: valid fixtures, invalid per-code span snapshots,
//! parse-print-parse round-trip equivalence, formatter idempotence,
//! and experimental gating.

use aion_span::FileId;
use aion_syntax::{parse, parse_with_flags, FeatureFlags};

fn parse_fixture(rel: &str) -> (Vec<u8>, aion_syntax::ParseResult) {
    let path = format!(
        "{}/../../tests/fixtures/m1/{rel}",
        env!("CARGO_MANIFEST_DIR")
    );
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let r = parse(FileId(0), &bytes);
    (bytes, r)
}

// ---- valid fixtures must parse clean ----

const VALID: &[&str] = &[
    "valid/comments.aion",
    "valid/block_comment.aion",
    "valid/identifiers_delimiters.aion",
    "valid/literals.aion",
    "valid/precedence.aion",
    "valid/let_do.aion",
    "valid/multi_fn.aion",
];

#[test]
fn m1_valid_fixtures_parse_clean() {
    for rel in VALID {
        let (_, r) = parse_fixture(rel);
        assert!(
            r.errors.is_empty(),
            "{rel} must parse clean: {:?}",
            r.errors
                .iter()
                .map(|d| d.render_human())
                .collect::<Vec<_>>()
        );
        assert!(!r.program.items.is_empty(), "{rel} must yield items");
    }
}

// ---- invalid fixtures: one per error code with span snapshots ----

#[test]
fn m1_invalid_e0001_span_snapshot() {
    let (_, r) = parse_fixture("invalid/e0001_invalid_byte.aion");
    let e1: Vec<_> = r
        .errors
        .iter()
        .map(|d| d.render_human())
        .filter(|h| h.contains("AION-E0001"))
        .collect();
    assert_eq!(e1.len(), 1);
    let h = &e1[0];
    assert!(h.contains("AION-E0001"), "{h}");
    assert!(h.contains("@ file#0:1..2"), "{h}");
}

#[test]
fn m1_invalid_e0002_span_snapshot() {
    let (_, r) = parse_fixture("invalid/e0002_unexpected_token.aion");
    assert!(!r.errors.is_empty());
    // `fn broken( {}` — the `{` where a param name was expected.
    let h = r.errors[0].render_human();
    assert!(h.contains("AION-E0002"), "{h}");
    assert!(h.contains("@ file#0:11..12"), "{h}");

    let (_, r2) = parse_fixture("invalid/e0002_missing_expr.aion");
    assert!(!r2.errors.is_empty());
    assert!(r2
        .errors
        .iter()
        .any(|d| d.render_human().contains("AION-E0002")));
}

#[test]
fn m1_invalid_e0003_span_snapshot() {
    // Missing `}` at EOF: error has no misleading span or EOF note.
    let (_, r) = parse_fixture("invalid/e0003_unexpected_eof.aion");
    assert!(!r.errors.is_empty());
    assert!(r
        .errors
        .iter()
        .any(|d| d.render_human().contains("AION-E0003")));
    // Unterminated string literal reports E0003 with the literal's span.
    let (_, r2) = parse_fixture("invalid/e0003_unterminated_string.aion");
    assert!(r2
        .errors
        .iter()
        .any(|d| d.render_human().contains("AION-E0003")));
    let h = r2
        .errors
        .iter()
        .map(|d| d.render_human())
        .find(|h| h.contains("AION-E0003"))
        .unwrap();
    assert!(h.contains("@ file#0:"), "{h}");
}

#[test]
fn m1_invalid_e0004_span_snapshot() {
    let (_, r) = parse_fixture("invalid/e0004_bad_int.aion");
    assert!(!r.errors.is_empty());
    let h = r
        .errors
        .iter()
        .map(|d| d.render_human())
        .find(|h| h.contains("AION-E0004"))
        .expect("expected E0004");
    assert!(h.contains("invalid integer literal"), "{h}");
    assert!(h.contains("@ file#0:"), "{h}");
}

// ---- round-trip: parse(x) == parse(fmt(parse(x))) and idempotence ----

#[test]
fn m1_round_trip_equivalence() {
    for rel in VALID {
        let (bytes, r) = parse_fixture(rel);
        assert!(r.errors.is_empty(), "{rel}: {bytes:?}");
        let once = r.formatted();
        let r2 = parse(FileId(0), once.as_bytes());
        assert!(r2.errors.is_empty(), "{rel} reparse: {:?}", r2.errors);
        assert_eq!(r.shape(), r2.shape(), "{rel} shape mismatch:\n{once}");
    }
}

#[test]
fn m1_formatter_idempotent() {
    for rel in VALID {
        let (_, r) = parse_fixture(rel);
        let once = r.formatted();
        // Idempotence via round-trip: fmt(parse(fmt(x))) == fmt(x).
        let r2 = parse(FileId(0), once.as_bytes());
        let twice = r2.formatted();
        assert_eq!(once, twice, "{rel} formatter not idempotent");
    }
    // Unformatted input still converges in one step.
    let r = parse(FileId(0), b"fn main(){let x=1+2*3;do x;}");
    assert!(r.errors.is_empty());
    let once = r.formatted();
    assert_eq!(once, "fn main() {\n    let x = 1 + 2 * 3;\n    do x;\n}\n");
    let twice = parse(FileId(0), once.as_bytes()).formatted();
    assert_eq!(once, twice);
}

#[test]
fn m1_precedence_shape_preserved_through_format() {
    // Parenthesization required by precedence must survive the round-trip.
    let r = parse(FileId(0), b"fn main() { let b = (1 + 2) * 3; do b; }");
    assert!(r.errors.is_empty());
    let once = r.formatted();
    assert!(once.contains("(1 + 2) * 3"), "{once}");
    let r2 = parse(FileId(0), once.as_bytes());
    assert_eq!(r.shape(), r2.shape());
}

// ---- experimental gating (default off) ----

#[test]
fn m1_experimental_gated_by_default() {
    // intent block
    let r = parse(FileId(0), b"intent \"goal\" { }");
    assert!(
        r.errors
            .iter()
            .any(|d| d.render_human().contains("AION-E0002")),
        "intent must be gated: {:?}",
        r.errors
    );
    // goal field
    let r = parse(FileId(0), b"fn main() { goal = \"x\"; }");
    assert!(
        r.errors
            .iter()
            .any(|d| d.render_human().contains("AION-E0002")),
        "goal must be gated: {:?}",
        r.errors
    );
    // assert statement
    let r = parse(FileId(0), b"fn main() { assert x; }");
    assert!(
        r.errors
            .iter()
            .any(|d| d.render_human().contains("AION-E0002")),
        "assert must be gated: {:?}",
        r.errors
    );
}

#[test]
fn m1_experimental_enabled_with_flags() {
    let flags = FeatureFlags::all_experimental();
    let r = parse_with_flags(FileId(0), b"intent \"ship\" { }", flags);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let r = parse_with_flags(FileId(0), b"fn main() { goal : x; assert y; }", flags);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    // Round-trip works for experimental items too when enabled.
    let once = r.formatted();
    let r2 = parse_with_flags(FileId(0), once.as_bytes(), flags);
    assert_eq!(r.shape(), r2.shape());
    let twice = r2.formatted();
    assert_eq!(once, twice);
}
