use aion_span::FileId;
use aion_syntax::parse;

#[test]
fn golden_smoke_valid_fixture_parses_and_prints() {
    let src = include_bytes!("../../../tests/fixture_min.aion");
    let r = parse(FileId(0), src);
    let printed = format!("{:?}", r.root);
    assert!(!printed.is_empty());
    assert!(
        r.errors.is_empty(),
        "valid fixture must parse clean: {:?}",
        r.errors
    );
}
