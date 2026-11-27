//! Stage 4 return-move recording. Semantic analysis is the only writer
//! of `moved_uses`. These tests check membership and `AION-E0020`; they
//! do not lower.

use aion_error::{Diagnostic, ErrorCode};
use aion_sem_model::{SemModel, SymbolKind};
use aion_span::{ByteSpan, FileId};
use aion_syntax::{
    Block, BlockIds, Expr, ExprIds, ExprIdsKind, FeatureFlags, Item, ItemIds, NodeId, ParseResult,
    Stmt, StmtIds,
};

fn check(src: &str) -> (Vec<Diagnostic>, SemModel, ParseResult) {
    let parse = aion_syntax::parse(FileId(0), src.as_bytes());
    assert!(
        parse.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        parse.errors
    );
    let (diags, model) = aion_sema::check_with_model(&parse, FileId(0), src.as_bytes());
    (diags, model, parse)
}

fn check_assert(src: &str) -> (Vec<Diagnostic>, SemModel, ParseResult) {
    let flags = FeatureFlags {
        intent: false,
        goal: false,
        assert_stmt: true,
    };
    let parse = aion_syntax::parse_with_flags(FileId(0), src.as_bytes(), flags);
    assert!(
        parse.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        parse.errors
    );
    let (diags, model) = aion_sema::check_with_model(&parse, FileId(0), src.as_bytes());
    (diags, model, parse)
}

fn slice(src: &str, sp: ByteSpan) -> &str {
    &src[sp.start as usize..sp.end as usize]
}

/// Bare identifier uses in lowering order: `(source text, node, moved)`.
fn ident_facts(src: &str, parse: &ParseResult, model: &SemModel) -> Vec<(String, bool)> {
    fn expr(
        e: &Expr,
        ids: &ExprIds,
        parse: &ParseResult,
        model: &SemModel,
        src: &str,
        out: &mut Vec<(String, bool)>,
    ) {
        match e {
            Expr::Ident(name) => {
                let sp = parse.spans.get(ids.node).expect("ident span");
                assert_eq!(slice(src, sp), name);
                out.push((name.clone(), model.moved_uses.contains(&ids.node)));
            }
            Expr::Unary { rhs, .. } => expr(rhs, unary_rhs(ids), parse, model, src, out),
            Expr::Binary { lhs, rhs, .. } => {
                let (l, r) = binary_parts(ids);
                expr(lhs, l, parse, model, src, out);
                expr(rhs, r, parse, model, src, out);
            }
            Expr::Call { args, .. } => {
                let (_, _, arg_ids) = call_parts(ids);
                for (a, a_ids) in args.iter().zip(arg_ids.iter()) {
                    expr(a, a_ids, parse, model, src, out);
                }
            }
            _ => {}
        }
    }
    fn unary_rhs(ids: &ExprIds) -> &ExprIds {
        match &ids.kind {
            ExprIdsKind::Unary { rhs, .. } => rhs,
            _ => ids,
        }
    }
    fn binary_parts(ids: &ExprIds) -> (&ExprIds, &ExprIds) {
        match &ids.kind {
            ExprIdsKind::Binary { lhs, rhs, .. } => (lhs, rhs),
            _ => (ids, ids),
        }
    }
    fn call_parts(ids: &ExprIds) -> (NodeId, NodeId, &[ExprIds]) {
        match &ids.kind {
            ExprIdsKind::Call {
                callee,
                arg_list,
                args,
            } => (*callee, *arg_list, args),
            _ => (ids.node, ids.node, &[]),
        }
    }
    fn block(
        b: &Block,
        bids: &BlockIds,
        parse: &ParseResult,
        model: &SemModel,
        src: &str,
        out: &mut Vec<(String, bool)>,
    ) {
        for (st, sid) in b.stmts.iter().zip(bids.stmts.iter()) {
            match (st, sid) {
                (Stmt::Let { expr: ex, .. }, StmtIds::Let { expr: eids, .. }) => {
                    expr(ex, eids, parse, model, src, out);
                }
                (Stmt::Do(ex), StmtIds::Do { expr: eids, .. }) => {
                    expr(ex, eids, parse, model, src, out);
                }
                (Stmt::Block(inner), StmtIds::Block { body: ibids, .. }) => {
                    block(inner, ibids, parse, model, src, out);
                }
                (Stmt::Assert { expr: ex, .. }, StmtIds::Assert { expr: eids, .. }) => {
                    expr(ex, eids, parse, model, src, out);
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    for (item, iids) in parse.program.items.iter().zip(parse.index.items.iter()) {
        if let (Item::Fn(f), ItemIds::Fn(fids)) = (item, iids) {
            block(&f.body, &fids.body, parse, model, src, &mut out);
        }
    }
    out
}

fn use_after_move_count(diags: &[Diagnostic]) -> usize {
    diags
        .iter()
        .filter(|d| d.code == ErrorCode::UseAfterMove)
        .count()
}

fn param_moved_flags(model: &SemModel, name: &str) -> Vec<bool> {
    let mut flags = Vec::new();
    for scope in &model.scope_tree.scopes {
        for sym in scope.symbols.values() {
            if sym.kind == SymbolKind::Param && sym.name == name {
                flags.push(sym.moved);
            }
        }
    }
    flags.sort();
    flags
}

fn expect_clean(src: &str, facts: &[(&str, bool)]) {
    let (diags, model, parse) = check(src);
    assert!(diags.is_empty(), "{src:?}: {diags:?}");
    let got = ident_facts(src, &parse, &model);
    let expect: Vec<(String, bool)> = facts.iter().map(|(n, m)| ((*n).to_string(), *m)).collect();
    assert_eq!(got, expect, "{src}");
}

#[test]
fn body_local_return_is_recorded() {
    expect_clean("fn main() { let s = \"hi\"; do s; }", &[("s", true)]);
}

#[test]
fn parameter_return_is_recorded() {
    expect_clean(
        "fn id(s) { do s; } fn main() { let a = \"hi\"; do id(a); }",
        &[("s", true), ("a", true)],
    );
}

#[test]
fn shadowed_return_records_the_body_local() {
    // `let s = s` moves the parameter. The trailing `do s` still moves
    // the new body local, so both use sites are recorded.
    let src = "fn main(s) { let s = s; do s; } fn caller() { let t = \"hi\"; do main(t); }";
    expect_clean(src, &[("s", true), ("s", true), ("t", true)]);
    let (_, model, _) = check(src);
    assert_eq!(param_moved_flags(&model, "s"), vec![true]);
}

#[test]
fn fresh_body_local_does_not_move_the_parameter() {
    // The literal does not move the parameter. Recording the body return
    // must not mark that parameter moved.
    let src = "fn main(s) { let s = \"hi\"; do s; } fn caller() { let t = \"hi\"; do main(t); }";
    expect_clean(src, &[("s", true), ("t", true)]);
    let (_, model, _) = check(src);
    assert_eq!(
        param_moved_flags(&model, "s"),
        vec![false],
        "parameter fallback would mark `s` moved"
    );
}

#[test]
fn already_moved_body_local_is_not_recorded_again() {
    let src = "fn f(s) { do s; } fn main() { let s = \"hi\"; do f(s); do s; }";
    let (diags, model, parse) = check(src);
    assert_eq!(use_after_move_count(&diags), 1, "{diags:?}");
    assert!(diags.iter().all(|d| d.code == ErrorCode::UseAfterMove));
    let got = ident_facts(src, &parse, &model);
    assert_eq!(
        got,
        vec![
            ("s".to_string(), true),
            ("s".to_string(), true),
            ("s".to_string(), false),
        ]
    );
}

#[test]
fn already_moved_shadow_does_not_fall_through_to_parameter() {
    let src = "fn f(s) { do s; } fn main(s) { let s = \"hi\"; do f(s); do s; }";
    let (diags, model, parse) = check(src);
    assert_eq!(use_after_move_count(&diags), 1, "{diags:?}");
    let got = ident_facts(src, &parse, &model);
    // f's return, main's call argument, main's trailing `do` (absent).
    assert_eq!(
        got,
        vec![
            ("s".to_string(), true),
            ("s".to_string(), true),
            ("s".to_string(), false),
        ]
    );
    // f's parameter is moved by its return. main's parameter stays live.
    assert_eq!(param_moved_flags(&model, "s"), vec![false, true]);
}

#[test]
fn root_function_name_is_not_a_return_move() {
    // Empty parameter frame: `do id` resolves in the root, not as a
    // parameter. The parameter return inside `id` is still a move.
    expect_clean(
        "fn id(s) { do s == \"x\"; do s; } fn main() { do id; }",
        &[("s", false), ("s", true), ("id", false)],
    );
    expect_clean(
        "fn main() { do id; } fn id(s) { do s; }",
        &[("id", false), ("s", true)],
    );
}

#[test]
fn parameter_named_id_is_a_return_move() {
    let src = "fn main(id) { do id; } fn caller() { let s = \"hi\"; do main(s); }";
    expect_clean(src, &[("id", true), ("s", true)]);
    let (_, model, _) = check(src);
    assert_eq!(param_moved_flags(&model, "id"), vec![true]);
}

#[test]
fn nested_block_trailing_do_stays_unmoved() {
    expect_clean(
        "fn main() { { let s = \"hi\"; do s; } do \"ok\"; }",
        &[("s", false)],
    );
}

#[test]
fn copy_returns_are_not_recorded() {
    expect_clean(
        "fn main() { let x = 1; do x; do x; }",
        &[("x", false), ("x", false)],
    );
    expect_clean(
        "fn main(n) { do n; } fn caller() { do main(1); }",
        &[("n", false)],
    );
}

#[test]
fn assert_operand_does_not_move_and_trailing_return_does() {
    let src = "fn main() { let s = \"hi\"; assert s == \"hi\"; do s; }";
    let (diags, model, parse) = check_assert(src);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        ident_facts(src, &parse, &model),
        vec![("s".to_string(), false), ("s".to_string(), true)]
    );
}

#[test]
fn assert_true_after_do_is_not_a_return_move() {
    // The source-last statement is the assertion, so the `do` is a borrow.
    let src = "fn main() { let s = \"hi\"; do s; assert true; }";
    let (diags, model, parse) = check_assert(src);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        ident_facts(src, &parse, &model),
        vec![("s".to_string(), false)]
    );
}

#[test]
fn local_under_operator_is_not_moved_by_the_call() {
    expect_clean(
        "fn f(b) { do b; } fn main() { let s = \"hi\"; do f(s == \"x\"); }",
        &[("b", false), ("s", false)],
    );
}

#[test]
fn direct_call_argument_is_recorded() {
    expect_clean(
        "fn f(s) { do s; } fn main() { let a = \"hi\"; do f(a); }",
        &[("s", true), ("a", true)],
    );
}
