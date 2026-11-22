//! `aion-span`: byte-based source spans, lossless on arbitrary bytes.
//!
//! Contract (M0):
//! - All offsets are **byte** offsets into the file's raw `Vec<u8>`.
//! - Invalid UTF-8 is preserved losslessly; no function panics on
//!   arbitrary bytes. Fallible lookups return `Option`.
//! - Columns are byte-based (byte offset within the line), so they are
//!   well-defined even when the line is not valid UTF-8.

use std::fmt;

/// Opaque identifier for a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId(pub u32);

impl fmt::Display for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "file#{}", self.0)
    }
}

/// Half-open byte range `[start, end)` in `file`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ByteSpan {
    pub file: FileId,
    pub start: u32,
    pub end: u32,
}

impl ByteSpan {
    /// Total constructor: returns `None` when `start > end`.
    pub fn new(file: FileId, start: u32, end: u32) -> Option<Self> {
        if start <= end {
            Some(Self { file, start, end })
        } else {
            None
        }
    }

    /// Empty span at `offset`.
    pub fn empty(file: FileId, offset: u32) -> Self {
        Self {
            file,
            start: offset,
            end: offset,
        }
    }

    pub fn len(&self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Total cover of two spans in the same file; `None` for mixed files.
    pub fn cover(self, other: ByteSpan) -> Option<ByteSpan> {
        if self.file != other.file {
            return None;
        }
        Some(ByteSpan {
            file: self.file,
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        })
    }
}

/// A source file: raw bytes kept losslessly, never decoded eagerly.
#[derive(Debug, Clone)]
pub struct SourceFile {
    id: FileId,
    name: String,
    bytes: Vec<u8>,
    lines: LineTable,
}

impl SourceFile {
    pub fn new(id: FileId, name: impl Into<String>, bytes: Vec<u8>) -> Self {
        let lines = LineTable::build(&bytes);
        Self {
            id,
            name: name.into(),
            bytes,
            lines,
        }
    }

    pub fn id(&self) -> FileId {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Raw bytes (lossless, may be invalid UTF-8).
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn line_table(&self) -> &LineTable {
        &self.lines
    }

    /// Total slice of a span: `None` when the span is for another file or
    /// out of bounds. Never panics, never decodes UTF-8.
    pub fn slice(&self, span: ByteSpan) -> Option<&[u8]> {
        if span.file != self.id {
            return None;
        }
        let (s, e) = (span.start as usize, span.end as usize);
        if s <= e && e <= self.bytes.len() {
            Some(&self.bytes[s..e])
        } else {
            None
        }
    }

    /// Lossy display snippet for a span (for diagnostics). Invalid UTF-8
    /// becomes U+FFFD; always returns a string, never panics.
    pub fn snippet_lossy(&self, span: ByteSpan) -> String {
        match self.slice(span) {
            Some(b) => String::from_utf8_lossy(b).into_owned(),
            None => String::new(),
        }
    }

    /// Total line/column lookup (1-based line, 0-based byte column).
    pub fn line_col(&self, offset: u32) -> Option<(usize, usize)> {
        self.lines.offset_to_line_col(offset as usize)
    }
}

/// Byte-based line index: start offset of every line. Built by scanning for
/// the byte `b'\n'` only, so invalid UTF-8 cannot break it.
#[derive(Debug, Clone, Default)]
pub struct LineTable {
    /// Byte offset at which each line (0-based) starts. Always nonempty:
    /// `[0]` for an empty file.
    line_starts: Vec<usize>,
    len: usize,
}

impl LineTable {
    pub fn build(bytes: &[u8]) -> Self {
        let mut line_starts = vec![0usize];
        for (i, b) in bytes.iter().enumerate() {
            if *b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self {
            line_starts,
            len: bytes.len(),
        }
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Total: `None` when `offset > len`. Otherwise 1-based line and
    /// 0-based byte column within the line.
    pub fn offset_to_line_col(&self, offset: usize) -> Option<(usize, usize)> {
        if offset > self.len {
            return None;
        }
        // Last line_start <= offset.
        let mut line = 0usize;
        for (i, s) in self.line_starts.iter().enumerate() {
            if *s <= offset {
                line = i;
            } else {
                break;
            }
        }
        let col = offset - self.line_starts[line];
        Some((line + 1, col))
    }

    /// Total byte range of 1-based `line`; `None` when out of range.
    /// End excludes the trailing `\n` when present.
    pub fn line_range(&self, line: usize) -> Option<(usize, usize)> {
        if line == 0 || line > self.line_starts.len() {
            return None;
        }
        let start = self.line_starts[line - 1];
        let raw_end = if line < self.line_starts.len() {
            self.line_starts[line]
        } else {
            self.len
        };
        let mut end = raw_end;
        if end > start && raw_end > start && self.len > start {
            // If the line is newline-terminated, exclude the `\n`.
            if raw_end > 0 && raw_end <= self.len && end > start {
                // line_starts[line] == position after `\n`
                end = raw_end - 1;
            }
        }
        Some((start, end.min(self.len)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_offsets_with_invalid_utf8() {
        let bytes = vec![0x66, 0x6f, 0xFF, 0xFE, 0x0A, 0x62, 0xFF];
        let f = SourceFile::new(FileId(1), "x", bytes.clone());
        assert_eq!(f.len(), 7);
        // Span over invalid bytes is fine.
        let sp = ByteSpan::new(FileId(1), 2, 4).unwrap();
        assert_eq!(f.slice(sp), Some(&bytes[2..4]));
        // Line table splits on 0x0A regardless of decoding.
        assert_eq!(f.line_col(0), Some((1, 0)));
        assert_eq!(f.line_col(5), Some((2, 0)));
        assert_eq!(f.line_col(99), None);
        // Lossy snippet never panics.
        let _ = f.snippet_lossy(sp);
    }

    #[test]
    fn total_constructors_reject_bad_input() {
        assert!(ByteSpan::new(FileId(0), 5, 3).is_none());
        let f = SourceFile::new(FileId(0), "e", vec![]);
        assert_eq!(f.slice(ByteSpan::empty(FileId(9), 0)), None);
        assert_eq!(f.slice(ByteSpan::empty(FileId(0), 1)), None);
        let t = LineTable::build(b"a\nb");
        assert_eq!(t.line_range(0), None);
        assert_eq!(t.line_range(9), None);
        assert_eq!(t.line_range(1), Some((0, 1)));
    }

    #[test]
    fn empty_file_has_one_line() {
        let t = LineTable::build(b"");
        assert_eq!(t.line_count(), 1);
        assert_eq!(t.offset_to_line_col(0), Some((1, 0)));
        assert_eq!(t.offset_to_line_col(1), None);
    }
}
