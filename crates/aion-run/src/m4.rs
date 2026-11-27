//! M4.1 determinism-gate + parity tests (crate unit tests: they use
//! `pub(crate)` observer plumbing, which integration tests cannot see).

use super::trace;
use super::{execute, Config, Outcome, RunResult, Value};
use aion_ir::{
    AllocKind, ArenaTable, ExprKind, TypedBlock, TypedExpr, TypedFn, TypedModule, ValidatedModule,
};
use aion_sem_model::{ScopeId, Ty};
use aion_span::{ByteSpan, FileId};
use aion_syntax::BinOp;

fn sp(s: u32, e: u32) -> ByteSpan {
    ByteSpan {
        file: FileId(0),
        start: s,
        end: e,
    }
}

fn int_expr(v: i64, s: u32, e: u32) -> TypedExpr {
    TypedExpr {
        ty: Ty::Int,
        span: sp(s, e),
        kind: ExprKind::Int(v),
        // Inert for non-string types; assigned explicitly.
        alloc: AllocKind::Refcounted,
    }
}

fn validated(m: &TypedModule) -> ValidatedModule {
    aion_ir::validate(m).unwrap_or_else(|ds| {
        panic!(
            "expected clean validation, got {:?}",
            ds.iter().map(|d| d.render_human()).collect::<Vec<_>>()
        )
    })
}

/// Fixed fixture: `fn main() { 1 + 2 * 3 }` with stable spans.
pub(crate) fn fixture_basic() -> TypedModule {
    let mul = TypedExpr {
        ty: Ty::Int,
        span: sp(4, 9),
        kind: ExprKind::Binary {
            op: BinOp::Mul,
            lhs: Box::new(int_expr(2, 4, 5)),
            rhs: Box::new(int_expr(3, 8, 9)),
        },
        // Inert for non-string types; assigned explicitly.
        alloc: AllocKind::Refcounted,
    };
    let add = TypedExpr {
        ty: Ty::Int,
        span: sp(0, 9),
        kind: ExprKind::Binary {
            op: BinOp::Add,
            lhs: Box::new(int_expr(1, 0, 1)),
            rhs: Box::new(mul),
        },
        // Inert for non-string types; assigned explicitly.
        alloc: AllocKind::Refcounted,
    };
    TypedModule {
        name: String::from("m4"),
        scope: ScopeId(u32::MAX),
        eof_span: sp(10, 10),
        arenas: ArenaTable::new(),
        functions: vec![TypedFn {
            name: String::from("main"),
            scope: ScopeId(u32::MAX - 1),
            decl_span: sp(0, 8),
            params: vec![],
            ret: Ty::Int,
            body: TypedBlock {
                stmts: vec![aion_ir::TypedStmt::Do { expr: add }],
                value: Ty::Int,
                // Test root scope: this block's own scope.
                scope: ScopeId(0),
            },
        }],
    }
}

/// Same semantics as [`fixture_basic`] with every span shifted by `delta`
/// (a P/metadata-layout pair: spans differ, semantics identical).
pub(crate) fn fixture_shifted(delta: u32) -> TypedModule {
    fn shift(e: &TypedExpr, d: u32) -> TypedExpr {
        let kind = match &e.kind {
            ExprKind::Binary { op, lhs, rhs } => ExprKind::Binary {
                op: *op,
                lhs: Box::new(shift(lhs, d)),
                rhs: Box::new(shift(rhs, d)),
            },
            ExprKind::Unary { op, rhs } => ExprKind::Unary {
                op: *op,
                rhs: Box::new(shift(rhs, d)),
            },
            ExprKind::Call { callee, args } => ExprKind::Call {
                callee: callee.clone(),
                args: args.iter().map(|a| shift(a, d)).collect(),
            },
            k => k.clone(),
        };
        TypedExpr {
            ty: e.ty.clone(),
            span: ByteSpan {
                file: e.span.file,
                start: e.span.start + d,
                end: e.span.end + d,
            },
            kind,
            // Allocation provenance survives the span shift unchanged.
            alloc: e.alloc,
        }
    }
    let mut m = fixture_basic();
    for f in &mut m.functions {
        // Preserve the body's own scope across the temporary swap.
        let scope = f.body.scope;
        let body = std::mem::replace(
            &mut f.body,
            TypedBlock {
                stmts: vec![],
                value: Ty::Unit,
                scope,
            },
        );
        let stmts = body
            .stmts
            .into_iter()
            .map(|s| match s {
                aion_ir::TypedStmt::Do { expr } => aion_ir::TypedStmt::Do {
                    expr: shift(&expr, delta),
                },
                other => other,
            })
            .collect();
        f.body = TypedBlock {
            stmts,
            value: body.value,
            scope: body.scope,
        };
        f.decl_span = ByteSpan {
            file: f.decl_span.file,
            start: f.decl_span.start + delta,
            end: f.decl_span.end + delta,
        };
    }
    m.eof_span = ByteSpan {
        file: m.eof_span.file,
        start: m.eof_span.start + delta,
        end: m.eof_span.end + delta,
    };
    m
}

pub(crate) fn run_traced(m: &ValidatedModule) -> (RunResult, Vec<u8>) {
    trace::with_observer(|| execute(m, &Config::default()))
}

fn outcome_key(r: &RunResult) -> (Option<Outcome>, Option<String>) {
    match r {
        RunResult::Ok(o) => (Some(o.clone()), None),
        RunResult::Trap(d) => (None, Some(d.code.code_str().to_string())),
    }
}

/// Strip every `"span":{...}` object (brace-matched) for cross-layout
/// semantic comparison. Spans are excluded; everything else must match.
pub(crate) fn strip_spans(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if s[i..].starts_with("\"span\":{") {
            i += "\"span\":{".len();
            let mut depth = 1;
            while i < b.len() && depth > 0 {
                match b[i] {
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    _ => {}
                }
                i += 1;
            }
        } else {
            out.push(b[i] as char);
            i += 1;
        }
    }
    out
}

#[test]
fn determinism_ten_reps_byte_identical() {
    let m = validated(&fixture_basic());
    let (_, first) = run_traced(&m);
    assert!(first.ends_with(b"\n"), "canonical bytes need LF terminator");
    for _ in 0..9 {
        let (_, next) = run_traced(&m);
        assert_eq!(next, first, "in-process reps must be byte-identical");
    }
}

#[test]
fn fresh_process_determinism() {
    const MARK: &str = "AION_M4_TRACE:";
    // Child entry: print this test's canonical bytes, then return so the
    // parent can compare across fresh processes.
    if std::env::var("AION_M4_CHILD").is_ok() {
        let m = validated(&fixture_basic());
        let (_, bytes) = run_traced(&m);
        println!("{MARK}{}", String::from_utf8_lossy(&bytes));
        return;
    }
    let exe = std::env::current_exe().expect("current test exe");
    let mut outs = Vec::new();
    for _ in 0..3 {
        let out = std::process::Command::new(&exe)
            .arg("m4::fresh_process_determinism")
            .arg("--exact")
            .arg("--nocapture")
            .env("AION_M4_CHILD", "1")
            .output()
            .expect("spawn child test exe");
        assert!(out.status.success(), "child run failed: {out:?}");
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let line = text
            .lines()
            .find(|l| l.starts_with(MARK))
            .expect("child must print canonical bytes");
        outs.push(line[MARK.len()..].to_string());
    }
    assert_eq!(outs[0], outs[1]);
    assert_eq!(outs[1], outs[2]);
    // And the fresh-process bytes match this process (`println!` in the
    // child adds the line break that `lines()` strips; compare trimmed).
    let m = validated(&fixture_basic());
    let (_, here) = run_traced(&m);
    assert_eq!(outs[0], String::from_utf8_lossy(&here).trim_end());
}

#[test]
fn hooks_disabled_vs_enabled_parity() {
    let m = validated(&fixture_basic());
    // Hooks disabled: plain execute.
    let plain = execute(&m, &Config::default());
    let (pk, _) = (outcome_key(&plain), ());
    let plain_steps = match &plain {
        RunResult::Ok(o) => o.steps_used,
        RunResult::Trap(_) => 0,
    };
    // Hooks enabled: traced execute.
    let (traced, bytes) = run_traced(&m);
    assert_eq!(outcome_key(&traced), pk, "Outcome value must match");
    match (&plain, &traced) {
        (RunResult::Ok(a), RunResult::Ok(b)) => {
            assert_eq!(a.value, b.value);
            assert_eq!(a.steps_used, b.steps_used);
        }
        (RunResult::Trap(a), RunResult::Trap(b)) => {
            assert_eq!(a.code, b.code);
            assert_eq!(a.span, b.span, "trap code+span must match");
        }
        _ => panic!("ok/trap mismatch between plain and traced runs"),
    }
    assert_eq!(
        plain_steps,
        match &traced {
            RunResult::Ok(o) => o.steps_used,
            RunResult::Trap(_) => 0,
        }
    );
    assert!(!bytes.is_empty());
    // Trap parity case: division by zero.
    let div = TypedExpr {
        ty: Ty::Int,
        span: sp(0, 5),
        kind: ExprKind::Binary {
            op: BinOp::Div,
            lhs: Box::new(int_expr(1, 0, 1)),
            rhs: Box::new(int_expr(0, 4, 5)),
        },
        // Inert for non-string types; assigned explicitly.
        alloc: AllocKind::Refcounted,
    };
    let tm = TypedModule {
        name: String::from("t"),
        scope: ScopeId(u32::MAX),
        eof_span: sp(6, 6),
        arenas: ArenaTable::new(),
        functions: vec![TypedFn {
            name: String::from("main"),
            scope: ScopeId(u32::MAX - 1),
            decl_span: sp(0, 8),
            params: vec![],
            ret: Ty::Int,
            body: TypedBlock {
                stmts: vec![aion_ir::TypedStmt::Do { expr: div }],
                value: Ty::Int,
                // Test root scope: this block's own scope.
                scope: ScopeId(0),
            },
        }],
    };
    let tm = validated(&tm);
    let p = execute(&tm, &Config::default());
    let (t, _) = run_traced(&tm);
    match (&p, &t) {
        (RunResult::Trap(a), RunResult::Trap(b)) => {
            assert_eq!(a.code, b.code);
            assert_eq!(a.span, b.span);
        }
        _ => panic!("expected traps in both runs"),
    }
}

#[test]
fn non_interference_skeleton_p_vs_metadata() {
    // P vs P+metadata: same semantics, shifted spans. Compare
    // results/traces/fuel/traps semantically (spans excluded cross-layout).
    // NOTE (M4.3-ready skeleton): uses a synthetic shifted-span pair; swap
    // in a gated-syntax fixture pair when one exists.
    let a = validated(&fixture_basic());
    let b = validated(&fixture_shifted(100));
    let (ra, ta) = run_traced(&a);
    let (rb, tb) = run_traced(&b);
    let sa = String::from_utf8(ta).expect("trace is UTF-8");
    let sb = String::from_utf8(tb).expect("trace is UTF-8");
    assert_eq!(
        strip_spans(&sa),
        strip_spans(&sb),
        "traces must match semantically (spans excluded)"
    );
    match (&ra, &rb) {
        (RunResult::Ok(oa), RunResult::Ok(ob)) => {
            assert_eq!(oa.value, ob.value);
            assert_eq!(oa.steps_used, ob.steps_used);
        }
        (RunResult::Trap(da), RunResult::Trap(db)) => {
            assert_eq!(da.code, db.code);
        }
        _ => panic!("ok/trap mismatch across layouts"),
    }
}

#[test]
fn str_values_recorded_as_len_and_hash_only() {
    assert_eq!(
        trace::fnv1a_hex(""),
        "cbf29ce484222325",
        "empty string hashes to offset basis"
    );
    let s = trace::value_summary(&Value::Str(String::from("hi")));
    assert!(s.contains("\"len\":2"));
    assert!(s.contains(&trace::fnv1a_hex("hi")));
    assert!(!s.contains("hi\""), "full contents must never be recorded");
}

#[test]
fn no_rng_time_or_locale_in_canonical_paths() {
    let trace_src = include_str!("trace.rs");
    for pat in [
        "SystemTime",
        "UNIX_EPOCH",
        "rand",
        "Rng",
        "thread_rng",
        "setlocale",
        "LC_ALL",
        "to_locale",
    ] {
        assert!(
            !trace_src.contains(pat),
            "canonical path must not contain {pat}"
        );
    }
    let lib_src = include_str!("lib.rs");
    for pat in ["SystemTime", "thread_rng", "setlocale"] {
        assert!(
            !lib_src.contains(pat),
            "canonical path must not contain {pat}"
        );
    }
}

#[test]
fn trace_golden_basic() {
    let m = validated(&fixture_basic());
    let (_, bytes) = run_traced(&m);
    let golden = include_str!("../../../tests/golden/m4/canonical/trace_basic.json");
    assert_eq!(String::from_utf8_lossy(&bytes), golden);
}

#[test]
fn trace_golden_str_and_trap() {
    // String value + trap fixture golden.
    let stre = TypedExpr {
        ty: Ty::Str,
        span: sp(0, 4),
        kind: ExprKind::Str(String::from("hi")),
        // Explicit refcounted provenance for string fixtures.
        alloc: AllocKind::Refcounted,
    };
    let m = TypedModule {
        name: String::from("s"),
        scope: ScopeId(u32::MAX),
        eof_span: sp(5, 5),
        arenas: ArenaTable::new(),
        functions: vec![TypedFn {
            name: String::from("main"),
            scope: ScopeId(u32::MAX - 1),
            decl_span: sp(0, 8),
            params: vec![],
            ret: Ty::Str,
            body: TypedBlock {
                stmts: vec![aion_ir::TypedStmt::Do { expr: stre }],
                value: Ty::Str,
                // Test root scope: this block's own scope.
                scope: ScopeId(0),
            },
        }],
    };
    let m = validated(&m);
    let (_, bytes) = run_traced(&m);
    let golden = include_str!("../../../tests/golden/m4/canonical/trace_str.json");
    assert_eq!(String::from_utf8_lossy(&bytes), golden);

    let div = TypedExpr {
        ty: Ty::Int,
        span: sp(0, 5),
        kind: ExprKind::Binary {
            op: BinOp::Div,
            lhs: Box::new(int_expr(1, 0, 1)),
            rhs: Box::new(int_expr(0, 4, 5)),
        },
        // Inert for non-string types; assigned explicitly.
        alloc: AllocKind::Refcounted,
    };
    let tm = TypedModule {
        name: String::from("t"),
        scope: ScopeId(u32::MAX),
        eof_span: sp(6, 6),
        arenas: ArenaTable::new(),
        functions: vec![TypedFn {
            name: String::from("main"),
            scope: ScopeId(u32::MAX - 1),
            decl_span: sp(0, 8),
            params: vec![],
            ret: Ty::Int,
            body: TypedBlock {
                stmts: vec![aion_ir::TypedStmt::Do { expr: div }],
                value: Ty::Int,
                // Test root scope: this block's own scope.
                scope: ScopeId(0),
            },
        }],
    };
    let tm = validated(&tm);
    let (_, tbytes) = run_traced(&tm);
    let tgolden = include_str!("../../../tests/golden/m4/canonical/trace_trap.json");
    assert_eq!(String::from_utf8_lossy(&tbytes), tgolden);
}

#[test]
fn truncation_marker_is_deterministic() {
    let m = validated(&fixture_basic());
    // Force truncation with a tiny cap by recording directly.
    let ((), bytes) = trace::with_observer(|| {
        for i in 0..(trace::CAP + 10) {
            trace::record(format!("{{\"seq\":{i}}}"));
        }
    });
    let s = String::from_utf8(bytes).expect("UTF-8");
    assert!(s.contains("\"truncated\":true"));
    assert!(s.contains(&format!("\"kept\":{}", trace::CAP)));
    let _ = m;
}
