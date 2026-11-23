# TDR-M4-8 — Assertion provenance design decisions (S1–S6 approved)

Status: S1–S6 decided by Product Owner (S6 API shape approved below).
No implementation authorized.

## S1 — Origin classification (approved)

Exactly three variants, no payloads: `HumanAuthored`, `AiGenerated`,
`Transformed`. Identity details live outside the classification.

## S2 — Default provenance (approved with clarifications)

Plain-source assertions default to `HumanAuthored` (a convention, not
verified authorship). `AiGenerated` and `Transformed` arise only
through trusted tooling interfaces; no grammar or annotation syntax is
introduced in M4.4. Provenance is non-authoritative metadata: it never alters the
outcome of type checking, inference, moves, lowering, execution,
fuel consumption, or trap decisions (pass/fail, codes, spans). Tooling supplies provenance
through parser/tooling APIs, never by manipulating AST internals.
Provenance never influences type checking, inference, move analysis,
execution, fuel accounting, or runtime trap decisions; lowering and
diagnostic plumbing may read and carry it solely for propagation and
diagnostic attribution, which never changes program semantics or
runtime behavior.
Omit-when-default serialization (per approved S5).

## S3 — AI-generated identity (approved: opaque optional string)

Generator identity is an optional string; AION never interprets its
contents; empty normalizes to absent; missing identity does not change
the `AiGenerated` classification. No model/agent/process/instance
taxonomy is standardized. Identity grants no authority. See S6 for
the API shape and S5 for serialization.

## S4 — Transformation identity and inheritance (approved: S4a–S4d)

`Transformed` carries opaque `(source_ref, transform_id)` strings set
by the transforming tool; AION interprets neither. Chaining is
flatten-to-origin: earliest `source_ref` preserved, only the latest
`transform_id` retained, no unbounded history.

- S4a. What counts: representation-only lowering and mandated
  constant folding **propagate** provenance; they never set
  `Transformed`. Only genuine semantic AST rewrites and external
  tooling that changes assertion content set it.
- S4b. A folded `assert false` retains its original provenance in the
  resulting E0044 diagnostic.
- S4c. Validation is type-system UTF-8 plus normalize-empty-to-absent
  at the API boundary. Missing identifiers never downgrade the
  `Transformed` classification.
- S4d. Cosmetic formatting and pretty-printing never count as
  transformations.

Each identifier is independently optional: a `Transformed` value with
neither, either, or both of `source_ref` / `transform_id` present is
complete and valid. Absence never triggers inference of a value, a
default, a transformation, or a reclassification.

Ordinary lowering (AST→IR shape change) and mandated constant folding
propagate the existing provenance unchanged: they never set
`Transformed`, never invent identifiers, and never change origins on
their own. Only genuine semantic rewrites and external tooling that
changes assertion content invoke the transition table.

## S5 — Serialization boundaries (approved)

Diagnostic-only boundary: assertion provenance is serialized
exclusively on `aion.diagnostic/v1` diagnostic objects. No other
envelope carries it (`aion.trace/v1` and `aion.intent/v1` unchanged;
source rendering, SemModel, and IR have no serialization and gain
none). `render_human` output is display-only and never contractual.

Wire shape: an optional `"provenance"` object key on diagnostic
objects, placed last after `"related"`. `"origin"` values are exactly
`"ai-generated"` and `"transformed"` (`HumanAuthored` is never
emitted). `AiGenerated` carries optional `"identity"` (omitted when
absent; empty normalizes to absent at the API boundary).
`Transformed` carries optional `"source_ref"` and `"transform_id"`
(each omitted when absent; missing identifiers never downgrade the
classification).

Omit-when-default: `HumanAuthored` (including all plain-source
assertions) emits no key, so existing canonical bytes are unchanged.
Serialization normalizes defensively as well: an empty identifier
string is emitted as absent (never as `""`), without changing the
origin classification.
Absent provenance in old artifacts reads as `None`, distinct
from `Some(HumanAuthored)`; no provenance is inferred. No
version bump: the optional object is additive within v1; old readers
ignore the unknown key via the established skip rule.

Attribution boundary (explicit):
1. An assertion-specific `E0010` (operand-type contract of one
   `assert` statement) carries that assertion's provenance.
2. An `E0044` AssertFailed trap carries the failing assertion's
   provenance.
3. Diagnostics from nested operand subexpressions (name resolution,
   arithmetic traps, nested type errors) never inherit the enclosing
   assertion's provenance.
4. Unrelated diagnostics never receive assertion provenance.
5. Not every `E0010` carries provenance: only the operand-type
   contract of an `assert` statement does. A reader must never infer
   assertion attribution from the diagnostic code alone.
(Asserts cannot nest, so the rule has no recursive ambiguity.)

In-memory representation: an assertion diagnostic carries
`Some(origin)` — including `Some(HumanAuthored)` — while an unrelated
diagnostic carries `None`.

Deserialization: a missing `provenance` key uniformly yields `None`,
which is distinct from `Some(HumanAuthored)` (`HumanAuthored` is an
explicit in-memory provenance value, never a fallback for missing
serialized provenance); attribution is never inferred from `E0044`
or `E0010` codes. An unknown `origin` value
fails closed with a reader error. Identifier fields inconsistent with
the declared origin are dropped on read (mirroring serialization);
empty identifier strings normalize to absent on read (never emitted
as `""`). Nested-operand and unrelated diagnostics never gain
provenance through parsing.

Required tests: existing suite as default-omission proof (all current
goldens byte-identical); new canonical goldens for each non-default
origin (with and without identifiers); missing-identifier omission
cases; parse-to-emit round-trip identity; old-reader tolerance of the
new key; no-`Debug` and canonical-form rules apply unchanged.

## S6 — Tooling API design (approved)

Approved transition rules (any genuine transformation upgrades
`HumanAuthored` or `AiGenerated` to `Transformed`):

1. Any genuine transformation changes `HumanAuthored` or `AiGenerated`
   into `Transformed`.
2. AI identity is discarded when transitioning to `Transformed`.
3. `Transformed` never downgrades to `HumanAuthored` or `AiGenerated`.
4. Preserve the earliest known `source_ref`.
5. A later transformation may supply `source_ref` if none was
   previously known (it then becomes the earliest-known reference;
   nothing is ever invented or auto-derived — see 6).
6. Never invent or automatically derive `source_ref` (no span-sniffing,
   no digesting, no inference from context).
7. `transform_id` is optional; constructors are total, never fallible
   for missing identifiers.
8. A non-empty new `transform_id` becomes the latest meaningful
   identifier.
9. With no new `transform_id`, the previous known `transform_id` is
   retained (stability over recency; an absent id carries no meaning
   to supersede with).
10. Empty identifiers normalize to absent at the API boundary.
11. Missing identifiers never change the `Transformed` classification.
12. No unbounded transformation history (flatten-to-origin only).

Transition reference (inputs post-normalization; `nsrc`/`ntid` are the
new transformation's supplied identifiers, either possibly absent):

| Current | Supplies | Result |
|---|---|---|
| `HumanAuthored` | `(nsrc, ntid)` | `Transformed`, `src = nsrc`, `tid = ntid` (either may stay absent) |
| `AiGenerated(id?)` | `(nsrc, ntid)` | `Transformed`, `src = nsrc`, `tid = ntid`, AI identity dropped |
| `Transformed(S, T)` | `(nsrc, ntid)` | `Transformed`, `src = S` if present else `nsrc` (earliest known wins; an absent `source_ref` accepts a newly supplied one), `tid = ntid` if present else `T` |

Downgrade (`Transformed` → anything else) is inexpressible: no API
for it exists, preventing provenance whitewashing.

Approved API shape: struct-variant AST (`Stmt::Assert { expr,
provenance }`); `AssertProvenance` with private fields, public
constructors (`Stmt::assert(expr)` defaulting to human;
`Stmt::assert_with_provenance(expr, p)` for explicit origins), and
read-only accessors; normalization at construction and at every
boundary (empty strings normalize to absent; mismatched identifiers
dropped on serialization and deserialization); `Clone` preservation;
formatter invisibility (no origin syntax exists to print); no generic
provenance setter (only assertion-specific constructors, so the
non-assertion case cannot arise). Side tables, wrapper types, and
payload enums rejected per S1 and the migration review. Struct
literals with mismatched fields are inert: no type checking,
inference, move analysis, execution, fuel accounting, or runtime trap
decision branches on provenance. Lowering and diagnostic plumbing
read provenance solely to propagate it (including into `E0010` /
`E0044` diagnostics); propagation never changes program semantics or
runtime behavior. The serializer drops mismatched fields and the
deserializer drops them on read.
