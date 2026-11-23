//! M4.4 milestone M6a (T16): enabled-pipeline assertion integration.
//!
//! Composes existing public APIs only
//! (`parse_with_flags` → `check_with_model` → `lower` → `validate` →
//! `execute`); no new driver entry point. Covers E0010-before-runtime,
//! true elimination, dynamic true/false, folded false, E0016 halt, and
//! default-off gating.

use aion_driver::analyze_with_flags;
use aion_error::{AssertProvenance, ErrorCode, Severity};
use aion_span::FileId;
use aion_syntax::FeatureFlags;

fn fid() -> FileId {
    FileId(0)
}

fn flags() -> FeatureFlags {
    FeatureFlags::all_experimental()
}

fn fixture(rel: &str) -> Vec<u8> {
    let path = format!(
        "{}/../../tests/fixtures/m4_4/{rel}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// Full enabled pipeline: parse → sema → lower → validate.
/// Panics on any stage diagnostic; returns the validated module.
fn enabled_module(rel: &str) -> (String, aion_ir::ValidatedModule) {
    let src = fixture(rel);
    let text = String::from_utf8_lossy(&src).into_owned();
    let parse = aion_syntax::parse_with_flags(fid(), &src, flags());
    assert!(
        parse.errors.is_empty(),
        "{rel} must parse clean: {:?}",
        parse
            .errors
            .iter()
            .map(|d| d.render_human())
            .collect::<Vec<_>>()
    );
    let (sema_diags, model) = aion_sema::check_with_model(&parse, fid(), &src);
    assert!(
        sema_diags.is_empty(),
        "{rel} must check clean: {:?}",
        sema_diags
            .iter()
            .map(|d| d.render_human())
            .collect::<Vec<_>>()
    );
    let (module, lower_diags) = aion_ir::lower(&parse, &model, fid(), text.len() as u32);
    assert!(
        lower_diags.is_empty(),
        "{rel} must lower clean: {:?}",
        lower_diags
            .iter()
            .map(|d| d.render_human())
            .collect::<Vec<_>>()
    );
    let validated = aion_ir::validate(&module).unwrap_or_else(|ds| {
        panic!(
            "{rel} must validate clean: {:?}",
            ds.iter().map(|d| d.render_human()).collect::<Vec<_>>()
        )
    });
    (text, validated)
}

fn assert_stmt_span(text: &str) -> (u32, u32) {
    let start = text
        .find("assert")
        .unwrap_or_else(|| panic!("no assert in {text:?}")) as u32;
    let semi = text[start as usize..]
        .find(';')
        .unwrap_or_else(|| panic!("no `;` in {text:?}"));
    (start, start + semi as u32 + 1)
}

#[test]
fn e0010_rejects_non_bool_before_lowering_or_execution() {
    let src = fixture("invalid/e0010_non_bool.aion");
    let err = analyze_with_flags(fid(), &src, flags()).expect_err("E0010 must fail analysis");
    assert_eq!(err.len(), 1, "exactly one blocking diagnostic");
    let d = &err[0];
    assert_eq!(d.code, ErrorCode::TypeMismatch);
    assert_eq!(d.code.code_str(), "AION-E0010");
    assert_eq!(d.severity, Severity::Error);
    assert_eq!(d.provenance, Some(AssertProvenance::human_authored()));
    // `analyze` returns before lowering on sema errors: no model, no
    // IR, no execution is produced — the Err itself is the halt proof.
}

#[test]
fn true_assertion_eliminates_through_pipeline() {
    let (text, validated) = enabled_module("valid/assert_true_elim.aion");
    assert_eq!(text, "fn main() { assert true; }\n");
    // Elimination proof: no assertion node of either kind survives
    // lowering — the statement is gone, not merely evaluated.
    assert!(
        validated.module().functions.iter().all(|f| {
            !f.body.stmts.iter().any(|s| {
                matches!(
                    s,
                    aion_ir::TypedStmt::Assert { .. } | aion_ir::TypedStmt::AssertFalse { .. }
                )
            })
        }),
        "assert true must leave no lowered assertion node"
    );
    let r = aion_run::execute(&validated, &aion_run::Config::default());
    match r {
        aion_run::RunResult::Ok(o) => assert_eq!(o.value, aion_run::Value::Unit),
        aion_run::RunResult::Trap(d) => panic!("unexpected trap: {}", d.render_human()),
    }
}

#[test]
fn dynamic_true_assertion_succeeds_through_pipeline() {
    let (_text, validated) = enabled_module("valid/assert_dynamic_true.aion");
    match aion_run::execute(&validated, &aion_run::Config::default()) {
        aion_run::RunResult::Ok(o) => assert_eq!(o.value, aion_run::Value::Unit),
        aion_run::RunResult::Trap(d) => panic!("unexpected trap: {}", d.render_human()),
    }
}

#[test]
fn dynamic_false_assertion_traps_e0044() {
    let (text, validated) = enabled_module("trap/assert_dynamic_false.aion");
    let d = match aion_run::execute(&validated, &aion_run::Config::default()) {
        aion_run::RunResult::Trap(d) => d,
        aion_run::RunResult::Ok(o) => panic!("expected E0044 trap, got {o:?}"),
    };
    assert_eq!(d.code, ErrorCode::RuntimeTrap);
    assert_eq!(d.code.code_str(), "AION-E0044");
    assert_eq!(d.message, "assertion failed");
    assert_eq!(d.severity, Severity::Error);
    assert!(d.notes.is_empty() && d.related.is_empty());
    let (start, end) = assert_stmt_span(&text);
    assert_eq!(
        d.span.map(|sp| (sp.start, sp.end)),
        Some((start, end)),
        "E0044 must carry the assert statement span"
    );
    assert_eq!(d.provenance, Some(AssertProvenance::human_authored()));
}

#[test]
fn folded_false_assertion_traps_without_evaluation_or_fuel() {
    let (text, validated) = enabled_module("trap/assert_folded_false.aion");
    // Zero fuel: any operand evaluation or fuel consultation would
    // trap FuelExhausted instead of E0044.
    let cfg = aion_run::Config {
        fuel: 0,
        frames: 1024,
    };
    let d = match aion_run::execute(&validated, &cfg) {
        aion_run::RunResult::Trap(d) => d,
        aion_run::RunResult::Ok(o) => panic!("expected E0044 trap, got {o:?}"),
    };
    assert_eq!(d.code, ErrorCode::RuntimeTrap);
    assert_eq!(d.code.code_str(), "AION-E0044");
    let (start, end) = assert_stmt_span(&text);
    assert_eq!(
        d.span.map(|sp| (sp.start, sp.end)),
        Some((start, end)),
        "folded E0044 must carry the assert statement span"
    );
    assert_eq!(d.provenance, Some(AssertProvenance::human_authored()));
}

#[test]
fn e0016_blocks_pipeline_with_no_downstream_stages() {
    let src = fixture("invalid/e0016_intent_assert.aion");
    let err = analyze_with_flags(fid(), &src, flags()).expect_err("E0016 must fail analysis");
    assert_eq!(err.len(), 1, "exactly one blocking diagnostic");
    let d = &err[0];
    assert_eq!(d.code, ErrorCode::AssertInIntent);
    assert_eq!(d.code.code_str(), "AION-E0016");
    assert_eq!(
        d.message,
        "assert statements are not allowed in intent blocks"
    );
    assert_eq!(d.severity, Severity::Error);
    assert_eq!(d.provenance, Some(AssertProvenance::human_authored()));
    // `analyze` halts on sema diagnostics: no lowering, validation,
    // or execution artifacts are produced — the Err itself proves
    // downstream stages never ran.
}

#[test]
fn assertions_remain_gated_off_by_default() {
    // Default-flags analysis rejects gated syntax at parse time;
    // sema never runs, so no assert-specific diagnostics can appear.
    for rel in [
        "valid/assert_true_elim.aion",
        "valid/assert_dynamic_true.aion",
        "invalid/e0016_intent_assert.aion",
    ] {
        let src = fixture(rel);
        let err =
            aion_driver::analyze(fid(), &src).expect_err("gated syntax must fail default analysis");
        assert!(!err.is_empty(), "{rel} must yield parse diagnostics");
        assert!(
            err.iter().any(|d| d.code == ErrorCode::UnexpectedToken),
            "{rel} must fail at the gate: {:?}",
            err.iter().map(|d| d.render_human()).collect::<Vec<_>>()
        );
        assert!(
            err.iter()
                .all(|d| d.code != ErrorCode::TypeMismatch && d.code != ErrorCode::AssertInIntent),
            "{rel}: no sema assert diagnostics under default flags"
        );
    }
}
