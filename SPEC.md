# RINA SPEC (M4)

## Sec 5 — M4 normative (intent/goal exposure, assertions, determinism, JSON contract)

M4 is docs-plus-gated-behavior only: Secs 2–4 are unchanged except the
Sec 4 code-table row naming `AION-E0044` AssertFailed (with `E0045+`
reserved); no other Sec 2–4 rule changes (the
`GatedGoal = 'goal' ':' Expr ';'` row in Appendix A already records the
Goal(Expr) adoption; no `goal(Ident)` form exists). All M4 surface
(intent blocks, goal fields, `assert` statements) is experimental,
gated, off by default, and rejected with a diagnostic unless explicitly
enabled. Default-off programs behave exactly as M3.

### Intent/goal exposure (queryable metadata, default-off, non-interference)

- `intent` blocks and goal statements are queryable metadata attached
  to program items, exposed through the entry-point query surface only.
- Ownership: each top-level `intent` block is owned by the nearest
  following `fn` item in source order (R-FOLLOWING). An `intent` block
  with no following `fn` is an *orphan*: it is retained in the
  semantic model but excluded from entry-point queries and from the
  normal intent envelope. Multiple `intent` blocks under one item
  accumulate independently, in source order; no merging.
  Synthesized entries: for each function, if its body contains one
  or more body goals, create exactly one synthesized entry containing
  those goals, regardless of whether the function also has explicit
  intent entries. A synthesized entry has no
  intent block: its description is empty (and omitted from JSON) and
  it carries no intent-block span. Explicit intent entries hold only
  their own block-body goals. Synthesized entries are created only
  when body goals exist, are owned by their function like any other
  entry, and are never orphans.
  Ordering: every entry is anchored at a source span — explicit
  entries at their intent-block span, synthesized entries at the span
  of their first body goal in source order. Entries sort ascending by
  anchor `(file, start, end)`; goals within each entry remain in
  source order.
- Entry scope: entry-anchored queries return intents owned by `fn
  main` (zero-parameter) and by its transitively reachable callees.
  Analysis entry: `analyze()` keeps its default-flags behavior
  unchanged; `analyze_with_flags(file, bytes, flags)` composes
  `parse_with_flags()` with `check_with_model()` for enabled
  analysis. Both share the M4.2 success/failure contract.
  Unreachable-item intents and orphans are retained in the model but
  invisible to entry queries.
- Non-interference theorem: for any executable program P and the
  otherwise identical program P+metadata (same items, statements, and
  expressions, plus intent/goal metadata), execution results, semantic
  traces, fuel consumption, and trap identity/payload must be
  identical between the two variants. Metadata never affects execution.
  Source spans, line/column positions, and source excerpts are
  provenance/presentation data: they may differ when metadata changes
  source layout. Canonical diagnostic comparisons must separate
  semantic diagnostic fields from source-location fields — never
  require byte-identical source spans across differently laid-out
  source variants. Within each individual source variant, source spans
  remain deterministic. The acceptance test executes both variants and
  compares results, fuel, and trap identity/payload; cross-path trace
  equivalence is deferred until AION has a traced-execution interface
  (the public trace serializer output is covered by its golden test).
- Goal form: `goal : Expr ;` (`Goal(Expr)`) — a goal carries a full `Expr`, its source span and referenced binding ids. A goal whose expression happens
  to be a single identifier (e.g. `goal : x ;`) is a valid `Goal(Expr)`
  and serializes as `{"kind":"ident","name":"x"}`; there is no
  separate identifier-only production.
- Goal checking: name resolution only. Goals resolve in their owning
  function's lexical environment; orphan-block goals resolve in the
  top-level environment only and are never attributed to a function.
  Unbound names yield `AION-E0011` (UnboundName) at the identifier
  span; E0011 from goals is blocking: a program with an unbound goal
  name fails semantic checking and is not executed, like any other M2
  diagnostic. No type constraints are imposed, no ownership move facts
  are recorded, and goals contribute nothing to block values, scopes,
  or moves.

### Assertions

- Static: Bool checking plus literal folding only. No solver. An
  `assert` operand must type as `Bool`; otherwise the M2 `AION-E0010`
  (TypeMismatch) rule applies. Still-unresolved (`Var`-typed) and
  `Error`-typed operands are exempt: the former fails later at the
  `E0014` lowering gate, the latter already failed elsewhere. Any
  other non-`Bool` operand type yields `E0010`.
- `TypedStmt::Assert` is its own statement node, never routed via `Do`.
  It is a 0-cost wrapper: on success it yields nothing (see no-output
  rule below).
- Operand fuel: evaluating the operand costs normal per-node fuel (each
  IR expression node 1 unit, charged once on first evaluation, per
  Sec 4). The wrapper itself adds no fuel.
- Literal folding at lowering: literal-`true` asserts are eliminated (no
  runtime node, no fuel). Literal-`false` asserts become an unconditional
  `AION-E0044` trap with no operand evaluation.
- Eligible runtime asserts evaluate their operand; `true` succeeds
  silently, `false` traps `AION-E0044` (AssertFailed) at the statement
  span. The diagnostic message is `assertion failed` with Error
  severity. No `notes` or `related` locations are attached.
- Evaluation order: the operand is fully evaluated first; operand
  traps and fuel exhaustion during evaluation take precedence over
  `E0044`. `E0044` is considered only after successful evaluation
  produces `false`. Exception: literal-`false` traps unconditionally
  without evaluating the operand and without consulting fuel.
- Assert reads are non-moving reads that produce no ownership move
  facts: evaluating the operand never moves a `Str`, and no
  access-authority move fact is recorded for assert-operand reads.
  This applies recursively through nested calls and subexpressions,
  while reading an already-moved value inside an operand still
  reports `E0020` (only new move recording is suppressed, never
  existing diagnostics).
- Provenance metadata: every assertion carries an origin — one of
  `human-authored`, `ai-generated`, or `transformed` (the latter
  carrying optional tool-supplied `source_ref` and `transform_id`
  identifiers; ordinary lowering and mandated constant folding preserve
  existing provenance unchanged and never set `Transformed` or invent
  identifiers — only genuine semantic rewrites or external tooling
  that changes assertion content invoke the transformation rules).
  Provenance is
  preserved into diagnostics but grants no authority and changes no
  rule: all assertions follow identical static and execution rules
  regardless of origin. Provenance never counts as proof.

### Entry-point query

Execution still starts at zero-parameter `fn main` (Sec 4 entry rule).
Tooling may query intent/goal metadata anchored at the entry point
(shape in TDR-M4-3); queries are read-only and never alter execution.

### `aion.intent/v1` envelope

- Schema `"aion.intent/v1"`, `"version": 1`. Key order: `schema`,
  `version`, `entry`, `intents`. Omit-when-empty is preserved
  throughout; the envelope is LF-terminated; canonical bytes are the
  contract and `Debug` output is never contractual. Readers reject
  unknown major versions and ignore unknown fields (policy in
  TDR-M4-1); within major version 1 only additive field additions are
  allowed.
- `entry`: `{"name": "main"}` when the zero-parameter entry point
  resolves; omitted when there is no entry point. A missing entry
  yields an empty envelope, never an error; `AION-E0015` remains the
  execution path's concern.
- `intents`: array in source order, entry scope only (orphan blocks
  and unreachable-item intents never appear). Each entry:
  `{"owner": {"item": "<fn-name>", "span": {file,start,end}},
  "span": {file,start,end}, "desc": "...", "goals": [...]}`.
  `owner.span` is the whole-item span; `span` is the intent block's
  span; all spans are complete `{file,start,end}` objects and are
  never synthesized (missing span means the field is omitted).
  `desc` is the parser-provided optional description, `""` when
  absent, and is omitted from JSON when empty. The Appendix A
  `GatedIntent` row does not record the description slot; that gap is
  logged as a specification issue and is out of M4.3 scope.
- `goals`: array in source order per intent. Each goal:
  `{"goal_ast": <expr>, "goal_span": {file,start,end}, "refs": [...]}`.
  `goal_ast` is the closed 8-kind vocabulary below; no other node
  kinds exist.
- Expression vocabulary (frozen at M4.3; mirrors the AST field names —
  struct variants reuse their field names, tuple variants gain one
  key):

  | AST | JSON | Fields |
  | --- | ---- | ------ |
  | `Int(i64)` | `{"kind":"int","value":N}` | `value`: JSON number, full i64 range |
  | `Str(String)` | `{"kind":"str","value":S}` | `value`: the AST's unquoted content as a JSON string |
  | `Bool(bool)` | `{"kind":"bool","value":B}` | `value`: JSON `true`/`false` |
  | `Null` | `{"kind":"null"}` | no other fields |
  | `Ident(String)` | `{"kind":"ident","name":S}` | `name`: the identifier text |
  | `Unary{op,rhs}` | `{"kind":"unary","op":O,"rhs":E}` | `op`: `"-"` (`Neg`) or `"!"` (`Not`); `rhs`: goal-expr, recursed |
  | `Binary{op,lhs,rhs}` | `{"kind":"binary","op":O,"lhs":E,"rhs":E}` | `op`: one of `"||","&&","==","!=","<","<=",">",">=","+","-","*","/","%"`; `lhs`/`rhs`: goal-exprs, recursed |
  | `Call{callee,args}` | `{"kind":"call","callee":S,"args":[...]}` | `callee`: bare name string (the AST has no callee expression); `args`: array of goal-exprs, possibly empty |

  Parenthesised source yields the identical node (parens are
  precedence-only); nesting recurses with this same table.
- `refs`: one entry per identifier occurrence in the goal, sorted by
  occurrence span. `refs` covers every resolved-or-unbound name
  occurrence in the goal expression, including call callees
  (occurrence = callee identifier span); argument expressions recurse
  normally. Bound:
  `{"name","status":"bound","occurrence":{file,start,end},
  "decl":{"name","span":{file,start,end}}}`. Unresolved:
  `{"name","status":"unresolved","occurrence":{...},"decl":null,
  "diagnostic":"AION-E0011"}`. `decl` names the declaration and its
  name span, which distinguishes shadowed declarations sharing a
  name. No type information appears (name-resolution-only rule). Raw
  `NodeId`s never appear anywhere in the envelope.
- Freeze note: this vocabulary and field list are frozen at M4.3 and
  recorded here before any golden tests are written.

### Determinism

- Canonical trace form and canonical diagnostic form are the normative
  observable outputs. Determinism gate (M4.1) runs before M4.3/M4.4
  work: identical input plus configuration yields byte-identical
  canonical forms — seed independence (no RNG input) and locale
  independence (no locale-dependent rendering) hold.
- `Debug` formatting is explicitly unstable and never part of the
  contract; only the canonical forms are compared.

### JSON contract pointers

- Omit-when-empty is preserved: empty `notes`/`related` fields are
  omitted (never `null`/empty-list drift) per the established envelope.
- Versioning convention (policy in TDR-M4-1): every JSON envelope
  carries `"schema": "aion.<artifact>/v<MAJOR>"` and `"version":
  <MAJOR>` (integer, starting at 1), e.g. `"schema":
  "aion.diagnostic/v1", "version": 1`. Within a major version, only
  additive field additions are allowed; renames, removals, retypings,
  or semantic changes require a major bump plus a SPEC note.
  Consumers must reject unknown major versions and must ignore
  unknown fields (forward compatibility for additive changes).
- `Debug` output is explicitly unstable and must not be snapshot-tested.

### Extended code table (additions)

| Code | Name | Meaning |
| ---- | ---- | ------- |
| `AION-E0044` | AssertFailed | `assert` operand was `false` (statement span); literal-`false` traps unconditionally without operand eval |
| `AION-E0045` and above | (reserved) | future runtime traps |

`AION-E0044` is a runtime trap (faults during execution), joining the
`AION-E0040`+ class in Sec 4.

### No-output rule interaction

A successful `assert` yields nothing observable: no value, no output,
no state write — same class as `do`'s no-observable-effect rule. Only a
failing assert is observable, as an `AION-E0044` trap diagnostic.

## Appendix A — Grammar (normative)

## Sec 2 — Syntax (normative)

> Normative grammar lives in Appendix A. `syntax/grammar.ungram` is
> derived from Appendix A; see its header. On conflict, SPEC wins.

M1 stable syntax (TDR-6):
- A program is a sequence of items; the only stable item is a function
  declaration `fn Ident ( Params ) Block`.
- A block is `{` followed by zero or more statements followed by `}`.
- Statements: `let Ident = Expr ;`, `do Expr ;`, or a nested block.
- Expressions with standard precedence (lowest to highest): `||`, `&&`,
  `==`/`!=`, `<`/`<=`/`>`/`>=`, `+`/`-`, `*`/`/`/`%`, unary `!`/`-`,
  then primary (literal, identifier, call `Ident ( args )`, parenthesised).
- Literals: decimal integers, double-quoted strings with `\` escapes,
  `true`/`false`, `null`.
- Delimiters: statements end with `;`; blocks and argument lists use braces
  and parentheses. Comments (`//` line, `/* */` block) and whitespace are
  trivia: preserved in the CST, invisible to the AST.
- Spans are byte spans (`ByteSpan`) over the raw input; the parser is total
  (no panic on arbitrary bytes or invalid UTF-8; invalid bytes are `Invalid`
  tokens with `AION-E0001` diagnostics, other errors become `Error` nodes
  with diagnostics; parsing always yields a tree plus diagnostics).
- Experimental (gated, off by default, rejected with a diagnostic unless
  explicitly enabled): `intent` blocks, `goal` fields, `assert` statements.

## Sec 3 — Types and ownership (M2 normative)

M2 adds static typing and ownership over the M1 syntax (Sec 2 /
Appendix A). There is NO annotation syntax in M2: no `TyExpr` or `RetAnn`
productions exist, and none are added to Appendix A. All types are inferred
(inference-first); a function returns the type of the trailing `do` Expr in
its body block, or `Unit` when the body has no trailing `do` Expr.

### `null` literal

`null` is a reserved literal of type `Null`. It is not an identifier and
cannot be bound, assigned, or called. NOTE (known gap): the M1 parser
currently reads `null` as an `Ident`; WS-C will fix the parser to produce
the `Lit` node. Until then, typechecking treats the `null` keyword position
as the `Null` literal.

### Type universe (closed)

Exactly six types; no generics, no subtyping:

| Type   | Introduced by |
| ------ | ------------- |
| `Int`  | integer literal |
| `Str`  | string literal |
| `Bool` | `true` / `false` |
| `Null` | `null` |
| `Unit` | block with no trailing `do` Expr |
| `Fn`   | `fn` declaration (param types and return type inferred) |

### Operator typing

| Operator(s) | Operand types | Result |
| ----------- | ------------- | ------ |
| `\|\|` `&&` | `Bool`, `Bool` | `Bool` |
| unary `-`, binary `+` `-` `*` `/` `%` | `Int`, `Int` | `Int` |
| `<` `<=` `>` `>=` | `Int`, `Int` | `Bool` |
| `==` `!=` | two identical ground types (`Int`/`Str`/`Bool`/`Null`/`Unit`) | `Bool` |
| unary `!` | `Bool` | `Bool` |

Any other operand combination is `AION-E0010` (TypeMismatch). The diagnostic
span is the operator plus its operand(s).

### Ownership and bindings

- `Int`, `Bool`, `Null`, `Unit` are `Copy`: reuse after binding is free.
- `Str` is owned: a `let`-binding takes ownership, and passing a `Str` as a
  call argument moves it. Using a moved `Str` is `AION-E0020` (UseAfterMove).
- Unresolved-type rule: move analysis runs on post-inference resolved types.
  A type variable still unresolved after inference (genuinely unknown, never
  constrained to a `Copy` ground type) is treated as owned for move purposes
  (sound direction: a move error is never missed). Once later constraints
  resolve such a variable to a `Copy` ground type (`Int`/`Bool`/`Null`/`Unit`),
  the move is retracted (no diagnostic).
- Return ownership: a `Str`-typed block value is owned by the caller on
  return. Returning a local owned `Str` transfers it (the caller binding is
  a fresh owned value); returning a `Str` parameter moves it. Call results
  are independent owned values: binding a call result and then moving it
  follows the normal `let`-move rules (reusing it afterwards is `AION-E0020`).
  `Copy` returns never move.
- Function parameters are immutable bindings; they cannot be reassigned.
- A `let` that reuses a name in the same scope is shadowing: the old binding
  is hidden, not an error, and (for `Str`) the old value is dropped.
- `AION-E0013` (DuplicateDefinition) covers exactly two cases: duplicate
  function names at the top level, and duplicate parameter names within one
  function. Shadowing by `let` is not duplication.

### Calls

Call arity (argument count vs parameter count) is checked at each call site;
a mismatch is `AION-E0012` (ArityMismatch) spanning the call argument list.
Referring to a name with no visible binding is `AION-E0011` (UnboundName)
spanning the identifier.

### Diagnostics

- `AION-E0010` TypeMismatch: span = the offending operator and operands.
- `AION-E0011` UnboundName: span = the identifier.
- `AION-E0012` ArityMismatch: span = the call argument list.
- `AION-E0013` DuplicateDefinition: span = the second (duplicate) definition
  site; the first definition site is attached as related.
- `AION-E0020` UseAfterMove: span = the use site; the earlier move site is
  attached as related.

## Sec 4 — Execution (M3 normative)

M3 executes the M2-checked program. Sema (M2) must be clean before
execution; any residual M2 diagnostic blocks execution (no IR emitted).

### Entry rule

Execution starts at a zero-parameter `fn main`. A program with no `main`
function, or whose `main` takes one or more parameters, is `AION-E0015`
(MissingEntry, span = the offending `main` declaration if present, else
the end of input). Duplicate `main` definitions stay `AION-E0013`
(DuplicateDefinition, span = the second definition site).

### Body-value rule

A function body evaluates to the value of its trailing `do` Expr, else
`Unit` when the body has no trailing `do` Expr. Nested blocks are
recursive: a nested block statement evaluates the same way (its trailing
`do` Expr, else `Unit`). `main` discards its body value; the program
result is always `Unit`.

### Access-authority rule

A move (of a `Str` value) executes iff the M2 move analysis recorded a
move fact for that use. Exactly four move sites exist:

1. passing a `Str` as a call argument moves it;
2. binding a `Str` identifier with `let` moves it into the new binding;
3. returning a local owned `Str` transfers it to the caller;
4. returning a `Str` parameter moves it.

Any other use of a `Str` is a borrow/read, never a move. The executor
performs no independent move inference.

### Emission-strictness rule

Lowering to IR requires a fully resolved program: any residual `Var`
(unresolved type variable after M2 inference) is `AION-E0014`
(NotLowerable, span = the expression or binding carrying the variable).
No IR is emitted when `AION-E0014` fires.

### Fuel and frame rules

Each IR expression node costs 1 fuel unit, charged exactly once on first
evaluation. Defaults: fuel limit 1,000,000 units, call-frame limit 1024
frames; both are configurable. Exceeding fuel is `AION-E0042`
(FuelExhausted); exceeding frames is `AION-E0043` (StackOverflow).

### Checked arithmetic and traps

Integer arithmetic is checked: overflow, division by zero, and remainder
by zero trap (execution aborts with a trap diagnostic at the operator
span). There is no wrapping, no partial result, and no recovery.

### No-output rule

`do` has no observable effect: it only yields its Expr value to the
enclosing block. M3 has no `print`, no `emit`, and no host calls; nothing
in M3 produces output, writes state, or calls out of the program.

### Extended code table

| Code | Name | Meaning |
| ---- | ---- | ------- |
| `AION-E0014` | UnresolvedType | residual `Var` at lowering; no IR emitted |
| `AION-E0015` | EntryPointInvalid | no zero-param `fn main`, or bad signature |
| `AION-E0030` | InternalIrInvalid | internal IR invariant violation; always a compiler bug, never user error |
| `AION-E0031`–`AION-E0039` | (reserved) | future internal diagnostics |
| `AION-E0040` | DivByZero | integer division/remainder by zero |
| `AION-E0041` | IntOverflow | checked-arithmetic overflow (incl. `MIN / -1`, negation of `MIN`) |
| `AION-E0042` | FuelExhausted | fuel limit exceeded |
| `AION-E0043` | StackOverflow | call-frame limit exceeded |
| `AION-E0044` | AssertFailed | `assert` operand was `false` (statement span; literal-`false` traps unconditionally without operand eval — see Sec 5) |
| `AION-E0045` and above | (reserved) | future runtime traps |

`AION-E0014`/`AION-E0015` are static faults (no execution).
`AION-E0030`–`AION-E0039` are internal compiler faults (never user error).
`AION-E0040`+ are runtime traps (faults during execution).

### `null`-as-literal confirmation

The parser is authoritative for the `null` literal: it produces the `Lit`
node. Sema's keyword-position fallback (Sec 3) is defensive only and must
agree with the parser; on conflict the parser wins.

## Appendix A — Grammar (normative)

```ungram
// Mirrors syntax/grammar.ungram (M1 stable subset + gated experimental).
Program = Item*
Item = FuncDecl
FuncDecl = 'fn' Ident '(' Params ')' Block
Params = Ident (',' Ident)*
Block = '{' Stmt* '}'
Stmt = LetStmt | DoStmt | Block
LetStmt = 'let' Ident '=' Expr ';'
DoStmt = 'do' Expr ';'
Expr = OrExpr
OrExpr = AndExpr ('||' AndExpr)*
AndExpr = CmpExpr ('&&' CmpExpr)*
CmpExpr = AddExpr (('==' | '!=' | '<' | '<=' | '>' | '>=') AddExpr)*
AddExpr = MulExpr (('+' | '-') MulExpr)*
MulExpr = UnaryExpr (('*' | '/' | '%') UnaryExpr)*
UnaryExpr = ('!' | '-') UnaryExpr | Primary
Primary = Lit | Ident | Call | '(' Expr ')'
Call = Ident '(' Args ')'
Args = Expr (',' Expr)*
Lit = Int | Str | 'true' | 'false' | 'null'
Ident = '[A-Za-z_][A-Za-z0-9_]*'
Int = '[0-9]+'
Str = '"...' '"'
GatedIntent = 'intent' Block
GatedGoal = 'goal' ':' Expr ';'
GatedAssert = 'assert' Expr ';'
// M4: gated rows are experimental, off by default (no default-behavior
// change). GatedGoal is the Goal(Expr) form: full Expr + span/ids; there
// is no separate identifier-only production (a goal whose Expr is a
// single Ident is a valid Goal(Expr)).
```
