//! M4.1 public-surface tests (integration: public API only, no observer
//! access). Compares canonical diagnostic envelopes and byte-exact legacy
//! rendering against `tests/golden/m4/` artifacts.

use aion_error::{Diagnostic, ErrorCode};
use aion_span::{ByteSpan, FileId};

fn span(s: u32, e: u32) -> ByteSpan {
    ByteSpan {
        file: FileId(0),
        start: s,
        end: e,
    }
}

#[test]
fn canonical_envelope_is_stable_and_terminated() {
    let d = Diagnostic::new(ErrorCode::UnexpectedToken, "bad token")
        .with_span(span(3, 5))
        .with_note("try again");
    let bytes = Diagnostic::canonical_envelope_bytes(&[d]);
    assert_eq!(
        bytes,
        include_bytes!("../../../tests/golden/m4/canonical/diag_full.json")
    );
    assert!(bytes.ends_with(b"\n"));
}

#[test]
fn canonical_omitted_span_case_is_stable() {
    let d = Diagnostic::new(ErrorCode::ParseError, "boom");
    let bytes = Diagnostic::canonical_envelope_bytes(&[d]);
    assert_eq!(
        bytes,
        include_bytes!("../../../tests/golden/m4/canonical/diag_bare.json")
    );
}

#[test]
fn canonical_related_case_is_stable() {
    let d = Diagnostic::new(ErrorCode::TypeMismatch, "mismatch")
        .with_span(span(0, 4))
        .with_note("n1")
        .with_related(span(1, 2), "lbl");
    let bytes = Diagnostic::canonical_envelope_bytes(&[d]);
    assert_eq!(
        bytes,
        include_bytes!("../../../tests/golden/m4/canonical/diag_related.json")
    );
}

#[test]
fn legacy_render_unchanged_byte_exact() {
    let full = Diagnostic::new(ErrorCode::UnexpectedToken, "bad token")
        .with_span(span(3, 5))
        .with_note("try again");
    assert_eq!(
        full.render_json().as_bytes(),
        include_bytes!("../../../tests/golden/m4/legacy/diag_full.json")
    );
    let bare = Diagnostic::new(ErrorCode::ParseError, "boom");
    assert_eq!(
        bare.render_json().as_bytes(),
        include_bytes!("../../../tests/golden/m4/legacy/diag_bare.json")
    );
}

#[test]
fn trace_goldens_are_valid_envelopes() {
    for name in ["trace_basic.json", "trace_str.json", "trace_trap.json"] {
        let _ = name;
    }
    let basic = include_str!("../../../tests/golden/m4/canonical/trace_basic.json");
    assert!(basic.starts_with("{\"schema\":\"aion.trace/v1\",\"version\":1,\"events\":["));
    assert!(basic.ends_with("}\n"));
    let trap = include_str!("../../../tests/golden/m4/canonical/trace_trap.json");
    assert!(trap.contains("\"kind\":\"trap\""));
    assert!(trap.contains("\"code\":\"AION-E0040\""));
    // No absolute paths leak into trace goldens.
    for g in [
        basic,
        include_str!("../../../tests/golden/m4/canonical/trace_str.json"),
        trap,
    ] {
        assert!(!g.contains("/run/"), "no absolute paths in trace");
        assert!(!g.contains("/tmp/"), "no absolute paths in trace");
    }
}
