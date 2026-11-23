//! M4.3 valid-metadata runtime non-interference (Suite A).
//!
//! Cleanliness-gate-then-compare: for each P / P+metadata pair, both
//! variants must first analyze clean under enabled flags (a dirty
//! variant invalidates the pair). Only then are three runtime channels
//! compared independently across the two executable paths: execution
//! result value, fuel consumption, and full trap identity plus payload.
//! Trace-equivalence verification is explicitly deferred (see below)
//! and is NOT established by this suite.
//!
//! Execution route: the M3 lowering stage predates metadata items, so
//! the meta variant is executed from its metadata-stripped parse (intent
//! items and goal statements removed). The strip is verified faithful
//! by structural shape equality against the plain variant's parse
//! (`ParseResult::shape`, the established structural comparator — not a
//! canonical contract). Spans are never compared across variants;
//! within-variant reruns pin in-variant determinism. No shared helpers
//! with the invalid-goal suite (`intent_goals.rs`); every helper here is
//! local to this file.

use aion_run::{trace_canonical_bytes, Config, RunResult};
use aion_span::FileId;
use aion_syntax::{
    Block, BlockIds, FeatureFlags, Item, ItemIds, ParseResult, Program, ProgramIds, Stmt, StmtIds,
};

fn fid() -> FileId {
    FileId(0)
}

fn enabled() -> FeatureFlags {
    FeatureFlags::all_experimental()
}

fn default_config() -> Config {
    Config::default()
}

/// Outcome projection with cross-variant-comparable fields only: result
/// value rendering, fuel consumed, trap code plus message payload.
/// Spans are deliberately excluded (provenance, never semantic).
#[derive(Debug, PartialEq, Eq)]
enum ExecProjection {
    Ok { value: String, fuel: u64 },
    Trap { code: String, message: String },
}

/// Metadata-stripped copy of a clean meta parse: top-level intent items
/// and goal statements (at any nesting depth) removed, everything else
/// cloned. The span table is shared unchanged; dropped nodes simply
/// leave unused entries behind.
fn strip_metadata(parse: &ParseResult) -> ParseResult {
    fn strip_block(b: &Block, bids: &BlockIds) -> (Block, BlockIds) {
        let mut stmts = Vec::with_capacity(b.stmts.len());
        let mut ids = Vec::with_capacity(b.stmts.len());
        assert_eq!(
            b.stmts.len(),
            bids.stmts.len(),
            "block/index shape must agree on clean input"
        );
        for (st, sid) in b.stmts.iter().zip(bids.stmts.iter()) {
            match (st, sid) {
                (Stmt::Goal(_), StmtIds::Goal { .. }) => {}
                (Stmt::Block(inner), StmtIds::Block { node, body }) => {
                    let (nb, nid) = strip_block(inner, body);
                    stmts.push(Stmt::Block(nb));
                    ids.push(StmtIds::Block {
                        node: *node,
                        body: nid,
                    });
                }
                _ => {
                    stmts.push(st.clone());
                    ids.push(sid.clone());
                }
            }
        }
        (
            Block { stmts },
            BlockIds {
                node: bids.node,
                stmts: ids,
            },
        )
    }

    assert_eq!(
        parse.program.items.len(),
        parse.index.items.len(),
        "top-level item/index shape must agree on clean input"
    );
    let mut items = Vec::with_capacity(parse.program.items.len());
    let mut iids = Vec::with_capacity(parse.index.items.len());
    for (item, iid) in parse.program.items.iter().zip(parse.index.items.iter()) {
        match (item, iid) {
            (Item::Fn(f), ItemIds::Fn(fids)) => {
                let (body, bids) = strip_block(&f.body, &fids.body);
                let mut f2 = f.clone();
                f2.body = body;
                let mut g2 = fids.clone();
                g2.body = bids;
                items.push(Item::Fn(f2));
                iids.push(ItemIds::Fn(g2));
            }
            (Item::Intent(_), ItemIds::Intent(_)) => {}
            _ => panic!("item/index kind mismatch on clean input"),
        }
    }
    ParseResult {
        root: parse.root.clone(),
        program: Program { items },
        errors: Vec::new(),
        spans: parse.spans.clone(),
        index: ProgramIds {
            node: parse.index.node,
            items: iids,
        },
    }
}

fn exec_parse(parse: &ParseResult, bytes: &[u8], config: &Config) -> ExecProjection {
    assert!(
        parse.errors.is_empty(),
        "pipeline input must parse clean: {:?}",
        parse.errors.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let (sema_diags, model) = aion_sema::check_with_model(parse, fid(), bytes);
    assert!(
        sema_diags.is_empty(),
        "pipeline input must check clean: {:?}",
        sema_diags
            .iter()
            .map(|d| (&d.code, &d.message))
            .collect::<Vec<_>>()
    );
    let (module, lower_diags) = aion_ir::lower(parse, &model, fid(), bytes.len() as u32);
    assert!(
        lower_diags.is_empty(),
        "lowering must be clean: {lower_diags:?}"
    );
    let module = aion_ir::validate(&module).unwrap_or_else(|ds| {
        panic!(
            "validation must succeed: {:?}",
            ds.iter().map(|d| (&d.code, &d.message)).collect::<Vec<_>>()
        )
    });
    match aion_run::execute(&module, config) {
        RunResult::Ok(o) => ExecProjection::Ok {
            value: format!("{:?}", o.value),
            fuel: o.steps_used,
        },
        RunResult::Trap(d) => ExecProjection::Trap {
            code: d.code.code_str().to_string(),
            message: d.message.clone(),
        },
    }
}

/// Trace serializer check (NOT a cross-variant equivalence proof): no
/// public traced-execute entry point exists (see `run_trace_json`'s
/// NOTE), so the publicly observable trace for any clean program is
/// the canonical envelope over the empty event list. This assertion
/// verifies that serializer output against its golden file. It does
/// NOT establish trace equivalence between the two execution paths;
/// that verification is deferred until AION has a traced-execution
/// interface.
fn public_trace_envelope() -> Vec<u8> {
    trace_canonical_bytes(&[], false, 0)
}

fn trace_golden() -> Vec<u8> {
    std::fs::read("../../tests/golden/m4/driver/trace_ok.json")
        .unwrap_or_else(|e| panic!("read trace golden: {e}"))
}

/// Gate both variants clean, verify the strip is faithful, then compare
/// the three executable channels. Reruns each variant once more to pin
/// within-variant determinism (the only place spans may be compared:
/// never across variants).
fn assert_equivalent(plain: &[u8], meta: &[u8], config: &Config) {
    aion_driver::analyze_with_flags(fid(), plain, enabled())
        .unwrap_or_else(|ds| panic!("P must analyze clean: {ds:?}"));
    aion_driver::analyze_with_flags(fid(), meta, enabled())
        .unwrap_or_else(|ds| panic!("P+metadata must analyze clean: {ds:?}"));
    let parse_p = aion_syntax::parse_with_flags(fid(), plain, enabled());
    let parse_m = aion_syntax::parse_with_flags(fid(), meta, enabled());
    assert!(parse_p.errors.is_empty() && parse_m.errors.is_empty());
    let stripped = strip_metadata(&parse_m);
    assert_eq!(
        stripped.shape(),
        parse_p.shape(),
        "stripped P+metadata must be structurally identical to P"
    );
    let a = exec_parse(&parse_p, plain, config);
    let b = exec_parse(&stripped, meta, config);
    assert_eq!(
        a, b,
        "result value / fuel / trap identity+payload must match"
    );
    assert_eq!(
        public_trace_envelope(),
        trace_golden(),
        "public empty-event trace serializer output must match its golden"
    );
    // Within-variant determinism rerun.
    let a2 = exec_parse(&parse_p, plain, config);
    let b2 = exec_parse(&strip_metadata(&parse_m), meta, config);
    assert_eq!(a, a2, "P must be deterministic");
    assert_eq!(b, b2, "P+metadata must be deterministic");
}

fn fixture_pair(dir: &str) -> (Vec<u8>, Vec<u8>) {
    let base = format!("../../tests/fixtures/m4_3/valid_equiv/{dir}");
    let p = std::fs::read(format!("{base}/p.aion")).expect("read p.aion");
    let m = std::fs::read(format!("{base}/p_meta.aion")).expect("read p_meta.aion");
    (p, m)
}

#[test]
fn intent_on_main() {
    let plain = b"fn main() { do 1; }".as_slice();
    let meta = b"intent \"m\" { goal : 1; }\nfn main() { do 1; }".as_slice();
    assert_equivalent(plain, meta, &default_config());
}

#[test]
fn intent_on_helper_only() {
    let plain = b"fn helper() { do 7; }\nfn main() { do helper(); }".as_slice();
    let meta =
        b"intent \"h\" { goal : 2; }\nfn helper() { goal : 1; do 7; }\nfn main() { do helper(); }"
            .as_slice();
    assert_equivalent(plain, meta, &default_config());
}

#[test]
fn goal_bound_name() {
    let plain = b"fn id(a) { do a; }\nfn main() { do id(1); }".as_slice();
    let meta = b"fn id(a) { goal : a; do a; }\nfn main() { do id(1); }".as_slice();
    assert_equivalent(plain, meta, &default_config());
}

#[test]
fn goal_expr_arith() {
    let plain = b"fn main() { let x = 2; do x * 3 + 1; }".as_slice();
    let meta = b"fn main() { let x = 2; goal : x * 3 + 1; do x * 3 + 1; }".as_slice();
    assert_equivalent(plain, meta, &default_config());
}

#[test]
fn multi_item() {
    let plain =
        b"fn h1() { do 1; }\nfn h2(x) { do x + 1; }\nfn main() { do h1(); do h2(1); }".as_slice();
    let meta = b"intent \"items\" { goal : h1(); }\nfn h1() { goal : 1; do 1; }\nfn h2(x) { goal : x; do x + 1; }\nfn main() { do h1(); do h2(1); }".as_slice();
    assert_equivalent(plain, meta, &default_config());
}

#[test]
fn empty_intent() {
    let plain = b"fn main() { do 1; }".as_slice();
    let meta = b"intent \"empty\" { }\nfn main() { do 1; }".as_slice();
    assert_equivalent(plain, meta, &default_config());
}

#[test]
fn gate_off_control() {
    // Default-off programs behave exactly as before: default `analyze`
    // and enabled `analyze_with_flags` agree, and execution matches.
    let plain = b"fn main() { let x = 1 + 2; do x; }".as_slice();
    aion_driver::analyze(fid(), plain).expect("default analyze must succeed");
    aion_driver::analyze_with_flags(fid(), plain, enabled()).expect("enabled analyze must succeed");
    let parse_d = aion_syntax::parse(fid(), plain);
    let parse_e = aion_syntax::parse_with_flags(fid(), plain, enabled());
    let a = exec_parse(&parse_d, plain, &default_config());
    let b = exec_parse(&parse_e, plain, &default_config());
    assert_eq!(a, b);
}

#[test]
fn div_by_zero_metadata_trap_pair() {
    let plain = b"fn main() { do 1 / 0; }".as_slice();
    let meta = b"intent \"d\" { goal : 1; }\nfn main() { do 1 / 0; }".as_slice();
    // Cleanliness gate holds (traps are runtime, not sema), then the
    // trap identity plus payload must be identical.
    assert_equivalent(plain, meta, &default_config());
    let parse_m = aion_syntax::parse_with_flags(fid(), meta, enabled());
    match exec_parse(&strip_metadata(&parse_m), meta, &default_config()) {
        ExecProjection::Trap { code, .. } => assert_eq!(code, "AION-E0040"),
        other => panic!("expected DivByZero trap, got {other:?}"),
    }
}

#[test]
fn fuel_boundary_pair() {
    // A tiny fuel budget exhausts identically with and without metadata.
    let config = Config {
        fuel: 1,
        frames: 1024,
    };
    let plain = b"fn main() { do 1 + 2; }".as_slice();
    let meta = b"intent \"f\" { goal : 1; }\nfn main() { goal : 1 + 2; do 1 + 2; }".as_slice();
    assert_equivalent(plain, meta, &config);
    let parse_m = aion_syntax::parse_with_flags(fid(), meta, enabled());
    match exec_parse(&strip_metadata(&parse_m), meta, &config) {
        ExecProjection::Trap { code, .. } => assert_eq!(code, "AION-E0042"),
        other => panic!("expected FuelExhausted trap, got {other:?}"),
    }
}

#[test]
fn fixture_arith_pair() {
    let (p, m) = fixture_pair("arith");
    assert_equivalent(&p, &m, &default_config());
}

#[test]
fn fixture_callee_pair() {
    let (p, m) = fixture_pair("callee");
    assert_equivalent(&p, &m, &default_config());
}
