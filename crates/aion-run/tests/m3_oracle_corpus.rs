//! M3 public-API corpus test: every `tests/fixtures/m3` case through the
//! public pipeline only (`parse` → `lower` → `validate` → `execute`).
//!
//! Value-oracle agreement lives in the test-only oracle module
//! (`crates/aion-run/src/oracle.rs`, unit tests); this file pins the
//! observable behavior: clean lower+validate, `Unit` program result,
//! trap codes with exact spans, run-to-run determinism, golden sidecar
//! presence/shape, and the M2-invalid by-reference span pins.

use aion_run::{Config, RunResult, Value};
use aion_sem_model::{ScopeTree, Symbol, SymbolKind, Ty};
use aion_span::{ByteSpan, FileId};
use std::collections::{HashMap, HashSet};

fn fixture(rel: &str) -> Vec<u8> {
    let path = format!(
        "{}/../../tests/fixtures/m3/{rel}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

fn m2_fixture(rel: &str) -> Vec<u8> {
    let path = format!(
        "{}/../../tests/fixtures/m2/{rel}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

fn parse_clean(rel: &str) -> (String, aion_syntax::ParseResult) {
    let src = fixture(rel);
    let text = String::from_utf8_lossy(&src).into_owned();
    let r = aion_syntax::parse(FileId(0), &src);
    assert!(
        r.errors.is_empty(),
        "{rel} must parse clean: {:?}",
        r.errors
    );
    (text, r)
}

type Sig<'a> = (&'a str, Vec<Ty>, Ty);

fn model_for(sigs: &[Sig<'_>]) -> aion_sem_model::SemModel {
    let mut tree = ScopeTree::new();
    let root = tree.root();
    for (name, params, ret) in sigs {
        assert!(
            tree.define(
                root,
                Symbol {
                    kind: SymbolKind::Fn,
                    name: name.to_string(),
                    ty: Ty::Fn {
                        params: params.clone(),
                        ret: Box::new(ret.clone()),
                    },
                    span: ByteSpan::empty(FileId(0), 0),
                    moved: false,
                },
            ),
            "duplicate sig {name}"
        );
    }
    aion_sem_model::SemModel {
        items: Vec::new(),
        scope_tree: tree,
        types: HashMap::new(),
        moved_uses: HashSet::new(),
    }
}

fn pipeline(rel: &str, sigs: &[Sig<'_>]) -> (String, aion_ir::ValidatedModule) {
    let (text, r) = parse_clean(rel);
    let model = model_for(sigs);
    let (module, diags) = aion_ir::lower(&r, &model, aion_span::FileId(0), text.len() as u32);
    assert!(
        diags.is_empty(),
        "{rel} lower must be clean: {:?}",
        diags.iter().map(|d| d.render_human()).collect::<Vec<_>>()
    );
    let module = aion_ir::validate(&module).unwrap_or_else(|vdiags| {
        panic!(
            "{rel} validate must be clean: {:?}",
            vdiags.iter().map(|d| d.render_human()).collect::<Vec<_>>()
        )
    });
    (text, module)
}

const I: Ty = Ty::Int;
const S: Ty = Ty::Str;
const B: Ty = Ty::Bool;
const U: Ty = Ty::Unit;

fn valid_cases() -> Vec<(&'static str, Vec<Sig<'static>>)> {
    vec![
        ("exec/valid/arith.aion", vec![("main", vec![], I)]),
        ("exec/valid/precedence.aion", vec![("main", vec![], B)]),
        (
            "exec/valid/calls.aion",
            vec![
                ("add", vec![I, I], I),
                ("neg", vec![I], I),
                ("main", vec![], I),
            ],
        ),
        ("exec/valid/shadowing.aion", vec![("main", vec![], B)]),
        (
            "exec/valid/moves.aion",
            vec![("id", vec![S], S), ("main", vec![], S)],
        ),
        (
            "exec/valid/return_str.aion",
            vec![("mk", vec![], S), ("main", vec![], S)],
        ),
        ("exec/valid/nested.aion", vec![("main", vec![], I)]),
        ("exec/valid/unary_bool.aion", vec![("main", vec![], B)]),
        ("exec/valid/cmp_eq.aion", vec![("main", vec![], B)]),
        (
            "exec/valid/fresh_calls.aion",
            vec![("mk", vec![], S), ("main", vec![], B)],
        ),
        ("lower_valid/empty_main.aion", vec![("main", vec![], U)]),
        ("lower_valid/nested_blocks.aion", vec![("main", vec![], I)]),
        ("lower_valid/shadow_shape.aion", vec![("main", vec![], S)]),
    ]
}

#[test]
fn m3_corpus_executes_to_unit() {
    for (rel, sigs) in valid_cases() {
        let (_text, module) = pipeline(rel, &sigs);
        match aion_run::execute(&module, &Config::default()) {
            RunResult::Ok(o) => assert_eq!(o.value, Value::Unit, "{rel}: program result"),
            RunResult::Trap(d) => panic!("{rel} trapped: {}", d.render_human()),
        }
    }
}

#[test]
fn m3_execute_determinism_four_runs_identical() {
    for (rel, sigs) in valid_cases() {
        let (_text, module) = pipeline(rel, &sigs);
        let mut seen = Vec::new();
        for _ in 0..4 {
            let out = match aion_run::execute(&module, &Config::default()) {
                RunResult::Ok(o) => format!("{:?}|{}", o.value, o.steps_used),
                RunResult::Trap(d) => panic!("{rel} trapped: {}", d.render_human()),
            };
            seen.push(out);
        }
        for w in seen.windows(2) {
            assert_eq!(w[0], w[1], "{rel}: consecutive runs differ");
        }
    }
}

#[test]
fn m3_traps_exact_code_and_span() {
    let cases: &[(&str, Vec<Sig<'_>>, &str, &str, Config)] = &[
        (
            "exec/traps/divzero.aion",
            vec![("main", vec![], I)],
            "AION-E0040",
            "1 / 0",
            Config::default(),
        ),
        (
            "exec/traps/overflow.aion",
            vec![("main", vec![], I)],
            "AION-E0041",
            "9223372036854775807 + 1",
            Config::default(),
        ),
        (
            "exec/traps/fuel.aion",
            vec![("main", vec![], I)],
            "AION-E0042",
            "1",
            Config {
                fuel: 3,
                frames: 1024,
            },
        ),
        (
            "exec/traps/recursion.aion",
            vec![("ping", vec![], U), ("main", vec![], U)],
            "AION-E0043",
            "ping()",
            Config {
                fuel: 1_000_000,
                frames: 16,
            },
        ),
    ];
    for (rel, sigs, code, anchor, cfg) in cases {
        let (text, module) = pipeline(rel, sigs);
        match aion_run::execute(&module, cfg) {
            RunResult::Trap(d) => {
                assert_eq!(d.code.code_str(), *code, "{rel}");
                let sp = d.span.expect("trap span");
                assert_eq!(&text[sp.start as usize..sp.end as usize], *anchor, "{rel}");
            }
            RunResult::Ok(o) => panic!("{rel}: expected trap {code}, got {o:?}"),
        }
    }
}

#[test]
fn m3_golden_sidecars_present_and_shaped() {
    for (rel, _) in valid_cases()
        .into_iter()
        .filter(|(r, _)| r.starts_with("exec/valid/"))
    {
        let raw = fixture(&format!("{rel}.expected.json"));
        let text = String::from_utf8_lossy(&raw).into_owned();
        let t = text.trim();
        assert!(t.starts_with("{\"value\":"), "{rel} golden shape: {t:?}");
        assert!(t.ends_with('}'), "{rel} golden shape: {t:?}");
    }
}

/// M2 invalid fixtures, referenced by path (no copies under `m3/`).
/// Each must parse clean (the fault is semantic) and the parser must
/// have recorded the exact span the sema diagnostic points at; the
/// sema code+span contract itself is owned by
/// `crates/aion-sema/tests/check_m2.rs`.
#[test]
fn m3_invalid_by_reference_span_pins() {
    // (fixture, anchor, nth occurrence) — the occurrence matches the
    // use/definition site the m2 sema diagnostic points at.
    let cases: &[(&str, &str, usize)] = &[
        ("invalid/e0010_typemismatch.aion", "1 + \"x\"", 0),
        ("invalid/e0011_unbound.aion", "missing", 0),
        ("invalid/e0012_arity.aion", "(1)", 0),
        ("invalid/e0013_dupdef.aion", "f", 3),
        ("invalid/e0013_dupparam.aion", "a", 1),
        ("invalid/e0020_move.aion", "s", 2),
        ("invalid/e0020_param_move.aion", "s", 5),
        ("invalid/e0020_call_result.aion", "a", 2),
        ("invalid/e0020_param_inferred_str.aion", "p", 2),
    ];
    for (rel, anchor, nth) in cases {
        let src = m2_fixture(rel);
        let text = String::from_utf8_lossy(&src).into_owned();
        let r = aion_syntax::parse(FileId(0), &src);
        assert!(
            r.errors.is_empty(),
            "{rel} must parse clean: {:?}",
            r.errors
        );
        let (s, e) = span_of(&text, anchor, *nth);
        // The parser recorded exactly this span on some node: the span
        // chain the sema diagnostic consumes.
        let mut found = false;
        let mut id = aion_syntax::NodeId(0);
        loop {
            match r.spans.get(id) {
                Some(sp) if sp.start == s && sp.end == e => {
                    found = true;
                    break;
                }
                Some(_) => {}
                None => break,
            }
            id = aion_syntax::NodeId(id.0 + 1);
        }
        assert!(found, "{rel}: no parser-recorded span for {anchor:?}");
        assert_eq!(&text[s as usize..e as usize], *anchor);
    }
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
