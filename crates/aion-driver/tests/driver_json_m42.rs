//! M4.2 driver JSON composition tests: goldens + version-reject coverage.

use aion_driver::{run_diagnostics_json, run_trace_json, Config};
use aion_span::FileId;

fn fid() -> FileId {
    FileId(0)
}

fn golden(name: &str) -> String {
    let path = format!("../../tests/golden/m4/driver/{name}");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

#[test]
fn diag_json_empty_on_ok() {
    let out = run_diagnostics_json(fid(), b"fn main() { do 1; }");
    assert_eq!(out, golden("diag_empty.json"));
}

#[test]
fn diag_json_sema_diags() {
    let src = b"fn main() { let s = \"hi\"; let t = s; do s; }";
    let out = run_diagnostics_json(fid(), src);
    assert_eq!(out, golden("diag_sema.json"));
}

#[test]
fn diag_json_trap() {
    let out = run_diagnostics_json(fid(), b"fn helper() { do 1; }");
    assert_eq!(out, golden("diag_trap.json"));
}

#[test]
fn trace_json_ok_envelope() {
    let out = run_trace_json(fid(), b"fn main() { do 1; }", &Config::default());
    assert_eq!(out, golden("trace_ok.json"));
}

#[test]
fn trace_json_diags_falls_back_to_diag_envelope() {
    let src = b"fn main() { let s = \"hi\"; let t = s; do s; }";
    let out = run_trace_json(fid(), src, &Config::default());
    assert_eq!(out, run_diagnostics_json(fid(), src));
}

#[test]
fn trace_json_trap_falls_back_to_diag_envelope() {
    let src = b"fn helper() { do 1; }";
    let out = run_trace_json(fid(), src, &Config::default());
    assert_eq!(out, run_diagnostics_json(fid(), src));
    // The trap diagnostic round-trips through the diagnostic reader.
    let diags = aion_error::parse_envelope(out.as_bytes()).expect("trap envelope must parse");
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].code, aion_error::ErrorCode::EntryPointInvalid);
}

#[test]
fn diag_envelope_version_rejected() {
    let ok = run_diagnostics_json(fid(), b"fn main() { do 1; }");
    // Round-trip: the emitted envelope parses.
    let diags = aion_error::parse_envelope(ok.as_bytes()).expect("must parse");
    assert!(diags.is_empty());
    // Wrong major version rejected.
    let bad = ok.replacen("\"version\":1", "\"version\":2", 1);
    assert!(aion_error::parse_envelope(bad.as_bytes()).is_err());
    // Missing version rejected.
    let missing = ok.replacen("\"version\":1,", "", 1);
    assert!(aion_error::parse_envelope(missing.as_bytes()).is_err());
}

#[test]
fn trace_envelope_version_rejected() {
    let ok = run_trace_json(fid(), b"fn main() { do 1; }", &Config::default());
    let p = aion_run::parse_trace_envelope(ok.as_bytes()).expect("must parse");
    assert!(p.events.is_empty() && !p.truncated && p.kept == 0);
    let bad = ok.replacen("\"version\":1", "\"version\":2", 1);
    assert!(aion_run::parse_trace_envelope(bad.as_bytes()).is_err());
    let missing = ok.replacen("\"version\":1,", "", 1);
    assert!(aion_run::parse_trace_envelope(missing.as_bytes()).is_err());
}
