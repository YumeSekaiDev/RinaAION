//! M4.4 milestone M2: E0016 blocks analysis (no model is produced).

use aion_driver::analyze_with_flags;
use aion_error::ErrorCode;
use aion_span::FileId;
use aion_syntax::FeatureFlags;

#[test]
fn e0016_blocks_analysis() {
    let src = b"intent \"d\" { assert true; } fn main() {}";
    let err = analyze_with_flags(FileId(0), src, FeatureFlags::all_experimental())
        .expect_err("E0016 must block analysis");
    assert_eq!(err.len(), 1, "exactly one blocking diagnostic");
    let d = &err[0];
    assert_eq!(d.code, ErrorCode::AssertInIntent);
    assert_eq!(d.code.code_str(), "AION-E0016");
    assert_eq!(
        d.message,
        "assert statements are not allowed in intent blocks"
    );
    assert_eq!(d.severity, aion_error::Severity::Error);
}
