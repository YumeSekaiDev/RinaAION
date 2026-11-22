//! M4.1 canonical-diagnostic tests (additive; legacy rendering untouched).

use super::*;
use aion_span::FileId;

fn span(s: u32, e: u32) -> ByteSpan {
    ByteSpan {
        file: FileId(0),
        start: s,
        end: e,
    }
}

fn fixture_full() -> Diagnostic {
    Diagnostic::new(ErrorCode::UnexpectedToken, "bad token")
        .with_span(span(3, 5))
        .with_note("try again")
}

fn fixture_bare() -> Diagnostic {
    Diagnostic::new(ErrorCode::ParseError, "boom")
}

fn fixture_related() -> Diagnostic {
    Diagnostic::new(ErrorCode::TypeMismatch, "mismatch")
        .with_span(span(0, 4))
        .with_note("n1")
        .with_related(span(1, 2), "lbl")
}

#[test]
fn canonical_envelope_matches_goldens() {
    assert_eq!(
        Diagnostic::canonical_envelope_bytes(&[fixture_full()]),
        include_bytes!("../../../tests/golden/m4/canonical/diag_full.json")
    );
    assert_eq!(
        Diagnostic::canonical_envelope_bytes(&[fixture_bare()]),
        include_bytes!("../../../tests/golden/m4/canonical/diag_bare.json")
    );
    assert_eq!(
        Diagnostic::canonical_envelope_bytes(&[fixture_related()]),
        include_bytes!("../../../tests/golden/m4/canonical/diag_related.json")
    );
}

#[test]
fn canonical_omit_when_empty() {
    // Absent span omitted (no null), empty notes/related omitted.
    let bare = fixture_bare().render_canonical_json();
    assert!(!bare.contains("\"span\""), "absent span must be omitted");
    assert!(!bare.contains("\"notes\""), "empty notes must be omitted");
    assert!(
        !bare.contains("\"related\""),
        "empty related must be omitted"
    );
    // Full form keeps fixed key order.
    let full = fixture_full().render_canonical_json();
    let order = ["severity", "code", "message", "span", "notes"];
    let mut pos = 0;
    for k in order {
        let key = format!("\"{k}\":");
        let p = full.find(&key).unwrap_or_else(|| panic!("missing {k}"));
        assert!(p >= pos, "key order violated at {k}");
        pos = p;
    }
    // Location byte-exact within a variant.
    assert!(full.contains("\"span\":{\"file\":0,\"start\":3,\"end\":5}"));
    let rel = fixture_related().render_canonical_json();
    assert!(rel.contains("\"related\":[{\"file\":0,\"start\":1,\"end\":2,\"label\":\"lbl\"}]"));
}

#[test]
fn canonical_envelope_shape_and_terminator() {
    let bytes = Diagnostic::canonical_envelope_bytes(&[fixture_full(), fixture_bare()]);
    assert!(bytes.ends_with(b"\n"), "single LF terminator required");
    assert_eq!(bytes.iter().filter(|&&b| b == b'\n').count(), 1);
    let s = String::from_utf8(bytes).expect("UTF-8");
    assert!(s.starts_with("{\"schema\":\"aion.diagnostic/v1\",\"version\":1,\"diagnostics\":["));
}

#[test]
fn legacy_render_byte_identical_to_goldens() {
    assert_eq!(
        fixture_full().render_json().as_bytes(),
        include_bytes!("../../../tests/golden/m4/legacy/diag_full.json")
    );
    assert_eq!(
        fixture_bare().render_json().as_bytes(),
        include_bytes!("../../../tests/golden/m4/legacy/diag_bare.json")
    );
}

#[test]
fn no_rng_time_or_locale_in_canonical_paths() {
    let src = include_str!("lib.rs");
    // Restrict to the canonical constructors (legacy esc/render untouched).
    let at = src
        .find("render_canonical_json")
        .expect("canonical ctor present");
    let canon = &src[at..];
    for pat in [
        "SystemTime",
        "UNIX_EPOCH",
        "rand",
        "Rng",
        "thread_rng",
        "setlocale",
        "LC_ALL",
    ] {
        assert!(
            !canon.contains(pat),
            "canonical path must not contain {pat}"
        );
    }
}
