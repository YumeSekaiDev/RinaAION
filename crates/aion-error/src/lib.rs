//! `aion-error`: stable diagnostic schema (M0).
//!
//! Codes `AION-E0001`.. are stable across releases. Rendering has two
//! forms: human (one line + optional span + notes) and JSON (one object
//! per diagnostic, no external dependencies).

use aion_span::ByteSpan;
use std::fmt;

/// Stable severities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Error => write!(f, "error"),
            Severity::Warning => write!(f, "warning"),
            Severity::Note => write!(f, "note"),
        }
    }
}

/// Stable diagnostic codes. Discriminants are the numeric part of
/// `AION-E00xx`. Do not renumber; only append.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ErrorCode {
    /// Invalid byte / invalid UTF-8 where text was required.
    InvalidByte = 1,
    /// Unexpected token during lexing/parsing.
    UnexpectedToken = 2,
    /// Unexpected end of input.
    UnexpectedEof = 3,
    /// Generic parse error (stub parser fallback).
    ParseError = 4,
    /// Type mismatch.
    TypeMismatch = 10,
    /// Unbound name.
    UnboundName = 11,
    /// Arity mismatch.
    ArityMismatch = 12,
    /// Duplicate definition.
    DuplicateDefinition = 13,
    /// Residual type variable at lowering (not lowerable, no IR emitted).
    UnresolvedType = 14,
    /// Use after move.
    UseAfterMove = 20,
    /// Internal compiler fault; indicates a compiler bug, never a user error.
    InternalIrInvalid = 30,
    /// Missing / bad entry point (no zero-param `fn main`).
    EntryPointInvalid = 15,
    /// `assert` statement inside an intent block (M4.4, static rejection).
    AssertInIntent = 16,
    /// Integer division / remainder by zero.
    DivByZero = 40,
    /// Integer overflow (incl. `MIN / -1` and negation of `MIN`).
    IntOverflow = 41,
    /// Fuel limit exceeded.
    FuelExhausted = 42,
    /// Call-frame limit exceeded.
    StackOverflow = 43,
    /// Reserved future runtime fault.
    RuntimeTrap = 44,
}

impl ErrorCode {
    pub fn code_str(self) -> &'static str {
        match self {
            ErrorCode::InvalidByte => "AION-E0001",
            ErrorCode::UnexpectedToken => "AION-E0002",
            ErrorCode::UnexpectedEof => "AION-E0003",
            ErrorCode::ParseError => "AION-E0004",
            ErrorCode::TypeMismatch => "AION-E0010",
            ErrorCode::UnboundName => "AION-E0011",
            ErrorCode::ArityMismatch => "AION-E0012",
            ErrorCode::DuplicateDefinition => "AION-E0013",
            ErrorCode::UnresolvedType => "AION-E0014",
            ErrorCode::UseAfterMove => "AION-E0020",
            ErrorCode::InternalIrInvalid => "AION-E0030",
            ErrorCode::EntryPointInvalid => "AION-E0015",
            ErrorCode::AssertInIntent => "AION-E0016",
            ErrorCode::DivByZero => "AION-E0040",
            ErrorCode::IntOverflow => "AION-E0041",
            ErrorCode::FuelExhausted => "AION-E0042",
            ErrorCode::StackOverflow => "AION-E0043",
            ErrorCode::RuntimeTrap => "AION-E0044",
        }
    }
}

/// Assertion provenance origin (M4.4, S1).
///
/// Identity details are separate from this classification. Provenance is
/// non-authoritative metadata: it never influences type checking,
/// inference, move analysis, execution, fuel accounting, or trap
/// decisions.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AssertOrigin {
    /// Plain source assertion (default convention, not verified authorship).
    #[default]
    HumanAuthored,
    /// Marked `AiGenerated` by trusted tooling via the approved API.
    AiGenerated,
    /// Marked `Transformed` by a genuine semantic rewrite or external
    /// tooling that changed assertion content.
    Transformed,
}

/// Assertion provenance (M4.4, S1–S4, S6).
///
/// Fields are private; construction goes through the constructors below
/// and read access through the accessors. Empty identifier strings
/// normalize to absent at every boundary.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AssertProvenance {
    origin: AssertOrigin,
    generator: Option<String>,
    source_ref: Option<String>,
    transform_id: Option<String>,
}

fn normalize_id(s: Option<String>) -> Option<String> {
    match s {
        Some(v) if v.is_empty() => None,
        other => other,
    }
}

impl AssertProvenance {
    /// Plain source assertion: `HumanAuthored`, no identifiers.
    pub fn human_authored() -> Self {
        Self {
            origin: AssertOrigin::HumanAuthored,
            generator: None,
            source_ref: None,
            transform_id: None,
        }
    }

    /// `AiGenerated` with optional opaque generator identity.
    /// Empty identity normalizes to absent; missing identity does not
    /// change the classification.
    pub fn ai_generated(generator: Option<impl Into<String>>) -> Self {
        Self {
            origin: AssertOrigin::AiGenerated,
            generator: normalize_id(generator.map(Into::into)),
            source_ref: None,
            transform_id: None,
        }
    }

    /// `Transformed` with optional opaque identifiers. Empty identifiers
    /// normalize to absent; missing identifiers never downgrade the
    /// classification.
    pub fn transformed<S: Into<String>, T: Into<String>>(
        source_ref: Option<S>,
        transform_id: Option<T>,
    ) -> Self {
        Self {
            origin: AssertOrigin::Transformed,
            generator: None,
            source_ref: normalize_id(source_ref.map(Into::into)),
            transform_id: normalize_id(transform_id.map(Into::into)),
        }
    }

    /// Origin classification.
    pub fn origin(&self) -> AssertOrigin {
        self.origin.clone()
    }

    /// Apply a genuine semantic transformation (M4.4 M1).
    ///
    /// Only genuine semantic AST rewrites or external tooling that
    /// changes assertion content may call this; formatting-only and
    /// representation-only changes must preserve provenance instead.
    /// Existing fields are first normalized by the current origin
    /// (incompatible identifiers are discarded), then the approved
    /// transition rules apply: earliest non-empty `source_ref` is
    /// preserved (a new one is accepted only when none was known), a
    /// new non-empty `transform_id` replaces the previous one (otherwise
    /// retained), AI identity is discarded, origins never downgrade,
    /// and no history is retained.
    pub fn apply_transform<S: Into<String>, T: Into<String>>(
        &self,
        source_ref: Option<S>,
        transform_id: Option<T>,
    ) -> Self {
        let new_src = normalize_id(source_ref.map(Into::into));
        let new_tid = normalize_id(transform_id.map(Into::into));
        let (kept_src, kept_tid) = match self.origin {
            AssertOrigin::Transformed => (self.source_ref.clone(), self.transform_id.clone()),
            _ => (None, None),
        };
        Self {
            origin: AssertOrigin::Transformed,
            generator: None,
            source_ref: kept_src.or(new_src),
            transform_id: new_tid.or(kept_tid),
        }
    }

    /// Opaque generator identity (`AiGenerated` only).
    pub fn generator(&self) -> Option<&str> {
        self.generator.as_deref()
    }

    /// Earliest known non-empty source reference (`Transformed` only).
    pub fn source_ref(&self) -> Option<&str> {
        self.source_ref.as_deref()
    }

    /// Latest meaningful transformation identifier (`Transformed` only).
    pub fn transform_id(&self) -> Option<&str> {
        self.transform_id.as_deref()
    }
}

/// One diagnostic.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub code: ErrorCode,
    pub severity: Severity,
    pub message: String,
    pub span: Option<ByteSpan>,
    pub notes: Vec<String>,
    pub related: Vec<(ByteSpan, String)>,
    /// In-memory assertion provenance (M4.4). `Some` marks an
    /// assertion-related diagnostic; `None` marks an unrelated one.
    /// Omitted from canonical JSON when `None` or `HumanAuthored`
    /// (serialization belongs to M5).
    pub provenance: Option<AssertProvenance>,
}

impl Diagnostic {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: Severity::Error,
            message: message.into(),
            span: None,
            notes: Vec::new(),
            related: Vec::new(),
            provenance: None,
        }
    }

    pub fn with_span(mut self, span: ByteSpan) -> Self {
        self.span = Some(span);
        self
    }

    pub fn with_severity(mut self, sev: Severity) -> Self {
        self.severity = sev;
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn with_related(mut self, span: ByteSpan, label: impl Into<String>) -> Self {
        self.related.push((span, label.into()));
        self
    }

    /// Attach in-memory assertion provenance (M4.4). Generic over
    /// diagnostics; the S6 no-generic-setter rule constrains only the
    /// AST API.
    pub fn with_provenance(mut self, provenance: AssertProvenance) -> Self {
        self.provenance = Some(provenance);
        self
    }

    /// Human rendering: `error[AION-E0002]: msg @ file#0:3..5 | note`.
    /// Total: never panics.
    pub fn render_human(&self) -> String {
        let mut s = format!(
            "{}[{}]: {}",
            self.severity,
            self.code.code_str(),
            self.message
        );
        if let Some(sp) = self.span {
            s.push_str(&format!(" @ {}:{}..{}", sp.file, sp.start, sp.end));
        }
        for n in &self.notes {
            s.push_str(&format!(" | note: {n}"));
        }
        for (sp, label) in &self.related {
            s.push_str(&format!(
                " | related: {label} @ {}:{}..{}",
                sp.file, sp.start, sp.end
            ));
        }
        s
    }

    /// JSON rendering without external crates. Strings are escaped per
    /// JSON; output is one object, no trailing newline.
    pub fn render_json(&self) -> String {
        let mut s = String::from("{");
        s.push_str(&format!(
            "\"severity\":\"{}\",\"code\":\"{}\",\"message\":\"{}\"",
            self.severity,
            self.code.code_str(),
            esc(&self.message)
        ));
        match self.span {
            Some(sp) => s.push_str(&format!(
                ",\"span\":{{\"file\":{},\"start\":{},\"end\":{}}}",
                sp.file.0, sp.start, sp.end
            )),
            None => s.push_str(",\"span\":null"),
        }
        s.push_str(",\"notes\":[");
        for (i, n) in self.notes.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!("\"{}\"", esc(n)));
        }
        s.push(']');
        if !self.related.is_empty() {
            s.push_str(",\"related\":[");
            for (i, (sp, label)) in self.related.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                s.push_str(&format!(
                    "{{\"file\":{},\"start\":{},\"end\":{},\"label\":\"{}\"}}",
                    sp.file.0,
                    sp.start,
                    sp.end,
                    esc(label)
                ));
            }
            s.push(']');
        }
        s.push('}');
        s
    }
}

impl Diagnostic {
    /// Canonical per-diagnostic JSON (M4.1): fixed key order
    /// `severity, code, message, span?, notes?, related?`; absent span and
    /// empty notes/related are omitted (omit-when-empty). Location fields
    /// are byte-exact within a variant. UTF-8 output, no trailing newline
    /// (the envelope adds the LF terminator).
    pub fn render_canonical_json(&self) -> String {
        let mut s = format!(
            "{{\"severity\":\"{}\",\"code\":\"{}\",\"message\":\"{}\"",
            self.severity,
            self.code.code_str(),
            esc(&self.message)
        );
        if let Some(sp) = self.span {
            s.push_str(&format!(
                ",\"span\":{{\"file\":{},\"start\":{},\"end\":{}}}",
                sp.file.0, sp.start, sp.end
            ));
        }
        if !self.notes.is_empty() {
            s.push_str(",\"notes\":[");
            for (i, n) in self.notes.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                s.push_str(&format!("\"{}\"", esc(n)));
            }
            s.push(']');
        }
        if !self.related.is_empty() {
            s.push_str(",\"related\":[");
            for (i, (sp, label)) in self.related.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                s.push_str(&format!(
                    "{{\"file\":{},\"start\":{},\"end\":{},\"label\":\"{}\"}}",
                    sp.file.0,
                    sp.start,
                    sp.end,
                    esc(label)
                ));
            }
            s.push(']');
        }
        // M4.4 M5a: optional provenance object, always last.
        // `HumanAuthored` (and absent provenance) emits no key, so
        // pre-provenance canonical bytes are unchanged.
        if let Some(p) = &self.provenance {
            match p.origin() {
                AssertOrigin::HumanAuthored => {}
                AssertOrigin::AiGenerated => {
                    s.push_str(",\"provenance\":{\"origin\":\"ai-generated\"");
                    if let Some(g) = p.generator() {
                        if !g.is_empty() {
                            s.push_str(&format!(",\"identity\":\"{}\"", esc(g)));
                        }
                    }
                    s.push('}');
                }
                AssertOrigin::Transformed => {
                    s.push_str(",\"provenance\":{\"origin\":\"transformed\"");
                    if let Some(r) = p.source_ref() {
                        if !r.is_empty() {
                            s.push_str(&format!(",\"source_ref\":\"{}\"", esc(r)));
                        }
                    }
                    if let Some(t) = p.transform_id() {
                        if !t.is_empty() {
                            s.push_str(&format!(",\"transform_id\":\"{}\"", esc(t)));
                        }
                    }
                    s.push('}');
                }
            }
        }
        s.push('}');
        s
    }

    /// Canonical envelope bytes for a diagnostic list (M4.1): fixed key
    /// order `schema, version, diagnostics`, UTF-8, single LF terminator.
    pub fn canonical_envelope_bytes(diags: &[Diagnostic]) -> Vec<u8> {
        let mut s =
            String::from("{\"schema\":\"aion.diagnostic/v1\",\"version\":1,\"diagnostics\":[");
        for (i, d) in diags.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&d.render_canonical_json());
        }
        s.push_str("]}");
        s.push('\n');
        s.into_bytes()
    }
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

/// Canonical diagnostic envelope reader (M4.2, test/tooling-side).
///
/// Accepts bytes produced by [`Diagnostic::canonical_envelope_bytes`]:
/// UTF-8, single LF terminator, `schema` `"aion.diagnostic/v1"`.
/// Checks the major `version` (only `1` accepted; missing or
/// non-`1` versions are rejected), ignores unknown object fields, and
/// returns plain `String` errors (never [`ErrorCode`]: the reader is
/// not the compiler).
pub fn parse_envelope(bytes: &[u8]) -> Result<Vec<Diagnostic>, String> {
    let s = std::str::from_utf8(bytes).map_err(|e| format!("envelope is not UTF-8: {e}"))?;
    let body = s
        .strip_suffix('\n')
        .ok_or_else(|| "envelope missing LF terminator".to_string())?;
    if body.contains('\n') || body.contains('\r') {
        return Err("envelope must be a single line".to_string());
    }
    let mut p = Cur::new(body);
    p.expect(b'{')?;
    let mut schema: Option<String> = None;
    let mut version: Option<u64> = None;
    let mut diags_raw: Option<String> = None;
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
            "diagnostics" => diags_raw = Some(p.raw_value()?),
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
        Some("aion.diagnostic/v1") => {}
        Some(other) => return Err(format!("unsupported schema: {other}")),
        None => return Err("envelope missing schema".to_string()),
    }
    match version {
        Some(1) => {}
        Some(v) => return Err(format!("unsupported major version: {v}")),
        None => return Err("envelope missing version".to_string()),
    }
    let raw = diags_raw.ok_or_else(|| "envelope missing diagnostics".to_string())?;
    parse_diagnostics(&raw)
}

fn parse_diagnostics(raw: &str) -> Result<Vec<Diagnostic>, String> {
    let mut p = Cur::new(raw);
    p.expect(b'[')?;
    let mut out = Vec::new();
    p.ws();
    if p.eat(b']') {
        return Ok(out);
    }
    loop {
        p.ws();
        let obj = p.raw_value()?;
        out.push(parse_diagnostic(&obj)?);
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

fn parse_diagnostic(raw: &str) -> Result<Diagnostic, String> {
    let mut p = Cur::new(raw);
    p.expect(b'{')?;
    let mut severity: Option<String> = None;
    let mut code: Option<String> = None;
    let mut message: Option<String> = None;
    let mut span_raw: Option<String> = None;
    let mut span_null = false;
    let mut notes: Vec<String> = Vec::new();
    let mut notes_raw: Option<String> = None;
    let mut related: Vec<(ByteSpan, String)> = Vec::new();
    let mut related_raw: Option<String> = None;
    let mut provenance_raw: Option<String> = None;
    let mut provenance_null = false;
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
            "severity" => severity = Some(p.string()?),
            "code" => code = Some(p.string()?),
            "message" => message = Some(p.string()?),
            "span" => {
                if p.peek_is_null() {
                    p.literal("null")?;
                    span_null = true;
                } else {
                    span_raw = Some(p.raw_value()?);
                }
            }
            "notes" => notes_raw = Some(p.raw_value()?),
            "related" => related_raw = Some(p.raw_value()?),
            "provenance" => {
                if p.peek_is_null() {
                    p.literal("null")?;
                    provenance_null = true;
                } else {
                    provenance_raw = Some(p.raw_value()?);
                }
            }
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
    let _ = span_null;
    let sev = match severity.as_deref() {
        Some("error") => Severity::Error,
        Some("warning") => Severity::Warning,
        Some("note") => Severity::Note,
        Some(o) => return Err(format!("unknown severity: {o}")),
        None => return Err("diagnostic missing severity".to_string()),
    };
    let code = match code.as_deref() {
        Some("AION-E0001") => ErrorCode::InvalidByte,
        Some("AION-E0002") => ErrorCode::UnexpectedToken,
        Some("AION-E0003") => ErrorCode::UnexpectedEof,
        Some("AION-E0004") => ErrorCode::ParseError,
        Some("AION-E0010") => ErrorCode::TypeMismatch,
        Some("AION-E0011") => ErrorCode::UnboundName,
        Some("AION-E0012") => ErrorCode::ArityMismatch,
        Some("AION-E0013") => ErrorCode::DuplicateDefinition,
        Some("AION-E0014") => ErrorCode::UnresolvedType,
        Some("AION-E0020") => ErrorCode::UseAfterMove,
        Some("AION-E0030") => ErrorCode::InternalIrInvalid,
        Some("AION-E0015") => ErrorCode::EntryPointInvalid,
        Some("AION-E0016") => ErrorCode::AssertInIntent,
        Some("AION-E0040") => ErrorCode::DivByZero,
        Some("AION-E0041") => ErrorCode::IntOverflow,
        Some("AION-E0042") => ErrorCode::FuelExhausted,
        Some("AION-E0043") => ErrorCode::StackOverflow,
        Some("AION-E0044") => ErrorCode::RuntimeTrap,
        Some(o) => return Err(format!("unknown code: {o}")),
        None => return Err("diagnostic missing code".to_string()),
    };
    let message = message.ok_or_else(|| "diagnostic missing message".to_string())?;
    let span = match span_raw {
        Some(r) => Some(parse_span(&r)?),
        None => None,
    };
    if let Some(r) = notes_raw {
        notes = parse_strings(&r)?;
    }
    if let Some(r) = related_raw {
        related = parse_related(&r)?;
    }
    // Keyless or explicit-null provenance reads as `None`; attribution
    // is never inferred from the diagnostic code.
    let provenance = if provenance_null {
        None
    } else {
        match provenance_raw {
            Some(r) => Some(parse_provenance(&r)?),
            None => None,
        }
    };
    Ok(Diagnostic {
        code,
        severity: sev,
        message,
        span,
        notes,
        related,
        provenance,
    })
}

fn parse_span(raw: &str) -> Result<ByteSpan, String> {
    let mut p = Cur::new(raw);
    p.expect(b'{')?;
    let mut file: Option<u64> = None;
    let mut start: Option<u64> = None;
    let mut end: Option<u64> = None;
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
            "file" => file = Some(p.number()?),
            "start" => start = Some(p.number()?),
            "end" => end = Some(p.number()?),
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
    let (f, s, e) = match (file, start, end) {
        (Some(f), Some(s), Some(e)) => (f, s, e),
        _ => return Err("span missing field".to_string()),
    };
    ByteSpan::new(
        aion_span::FileId(u32::try_from(f).map_err(|_| "span file out of range".to_string())?),
        u32::try_from(s).map_err(|_| "span start out of range".to_string())?,
        u32::try_from(e).map_err(|_| "span end out of range".to_string())?,
    )
    .ok_or_else(|| "span start > end".to_string())
}

fn parse_strings(raw: &str) -> Result<Vec<String>, String> {
    let mut p = Cur::new(raw);
    p.expect(b'[')?;
    let mut out = Vec::new();
    p.ws();
    if p.eat(b']') {
        return Ok(out);
    }
    loop {
        p.ws();
        out.push(p.string()?);
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

fn parse_related(raw: &str) -> Result<Vec<(ByteSpan, String)>, String> {
    let mut p = Cur::new(raw);
    p.expect(b'[')?;
    let mut out = Vec::new();
    p.ws();
    if p.eat(b']') {
        return Ok(out);
    }
    loop {
        p.ws();
        let obj = p.raw_value()?;
        let mut q = Cur::new(&obj);
        q.expect(b'{')?;
        let mut file: Option<u64> = None;
        let mut start: Option<u64> = None;
        let mut end: Option<u64> = None;
        let mut label: Option<String> = None;
        loop {
            q.ws();
            if q.eat(b'}') {
                break;
            }
            let key = q.string()?;
            q.ws();
            q.expect(b':')?;
            q.ws();
            match key.as_str() {
                "file" => file = Some(q.number()?),
                "start" => start = Some(q.number()?),
                "end" => end = Some(q.number()?),
                "label" => label = Some(q.string()?),
                _ => {
                    q.skip_value()?;
                }
            }
            q.ws();
            if q.eat(b',') {
                continue;
            }
            q.ws();
            q.expect(b'}')?;
            break;
        }
        let (f, s, e) = match (file, start, end) {
            (Some(f), Some(s), Some(e)) => (f, s, e),
            _ => return Err("related entry missing field".to_string()),
        };
        let sp = ByteSpan::new(
            aion_span::FileId(
                u32::try_from(f).map_err(|_| "related file out of range".to_string())?,
            ),
            u32::try_from(s).map_err(|_| "related start out of range".to_string())?,
            u32::try_from(e).map_err(|_| "related end out of range".to_string())?,
        )
        .ok_or_else(|| "related span start > end".to_string())?;
        let label = label.ok_or_else(|| "related entry missing label".to_string())?;
        out.push((sp, label));
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

/// Parse a canonical `provenance` object (M4.4 M5b).
///
/// `origin` is required: `"ai-generated"` keeps only `identity`,
/// `"transformed"` keeps only `source_ref`/`transform_id`; anything
/// else fails closed. Identifier fields incompatible with the declared
/// origin are dropped, empty identifiers normalize to absent, unknown
/// keys are skipped, and attribution is never inferred — the caller
/// supplies no diagnostic code.
fn parse_provenance(raw: &str) -> Result<AssertProvenance, String> {
    fn non_empty(s: Option<String>) -> Option<String> {
        match s {
            Some(v) if v.is_empty() => None,
            other => other,
        }
    }
    let mut p = Cur::new(raw);
    p.expect(b'{')?;
    let mut origin: Option<String> = None;
    let mut identity: Option<String> = None;
    let mut source_ref: Option<String> = None;
    let mut transform_id: Option<String> = None;
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
            "origin" => origin = Some(p.string()?),
            "identity" => identity = Some(p.string()?),
            "source_ref" => source_ref = Some(p.string()?),
            "transform_id" => transform_id = Some(p.string()?),
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
    match origin.as_deref() {
        Some("ai-generated") => Ok(AssertProvenance::ai_generated(non_empty(identity))),
        Some("transformed") => Ok(AssertProvenance::transformed(
            non_empty(source_ref),
            non_empty(transform_id),
        )),
        Some(o) => Err(format!("unknown provenance origin: {o}")),
        None => Err("provenance missing origin".to_string()),
    }
}

/// Minimal JSON cursor over a `&str` (byte-indexed; input is UTF-8).
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
    fn peek_is_null(&self) -> bool {
        self.b.len() >= self.pos + 4 && &self.b[self.pos..self.pos + 4] == b"null"
    }
    fn literal(&mut self, lit: &str) -> Result<(), String> {
        if self.b.len() >= self.pos + lit.len()
            && &self.b[self.pos..self.pos + lit.len()] == lit.as_bytes()
        {
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
    /// Raw slice of one JSON value starting at the cursor.
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

#[cfg(test)]
mod m4_compat;

#[cfg(test)]
mod m4_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use aion_span::FileId;

    #[test]
    fn codes_are_stable() {
        assert_eq!(ErrorCode::InvalidByte.code_str(), "AION-E0001");
        assert_eq!(ErrorCode::UnexpectedToken.code_str(), "AION-E0002");
        assert_eq!(ErrorCode::UnexpectedEof.code_str(), "AION-E0003");
        assert_eq!(ErrorCode::ParseError.code_str(), "AION-E0004");
        assert_eq!(ErrorCode::TypeMismatch.code_str(), "AION-E0010");
        assert_eq!(ErrorCode::UnboundName.code_str(), "AION-E0011");
        assert_eq!(ErrorCode::ArityMismatch.code_str(), "AION-E0012");
        assert_eq!(ErrorCode::DuplicateDefinition.code_str(), "AION-E0013");
        assert_eq!(ErrorCode::UnresolvedType.code_str(), "AION-E0014");
        assert_eq!(ErrorCode::UseAfterMove.code_str(), "AION-E0020");
        assert_eq!(ErrorCode::InternalIrInvalid.code_str(), "AION-E0030");
        assert_eq!(ErrorCode::EntryPointInvalid.code_str(), "AION-E0015");
        assert_eq!(ErrorCode::DivByZero.code_str(), "AION-E0040");
        assert_eq!(ErrorCode::IntOverflow.code_str(), "AION-E0041");
        assert_eq!(ErrorCode::FuelExhausted.code_str(), "AION-E0042");
        assert_eq!(ErrorCode::StackOverflow.code_str(), "AION-E0043");
        assert_eq!(ErrorCode::RuntimeTrap.code_str(), "AION-E0044");
        assert_eq!(ErrorCode::AssertInIntent.code_str(), "AION-E0016");
    }

    #[test]
    fn e0016_assert_in_intent_round_trip() {
        let d = Diagnostic::new(
            ErrorCode::AssertInIntent,
            "assert statements are not allowed in intent blocks",
        )
        .with_span(ByteSpan::empty(FileId(0), 7));
        let bytes = Diagnostic::canonical_envelope_bytes(&[d]);
        let back = parse_envelope(&bytes).expect("envelope must parse");
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].code, ErrorCode::AssertInIntent);
        assert_eq!(
            back[0].message,
            "assert statements are not allowed in intent blocks"
        );
        assert_eq!(back[0].span, Some(ByteSpan::empty(FileId(0), 7)));
        assert_eq!(back[0].provenance, None);
    }

    #[test]
    fn human_and_json_render() {
        let d = Diagnostic::new(ErrorCode::UnexpectedToken, "bad token")
            .with_span(ByteSpan::empty(FileId(0), 3))
            .with_note("try again");
        let h = d.render_human();
        assert!(h.contains("AION-E0002") && h.contains("bad token"));
        let j = d.render_json();
        assert!(j.contains("\"code\":\"AION-E0002\""));
        assert!(j.contains("\"notes\":[\"try again\"]"));
        // No-span diagnostic renders null span.
        let j2 = Diagnostic::new(ErrorCode::ParseError, "x\"\\\n").render_json();
        assert!(j2.contains("\"span\":null") && j2.contains("\\\""));
    }

    #[test]
    fn default_provenance_is_human_authored_absent() {
        let p = AssertProvenance::default();
        assert_eq!(p.origin(), AssertOrigin::HumanAuthored);
        assert_eq!(p.generator(), None);
        assert_eq!(p.source_ref(), None);
        assert_eq!(p.transform_id(), None);
        assert_eq!(
            AssertProvenance::human_authored(),
            AssertProvenance::default()
        );
        let d = Diagnostic::new(ErrorCode::ParseError, "x");
        assert_eq!(d.provenance, None);
    }

    #[test]
    fn provenance_transitions_follow_approved_rules() {
        // HumanAuthored -> Transformed.
        let t = AssertProvenance::human_authored().apply_transform::<String, String>(None, None);
        assert_eq!(t.origin(), AssertOrigin::Transformed);
        assert_eq!(t.source_ref(), None);
        assert_eq!(t.transform_id(), None);
        let t = AssertProvenance::human_authored()
            .apply_transform(Some("s1".to_string()), Some("t1".to_string()));
        assert_eq!(t.source_ref(), Some("s1"));
        assert_eq!(t.transform_id(), Some("t1"));
        // Empty identifiers normalize to absent.
        let t = AssertProvenance::human_authored()
            .apply_transform(Some(String::new()), Some(String::new()));
        assert_eq!(t.source_ref(), None);
        assert_eq!(t.transform_id(), None);

        // AiGenerated -> Transformed discards generator identity.
        let ai = AssertProvenance::ai_generated(Some("agent-7"));
        assert_eq!(ai.generator(), Some("agent-7"));
        let t = ai.apply_transform(Some("s".to_string()), None::<String>);
        assert_eq!(t.origin(), AssertOrigin::Transformed);
        assert_eq!(t.generator(), None);
        assert_eq!(t.source_ref(), Some("s"));
        assert_eq!(t.transform_id(), None);
        // Empty generator normalizes to absent at construction.
        assert_eq!(
            AssertProvenance::ai_generated(Some(String::new())).generator(),
            None
        );

        // Transformed -> Transformed: replacement, retention, late supply.
        let base = AssertProvenance::transformed(Some("orig".to_string()), Some("t1".to_string()));
        let t = base.apply_transform(Some("new".to_string()), Some("t2".to_string()));
        assert_eq!(t.source_ref(), Some("orig"));
        assert_eq!(t.transform_id(), Some("t2"));
        let t = base.apply_transform::<String, String>(None, None);
        assert_eq!(t.source_ref(), Some("orig"));
        assert_eq!(t.transform_id(), Some("t1"));
        // Late source supply with no new transform id.
        let srcless = AssertProvenance::transformed(None::<String>, Some("t9".to_string()));
        let t = srcless.apply_transform(Some("late".to_string()), None::<String>);
        assert_eq!(t.source_ref(), Some("late"));
        assert_eq!(t.transform_id(), Some("t9"));
        // Never downgrades.
        assert_eq!(t.origin(), AssertOrigin::Transformed);
    }
}
