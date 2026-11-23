# Specification issue — call argument list vs zero-argument calls

Status: recorded, out of scope for M4.3. No action taken.

## Discrepancy

SPEC Appendix A defines `Args = Expr (',' Expr)*`, which admits one or
more arguments. The parser and the existing test corpus accept
zero-argument calls (`f()`).

## Disposition

This file records the discrepancy only. Resolving it (grammar-row
correction or parser restriction) is explicitly outside the M4.3 scope
and requires a separate Product Owner decision. The `aion.intent/v1`
`call` vocabulary permits an empty `args` array, following the code.
