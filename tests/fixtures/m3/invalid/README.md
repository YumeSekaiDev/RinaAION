# invalid/ — by reference, no copies

M3 adds no new static diagnostics, so there are no M3-specific invalid
fixtures. The M2 invalid fixtures under `tests/fixtures/m2/invalid/`
(`e0010`–`e0020` family) are reused by path: the m3 harness
(`crates/aion-run/tests/m3_oracle_corpus.rs`) loads them, asserts they
parse clean (the fault is semantic, not syntactic), and pins the exact
parser-recorded span each sema diagnostic points at. The sema code+span
contract itself is owned by `crates/aion-sema/tests/check_m2.rs`; the m3
test only guards the span chain it consumes.
