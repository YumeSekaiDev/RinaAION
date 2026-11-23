//! M3.4 interpreter tests: per-trap codes + spans, boundaries,
//! fuel determinism, recursion cap, moves, entry, no-output.

use super::*;
use aion_error::AssertProvenance;
use aion_ir::{TypedFn, TypedModule, ValidatedModule};
use aion_sem_model::Ty;
use aion_span::{ByteSpan, FileId};

fn sp(s: u32, e: u32) -> ByteSpan {
    ByteSpan {
        file: FileId(0),
        start: s,
        end: e,
    }
}

fn expr(ty: Ty, s: u32, e: u32, kind: ExprKind) -> TypedExpr {
    TypedExpr {
        ty,
        span: sp(s, e),
        kind,
    }
}

fn int(v: i64, s: u32, e: u32) -> TypedExpr {
    expr(Ty::Int, s, e, ExprKind::Int(v))
}

fn boolean(v: bool, s: u32, e: u32) -> TypedExpr {
    expr(Ty::Bool, s, e, ExprKind::Bool(v))
}

fn main_fn(body: TypedBlock) -> TypedModule {
    // `ret` mirrors the body value (a real pipeline always agrees);
    // execution discards it and yields `Unit` regardless.
    let ret = body.value.clone();
    TypedModule {
        name: String::new(),
        eof_span: sp(0, 0),
        functions: vec![TypedFn {
            name: "main".to_string(),
            decl_span: sp(0, 9),
            params: vec![],
            ret,
            body,
        }],
    }
}

/// The only way to reach `execute`: validation must succeed first.
/// (There is no overload taking a bare `TypedModule`, so unvalidated
/// IR cannot be executed — this helper is the single choke point.)
fn validated(m: &TypedModule) -> ValidatedModule {
    aion_ir::validate(m).unwrap_or_else(|ds| {
        panic!(
            "expected clean validation, got {:?}",
            ds.iter().map(|d| d.render_human()).collect::<Vec<_>>()
        )
    })
}

fn run_ok(m: &TypedModule) -> Outcome {
    match execute(&validated(m), &Config::default()) {
        RunResult::Ok(o) => o,
        RunResult::Trap(d) => panic!("unexpected trap {d:?}"),
    }
}

fn do_block(e: TypedExpr, value: Ty) -> TypedBlock {
    TypedBlock {
        stmts: vec![aion_ir::TypedStmt::Do { expr: e }],
        value,
    }
}

fn trap_of(r: RunResult) -> Diagnostic {
    match r {
        RunResult::Trap(d) => d,
        RunResult::Ok(o) => panic!("expected trap, got {o:?}"),
    }
}

fn assert_block(e: TypedExpr, span: ByteSpan) -> TypedBlock {
    TypedBlock {
        stmts: vec![aion_ir::TypedStmt::Assert {
            expr: e,
            span,
            provenance: AssertProvenance::human_authored(),
        }],
        value: Ty::Unit,
    }
}

#[test]
fn assert_true_succeeds_silently() {
    let m = main_fn(assert_block(boolean(true, 11, 15), sp(4, 16)));
    match execute(&validated(&m), &Config::default()) {
        RunResult::Ok(o) => assert_eq!(o.value, Value::Unit),
        RunResult::Trap(d) => panic!("unexpected trap {d:?}"),
    }
}

#[test]
fn assert_false_traps_e0044_with_span_and_provenance() {
    let stmt_span = sp(4, 17);
    let m = main_fn(assert_block(boolean(false, 11, 16), stmt_span));
    let d = trap_of(execute(&validated(&m), &Config::default()));
    assert_eq!(d.code, ErrorCode::RuntimeTrap);
    assert_eq!(d.code.code_str(), "AION-E0044");
    assert_eq!(d.message, "assertion failed");
    assert_eq!(d.severity, aion_error::Severity::Error);
    assert_eq!(d.span, Some(stmt_span));
    assert!(d.notes.is_empty() && d.related.is_empty());
    assert_eq!(d.provenance, Some(AssertProvenance::human_authored()));
}

#[test]
fn assert_operand_fuel_exhaustion_precedes_e0044() {
    // Operand `1+2` needs 3 fuel; with 2 the evaluation traps before
    // E0044 is ever considered.
    let cfg = Config {
        fuel: 2,
        frames: 1024,
    };
    let operand = expr(
        Ty::Int,
        0,
        5,
        ExprKind::Binary {
            op: BinOp::Add,
            lhs: Box::new(int(1, 0, 1)),
            rhs: Box::new(int(2, 4, 5)),
        },
    );
    let m = main_fn(assert_block(operand, sp(0, 5)));
    let d = trap_of(execute(&validated(&m), &cfg));
    assert_eq!(d.code, ErrorCode::FuelExhausted);
    assert_eq!(d.code.code_str(), "AION-E0042");
}

/// M6a folded-false fixture through parse → lower → validate.
/// Hand-built scope model (mirrors the oracle harness): types are fully
/// known, so this exercises the real lowering path for the fixture.
fn folded_false_fixture_module() -> ValidatedModule {
    let src = std::fs::read(format!(
        "{}/../../tests/fixtures/m4_4/trap/assert_folded_false.aion",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("fixture readable");
    let text = String::from_utf8_lossy(&src).into_owned();
    let parse = aion_syntax::parse_with_flags(
        FileId(0),
        &src,
        aion_syntax::FeatureFlags::all_experimental(),
    );
    assert!(parse.errors.is_empty(), "fixture must parse clean");
    let mut tree = aion_sem_model::ScopeTree::new();
    let root = tree.root();
    assert!(tree.define(
        root,
        aion_sem_model::Symbol {
            kind: aion_sem_model::SymbolKind::Fn,
            name: "main".to_string(),
            ty: Ty::Fn {
                params: vec![],
                ret: Box::new(Ty::Unit),
            },
            span: ByteSpan::empty(FileId(0), 0),
            moved: false,
        },
    ));
    let model = aion_sem_model::SemModel {
        items: Vec::new(),
        scope_tree: tree,
        types: std::collections::HashMap::new(),
        moved_uses: std::collections::HashSet::new(),
        ..Default::default()
    };
    let (module, diags) = aion_ir::lower(&parse, &model, FileId(0), text.len() as u32);
    assert!(diags.is_empty(), "must lower clean");
    validated(&module)
}

#[test]
fn assert_trap_trace_matches_golden_and_is_deterministic() {
    // T14 + determinism: two runs produce identical bytes, matching
    // the reviewed golden — a single trap event with the fixture's
    // assert statement span and no operand evaluation events.
    let golden =
        include_bytes!("../../../tests/golden/m4/canonical/trace_assert_trap.json").to_vec();
    for _ in 0..2 {
        let validated_m = folded_false_fixture_module();
        let (r, bytes) = trace::with_observer(|| execute(&validated_m, &Config::default()));
        assert!(
            matches!(r, RunResult::Trap(_)),
            "folded false must trap, got {r:?}"
        );
        assert_eq!(
            bytes.as_slice(),
            &golden[..],
            "trace bytes must match the golden"
        );
    }
    let text = String::from_utf8(golden).expect("golden is utf-8");
    assert!(
        text.contains("\"code\":\"AION-E0044\""),
        "trap code pinned: {text}"
    );
    assert!(
        text.contains("\"start\":12") && text.contains("\"end\":25"),
        "fixture statement span pinned: {text}"
    );
    assert!(
        !text.contains("\"provenance\""),
        "no provenance in trace events: {text}"
    );
}

fn assert_false_block(span: ByteSpan) -> TypedBlock {
    TypedBlock {
        stmts: vec![aion_ir::TypedStmt::AssertFalse {
            span,
            provenance: AssertProvenance::human_authored(),
        }],
        value: Ty::Unit,
    }
}

#[test]
fn folded_false_traps_without_evaluation_or_fuel() {
    // Zero fuel: any operand evaluation or fuel consultation would
    // trap FuelExhausted instead. The folded path uses neither.
    let stmt_span = sp(4, 18);
    let m = main_fn(assert_false_block(stmt_span));
    let cfg = Config {
        fuel: 0,
        frames: 1024,
    };
    let d = trap_of(execute(&validated(&m), &cfg));
    assert_eq!(d.code, ErrorCode::RuntimeTrap);
    assert_eq!(d.code.code_str(), "AION-E0044");
    assert_eq!(d.message, "assertion failed");
    assert_eq!(d.severity, aion_error::Severity::Error);
    assert_eq!(d.span, Some(stmt_span));
    assert!(d.notes.is_empty() && d.related.is_empty());
    assert_eq!(d.provenance, Some(AssertProvenance::human_authored()));
}

#[test]
fn folded_false_trap_is_recorded_with_statement_span() {
    let stmt_span = sp(4, 18);
    let m = main_fn(assert_false_block(stmt_span));
    let validated_m = validated(&m);
    let (r, bytes) = trace::with_observer(|| execute(&validated_m, &Config::default()));
    let d = trap_of(r);
    assert_eq!(d.code.code_str(), "AION-E0044");
    let s = String::from_utf8(bytes).expect("trace bytes are utf-8");
    assert!(s.contains("\"kind\":\"trap\""), "trap event recorded: {s}");
    assert!(s.contains("\"code\":\"AION-E0044\""), "trap code: {s}");
    assert!(
        s.contains("\"start\":4") && s.contains("\"end\":18"),
        "statement span: {s}"
    );
}

#[test]
fn assert_false_trap_is_recorded_with_statement_span() {
    let stmt_span = sp(4, 17);
    let m = main_fn(assert_block(boolean(false, 11, 16), stmt_span));
    let validated_m = validated(&m);
    let (r, bytes) = trace::with_observer(|| execute(&validated_m, &Config::default()));
    let d = trap_of(r);
    assert_eq!(d.code.code_str(), "AION-E0044");
    let s = String::from_utf8(bytes).expect("trace bytes are utf-8");
    assert!(s.contains("\"kind\":\"trap\""), "trap event recorded: {s}");
    assert!(s.contains("\"code\":\"AION-E0044\""), "trap code: {s}");
    assert!(
        s.contains("\"start\":4") && s.contains("\"end\":17"),
        "statement span: {s}"
    );
}

#[test]
fn entry_missing_is_e0015() {
    let mut m = TypedModule::new("x");
    m.eof_span = ByteSpan::empty(FileId(0), 17);
    let d = trap_of(execute(&validated(&m), &Config::default()));
    assert_eq!(d.code, ErrorCode::EntryPointInvalid);
    assert_eq!(d.code.code_str(), "AION-E0015");
    assert_eq!(
        d.span,
        Some(ByteSpan::empty(FileId(0), 17)),
        "missing main must pin the end-of-input span"
    );
}

/// Bad-signature `main` pins its declaration span (E0015).
#[test]
fn entry_with_params_pins_decl_span() {
    let mut m = TypedModule::new("x");
    m.eof_span = ByteSpan::empty(FileId(0), 40);
    m.functions.push(TypedFn {
        name: "main".to_string(),
        decl_span: sp(0, 14),
        params: vec![("a".to_string(), Ty::Int)],
        ret: Ty::Unit,
        body: TypedBlock {
            stmts: vec![],
            value: Ty::Unit,
        },
    });
    let d = trap_of(execute(&validated(&m), &Config::default()));
    assert_eq!(d.code, ErrorCode::EntryPointInvalid);
    assert_eq!(d.code.code_str(), "AION-E0015");
    assert_eq!(
        d.span,
        Some(sp(0, 14)),
        "bad-signature main must pin its decl span, not the body or EOF"
    );
}

/// Only `ValidatedModule` reaches `execute`: documenting the
/// compile-time gate (a bare `TypedModule` has no matching overload).
#[test]
fn only_validated_modules_execute() {
    let m = main_fn(do_block(int(1, 0, 1), Ty::Int));
    let v = validated(&m);
    assert!(matches!(execute(&v, &Config::default()), RunResult::Ok(_)));
}

#[test]
fn entry_with_params_is_e0015() {
    let m = TypedModule {
        name: String::new(),
        eof_span: sp(0, 0),
        functions: vec![TypedFn {
            name: "main".to_string(),
            decl_span: sp(0, 14),
            params: vec![("a".to_string(), Ty::Int)],
            ret: Ty::Unit,
            body: TypedBlock {
                stmts: vec![],
                value: Ty::Unit,
            },
        }],
    };
    let d = trap_of(execute(&validated(&m), &Config::default()));
    assert_eq!(d.code, ErrorCode::EntryPointInvalid);
}

#[test]
fn unit_body_ok_and_discards() {
    // `do 1;` trailing value Int but main discards -> Unit.
    let m = main_fn(do_block(int(1, 0, 1), Ty::Int));
    match execute(&validated(&m), &Config::default()) {
        RunResult::Ok(o) => {
            assert_eq!(o.value, Value::Unit);
            assert!(o.steps_used >= 1);
        }
        RunResult::Trap(d) => panic!("unexpected trap {d:?}"),
    }
}

#[test]
fn div_by_zero_traps_e0040_with_span() {
    let op = expr(
        Ty::Int,
        0,
        5,
        ExprKind::Binary {
            op: BinOp::Div,
            lhs: Box::new(int(1, 0, 1)),
            rhs: Box::new(int(0, 4, 5)),
        },
    );
    let m = main_fn(do_block(op, Ty::Int));
    let d = trap_of(execute(&validated(&m), &Config::default()));
    assert_eq!(d.code, ErrorCode::DivByZero);
    assert_eq!(d.code.code_str(), "AION-E0040");
    assert_eq!(d.span, Some(sp(0, 5)));
}

#[test]
fn mod_by_zero_traps_e0040() {
    let op = expr(
        Ty::Int,
        2,
        7,
        ExprKind::Binary {
            op: BinOp::Mod,
            lhs: Box::new(int(5, 2, 3)),
            rhs: Box::new(int(0, 6, 7)),
        },
    );
    let m = main_fn(do_block(op, Ty::Int));
    let d = trap_of(execute(&validated(&m), &Config::default()));
    assert_eq!(d.code, ErrorCode::DivByZero);
}

#[test]
fn add_overflow_traps_e0041() {
    let op = expr(
        Ty::Int,
        0,
        9,
        ExprKind::Binary {
            op: BinOp::Add,
            lhs: Box::new(int(i64::MAX, 0, 3)),
            rhs: Box::new(int(1, 8, 9)),
        },
    );
    let m = main_fn(do_block(op, Ty::Int));
    let d = trap_of(execute(&validated(&m), &Config::default()));
    assert_eq!(d.code, ErrorCode::IntOverflow);
    assert_eq!(d.code.code_str(), "AION-E0041");
    assert_eq!(d.span, Some(sp(0, 9)));
}

#[test]
fn min_div_neg_one_traps_e0041() {
    let op = expr(
        Ty::Int,
        1,
        2,
        ExprKind::Binary {
            op: BinOp::Div,
            lhs: Box::new(int(i64::MIN, 1, 2)),
            rhs: Box::new(int(-1, 1, 2)),
        },
    );
    let m = main_fn(do_block(op, Ty::Int));
    assert_eq!(
        trap_of(execute(&validated(&m), &Config::default())).code,
        ErrorCode::IntOverflow
    );
}

#[test]
fn neg_min_traps_e0041() {
    let op = expr(
        Ty::Int,
        3,
        8,
        ExprKind::Unary {
            op: UnOp::Neg,
            rhs: Box::new(int(i64::MIN, 3, 8)),
        },
    );
    let m = main_fn(do_block(op, Ty::Int));
    assert_eq!(
        trap_of(execute(&validated(&m), &Config::default())).code,
        ErrorCode::IntOverflow
    );
}

#[test]
fn arithmetic_boundaries_ok() {
    // MAX-1 + 1, MIN+1 negated, normal div/mod.
    let mk = |kind, ty| expr(ty, 0, 4, kind);
    let cases = vec![
        mk(
            ExprKind::Binary {
                op: BinOp::Add,
                lhs: Box::new(int(i64::MAX - 1, 0, 1)),
                rhs: Box::new(int(1, 2, 3)),
            },
            Ty::Int,
        ),
        mk(
            ExprKind::Unary {
                op: UnOp::Neg,
                rhs: Box::new(int(i64::MIN + 1, 0, 1)),
            },
            Ty::Int,
        ),
        mk(
            ExprKind::Binary {
                op: BinOp::Div,
                lhs: Box::new(int(7, 0, 1)),
                rhs: Box::new(int(2, 2, 3)),
            },
            Ty::Int,
        ),
    ];
    for c in cases {
        let m = main_fn(do_block(c, Ty::Int));
        assert!(matches!(
            execute(&validated(&m), &Config::default()),
            RunResult::Ok(_)
        ));
    }
}

#[test]
fn fuel_exhaustion_deterministic_e0042() {
    // `do 1+2;` = 3 expr nodes. Fuel 2 traps; run twice, same code+span.
    let cfg = Config {
        fuel: 2,
        frames: 1024,
    };
    let run = || {
        let m = main_fn(do_block(
            expr(
                Ty::Int,
                0,
                5,
                ExprKind::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(int(1, 0, 1)),
                    rhs: Box::new(int(2, 4, 5)),
                },
            ),
            Ty::Int,
        ));
        trap_of(execute(&validated(&m), &cfg))
    };
    let d1 = run();
    let d2 = run();
    assert_eq!(d1.code, ErrorCode::FuelExhausted);
    assert_eq!(d1.code.code_str(), "AION-E0042");
    assert_eq!(d1.span, d2.span);
    assert_eq!(d1.code, d2.code);
}

#[test]
fn recursion_cap_e0043_with_call_span() {
    // fn loop() { do loop(); } main { do loop(); }
    let call_self = expr(
        Ty::Unit,
        10,
        18,
        ExprKind::Call {
            callee: "loopy".to_string(),
            args: vec![],
        },
    );
    let m = TypedModule {
        name: String::new(),
        eof_span: sp(0, 0),
        functions: vec![
            TypedFn {
                name: "loopy".to_string(),
                decl_span: sp(0, 11),
                params: vec![],
                ret: Ty::Unit,
                body: do_block(call_self, Ty::Unit),
            },
            TypedFn {
                name: "main".to_string(),
                decl_span: sp(12, 22),
                params: vec![],
                ret: Ty::Unit,
                body: do_block(
                    expr(
                        Ty::Unit,
                        0,
                        8,
                        ExprKind::Call {
                            callee: "loopy".to_string(),
                            args: vec![],
                        },
                    ),
                    Ty::Unit,
                ),
            },
        ],
    };
    let cfg = Config {
        fuel: 1_000_000,
        frames: 8,
    };
    let d = trap_of(execute(&validated(&m), &cfg));
    assert_eq!(d.code, ErrorCode::StackOverflow);
    assert_eq!(d.code.code_str(), "AION-E0043");
    assert_eq!(d.span, Some(sp(10, 18)));
}

#[test]
fn move_semantics_str_transfer_no_clone() {
    // let s = "hi"; do s(move);
    let body = TypedBlock {
        stmts: vec![
            aion_ir::TypedStmt::Let {
                name: "s".to_string(),
                ty: Ty::Str,
                init: expr(Ty::Str, 0, 4, ExprKind::Str("hi".to_string())),
            },
            aion_ir::TypedStmt::Do {
                expr: expr(
                    Ty::Str,
                    5,
                    6,
                    ExprKind::Local {
                        name: "s".to_string(),
                        access: LocalAccess::Move,
                    },
                ),
            },
        ],
        value: Ty::Str,
    };
    let m = main_fn(body);
    assert_eq!(run_ok(&m).value, Value::Unit);
}

#[test]
fn short_circuit_skips_rhs() {
    // `false && (1/0 == 0)` would trap if evaluated; short-circuit skips
    // it. (The comparison keeps the tree well-typed so the module
    // validates; the trap still fires when the rhs runs.)
    let trap_cmp = || {
        expr(
            Ty::Bool,
            4,
            12,
            ExprKind::Binary {
                op: BinOp::Eq,
                lhs: Box::new(expr(
                    Ty::Int,
                    4,
                    9,
                    ExprKind::Binary {
                        op: BinOp::Div,
                        lhs: Box::new(int(1, 4, 5)),
                        rhs: Box::new(int(0, 8, 9)),
                    },
                )),
                rhs: Box::new(int(0, 10, 11)),
            },
        )
    };
    for (op, l) in [(BinOp::And, false), (BinOp::Or, true)] {
        let b = expr(
            Ty::Bool,
            0,
            12,
            ExprKind::Binary {
                op,
                lhs: Box::new(boolean(l, 0, 1)),
                rhs: Box::new(trap_cmp()),
            },
        );
        let m = main_fn(do_block(b, Ty::Bool));
        assert!(matches!(
            execute(&validated(&m), &Config::default()),
            RunResult::Ok(_)
        ));
    }
    // And the taken branch still evaluates: `true && (1/0 == 0)` traps.
    let b = expr(
        Ty::Bool,
        0,
        12,
        ExprKind::Binary {
            op: BinOp::And,
            lhs: Box::new(boolean(true, 0, 1)),
            rhs: Box::new(trap_cmp()),
        },
    );
    let m = main_fn(do_block(b, Ty::Bool));
    assert_eq!(
        trap_of(execute(&validated(&m), &Config::default())).code,
        ErrorCode::DivByZero
    );
}

#[test]
fn nested_block_trailing_value_and_do_discarded() {
    // { do 1; do 2; } as non-trailing stmt -> discarded; trailing do 3.
    let inner = TypedBlock {
        stmts: vec![
            aion_ir::TypedStmt::Do { expr: int(1, 0, 1) },
            aion_ir::TypedStmt::Do { expr: int(2, 2, 3) },
        ],
        value: Ty::Int,
    };
    let body = TypedBlock {
        stmts: vec![
            aion_ir::TypedStmt::Block { body: inner },
            aion_ir::TypedStmt::Do { expr: int(3, 4, 5) },
        ],
        value: Ty::Int,
    };
    let m = main_fn(body);
    assert!(matches!(
        execute(&validated(&m), &Config::default()),
        RunResult::Ok(_)
    ));
}

#[test]
fn args_evaluated_left_to_right() {
    // fn second(a: Int, b: Int) { do b; } main { do second(trap?, 42); }
    // L-to-R: first arg div-zero traps before second is evaluated.
    let m = TypedModule {
        name: String::new(),
        eof_span: sp(0, 0),
        functions: vec![
            TypedFn {
                name: "second".to_string(),
                decl_span: sp(0, 20),
                params: vec![("a".to_string(), Ty::Int), ("b".to_string(), Ty::Int)],
                ret: Ty::Int,
                body: do_block(
                    expr(
                        Ty::Int,
                        0,
                        1,
                        ExprKind::Local {
                            name: "b".to_string(),
                            access: LocalAccess::Copy,
                        },
                    ),
                    Ty::Int,
                ),
            },
            TypedFn {
                name: "main".to_string(),
                decl_span: sp(21, 31),
                params: vec![],
                ret: Ty::Int,
                body: do_block(
                    expr(
                        Ty::Int,
                        20,
                        30,
                        ExprKind::Call {
                            callee: "second".to_string(),
                            args: vec![
                                expr(
                                    Ty::Int,
                                    21,
                                    26,
                                    ExprKind::Binary {
                                        op: BinOp::Div,
                                        lhs: Box::new(int(1, 21, 22)),
                                        rhs: Box::new(int(0, 25, 26)),
                                    },
                                ),
                                int(42, 28, 30),
                            ],
                        },
                    ),
                    Ty::Int,
                ),
            },
        ],
    };
    let d = trap_of(execute(&validated(&m), &Config::default()));
    assert_eq!(d.code, ErrorCode::DivByZero);
    assert_eq!(d.span, Some(sp(21, 26)));
}
