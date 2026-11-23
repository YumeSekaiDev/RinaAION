//! M4.2 trace serializer + version-checked reader compat tests.
//! Comparisons are on bytes/strings only, never `Debug` formatting.

use super::{parse_trace_envelope, trace_canonical_bytes};

fn round_trip(golden: &[u8], name: &str) {
    let p = parse_trace_envelope(golden).unwrap_or_else(|e| panic!("parse {name}: {e}"));
    let re = trace_canonical_bytes(&p.events, p.truncated, p.kept);
    assert_eq!(re, golden, "round-trip must be byte-identical for {name}");
}

#[test]
fn round_trip_canonical_goldens() {
    round_trip(
        include_bytes!("../../../tests/golden/m4/canonical/trace_basic.json"),
        "trace_basic.json",
    );
    round_trip(
        include_bytes!("../../../tests/golden/m4/canonical/trace_str.json"),
        "trace_str.json",
    );
    round_trip(
        include_bytes!("../../../tests/golden/m4/canonical/trace_trap.json"),
        "trace_trap.json",
    );
}

#[test]
fn compat_version2_rejected() {
    let bytes = include_bytes!("../../../tests/golden/m4/compat/trace_version2.json");
    let err = parse_trace_envelope(bytes).expect_err("version 2 must be rejected");
    assert!(err.contains('2'), "error names the version: {err}");
}

#[test]
fn compat_missing_version_rejected() {
    let bytes = include_bytes!("../../../tests/golden/m4/compat/trace_missing_version.json");
    parse_trace_envelope(bytes).expect_err("missing version must be rejected");
}

#[test]
fn compat_unknown_extra_field_accepted() {
    let bytes = include_bytes!("../../../tests/golden/m4/compat/trace_extra_field.json");
    let p = parse_trace_envelope(bytes).expect("unknown fields must be ignored");
    let canonical = include_bytes!("../../../tests/golden/m4/canonical/trace_basic.json");
    assert_eq!(
        trace_canonical_bytes(&p.events, p.truncated, p.kept),
        canonical
    );
}
