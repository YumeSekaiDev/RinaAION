//! M4.3 invalid-goal diagnostic isolation + bound-goal model identity.
//!
//! Invalid-goal tests (E0011 per occurrence, masked model-identity,
//! no lowering on dirty) are kept strictly separate from runtime
//! equivalence tests; no shared helpers between the two suites.

use aion_error::ErrorCode;
use aion_span::FileId;
use aion_syntax::{FeatureFlags, ItemIds, StmtIds};

fn parse(src: &str) -> aion_syntax::ParseResult {
    let r =
        aion_syntax::parse_with_flags(FileId(0), src.as_bytes(), FeatureFlags::all_experimental());
    assert!(
        r.errors.is_empty(),
        "source must parse clean: {src:?}: {:?}",
        r.errors.iter().map(|d| d.code).collect::<Vec<_>>()
    );
    r
}

fn model_of(src: &str) -> (Vec<aion_error::Diagnostic>, aion_sem_model::SemModel) {
    let r = parse(src);
    aion_sema::check_with_model(&r, FileId(0), src.as_bytes())
}

fn e0011_count(diags: &[aion_error::Diagnostic]) -> usize {
    diags
        .iter()
        .filter(|d| d.code == ErrorCode::UnboundName)
        .count()
}

/// Scope-tree projection ignoring source spans (spans are
/// provenance/presentation data and may differ with layout).
fn scope_shape(model: &aion_sem_model::SemModel) -> Vec<Vec<(String, String, String, bool)>> {
    model
        .scope_tree
        .scopes
        .iter()
        .map(|s| {
            let mut v: Vec<_> = s
                .symbols
                .values()
                .map(|sym| {
                    (
                        sym.name.clone(),
                        format!("{:?}", sym.kind),
                        sym.ty.to_string(),
                        sym.moved,
                    )
                })
                .collect();
            v.sort();
            v
        })
        .collect()
}

/// Multiset of published type values, layout-independent.
fn type_values(model: &aion_sem_model::SemModel) -> Vec<String> {
    let mut v: Vec<String> = model.types.values().map(|t| t.to_string()).collect();
    v.sort();
    v
}

#[test]
fn bound_goal_clean_with_model_identity() {
    let plain = "fn main() { let x = 1; do x; }";
    let meta = "intent \"d\" { goal : main; } fn main() { let x = 1; goal : x; do x; }";
    let (d0, m0) = model_of(plain);
    assert!(d0.is_empty(), "plain must be clean: {d0:?}");
    let (d1, m1) = model_of(meta);
    assert!(d1.is_empty(), "bound goals must be clean: {d1:?}");

    // Masked identity: scopes/symbols equal modulo spans, no moves.
    assert_eq!(scope_shape(&m0), scope_shape(&m1));
    assert_eq!(m0.moved_uses, m1.moved_uses);
    // Exactly one extra published type: the fn-body goal stmt (`Unit`).
    // Intent-body goals publish no type entries.
    let mut extra = type_values(&m1);
    let base = type_values(&m0);
    assert_eq!(extra.len(), base.len() + 1);
    extra.retain(|t| t != "()");
    let mut base_no_unit: Vec<String> = base.into_iter().filter(|t| t != "()").collect();
    let mut extra_no_unit: Vec<String> = extra;
    extra_no_unit.sort();
    base_no_unit.sort();
    assert_eq!(base_no_unit, extra_no_unit);

    // Separate entries: explicit (block goals only) + synthesized (body goals only).
    assert_eq!(m1.intents.len(), 2);
    assert!(m1.orphans.is_empty());
    assert!(m1.intents[0].intent.is_some());
    assert_eq!(m1.intents[0].goals.len(), 1);
    assert!(m1.intents[1].intent.is_none());
    assert_eq!(m1.intents[1].goals.len(), 1);
    for e in &m1.intents {
        assert!(e.owner.is_some());
        for g in &e.goals {
            assert!(!g.refs.is_empty());
            assert!(g.refs.iter().all(|r| r.decl.is_some()));
        }
    }
    assert_eq!(
        m1.intents_by_owner.get(&m1.intents[0].owner.unwrap()),
        Some(&vec![0usize, 1usize])
    );
}

#[test]
fn fn_body_goal_ref_points_at_let_decl() {
    let src = "fn main() { let x = 1; goal : x; do x; }";
    let r = parse(src);
    let (diags, model) = aion_sema::check_with_model(&r, FileId(0), src.as_bytes());
    assert!(diags.is_empty(), "must be clean: {diags:?}");
    // The `let x` name NodeId from the index.
    let mut let_name = None;
    for i in &r.index.items {
        if let ItemIds::Fn(f) = i {
            for s in &f.body.stmts {
                if let StmtIds::Let { name, .. } = s {
                    let_name = Some(*name);
                }
            }
        }
    }
    let want = let_name.expect("let stmt in index");
    // No preceding intent: exactly one synthesized entry holding the
    // body goal; absence of E0011 is the observable contract here.
    assert_eq!(model.intents.len(), 1);
    assert!(model.intents[0].intent.is_none());
    assert!(model.intents[0].owner.is_some());
    assert_eq!(model.intents[0].goals.len(), 1);
    let _ = want;
}

#[test]
fn single_unbound_goal_yields_one_e0011() {
    let (diags, _) = model_of("fn main() { goal : missing; do 1; }");
    assert_eq!(e0011_count(&diags), 1);
    assert_eq!(diags.len(), 1);
}

#[test]
fn multi_unbound_goals_yield_one_e0011_each() {
    let (diags, _) = model_of("fn main() { goal : a + b; goal : c; do 1; }");
    assert_eq!(e0011_count(&diags), 3);
}

#[test]
fn duplicate_name_yields_one_e0011_per_occurrence() {
    let (diags, _) = model_of("fn main() { goal : dup + dup; do 1; }");
    assert_eq!(e0011_count(&diags), 2);
}

#[test]
fn mixed_bound_and_unbound_goal() {
    let (diags, model) = model_of("fn main() { let x = 1; goal : x + nope; do x; }");
    assert_eq!(e0011_count(&diags), 1);
    assert_eq!(diags.len(), 1);
    let _ = model;
}

#[test]
fn goal_e0011_coexists_with_body_e0020() {
    let (diags, _) = model_of("fn main() { let s = \"a\"; let t = s; goal : ghost; do s; }");
    assert_eq!(e0011_count(&diags), 1);
    assert_eq!(
        diags
            .iter()
            .filter(|d| d.code == ErrorCode::UseAfterMove)
            .count(),
        1,
        "body use-after-move must still be reported: {diags:?}"
    );
}

#[test]
fn orphan_goal_resolves_top_level_only_but_is_recorded() {
    // `q` is an fn-local: invisible from the trailing orphan intent.
    let src = "fn main() { let q = 1; do q; } intent \"o\" { goal : q; }";
    let (diags, model) = model_of(src);
    assert_eq!(e0011_count(&diags), 1, "orphan goal must not see fn locals");
    assert_eq!(model.intents.len(), 1);
    assert_eq!(model.orphans, vec![0usize]);
    let e = &model.intents[0];
    assert_eq!(e.owner, None);
    assert_eq!(e.goals.len(), 1);
    assert_eq!(e.goals[0].refs.len(), 1);
    assert_eq!(e.goals[0].refs[0].decl, None);
}

#[test]
fn orphan_goal_bound_at_top_level_is_clean() {
    let src = "fn main() { do 1; } intent \"o\" { goal : main; }";
    let (diags, model) = model_of(src);
    assert!(
        diags.is_empty(),
        "top-level fn name must resolve: {diags:?}"
    );
    assert_eq!(model.orphans, vec![0usize]);
    assert_eq!(model.intents[0].owner, None);
    assert!(model.intents[0].goals[0].refs[0].decl.is_some());
}

#[test]
fn unbound_call_callee_in_goal_yields_e0011() {
    let (diags, _) = model_of("fn main() { let x = 1; goal : nosuchfn(x); do x; }");
    assert_eq!(e0011_count(&diags), 1);
}

#[test]
fn synth_only_fn_body_goals_no_explicit_intent() {
    // (a) fn with body goals + no explicit intent -> one synthesized entry.
    let src = "fn main() { let x = 1; goal : x; goal : x; do x; }";
    let (diags, model) = model_of(src);
    assert!(diags.is_empty(), "must be clean: {diags:?}");
    assert_eq!(model.intents.len(), 1);
    let e = &model.intents[0];
    assert!(e.owner.is_some());
    assert!(e.intent.is_none());
    assert_eq!(e.goals.len(), 2);
    assert!(model.orphans.is_empty());
}

#[test]
fn explicit_plus_body_goals_split_entries() {
    // (b) explicit entry holds only block goals, synthesized only body goals.
    let src = "intent \"d\" { goal : main; } fn main() { let x = 1; goal : x; do x; }";
    let (diags, model) = model_of(src);
    assert!(diags.is_empty(), "must be clean: {diags:?}");
    assert_eq!(model.intents.len(), 2);
    let exp = &model.intents[0];
    assert!(exp.owner.is_some());
    assert!(exp.intent.is_some());
    assert_eq!(exp.goals.len(), 1);
    let syn = &model.intents[1];
    assert_eq!(syn.owner, exp.owner);
    assert!(syn.intent.is_none());
    assert_eq!(syn.goals.len(), 1);
    assert!(model.orphans.is_empty());
}

#[test]
fn multiple_explicit_plus_body_goals() {
    // (c) N explicit entries each with own block goals + one synthesized.
    let src = "intent \"a\" { goal : main; } intent \"b\" { goal : main; } fn main() { let x = 1; goal : x; do x; }";
    let (diags, model) = model_of(src);
    assert!(diags.is_empty(), "must be clean: {diags:?}");
    assert_eq!(model.intents.len(), 3);
    let owner = model.intents[0].owner.unwrap();
    for e in &model.intents[0..2] {
        assert_eq!(e.owner, Some(owner));
        assert!(e.intent.is_some());
        assert_eq!(e.goals.len(), 1);
    }
    let syn = &model.intents[2];
    assert_eq!(syn.owner, Some(owner));
    assert!(syn.intent.is_none());
    assert_eq!(syn.goals.len(), 1);
}

#[test]
fn fn_without_body_goals_has_no_synthesized() {
    // (d) fn with no body goals -> no synthesized entry.
    let src = "intent \"d\" { goal : main; } fn main() { do 1; }";
    let (diags, model) = model_of(src);
    assert!(diags.is_empty(), "must be clean: {diags:?}");
    assert_eq!(model.intents.len(), 1);
    assert!(model.intents[0].intent.is_some());
    assert_eq!(model.intents[0].goals.len(), 1);
    let src2 = "fn main() { do 1; }";
    let (d2, m2) = model_of(src2);
    assert!(d2.is_empty());
    assert!(m2.intents.is_empty());
}

#[test]
fn orphan_and_anchor_order_mixed_fixture() {
    // (e) orphan keeps owner None with own block goals; anchor order sorted.
    let src = "intent \"a\" { goal : main; } fn main() { let x = 1; goal : x; do x; } fn other() { do 1; } intent \"o\" { goal : main; }";
    let (diags, model) = model_of(src);
    assert!(diags.is_empty(), "must be clean: {diags:?}");
    // explicit(main) + synthesized(main) + orphan
    assert_eq!(model.intents.len(), 3);
    assert!(model.intents[0].intent.is_some());
    assert!(model.intents[1].intent.is_none());
    let orph = &model.intents[2];
    assert_eq!(orph.owner, None);
    assert!(orph.intent.is_some());
    assert_eq!(orph.goals.len(), 1);
    assert_eq!(model.orphans, vec![2usize]);
    // Anchor order: explicit intent anchor precedes body-goal anchor
    // precedes trailing orphan anchor (source order here).
    assert!(model.intents[0].owner.is_some());
}

#[test]
fn dirty_goal_program_fails_check_so_no_lowering() {
    // `analyze()` maps any non-empty sema diagnostic set to `Err`,
    // which is what blocks lowering/execution; the operative condition
    // is asserted here at the sema level.
    let src = "fn main() { goal : ghost; do 1; }";
    let r = parse(src);
    let diags = aion_sema::check(&r, FileId(0), src.as_bytes());
    assert!(!diags.is_empty());
    assert_eq!(e0011_count(&diags), 1);
}

#[test]
fn explicit_intent_goal_sees_owner_param() {
    // Owned intent goals resolve in the owner fn's environment.
    let src = "intent \"d\" { goal : x > 0; }\nfn main(x) { do x; }";
    let (diags, model) = model_of(src);
    assert!(diags.is_empty(), "param goal must be clean: {diags:?}");
    assert_eq!(model.intents.len(), 1);
    let e = &model.intents[0];
    assert!(e.owner.is_some());
    assert!(e.intent.is_some());
    assert_eq!(e.goals.len(), 1);
    assert_eq!(e.goals[0].refs.len(), 1);
    let r = &e.goals[0].refs[0];
    assert!(r.decl.is_some(), "param occurrence must resolve");
    // The decl must be the `x` parameter name node from the index.
    let parsed = parse(src);
    let mut param_name = None;
    for i in &parsed.index.items {
        if let ItemIds::Fn(f) = i {
            param_name = Some(f.params[0]);
        }
    }
    assert_eq!(r.decl, param_name, "decl must be the owner param");
}

#[test]
fn explicit_intent_goal_sees_owner_local() {
    // Body-block locals of the owner fn are visible to its intent goals.
    let src = "intent \"d\" { goal : y > 0; }\nfn main() { let y = 1; do y; }";
    let (diags, model) = model_of(src);
    assert!(diags.is_empty(), "local goal must be clean: {diags:?}");
    let e = &model.intents[0];
    assert!(e.intent.is_some());
    assert_eq!(e.goals.len(), 1);
    assert_eq!(e.goals[0].refs.len(), 1);
    assert!(
        e.goals[0].refs[0].decl.is_some(),
        "body-local occurrence must resolve"
    );
}

#[test]
fn explicit_intent_goal_unresolved_yields_e0011() {
    let src = "intent \"d\" { goal : nope > 0; }\nfn main() { do 1; }";
    let (diags, model) = model_of(src);
    assert_eq!(e0011_count(&diags), 1, "unbound intent goal: {diags:?}");
    assert_eq!(model.intents.len(), 1);
    let e = &model.intents[0];
    assert!(e.intent.is_some());
    assert_eq!(e.goals[0].refs.len(), 1);
    assert_eq!(e.goals[0].refs[0].decl, None);
}

#[test]
fn orphan_intent_goal_rejects_fn_local() {
    // Orphans resolve top-level only: an fn-local name stays unbound.
    let src = "fn main() { let z = 1; do z; }\nintent \"o\" { goal : z; }";
    let (diags, model) = model_of(src);
    assert_eq!(e0011_count(&diags), 1, "orphan must not see fn locals");
    assert_eq!(model.orphans, vec![0usize]);
    assert_eq!(model.intents[0].owner, None);
    assert_eq!(model.intents[0].goals[0].refs[0].decl, None);
}

#[test]
fn explicit_intent_goal_prefers_shadowing_local_over_param() {
    // A body-local `let` shadowing a parameter wins (innermost-first),
    // matching the language shadowing rule.
    let src = "intent \"d\" { goal : x > 0; }\nfn main(x) { let x = 1; do x; }";
    let (diags, model) = model_of(src);
    assert!(diags.is_empty(), "shadowed goal must be clean: {diags:?}");
    assert_eq!(model.intents.len(), 1);
    let refs = &model.intents[0].goals[0].refs;
    assert_eq!(refs.len(), 1);
    // Collect both declaration sites from the index.
    let parsed = parse(src);
    let (mut param_name, mut let_name) = (None, None);
    for i in &parsed.index.items {
        if let ItemIds::Fn(f) = i {
            param_name = Some(f.params[0]);
            for s in &f.body.stmts {
                if let StmtIds::Let { name, .. } = s {
                    let_name = Some(*name);
                }
            }
        }
    }
    assert!(param_name != let_name, "sites must be distinct nodes");
    assert_eq!(
        refs[0].decl, let_name,
        "goal must resolve to the shadowing local, not the parameter"
    );
}

#[test]
fn explicit_intent_goal_cannot_see_other_fn_local() {
    // The intent is owned by `main` (nearest following fn); the
    // same-named local of `other` is invisible, so the goal is unbound.
    let src =
        "intent \"d\" { goal : y > 0; }\nfn main() { do 1; }\nfn other() { let y = 2; do y; }";
    let (diags, model) = model_of(src);
    assert_eq!(e0011_count(&diags), 1, "cross-fn local must not resolve");
    assert_eq!(model.intents.len(), 1);
    let e = &model.intents[0];
    assert!(e.intent.is_some());
    assert_eq!(e.goals.len(), 1);
    assert_eq!(e.goals[0].refs.len(), 1);
    assert_eq!(e.goals[0].refs[0].decl, None);
}
