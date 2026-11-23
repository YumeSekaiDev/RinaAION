//! `aion-run`: M3.4 tree-walking interpreter over typed IR.
//!
//! Total: [`execute`] never panics on any input (all indexing via
//! `.get()`, checked arithmetic, explicit depth counter).

use aion_error::{Diagnostic, ErrorCode};
use aion_ir::{ExprKind, LocalAccess, TypedBlock, TypedExpr, ValidatedModule};
use aion_span::ByteSpan;
use aion_syntax::{BinOp, UnOp};
use std::collections::HashMap;

/// Runtime value. Exactly these variants; validated IR never contains
/// anything else (impossible-by-construction cases trap `AION-E0030`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Int(i64),
    Str(String),
    Bool(bool),
    Null,
    Unit,
}

/// Successful outcome: program value plus fuel consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub value: Value,
    pub steps_used: u64,
}

/// Either a value or a trap diagnostic.
#[derive(Debug, Clone)]
pub enum RunResult {
    Ok(Outcome),
    Trap(Diagnostic),
}

/// Execution limits. Defaults: fuel 1_000_000, frames 1024.
#[derive(Debug, Clone)]
pub struct Config {
    pub fuel: u64,
    pub frames: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            fuel: 1_000_000,
            frames: 1024,
        }
    }
}

/// Static entry pre-pass: index of the zero-parameter `fn main`.
///
/// `AION-E0015` when no `main` exists or `main` takes parameters.
/// Span: the offending `main`'s declaration span when one exists
/// (threaded from the parser index at lowering time), else the
/// module's end-of-input span (missing `main` = end of input).
pub fn find_entry(module: &ValidatedModule) -> Result<usize, Diagnostic> {
    let inner = module.module();
    for (i, f) in inner.functions.iter().enumerate() {
        if f.name == "main" {
            if f.params.is_empty() {
                return Ok(i);
            }
            return Err(Diagnostic::new(
                ErrorCode::EntryPointInvalid,
                "entry `main` must take no parameters",
            )
            .with_span(f.decl_span));
        }
    }
    Err(Diagnostic::new(
        ErrorCode::EntryPointInvalid,
        "missing entry: no zero-parameter `fn main`",
    )
    .with_span(inner.eof_span))
}

/// Execute a validated module. The `&ValidatedModule` parameter is the
/// whole point: only successfully validated IR can reach execution, so
/// feeding unvalidated (hand-built or stale) IR is a compile-time error
/// — there is no overload accepting a bare `TypedModule`.
/// `main`'s body value is discarded; the program result is always `Unit`.
pub fn execute(module: &ValidatedModule, config: &Config) -> RunResult {
    let entry = match find_entry(module) {
        Ok(i) => i,
        Err(d) => return RunResult::Trap(d),
    };
    let inner = module.module();
    let main = match inner.functions.get(entry) {
        Some(f) => f,
        None => {
            return RunResult::Trap(Diagnostic::new(
                ErrorCode::InternalIrInvalid,
                "internal interpreter error: entry index out of bounds",
            ));
        }
    };
    let mut st = State {
        fuel: config.fuel,
        used: 0,
        frames: config.frames,
        depth: 0,
    };
    let mut env = Env::new();
    match st.eval_block(&main.body, &mut env, module) {
        Ok(_) => RunResult::Ok(Outcome {
            value: Value::Unit,
            steps_used: st.used,
        }),
        Err(d) => RunResult::Trap(d),
    }
}

// ---------------------------------------------------------------------------
// Interpreter state
// ---------------------------------------------------------------------------

/// A slot: `None` = moved away (tombstone).
type Slot = Option<Value>;

struct Env {
    scopes: Vec<HashMap<String, Slot>>,
}

impl Env {
    fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
        }
    }
    fn push(&mut self) {
        self.scopes.push(HashMap::new());
    }
    fn pop(&mut self) {
        self.scopes.pop();
    }
    fn define(&mut self, name: &str, v: Value) {
        if let Some(top) = self.scopes.last_mut() {
            top.insert(name.to_string(), Some(v));
        }
    }
    fn lookup(&self, name: &str) -> Option<&Slot> {
        for s in self.scopes.iter().rev() {
            if let Some(slot) = s.get(name) {
                return Some(slot);
            }
        }
        None
    }
    fn take(&mut self, name: &str) -> Option<Slot> {
        for s in self.scopes.iter_mut().rev() {
            if s.contains_key(name) {
                return s.get_mut(name).map(|slot| slot.take());
            }
        }
        None
    }
}

struct State {
    fuel: u64,
    used: u64,
    frames: usize,
    depth: usize,
}

impl State {
    /// Charge 1 fuel for evaluating a node. `false` = exhausted.
    fn charge(&mut self) -> bool {
        if self.used >= self.fuel {
            return false;
        }
        self.used += 1;
        true
    }

    fn eval_block(
        &mut self,
        block: &TypedBlock,
        env: &mut Env,
        module: &ValidatedModule,
    ) -> Result<Value, Diagnostic> {
        env.push();
        let mut trailing: Option<Value> = None;
        let n = block.stmts.len();
        for (i, stmt) in block.stmts.iter().enumerate() {
            let last = i + 1 == n;
            match stmt {
                aion_ir::TypedStmt::Let { name, init, .. } => {
                    let v = self.eval_expr(init, env, module)?;
                    env.define(name, v);
                }
                aion_ir::TypedStmt::Do { expr } => {
                    let v = self.eval_expr(expr, env, module)?;
                    if last {
                        trailing = Some(v);
                    }
                }
                aion_ir::TypedStmt::Block { body } => {
                    let v = self.eval_block(body, env, module)?;
                    if last {
                        trailing = Some(v);
                    }
                }
                aion_ir::TypedStmt::Empty => {}
            }
        }
        env.pop();
        Ok(trailing.unwrap_or(Value::Unit))
    }

    fn eval_expr(
        &mut self,
        e: &TypedExpr,
        env: &mut Env,
        module: &ValidatedModule,
    ) -> Result<Value, Diagnostic> {
        if !self.charge() {
            return Err(
                Diagnostic::new(ErrorCode::FuelExhausted, "fuel limit exceeded").with_span(e.span),
            );
        }
        // M4.1 observer hook: fires only after charge() succeeded, never
        // charges fuel, never panics, never does I/O. Evaluation continues
        // identically whether or not the observer is enabled.
        let _obs_fuel = self.used;
        let _obs_depth = self.depth as u64;
        let outcome: Result<Value, Diagnostic> = match &e.kind {
            ExprKind::Int(v) => Ok(Value::Int(*v)),
            ExprKind::Str(s) => Ok(Value::Str(s.clone())),
            ExprKind::Bool(v) => Ok(Value::Bool(*v)),
            ExprKind::Null => Ok(Value::Null),
            ExprKind::Local { name, access } => match access {
                LocalAccess::Copy => match env.lookup(name) {
                    Some(Some(v)) => Ok(v.clone()),
                    _ => Err(Diagnostic::new(
                        ErrorCode::InternalIrInvalid,
                        format!("internal interpreter error: unusable slot `{name}`"),
                    )
                    .with_span(e.span)),
                },
                LocalAccess::Move => match env.take(name) {
                    Some(Some(v)) => Ok(v),
                    _ => Err(Diagnostic::new(
                        ErrorCode::InternalIrInvalid,
                        format!("internal interpreter error: cannot move slot `{name}`"),
                    )
                    .with_span(e.span)),
                },
            },
            ExprKind::Unary { op, rhs } => {
                let v = self.eval_expr(rhs, env, module)?;
                self.eval_unary(*op, v, e.span)
            }
            ExprKind::Binary { op, lhs, rhs } => match op {
                BinOp::Or => {
                    let l = self.eval_expr(lhs, env, module)?;
                    match l {
                        Value::Bool(true) => Ok(Value::Bool(true)),
                        Value::Bool(false) => {
                            let r = self.eval_expr(rhs, env, module)?;
                            match r {
                                Value::Bool(b) => Ok(Value::Bool(b)),
                                _ => Err(internal(e, "non-bool `||` operand")),
                            }
                        }
                        _ => Err(internal(e, "non-bool `||` operand")),
                    }
                }
                BinOp::And => {
                    let l = self.eval_expr(lhs, env, module)?;
                    match l {
                        Value::Bool(false) => Ok(Value::Bool(false)),
                        Value::Bool(true) => {
                            let r = self.eval_expr(rhs, env, module)?;
                            match r {
                                Value::Bool(b) => Ok(Value::Bool(b)),
                                _ => Err(internal(e, "non-bool `&&` operand")),
                            }
                        }
                        _ => Err(internal(e, "non-bool `&&` operand")),
                    }
                }
                _ => {
                    let l = self.eval_expr(lhs, env, module)?;
                    let r = self.eval_expr(rhs, env, module)?;
                    self.eval_binary(*op, l, r, e.span)
                }
            },
            ExprKind::Call { callee, args } => {
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval_expr(a, env, module)?);
                }
                self.eval_call(callee, vals, e.span, module)
            }
        };
        match &outcome {
            Ok(v) => {
                Self::observe_expr(e, _obs_depth, _obs_fuel, Some(v));
            }
            Err(d) => {
                Self::observe_expr(e, _obs_depth, _obs_fuel, None);
                let code = d.code.code_str().to_string();
                if trace::enabled() {
                    trace::record_trap(&code, e.span.file.0, e.span.start, e.span.end, _obs_depth);
                }
            }
        }
        outcome
    }

    /// M4.1 observer record for one expression evaluation. Best-effort:
    /// any internal failure yields no event, never a panic, never fuel use.
    fn observe_expr(e: &TypedExpr, depth: u64, fuel_used: u64, val: Option<&Value>) {
        if !trace::enabled() {
            return;
        }
        let op: &str = match &e.kind {
            ExprKind::Int(_) => "int",
            ExprKind::Str(_) => "str",
            ExprKind::Bool(_) => "bool",
            ExprKind::Null => "null",
            ExprKind::Local { access, .. } => match access {
                LocalAccess::Copy => "local.copy",
                LocalAccess::Move => "local.move",
            },
            ExprKind::Unary { op, .. } => match op {
                UnOp::Not => "unary.not",
                UnOp::Neg => "unary.neg",
            },
            ExprKind::Binary { op, .. } => match op {
                BinOp::Or => "binary.or",
                BinOp::And => "binary.and",
                BinOp::Eq => "binary.eq",
                BinOp::Ne => "binary.ne",
                BinOp::Lt => "binary.lt",
                BinOp::Le => "binary.le",
                BinOp::Gt => "binary.gt",
                BinOp::Ge => "binary.ge",
                BinOp::Add => "binary.add",
                BinOp::Sub => "binary.sub",
                BinOp::Mul => "binary.mul",
                BinOp::Div => "binary.div",
                BinOp::Mod => "binary.mod",
            },
            ExprKind::Call { .. } => "call",
        };
        let ty = e.ty.to_string();
        let value_json = match val {
            Some(v) => trace::value_summary(v),
            None => "{\"type\":\"unknown\"}".to_string(),
        };
        let seq = trace::next_seq();
        trace::record(trace::expr_event(
            seq,
            op,
            &ty,
            e.span,
            depth,
            fuel_used,
            &value_json,
        ));
    }

    fn eval_unary(&self, op: UnOp, v: Value, span: ByteSpan) -> Result<Value, Diagnostic> {
        match (op, v) {
            (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
            (UnOp::Neg, Value::Int(i)) => match i.checked_neg() {
                Some(n) => Ok(Value::Int(n)),
                None => Err(int_overflow(span)),
            },
            _ => Err(Diagnostic::new(
                ErrorCode::InternalIrInvalid,
                "internal interpreter error: bad unary operand",
            )
            .with_span(span)),
        }
    }

    fn eval_binary(
        &self,
        op: BinOp,
        l: Value,
        r: Value,
        span: ByteSpan,
    ) -> Result<Value, Diagnostic> {
        match (op, l, r) {
            (BinOp::Add, Value::Int(a), Value::Int(b)) => match a.checked_add(b) {
                Some(n) => Ok(Value::Int(n)),
                None => Err(int_overflow(span)),
            },
            (BinOp::Sub, Value::Int(a), Value::Int(b)) => match a.checked_sub(b) {
                Some(n) => Ok(Value::Int(n)),
                None => Err(int_overflow(span)),
            },
            (BinOp::Mul, Value::Int(a), Value::Int(b)) => match a.checked_mul(b) {
                Some(n) => Ok(Value::Int(n)),
                None => Err(int_overflow(span)),
            },
            (BinOp::Div, Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    return Err(div_zero(span));
                }
                match a.checked_div(b) {
                    Some(n) => Ok(Value::Int(n)),
                    None => Err(int_overflow(span)),
                }
            }
            (BinOp::Mod, Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    return Err(div_zero(span));
                }
                match a.checked_rem(b) {
                    Some(n) => Ok(Value::Int(n)),
                    None => Err(int_overflow(span)),
                }
            }
            (BinOp::Lt, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a < b)),
            (BinOp::Le, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a <= b)),
            (BinOp::Gt, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a > b)),
            (BinOp::Ge, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a >= b)),
            (BinOp::Eq, a, b) => Ok(Value::Bool(values_eq(&a, &b, span)?)),
            (BinOp::Ne, a, b) => Ok(Value::Bool(!values_eq(&a, &b, span)?)),
            (BinOp::Or, _, _) | (BinOp::And, _, _) => Err(Diagnostic::new(
                ErrorCode::InternalIrInvalid,
                "internal interpreter error: lazy op in strict position",
            )
            .with_span(span)),
            _ => Err(Diagnostic::new(
                ErrorCode::InternalIrInvalid,
                "internal interpreter error: bad binary operands",
            )
            .with_span(span)),
        }
    }

    fn eval_call(
        &mut self,
        callee: &str,
        args: Vec<Value>,
        span: ByteSpan,
        module: &ValidatedModule,
    ) -> Result<Value, Diagnostic> {
        let f = module
            .module()
            .functions
            .iter()
            .find(|f| f.name == *callee)
            .ok_or_else(|| {
                Diagnostic::new(
                    ErrorCode::InternalIrInvalid,
                    format!("internal interpreter error: unknown callee `{callee}`"),
                )
                .with_span(span)
            })?;
        if args.len() != f.params.len() {
            return Err(Diagnostic::new(
                ErrorCode::InternalIrInvalid,
                format!("internal interpreter error: arity mismatch calling `{callee}`"),
            )
            .with_span(span));
        }
        if self.depth + 1 > self.frames {
            return Err(
                Diagnostic::new(ErrorCode::StackOverflow, "call-frame limit exceeded")
                    .with_span(span),
            );
        }
        self.depth += 1;
        if trace::enabled() {
            let seq = trace::next_seq();
            trace::record(trace::call_event(
                seq,
                true,
                callee,
                span,
                self.depth as u64,
                self.used,
            ));
        }
        let mut env = Env::new();
        for ((name, _), v) in f.params.iter().zip(args.into_iter()) {
            env.define(name, v);
        }
        let r = self.eval_block(&f.body, &mut env, module);
        if trace::enabled() {
            let seq = trace::next_seq();
            trace::record(trace::call_event(
                seq,
                false,
                callee,
                span,
                self.depth as u64,
                self.used,
            ));
        }
        self.depth -= 1;
        r
    }
}

fn values_eq(a: &Value, b: &Value, span: ByteSpan) -> Result<bool, Diagnostic> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Ok(x == y),
        (Value::Str(x), Value::Str(y)) => Ok(x == y),
        (Value::Bool(x), Value::Bool(y)) => Ok(x == y),
        (Value::Null, Value::Null) => Ok(true),
        (Value::Unit, Value::Unit) => Ok(true),
        _ => Err(Diagnostic::new(
            ErrorCode::InternalIrInvalid,
            "internal interpreter error: `==` on mismatched values",
        )
        .with_span(span)),
    }
}

fn div_zero(span: ByteSpan) -> Diagnostic {
    Diagnostic::new(ErrorCode::DivByZero, "integer division by zero").with_span(span)
}

fn int_overflow(span: ByteSpan) -> Diagnostic {
    Diagnostic::new(ErrorCode::IntOverflow, "integer overflow").with_span(span)
}

fn internal(e: &TypedExpr, msg: &str) -> Diagnostic {
    Diagnostic::new(
        ErrorCode::InternalIrInvalid,
        format!("internal interpreter error: {msg}"),
    )
    .with_span(e.span)
}

/// Parsed canonical trace envelope (M4.2, test/tooling-side). Events are
/// raw canonical event objects so a parse → re-emit round-trip through
/// [`trace_canonical_bytes`] is byte-identical.
#[derive(Debug, Clone)]
pub struct ParsedTrace {
    /// Raw canonical event objects, in order.
    pub events: Vec<String>,
    /// Truncation flag from the envelope.
    pub truncated: bool,
    /// Kept-event count from the envelope.
    pub kept: u64,
}

/// Canonical trace envelope reader (M4.2, test/tooling-side): version-checked
/// (`aion.trace/v1`, major `1` only), unknown fields ignored, plain `String`
/// errors (never diagnostics).
pub fn parse_trace_envelope(bytes: &[u8]) -> Result<ParsedTrace, String> {
    let p = trace::parse_envelope(bytes)?;
    Ok(ParsedTrace {
        events: p.events,
        truncated: p.truncated,
        kept: p.kept,
    })
}

/// Canonical trace emitter for tooling round-trips: fixed key order, UTF-8,
/// single LF terminator.
pub fn trace_canonical_bytes(events: &[String], truncated: bool, kept: u64) -> Vec<u8> {
    trace::canonical_bytes(events, truncated, kept)
}

#[cfg(test)]
mod m4;
#[cfg(test)]
mod m4_trace_compat;
#[cfg(test)]
mod oracle;
#[cfg(test)]
mod tests;
mod trace;
