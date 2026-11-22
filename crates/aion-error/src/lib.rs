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
    /// Use after move.
    UseAfterMove = 20,
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
            ErrorCode::UseAfterMove => "AION-E0020",
        }
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
        assert_eq!(ErrorCode::UseAfterMove.code_str(), "AION-E0020");
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
}
