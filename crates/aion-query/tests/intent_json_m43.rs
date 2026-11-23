//! M4.3 `aion.intent/v1` envelope tests (Suite B).
//!
//! No shared helpers with the query suite (`query_m43.rs`); every
//! helper below is local to this file.

use aion_span::FileId;
use aion_syntax::FeatureFlags;

fn fid() -> FileId {
    FileId(0)
}

/// Enabled-flags parse + clean sema, returning owned artifacts.
fn build(src: &str) -> (aion_syntax::ParseResult, aion_sem_model::SemModel) {
    aion_driver::analyze_with_flags(fid(), src.as_bytes(), FeatureFlags::all_experimental())
        .unwrap_or_else(|diags| {
            panic!(
                "analyze must be clean for {src:?}: {:?}",
                diags
                    .iter()
                    .map(|d| (&d.code, &d.message))
                    .collect::<Vec<_>>()
            )
        })
}

/// Canonical envelope bytes for `src` (entry-anchored intents).
fn envelope(src: &str) -> Vec<u8> {
    let (parse, model) = build(src);
    let view = aion_query::view(&parse, &model);
    let name = view.entry_point().ok().map(|h| h.name);
    let intents = view.entry_intents();
    aion_query::intent_canonical_bytes(name.as_deref(), &intents)
}

const RICH: &str = "intent \"h\" { goal : helper(1); }\n\
                    fn helper(a) { goal : a * 2; do a + 1; }\n\
                    fn main() { do helper(1); }\n";

#[test]
fn canonical_round_trip_is_byte_identical() {
    let bytes = envelope(RICH);
    let parsed = aion_query::parse_intent_envelope(&bytes).expect("canonical bytes must parse");
    let again = aion_query::intent_canonical_bytes(parsed.entry.as_deref(), &parsed.intents);
    assert_eq!(again, bytes, "re-emit must be byte-identical");
    // And once more, to prove fixpoint.
    let parsed2 = aion_query::parse_intent_envelope(&again).expect("re-emit must parse");
    let third = aion_query::intent_canonical_bytes(parsed2.entry.as_deref(), &parsed2.intents);
    assert_eq!(third, bytes);
}

#[test]
fn version2_is_rejected_naming_version() {
    let bytes = envelope(RICH);
    let text = std::str::from_utf8(&bytes).unwrap();
    let v2 = text.replacen("\"version\":1", "\"version\":2", 1);
    let err = aion_query::parse_intent_envelope(v2.as_bytes()).expect_err("v2 must reject");
    assert!(err.contains('2'), "rejection must name the version: {err}");
}

#[test]
fn missing_version_is_rejected() {
    let bytes = envelope(RICH);
    let text = std::str::from_utf8(&bytes).unwrap();
    let stripped = text.replacen(",\"version\":1", "", 1);
    assert_ne!(stripped, text);
    let err = aion_query::parse_intent_envelope(stripped.as_bytes())
        .expect_err("missing version must reject");
    assert!(err.contains("version"), "{err}");
}

#[test]
fn extra_field_accepted_and_reemitted_canonical() {
    let bytes = envelope(RICH);
    let text = std::str::from_utf8(&bytes).unwrap();
    // Splice an unknown field after the schema key.
    let extra = text.replacen(
        "\"schema\":\"aion.intent/v1\"",
        "\"schema\":\"aion.intent/v1\",\"zzz_new\":[1,{\"q\":\"x\"}]",
        1,
    );
    assert_ne!(extra, text);
    let parsed = aion_query::parse_intent_envelope(extra.as_bytes())
        .expect("unknown fields must be ignored");
    let again = aion_query::intent_canonical_bytes(parsed.entry.as_deref(), &parsed.intents);
    assert_eq!(again, bytes, "re-emit must drop the unknown field");
}

#[test]
fn synthetic_unbound_ref_emits_decl_null() {
    // Synthetic view (schema-level check): an unresolved ref carries
    // `decl: null` plus the `AION-E0011` diagnostic marker.
    let views = vec![aion_query::IntentView {
        owner: "main".to_string(),
        owner_span: None,
        span: None,
        desc: String::new(),
        goals: vec![aion_query::GoalView {
            expr: aion_syntax::Expr::Ident("missing".to_string()),
            span: None,
            refs: vec![aion_query::GoalRefView {
                name: "missing".to_string(),
                bound: false,
                occurrence: Some(aion_span::ByteSpan::new(FileId(0), 0, 7).unwrap()),
                decl_name: None,
                decl_span: None,
            }],
        }],
    }];
    let bytes = aion_query::intent_canonical_bytes(Some("main"), &views);
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(text.contains("\"status\":\"unresolved\""), "{text}");
    assert!(text.contains("\"decl\":null"), "{text}");
    assert!(text.contains("\"diagnostic\":\"AION-E0011\""), "{text}");
    // Omit-when-empty: empty desc and missing spans are dropped.
    assert!(!text.contains("\"desc\""), "{text}");
    assert!(!text.contains("\"goal_span\""), "{text}");
}

#[test]
fn no_new_diagnostic_codes_beyond_e0011() {
    // The only diagnostic code the intent envelope may name is
    // `AION-E0011`; scan a bound+unbound mix for any other code.
    let views = vec![aion_query::IntentView {
        owner: "main".to_string(),
        owner_span: Some(aion_span::ByteSpan::new(FileId(0), 0, 10).unwrap()),
        span: Some(aion_span::ByteSpan::new(FileId(0), 11, 20).unwrap()),
        desc: "d".to_string(),
        goals: vec![aion_query::GoalView {
            expr: aion_syntax::Expr::Binary {
                op: aion_syntax::BinOp::Add,
                lhs: Box::new(aion_syntax::Expr::Ident("x".to_string())),
                rhs: Box::new(aion_syntax::Expr::Ident("missing".to_string())),
            },
            span: Some(aion_span::ByteSpan::new(FileId(0), 21, 30).unwrap()),
            refs: vec![
                aion_query::GoalRefView {
                    name: "x".to_string(),
                    bound: true,
                    occurrence: Some(aion_span::ByteSpan::new(FileId(0), 21, 22).unwrap()),
                    decl_name: Some("x".to_string()),
                    decl_span: Some(aion_span::ByteSpan::new(FileId(0), 4, 5).unwrap()),
                },
                aion_query::GoalRefView {
                    name: "missing".to_string(),
                    bound: false,
                    occurrence: Some(aion_span::ByteSpan::new(FileId(0), 25, 32).unwrap()),
                    decl_name: None,
                    decl_span: None,
                },
            ],
        }],
    }];
    let bytes = aion_query::intent_canonical_bytes(Some("main"), &views);
    let text = std::str::from_utf8(&bytes).unwrap();
    let mut rest = text;
    let mut codes = Vec::new();
    while let Some(i) = rest.find("\"diagnostic\":\"") {
        let tail = &rest[i + 14..];
        let end = tail.find('"').expect("terminated code");
        codes.push(tail[..end].to_string());
        rest = &tail[end..];
    }
    assert!(!codes.is_empty());
    assert!(
        codes.iter().all(|c| c == "AION-E0011"),
        "only AION-E0011 may appear, got {codes:?}"
    );
}

#[test]
fn blocking_e0011_dirty_program_yields_diagnostic_via_driver() {
    // An unbound goal name blocks sema under enabled flags ...
    let src = "fn main() { goal : missing; do 1; }";
    let diags = aion_driver::analyze_with_flags(
        fid(),
        src.as_bytes(),
        aion_syntax::FeatureFlags::all_experimental(),
    )
    .expect_err("unbound goal must block analysis");
    assert!(
        diags
            .iter()
            .any(|d| d.code == aion_error::ErrorCode::UnboundName),
        "goal E0011 must block: {:?}",
        diags.iter().map(|d| &d.code).collect::<Vec<_>>()
    );
    // ... and the default-flags driver route yields the diagnostic
    // envelope (never an intent envelope) for the dirty program.
    let out = aion_driver::run_intent_json(fid(), src.as_bytes());
    assert!(
        out.starts_with("{\"schema\":\"aion.diagnostic/v1\""),
        "dirty program must route to diagnostics: {out}"
    );
}

#[test]
fn entry_omitted_when_no_entry() {
    // `run_intent_json` uses default flags: a main-less program is
    // clean, so the result is the empty intent envelope.
    let out = aion_driver::run_intent_json(fid(), b"fn helper() { do 1; }");
    assert_eq!(out, "{\"schema\":\"aion.intent/v1\",\"version\":1}\n");
}

#[test]
fn goldens_match_canonical_and_compat_triplet() {
    let read = |name: &str| {
        let path = format!("../../tests/golden/m4/intent/{name}");
        std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
    };
    let bytes = envelope(RICH);
    assert_eq!(bytes, read("canonical.json"));
    // Compat triplet: version2 and missing-version reject.
    assert!(aion_query::parse_intent_envelope(&read("intent_version2.json")).is_err());
    assert!(aion_query::parse_intent_envelope(&read("intent_missing_version.json")).is_err());
    // Extra-field golden parses and re-emits to canonical.
    let extra = read("intent_extra_field.json");
    let parsed = aion_query::parse_intent_envelope(&extra).expect("extra field must parse");
    let again = aion_query::intent_canonical_bytes(parsed.entry.as_deref(), &parsed.intents);
    assert_eq!(again, bytes);
}
