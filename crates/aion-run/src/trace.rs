//! M4.1 internal test-only execution observer.
//!
//! Contract:
//! - No public API change: everything here is `pub(crate)`.
//! - The hook fires only after `charge()` succeeds, never charges fuel,
//!   never panics, never does I/O.
//! - Events: per-expression evaluations plus call boundaries plus traps.
//!   NO block events.
//! - `Str` values are recorded as `{len, hash}` where `len` is the UTF-8
//!   byte length and `hash` is FNV-1a 64 hex; full contents never recorded.
//! - Bounded buffer with a deterministic truncation marker.
//! - No RNG, no time, no locale-sensitive formatting anywhere here.

use std::cell::RefCell;

use aion_span::ByteSpan;

use super::Value;

/// Maximum retained events per execution.
pub(crate) const CAP: usize = 4096;

/// FNV-1a 64-bit. Empty string hashes to the offset basis.
pub(crate) fn fnv1a_hex(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// JSON-escape a string (ASCII-safe, deterministic).
pub(crate) fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                o.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => o.push(c),
        }
    }
    o
}

/// Summary of a runtime value. Never contains string contents.
pub(crate) fn value_summary(v: &Value) -> String {
    match v {
        Value::Int(i) => format!("{{\"type\":\"int\",\"value\":{i}}}"),
        Value::Bool(b) => format!("{{\"type\":\"bool\",\"value\":{b}}}"),
        Value::Null => "{\"type\":\"null\"}".to_string(),
        Value::Unit => "{\"type\":\"unit\"}".to_string(),
        Value::Str(s) => format!(
            "{{\"type\":\"string\",\"len\":{},\"hash\":\"{}\"}}",
            s.len(),
            fnv1a_hex(s)
        ),
    }
}

#[derive(Clone)]
pub(crate) struct Observer {
    enabled: bool,
    kept: u64,
    truncated: bool,
    events: Vec<String>,
}

impl Observer {
    fn new() -> Self {
        Self {
            enabled: false,
            kept: 0,
            truncated: false,
            events: Vec::new(),
        }
    }
}

thread_local! {
    static OBS: RefCell<Observer> = RefCell::new(Observer::new());
}

/// Run `f` with the observer enabled (cap [`CAP`]). Returns `f`'s value
/// plus the canonical trace bytes for the events recorded during `f`.
/// Test-only entry point (unit tests live in the crate); allowed dead in
/// non-test builds where only the recording hooks are linked.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn with_observer<R>(f: impl FnOnce() -> R) -> (R, Vec<u8>) {
    OBS.with(|o| {
        let mut ob = o.borrow_mut();
        ob.enabled = true;
        ob.kept = 0;
        ob.truncated = false;
        ob.events.clear();
    });
    let r = f();
    let bytes = OBS.with(|o| {
        let mut ob = o.borrow_mut();
        ob.enabled = false;
        let b = canonical_bytes(&ob.events, ob.truncated, ob.kept);
        ob.events.clear();
        b
    });
    (r, bytes)
}

/// True while inside [`with_observer`].
pub(crate) fn enabled() -> bool {
    OBS.with(|o| o.borrow().enabled)
}

/// Record one pre-serialized event object. Drops (and latches the
/// truncation flag) once `kept == CAP`. Total: never panics.
pub(crate) fn record(event_json: String) {
    OBS.with(|o| {
        let mut ob = o.borrow_mut();
        if !ob.enabled {
            return;
        }
        if ob.events.len() < CAP {
            ob.events.push(event_json);
            ob.kept += 1;
        } else {
            ob.truncated = true;
        }
    });
}

/// Record a trap event. Total: never panics.
pub(crate) fn record_trap(code: &str, file: u32, start: u32, end: u32, depth: u64) {
    // seq = kept count so far (next index); best-effort, never panics.
    let seq: u64 = OBS.with(|o| o.borrow().kept);
    record(format!(
        "{{\"seq\":{seq},\"kind\":\"trap\",\"op\":\"trap\",\"ty\":\"trap\",\"span\":{{\"file\":{file},\"start\":{start},\"end\":{end}}},\"depth\":{depth},\"fuel_used\":0,\"value\":{{\"type\":\"trap\",\"code\":\"{}\"}}}}",
        esc(code),
    ));
}

/// Canonical trace bytes: fixed key order, UTF-8, LF terminator.
/// Test-only entry point; see [`with_observer`].
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn canonical_bytes(events: &[String], truncated: bool, kept: u64) -> Vec<u8> {
    let mut s = String::from("{\"schema\":\"aion.trace/v1\",\"version\":1,\"events\":[");
    for (i, e) in events.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(e);
    }
    s.push_str(&format!(
        "],\"truncated\":{{\"truncated\":{},\"kept\":{kept}}}}}",
        if truncated { "true" } else { "false" }
    ));
    s.push('\n');
    s.into_bytes()
}

/// Build one `expr` event object with fixed key order.
pub(crate) fn expr_event(
    seq: u64,
    op: &str,
    ty: &str,
    span: ByteSpan,
    depth: u64,
    fuel_used: u64,
    value_json: &str,
) -> String {
    format!(
        "{{\"seq\":{seq},\"kind\":\"expr\",\"op\":\"{}\",\"ty\":\"{}\",\"span\":{{\"file\":{},\"start\":{},\"end\":{}}},\"depth\":{depth},\"fuel_used\":{fuel_used},\"value\":{value_json}}}",
        esc(op),
        esc(ty),
        span.file.0,
        span.start,
        span.end,
    )
}

/// Build one call-boundary event object with fixed key order.
pub(crate) fn call_event(
    seq: u64,
    enter: bool,
    callee: &str,
    span: ByteSpan,
    depth: u64,
    fuel_used: u64,
) -> String {
    format!(
        "{{\"seq\":{seq},\"kind\":\"{}\",\"op\":\"{}\",\"ty\":\"()\",\"span\":{{\"file\":{},\"start\":{},\"end\":{}}},\"depth\":{depth},\"fuel_used\":{fuel_used},\"value\":{{\"type\":\"unit\"}}}}",
        if enter { "call_enter" } else { "call_exit" },
        esc(callee),
        span.file.0,
        span.start,
        span.end,
    )
}

/// Next sequence number (= events kept so far). Total: never panics.
pub(crate) fn next_seq() -> u64 {
    OBS.with(|o| o.borrow().kept)
}

/// Parsed canonical trace envelope (M4.2, test/tooling-side). Events are
/// kept as raw canonical event objects so a parse → re-emit round-trip
/// through [`canonical_bytes`] is byte-identical.
pub struct ParsedTrace {
    /// Raw canonical event objects, in order.
    pub events: Vec<String>,
    /// Truncation flag from the envelope.
    pub truncated: bool,
    /// Kept-event count from the envelope.
    pub kept: u64,
}

/// Canonical trace envelope reader (M4.2, test/tooling-side).
///
/// Accepts bytes produced by [`canonical_bytes`]: UTF-8, single LF
/// terminator, `schema` `"aion.trace/v1"`. Only major `version` `1` is
/// accepted (missing or non-`1` rejected); unknown fields are ignored.
/// Errors are plain strings, never diagnostics.
pub fn parse_envelope(bytes: &[u8]) -> Result<ParsedTrace, String> {
    let s = std::str::from_utf8(bytes).map_err(|e| format!("trace envelope is not UTF-8: {e}"))?;
    let body = s
        .strip_suffix('\n')
        .ok_or_else(|| "trace envelope missing LF terminator".to_string())?;
    if body.contains('\n') || body.contains('\r') {
        return Err("trace envelope must be a single line".to_string());
    }
    let mut p = Cur::new(body);
    p.expect(b'{')?;
    let mut schema: Option<String> = None;
    let mut version: Option<u64> = None;
    let mut events_raw: Option<String> = None;
    let mut truncated_raw: Option<String> = None;
    loop {
        p.ws();
        if p.eat(b'}') {
            break;
        }
        let key = p.string()?;
        p.ws();
        p.expect(b':')?;
        p.ws();
        match key.as_str() {
            "schema" => schema = Some(p.string()?),
            "version" => version = Some(p.number()?),
            "events" => events_raw = Some(p.raw_value()?),
            "truncated" => truncated_raw = Some(p.raw_value()?),
            _ => {
                p.skip_value()?;
            }
        }
        p.ws();
        if p.eat(b',') {
            continue;
        }
        p.ws();
        p.expect(b'}')?;
        break;
    }
    match schema.as_deref() {
        Some("aion.trace/v1") => {}
        Some(other) => return Err(format!("unsupported trace schema: {other}")),
        None => return Err("trace envelope missing schema".to_string()),
    }
    match version {
        Some(1) => {}
        Some(v) => return Err(format!("unsupported major version: {v}")),
        None => return Err("trace envelope missing version".to_string()),
    }
    let events_raw = events_raw.ok_or_else(|| "trace envelope missing events".to_string())?;
    let truncated_raw =
        truncated_raw.ok_or_else(|| "trace envelope missing truncated".to_string())?;
    let events = split_array_raw(&events_raw)?;
    let (truncated, kept) = parse_truncated(&truncated_raw)?;
    Ok(ParsedTrace {
        events,
        truncated,
        kept,
    })
}

fn split_array_raw(raw: &str) -> Result<Vec<String>, String> {
    let mut p = Cur::new(raw);
    p.expect(b'[')?;
    let mut out = Vec::new();
    p.ws();
    if p.eat(b']') {
        return Ok(out);
    }
    loop {
        p.ws();
        out.push(p.raw_value()?);
        p.ws();
        if p.eat(b',') {
            continue;
        }
        p.ws();
        p.expect(b']')?;
        break;
    }
    Ok(out)
}

fn parse_truncated(raw: &str) -> Result<(bool, u64), String> {
    let mut p = Cur::new(raw);
    p.expect(b'{')?;
    let mut truncated: Option<bool> = None;
    let mut kept: Option<u64> = None;
    loop {
        p.ws();
        if p.eat(b'}') {
            break;
        }
        let key = p.string()?;
        p.ws();
        p.expect(b':')?;
        p.ws();
        match key.as_str() {
            "truncated" => {
                if p.peek("true") {
                    p.literal("true")?;
                    truncated = Some(true);
                } else if p.peek("false") {
                    p.literal("false")?;
                    truncated = Some(false);
                } else {
                    return Err("truncated flag must be bool".to_string());
                }
            }
            "kept" => kept = Some(p.number()?),
            _ => {
                p.skip_value()?;
            }
        }
        p.ws();
        if p.eat(b',') {
            continue;
        }
        p.ws();
        p.expect(b'}')?;
        break;
    }
    match (truncated, kept) {
        (Some(t), Some(k)) => Ok((t, k)),
        _ => Err("truncated marker missing field".to_string()),
    }
}

/// Minimal JSON cursor (byte-indexed; input is UTF-8).
struct Cur<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Cur<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            b: s.as_bytes(),
            pos: 0,
        }
    }
    fn ws(&mut self) {
        while self.pos < self.b.len() && matches!(self.b[self.pos], b' ' | b'\t') {
            self.pos += 1;
        }
    }
    fn eat(&mut self, c: u8) -> bool {
        if self.pos < self.b.len() && self.b[self.pos] == c {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, c: u8) -> Result<(), String> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(format!("expected `{}`", c as char))
        }
    }
    fn peek(&self, lit: &str) -> bool {
        self.b.len() >= self.pos + lit.len()
            && &self.b[self.pos..self.pos + lit.len()] == lit.as_bytes()
    }
    fn literal(&mut self, lit: &str) -> Result<(), String> {
        if self.peek(lit) {
            self.pos += lit.len();
            Ok(())
        } else {
            Err(format!("expected `{lit}`"))
        }
    }
    fn number(&mut self) -> Result<u64, String> {
        let start = self.pos;
        while self.pos < self.b.len() && self.b[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        if start == self.pos {
            return Err("expected number".to_string());
        }
        std::str::from_utf8(&self.b[start..self.pos])
            .map_err(|e| e.to_string())?
            .parse::<u64>()
            .map_err(|e| e.to_string())
    }
    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            if self.pos >= self.b.len() {
                return Err("unterminated string".to_string());
            }
            match self.b[self.pos] {
                b'"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.pos += 1;
                    if self.pos >= self.b.len() {
                        return Err("unterminated escape".to_string());
                    }
                    match self.b[self.pos] {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000C}'),
                        b'u' => {
                            if self.pos + 5 > self.b.len() {
                                return Err("bad \\u escape".to_string());
                            }
                            let hex = std::str::from_utf8(&self.b[self.pos + 1..self.pos + 5])
                                .map_err(|e| e.to_string())?;
                            let cp = u32::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
                            out.push(
                                char::from_u32(cp).ok_or_else(|| "bad codepoint".to_string())?,
                            );
                            self.pos += 4;
                        }
                        c => return Err(format!("bad escape `\\{}`", c as char)),
                    }
                    self.pos += 1;
                }
                _ => {
                    let ch_len = std::str::from_utf8(&self.b[self.pos..])
                        .map_err(|e| e.to_string())?
                        .chars()
                        .next()
                        .ok_or_else(|| "unterminated string".to_string())?
                        .len_utf8();
                    out.push_str(
                        std::str::from_utf8(&self.b[self.pos..self.pos + ch_len])
                            .map_err(|e| e.to_string())?,
                    );
                    self.pos += ch_len;
                }
            }
        }
    }
    fn raw_value(&mut self) -> Result<String, String> {
        self.ws();
        if self.pos >= self.b.len() {
            return Err("unexpected end".to_string());
        }
        match self.b[self.pos] {
            b'"' => {
                let start = self.pos;
                let _ = self.string()?;
                std::str::from_utf8(&self.b[start..self.pos])
                    .map(|s| s.to_string())
                    .map_err(|e| e.to_string())
            }
            b'{' | b'[' => {
                let start = self.pos;
                let open = self.b[self.pos];
                let close = if open == b'{' { b'}' } else { b']' };
                let mut depth = 0usize;
                let mut in_str = false;
                let mut esc = false;
                while self.pos < self.b.len() {
                    let c = self.b[self.pos];
                    if in_str {
                        if esc {
                            esc = false;
                        } else if c == b'\\' {
                            esc = true;
                        } else if c == b'"' {
                            in_str = false;
                        }
                    } else if c == b'"' {
                        in_str = true;
                    } else if c == open {
                        depth += 1;
                    } else if c == close {
                        depth -= 1;
                        if depth == 0 {
                            self.pos += 1;
                            break;
                        }
                    }
                    self.pos += 1;
                }
                if depth != 0 {
                    return Err("unterminated value".to_string());
                }
                std::str::from_utf8(&self.b[start..self.pos])
                    .map(|s| s.to_string())
                    .map_err(|e| e.to_string())
            }
            b't' => {
                self.literal("true")?;
                Ok("true".to_string())
            }
            b'f' => {
                self.literal("false")?;
                Ok("false".to_string())
            }
            b'n' => {
                self.literal("null")?;
                Ok("null".to_string())
            }
            c if c.is_ascii_digit() => {
                let n = self.number()?;
                Ok(n.to_string())
            }
            _ => Err("unexpected value".to_string()),
        }
    }
    fn skip_value(&mut self) -> Result<(), String> {
        self.raw_value().map(|_| ())
    }
}
