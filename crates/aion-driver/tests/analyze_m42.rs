//! M4.2 `analyze` entry-point tests.

use aion_driver::analyze;
use aion_sem_model::Ty;
use aion_span::FileId;

fn fid() -> FileId {
    FileId(0)
}

#[test]
fn valid_move_program_yields_populated_artifacts() {
    let src = b"fn main() { let s = \"hi\"; let t = s; do t; }";
    let (parse, model) = analyze(fid(), src).expect("valid program must analyze clean");
    assert!(parse.errors.is_empty());
    assert!(!model.items.is_empty(), "items must be populated");
    assert!(
        model.items.iter().any(|i| i.name == "main"),
        "items must contain main: {:?}",
        model.items
    );
    assert!(!model.types.is_empty(), "types must be populated");
    assert!(
        model.types.values().any(|t| *t == Ty::Str),
        "a string-typed node is expected: {:?}",
        model.types.values().collect::<Vec<_>>()
    );
    assert!(
        !model.moved_uses.is_empty(),
        "the `let t = s` move must be recorded"
    );
}

#[test]
fn parse_error_yields_parse_diags_without_sema() {
    let src = b"fn main( {";
    let err = analyze(fid(), src).expect_err("parse error must fail");
    assert!(!err.is_empty());
    // `check` returns empty on parse failure (sema never runs); analyze
    // must surface exactly the parse diagnostics.
    assert_eq!(
        format!("{err:?}"),
        format!("{:?}", aion_syntax::parse(fid(), src).errors)
    );
}

#[test]
fn sema_error_yields_sema_diags() {
    let src = b"fn main() { let s = \"hi\"; let t = s; do s; }";
    let err = analyze(fid(), src).expect_err("use-after-move must fail");
    assert!(!err.is_empty());
    let sema_diags = aion_sema::check(&aion_syntax::parse(fid(), src), fid(), src);
    assert_eq!(format!("{err:?}"), format!("{sema_diags:?}"));
}

#[test]
fn analyze_is_deterministic() {
    let src = b"fn main() { let s = \"hi\"; let t = s; do t; }";
    let (_, m1) = analyze(fid(), src).expect("must analyze clean");
    let (_, m2) = analyze(fid(), src).expect("must analyze clean");
    // Canonical projection: every HashMap-backed collection is sorted
    // before rendering, so the comparison is order-stable. Never compare
    // `Debug` output of `SemModel` directly: `types` and scope
    // `symbols` are HashMaps whose Debug order varies run to run.
    fn canon(m: &aion_sem_model::SemModel) -> String {
        let mut types: Vec<_> = m.types.iter().collect();
        types.sort_by_key(|(k, _)| k.0);
        let mut moved: Vec<_> = m.moved_uses.iter().collect();
        moved.sort_by_key(|k| k.0);
        let mut scopes: Vec<String> = m
            .scope_tree
            .scopes
            .iter()
            .map(|s| {
                let mut syms: Vec<_> = s.symbols.values().collect();
                syms.sort_by(|a, b| a.name.cmp(&b.name));
                format!("{:?}{syms:?}", s.parent)
            })
            .collect();
        // Scope insertion order is deterministic, but normalize anyway.
        scopes.sort();
        format!("{:?}{types:?}{moved:?}{scopes:?}", m.items)
    }
    assert_eq!(canon(&m1), canon(&m2));
}

#[test]
fn e0014_multi_var_diags_in_deterministic_span_order() {
    // Two unconstrained params in different spans: residual Vars must
    // surface as E0014 diagnostics in span order, identically on every
    // run (regression test for HashMap-iteration-ordered gate output).
    let src = b"fn id(x, y) { do x; do y; }";
    let parse = aion_syntax::parse(fid(), src);
    assert!(parse.errors.is_empty());
    let (sema_diags, model) = aion_sema::check_with_model(&parse, fid(), src);
    assert!(sema_diags.is_empty());
    let canon_once = |diags: &[aion_error::Diagnostic]| -> String {
        diags
            .iter()
            .map(|d| {
                let (s, e) = d
                    .span
                    .map(|sp| (sp.start, sp.end))
                    .unwrap_or((u32::MAX, u32::MAX));
                format!("{}|{s:010}|{e:010}|{}", d.code.code_str(), d.message)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut first: Option<String> = None;
    for _ in 0..10 {
        let (_, ldiags) = aion_ir::lower(&parse, &model, fid(), src.len() as u32);
        assert!(ldiags.len() >= 2, "two residual Vars expected: {ldiags:?}");
        assert!(ldiags
            .iter()
            .all(|d| d.code == aion_error::ErrorCode::UnresolvedType));
        // Span order: diagnostic spans must be non-decreasing by start.
        let starts: Vec<u32> = ldiags
            .iter()
            .map(|d| d.span.map(|sp| sp.start).unwrap_or(u32::MAX))
            .collect();
        let mut sorted = starts.clone();
        sorted.sort();
        assert_eq!(starts, sorted, "E0014 diags must be in span order");
        let c = canon_once(&ldiags);
        if let Some(prev) = &first {
            assert_eq!(&c, prev, "E0014 output must be identical across runs");
        } else {
            first = Some(c);
        }
    }
}
