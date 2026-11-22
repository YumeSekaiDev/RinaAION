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
        match &e.kind {
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
        }
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
        let mut env = Env::new();
        for ((name, _), v) in f.params.iter().zip(args.into_iter()) {
            env.define(name, v);
        }
        let r = self.eval_block(&f.body, &mut env, module);
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

#[cfg(test)]
mod oracle;
#[cfg(test)]
mod tests;
