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

#[test]
fn provenance_omitted_for_default_and_absent() {
    // T02b: `None` and `HumanAuthored` emit byte-identical output to
    // the pre-provenance writer (existing goldens pin this).
    let bare_none = fixture_bare().render_canonical_json();
    assert!(!bare_none.contains("provenance"));
    let bare_human = fixture_bare()
        .with_provenance(AssertProvenance::human_authored())
        .render_canonical_json();
    assert_eq!(bare_human, bare_none);
    let bytes = Diagnostic::canonical_envelope_bytes(&[
        fixture_full().with_provenance(AssertProvenance::default())
    ]);
    let golden = include_bytes!("../../../tests/golden/m4/canonical/diag_full.json");
    assert_eq!(
        bytes.as_slice(),
        &golden[..],
        "HumanAuthored must not alter canonical bytes"
    );
}

#[test]
fn provenance_object_exact_bytes_and_last_position() {
    // T02b/T11: exact canonical bytes; the provenance object is the
    // last field, after `related`.
    let ai = fixture_bare().with_provenance(AssertProvenance::ai_generated(Some("agent-7")));
    assert_eq!(
        ai.render_canonical_json(),
        "{\"severity\":\"error\",\"code\":\"AION-E0004\",\"message\":\"boom\",\"provenance\":{\"origin\":\"ai-generated\",\"identity\":\"agent-7\"}}"
    );
    let ai_bare = fixture_bare().with_provenance(AssertProvenance::ai_generated(None::<String>));
    assert_eq!(
        ai_bare.render_canonical_json(),
        "{\"severity\":\"error\",\"code\":\"AION-E0004\",\"message\":\"boom\",\"provenance\":{\"origin\":\"ai-generated\"}}"
    );
    let tx = fixture_related().with_provenance(AssertProvenance::transformed(
        Some("s1".to_string()),
        Some("t9".to_string()),
    ));
    let j = tx.render_canonical_json();
    assert_eq!(
        j,
        "{\"severity\":\"error\",\"code\":\"AION-E0010\",\"message\":\"mismatch\",\"span\":{\"file\":0,\"start\":0,\"end\":4},\"notes\":[\"n1\"],\"related\":[{\"file\":0,\"start\":1,\"end\":2,\"label\":\"lbl\"}],\"provenance\":{\"origin\":\"transformed\",\"source_ref\":\"s1\",\"transform_id\":\"t9\"}}"
    );
    // Empty identifiers normalize to absent on the wire.
    let tx_empty = fixture_bare().with_provenance(AssertProvenance::transformed(
        Some(String::new()),
        Some(String::new()),
    ));
    assert_eq!(
        tx_empty.render_canonical_json(),
        "{\"severity\":\"error\",\"code\":\"AION-E0004\",\"message\":\"boom\",\"provenance\":{\"origin\":\"transformed\"}}"
    );
    // Opaque identifiers are escaped with the shared escaper.
    let quoted = fixture_bare().with_provenance(AssertProvenance::ai_generated(Some("a\"b\\c")));
    assert!(
        quoted
            .render_canonical_json()
            .contains("\"identity\":\"a\\\"b\\\\c\""),
        "identifier escaping: {}",
        quoted.render_canonical_json()
    );
}

fn envelope_of(diag_json: &str) -> Vec<u8> {
    format!("{{\"schema\":\"aion.diagnostic/v1\",\"version\":1,\"diagnostics\":[{diag_json}]}}\n")
        .into_bytes()
}

fn assert_provenance_golden(golden: &[u8], name: &str) -> Diagnostic {
    let back = parse_envelope(golden).unwrap_or_else(|e| panic!("parse {name}: {e}"));
    assert_eq!(back.len(), 1, "{name}");
    let re = Diagnostic::canonical_envelope_bytes(&back);
    assert_eq!(
        re.as_slice(),
        golden,
        "{name}: round-trip must be byte-identical"
    );
    back.into_iter().next().unwrap()
}

#[test]
fn provenance_canonical_goldens_round_trip() {
    // T14: reviewed goldens for every non-default origin shape; parse
    // then re-emit is byte-identical and provenance is preserved.
    let d = assert_provenance_golden(
        include_bytes!("../../../tests/golden/m4/canonical/diag_provenance_ai.json"),
        "diag_provenance_ai.json",
    );
    assert_eq!(d.code, ErrorCode::RuntimeTrap);
    assert_eq!(
        d.provenance,
        Some(AssertProvenance::ai_generated(Some("agent-7")))
    );
    let d = assert_provenance_golden(
        include_bytes!("../../../tests/golden/m4/canonical/diag_provenance_ai_bare.json"),
        "diag_provenance_ai_bare.json",
    );
    assert_eq!(
        d.provenance,
        Some(AssertProvenance::ai_generated(None::<String>))
    );
    let d = assert_provenance_golden(
        include_bytes!("../../../tests/golden/m4/canonical/diag_provenance_transformed.json"),
        "diag_provenance_transformed.json",
    );
    assert_eq!(d.code, ErrorCode::TypeMismatch);
    assert_eq!(
        d.provenance,
        Some(AssertProvenance::transformed(
            Some("s1".to_string()),
            Some("t9".to_string())
        ))
    );
    let d = assert_provenance_golden(
        include_bytes!("../../../tests/golden/m4/canonical/diag_provenance_transformed_bare.json"),
        "diag_provenance_transformed_bare.json",
    );
    assert_eq!(
        d.provenance,
        Some(AssertProvenance::transformed(
            None::<String>,
            None::<String>
        ))
    );
}

#[test]
fn provenance_round_trip_non_default_origins() {
    // T02c/T14d: writer output parses back identically, including
    // identifier-bearing and identifier-free variants.
    for p in [
        AssertProvenance::ai_generated(Some("agent-7")),
        AssertProvenance::ai_generated(None::<String>),
        AssertProvenance::transformed(Some("s1".to_string()), Some("t9".to_string())),
        AssertProvenance::transformed(None::<String>, None::<String>),
    ] {
        let d = fixture_bare().with_provenance(p.clone());
        let bytes = Diagnostic::canonical_envelope_bytes(&[d]);
        let back = parse_envelope(&bytes).expect("must parse");
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].code, ErrorCode::ParseError);
        assert_eq!(back[0].message, "boom");
        assert_eq!(back[0].provenance, Some(p));
    }
}

#[test]
fn keyless_and_null_provenance_reads_none() {
    // T12/T14a: old artifacts without the key, and explicit nulls,
    // both yield None — identically for codes that can carry
    // provenance (E0010/E0044) and codes that cannot.
    for code in [
        ErrorCode::TypeMismatch,
        ErrorCode::RuntimeTrap,
        ErrorCode::ParseError,
    ] {
        let bytes = Diagnostic::canonical_envelope_bytes(&[Diagnostic::new(code, "m")]);
        let back = parse_envelope(&bytes).expect("must parse");
        assert_eq!(back[0].provenance, None, "keyless: {:?}", code.code_str());
    }
    let raw = envelope_of(
        "{\"severity\":\"error\",\"code\":\"AION-E0044\",\"message\":\"m\",\"provenance\":null}",
    );
    let back = parse_envelope(&raw).expect("explicit null must parse");
    assert_eq!(back[0].provenance, None);
    // Old golden artifacts carry no provenance either.
    let golden = include_bytes!("../../../tests/golden/m4/canonical/diag_full.json");
    let back = parse_envelope(golden).expect("golden must parse");
    assert!(back.iter().all(|d| d.provenance.is_none()));
}

#[test]
fn unknown_provenance_origin_fails_closed() {
    // T12: only the two non-default origins are accepted; anything
    // else — including a HumanAuthored-shaped origin the writer
    // never emits — is a reader error.
    for origin in ["\"human-authored\"", "\"ai\"", "\"\"", "42"] {
        let raw = envelope_of(&format!(
            "{{\"severity\":\"error\",\"code\":\"AION-E0010\",\"message\":\"m\",\"provenance\":{{\"origin\":{origin}}}}}"
        ));
        assert!(
            parse_envelope(&raw).is_err(),
            "origin {origin} must fail closed"
        );
    }
    let raw = envelope_of(
        "{\"severity\":\"error\",\"code\":\"AION-E0010\",\"message\":\"m\",\"provenance\":{}}",
    );
    assert!(parse_envelope(&raw).is_err(), "missing origin must fail");
}

#[test]
fn mismatched_provenance_fields_are_dropped() {
    // T12: identifier fields incompatible with the declared origin
    // are dropped in both directions; never an error.
    let raw = envelope_of("{\"severity\":\"error\",\"code\":\"AION-E0010\",\"message\":\"m\",\"provenance\":{\"origin\":\"ai-generated\",\"identity\":\"g\",\"source_ref\":\"s\",\"transform_id\":\"t\"}}");
    let back = parse_envelope(&raw).expect("must parse");
    assert_eq!(
        back[0].provenance,
        Some(AssertProvenance::ai_generated(Some("g")))
    );
    let raw = envelope_of("{\"severity\":\"error\",\"code\":\"AION-E0044\",\"message\":\"m\",\"provenance\":{\"origin\":\"transformed\",\"identity\":\"g\",\"source_ref\":\"s\"}}");
    let back = parse_envelope(&raw).expect("must parse");
    assert_eq!(
        back[0].provenance,
        Some(AssertProvenance::transformed(
            Some("s".to_string()),
            None::<String>
        ))
    );
}

#[test]
fn empty_identifiers_normalize_to_absent() {
    // T02c: empty identifier strings read as absent without changing
    // the declared origin.
    let raw = envelope_of("{\"severity\":\"error\",\"code\":\"AION-E0010\",\"message\":\"m\",\"provenance\":{\"origin\":\"ai-generated\",\"identity\":\"\"}}");
    let back = parse_envelope(&raw).expect("must parse");
    assert_eq!(
        back[0].provenance,
        Some(AssertProvenance::ai_generated(None::<String>))
    );
    let raw = envelope_of("{\"severity\":\"error\",\"code\":\"AION-E0044\",\"message\":\"m\",\"provenance\":{\"origin\":\"transformed\",\"source_ref\":\"\",\"transform_id\":\"\"}}");
    let back = parse_envelope(&raw).expect("must parse");
    assert_eq!(
        back[0].provenance,
        Some(AssertProvenance::transformed(
            None::<String>,
            None::<String>
        ))
    );
}

#[test]
fn provenance_unknown_subkeys_skipped_and_code_ignored() {
    // T12/T14: unknown sub-keys are skipped, and the diagnostic code
    // plays no role: the same provenance body yields the same value
    // under assertion and non-assertion codes.
    let body = "\"provenance\":{\"origin\":\"transformed\",\"source_ref\":\"s\",\"x_future\":{\"nested\":[1,2]},\"zzz\":42}";
    let mut first = None;
    for code in ["AION-E0010", "AION-E0044", "AION-E0002"] {
        let raw = envelope_of(&format!(
            "{{\"severity\":\"error\",\"code\":\"{code}\",\"message\":\"m\",{body}}}"
        ));
        let back = parse_envelope(&raw).expect("must parse");
        let expected = Some(AssertProvenance::transformed(
            Some("s".to_string()),
            None::<String>,
        ));
        assert_eq!(back[0].provenance, expected, "code {code}");
        if let Some(prev) = first.replace(back[0].provenance.clone()) {
            assert_eq!(prev, back[0].provenance, "code-independent");
        }
    }
}
