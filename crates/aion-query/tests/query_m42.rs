//! M4.2 query tests via `aion-driver::analyze` on inline sources.

use aion_driver::analyze;
use aion_query::{view, QueryError, ResolvedTy};
use aion_span::FileId;
use aion_syntax::NodeId;

fn fid() -> FileId {
    FileId(0)
}

fn analyze_src(src: &str) -> (aion_syntax::ParseResult, aion_sem_model::SemModel) {
    analyze(fid(), src.as_bytes()).expect("fixture must be sema-clean")
}

#[test]
fn entry_point_happy_path() {
    let (parse, model) = analyze_src("fn main() { let x = 1 + 2; do x; }");
    let v = view(&parse, &model);
    let entry = v.entry_point().expect("main must be found");
    assert_eq!(entry.name, "main");
    assert_eq!(
        src_slice("fn main() { let x = 1 + 2; do x; }", &entry),
        "main"
    );
}

fn src_slice<'s>(src: &'s str, h: &aion_query::FnHandle) -> &'s str {
    let span = h.span.expect("entry span must be recorded");
    &src[span.start as usize..span.end as usize]
}

#[test]
fn entry_point_missing() {
    let (parse, model) = analyze_src("fn helper() { do 1; }");
    let v = view(&parse, &model);
    assert_eq!(v.entry_point(), Err(QueryError::NoEntry));
}

#[test]
fn entry_point_with_params_is_absent() {
    let (parse, model) = analyze_src("fn main(x) { do x == \"a\"; do x; }");
    let v = view(&parse, &model);
    assert_eq!(v.entry_point(), Err(QueryError::NoEntry));
}

#[test]
fn fn_lookup_happy_and_unknown() {
    let (parse, model) = analyze_src("fn helper() { do 1; } fn main() { do helper(); }");
    let v = view(&parse, &model);
    let sig = v.fn_lookup("helper").unwrap();
    assert_eq!(sig.name, "helper");
    assert!(sig.params.is_empty());
    assert_eq!(
        v.fn_lookup("nope"),
        Err(QueryError::UnknownFunction("nope".into()))
    );
}

#[test]
fn call_edges_direct_and_deterministic() {
    let src = "fn a() { do 1; } fn b() { do a(); do a(); } fn main() { do b(); do a(); }";
    let (parse, model) = analyze_src(src);
    let v = view(&parse, &model);
    assert_eq!(v.callees("main").unwrap(), vec!["b", "a"]);
    assert_eq!(v.callees("b").unwrap(), vec!["a"]);
    assert_eq!(v.callees("a").unwrap(), Vec::<String>::new());
    assert_eq!(v.callers("a").unwrap(), vec!["b", "main"]);
    assert_eq!(v.callers("b").unwrap(), vec!["main"]);
    assert!(v.callers("nope").is_err());
    assert!(v.callees("nope").is_err());
    // Twice-run equality.
    let (parse2, model2) = analyze_src(src);
    let v2 = view(&parse2, &model2);
    assert_eq!(v.callees("main").unwrap(), v2.callees("main").unwrap());
    assert_eq!(v.callers("a").unwrap(), v2.callers("a").unwrap());
}

#[test]
fn recursion_includes_self() {
    let (parse, model) = analyze_src("fn f() { do f(); } fn main() { do f(); }");
    let v = view(&parse, &model);
    assert_eq!(v.callees("f").unwrap(), vec!["f"]);
    assert_eq!(v.callers("f").unwrap(), vec!["f", "main"]);
}

#[test]
fn type_at_ground_and_unknown_node() {
    let (parse, model) = analyze_src("fn main() { let x = 1 + 2; do x; }");
    let v = view(&parse, &model);
    let mut nodes: Vec<NodeId> = model.types.keys().copied().collect();
    nodes.sort_by_key(|n| n.0);
    assert!(!nodes.is_empty(), "model must record some types");
    // At least one recorded type resolves to a ground type display.
    let grounds: Vec<_> = nodes
        .iter()
        .filter_map(|n| match v.type_at(*n) {
            Ok(ResolvedTy::Ground { ty }) => Some(ty),
            _ => None,
        })
        .collect();
    assert!(!grounds.is_empty(), "expected a ground type, got none");
    assert!(
        grounds.iter().any(|t| t == "int"),
        "int expected: {grounds:?}"
    );
    // Var-typed nodes (if any) report Unknown, never a raw `^N` string.
    for n in &nodes {
        if let Ok(ResolvedTy::Ground { ty }) = v.type_at(*n) {
            assert!(!ty.starts_with('^'), "Var identity leaked: {ty}");
        }
    }
    assert_eq!(
        v.type_at(NodeId(u32::MAX)),
        Err(QueryError::UnknownNode(NodeId(u32::MAX)))
    );
}

#[test]
fn moves_sorted_and_deterministic() {
    let src = "fn main() { let s = \"hi\"; do s; }";
    let (parse, model) = analyze_src(src);
    let v = view(&parse, &model);
    let moves = v.moves();
    let ids: Vec<u32> = moves.iter().map(|m| m.node.0).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted, "moves must be sorted by node id");
    let (parse2, model2) = analyze_src(src);
    assert_eq!(moves, view(&parse2, &model2).moves());
}

#[test]
fn program_structure_definition_order() {
    let (parse, model) = analyze_src("fn b() { do 1; } fn a(x) { do x; } fn main() { do b(); }");
    let v = view(&parse, &model);
    let names: Vec<_> = v
        .program_structure()
        .iter()
        .map(|s| s.name.clone())
        .collect();
    assert_eq!(names, vec!["b", "a", "main"]);
    let a = v.program_structure()[1].clone();
    assert_eq!(a.params, vec!["x"]);
    // Twice-run equality.
    let (parse2, model2) = analyze_src("fn b() { do 1; } fn a(x) { do x; } fn main() { do b(); }");
    assert_eq!(
        v.program_structure(),
        view(&parse2, &model2).program_structure()
    );
}

#[test]
fn scope_chain_innermost_first_sorted_symbols() {
    let (parse, model) = analyze_src("fn main() { let b = 2; let a = 1; do a; }");
    let v = view(&parse, &model);
    let chain = v.scope_chain();
    assert!(!chain.is_empty());
    // Innermost-first: ids descending.
    let ids: Vec<u32> = chain.iter().map(|s| s.id).collect();
    let mut desc = ids.clone();
    desc.sort_by(|a, b| b.cmp(a));
    assert_eq!(ids, desc);
    // Symbols sorted by name within each scope.
    for scope in &chain {
        let names: Vec<_> = scope.symbols.iter().map(|s| s.name.clone()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }
    let (parse2, model2) = analyze_src("fn main() { let b = 2; let a = 1; do a; }");
    assert_eq!(chain, view(&parse2, &model2).scope_chain());
}

#[test]
fn symbol_kind_strings_are_explicit() {
    assert_eq!(
        aion_query::scope::symbol_kind_str(&aion_sem_model::SymbolKind::Fn),
        "Fn"
    );
    assert_eq!(
        aion_query::scope::symbol_kind_str(&aion_sem_model::SymbolKind::Param),
        "Param"
    );
    assert_eq!(
        aion_query::scope::symbol_kind_str(&aion_sem_model::SymbolKind::Let),
        "Let"
    );
    let (parse, model) = analyze_src("fn main(x) { let y = x; do y; }");
    let kinds: Vec<String> = view(&parse, &model)
        .scope_chain()
        .iter()
        .flat_map(|s| s.symbols.iter().map(|sym| sym.kind.clone()))
        .collect();
    assert!(!kinds.is_empty());
    for k in &kinds {
        assert!(
            ["Fn", "Param", "Let"].contains(&k.as_str()),
            "unexpected kind: {k}"
        );
    }
}

#[test]
fn type_at_distinguishes_var_from_error() {
    use aion_sem_model::{SemModel, Ty};
    use aion_syntax::NodeId;
    let parse = aion_syntax::parse(aion_span::FileId(0), b"fn main() { do 1; }");
    let mut model = SemModel::new();
    model.types.insert(NodeId(10), Ty::Var(0));
    model.types.insert(NodeId(11), Ty::Error);
    model.types.insert(NodeId(12), Ty::Int);
    let v = view(&parse, &model);
    assert_eq!(v.type_at(NodeId(10)), Ok(ResolvedTy::Unknown));
    assert_eq!(v.type_at(NodeId(11)), Ok(ResolvedTy::Error));
    assert_eq!(
        v.type_at(NodeId(12)),
        Ok(ResolvedTy::Ground { ty: "int".into() })
    );
}

#[test]
fn missing_model_item_yields_none_span() {
    // Function present in syntax but absent from the model: span must be
    // None rather than a synthesized 0..0 span.
    let src = "fn main() { do 1; }";
    let (parse, mut model) = analyze_src(src);
    model.items.clear();
    let v = view(&parse, &model);
    assert_eq!(v.entry_point().unwrap().span, None);
    assert_eq!(v.fn_lookup("main").unwrap().span, None);
    assert!(v.program_structure().iter().all(|s| s.span.is_none()));
}
