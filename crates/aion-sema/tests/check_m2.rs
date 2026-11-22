//! M2 sema corpus: valid fixtures check clean; invalid fixtures yield
//! exactly one diagnostic of the specified code at the anchor span.

use aion_error::ErrorCode;
use aion_span::FileId;

fn load(rel: &str) -> Vec<u8> {
    let path = format!(
        "{}/../../tests/fixtures/m2/{rel}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

fn sema(src: &[u8]) -> (String, Vec<aion_error::Diagnostic>) {
    let s = String::from_utf8_lossy(src).into_owned();
    let r = aion_syntax::parse(FileId(0), src);
    assert!(r.errors.is_empty(), "fixture must parse clean: {s}");
    let ds = aion_sema::check(&r, FileId(0), src);
    (s, ds)
}

fn span_of(src: &str, anchor: &str, nth: usize) -> (u32, u32) {
    let mut from = 0usize;
    let mut pos = 0usize;
    for _ in 0..=nth {
        pos = src[from..]
            .find(anchor)
            .unwrap_or_else(|| panic!("anchor {anchor:?} missing in {src:?}"))
            + from;
        from = pos + 1;
    }
    (pos as u32, (pos + anchor.len()) as u32)
}

// ---- valid fixtures must check clean ----

const VALID: &[&str] = &[
    "valid/inference.aion",
    "valid/precedence.aion",
    "valid/shadowing.aion",
    "valid/nested_scopes.aion",
    "valid/multi_fn.aion",
    "valid/return_position.aion",
    "valid/return_str_local.aion",
    "valid/return_str_param.aion",
];

#[test]
fn m2_valid_fixtures_check_clean() {
    for rel in VALID {
        let src = load(rel);
        let (text, ds) = sema(&src);
        assert!(
            ds.is_empty(),
            "{rel} must check clean, got: {:?} in {text:?}",
            ds.iter().map(|d| d.render_human()).collect::<Vec<_>>()
        );
    }
}

// ---- invalid fixtures: one code each, exact span ----

#[test]
fn m2_invalid_e0010_span() {
    let src = load("invalid/e0010_typemismatch.aion");
    let (text, ds) = sema(&src);
    assert_eq!(ds.len(), 1, "{text:?}: {ds:?}");
    let d = &ds[0];
    assert_eq!(d.code, ErrorCode::TypeMismatch);
    let (s, e) = span_of(&text, "1 + \"x\"", 0);
    assert_eq!(
        (d.span.unwrap().start, d.span.unwrap().end),
        (s, e),
        "{}",
        d.render_human()
    );
}

#[test]
fn m2_invalid_e0011_span() {
    let src = load("invalid/e0011_unbound.aion");
    let (text, ds) = sema(&src);
    assert_eq!(ds.len(), 1, "{text:?}: {ds:?}");
    let d = &ds[0];
    assert_eq!(d.code, ErrorCode::UnboundName);
    let (s, e) = span_of(&text, "missing", 0);
    assert_eq!(
        (d.span.unwrap().start, d.span.unwrap().end),
        (s, e),
        "{}",
        d.render_human()
    );
}

#[test]
fn m2_invalid_e0012_span() {
    let src = load("invalid/e0012_arity.aion");
    let (text, ds) = sema(&src);
    assert_eq!(ds.len(), 1, "{text:?}: {ds:?}");
    let d = &ds[0];
    assert_eq!(d.code, ErrorCode::ArityMismatch);
    let (s, e) = span_of(&text, "(1)", 0);
    assert_eq!(
        (d.span.unwrap().start, d.span.unwrap().end),
        (s, e),
        "{}",
        d.render_human()
    );
}

#[test]
fn m2_invalid_e0013_dupdef_span() {
    let src = load("invalid/e0013_dupdef.aion");
    let (text, ds) = sema(&src);
    assert_eq!(ds.len(), 1, "{text:?}: {ds:?}");
    let d = &ds[0];
    assert_eq!(d.code, ErrorCode::DuplicateDefinition);
    let second = text.rfind("fn f").expect("second fn");
    let (s, e) = (second as u32 + 3, second as u32 + 4);
    assert_eq!(
        (d.span.unwrap().start, d.span.unwrap().end),
        (s, e),
        "{}",
        d.render_human()
    );
    // First definition site is attached as related.
    assert_eq!(d.related.len(), 1);
    let first = text.find("fn f").unwrap();
    assert_eq!(
        (d.related[0].0.start, d.related[0].0.end),
        (first as u32 + 3, first as u32 + 4)
    );
}

#[test]
fn m2_invalid_e0013_dupparam_span() {
    let src = load("invalid/e0013_dupparam.aion");
    let (text, ds) = sema(&src);
    assert_eq!(ds.len(), 1, "{text:?}: {ds:?}");
    let d = &ds[0];
    assert_eq!(d.code, ErrorCode::DuplicateDefinition);
    let hdr = text.find("(a, a)").unwrap();
    let (s, e) = (hdr as u32 + 4, hdr as u32 + 5); // second `a`
    assert_eq!(
        (d.span.unwrap().start, d.span.unwrap().end),
        (s, e),
        "{}",
        d.render_human()
    );
    assert_eq!(d.related.len(), 1);
    assert_eq!(
        (d.related[0].0.start, d.related[0].0.end),
        (hdr as u32 + 1, hdr as u32 + 2)
    );
}

#[test]
fn m2_invalid_e0020_span() {
    let src = load("invalid/e0020_move.aion");
    let (text, ds) = sema(&src);
    assert_eq!(ds.len(), 1, "{text:?}: {ds:?}");
    let d = &ds[0];
    assert_eq!(d.code, ErrorCode::UseAfterMove);
    let use_pos = text.rfind("do s").unwrap() + 3;
    assert_eq!(
        (d.span.unwrap().start, d.span.unwrap().end),
        (use_pos as u32, use_pos as u32 + 1),
        "{}",
        d.render_human()
    );
    // The move site (`let t = s;`) is attached as related.
    assert_eq!(d.related.len(), 1);
    let mv = text.find("= s;").unwrap() + 2;
    assert_eq!(
        (d.related[0].0.start, d.related[0].0.end),
        (mv as u32, mv as u32 + 1)
    );
}

// ---- behavior: Copy, moves, shadowing, scopes, calls, returns ----

fn check_src(src: &str) -> Vec<aion_error::Diagnostic> {
    let b = src.as_bytes();
    let r = aion_syntax::parse(FileId(0), b);
    assert!(r.errors.is_empty(), "test input must parse clean: {src:?}");
    aion_sema::check(&r, FileId(0), b)
}

#[test]
fn m2_invalid_e0020_param_move_span() {
    let src = load("invalid/e0020_param_move.aion");
    let (text, ds) = sema(&src);
    assert_eq!(ds.len(), 1, "{text:?}: {ds:?}");
    let d = &ds[0];
    assert_eq!(d.code, ErrorCode::UseAfterMove);
    // The trailing `do s;` in `fn f` is the use site …
    let f_body = text.find("fn f").unwrap();
    let use_pos = text[f_body..].rfind("do s").unwrap() + f_body + 3;
    assert_eq!(
        (d.span.unwrap().start, d.span.unwrap().end),
        (use_pos as u32, use_pos as u32 + 1),
        "{}",
        d.render_human()
    );
    // … and the move site is the `eat(s)` argument.
    assert_eq!(d.related.len(), 1);
    let mv = text[f_body..].find("eat(s)").unwrap() + f_body + 4;
    assert_eq!(
        (d.related[0].0.start, d.related[0].0.end),
        (mv as u32, mv as u32 + 1),
        "{}",
        d.render_human()
    );
}

#[test]
fn m2_invalid_e0020_call_result_reuse_span() {
    let src = load("invalid/e0020_call_result.aion");
    let (text, ds) = sema(&src);
    assert_eq!(ds.len(), 1, "{text:?}: {ds:?}");
    let d = &ds[0];
    assert_eq!(d.code, ErrorCode::UseAfterMove);
    // The trailing `do a;` reuses the moved call result …
    let main = text.find("fn main").unwrap();
    let use_pos = text[main..].rfind("do a").unwrap() + main + 3;
    assert_eq!(
        (d.span.unwrap().start, d.span.unwrap().end),
        (use_pos as u32, use_pos as u32 + 1),
        "{}",
        d.render_human()
    );
    // … moved by the earlier `eat(a)` argument.
    assert_eq!(d.related.len(), 1);
    let mv = text[main..].find("eat(a)").unwrap() + main + 4;
    assert_eq!(
        (d.related[0].0.start, d.related[0].0.end),
        (mv as u32, mv as u32 + 1),
        "{}",
        d.render_human()
    );
}

#[test]
fn m2_copy_reuse_is_free() {
    for src in [
        "fn main() { let x = 1; let y = x; do x; }",
        "fn main() { let b = true; do b && b; }",
        "fn main() { let n = null; do n == null; }",
        "fn main() { do 1; }",
    ] {
        let ds = check_src(src);
        assert!(
            ds.is_empty(),
            "{src:?}: {:?}",
            ds.iter().map(|d| d.render_human()).collect::<Vec<_>>()
        );
    }
}

#[test]
fn m2_str_let_binding_moves() {
    let ds = check_src("fn main() { let s = \"hi\"; let t = s; do s; }");
    assert_eq!(ds.len(), 1);
    assert_eq!(ds[0].code, ErrorCode::UseAfterMove);
    // The untouched `t` binding itself is fine; only the later use errors.
    let ds = check_src("fn main() { let s = \"hi\"; let t = s; do t; }");
    assert!(ds.is_empty());
}

#[test]
fn m2_str_call_arg_moves() {
    let ds = check_src("fn eat(s) { do s; }\nfn main() { let a = \"x\"; do eat(a); do a; }");
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert_eq!(ds[0].code, ErrorCode::UseAfterMove);
    // Int call args never move.
    let ds = check_src("fn eat(n) { do n; }\nfn main() { let a = 1; do eat(a); do a; }");
    assert!(ds.is_empty());
}

#[test]
fn m2_shadowing_is_legal_and_fresh() {
    // Re-let (even of a moved Str) shadows: the old value is dropped,
    // the new binding is unmoved.
    let ds = check_src("fn main() { let s = \"a\"; let t = s; let s = \"b\"; do s; }");
    assert!(ds.is_empty(), "{ds:?}");
    let ds = check_src("fn main() { let x = 1; let x = x; do x; }");
    assert!(ds.is_empty(), "{ds:?}");
}

#[test]
fn m2_unbound_callee_and_no_cascade() {
    // Unbound name yields exactly one E0011; the error type suppresses
    // follow-on mismatches.
    let ds = check_src("fn main() { do nosuchfn(1) + true; }");
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert_eq!(ds[0].code, ErrorCode::UnboundName);
}

#[test]
fn m2_return_type_flows_through_calls() {
    // `f` returns Int, so `f() + "x"` is a mismatch on the call result.
    let ds = check_src("fn f() { let x = 1; do x; }\nfn main() { do f() + \"x\"; }");
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert_eq!(ds[0].code, ErrorCode::TypeMismatch);
    // Unit-returning body used as Int is also a mismatch.
    let ds = check_src("fn g() { let x = 1; }\nfn main() { do g() + 1; }");
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert_eq!(ds[0].code, ErrorCode::TypeMismatch);
}

#[test]
fn m2_equality_needs_identical_grounds() {
    let ds = check_src("fn main() { do 1 == \"x\"; }");
    assert_eq!(ds.len(), 1);
    assert_eq!(ds[0].code, ErrorCode::TypeMismatch);
    for src in [
        "fn main() { do \"a\" == \"b\"; }",
        "fn main() { do null == null; }",
        "fn main() { do 1 != 2; }",
        "fn main() { do true == true; }",
    ] {
        assert!(check_src(src).is_empty(), "{src:?}");
    }
}

#[test]
fn m2_params_visible_in_body() {
    let ds = check_src("fn id(x) { do x; }\nfn main() { do id(7); }");
    assert!(ds.is_empty(), "{ds:?}");
    // Calling a later-defined function works (forward reference).
    let ds = check_src("fn main() { do helper(1); }\nfn helper(x) { do x; }");
    assert!(ds.is_empty(), "{ds:?}");
}

#[test]
fn m2_skips_when_parse_failed() {
    // Sema entry takes the ParseResult and returns empty on parse errors.
    let src = "fn broken( {";
    let b = src.as_bytes();
    let r = aion_syntax::parse(FileId(0), b);
    assert!(!r.errors.is_empty());
    assert!(aion_sema::check(&r, FileId(0), b).is_empty());
}

#[test]
fn m2_return_str_transfers_ownership() {
    // Returning a local owned Str is a transfer: the caller binding is
    // fresh owned, and normal let-move rules apply to it afterwards.
    let ds = check_src("fn mk() { let s = \"hi\"; do s; }\nfn main() { let r = mk(); do r; }");
    assert!(ds.is_empty(), "{ds:?}");
    // Moving out of the returned value then reusing it is E0020.
    let ds = check_src(
        "fn mk() { do \"hi\"; }\nfn eat(s) { do s; }\nfn main() { let r = mk(); do eat(r); do r; }",
    );
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert_eq!(ds[0].code, ErrorCode::UseAfterMove);
}

#[test]
fn m2_return_str_param_moves() {
    // Returning a Str parameter moves it; the function itself is still
    // clean (nothing follows the trailing use in its own body).
    let ds = check_src("fn id(s) { do s; }\nfn main() { do id(\"x\"); }");
    assert!(ds.is_empty(), "{ds:?}");
    // A moved-from parameter reused in the same function is E0020
    // (the `==` pins `s` to `Str`; the call then moves it).
    let ds = check_src("fn eat(s) { do s; }\nfn f(s) { do s == \"x\"; do eat(s); do s; }");
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert_eq!(ds[0].code, ErrorCode::UseAfterMove);
}

#[test]
fn m2_copy_return_never_moves() {
    // Int (Copy) returns never move: reuse after return-position use is free.
    let ds =
        check_src("fn f() { let x = 1; do x; }\nfn main() { let a = f(); let b = a; do a; do b; }");
    assert!(ds.is_empty(), "{ds:?}");
    let ds = check_src("fn f() { let x = 1; do x; }\nfn main() { do f(); do f(); }");
    assert!(ds.is_empty(), "{ds:?}");
}

// ---- span provenance: sema uses only parser-recorded spans ----

use aion_syntax::{BlockIds, ExprIds, ExprIdsKind, ItemIds, NodeId, StmtIds};

fn collect_ids(pid: &aion_syntax::ProgramIds) -> Vec<NodeId> {
    fn walk_expr(e: &ExprIds, out: &mut Vec<NodeId>) {
        out.push(e.node);
        match &e.kind {
            ExprIdsKind::Leaf => {}
            ExprIdsKind::Unary { op, rhs } => {
                out.push(*op);
                walk_expr(rhs, out);
            }
            ExprIdsKind::Binary { op, lhs, rhs } => {
                out.push(*op);
                walk_expr(lhs, out);
                walk_expr(rhs, out);
            }
            ExprIdsKind::Call {
                callee,
                arg_list,
                args,
            } => {
                out.push(*callee);
                out.push(*arg_list);
                for a in args {
                    walk_expr(a, out);
                }
            }
        }
    }
    fn walk_block(b: &BlockIds, out: &mut Vec<NodeId>) {
        out.push(b.node);
        for s in &b.stmts {
            match s {
                StmtIds::Let { node, name, expr } => {
                    out.push(*node);
                    out.push(*name);
                    walk_expr(expr, out);
                }
                StmtIds::Do { node, expr } => {
                    out.push(*node);
                    walk_expr(expr, out);
                }
                StmtIds::Block { node, body } => {
                    out.push(*node);
                    walk_block(body, out);
                }
                StmtIds::Goal(n) | StmtIds::Empty(n) => out.push(*n),
                StmtIds::Assert { node, expr } => {
                    out.push(*node);
                    walk_expr(expr, out);
                }
            }
        }
    }
    let mut out = vec![pid.node];
    for i in &pid.items {
        match i {
            ItemIds::Fn(f) => {
                out.push(f.node);
                out.push(f.name);
                out.extend(f.params.iter().copied());
                walk_block(&f.body, &mut out);
            }
            ItemIds::Intent(b) => {
                out.push(b.node);
                walk_block(&b.body, &mut out);
            }
        }
    }
    out
}

/// Every diagnostic span (and every related span) sema emits must be a
/// span the parser itself recorded in `ParseResult.spans`.
fn assert_spans_are_parser_recorded(rel: &str) {
    let src = load(rel);
    let r = aion_syntax::parse(FileId(0), &src);
    assert!(r.errors.is_empty());
    let text = String::from_utf8_lossy(&src).into_owned();
    let ds = aion_sema::check(&r, FileId(0), &src);
    assert!(!ds.is_empty(), "{rel} must yield a diagnostic");
    let ids = collect_ids(&r.index);
    let mut table: Vec<(u32, u32)> = Vec::new();
    for id in ids {
        if let Some(sp) = r.spans.get(id) {
            table.push((sp.start, sp.end));
        }
    }
    for d in &ds {
        let sp = d.span.expect("diagnostic must carry a span");
        assert!(
            table.contains(&(sp.start, sp.end)),
            "{rel}: diagnostic span {:?} not parser-recorded: {}",
            (sp.start, sp.end),
            d.render_human(),
        );
        // The sliced source must be the construct the test anchors on.
        let slice = &text[sp.start as usize..sp.end as usize];
        assert!(!slice.is_empty(), "{rel}: empty diagnostic slice");
        for (rsp, _) in &d.related {
            assert!(
                table.contains(&(rsp.start, rsp.end)),
                "{rel}: related span {:?} not parser-recorded",
                (rsp.start, rsp.end),
            );
            assert!(!text[rsp.start as usize..rsp.end as usize].is_empty());
        }
        let _ = slice;
    }
}

#[test]
fn m2_spans_are_parser_recorded() {
    for rel in [
        "invalid/e0010_typemismatch.aion",
        "invalid/e0011_unbound.aion",
        "invalid/e0012_arity.aion",
        "invalid/e0013_dupdef.aion",
        "invalid/e0013_dupparam.aion",
        "invalid/e0020_move.aion",
        "invalid/e0020_param_move.aion",
        "invalid/e0020_call_result.aion",
        "invalid/e0020_param_inferred_str.aion",
    ] {
        assert_spans_are_parser_recorded(rel);
    }
}

#[test]
fn m2_invalid_spans_slice_expected_source() {
    // Every diagnostic span in the m2 invalid fixtures slices the exact
    // source substring the error is about (no drift from text searching).
    let cases: &[(&str, &str)] = &[
        ("invalid/e0010_typemismatch.aion", "1 + \"x\""),
        ("invalid/e0011_unbound.aion", "missing"),
        ("invalid/e0012_arity.aion", "(1)"),
        ("invalid/e0013_dupdef.aion", "f"),
        ("invalid/e0013_dupparam.aion", "a"),
        ("invalid/e0020_move.aion", "s"),
        ("invalid/e0020_param_move.aion", "s"),
        ("invalid/e0020_call_result.aion", "a"),
        ("invalid/e0020_param_inferred_str.aion", "p"),
    ];
    for (rel, anchor) in cases {
        let src = load(rel);
        let r = aion_syntax::parse(FileId(0), &src);
        let text = String::from_utf8_lossy(&src).into_owned();
        let ds = aion_sema::check(&r, FileId(0), &src);
        assert_eq!(ds.len(), 1, "{rel}: {ds:?}");
        let sp = ds[0].span.unwrap();
        let slice = &text[sp.start as usize..sp.end as usize];
        assert_eq!(slice, *anchor, "{rel}: span slices {slice:?}");
        for (rsp, _) in &ds[0].related {
            let rslice = &text[rsp.start as usize..rsp.end as usize];
            assert_eq!(rslice, *anchor, "{rel}: related slices {rslice:?}");
        }
    }
}

#[test]
fn m2_invalid_e0020_param_inferred_str_span() {
    // `p` is never annotated: it is inferred as `Str` through the `eat(p)`
    // call (whose body pins its parameter to `Str`), so the call argument
    // moves it and the second use is E0020 with the move site attached.
    let src = load("invalid/e0020_param_inferred_str.aion");
    let (text, ds) = sema(&src);
    assert_eq!(ds.len(), 1, "{text:?}: {ds:?}");
    let d = &ds[0];
    assert_eq!(d.code, ErrorCode::UseAfterMove);
    let f_body = text.find("fn f").unwrap();
    let use_pos = text[f_body..].rfind("do p").unwrap() + f_body + 3;
    assert_eq!(
        (d.span.unwrap().start, d.span.unwrap().end),
        (use_pos as u32, use_pos as u32 + 1),
        "{}",
        d.render_human()
    );
    assert_eq!(d.related.len(), 1);
    let mv = text[f_body..].find("eat(p)").unwrap() + f_body + 4;
    assert_eq!(
        (d.related[0].0.start, d.related[0].0.end),
        (mv as u32, mv as u32 + 1),
        "{}",
        d.render_human()
    );
}

#[test]
fn m2_copy_param_inferred_through_call_never_moves() {
    // Soundness goes both ways: a parameter only passed to an `Int`-taking
    // callee resolves to `Copy`, so reuse after the call is free.
    let ds = check_src("fn eat(n) { do n + 1; }\nfn f(p) { do eat(p); do p; }");
    assert!(ds.is_empty(), "{ds:?}");
    // Same when the callee is defined after the caller (forward reference).
    let ds = check_src("fn f(p) { do eat(p); do p; }\nfn eat(n) { do n + 1; }");
    assert!(ds.is_empty(), "{ds:?}");
}

#[test]
fn m2_index_mismatch_is_controlled_diagnostic() {
    // Adversarial pairing: program from one parse, index/spans from another
    // with a different item count. Must not panic; yields exactly the
    // controlled ParseError diagnostic.
    let a = b"fn f() { do 1; }";
    let b_src = b"fn f() { do 1; }\nfn g() { do 2; }";
    let mut r = aion_syntax::parse(FileId(0), a);
    let other = aion_syntax::parse(FileId(0), b_src);
    assert!(r.errors.is_empty() && other.errors.is_empty());
    r.index = other.index;
    r.spans = other.spans;
    let ds = aion_sema::check(&r, FileId(0), a);
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert_eq!(ds[0].code, ErrorCode::ParseError);
}

#[test]
fn m2_never_panics_on_arbitrary_bytes() {
    let bytes: Vec<u8> = (0u8..=255u8).collect();
    let r = aion_syntax::parse(FileId(0), &bytes);
    let _ = aion_sema::check(&r, FileId(0), &bytes);
    let r = aion_syntax::parse(FileId(0), b"");
    assert!(aion_sema::check(&r, FileId(0), b"").is_empty());
}
