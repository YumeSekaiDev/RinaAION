//! M4.3 `run_intent_json` driver routing tests.
//!
//! Shape, empty-on-missing-entry, and dirty-program routing to the
//! diagnostic envelope. Enabled-flags coverage lives in
//! `analyze_with_flags` tests only; `run_intent_json` itself uses
//! default flags.

use aion_driver::{analyze_with_flags, run_intent_json};
use aion_span::FileId;
use aion_syntax::FeatureFlags;

fn fid() -> FileId {
    FileId(0)
}

fn enabled() -> FeatureFlags {
    FeatureFlags::all_experimental()
}

#[test]
fn shape_clean_default_program() {
    // Default flags admit no metadata: entry present, intents omitted.
    let out = run_intent_json(fid(), b"fn main() { do 1; }");
    assert_eq!(
        out,
        "{\"schema\":\"aion.intent/v1\",\"version\":1,\"entry\":{\"name\":\"main\"}}\n"
    );
    assert!(out.ends_with('\n'));
    assert!(
        !out[..out.len() - 1].contains('\n'),
        "envelope must be a single line"
    );
}

#[test]
fn empty_on_missing_entry() {
    let out = run_intent_json(fid(), b"fn helper() { do 1; }");
    assert_eq!(out, "{\"schema\":\"aion.intent/v1\",\"version\":1}\n");
}

#[test]
fn dirty_parse_error_yields_diagnostic_envelope() {
    let out = run_intent_json(fid(), b"fn main( {");
    assert!(
        out.starts_with("{\"schema\":\"aion.diagnostic/v1\""),
        "parse failure must route to diagnostics: {out}"
    );
}

#[test]
fn dirty_sema_error_yields_diagnostic_envelope() {
    // Use-after-move is a sema failure on default flags too.
    let out = run_intent_json(fid(), b"fn main() { let s = \"hi\"; let t = s; do s; }");
    assert!(
        out.starts_with("{\"schema\":\"aion.diagnostic/v1\""),
        "sema failure must route to diagnostics: {out}"
    );
}

#[test]
fn invalid_goal_fixtures_are_blocking_e0011() {
    for name in ["unbound_body.aion", "unbound_block.aion"] {
        let path = format!("../../tests/fixtures/m4_3/invalid_goal/{name}");
        let src = std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let diags = analyze_with_flags(fid(), &src, enabled())
            .expect_err(&format!("{name} must fail analysis"));
        assert!(
            diags
                .iter()
                .any(|d| d.code == aion_error::ErrorCode::UnboundName),
            "{name} must carry E0011: {:?}",
            diags.iter().map(|d| &d.code).collect::<Vec<_>>()
        );
    }
}

#[test]
fn analyze_with_flags_accepts_metadata_program() {
    // Enabled flags: the metadata program analyzes clean and exposes
    // entry-anchored intents; default flags reject the same program.
    let src = b"intent \"m\" { goal : 1; }\nfn main() { goal : 1; do 1; }";
    let (parse, model) = analyze_with_flags(fid(), src, enabled()).expect("enabled must accept");
    let view = aion_query::view(&parse, &model);
    assert_eq!(view.entry_intents().len(), 2);
    assert!(
        aion_driver::analyze(fid(), src).is_err(),
        "gate-off must reject metadata"
    );
}

#[test]
fn run_intent_json_matches_query_canonical_bytes() {
    // The driver route composes the same canonical emitter the query
    // crate owns: byte-identical to building it directly.
    let src = b"fn main() { do 1; }";
    let out = run_intent_json(fid(), src);
    let (parse, model) = aion_driver::analyze(fid(), src).expect("clean");
    let view = aion_query::view(&parse, &model);
    let name = view.entry_point().ok().map(|h| h.name);
    let intents = view.entry_intents();
    let direct = String::from_utf8(aion_query::intent_canonical_bytes(
        name.as_deref(),
        &intents,
    ))
    .expect("envelope is UTF-8");
    assert_eq!(out, direct);
}
