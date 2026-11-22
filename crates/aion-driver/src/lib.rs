//! Driver stub (M0 only + M1 formatter passthrough): runs total lex +
//! stub parse, and exposes the pure M1 formatter.
pub fn compile(file: aion_span::FileId, bytes: &[u8]) -> aion_syntax::ParseResult {
    let mut r = aion_syntax::parse(file, bytes);
    if r.errors.is_empty() {
        // M2: merge semantic diagnostics into the parse result.
        r.errors.extend(aion_sema::check(&r, file, bytes));
    }
    r
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
