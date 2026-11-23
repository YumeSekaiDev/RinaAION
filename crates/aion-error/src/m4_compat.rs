//! M4.2 diagnostic serializer + version-checked reader compat tests.
//! Reader errors are plain strings (never diagnostics); comparisons are on
//! bytes/strings only, never `Debug` formatting.

use super::{parse_envelope, Diagnostic, ErrorCode};

fn round_trip(golden: &[u8], name: &str) {
    let diags = parse_envelope(golden).unwrap_or_else(|e| panic!("parse {name}: {e}"));
    let re = Diagnostic::canonical_envelope_bytes(&diags);
    assert_eq!(re, golden, "round-trip must be byte-identical for {name}");
}

#[test]
fn round_trip_canonical_goldens() {
    round_trip(
        include_bytes!("../../../tests/golden/m4/canonical/diag_full.json"),
        "diag_full.json",
    );
    round_trip(
        include_bytes!("../../../tests/golden/m4/canonical/diag_bare.json"),
        "diag_bare.json",
    );
    round_trip(
        include_bytes!("../../../tests/golden/m4/canonical/diag_related.json"),
        "diag_related.json",
    );
}

#[test]
fn compat_version2_rejected() {
    let bytes = include_bytes!("../../../tests/golden/m4/compat/diag_version2.json");
    let err = parse_envelope(bytes).expect_err("version 2 must be rejected");
    assert!(err.contains('2'), "error names the version: {err}");
}

#[test]
fn compat_missing_version_rejected() {
    let bytes = include_bytes!("../../../tests/golden/m4/compat/diag_missing_version.json");
    parse_envelope(bytes).expect_err("missing version must be rejected");
}

#[test]
fn compat_unknown_extra_field_accepted() {
    let bytes = include_bytes!("../../../tests/golden/m4/compat/diag_extra_field.json");
    let diags = parse_envelope(bytes).expect("unknown fields must be ignored");
    // Re-emit drops the unknown fields, matching the canonical golden.
    let canonical = include_bytes!("../../../tests/golden/m4/canonical/diag_full.json");
    assert_eq!(Diagnostic::canonical_envelope_bytes(&diags), canonical);
}

#[test]
fn compat_provenance_extra_field_accepted() {
    // T14: unknown fields beside and inside `provenance` are ignored;
    // re-emit drops them, matching the canonical provenance golden.
    let bytes = include_bytes!("../../../tests/golden/m4/compat/diag_provenance_extra_field.json");
    let diags = parse_envelope(bytes).expect("provenance with extras must parse");
    assert_eq!(diags.len(), 1);
    assert_eq!(
        diags[0].provenance,
        Some(super::AssertProvenance::transformed(
            Some("s1".to_string()),
            Some("t9".to_string())
        )),
        "provenance preserved, unknowns dropped"
    );
    let canonical =
        include_bytes!("../../../tests/golden/m4/canonical/diag_provenance_transformed.json");
    assert_eq!(Diagnostic::canonical_envelope_bytes(&diags), canonical);
}

#[test]
fn compat_nested_unknown_object_skipped() {
    // T14b: old-reader tolerance covers nested unknown objects (the
    // shape a future `provenance` key would take), not just flat fields.
    let raw = b"{\"schema\":\"aion.diagnostic/v1\",\"version\":1,\"diagnostics\":[{\"severity\":\"error\",\"code\":\"AION-E0002\",\"message\":\"bad token\",\"x_nested\":{\"a\":[1,{\"b\":null}],\"c\":{}}}]}\n";
    let diags = parse_envelope(raw).expect("nested unknown fields must be ignored");
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].code, ErrorCode::UnexpectedToken);
    assert_eq!(diags[0].provenance, None);
}

#[test]
fn no_new_error_codes() {
    // Reader surface uses plain strings only: every failure is Err(String).
    let bad: &[&[u8]] = &[
        include_bytes!("../../../tests/golden/m4/compat/diag_version2.json"),
        include_bytes!("../../../tests/golden/m4/compat/diag_missing_version.json"),
        b"not json\n",
        b"\n",
    ];
    for b in bad {
        let e = parse_envelope(b).expect_err("must fail");
        let s: String = e;
        assert!(!s.is_empty());
    }
}
