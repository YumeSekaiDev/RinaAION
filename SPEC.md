# RINA SPEC (M2)

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
| `AION-E0044` and above | (reserved) | future runtime traps |

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
```
