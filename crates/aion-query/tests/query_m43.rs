//! M4.3 entry-anchored intent/goal query tests (Suite A).
//!
//! No shared helpers with the envelope suite (`intent_json_m43.rs`);
//! every helper below is local to this file.

use aion_query::SemanticView;
use aion_span::FileId;
use aion_syntax::FeatureFlags;

fn fid() -> FileId {
    FileId(0)
}

/// Parse with all experimental gates on, require clean parse + clean
/// sema, and return the borrowed pair's owned artifacts.
fn setup(src: &str) -> (aion_syntax::ParseResult, aion_sem_model::SemModel) {
    aion_driver::analyze_with_flags(fid(), src.as_bytes(), FeatureFlags::all_experimental())
        .unwrap_or_else(|diags| {
            panic!(
                "analyze must be clean for {src:?}: {:?}",
                diags
                    .iter()
                    .map(|d| (&d.code, &d.message))
                    .collect::<Vec<_>>()
            )
        })
}

fn intents_of(src: &str) -> Vec<aion_query::IntentView> {
    let (parse, model) = setup(src);
    let view = aion_query::view(&parse, &model);
    view.entry_intents()
}

fn owners(src: &str) -> Vec<String> {
    intents_of(src).into_iter().map(|v| v.owner).collect()
}

#[test]
fn entry_anchors_main_and_callees_only() {
    let src = "intent \"on-helper\" { goal : 1; }\n\
               fn helper() { do 7; }\n\
               intent \"on-main\" { goal : 2; }\n\
               fn main() { do helper(); }\n\
               intent \"on-stray\" { goal : 3; }\n\
               fn stray() { do 0; }\n";
    // Leading intents are owned by their following fn; the trailing
    // intent has no following fn and is a retained orphan. `stray` is
    // unreachable so its entries never surface.
    let got = owners(src);
    assert_eq!(got, vec!["helper".to_string(), "main".to_string()]);
}

#[test]
fn unreachable_helper_intent_excluded_unless_reached() {
    // Intent owned by a helper that main never calls is invisible, even
    // though the model retains it.
    let src = "intent \"h\" { goal : 1; }\n\
               fn helper() { do 7; }\n\
               fn main() { do 1; }\n";
    assert!(intents_of(src).is_empty());
    // The same model retains the entry (the join test below proves the
    // query only filters the view, not the model).
    let (parse, model) = setup(src);
    assert_eq!(model.intents.len(), 1);
    assert!(model.intents[0].owner.is_some());
    let _ = parse;
}

#[test]
fn reachable_helper_intent_included() {
    let src = "intent \"h\" { goal : 1; }\n\
               fn helper() { do 7; }\n\
               fn main() { do helper(); }\n";
    assert_eq!(owners(src), vec!["helper".to_string()]);
}

#[test]
fn gate_off_program_yields_empty() {
    // Default flags: no metadata can exist, so the view is empty.
    let src = "fn main() { do 1; }";
    let (parse, model) = aion_driver::analyze(fid(), src.as_bytes())
        .expect("plain program must analyze clean on default flags");
    assert!(parse.errors.is_empty());
    let view = aion_query::view(&parse, &model);
    assert!(view.entry_intents().is_empty());
}

#[test]
fn no_main_yields_empty_never_error() {
    let src = "intent \"h\" { goal : 1; }\nfn helper() { do 7; }\n";
    assert!(intents_of(src).is_empty());
}

#[test]
fn explicit_and_synthesized_entries() {
    let src = "intent \"m\" { goal : 2; }\nfn main() { goal : 1; do 1; }\n";
    let views = intents_of(src);
    assert_eq!(views.len(), 2);
    // Anchor order: the leading intent block anchors before the body
    // goal, so the explicit entry sorts first.
    assert_eq!(views[0].desc, "m");
    assert!(views[0].span.is_some());
    assert_eq!(views[0].goals.len(), 1);
    // Synthesized entry (body goal): empty desc, no block span.
    assert_eq!(views[1].desc, "");
    assert!(views[1].span.is_none());
    assert_eq!(views[1].goals.len(), 1);
    // Goal source order is preserved in each entry.
    let src2 = "fn main() { goal : 3; goal : 4; do 1; }\n";
    let views2 = intents_of(src2);
    assert_eq!(views2.len(), 1);
    assert_eq!(views2[0].goals.len(), 2);
}

#[test]
fn orphan_intent_excluded() {
    // Trailing intent block has no following fn: retained orphan,
    // invisible to entry queries.
    let src = "fn main() { do 1; }\nintent \"orphan\" { goal : 1; }\n";
    assert!(intents_of(src).is_empty());
    let (_, model) = setup(src);
    assert_eq!(model.orphans.len(), 1);
}

#[test]
fn anchor_order_ascending() {
    let src = "intent \"h\" { goal : 1; }\n\
               fn helper() { do 7; }\n\
               intent \"m\" { goal : 2; }\n\
               fn main() { do helper(); }\n";
    let views = intents_of(src);
    assert_eq!(views.len(), 2);
    let anchors: Vec<(u32, u32, u32)> = views
        .iter()
        .map(|v| {
            let sp = v.span.expect("explicit entries carry block spans");
            (sp.file.0, sp.start, sp.end)
        })
        .collect();
    let mut sorted = anchors.clone();
    sorted.sort();
    assert_eq!(anchors, sorted, "entries must sort ascending by anchor");
    assert_eq!(views[0].owner, "helper");
    assert_eq!(views[1].owner, "main");
}

#[test]
fn shared_model_join_is_field_for_field() {
    // One model, two views: every view field must match the underlying
    // model entry (goal stmt spans, ref occurrence spans, ref decls).
    let src = "intent \"h\" { goal : helper(1); }\n\
               fn helper(a) { goal : a; do a + 1; }\n\
               fn main() { do helper(1); }\n";
    let (parse, model) = setup(src);
    let view: SemanticView = aion_query::view(&parse, &model);
    let views = view.entry_intents();
    assert_eq!(views.len(), 2);
    // The synthesized helper entry holds the body goal; the explicit
    // entry holds the block goal.
    let synth = views
        .iter()
        .find(|v| v.span.is_none())
        .expect("synthesized entry");
    let explicit = views
        .iter()
        .find(|v| v.span.is_some())
        .expect("explicit entry");
    assert_eq!(synth.owner, "helper");
    assert_eq!(explicit.owner, "helper");
    assert_eq!(explicit.desc, "h");
    // Join synthesized goal: its span must equal the model stmt span.
    let synth_model = model
        .intents
        .iter()
        .find(|e| e.intent.is_none() && e.goals.len() == synth.goals.len())
        .expect("synthesized model entry");
    for (g_model, g_view) in synth_model.goals.iter().zip(synth.goals.iter()) {
        assert_eq!(parse.spans.get(g_model.stmt), g_view.span);
        assert_eq!(g_model.refs.len(), g_view.refs.len());
        // Occurrence spans sorted ascending, matching model use-sites.
        let key = |s: Option<aion_span::ByteSpan>| {
            s.map(|v| (v.file.0, v.start, v.end))
                .unwrap_or((u32::MAX, u32::MAX, u32::MAX))
        };
        let mut use_spans: Vec<_> = g_model
            .refs
            .iter()
            .map(|r| parse.spans.get(r.use_site))
            .collect();
        use_spans.sort_by_key(|s| key(*s));
        let view_spans: Vec<_> = g_view.refs.iter().map(|r| r.occurrence).collect();
        assert_eq!(use_spans, view_spans);
        for (r_model, r_view) in g_model.refs.iter().zip(g_view.refs.iter()) {
            assert_eq!(r_model.decl.is_some(), r_view.bound);
        }
    }
    // Join explicit entry: single block goal `helper(1)` with callee ref.
    let explicit_model = model
        .intents
        .iter()
        .find(|e| e.intent.is_some() && e.goals.len() == explicit.goals.len())
        .expect("explicit model entry");
    assert_eq!(explicit_model.goals.len(), 1);
    assert_eq!(explicit.goals.len(), 1);
    assert!(!explicit.goals[0].refs.is_empty(), "call callee is a ref");
    assert!(explicit.goals[0].refs.iter().all(|r| r.bound));
}

#[test]
fn twice_run_canonical_bytes_equal() {
    let src = "intent \"h\" { goal : helper(1); }\n\
               fn helper(a) { goal : a; do a + 1; }\n\
               fn main() { do helper(1); }\n";
    let (parse1, model1) = setup(src);
    let (parse2, model2) = setup(src);
    let entry_of = |parse: &aion_syntax::ParseResult, model: &aion_sem_model::SemModel| {
        let view = aion_query::view(parse, model);
        let name = view.entry_point().ok().map(|h| h.name);
        let intents = view.entry_intents();
        (name, intents)
    };
    let (e1, i1) = entry_of(&parse1, &model1);
    let (e2, i2) = entry_of(&parse2, &model2);
    assert_eq!(e1, e2);
    let b1 = aion_query::intent_canonical_bytes(e1.as_deref(), &i1);
    let b2 = aion_query::intent_canonical_bytes(e2.as_deref(), &i2);
    assert_eq!(
        b1, b2,
        "identical input must yield byte-identical envelopes"
    );
}

#[test]
fn contract_is_canonical_bytes_never_debug() {
    let src = "intent \"m\" { goal : 3; }\nfn main() { goal : 1 + 2; do 1; }\n";
    let (parse, model) = setup(src);
    let view = aion_query::view(&parse, &model);
    let name = view.entry_point().ok().map(|h| h.name);
    let intents = view.entry_intents();
    let bytes = aion_query::intent_canonical_bytes(name.as_deref(), &intents);
    let text = std::str::from_utf8(&bytes).expect("envelope is UTF-8");
    assert!(text.starts_with("{\"schema\":\"aion.intent/v1\",\"version\":1,"));
    assert!(text.ends_with("}\n"));
    assert!(!text.contains("NodeId"), "no raw NodeIds may appear");
    assert!(text.contains("\"entry\":{\"name\":\"main\"}"));
}
