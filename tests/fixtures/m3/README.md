# M3 fixture corpus

Layout (all `.aion` sources are sema-clean M2-valid programs):

- `lower_valid/` — shape/span cases: lowering + validation must be clean.
- `exec/valid/` — value cases: full pipeline must execute; the golden
  `main`-body value is pinned by the sibling `.expected.json` sidecar.
- `exec/traps/` — one trap each with an exact code and source span:
  `divzero` → `AION-E0040`, `overflow` → `AION-E0041`,
  `fuel` → `AION-E0042` (run with a tiny fuel budget),
  `recursion` → `AION-E0043` (run with tiny frame budget).
- `invalid/` — no copies here: the M2 invalid fixtures under
  `tests/fixtures/m2/invalid/` are referenced by path (see
  `crates/aion-run/tests/m3_oracle_corpus.rs`). The m3 pipeline test pins
  their parser-recorded spans; the sema code+span contract itself is owned
  by `crates/aion-sema/tests/check_m2.rs`.

## Golden JSON shape

Each `exec/valid/*.expected.json` is a single object with one key,
`"value"`, whose payload encodes the expected `main`-body value
(`aion_run::Value`; note the public `execute` entry discards the body
value and always yields `Unit`, so goldens pin the pre-discard value
observed through the test-only evaluator):

```json
{"value": {"Int": 7}}
{"value": {"Str": "hi"}}
{"value": {"Bool": true}}
{"value": "Null"}
{"value": "Unit"}
```
