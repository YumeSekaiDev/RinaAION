//! M3.2 lowering tests: shape, access kinds, E0014 gate, span preservation.
use super::*;
use aion_span::FileId;
use aion_syntax::Item;

fn parse_check(src: &str) -> (ParseResult, aion_sem_model::SemModel, Vec<Diagnostic>) {
    let file = FileId(0);
    let parse = aion_syntax::parse(file, src.as_bytes());
    assert!(parse.errors.is_empty(), "parse errors: {:?}", parse.errors);
    let (diags, model) = aion_sema::check_with_model(&parse, file, src.as_bytes());
    (parse, model, diags)
}

fn lower_clean(src: &str) -> (TypedModule, ParseResult) {
    let (parse, model, diags) = parse_check(src);
    assert!(diags.is_empty(), "sema diags: {:?}", diags);
    let m = lower(&parse, &model, FileId(0), src.len() as u32)
        .unwrap_or_else(|diags| panic!("lower failed: {diags:?}"));
    (m, parse)
}

fn parse_check_flags(
    src: &str,
    flags: aion_syntax::FeatureFlags,
) -> (ParseResult, aion_sem_model::SemModel, Vec<Diagnostic>) {
    let file = FileId(0);
    let parse = aion_syntax::parse_with_flags(file, src.as_bytes(), flags);
    assert!(parse.errors.is_empty(), "parse errors: {:?}", parse.errors);
    let (diags, model) = aion_sema::check_with_model(&parse, file, src.as_bytes());
    (parse, model, diags)
}

fn assert_flags() -> aion_syntax::FeatureFlags {
    aion_syntax::FeatureFlags {
        intent: false,
        goal: false,
        assert_stmt: true,
    }
}

fn lower_flags(src: &str) -> (TypedModule, ParseResult) {
    let (parse, model, diags) = parse_check_flags(src, assert_flags());
    assert!(diags.is_empty(), "sema diags: {:?}", diags);
    let m = lower(&parse, &model, FileId(0), src.len() as u32)
        .unwrap_or_else(|diags| panic!("lower failed: {diags:?}"));
    (m, parse)
}

#[test]
fn assert_lowers_to_distinct_typed_assert() {
    // Non-literal operand keeps the dynamic path (a bare `true`
    // literal would be eliminated by M3 folding). The test derives
    // the expected span from the recorded index instead of hard-coding it.
    let src = "fn main() { assert 1 == 1; }";
    let (parse, model, diags) = parse_check_flags(src, assert_flags());
    assert!(diags.is_empty(), "sema diags: {:?}", diags);
    let m = lower(&parse, &model, FileId(0), src.len() as u32)
        .unwrap_or_else(|diags| panic!("lower failed: {diags:?}"));
    let main = &m.functions[0];
    assert_eq!(main.body.stmts.len(), 1);
    // No block value: assertions contribute nothing.
    assert_eq!(main.body.value, Ty::Unit);
    let mut stmt_span = None;
    for item in &parse.index.items {
        if let ItemIds::Fn(fids) = item {
            for sid in &fids.body.stmts {
                if let StmtIds::Assert { node, .. } = sid {
                    stmt_span = parse.spans.get(*node);
                }
            }
        }
    }
    match &main.body.stmts[0] {
        TypedStmt::Assert {
            expr,
            span,
            provenance,
        } => {
            assert_eq!(expr.ty, Ty::Bool);
            assert_eq!(*provenance, AssertProvenance::human_authored());
            assert_eq!(Some(*span), stmt_span, "typed assert keeps statement span");
        }
        s => panic!("expected TypedStmt::Assert, got {s:?}"),
    }
}

fn assert_stmt_node(parse: &ParseResult) -> ByteSpan {
    for item in &parse.index.items {
        if let ItemIds::Fn(fids) = item {
            for sid in &fids.body.stmts {
                if let StmtIds::Assert { node, .. } = sid {
                    return parse.spans.get(*node).expect("assert stmt span recorded");
                }
            }
        }
    }
    panic!("no assert statement in index");
}

#[test]
fn assert_true_is_eliminated() {
    // Eliminated completely: no node survives, and a preceding
    // trailing value is preserved as if the statement never existed.
    let (m, _) = lower_flags("fn main() { assert true; }");
    assert!(m.functions[0].body.stmts.is_empty());
    assert_eq!(m.functions[0].body.value, Ty::Unit);
    let (m, _) = lower_flags("fn main() { do 1; assert true; }");
    assert_eq!(m.functions[0].body.stmts.len(), 1);
    assert!(matches!(m.functions[0].body.stmts[0], TypedStmt::Do { .. }));
    assert_eq!(m.functions[0].body.value, Ty::Int);
}

#[test]
fn assert_false_folds_to_unconditional_trap() {
    let src = "fn main() { assert false; }";
    let (m, parse) = lower_flags(src);
    let main = &m.functions[0];
    assert_eq!(main.body.stmts.len(), 1);
    assert_eq!(main.body.value, Ty::Unit);
    match &main.body.stmts[0] {
        TypedStmt::AssertFalse { span, provenance } => {
            assert_eq!(*span, assert_stmt_node(&parse));
            assert_eq!(*provenance, AssertProvenance::human_authored());
        }
        s => panic!("expected folded AssertFalse, got {s:?}"),
    }
    // The folded representation validates clean.
    crate::validate(&m).expect("folded assert must validate");
}

#[test]
fn assert_non_literal_keeps_dynamic_behavior() {
    // Inferred constants and computed expressions are never folded.
    let src = "fn main() { let x = false; assert x; assert 1 == 1; }";
    let (m, _) = lower_flags(src);
    let main = &m.functions[0];
    assert_eq!(main.body.stmts.len(), 3);
    assert!(matches!(main.body.stmts[1], TypedStmt::Assert { .. }));
    assert!(matches!(main.body.stmts[2], TypedStmt::Assert { .. }));
}

#[test]
fn assert_non_bool_operand_is_e0010() {
    // M1 semantic precondition lock: invalid assertions never reach
    // lowering or execution.
    let src = "fn main() { assert 1; }";
    let (_, _, diags) = parse_check_flags(src, assert_flags());
    assert!(
        diags.iter().any(|d| d.code == ErrorCode::TypeMismatch),
        "expected E0010, got {:?}",
        diags.iter().map(|d| d.render_human()).collect::<Vec<_>>()
    );
}

/// Collect every lowered string-typed expression and block scope.
fn lowered_allocs_and_scopes(m: &TypedModule) -> (Vec<AllocKind>, Vec<ScopeId>) {
    fn exprs(e: &TypedExpr, out: &mut Vec<AllocKind>) {
        if e.ty == Ty::Str {
            out.push(e.alloc);
        }
        match &e.kind {
            ExprKind::Unary { rhs, .. } => exprs(rhs, out),
            ExprKind::Binary { lhs, rhs, .. } => {
                exprs(lhs, out);
                exprs(rhs, out);
            }
            ExprKind::Call { args, .. } => {
                for a in args {
                    exprs(a, out);
                }
            }
            _ => {}
        }
    }
    fn block(b: &TypedBlock, allocs: &mut Vec<AllocKind>, scopes: &mut Vec<ScopeId>) {
        scopes.push(b.scope);
        for s in &b.stmts {
            match s {
                TypedStmt::Let { init, .. } => exprs(init, allocs),
                TypedStmt::Do { expr } => exprs(expr, allocs),
                TypedStmt::Assert { expr, .. } => exprs(expr, allocs),
                TypedStmt::AssertFalse { .. } => {}
                TypedStmt::Block { body } => block(body, allocs, scopes),
                TypedStmt::Empty => {}
            }
        }
    }
    let mut allocs = Vec::new();
    let mut scopes = Vec::new();
    for f in &m.functions {
        block(&f.body, &mut allocs, &mut scopes);
    }
    (allocs, scopes)
}

#[test]
fn string_nodes_carry_explicit_refcounted_provenance() {
    // Stage 3: every lowered string-typed expression is explicitly
    // refcounted (no defaults, no arena allocation yet).
    let (m, _) = lower_flags("fn main() { let s = \"hi\"; do s; assert s == \"hi\"; }");
    let (allocs, _) = lowered_allocs_and_scopes(&m);
    assert!(!allocs.is_empty(), "expected string operands");
    assert!(
        allocs.iter().all(|a| *a == AllocKind::Refcounted),
        "all string nodes must be explicitly refcounted: {allocs:?}"
    );
}

#[test]
fn lowered_blocks_carry_published_scopes() {
    // Stage 3: every lowered block's scope matches the model's
    // published mapping; scopes nest distinctly.
    let src = "fn main() { let x = 1; { do x; { do x; } } do x; }";
    let (parse, model, diags) = parse_check_flags(src, assert_flags());
    assert!(diags.is_empty(), "sema diags: {:?}", diags);
    let m = lower(&parse, &model, FileId(0), src.len() as u32)
        .unwrap_or_else(|diags| panic!("lower failed: {diags:?}"));
    let (_, scopes) = lowered_allocs_and_scopes(&m);
    assert_eq!(scopes.len(), 3, "fn body plus two nested blocks");
    let mut published: std::collections::HashSet<ScopeId> = std::collections::HashSet::new();
    for item in &parse.index.items {
        if let ItemIds::Fn(fids) = item {
            let mut nodes = Vec::new();
            collect_block_nodes(&fids.body, &mut nodes);
            for n in nodes {
                published.insert(model.block_scopes[&n]);
            }
        }
    }
    for s in &scopes {
        assert!(
            published.contains(s),
            "lowered scope {s:?} must be a published scope"
        );
    }
    let mut uniq = scopes.clone();
    uniq.sort_by_key(|s| s.0);
    uniq.dedup_by_key(|s| s.0);
    assert_eq!(uniq.len(), scopes.len(), "nested scopes must be distinct");
}

fn collect_block_nodes(bids: &BlockIds, out: &mut Vec<NodeId>) {
    out.push(bids.node);
    for sid in &bids.stmts {
        if let StmtIds::Block { body, .. } = sid {
            collect_block_nodes(body, out);
        }
    }
}

#[test]
fn missing_scope_fails_closed_without_partial_ir() {
    // Removing one published scope entry must fail lowering for the
    // whole function: an internal diagnostic, no default scope, and
    // no partially lowered function in the output module.
    let src = "fn main() { let x = 1; { do x; } do x; }";
    let (parse, mut model, diags) = parse_check_flags(src, assert_flags());
    assert!(diags.is_empty(), "sema must be clean: {:?}", diags);
    let mut nodes = Vec::new();
    for item in &parse.index.items {
        if let ItemIds::Fn(fids) = item {
            collect_block_nodes(&fids.body, &mut nodes);
        }
    }
    let removed = nodes[1];
    assert!(model.block_scopes.remove(&removed).is_some());
    let Err(ldiags) = lower(&parse, &model, FileId(0), src.len() as u32) else {
        panic!("expected lowering failure")
    };
    assert!(
        ldiags.iter().any(|d| {
            d.code == ErrorCode::InternalIrInvalid
                && d.code.code_str() == "AION-E0030"
                && d.message.contains("block scope missing")
        }),
        "expected fail-closed diagnostic, got {:?}",
        ldiags.iter().map(|d| d.render_human()).collect::<Vec<_>>()
    );
}

fn all_spans(parse: &ParseResult) -> Vec<ByteSpan> {
    // Collect every recorded span by walking ids in lockstep with the AST.
    let mut out = Vec::new();
    out.push(parse.index.node);
    for item in &parse.index.items {
        match item {
            ItemIds::Fn(f) => {
                out.push(f.node);
                out.push(f.name);
                out.extend(f.params.iter().copied());
                collect_block(&f.body, &mut out);
            }
            ItemIds::Intent(b) => collect_block(&b.body, &mut out),
        }
    }
    out.into_iter()
        .filter_map(|id| parse.spans.get(id))
        .collect()
}

fn collect_block(b: &BlockIds, out: &mut Vec<NodeId>) {
    out.push(b.node);
    for s in &b.stmts {
        match s {
            StmtIds::Let { node, name, expr } => {
                out.push(*node);
                out.push(*name);
                collect_expr(expr, out);
            }
            StmtIds::Do { node, expr } => {
                out.push(*node);
                collect_expr(expr, out);
            }
            StmtIds::Block { node, body } => {
                out.push(*node);
                collect_block(body, out);
            }
            StmtIds::Goal { node: id, .. } => out.push(*id),
            StmtIds::Assert { node, expr } => {
                out.push(*node);
                collect_expr(expr, out);
            }
            StmtIds::Empty(id) => out.push(*id),
        }
    }
}

fn collect_expr(e: &ExprIds, out: &mut Vec<NodeId>) {
    out.push(e.node);
    match &e.kind {
        ExprIdsKind::Leaf => {}
        ExprIdsKind::Unary { op, rhs } => {
            out.push(*op);
            collect_expr(rhs, out);
        }
        ExprIdsKind::Binary { op, lhs, rhs } => {
            out.push(*op);
            collect_expr(lhs, out);
            collect_expr(rhs, out);
        }
        ExprIdsKind::Call {
            callee,
            arg_list,
            args,
        } => {
            out.push(*callee);
            out.push(*arg_list);
            for a in args {
                collect_expr(a, out);
            }
        }
    }
}

fn expr_spans(e: &TypedExpr, out: &mut Vec<ByteSpan>) {
    out.push(e.span);
    match &e.kind {
        ExprKind::Unary { rhs, .. } => expr_spans(rhs, out),
        ExprKind::Binary { lhs, rhs, .. } => {
            expr_spans(lhs, out);
            expr_spans(rhs, out);
        }
        ExprKind::Call { args, .. } => {
            for a in args {
                expr_spans(a, out);
            }
        }
        _ => {}
    }
}

#[test]
fn shape_int_arith() {
    let (m, _) = lower_clean("fn main() { let x = 1 + 2; do x; }");
    assert_eq!(m.functions.len(), 1);
    let f = &m.functions[0];
    assert_eq!(f.name, "main");
    assert_eq!(f.params.len(), 0);
    assert_eq!(f.ret, Ty::Int);
    assert_eq!(f.body.value, Ty::Int);
    assert_eq!(f.body.stmts.len(), 2);
    match &f.body.stmts[0] {
        TypedStmt::Let { name, ty, init } => {
            assert_eq!(name, "x");
            assert_eq!(*ty, Ty::Int);
            assert_eq!(init.ty, Ty::Int);
            match &init.kind {
                ExprKind::Binary { op, .. } => assert_eq!(*op, BinOp::Add),
                k => panic!("expected binary, got {k:?}"),
            }
        }
        s => panic!("expected let, got {s:?}"),
    }
}

#[test]
fn access_copy_for_int_borrow() {
    // `Copy` value reused at a consuming position stays `Copy`.
    let (m, _) = lower_clean("fn main() { let x = 1; do x; do x; }");
    let f = &m.functions[0];
    // Trailing value is the last `do x` (Int).
    assert_eq!(f.body.value, Ty::Int);
    for stmt in &f.body.stmts {
        if let TypedStmt::Do { expr } = stmt {
            match &expr.kind {
                ExprKind::Local { access, .. } => assert_eq!(*access, LocalAccess::Copy),
                k => panic!("expected local, got {k:?}"),
            }
        }
    }
}

#[test]
fn access_move_for_call_arg() {
    let (m, _) =
        lower_clean("fn id(s) { do s == \"x\"; do s; } fn main() { let a = \"hi\"; do id(a); }");
    let main = m.functions.iter().find(|f| f.name == "main").unwrap();
    // `let a = "hi"` initializer is Str; `id(a)` moves `a`.
    match &main.body.stmts[1] {
        TypedStmt::Do { expr } => match &expr.kind {
            ExprKind::Call { args, .. } => {
                assert_eq!(args.len(), 1);
                match &args[0].kind {
                    ExprKind::Local { name, access } => {
                        assert_eq!(name, "a");
                        assert_eq!(*access, LocalAccess::Move);
                    }
                    k => panic!("expected local arg, got {k:?}"),
                }
            }
            k => panic!("expected call, got {k:?}"),
        },
        s => panic!("expected do, got {s:?}"),
    }
    // The `let` initializer itself is a Str literal (not a Local).
    match &main.body.stmts[0] {
        TypedStmt::Let { init, .. } => assert_eq!(init.ty, Ty::Str),
        s => panic!("expected let, got {s:?}"),
    }
}

#[test]
fn access_move_for_let_init_and_return() {
    // `let b = a` moves `a`; trailing `do b` moves `b` (return transfer).
    let (m, _) = lower_clean("fn main() { let a = \"hi\"; let b = a; do b; }");
    let f = &m.functions[0];
    assert_eq!(f.ret, Ty::Str);
    assert_eq!(f.body.value, Ty::Str);
    match &f.body.stmts[1] {
        TypedStmt::Let { init, .. } => match &init.kind {
            ExprKind::Local { name, access } => {
                assert_eq!(name, "a");
                assert_eq!(*access, LocalAccess::Move);
            }
            k => panic!("expected local init, got {k:?}"),
        },
        s => panic!("expected let, got {s:?}"),
    }
    match &f.body.stmts[2] {
        TypedStmt::Do { expr } => match &expr.kind {
            ExprKind::Local { name, access } => {
                assert_eq!(name, "b");
                assert_eq!(*access, LocalAccess::Move);
            }
            k => panic!("expected local, got {k:?}"),
        },
        s => panic!("expected do, got {s:?}"),
    }
}

#[test]
fn access_borrow_before_move_is_copy() {
    // `do a == "x"` borrows; `do id(a)` moves. Both must be clean sema.
    let (m, _) = lower_clean(
        "fn id(s) { do s == \"x\"; do s; } fn main() { let a = \"hi\"; do a == \"x\"; do id(a); }",
    );
    let main = m.functions.iter().find(|f| f.name == "main").unwrap();
    match &main.body.stmts[1] {
        TypedStmt::Do { expr } => match &expr.kind {
            ExprKind::Binary { lhs, .. } => match &lhs.kind {
                ExprKind::Local { access, .. } => assert_eq!(*access, LocalAccess::Copy),
                k => panic!("expected local, got {k:?}"),
            },
            k => panic!("expected binary, got {k:?}"),
        },
        s => panic!("expected do, got {s:?}"),
    }
}

#[test]
fn trailing_do_else_unit_and_nested_block() {
    // No trailing `do` => Unit; nested trailing block propagates its value.
    let (m, _) = lower_clean("fn a() { let x = 1; } fn b() { { do 2; } }");
    let a = m.functions.iter().find(|f| f.name == "a").unwrap();
    assert_eq!(a.body.value, Ty::Unit);
    assert_eq!(a.ret, Ty::Unit);
    let b = m.functions.iter().find(|f| f.name == "b").unwrap();
    assert_eq!(b.body.value, Ty::Int);
    match &b.body.stmts[0] {
        TypedStmt::Block { body } => assert_eq!(body.value, Ty::Int),
        s => panic!("expected block, got {s:?}"),
    }
}

#[test]
fn unresolved_var_gates_e0014_with_no_ir() {
    // `x` is never constrained: residual Var => E0014, empty module.
    let src = "fn id(x) { do x; }";
    let (parse, model, diags) = parse_check(src);
    assert!(diags.is_empty());
    let Err(ldiags) = lower(&parse, &model, FileId(0), src.len() as u32) else {
        panic!("expected unresolved-type failure")
    };
    assert!(!ldiags.is_empty());
    assert!(ldiags.iter().all(|d| d.code == ErrorCode::UnresolvedType));
    assert_eq!(ldiags[0].code.code_str(), "AION-E0014");
}

#[test]
fn spans_come_from_parse_table() {
    let (m, parse) =
        lower_clean("fn id(s) { do s == \"x\"; do s; } fn main() { let a = \"hi\"; do id(a); }");
    let table: Vec<ByteSpan> = all_spans(&parse);
    assert!(!table.is_empty());
    let mut got = Vec::new();
    for f in &m.functions {
        for s in &f.body.stmts {
            match s {
                TypedStmt::Let { init, .. } => expr_spans(init, &mut got),
                TypedStmt::Do { expr } => expr_spans(expr, &mut got),
                TypedStmt::Assert { expr, .. } => expr_spans(expr, &mut got),
                // Folded false carries no operand expression.
                TypedStmt::AssertFalse { .. } => {}
                TypedStmt::Block { body } => {
                    for s2 in &body.stmts {
                        if let TypedStmt::Do { expr } = s2 {
                            expr_spans(expr, &mut got);
                        }
                    }
                }
                TypedStmt::Empty => {}
            }
        }
    }
    assert!(!got.is_empty());
    for sp in got {
        assert!(table.contains(&sp), "span {sp:?} not in parse table");
    }
}

#[test]
fn lower_threads_decl_and_eof_spans() {
    let src = "fn main() { do 1; } fn id(s) { do s == \"x\"; do s; }";
    let (m, _parse) = lower_clean(src);
    assert_eq!(m.functions.len(), 2);
    let slice = |sp: ByteSpan| &src[sp.start as usize..sp.end as usize];
    assert_eq!(
        slice(m.functions[0].decl_span),
        "fn main()",
        "main decl must be the `fn main()` header"
    );
    assert_eq!(
        slice(m.functions[1].decl_span),
        "fn id(s)",
        "second fn decl must be its own header"
    );
    assert_eq!(
        m.eof_span,
        ByteSpan::empty(FileId(0), src.len() as u32),
        "eof span must be empty at end of input"
    );
}

mod validate_tests {
    use super::super::{
        lower, validate, AllocKind, ExprKind, LocalAccess, TypedBlock, TypedExpr, TypedFn,
        TypedModule, TypedStmt,
    };
    use super::lower_clean;
    use aion_error::ErrorCode;
    use aion_sem_model::{ScopeId, Ty};
    use aion_span::{ByteSpan, FileId};
    use aion_syntax::{BinOp, UnOp};

    fn sp(s: u32, e: u32) -> ByteSpan {
        ByteSpan::new(FileId(0), s, e).expect("valid span")
    }

    fn int_lit(v: i64, s: u32, e: u32) -> TypedExpr {
        TypedExpr {
            ty: Ty::Int,
            span: sp(s, e),
            kind: ExprKind::Int(v),
            alloc: AllocKind::Refcounted,
        }
    }

    fn str_lit(v: &str, s: u32, e: u32) -> TypedExpr {
        TypedExpr {
            ty: Ty::Str,
            span: sp(s, e),
            kind: ExprKind::Str(v.to_string()),
            // Explicit refcounted provenance for string literals.
            alloc: AllocKind::Refcounted,
        }
    }

    fn local(name: &str, ty: Ty, access: LocalAccess, s: u32, e: u32) -> TypedExpr {
        TypedExpr {
            ty,
            span: sp(s, e),
            kind: ExprKind::Local {
                name: name.to_string(),
                access,
            },
            // Inert for non-string types; assigned explicitly.
            alloc: AllocKind::Refcounted,
        }
    }

    fn unary(op: UnOp, rhs: TypedExpr, ty: Ty, s: u32, e: u32) -> TypedExpr {
        TypedExpr {
            ty,
            span: sp(s, e),
            kind: ExprKind::Unary {
                op,
                rhs: Box::new(rhs),
            },
            // Inert for non-string types; assigned explicitly.
            alloc: AllocKind::Refcounted,
        }
    }

    fn binary(op: BinOp, lhs: TypedExpr, rhs: TypedExpr, ty: Ty, s: u32, e: u32) -> TypedExpr {
        TypedExpr {
            ty,
            span: sp(s, e),
            kind: ExprKind::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            // Inert for non-string types; assigned explicitly.
            alloc: AllocKind::Refcounted,
        }
    }

    fn call(callee: &str, args: Vec<TypedExpr>, ty: Ty, s: u32, e: u32) -> TypedExpr {
        TypedExpr {
            ty,
            span: sp(s, e),
            kind: ExprKind::Call {
                callee: callee.to_string(),
                args,
            },
            // Inert for non-string types; assigned explicitly.
            alloc: AllocKind::Refcounted,
        }
    }

    fn mk_fn(
        name: &str,
        params: Vec<(String, Ty)>,
        ret: Ty,
        stmts: Vec<TypedStmt>,
        value: Ty,
    ) -> TypedFn {
        mk_fn_at(ScopeId(1001), name, params, ret, stmts, value)
    }

    fn mk_fn_at(
        scope: ScopeId,
        name: &str,
        params: Vec<(String, Ty)>,
        ret: Ty,
        stmts: Vec<TypedStmt>,
        value: Ty,
    ) -> TypedFn {
        TypedFn {
            name: name.to_string(),
            scope,
            decl_span: sp(0, 9),
            params,
            ret,
            // Single-block fixture: the body's own scope.
            body: TypedBlock {
                stmts,
                value,
                scope: ScopeId(scope.0 + 1),
            },
        }
    }

    /// Unwrap validation diagnostics (success maps to empty vec) for
    /// negative-path assertions.
    fn check(m: &TypedModule) -> Vec<aion_error::Diagnostic> {
        match validate(m) {
            Ok(_) => Vec::new(),
            Err(ds) => ds,
        }
    }

    fn do_stmt(e: TypedExpr) -> TypedStmt {
        TypedStmt::Do { expr: e }
    }

    fn let_stmt(name: &str, ty: Ty, init: TypedExpr) -> TypedStmt {
        TypedStmt::Let {
            name: name.to_string(),
            ty,
            init,
        }
    }

    fn assert_single_e0030(ds: &[aion_error::Diagnostic]) {
        assert_eq!(ds.len(), 1, "expected exactly one diagnostic, got {ds:?}");
        assert_eq!(ds[0].code, ErrorCode::InternalIrInvalid);
        assert_eq!(ds[0].code.code_str(), "AION-E0030");
        assert!(ds[0].span.is_some());
        assert!(ds[0].notes.iter().any(|n| n.contains("ir validator")));
    }

    fn valid_module() -> TypedModule {
        // fn main() -> int { let x: int = 1; do x; }
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions.push(mk_fn(
            "main",
            vec![],
            Ty::Int,
            vec![
                let_stmt("x", Ty::Int, int_lit(1, 1, 2)),
                do_stmt(local("x", Ty::Int, LocalAccess::Copy, 3, 4)),
            ],
            Ty::Int,
        ));
        m
    }

    #[test]
    fn valid_module_passes_clean() {
        let ds = check(&valid_module());
        assert!(ds.is_empty(), "unexpected diags: {ds:?}");
    }

    #[test]
    fn diagnostics_are_deduplicated_by_source_span() {
        let shared = sp(1, 2);
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions.push(mk_fn(
            "main",
            vec![],
            Ty::Int,
            vec![
                do_stmt(local("missing_first", Ty::Int, LocalAccess::Copy, 1, 2)),
                do_stmt(local("missing_second", Ty::Int, LocalAccess::Copy, 1, 2)),
            ],
            Ty::Int,
        ));

        let ds = check(&m);

        assert_eq!(ds.len(), 1, "distinct nodes with one span dedupe: {ds:?}");
        assert_eq!(ds[0].code, ErrorCode::InternalIrInvalid);
        assert_eq!(ds[0].span, Some(shared));
        assert_eq!(ds[0].message, "use of unbound slot `missing_first`");
        assert_eq!(ds[0].notes, vec!["ir validator: WF1"]);
    }

    #[test]
    fn wf1_unbound_slot() {
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions.push(mk_fn(
            "main",
            vec![],
            Ty::Int,
            vec![do_stmt(local("y", Ty::Int, LocalAccess::Copy, 1, 2))],
            Ty::Int,
        ));
        assert_single_e0030(&check(&m));
    }

    #[test]
    fn wf2_use_after_move() {
        // The first consumption is a legal let-initializer Move. The
        // later use is a different span and is already-moved, so WF2
        // wins and WF9 does not also fire.
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions.push(mk_fn(
            "main",
            vec![],
            Ty::Str,
            vec![
                let_stmt("s", Ty::Str, str_lit("a", 1, 4)),
                let_stmt("t", Ty::Str, local("s", Ty::Str, LocalAccess::Move, 5, 6)),
                do_stmt(local("s", Ty::Str, LocalAccess::Copy, 7, 8)),
            ],
            Ty::Str,
        ));
        let ds = check(&m);
        assert_single_e0030(&ds);
        assert_eq!(ds[0].span, Some(sp(7, 8)));
        assert!(
            ds[0].notes.iter().any(|n| n.contains("WF2")),
            "notes: {:?}",
            ds[0].notes
        );
        assert!(
            ds[0].notes.iter().all(|n| !n.contains("WF9")),
            "WF9 must not share this span: {:?}",
            ds[0].notes
        );
    }

    #[test]
    fn wf3_ret_mismatch() {
        // Declares `-> int` but the body yields `()`.
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions
            .push(mk_fn("main", vec![], Ty::Int, vec![], Ty::Unit));
        let ds = check(&m);
        assert_eq!(ds.len(), 1, "got {ds:?}");
        assert_eq!(ds[0].code.code_str(), "AION-E0030");
    }

    #[test]
    fn wf4_operator_mismatch() {
        // `1 + 2` recorded as Bool.
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions.push(mk_fn(
            "main",
            vec![],
            Ty::Bool,
            vec![do_stmt(binary(
                BinOp::Add,
                int_lit(1, 1, 2),
                int_lit(2, 3, 4),
                Ty::Bool,
                1,
                4,
            ))],
            Ty::Bool,
        ));
        assert_single_e0030(&check(&m));
    }

    #[test]
    fn wf4_arity_mismatch() {
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions.push(mk_fn(
            "id",
            vec![("s".to_string(), Ty::Str)],
            Ty::Bool,
            vec![do_stmt(binary(
                BinOp::Eq,
                local("s", Ty::Str, LocalAccess::Copy, 1, 2),
                str_lit("x", 3, 6),
                Ty::Bool,
                1,
                6,
            ))],
            Ty::Bool,
        ));
        m.functions.push(mk_fn_at(
            ScopeId(1003),
            "main",
            vec![],
            Ty::Bool,
            vec![do_stmt(call("id", vec![], Ty::Bool, 10, 14))],
            Ty::Bool,
        ));
        assert_single_e0030(&check(&m));
    }

    #[test]
    fn wf5_dummy_span() {
        // Lowering fallback span counts as missing coverage.
        let e = TypedExpr {
            ty: Ty::Int,
            span: ByteSpan::empty(FileId(0), 0),
            kind: ExprKind::Int(1),
            // Inert for non-string types; assigned explicitly.
            alloc: AllocKind::Refcounted,
        };
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions
            .push(mk_fn("main", vec![], Ty::Int, vec![do_stmt(e)], Ty::Int));
        assert_single_e0030(&check(&m));
    }

    #[test]
    fn wf6_dangling_callee() {
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions.push(mk_fn(
            "main",
            vec![],
            Ty::Int,
            vec![do_stmt(call("nope", vec![], Ty::Int, 1, 5))],
            Ty::Int,
        ));
        assert_single_e0030(&check(&m));
    }

    #[test]
    fn wf7_move_of_copy_typed_slot() {
        // Lowering never emits `Move` for a non-owned slot.
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions.push(mk_fn(
            "main",
            vec![],
            Ty::Int,
            vec![
                let_stmt("x", Ty::Int, int_lit(1, 1, 2)),
                do_stmt(local("x", Ty::Int, LocalAccess::Move, 3, 4)),
            ],
            Ty::Int,
        ));
        assert_single_e0030(&check(&m));
    }

    #[test]
    fn wf7_copy_of_str_after_move() {
        // A second Move after a legal let-init Move is WF2 only.
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions.push(mk_fn(
            "main",
            vec![],
            Ty::Str,
            vec![
                let_stmt("s", Ty::Str, str_lit("a", 1, 4)),
                let_stmt("t", Ty::Str, local("s", Ty::Str, LocalAccess::Move, 5, 6)),
                do_stmt(local("s", Ty::Str, LocalAccess::Move, 7, 8)),
            ],
            Ty::Str,
        ));
        let ds = check(&m);
        assert_single_e0030(&ds);
        assert_eq!(ds[0].span, Some(sp(7, 8)));
        assert!(
            ds[0].notes.iter().any(|n| n.contains("WF2")),
            "notes: {:?}",
            ds[0].notes
        );
        assert!(
            ds[0]
                .notes
                .iter()
                .all(|n| !n.contains("WF9") && !n.contains("WF7")),
            "later use must stay WF2: {:?}",
            ds[0].notes
        );
    }

    #[test]
    fn wf8_duplicate_function_names_rejected() {
        // Hand-built IR with two `main`s (sema would emit AION-E0013 on
        // the real pipeline, so this only arises adversarially) must
        // fail validation — dup mains must never silently execute.
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions.push(mk_fn(
            "main",
            vec![],
            Ty::Int,
            vec![do_stmt(int_lit(1, 1, 2))],
            Ty::Int,
        ));
        let mut second = mk_fn_at(
            ScopeId(1003),
            "main",
            vec![],
            Ty::Int,
            vec![do_stmt(int_lit(2, 3, 4))],
            Ty::Int,
        );
        second.decl_span = sp(20, 29);
        m.functions.push(second);
        let ds = check(&m);
        assert_single_e0030(&ds);
        assert_eq!(ds[0].span, Some(sp(20, 29)), "span must be the second decl");
        assert!(
            ds[0].notes.iter().any(|n| n.contains("WF8")),
            "notes must cite WF8: {:?}",
            ds[0].notes
        );
    }

    #[test]
    fn adversarial_totality() {
        // Empty module, zero functions: clean, no panic.
        let ds = check(&TypedModule::new("empty", ScopeId(1000)));
        assert!(ds.is_empty());
        // Deep nesting: no panic.
        let mut deep = int_lit(1, 1, 2);
        for i in 0..500u32 {
            deep = unary(UnOp::Neg, deep, Ty::Int, 1, 10 + i);
        }
        let mut m = TypedModule::new("deep", ScopeId(1000));
        m.functions
            .push(mk_fn("main", vec![], Ty::Int, vec![do_stmt(deep)], Ty::Int));
        let _ = validate(&m);
        // Garbage types and empty blocks: no panic.
        let mut g = TypedModule::new("garbage", ScopeId(1000));
        g.functions.push(mk_fn(
            "f",
            vec![("p".to_string(), Ty::Var(7))],
            Ty::Var(9),
            vec![
                TypedStmt::Empty,
                TypedStmt::Block {
                    body: TypedBlock {
                        stmts: vec![],
                        value: Ty::Var(3),
                        // Nested block gets its own scope, distinct
                        // from the enclosing function-body scope.
                        scope: ScopeId(1),
                    },
                },
                do_stmt(local("missing", Ty::Var(1), LocalAccess::Move, 2, 3)),
            ],
            Ty::Error,
        ));
        let ds = check(&g);
        assert!(ds.iter().all(|d| d.code == ErrorCode::InternalIrInvalid));
        assert!(!ds.is_empty());
    }

    fn rule_notes(ds: &[aion_error::Diagnostic]) -> Vec<String> {
        ds.iter().flat_map(|d| d.notes.clone()).collect()
    }

    #[test]
    fn wf7_precedes_wf9_on_move_of_int() {
        // Trailing `do` of an Int would also be the wrong stamp for a
        // return, but WF7 reports the non-Str Move first.
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions.push(mk_fn(
            "main",
            vec![],
            Ty::Int,
            vec![
                let_stmt("x", Ty::Int, int_lit(1, 1, 2)),
                do_stmt(local("x", Ty::Int, LocalAccess::Move, 3, 4)),
            ],
            Ty::Int,
        ));
        let ds = check(&m);
        assert_single_e0030(&ds);
        assert!(
            ds[0].notes.iter().any(|n| n.contains("WF7")),
            "notes: {:?}",
            ds[0].notes
        );
        assert!(ds[0].notes.iter().all(|n| !n.contains("WF9")));
    }

    #[test]
    fn wf9_move_outside_a_move_site() {
        // A non-final `do` of a live Str is a borrow. Stamping Move is WF9,
        // and that stamp still consumes the slot.
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions.push(mk_fn(
            "main",
            vec![],
            Ty::Str,
            vec![
                let_stmt("s", Ty::Str, str_lit("a", 1, 4)),
                do_stmt(local("s", Ty::Str, LocalAccess::Move, 5, 6)),
                do_stmt(local("s", Ty::Str, LocalAccess::Copy, 7, 8)),
            ],
            Ty::Str,
        ));
        let ds = check(&m);
        assert_eq!(ds.len(), 2, "got {ds:?}");
        assert!(ds.iter().all(|d| d.code == ErrorCode::InternalIrInvalid));
        assert_eq!(ds[0].span, Some(sp(5, 6)));
        assert!(ds[0].notes.iter().any(|n| n.contains("WF9")), "{:?}", ds[0]);
        assert_eq!(ds[1].span, Some(sp(7, 8)));
        assert!(ds[1].notes.iter().any(|n| n.contains("WF2")), "{:?}", ds[1]);
    }

    #[test]
    fn wf9_copy_on_let_init_and_trailing_return() {
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions.push(mk_fn(
            "main",
            vec![],
            Ty::Str,
            vec![
                let_stmt("a", Ty::Str, str_lit("a", 1, 4)),
                let_stmt("b", Ty::Str, local("a", Ty::Str, LocalAccess::Copy, 5, 6)),
                do_stmt(local("b", Ty::Str, LocalAccess::Copy, 7, 8)),
            ],
            Ty::Str,
        ));
        let ds = check(&m);
        assert_eq!(ds.len(), 2, "got {:?}", rule_notes(&ds));
        assert!(ds.iter().all(|d| d.notes.iter().any(|n| n.contains("WF9"))));
        assert_eq!(ds[0].span, Some(sp(5, 6)));
        assert_eq!(ds[1].span, Some(sp(7, 8)));
    }

    #[test]
    fn wf9_move_nested_under_call_or_assert() {
        // A local under an operator is not the call-argument root.
        let mut nested = TypedModule::new("t", ScopeId(1000));
        nested.functions.push(mk_fn(
            "f",
            vec![("p".into(), Ty::Bool)],
            Ty::Bool,
            vec![do_stmt(local("p", Ty::Bool, LocalAccess::Copy, 1, 2))],
            Ty::Bool,
        ));
        nested.functions.push(mk_fn_at(
            ScopeId(1003),
            "main",
            vec![],
            Ty::Bool,
            vec![
                let_stmt("s", Ty::Str, str_lit("a", 3, 6)),
                do_stmt(call(
                    "f",
                    vec![binary(
                        BinOp::Eq,
                        local("s", Ty::Str, LocalAccess::Move, 7, 8),
                        str_lit("a", 9, 12),
                        Ty::Bool,
                        7,
                        12,
                    )],
                    Ty::Bool,
                    7,
                    14,
                )),
            ],
            Ty::Bool,
        ));
        let ds = check(&nested);
        assert_single_e0030(&ds);
        assert_eq!(ds[0].span, Some(sp(7, 8)));
        assert!(ds[0].notes.iter().any(|n| n.contains("WF9")), "{ds:?}");

        // An assertion operand suppresses even a direct call-argument move.
        let mut asserted = TypedModule::new("t", ScopeId(1000));
        asserted.functions.push(mk_fn(
            "f",
            vec![("p".into(), Ty::Str)],
            Ty::Str,
            vec![do_stmt(local("p", Ty::Str, LocalAccess::Move, 1, 2))],
            Ty::Str,
        ));
        asserted.functions.push(mk_fn_at(
            ScopeId(1003),
            "main",
            vec![],
            Ty::Unit,
            vec![
                let_stmt("s", Ty::Str, str_lit("a", 3, 6)),
                TypedStmt::Assert {
                    expr: call(
                        "f",
                        vec![local("s", Ty::Str, LocalAccess::Move, 8, 9)],
                        Ty::Bool,
                        7,
                        11,
                    ),
                    span: sp(7, 12),
                    provenance: aion_error::AssertProvenance::human_authored(),
                },
            ],
            Ty::Unit,
        ));
        let ds = check(&asserted);
        assert_single_e0030(&ds);
        assert_eq!(ds[0].span, Some(sp(8, 9)));
        assert!(ds[0].notes.iter().any(|n| n.contains("WF9")), "{ds:?}");
    }

    #[test]
    fn legal_move_sites_and_nested_copy_pass() {
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions.push(mk_fn(
            "id",
            vec![("s".into(), Ty::Str)],
            Ty::Str,
            vec![do_stmt(local("s", Ty::Str, LocalAccess::Move, 1, 2))],
            Ty::Str,
        ));
        m.functions.push(mk_fn_at(
            ScopeId(1003),
            "main",
            vec![],
            Ty::Str,
            vec![
                let_stmt("a", Ty::Str, str_lit("hi", 3, 7)),
                TypedStmt::Block {
                    body: TypedBlock {
                        stmts: vec![
                            let_stmt("s", Ty::Str, str_lit("n", 8, 11)),
                            do_stmt(local("s", Ty::Str, LocalAccess::Copy, 12, 13)),
                        ],
                        value: Ty::Str,
                        scope: ScopeId(1),
                    },
                },
                let_stmt("b", Ty::Str, local("a", Ty::Str, LocalAccess::Move, 14, 15)),
                do_stmt(call(
                    "id",
                    vec![local("b", Ty::Str, LocalAccess::Move, 16, 17)],
                    Ty::Str,
                    16,
                    20,
                )),
            ],
            Ty::Str,
        ));
        let ds = check(&m);
        assert!(ds.is_empty(), "unexpected diags: {ds:?}");
    }

    #[test]
    fn assert_true_shape_keeps_wf3_when_stamp_is_copy() {
        // Lowering drops `assert true`, so a preceding `do` can be the
        // last IR statement while `TypedFn.ret` stays Unit. Copy matches
        // that return type. WF3 reports the value mismatch.
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions.push(mk_fn(
            "main",
            vec![],
            Ty::Unit,
            vec![
                let_stmt("s", Ty::Str, str_lit("hi", 1, 5)),
                do_stmt(local("s", Ty::Str, LocalAccess::Copy, 6, 7)),
            ],
            Ty::Str,
        ));
        let ds = check(&m);
        assert_single_e0030(&ds);
        assert!(
            ds[0].notes.iter().any(|n| n.contains("WF3")),
            "notes: {:?}",
            ds[0].notes
        );
        assert!(ds[0].notes.iter().all(|n| !n.contains("WF9")));
    }

    #[test]
    fn move_stamp_on_assert_true_shape_is_wf9() {
        // The same shape with a Move stamp occupies the span with WF9,
        // so WF3 does not also fire on that node.
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions.push(mk_fn(
            "main",
            vec![],
            Ty::Unit,
            vec![
                let_stmt("s", Ty::Str, str_lit("hi", 1, 5)),
                do_stmt(local("s", Ty::Str, LocalAccess::Move, 6, 7)),
            ],
            Ty::Str,
        ));
        let ds = check(&m);
        assert_single_e0030(&ds);
        assert_eq!(ds[0].span, Some(sp(6, 7)));
        assert!(ds[0].notes.iter().any(|n| n.contains("WF9")), "{ds:?}");
        assert!(ds[0].notes.iter().all(|n| !n.contains("WF3")));
    }

    fn wf14_diags(m: &TypedModule) -> Vec<aion_error::Diagnostic> {
        check(m)
            .into_iter()
            .filter(|d| d.notes.iter().any(|note| note == "ir validator: WF14"))
            .collect()
    }

    #[test]
    fn wf14_accepts_unique_nonmonotonic_scope_ancestry() {
        let mut m = TypedModule::new("scopes", ScopeId(41));
        let first = mk_fn_at(
            ScopeId(7),
            "first",
            vec![],
            Ty::Int,
            vec![TypedStmt::Block {
                body: TypedBlock {
                    stmts: vec![TypedStmt::Block {
                        body: TypedBlock {
                            stmts: vec![do_stmt(int_lit(1, 1, 2))],
                            value: Ty::Int,
                            scope: ScopeId(90),
                        },
                    }],
                    // Phase 4: recursive result propagates through nesting.
                    value: Ty::Int,
                    scope: ScopeId(3),
                },
            }],
            Ty::Int,
        );
        let second = mk_fn_at(
            ScopeId(100),
            "main",
            vec![],
            Ty::Int,
            vec![do_stmt(int_lit(3, 4, 5))],
            Ty::Int,
        );
        m.functions = vec![first, second];
        assert!(validate(&m).is_ok(), "unique opaque IDs should pass");
    }

    #[test]
    fn wf14_module_function_collision_uses_later_function_span() {
        let mut m = TypedModule::new("duplicate", ScopeId(50));
        m.eof_span = sp(30, 30);
        let mut f = mk_fn_at(
            m.scope,
            "main",
            vec![],
            Ty::Int,
            vec![do_stmt(int_lit(1, 1, 2))],
            Ty::Int,
        );
        f.decl_span = sp(10, 20);
        m.functions.push(f);
        let ds = wf14_diags(&m);
        assert_eq!(ds.len(), 1, "{ds:?}");
        assert_eq!(ds[0].code, ErrorCode::InternalIrInvalid);
        assert_eq!(
            ds[0].message,
            "scope id 50 is assigned to more than one scope"
        );
        assert_eq!(ds[0].span, Some(sp(10, 20)));
        assert_eq!(ds[0].notes, vec!["ir validator: WF14"]);
        assert!(
            validate(&m).is_err(),
            "invalid scopes must block validation"
        );
    }

    #[test]
    fn wf14_duplicate_function_scopes_use_later_declaration_span() {
        let mut m = TypedModule::new("duplicate", ScopeId(50));
        let mut first = mk_fn_at(
            ScopeId(20),
            "first",
            vec![],
            Ty::Int,
            vec![do_stmt(int_lit(1, 1, 2))],
            Ty::Int,
        );
        first.decl_span = sp(10, 20);
        let mut second = mk_fn_at(
            ScopeId(20),
            "second",
            vec![],
            Ty::Int,
            vec![do_stmt(int_lit(2, 3, 4))],
            Ty::Int,
        );
        second.body.scope = ScopeId(22);
        second.decl_span = sp(21, 30);
        m.functions = vec![first, second];
        let ds = wf14_diags(&m);
        assert_eq!(ds.len(), 1, "{ds:?}");
        assert_eq!(ds[0].span, Some(sp(21, 30)));
        assert_eq!(
            ds[0].message,
            "scope id 20 is assigned to more than one scope"
        );
    }

    #[test]
    fn wf14_function_block_collision_uses_later_block_expression_span() {
        let mut m = TypedModule::new("duplicate", ScopeId(50));
        let mut f = mk_fn_at(
            ScopeId(20),
            "main",
            vec![],
            Ty::Int,
            vec![do_stmt(int_lit(1, 1, 2))],
            Ty::Int,
        );
        f.decl_span = sp(10, 20);
        f.body.scope = f.scope;
        m.functions.push(f);
        let ds = wf14_diags(&m);
        assert_eq!(ds.len(), 1, "{ds:?}");
        assert_eq!(ds[0].span, Some(sp(1, 2)));
        assert_eq!(
            ds[0].message,
            "scope id 20 is assigned to more than one scope"
        );
    }

    #[test]
    fn wf14_block_collision_uses_later_expression_span() {
        let mut m = TypedModule::new("duplicate", ScopeId(50));
        let mut f = mk_fn_at(
            ScopeId(20),
            "main",
            vec![],
            Ty::Int,
            vec![
                TypedStmt::Block {
                    body: TypedBlock {
                        stmts: vec![do_stmt(int_lit(1, 1, 2))],
                        value: Ty::Int,
                        scope: ScopeId(3),
                    },
                },
                TypedStmt::Block {
                    body: TypedBlock {
                        stmts: vec![do_stmt(int_lit(2, 3, 4))],
                        value: Ty::Int,
                        scope: ScopeId(3),
                    },
                },
            ],
            // Phase 4: body derives Int through the trailing block.
            Ty::Int,
        );
        f.decl_span = sp(10, 20);
        m.functions.push(f);
        let ds = wf14_diags(&m);
        assert_eq!(ds.len(), 1, "{ds:?}");
        assert_eq!(ds[0].span, Some(sp(3, 4)));
        assert_eq!(
            ds[0].message,
            "scope id 3 is assigned to more than one scope"
        );
    }

    #[test]
    fn wf14_nested_block_collision_uses_later_block_span() {
        let mut m = TypedModule::new("duplicate", ScopeId(50));
        let mut f = mk_fn_at(
            ScopeId(20),
            "main",
            vec![],
            Ty::Int,
            vec![TypedStmt::Block {
                body: TypedBlock {
                    stmts: vec![
                        do_stmt(int_lit(0, 1, 2)),
                        TypedStmt::Block {
                            body: TypedBlock {
                                stmts: vec![do_stmt(int_lit(1, 3, 4))],
                                value: Ty::Int,
                                scope: ScopeId(3),
                            },
                        },
                    ],
                    // Phase 4: recursive result propagates through nesting.
                    value: Ty::Int,
                    scope: ScopeId(3),
                },
            }],
            // Phase 4: body derives Int through the trailing block.
            Ty::Int,
        );
        f.decl_span = sp(10, 20);
        m.functions.push(f);
        let ds = wf14_diags(&m);
        assert_eq!(ds.len(), 1, "{ds:?}");
        assert_eq!(
            ds[0].message,
            "scope id 3 is assigned to more than one scope"
        );
        assert_eq!(ds[0].span, Some(sp(3, 4)));
    }

    #[test]
    fn wf14_empty_block_collision_uses_function_declaration_span() {
        let mut m = TypedModule::new("duplicate", ScopeId(50));
        let mut f = mk_fn_at(
            ScopeId(20),
            "main",
            vec![],
            Ty::Unit,
            vec![
                TypedStmt::Block {
                    body: TypedBlock {
                        stmts: vec![do_stmt(int_lit(1, 1, 2))],
                        value: Ty::Int,
                        scope: ScopeId(3),
                    },
                },
                TypedStmt::Block {
                    body: TypedBlock {
                        stmts: vec![],
                        value: Ty::Unit,
                        scope: ScopeId(3),
                    },
                },
            ],
            Ty::Unit,
        );
        f.decl_span = sp(10, 20);
        m.functions.push(f);
        let ds = wf14_diags(&m);
        assert_eq!(ds.len(), 1, "{ds:?}");
        assert_eq!(ds[0].span, Some(sp(10, 20)));
    }

    #[test]
    fn wf14_reports_each_later_duplicate_assignment() {
        let mut m = TypedModule::new("duplicate", ScopeId(50));
        let mut f = mk_fn_at(
            ScopeId(20),
            "main",
            vec![],
            Ty::Int,
            vec![1u32, 3, 5]
                .into_iter()
                .map(|start| TypedStmt::Block {
                    body: TypedBlock {
                        stmts: vec![do_stmt(int_lit(start.into(), start, start + 1))],
                        value: Ty::Int,
                        scope: ScopeId(3),
                    },
                })
                .collect(),
            // Phase 4: body derives Int through the trailing block.
            Ty::Int,
        );
        f.decl_span = sp(10, 20);
        m.functions.push(f);

        let ds = wf14_diags(&m);
        assert_eq!(ds.len(), 2, "{ds:?}");
        assert_eq!(ds[0].span, Some(sp(3, 4)));
        assert_eq!(ds[1].span, Some(sp(5, 6)));
        assert!(ds.iter().all(|diag| {
            diag.code == ErrorCode::InternalIrInvalid
                && diag.message == "scope id 3 is assigned to more than one scope"
                && diag.notes == vec!["ir validator: WF14"]
        }));
    }

    #[test]
    fn wf14_follows_existing_wf_diagnostics_and_respects_span_deduplication() {
        let mut m = TypedModule::new("ordered", ScopeId(50));
        m.eof_span = sp(1, 2);
        let mut f = mk_fn_at(
            m.scope,
            "main",
            vec![],
            Ty::Int,
            vec![do_stmt(local("unbound", Ty::Int, LocalAccess::Copy, 1, 2))],
            Ty::Int,
        );
        f.decl_span = sp(1, 2);
        m.functions.push(f);
        let ds = check(&m);
        assert_eq!(
            ds.len(),
            1,
            "WF1 at the shared span suppresses WF14: {ds:?}"
        );
        assert_eq!(ds[0].notes, vec!["ir validator: WF1"]);

        let mut ordered = TypedModule::new("ordered", ScopeId(50));
        ordered.eof_span = sp(30, 30);
        let mut duplicate = mk_fn_at(
            ordered.scope,
            "main",
            vec![],
            Ty::Int,
            vec![do_stmt(local("unbound", Ty::Int, LocalAccess::Copy, 1, 2))],
            Ty::Int,
        );
        duplicate.decl_span = sp(10, 20);
        ordered.functions.push(duplicate);
        let ds = check(&ordered);
        assert_eq!(ds.len(), 2, "{ds:?}");
        assert_eq!(ds[0].notes, vec!["ir validator: WF1"]);
        assert_eq!(ds[1].notes, vec!["ir validator: WF14"]);
    }

    #[test]
    fn wf8_candidates_use_first_diagnostic_per_span() {
        let mut m = TypedModule::new("wf8", ScopeId(10));
        let first = mk_fn_at(ScopeId(20), "duplicate", vec![], Ty::Unit, vec![], Ty::Unit);
        let mut second = mk_fn_at(
            ScopeId(30),
            "duplicate",
            vec![],
            Ty::Int,
            vec![do_stmt(local(
                "missing",
                Ty::Int,
                LocalAccess::Copy,
                20,
                21,
            ))],
            Ty::Int,
        );
        second.decl_span = sp(20, 21);
        let mut third = mk_fn_at(ScopeId(40), "duplicate", vec![], Ty::Unit, vec![], Ty::Unit);
        third.decl_span = sp(30, 31);
        let mut fourth = mk_fn_at(ScopeId(50), "duplicate", vec![], Ty::Unit, vec![], Ty::Unit);
        fourth.decl_span = sp(30, 31);
        m.functions = vec![first, second, third, fourth];

        let ds = check(&m);
        assert!(
            ds.iter().any(
                |diag| diag.notes == vec!["ir validator: WF1"] && diag.span == Some(sp(20, 21))
            ),
            "WF1 must win the span shared with WF8: {ds:?}"
        );
        assert!(
            ds.iter().any(
                |diag| diag.span == Some(sp(30, 31)) && diag.notes == vec!["ir validator: WF8"]
            ),
            "WF8 should still be emitted at an otherwise unclaimed span: {ds:?}"
        );
        assert_eq!(
            ds.iter()
                .filter(|diag| diag.span == Some(sp(20, 21)))
                .count(),
            1,
            "only one diagnostic may claim WF1's span: {ds:?}"
        );
        assert_eq!(
            ds.iter()
                .filter(|diag| diag.span == Some(sp(30, 31)))
                .count(),
            1,
            "duplicate WF8 candidates at one span must deduplicate: {ds:?}"
        );
        assert!(!ds
            .iter()
            .any(|diag| diag.notes == vec!["ir validator: WF14"]));
    }

    #[test]
    fn lowered_clean_module_validates() {
        // End to end: real lowering output passes the validator.
        let parse = aion_syntax::parse(FileId(0), b"fn main() { let x = 1 + 2; do x; }");
        assert!(parse.errors.is_empty());
        let (diags, model) =
            aion_sema::check_with_model(&parse, FileId(0), b"fn main() { let x = 1 + 2; do x; }");
        assert!(diags.is_empty());
        let src = b"fn main() { let x = 1 + 2; do x; }";
        let m = lower(&parse, &model, FileId(0), src.len() as u32)
            .unwrap_or_else(|diags| panic!("lower failed: {diags:?}"));
        validate(&m).expect("clean module must validate");
    }

    // Phase 4: recursive block-result validation.

    fn nested_block(stmts: Vec<TypedStmt>, value: Ty, scope: u32) -> TypedStmt {
        TypedStmt::Block {
            body: TypedBlock {
                stmts,
                value,
                scope: ScopeId(scope),
            },
        }
    }

    fn assert_stmt(s: u32, e: u32) -> TypedStmt {
        TypedStmt::Assert {
            expr: binary(
                BinOp::Eq,
                int_lit(1, s, e),
                int_lit(1, s, e),
                Ty::Bool,
                s,
                e,
            ),
            span: sp(s, e),
            provenance: aion_error::AssertProvenance::human_authored(),
        }
    }

    fn assert_false_stmt(s: u32, e: u32) -> TypedStmt {
        TypedStmt::AssertFalse {
            span: sp(s, e),
            provenance: aion_error::AssertProvenance::human_authored(),
        }
    }

    #[test]
    fn phase4_recursive_multi_level_result_passes() {
        // fn main() -> int { { { do 1; } } }: result derives through
        // three levels to the innermost `do`.
        let inner = nested_block(vec![do_stmt(int_lit(1, 4, 5))], Ty::Int, 2002);
        let mid = nested_block(vec![inner], Ty::Int, 2001);
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions
            .push(mk_fn("main", vec![], Ty::Int, vec![mid], Ty::Int));
        assert!(check(&m).is_empty(), "deep result must validate");
    }

    #[test]
    fn phase4_wrong_nested_value_rejected_at_innermost_span() {
        // Inner block claims Bool while recursion derives Int.
        let inner = nested_block(vec![do_stmt(int_lit(1, 4, 5))], Ty::Bool, 2002);
        let mid = nested_block(vec![inner], Ty::Int, 2001);
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions
            .push(mk_fn("main", vec![], Ty::Int, vec![mid], Ty::Int));
        let ds = check(&m);
        assert_single_e0030(&ds);
        assert_eq!(ds[0].span, Some(sp(4, 5)));
        assert_eq!(ds[0].notes, vec!["ir validator: WF4"]);
    }

    #[test]
    fn phase4_wrong_mid_level_value_rejected() {
        // Mid block claims Unit while recursion through it derives Int.
        let inner = nested_block(vec![do_stmt(int_lit(1, 4, 5))], Ty::Int, 2002);
        let mid = nested_block(vec![inner], Ty::Unit, 2001);
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions
            .push(mk_fn("main", vec![], Ty::Int, vec![mid], Ty::Unit));
        let ds = check(&m);
        assert_single_e0030(&ds);
        assert_eq!(ds[0].span, Some(sp(4, 5)));
        assert_eq!(ds[0].notes, vec!["ir validator: WF4"]);
    }

    #[test]
    fn phase4_wrong_fn_return_rejected_through_nesting() {
        // Body derives Int through nesting but declares Bool return.
        let inner = nested_block(vec![do_stmt(int_lit(1, 4, 5))], Ty::Int, 2001);
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions
            .push(mk_fn("main", vec![], Ty::Bool, vec![inner], Ty::Int));
        let ds = check(&m);
        assert_single_e0030(&ds);
        assert_eq!(ds[0].span, Some(sp(4, 5)));
        assert_eq!(ds[0].notes, vec!["ir validator: WF3"]);
        assert!(
            ds[0].message.contains("function returns"),
            "unexpected message: {}",
            ds[0].message
        );
    }

    #[test]
    fn phase4_final_let_assert_and_empty_yield_unit() {
        // Final Let, Assert, AssertFalse, Empty, and no statements all
        // derive Unit; a Unit value validates clean.
        let cases: Vec<Vec<TypedStmt>> = vec![
            vec![let_stmt("x", Ty::Int, int_lit(1, 10, 11))],
            vec![assert_stmt(10, 12)],
            vec![assert_false_stmt(10, 12)],
            vec![TypedStmt::Empty],
            vec![],
        ];
        for (i, stmts) in cases.into_iter().enumerate() {
            let mut m = TypedModule::new("t", ScopeId(1000));
            m.functions
                .push(mk_fn("main", vec![], Ty::Unit, stmts, Ty::Unit));
            assert!(check(&m).is_empty(), "case {i} must derive Unit");
        }
    }

    #[test]
    fn phase4_final_let_with_non_unit_value_rejected() {
        // A function body ending in `let` derives Unit; claiming Int is
        // a WF3 mismatch (function bodies report WF3, nested blocks WF4).
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions.push(mk_fn(
            "main",
            vec![],
            Ty::Unit,
            vec![let_stmt("x", Ty::Int, int_lit(1, 10, 11))],
            Ty::Int,
        ));
        let ds = check(&m);
        assert_single_e0030(&ds);
        assert_eq!(ds[0].notes, vec!["ir validator: WF3"]);
        assert!(
            ds[0].message.contains("must be `()` without trailing `do`"),
            "unexpected message: {}",
            ds[0].message
        );
    }

    #[test]
    fn phase4_nested_final_let_with_non_unit_value_rejected() {
        // A nested block ending in `let` derives Unit; claiming Int is
        // a WF4 mismatch at the initializer span.
        let inner = nested_block(
            vec![let_stmt("x", Ty::Int, int_lit(1, 10, 11))],
            Ty::Int,
            2001,
        );
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions
            .push(mk_fn("main", vec![], Ty::Unit, vec![inner], Ty::Unit));
        let ds = check(&m);
        assert_single_e0030(&ds);
        assert_eq!(ds[0].span, Some(sp(10, 11)));
        assert_eq!(ds[0].notes, vec!["ir validator: WF4"]);
        assert!(
            ds[0].message.contains("must be `()` without trailing `do`"),
            "unexpected message: {}",
            ds[0].message
        );
    }

    #[test]
    fn phase4_nested_final_let_yields_unit() {
        // { let x = 1; } as trailing block derives Unit, not Int.
        let inner = nested_block(
            vec![let_stmt("x", Ty::Int, int_lit(1, 10, 11))],
            Ty::Unit,
            2001,
        );
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions
            .push(mk_fn("main", vec![], Ty::Unit, vec![inner], Ty::Unit));
        assert!(check(&m).is_empty());
    }

    #[test]
    fn phase4_trailing_empty_yields_unit() {
        // Per the contract, a final `Empty` yields Unit even when a
        // value-producing statement precedes it.
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions.push(mk_fn(
            "main",
            vec![],
            Ty::Unit,
            vec![do_stmt(int_lit(1, 4, 5)), TypedStmt::Empty],
            Ty::Unit,
        ));
        assert!(check(&m).is_empty());
    }

    #[test]
    fn phase4_trailing_empty_with_non_unit_value_rejected() {
        // [do 1, Empty] derives Unit; claiming Int is a WF3 mismatch.
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions.push(mk_fn(
            "main",
            vec![],
            Ty::Unit,
            vec![do_stmt(int_lit(1, 4, 5)), TypedStmt::Empty],
            Ty::Int,
        ));
        let ds = check(&m);
        assert_single_e0030(&ds);
        assert_eq!(ds[0].notes, vec!["ir validator: WF3"]);
    }

    #[test]
    fn phase4_final_block_child_selected_by_position() {
        // Two nested blocks: the parent must reuse the FINAL child's
        // derivation (Unit), not the earlier one (Int). This pins the
        // bottom-up capture against positional mix-ups.
        let first = nested_block(vec![do_stmt(int_lit(1, 4, 5))], Ty::Int, 2001);
        let second = nested_block(
            vec![let_stmt("x", Ty::Int, int_lit(1, 10, 11))],
            Ty::Unit,
            2002,
        );
        let mut m = TypedModule::new("t", ScopeId(1000));
        m.functions.push(mk_fn(
            "main",
            vec![],
            Ty::Unit,
            vec![first, second],
            Ty::Unit,
        ));
        assert!(check(&m).is_empty());
    }

    #[test]
    fn phase4_nested_final_local_read_stays_copy() {
        // Lowered nested `do a` is a Copy read: recursion must not
        // promote it to a return Move, and validation must pass.
        let (m, _) = lower_clean("fn main() { let a = \"hi\"; { do a; } do 1; }");
        let f = &m.functions[0];
        assert_eq!(f.body.value, Ty::Int);
        match &f.body.stmts[1] {
            TypedStmt::Block { body } => match &body.stmts[0] {
                TypedStmt::Do { expr } => match &expr.kind {
                    ExprKind::Local { name, access } => {
                        assert_eq!(name, "a");
                        assert_eq!(*access, LocalAccess::Copy);
                    }
                    k => panic!("expected local, got {k:?}"),
                },
                s => panic!("expected do, got {s:?}"),
            },
            s => panic!("expected block, got {s:?}"),
        };
        validate(&m).expect("nested copy read must validate");
    }

    #[test]
    fn phase4_direct_trailing_return_still_moves() {
        // The approved direct-return Move site is unchanged by recursion.
        let (m, _) = lower_clean("fn id(s) { do s; } fn main() { let a = \"hi\"; do id(a); }");
        validate(&m).expect("direct return move must validate");
        let id = m.functions.iter().find(|f| f.name == "id").unwrap();
        match &id.body.stmts[0] {
            TypedStmt::Do { expr } => match &expr.kind {
                ExprKind::Local { name, access } => {
                    assert_eq!(name, "s");
                    assert_eq!(*access, LocalAccess::Move);
                }
                k => panic!("expected local, got {k:?}"),
            },
            s => panic!("expected do, got {s:?}"),
        }
    }
}

/// Collect every bare-identifier use-site [`NodeId`] in the order
/// lowering visits expressions (statement order; `let` init, `do` value,
/// nested blocks, then callee args left to right).
fn ident_uses_in_order(parse: &ParseResult) -> Vec<NodeId> {
    fn expr(e: &Expr, ids: &ExprIds, out: &mut Vec<NodeId>) {
        match e {
            Expr::Ident(_) => out.push(ids.node),
            Expr::Unary { rhs, .. } => expr(rhs, unary_rhs(ids), out),
            Expr::Binary { lhs, rhs, .. } => {
                let (l, r) = binary_parts(ids);
                expr(lhs, l, out);
                expr(rhs, r, out);
            }
            Expr::Call { args, .. } => {
                let (_, _, arg_ids) = call_parts(ids);
                for (a, a_ids) in args.iter().zip(arg_ids.iter()) {
                    expr(a, a_ids, out);
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
    fn block(b: &Block, bids: &BlockIds, out: &mut Vec<NodeId>) {
        for (st, sid) in b.stmts.iter().zip(bids.stmts.iter()) {
            match (st, sid) {
                (Stmt::Let { expr: ex, .. }, StmtIds::Let { expr: eids, .. }) => {
                    expr(ex, eids, out)
                }
                (Stmt::Do(ex), StmtIds::Do { expr: eids, .. }) => expr(ex, eids, out),
                (Stmt::Block(inner), StmtIds::Block { body: ibids, .. }) => {
                    block(inner, ibids, out)
                }
                (Stmt::Assert { expr: ex, .. }, StmtIds::Assert { expr: eids, .. }) => {
                    expr(ex, eids, out)
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    for (item, iids) in parse.program.items.iter().zip(parse.index.items.iter()) {
        if let (aion_syntax::Item::Fn(f), ItemIds::Fn(fids)) = (item, iids) {
            block(&f.body, &fids.body, &mut out);
        }
    }
    out
}

/// Collect every lowered [`LocalAccess`] in the same visit order.
fn lowered_accesses_in_order(m: &TypedModule) -> Vec<LocalAccess> {
    fn expr(e: &TypedExpr, out: &mut Vec<LocalAccess>) {
        match &e.kind {
            ExprKind::Local { access, .. } => out.push(*access),
            ExprKind::Unary { rhs, .. } => expr(rhs, out),
            ExprKind::Binary { lhs, rhs, .. } => {
                expr(lhs, out);
                expr(rhs, out);
            }
            ExprKind::Call { args, .. } => {
                for a in args {
                    expr(a, out);
                }
            }
            _ => {}
        }
    }
    fn block(b: &TypedBlock, out: &mut Vec<LocalAccess>) {
        for st in &b.stmts {
            match st {
                TypedStmt::Let { init, .. } => expr(init, out),
                TypedStmt::Do { expr: e } => expr(e, out),
                TypedStmt::Assert { expr: e, .. } => expr(e, out),
                // Folded false records no accesses.
                TypedStmt::AssertFalse { .. } => {}
                TypedStmt::Block { body } => block(body, out),
                TypedStmt::Empty => {}
            }
        }
    }
    let mut out = Vec::new();
    for f in &m.functions {
        block(&f.body, &mut out);
    }
    out
}

#[test]
fn move_authority_is_sema_recorded_set() {
    use std::collections::HashSet;
    // Cover every move site (call arg, `let` init, trailing return) plus
    // Copy positions (borrow before move, `Copy` reuse, shadowing, nested
    // blocks whose trailing value must not move).
    let programs = [
        "fn id(s) { do s == \"x\"; do s; } fn main() { let a = \"hi\"; do id(a); }",
        "fn main() { let a = \"hi\"; let b = a; do b; }",
        "fn main() { let a = \"hi\"; do a == \"x\"; do a; }",
        "fn main() { let s = \"a\"; let s = s; do s; }",
        "fn main() { let x = 1; do x; do x; }",
        "fn main() { { do 1; } do 2; }",
        "fn mk() { let s = \"hi\"; do s; } fn main() { let r = mk(); do r; }",
    ];
    for src in programs {
        let (parse, model, diags) = parse_check(src);
        assert!(diags.is_empty(), "sema diags for {src:?}: {diags:?}");
        let uses = ident_uses_in_order(&parse);
        let m = lower(&parse, &model, FileId(0), src.len() as u32)
            .unwrap_or_else(|diags| panic!("lower failed: {diags:?}"));
        let accesses = lowered_accesses_in_order(&m);
        assert_eq!(
            uses.len(),
            accesses.len(),
            "ident/IR walk length mismatch for {src:?}"
        );
        // Every `Move` in lowered IR corresponds to a recorded use-site,
        // and every recorded use-site lowers to exactly one `Move`.
        let mut moved_sorted: Vec<NodeId> = model.moved_uses.iter().copied().collect();
        moved_sorted.sort_by_key(|id| id.0);
        let mut seen: HashSet<NodeId> = HashSet::new();
        for (id, access) in uses.iter().zip(accesses.iter()) {
            assert!(seen.insert(*id), "duplicate use-site {id:?} in {src:?}");
            let recorded = model.moved_uses.contains(id);
            assert_eq!(
                *access == LocalAccess::Move,
                recorded,
                "authority mismatch at {id:?} in {src:?}: lowered {access:?}, recorded={recorded}"
            );
        }
        for id in &moved_sorted {
            assert!(
                uses.contains(id),
                "recorded move {id:?} has no lowered use-site in {src:?}"
            );
        }
        let move_count = accesses.iter().filter(|a| **a == LocalAccess::Move).count();
        assert_eq!(
            move_count,
            model.moved_uses.len(),
            "Move count != recorded set size in {src:?}"
        );
    }
}

#[test]
fn move_facts_preserved_for_canonical_programs() {
    // Behavior preservation: the recorded facts (and hence IR decisions)
    // are exactly the historical move sites.
    let src = "fn main() { let a = \"hi\"; let b = a; do b; }";
    let (parse, model, diags) = parse_check(src);
    assert!(diags.is_empty(), "sema diags: {diags:?}");
    assert_eq!(
        model.moved_uses.len(),
        2,
        "expected let-init + return moves"
    );
    let uses = ident_uses_in_order(&parse);
    assert_eq!(
        uses.len(),
        2,
        "expected exactly two ident uses, got {uses:?}"
    );
    for id in uses {
        assert!(
            model.moved_uses.contains(&id),
            "use-site {id:?} not recorded"
        );
    }
    let m = lower(&parse, &model, FileId(0), src.len() as u32)
        .unwrap_or_else(|diags| panic!("lower failed: {diags:?}"));
    // Byte-identical diagnostics path: clean sema stays clean.
    validate(&m).expect("clean module must validate");
}

fn locals_in_order(m: &TypedModule) -> Vec<(String, LocalAccess)> {
    fn expr(e: &TypedExpr, out: &mut Vec<(String, LocalAccess)>) {
        match &e.kind {
            ExprKind::Local { name, access } => out.push((name.clone(), *access)),
            ExprKind::Unary { rhs, .. } => expr(rhs, out),
            ExprKind::Binary { lhs, rhs, .. } => {
                expr(lhs, out);
                expr(rhs, out);
            }
            ExprKind::Call { args, .. } => {
                for a in args {
                    expr(a, out);
                }
            }
            _ => {}
        }
    }
    fn block(b: &TypedBlock, out: &mut Vec<(String, LocalAccess)>) {
        for st in &b.stmts {
            match st {
                TypedStmt::Let { init, .. } => expr(init, out),
                TypedStmt::Do { expr: e } => expr(e, out),
                TypedStmt::Assert { expr: e, .. } => expr(e, out),
                TypedStmt::AssertFalse { .. } => {}
                TypedStmt::Block { body } => block(body, out),
                TypedStmt::Empty => {}
            }
        }
    }
    let mut out = Vec::new();
    for f in &m.functions {
        block(&f.body, &mut out);
    }
    out
}

#[test]
fn lowered_shadowed_body_return_is_move() {
    // The new body local is the returned Str. The already-moved parameter
    // must not keep that use site as Copy.
    let src = "fn main(s) { let s = s; do s; } fn caller() { let t = \"hi\"; do main(t); }";
    let (m, _) = lower_clean(src);
    assert_eq!(
        locals_in_order(&m),
        vec![
            ("s".to_string(), LocalAccess::Move),
            ("s".to_string(), LocalAccess::Move),
            ("t".to_string(), LocalAccess::Move),
        ]
    );
    validate(&m).expect("shadowed return must validate");
}

#[test]
fn lowered_nested_trailing_do_stays_copy() {
    let src = "fn main() { { let s = \"hi\"; do s; } do \"ok\"; }";
    let (parse, model, diags) = parse_check(src);
    assert!(diags.is_empty(), "{diags:?}");
    assert!(
        model.moved_uses.is_empty(),
        "nested trailing do is not a return move"
    );
    let (m, _) = lower_clean(src);
    assert_eq!(
        locals_in_order(&m),
        vec![("s".to_string(), LocalAccess::Copy)]
    );
    let _ = parse;
    validate(&m).expect("nested copy must validate");
}

#[test]
fn lowered_root_function_name_stays_copy() {
    // `do id` resolves to the root function in sema, so it is not a
    // return move. Lowering's local scopes do not contain function
    // names, so the same use is an unbound-name diagnostic and the
    // placeholder stamp stays Copy.
    let src = "fn id(s) { do s == \"x\"; do s; } fn main() { do id; }";
    let (parse, model, diags) = parse_check(src);
    assert!(diags.is_empty(), "{diags:?}");
    let uses = ident_uses_in_order(&parse);
    let id_use = *uses.last().expect("do id");
    assert!(
        !model.moved_uses.contains(&id_use),
        "root function name must not be recorded"
    );
    assert_eq!(model.moved_uses.len(), 1, "only id's parameter return");
    let Err(ldiags) = lower(&parse, &model, FileId(0), src.len() as u32) else {
        panic!("expected lowering failure")
    };
    assert!(
        ldiags
            .iter()
            .any(|d| d.message.contains("unbound name `id`")),
        "lowering has no function-value slot: {ldiags:?}"
    );
}

#[test]
fn lowered_assert_true_does_not_invent_a_return_move() {
    let src = "fn main() { let s = \"hi\"; do s; assert true; }";
    let (m, _) = lower_flags(src);
    let main = &m.functions[0];
    assert_eq!(main.ret, Ty::Unit);
    assert_eq!(main.body.value, Ty::Str);
    assert_eq!(
        locals_in_order(&m),
        vec![("s".to_string(), LocalAccess::Copy)]
    );
    let ds = match validate(&m) {
        Ok(_) => panic!("value/ret mismatch must not validate"),
        Err(ds) => ds,
    };
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert!(
        ds[0].notes.iter().any(|n| n.contains("WF3")),
        "notes: {:?}",
        ds[0].notes
    );
    assert!(ds[0].notes.iter().all(|n| !n.contains("WF9")));
}

#[test]
fn module_new_compat() {
    let m = Module::new("demo", ScopeId(1000));
    assert_eq!(m.name, "demo");
    assert!(m.functions.is_empty());
    let tm = TypedModule::new("demo", ScopeId(1000));
    assert_eq!(tm, m);
}

#[test]
fn lowering_publishes_module_function_and_block_scope_identities() {
    let src = "fn first() { do 1; } fn second(x) { do x + 1; { do x; } }";
    let (parse, model, diags) = parse_check(src);
    assert!(diags.is_empty(), "sema: {diags:?}");
    let module = lower(&parse, &model, FileId(0), src.len() as u32)
        .unwrap_or_else(|ds| panic!("lower: {ds:?}"));
    let roots: Vec<_> = model
        .scope_tree
        .scopes
        .iter()
        .enumerate()
        .filter(|(_, scope)| scope.parent.is_none())
        .map(|(index, _)| ScopeId(index as u32))
        .collect();
    assert_eq!(roots, [module.scope]);
    assert_eq!(module.functions.len(), 2);
    let mut seen = HashSet::from([module.scope]);
    for (ir_fn, ids) in module.functions.iter().zip(&parse.index.items) {
        let ItemIds::Fn(ids) = ids else {
            panic!("function index")
        };
        let body_scope = model.block_scopes[&ids.body.node];
        assert_eq!(
            model.scope_tree.scopes[body_scope.0 as usize].parent,
            Some(ir_fn.scope)
        );
        assert_eq!(
            model.scope_tree.scopes[ir_fn.scope.0 as usize].parent,
            Some(module.scope)
        );
        assert!(seen.insert(ir_fn.scope), "duplicate function scope");
        assert!(seen.insert(ir_fn.body.scope), "duplicate body scope");
        fn add_nested(block: &TypedBlock, seen: &mut HashSet<ScopeId>) {
            for stmt in &block.stmts {
                if let TypedStmt::Block { body } = stmt {
                    assert!(seen.insert(body.scope), "duplicate nested block scope");
                    add_nested(body, seen);
                }
            }
        }
        add_nested(&ir_fn.body, &mut seen);
    }
}

#[test]
fn empty_module_uses_the_unique_semantic_root() {
    let src = "";
    let (parse, model, diags) = parse_check(src);
    assert!(diags.is_empty());
    let module = lower(&parse, &model, FileId(0), 0).expect("empty module lowers");
    assert!(module.functions.is_empty());
    assert_eq!(module.scope, ScopeId(0));
}

#[test]
fn lowering_rejects_missing_or_multiple_module_roots() {
    let src = "fn main() { do 1; }";
    let (parse, model, diags) = parse_check(src);
    assert!(diags.is_empty());
    let mut missing = model.clone();
    missing.scope_tree.scopes.clear();
    let Err(ds) = lower(&parse, &missing, FileId(0), src.len() as u32) else {
        panic!("missing module root must fail")
    };
    assert!(ds.iter().any(|d| {
        d.code == ErrorCode::InternalIrInvalid
            && d.message == "scope tree has no parentless module scope"
    }));
    let mut multiple = model;
    multiple.scope_tree.scopes.push(Default::default());
    let Err(ds) = lower(&parse, &multiple, FileId(0), src.len() as u32) else {
        panic!("multiple roots must fail")
    };
    assert!(ds.iter().any(|d| d.message.contains("multiple parentless")));
}

#[test]
fn lowering_rejects_invalid_and_inconsistent_scope_relationships() {
    let src = "fn main() { { do 1; } }";
    let (parse, model, diags) = parse_check(src);
    assert!(diags.is_empty());
    let ItemIds::Fn(ids) = &parse.index.items[0] else {
        panic!("function")
    };

    let mut missing_mapping = model.clone();
    missing_mapping.block_scopes.remove(&ids.body.node);
    assert!(lower(&parse, &missing_mapping, FileId(0), src.len() as u32).is_err());

    let mut invalid_index = model.clone();
    invalid_index
        .block_scopes
        .insert(ids.body.node, ScopeId(u32::MAX));
    assert!(lower(&parse, &invalid_index, FileId(0), src.len() as u32).is_err());

    let mut missing_body_parent = model.clone();
    let body_scope = missing_body_parent.block_scopes[&ids.body.node];
    missing_body_parent.scope_tree.scopes[body_scope.0 as usize].parent = None;
    assert!(lower(&parse, &missing_body_parent, FileId(0), src.len() as u32).is_err());

    let mut wrong_parameter_parent = model;
    let function_scope = wrong_parameter_parent.scope_tree.scopes[body_scope.0 as usize]
        .parent
        .expect("parameter scope");
    wrong_parameter_parent.scope_tree.scopes[function_scope.0 as usize].parent = Some(body_scope);
    let Err(ds) = lower(&parse, &wrong_parameter_parent, FileId(0), src.len() as u32) else {
        panic!("inconsistent parameter parent must fail")
    };
    assert!(ds
        .iter()
        .any(|d| d.message.contains("not under module scope")));
}

#[test]
fn lowering_rejects_dangling_scope_parent_references() {
    let src = "fn main() { do 1; }";
    let (parse, mut model, diags) = parse_check(src);
    assert!(diags.is_empty());
    let ItemIds::Fn(ids) = &parse.index.items[0] else {
        panic!("function")
    };
    let body_scope = model.block_scopes[&ids.body.node];
    model.scope_tree.scopes[body_scope.0 as usize].parent = Some(ScopeId(u32::MAX));
    let Err(ds) = lower(&parse, &model, FileId(0), src.len() as u32) else {
        panic!("dangling scope parent must fail")
    };
    assert!(ds.iter().any(|d| {
        d.code == ErrorCode::InternalIrInvalid
            && d.message == "internal scope mismatch: function scope id is invalid"
    }));
}

#[test]
fn lowering_rejects_duplicate_scope_ids_for_distinct_sibling_blocks() {
    let src = "fn main() { { do 1; } { do 2; } }";
    let (parse, mut model, diags) = parse_check(src);
    assert!(diags.is_empty());
    let ItemIds::Fn(ids) = &parse.index.items[0] else {
        panic!("function")
    };
    let (StmtIds::Block { body: first, .. }, StmtIds::Block { body: second, .. }) =
        (&ids.body.stmts[0], &ids.body.stmts[1])
    else {
        panic!("expected sibling blocks")
    };
    let first_scope = model.block_scopes[&first.node];
    model.block_scopes.insert(second.node, first_scope);
    let Err(ds) = lower(&parse, &model, FileId(0), src.len() as u32) else {
        panic!("duplicate block scope must fail")
    };
    assert!(ds.iter().any(|d| {
        d.code == ErrorCode::InternalIrInvalid
            && d.message == "internal scope mismatch: duplicate block scope id"
    }));
}

#[test]
fn lowering_rejects_nested_block_with_wrong_valid_parent() {
    let src = "fn main() { { do 1; } }";
    let (parse, mut model, diags) = parse_check(src);
    assert!(diags.is_empty());
    let ItemIds::Fn(ids) = &parse.index.items[0] else {
        panic!("function")
    };
    let StmtIds::Block { body: nested, .. } = &ids.body.stmts[0] else {
        panic!("nested block")
    };
    let function_scope = model.scope_tree.scopes[model.block_scopes[&ids.body.node].0 as usize]
        .parent
        .expect("function scope");
    let nested_scope = model.block_scopes[&nested.node];
    // This is a valid scope ID, but it gives the nested block the
    // function scope as parent instead of its enclosing body block.
    model.scope_tree.scopes[nested_scope.0 as usize].parent = Some(function_scope);
    let Err(ds) = lower(&parse, &model, FileId(0), src.len() as u32) else {
        panic!("wrong nested-block parent must fail")
    };
    assert!(ds.iter().any(|d| {
        d.code == ErrorCode::InternalIrInvalid
            && d.message == "internal scope mismatch: block scope has an inconsistent parent"
    }));
}

#[test]
fn lowered_blocks_match_published_scope_per_node() {
    let src = "fn left() { { do 1; } { do 2; } } \
               fn main() { let x = 1; assert true; { do x; { do x; } } do x; }";
    let (parse, model, diags) = parse_check_flags(src, assert_flags());
    assert!(diags.is_empty(), "sema diags: {diags:?}");
    let module = lower(&parse, &model, FileId(0), src.len() as u32)
        .unwrap_or_else(|diags| panic!("lower failed: {diags:?}"));
    assert_eq!(parse.program.items.len(), parse.index.items.len());

    let mut fns = module.functions.iter();
    for (item, iids) in parse.program.items.iter().zip(parse.index.items.iter()) {
        let (aion_syntax::Item::Fn(f), ItemIds::Fn(fids)) = (item, iids) else {
            continue;
        };
        let tf = fns.next().expect("lower dropped a function");
        assert_eq!(tf.name, f.name);
        pair_block(&f.body, &fids.body, &tf.body, &model);
    }
    assert!(fns.next().is_none(), "lower invented a function");

    let ItemIds::Fn(left) = &parse.index.items[0] else {
        panic!("left")
    };
    let sibs: Vec<_> = left
        .body
        .stmts
        .iter()
        .filter_map(|s| {
            if let StmtIds::Block { body, .. } = s {
                Some(body.node)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(sibs.len(), 2);
    assert_ne!(model.block_scopes[&sibs[0]], model.block_scopes[&sibs[1]]);
}

fn pair_block(ast: &Block, ids: &BlockIds, ir: &TypedBlock, model: &aion_sem_model::SemModel) {
    let published = model.block_scopes[&ids.node];
    assert_eq!(
        ir.scope, published,
        "block {:?} has the wrong published scope",
        ids.node
    );
    assert_eq!(
        ast.stmts.len(),
        ids.stmts.len(),
        "block {:?} AST/index statement counts differ",
        ids.node
    );
    let mut ir_stmts = ir.stmts.iter();
    for (st, sid) in ast.stmts.iter().zip(ids.stmts.iter()) {
        if matches!(
            st,
            Stmt::Assert {
                expr: Expr::Bool(true),
                ..
            }
        ) {
            assert!(
                matches!(sid, StmtIds::Assert { .. }),
                "assert true paired with a non-assert index"
            );
            continue;
        }
        if matches!((st, sid), (Stmt::Goal(_), StmtIds::Goal { .. })) {
            panic!("goal statements are not lowered to IR; refusing to consume the next statement");
        }
        let got = ir_stmts.next().expect("IR dropped a statement");
        match (st, sid, got) {
            (Stmt::Block(inner), StmtIds::Block { body, .. }, TypedStmt::Block { body: tb }) => {
                pair_block(inner, body, tb, model);
            }
            (Stmt::Let { .. }, StmtIds::Let { .. }, TypedStmt::Let { .. }) => {}
            (Stmt::Do(_), StmtIds::Do { .. }, TypedStmt::Do { .. }) => {}
            (Stmt::Empty, StmtIds::Empty(_), TypedStmt::Empty) => {}
            (
                Stmt::Assert {
                    expr: Expr::Bool(false),
                    ..
                },
                StmtIds::Assert { .. },
                TypedStmt::AssertFalse { .. },
            ) => {}
            (Stmt::Assert { .. }, StmtIds::Assert { .. }, TypedStmt::Assert { .. }) => {}
            other => panic!("AST/IR shape diverged: {other:?}"),
        }
    }
    assert!(ir_stmts.next().is_none(), "IR invented a statement");
}

#[test]
fn statement_count_mismatch_omits_function_and_keeps_scope_stack() {
    // `leaked + 0` grounds the parameter so the E0014 gate does not
    // abort lowering before the mismatch. The nested index is cleared
    // after sema; its block node and published scope stay.
    let src = "fn bad(leaked) { let hidden = 1; do leaked + 0; { do hidden; } } \
               fn main() { let x = 1; do x; do x; }";
    let (mut parse, model, diags) = parse_check(src);
    assert!(diags.is_empty(), "sema diags: {diags:?}");

    let ItemIds::Fn(bad_ids) = &mut parse.index.items[0] else {
        panic!("bad")
    };
    let StmtIds::Block { body, .. } = &mut bad_ids.body.stmts[2] else {
        panic!("nested block");
    };
    body.stmts.clear();

    let Item::Fn(main_ast) = &mut parse.program.items[1] else {
        panic!("main")
    };
    rename_ident(&mut main_ast.body.stmts[1], "leaked");
    rename_ident(&mut main_ast.body.stmts[2], "hidden");

    let Err(ldiags) = lower(&parse, &model, FileId(0), src.len() as u32) else {
        panic!("expected malformed-index failure")
    };
    assert!(ldiags.iter().any(|d| {
        d.code == ErrorCode::InternalIrInvalid
            && d.code.code_str() == "AION-E0030"
            && d.message == "internal index mismatch: block statement count differs from index"
    }));
    assert!(ldiags.iter().all(|d| d.code != ErrorCode::UnresolvedType));
    assert!(ldiags
        .iter()
        .all(|d| !d.message.contains("block scope missing")));
    assert!(ldiags
        .iter()
        .any(|d| d.message.contains("unbound name `leaked`")));
    assert!(ldiags
        .iter()
        .any(|d| d.message.contains("unbound name `hidden`")));
}

fn rename_ident(st: &mut Stmt, name: &str) {
    let Stmt::Do(Expr::Ident(got)) = st else {
        panic!("expected do ident")
    };
    *got = name.to_string();
}

#[test]
fn lowered_module_arena_table_is_empty() {
    let src = "fn main() { let s = \"hi\"; { do s; } do s; }";
    let (module, _) = lower_flags(src);
    assert!(
        !module.functions.is_empty(),
        "lowering must produce the function"
    );
    let (allocs, scopes) = lowered_allocs_and_scopes(&module);
    assert_eq!(allocs.len(), 3, "literal plus two uses, got {allocs:?}");
    assert_eq!(
        scopes.len(),
        2,
        "function body plus nested block, got {scopes:?}"
    );
    assert!(
        allocs.iter().all(|a| *a == AllocKind::Refcounted),
        "string nodes must stay refcounted: {allocs:?}"
    );
    assert!(module.arenas.is_empty());
    assert_eq!(module.arenas.len(), 0);
}
