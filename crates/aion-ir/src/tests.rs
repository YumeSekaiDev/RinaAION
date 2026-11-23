//! M3.2 lowering tests: shape, access kinds, E0014 gate, span preservation.
use super::*;
use aion_span::FileId;

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
    let (m, ldiags) = lower(&parse, &model, FileId(0), src.len() as u32);
    assert!(ldiags.is_empty(), "lower diags: {:?}", ldiags);
    (m, parse)
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
    let (m, ldiags) = lower(&parse, &model, FileId(0), src.len() as u32);
    assert!(m.functions.is_empty());
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
        lower, validate, ExprKind, LocalAccess, TypedBlock, TypedExpr, TypedFn, TypedModule,
        TypedStmt,
    };
    use aion_error::ErrorCode;
    use aion_sem_model::Ty;
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
        }
    }

    fn str_lit(v: &str, s: u32, e: u32) -> TypedExpr {
        TypedExpr {
            ty: Ty::Str,
            span: sp(s, e),
            kind: ExprKind::Str(v.to_string()),
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
        }
    }

    fn mk_fn(
        name: &str,
        params: Vec<(String, Ty)>,
        ret: Ty,
        stmts: Vec<TypedStmt>,
        value: Ty,
    ) -> TypedFn {
        TypedFn {
            name: name.to_string(),
            decl_span: sp(0, 9),
            params,
            ret,
            body: TypedBlock { stmts, value },
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
        let mut m = TypedModule::new("t");
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
    fn wf1_unbound_slot() {
        let mut m = TypedModule::new("t");
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
        let mut m = TypedModule::new("t");
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
        assert_single_e0030(&check(&m));
    }

    #[test]
    fn wf3_ret_mismatch() {
        // Declares `-> int` but the body yields `()`.
        let mut m = TypedModule::new("t");
        m.functions
            .push(mk_fn("main", vec![], Ty::Int, vec![], Ty::Unit));
        let ds = check(&m);
        assert_eq!(ds.len(), 1, "got {ds:?}");
        assert_eq!(ds[0].code.code_str(), "AION-E0030");
    }

    #[test]
    fn wf4_operator_mismatch() {
        // `1 + 2` recorded as Bool.
        let mut m = TypedModule::new("t");
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
        let mut m = TypedModule::new("t");
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
        m.functions.push(mk_fn(
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
        };
        let mut m = TypedModule::new("t");
        m.functions
            .push(mk_fn("main", vec![], Ty::Int, vec![do_stmt(e)], Ty::Int));
        assert_single_e0030(&check(&m));
    }

    #[test]
    fn wf6_dangling_callee() {
        let mut m = TypedModule::new("t");
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
        let mut m = TypedModule::new("t");
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
        // Covered by WF2 (any use after move); must still be exactly one E0030.
        let mut m = TypedModule::new("t");
        m.functions.push(mk_fn(
            "main",
            vec![],
            Ty::Str,
            vec![
                let_stmt("s", Ty::Str, str_lit("a", 1, 4)),
                do_stmt(local("s", Ty::Str, LocalAccess::Move, 5, 6)),
                do_stmt(local("s", Ty::Str, LocalAccess::Move, 7, 8)),
            ],
            Ty::Str,
        ));
        assert_single_e0030(&check(&m));
    }

    #[test]
    fn wf8_duplicate_function_names_rejected() {
        // Hand-built IR with two `main`s (sema would emit AION-E0013 on
        // the real pipeline, so this only arises adversarially) must
        // fail validation — dup mains must never silently execute.
        let mut m = TypedModule::new("t");
        m.functions.push(mk_fn(
            "main",
            vec![],
            Ty::Int,
            vec![do_stmt(int_lit(1, 1, 2))],
            Ty::Int,
        ));
        let mut second = mk_fn(
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
        let ds = check(&TypedModule::new("empty"));
        assert!(ds.is_empty());
        // Deep nesting: no panic.
        let mut deep = int_lit(1, 1, 2);
        for i in 0..500u32 {
            deep = unary(UnOp::Neg, deep, Ty::Int, 1, 10 + i);
        }
        let mut m = TypedModule::new("deep");
        m.functions
            .push(mk_fn("main", vec![], Ty::Int, vec![do_stmt(deep)], Ty::Int));
        let _ = validate(&m);
        // Garbage types and empty blocks: no panic.
        let mut g = TypedModule::new("garbage");
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

    #[test]
    fn lowered_clean_module_validates() {
        // End to end: real lowering output passes the validator.
        let parse = aion_syntax::parse(FileId(0), b"fn main() { let x = 1 + 2; do x; }");
        assert!(parse.errors.is_empty());
        let (diags, model) =
            aion_sema::check_with_model(&parse, FileId(0), b"fn main() { let x = 1 + 2; do x; }");
        assert!(diags.is_empty());
        let src = b"fn main() { let x = 1 + 2; do x; }";
        let (m, ldiags) = lower(&parse, &model, FileId(0), src.len() as u32);
        assert!(ldiags.is_empty());
        validate(&m).expect("clean module must validate");
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
                (Stmt::Assert(ex), StmtIds::Assert { expr: eids, .. }) => expr(ex, eids, out),
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
        let (m, ldiags) = lower(&parse, &model, FileId(0), src.len() as u32);
        assert!(ldiags.is_empty(), "lower diags for {src:?}: {ldiags:?}");
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
    let (m, ldiags) = lower(&parse, &model, FileId(0), src.len() as u32);
    assert!(ldiags.is_empty(), "lower diags: {ldiags:?}");
    // Byte-identical diagnostics path: clean sema stays clean.
    validate(&m).expect("clean module must validate");
}

#[test]
fn module_new_compat() {
    let m = Module::new("demo");
    assert_eq!(m.name, "demo");
    assert!(m.functions.is_empty());
    let tm = TypedModule::new("demo");
    assert_eq!(tm, m);
}
