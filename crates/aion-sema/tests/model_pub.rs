//! M4.2 model publication: items, types, and refreshed symbol types.
//!
//! Runs the m2 corpus fixtures through both [`aion_sema::check`] and
//! [`aion_sema::check_with_model`] and asserts the model is populated
//! while diagnostics stay byte-identical.

use aion_span::FileId;
use aion_syntax::{ExprIds, ItemIds, NodeId, StmtIds};

fn load(rel: &str) -> Vec<u8> {
    let path = format!(
        "{}/../../tests/fixtures/m2/{rel}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

fn collect_expr_nodes(ids: &ExprIds, out: &mut Vec<NodeId>) {
    out.push(ids.node);
    match &ids.kind {
        aion_syntax::ExprIdsKind::Leaf => {}
        aion_syntax::ExprIdsKind::Unary { rhs, .. } => collect_expr_nodes(rhs, out),
        aion_syntax::ExprIdsKind::Binary { lhs, rhs, .. } => {
            collect_expr_nodes(lhs, out);
            collect_expr_nodes(rhs, out);
        }
        aion_syntax::ExprIdsKind::Call { args, .. } => {
            for a in args {
                collect_expr_nodes(a, out);
            }
        }
    }
}

fn collect_block(ids: &aion_syntax::BlockIds, exprs: &mut Vec<NodeId>) {
    for s in &ids.stmts {
        match s {
            StmtIds::Let { expr, .. } => collect_expr_nodes(expr, exprs),
            StmtIds::Do { expr, .. } => collect_expr_nodes(expr, exprs),
            StmtIds::Block { body, .. } => collect_block(body, exprs),
            StmtIds::Assert { expr, .. } => collect_expr_nodes(expr, exprs),
            StmtIds::Goal { .. } | StmtIds::Empty(_) => {}
        }
    }
}

fn has_var(ty: &aion_sem_model::Ty) -> bool {
    match ty {
        aion_sem_model::Ty::Var(_) => true,
        aion_sem_model::Ty::Fn { params, ret } => params.iter().any(has_var) || has_var(ret),
        _ => false,
    }
}

const FIXTURES: &[&str] = &[
    "valid/inference.aion",
    "valid/precedence.aion",
    "valid/shadowing.aion",
    "valid/nested_scopes.aion",
    "valid/multi_fn.aion",
    "valid/return_position.aion",
    "valid/return_str_local.aion",
    "valid/return_str_param.aion",
    "invalid/e0010_typemismatch.aion",
    "invalid/e0011_unbound.aion",
    "invalid/e0012_arity.aion",
    "invalid/e0013_dupdef.aion",
    "invalid/e0013_dupparam.aion",
    "invalid/e0020_call_result.aion",
    "invalid/e0020_move.aion",
    "invalid/e0020_param_inferred_str.aion",
    "invalid/e0020_param_move.aion",
];

#[test]
fn model_pub_items_types_symbols_diags() {
    for rel in FIXTURES {
        let src = load(rel);
        let parse = aion_syntax::parse(FileId(0), &src);
        assert!(parse.errors.is_empty(), "{rel} must parse clean");

        let diags_plain = aion_sema::check(&parse, FileId(0), &src);
        let (diags_model, model) = aion_sema::check_with_model(&parse, FileId(0), &src);

        // Diagnostics byte-identical between the two entry points.
        let render =
            |ds: &[aion_error::Diagnostic]| ds.iter().map(|d| d.render_human()).collect::<Vec<_>>();
        assert_eq!(
            render(&diags_plain),
            render(&diags_model),
            "{rel}: diagnostics differ between check and check_with_model"
        );

        // Items: one per top-level fn, name + name span.
        let mut want_names = Vec::new();
        for (item, iids) in parse.program.items.iter().zip(parse.index.items.iter()) {
            if let (aion_syntax::Item::Fn(f), ItemIds::Fn(fids)) = (item, iids) {
                want_names.push((f.name.clone(), parse.spans.get(fids.name)));
            }
        }
        assert_eq!(model.items.len(), want_names.len(), "{rel}: items count");
        for (got, (want_name, want_span)) in model.items.iter().zip(want_names.iter()) {
            assert_eq!(&got.name, want_name, "{rel}: item name");
            assert_eq!(got.span, want_span.unwrap(), "{rel}: item span");
        }
        assert!(!model.items.is_empty(), "{rel}: items must be non-empty");

        // Types: cover every expression NodeId in the index.
        let mut expr_nodes = Vec::new();
        for iids in &parse.index.items {
            if let ItemIds::Fn(fids) = iids {
                collect_block(&fids.body, &mut expr_nodes);
            }
        }
        assert!(!expr_nodes.is_empty(), "{rel}: expected expr nodes");
        for id in &expr_nodes {
            assert!(
                model.types.contains_key(id),
                "{rel}: types map missing expr node {id:?}"
            );
        }
    }
}

#[test]
fn model_pub_valid_fixtures_fully_ground() {
    // Valid fixtures infer ground types everywhere: no stale Var may leak
    // into the published model (types map or symbol types), since the final
    // resolve pass runs with the final substitution.
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
    for rel in VALID {
        let src = load(rel);
        let parse = aion_syntax::parse(FileId(0), &src);
        assert!(parse.errors.is_empty());
        let (diags, model) = aion_sema::check_with_model(&parse, FileId(0), &src);
        assert!(diags.is_empty(), "{rel}: valid fixture must check clean");
        for (id, ty) in &model.types {
            assert!(!has_var(ty), "{rel}: stale Var in types[{id:?}] = {ty}");
        }
        for scope in &model.scope_tree.scopes {
            for (name, sym) in &scope.symbols {
                assert!(
                    !has_var(&sym.ty),
                    "{rel}: stale Var in symbol `{name}` type = {}",
                    sym.ty
                );
            }
        }
    }
}
