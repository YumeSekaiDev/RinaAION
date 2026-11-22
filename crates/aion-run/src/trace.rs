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
