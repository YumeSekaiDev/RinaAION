//! Driver stub (M0 only + M1 formatter passthrough): runs total lex +
//! stub parse, and exposes the pure M1 formatter.
//!
//! M3.6 pipeline driver: [`run`] orchestrates parse → sema → lower →
//! validate → execute, halting at the first stage that reports
//! diagnostics; [`dump_ir`] lowers a clean program for `--dump-ir`/tests.

pub use aion_run::{Config, Outcome, RunResult, Value};

/// Pipeline outcome. Value types stay in `aion-run` (re-exported above);
/// nothing is redefined here.
#[derive(Debug, Clone)]
pub enum RunOutcome {
    /// Execution succeeded (program value is always `Unit`; see `aion-run`).
    Ok(Outcome),
    /// A pre-execution stage failed; diagnostics in stage order.
    Diags(Vec<aion_error::Diagnostic>),
    /// Execution trapped (entry, runtime, or resource diagnostic).
    Trap(aion_error::Diagnostic),
}

pub fn compile(file: aion_span::FileId, bytes: &[u8]) -> aion_syntax::ParseResult {
    let mut r = aion_syntax::parse(file, bytes);
    if r.errors.is_empty() {
        // M2: merge semantic diagnostics into the parse result.
        r.errors.extend(aion_sema::check(&r, file, bytes));
    }
    r
}

/// M4.2 analysis entry point: single parse, then sema with model iff
/// parsing is clean. Returns full artifacts on success; diagnostics in
/// stage order (parse errors first, else sema diags) on failure.
/// No execution; `compile`/`check`/`run`/`format`/`dump_ir` untouched.
pub fn analyze(
    file: aion_span::FileId,
    bytes: &[u8],
) -> Result<(aion_syntax::ParseResult, aion_sem_model::SemModel), Vec<aion_error::Diagnostic>> {
    let parse = aion_syntax::parse(file, bytes);
    if !parse.errors.is_empty() {
        return Err(parse.errors);
    }
    let (sema_diags, model) = aion_sema::check_with_model(&parse, file, bytes);
    if !sema_diags.is_empty() {
        return Err(sema_diags);
    }
    Ok((parse, model))
}

/// M2 semantic check: parse, then typecheck only when parsing is clean.
pub fn check(file: aion_span::FileId, bytes: &[u8]) -> Vec<aion_error::Diagnostic> {
    let r = aion_syntax::parse(file, bytes);
    if r.errors.is_empty() {
        aion_sema::check(&r, file, bytes)
    } else {
        Vec::new()
    }
}

/// M1 formatter entry point: pure, total, idempotent; delegates to
/// `aion_syntax::format`.
pub fn format(file: aion_span::FileId, bytes: &[u8]) -> Vec<u8> {
    aion_syntax::format(file, bytes)
}

/// M3.6 end-to-end pipeline: parse → sema → lower (E0014 stops) →
/// validate (E0030 stops) → execute (E0015 entry check lives in
/// `aion-run`; it is called, not duplicated).
///
/// Halts on the first diagnostic vector, merging diagnostics in stage
/// order. Total: never panics.
pub fn run(file: aion_span::FileId, bytes: &[u8], config: &aion_run::Config) -> RunOutcome {
    // Stage 1: parse.
    let parse = aion_syntax::parse(file, bytes);
    if !parse.errors.is_empty() {
        return RunOutcome::Diags(parse.errors);
    }
    // Stage 2: sema (parse is clean, so `check` runs fully).
    let (sema_diags, model) = aion_sema::check_with_model(&parse, file, bytes);
    if !sema_diags.is_empty() {
        return RunOutcome::Diags(sema_diags);
    }
    // Stage 3: lower (E0014 / index-mismatch stops).
    let (module, lower_diags) = aion_ir::lower(&parse, &model, file, bytes.len() as u32);
    if !lower_diags.is_empty() {
        return RunOutcome::Diags(lower_diags);
    }
    // Stage 4: validate (E0030 stops, incl. duplicate `main`s). Only
    // the validated module crosses into execution (compile-time gate).
    let module = match aion_ir::validate(&module) {
        Ok(v) => v,
        Err(validate_diags) => return RunOutcome::Diags(validate_diags),
    };
    // Stage 5: execute (entry E0015 + runtime traps via `aion-run`).
    match aion_run::execute(&module, config) {
        RunResult::Ok(outcome) => RunOutcome::Ok(outcome),
        RunResult::Trap(d) => RunOutcome::Trap(d),
    }
}

/// Pure helper for tests/`--dump-ir`: lower a clean program and print
/// its `Debug` shape. On any stage error, returns the diagnostics
/// rendering (one per line); never panics.
pub fn dump_ir(file: aion_span::FileId, bytes: &[u8]) -> String {
    let parse = aion_syntax::parse(file, bytes);
    if !parse.errors.is_empty() {
        return render_diags(&parse.errors);
    }
    let (sema_diags, model) = aion_sema::check_with_model(&parse, file, bytes);
    if !sema_diags.is_empty() {
        return render_diags(&sema_diags);
    }
    let (module, lower_diags) = aion_ir::lower(&parse, &model, file, bytes.len() as u32);
    if !lower_diags.is_empty() {
        return render_diags(&lower_diags);
    }
    let module = match aion_ir::validate(&module) {
        Ok(v) => v,
        Err(validate_diags) => return render_diags(&validate_diags),
    };
    format!("{module:#?}")
}

/// M4.2 JSON composition: run the pipeline and render the outcome as a
/// canonical diagnostic envelope (`aion.diagnostic/v1`, LF-terminated).
/// `Ok` yields the empty-diagnostics envelope; `Diags`/`Trap` yield the
/// envelope over the stage diagnostics (trap = single-diagnostic list).
/// Thin composition only: delegates to [`run`] with a default config, so
/// execute/run semantics are unchanged and no new pipeline stages exist.
pub fn run_diagnostics_json(file: aion_span::FileId, bytes: &[u8]) -> String {
    let diags: Vec<aion_error::Diagnostic> = match run(file, bytes, &Config::default()) {
        RunOutcome::Ok(_) => Vec::new(),
        RunOutcome::Diags(ds) => ds,
        RunOutcome::Trap(d) => vec![d],
    };
    String::from_utf8(aion_error::Diagnostic::canonical_envelope_bytes(&diags))
        .expect("canonical envelope is UTF-8")
}

/// M4.2 JSON composition: run the pipeline and render the outcome as a
/// canonical trace envelope (`aion.trace/v1`, LF-terminated) on success.
/// `Diags` yields the diagnostic envelope over the stage diagnostics;
/// `Trap` yields the diagnostic envelope over the single trap diagnostic
/// (traps have no trace to emit).
///
/// NOTE: the recording observer (`aion_run`'s `with_observer`) is
/// crate-private to `aion-run`, so an external driver cannot enable it;
/// the `Ok` envelope is therefore built with the public canonical emitter
/// (`aion_run::trace_canonical_bytes`) over the events the public API
/// exposes (currently none → empty `events`, `kept: 0`, untruncated).
/// Once `aion-run` exposes a public traced-execute entry point, only this
/// function's `Ok` arm needs to change; pipeline stages stay identical.
pub fn run_trace_json(file: aion_span::FileId, bytes: &[u8], config: &aion_run::Config) -> String {
    // Same stage order as `run` (parse → sema → lower → validate →
    // execute); no new stages, no semantic drift (single evaluation).
    let parse = aion_syntax::parse(file, bytes);
    if !parse.errors.is_empty() {
        return diag_envelope(&parse.errors);
    }
    let (sema_diags, model) = aion_sema::check_with_model(&parse, file, bytes);
    if !sema_diags.is_empty() {
        return diag_envelope(&sema_diags);
    }
    let (module, lower_diags) = aion_ir::lower(&parse, &model, file, bytes.len() as u32);
    if !lower_diags.is_empty() {
        return diag_envelope(&lower_diags);
    }
    let module = match aion_ir::validate(&module) {
        Ok(v) => v,
        Err(validate_diags) => return diag_envelope(&validate_diags),
    };
    match aion_run::execute(&module, config) {
        RunResult::Ok(_) => String::from_utf8(aion_run::trace_canonical_bytes(&[], false, 0))
            .expect("canonical trace envelope is UTF-8"),
        RunResult::Trap(d) => diag_envelope(std::slice::from_ref(&d)),
    }
}

fn diag_envelope(diags: &[aion_error::Diagnostic]) -> String {
    String::from_utf8(aion_error::Diagnostic::canonical_envelope_bytes(diags))
        .expect("canonical envelope is UTF-8")
}

fn render_diags(diags: &[aion_error::Diagnostic]) -> String {
    diags
        .iter()
        .map(aion_error::Diagnostic::render_human)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_span::FileId;

    fn fid() -> FileId {
        FileId(0)
    }

    #[test]
    fn halt_order_sema_invalid_never_lowers() {
        // M2 move-error fixture: sema must reject before lowering runs.
        let src = b"fn main() { let s = \"hi\"; let t = s; do s; }";
        match run(fid(), src, &Config::default()) {
            RunOutcome::Diags(ds) => {
                assert!(!ds.is_empty());
                assert!(
                    ds.iter()
                        .all(|d| d.code != aion_error::ErrorCode::UnresolvedType),
                    "must halt at sema, never reach lower: {ds:?}"
                );
            }
            other => panic!("expected sema Diags halt, got {other:?}"),
        }
        // dump_ir on the same input renders diagnostics, never IR/panic.
        let dump = dump_ir(fid(), src);
        assert!(dump.contains("error"), "dump must render diags: {dump}");
        assert!(
            !dump.contains("TypedModule"),
            "dump must not print IR: {dump}"
        );
    }

    #[test]
    fn entry_missing_is_e0015() {
        let src = b"fn helper() { do 1; }";
        match run(fid(), src, &Config::default()) {
            RunOutcome::Trap(d) => {
                assert_eq!(d.code, aion_error::ErrorCode::EntryPointInvalid, "{d:?}");
                assert_eq!(
                    d.span,
                    Some(aion_span::ByteSpan::empty(fid(), src.len() as u32)),
                    "missing main must pin end of input, got {:?}",
                    d.span
                );
            }
            other => panic!("expected E0015 trap, got {other:?}"),
        }
    }

    #[test]
    fn entry_bad_signature_pins_decl_span() {
        // Header `fn main(x)`; the E0015 trap must point there, not at
        // the body or the end of input. (`Str` grounds the parameter;
        // an `Int`-constrained parameter trips the pre-existing E0014
        // residual-`Var` gate before entry is reached.)
        let src = b"fn main(x) { do x == \"a\"; do x; }";
        match run(fid(), src, &Config::default()) {
            RunOutcome::Trap(d) => {
                assert_eq!(d.code, aion_error::ErrorCode::EntryPointInvalid, "{d:?}");
                let sp = d.span.expect("E0015 must carry a span");
                assert_eq!(&src[sp.start as usize..sp.end as usize], b"fn main(x)");
            }
            other => panic!("expected E0015 trap, got {other:?}"),
        }
    }

    #[test]
    fn end_to_end_valid_program() {
        let src = b"fn main() { let x = 1 + 2; do x; }";
        match run(fid(), src, &Config::default()) {
            RunOutcome::Ok(outcome) => {
                assert_eq!(outcome.value, Value::Unit);
            }
            other => panic!("expected Ok(Unit), got {other:?}"),
        }
    }

    #[test]
    fn dump_ir_smoke_clean_program() {
        let dump = dump_ir(fid(), b"fn main() { do 1; }");
        assert!(dump.contains("main"), "dump must name main fn: {dump}");
    }
}
