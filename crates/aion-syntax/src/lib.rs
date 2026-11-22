//! `aion-syntax`: M1 parser + formatter (M0 robustness retained).
//!
//! Stable M1 syntax (default features):
//! - line (`//`) and block (`/* */`) comments (trivia, skipped)
//! - delimiters: `( ) { } [ ] ; , =`
//! - identifiers, integer / string / bool / null literals
//! - expressions with precedence: `||`, `&&`, `== !=`, `< <= > >=`,
//!   `+ -`, `* / %`, unary `- !`, postfix calls `f(args)`,
//!   primaries (literals, idents, parenthesized)
//! - statements: `let <ident> = <expr> ;`, `do <expr> ;`, bare `;`
//! - items: `fn <ident> ( params? ) <block>`
//!
//! Experimental (gated OFF by default):
//! - `intent "desc" { ... }` item/block, `goal = "...";` field, `assert <expr>;`
//!   With default flags these produce `AION-E0002` diagnostics and are
//!   skipped; enable via [`FeatureFlags::all_experimental`].
//!
//! Guarantees: `lex` and `parse` are total (never panic, even on
//! arbitrary bytes); invalid bytes yield `TokenKind::Error` +
//! `AION-E0001` diagnostics.

use aion_error::{Diagnostic, ErrorCode};
use aion_span::{ByteSpan, FileId};
use std::collections::HashMap;
use std::fmt;

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

/// Minimal token kinds. M0 variants retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Word,
    Number,
    Whitespace,
    Punct,
    StrLit,
    LineComment,
    BlockComment,
    /// A single byte that is not valid text. Lossless span over raw byte.
    Error,
}

/// One lexed token with its byte span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: ByteSpan,
}

/// Total lexer over raw bytes.
pub fn lex(file: FileId, bytes: &[u8]) -> Vec<Token> {
    let mut toks = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let start = i as u32;
        let b = bytes[i];
        if b.is_ascii_alphabetic() || b == b'_' {
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            toks.push(Token {
                kind: TokenKind::Word,
                span: ByteSpan {
                    file,
                    start,
                    end: i as u32,
                },
            });
        } else if b.is_ascii_digit() {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            toks.push(Token {
                kind: TokenKind::Number,
                span: ByteSpan {
                    file,
                    start,
                    end: i as u32,
                },
            });
        } else if b == b'"' {
            // String literal: consume to closing quote with `\` escapes; if
            // unterminated, consume to EOF as one StrLit token (parse flags EOF).
            i += 1;
            let mut closed = false;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i += 1;
                    if i < bytes.len() {
                        i += 1;
                    }
                } else if bytes[i] == b'"' {
                    i += 1;
                    closed = true;
                    break;
                } else if bytes[i] == b'\n' {
                    break; // newline terminates unterminated literal
                } else {
                    i += 1;
                }
            }
            let _ = closed;
            toks.push(Token {
                kind: TokenKind::StrLit,
                span: ByteSpan {
                    file,
                    start,
                    end: i as u32,
                },
            });
        } else if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            toks.push(Token {
                kind: TokenKind::LineComment,
                span: ByteSpan {
                    file,
                    start,
                    end: i as u32,
                },
            });
        } else if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            // Block comment, possibly unterminated (runs to EOF).
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 < bytes.len() {
                i += 2;
            } else {
                i = bytes.len();
            }
            toks.push(Token {
                kind: TokenKind::BlockComment,
                span: ByteSpan {
                    file,
                    start,
                    end: i as u32,
                },
            });
        } else if b.is_ascii_whitespace() {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            toks.push(Token {
                kind: TokenKind::Whitespace,
                span: ByteSpan {
                    file,
                    start,
                    end: i as u32,
                },
            });
        } else if b < 0x80 {
            // Two-character operators: `|| && == != <= >=`.
            if i + 1 < bytes.len() {
                let c = bytes[i + 1];
                let two = matches!(
                    (b, c),
                    (b'|', b'|')
                        | (b'&', b'&')
                        | (b'=', b'=')
                        | (b'!', b'=')
                        | (b'<', b'=')
                        | (b'>', b'=')
                );
                if two {
                    i += 2;
                    toks.push(Token {
                        kind: TokenKind::Punct,
                        span: ByteSpan {
                            file,
                            start,
                            end: i as u32,
                        },
                    });
                    continue;
                }
            }
            i += 1;
            toks.push(Token {
                kind: TokenKind::Punct,
                span: ByteSpan {
                    file,
                    start,
                    end: i as u32,
                },
            });
        } else {
            let rest = &bytes[i..];
            match utf8_len(rest) {
                Some(len) => {
                    i += len;
                    toks.push(Token {
                        kind: TokenKind::Word,
                        span: ByteSpan {
                            file,
                            start,
                            end: i as u32,
                        },
                    });
                }
                None => {
                    i += 1;
                    toks.push(Token {
                        kind: TokenKind::Error,
                        span: ByteSpan {
                            file,
                            start,
                            end: i as u32,
                        },
                    });
                }
            }
        }
    }
    toks
}

/// Length of the UTF-8 sequence starting at `b`, or `None` if invalid.
fn utf8_len(b: &[u8]) -> Option<usize> {
    let first = *b.first()?;
    if first < 0x80 {
        return Some(1);
    }
    let want = if first >> 5 == 0b110 {
        2
    } else if first >> 4 == 0b1110 {
        3
    } else if first >> 3 == 0b11110 {
        4
    } else {
        return None;
    };
    if b.len() < want {
        return None;
    }
    for &byte in &b[1..want] {
        if byte >> 6 != 0b10 {
            return None;
        }
    }
    std::str::from_utf8(&b[..want]).ok()?;
    Some(want)
}

// ---------------------------------------------------------------------------
// AST (spans kept for diagnostics; equality is structural via `shape()`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Program {
    pub items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub enum Item {
    Fn(FnDecl),
    /// Experimental `intent` item (only with feature flag).
    Intent(IntentBlock),
}

#[derive(Debug, Clone)]
pub struct FnDecl {
    pub name: String,
    pub params: Vec<String>,
    pub body: Block,
}

#[derive(Debug, Clone)]
pub struct IntentBlock {
    pub desc: String,
    pub body: Block,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let {
        name: String,
        expr: Expr,
    },
    Do(Expr),
    /// Experimental `goal = "...";`
    Goal(String),
    /// Experimental `assert <expr>;`
    Assert(Expr),
    Empty,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Int(i64),
    Str(String),
    Bool(bool),
    Null,
    Ident(String),
    Unary {
        op: UnOp,
        rhs: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Call {
        callee: String,
        args: Vec<Expr>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

impl BinOp {
    fn as_str(self) -> &'static str {
        match self {
            BinOp::Or => "||",
            BinOp::And => "&&",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Mod => "%",
        }
    }
}

/// Feature flags for experimental syntax. All off by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FeatureFlags {
    pub intent: bool,
    pub goal: bool,
    pub assert_stmt: bool,
}

impl FeatureFlags {
    pub fn all_experimental() -> Self {
        Self {
            intent: true,
            goal: true,
            assert_stmt: true,
        }
    }
}

/// M0 stub parse tree placeholder (kept for API compat).
#[derive(Debug, Clone, Default)]
pub struct GreenNode {
    pub token_count: usize,
}

/// Parser-assigned node identifier (M2). Allocated in AST construction
/// order; AST structs themselves stay span-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub u32);

/// Map from [`NodeId`] to its source byte span.
#[derive(Debug, Clone, Default)]
pub struct SpanTable {
    map: HashMap<NodeId, ByteSpan>,
}

impl SpanTable {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }
    pub fn insert(&mut self, id: NodeId, span: ByteSpan) {
        self.map.insert(id, span);
    }
    pub fn get(&self, id: NodeId) -> Option<ByteSpan> {
        self.map.get(&id).copied()
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Parallel span index mirroring the AST shape.
///
/// Every AST node has a corresponding index node holding the [`NodeId`]s
/// (keys into [`ParseResult::spans`]) for the node itself and for the
/// sub-spans sema needs (names, operators, argument lists). The AST stays
/// span-free; sema walks `program` and `index` in lockstep and must never
/// search source text. `Debug` is structural (ids only) so goldens that
/// print it stay stable.
#[derive(Debug, Clone)]
pub struct ProgramIds {
    pub node: NodeId,
    pub items: Vec<ItemIds>,
}

#[derive(Debug, Clone)]
pub enum ItemIds {
    Fn(FnIds),
    Intent(IntentIds),
}

#[derive(Debug, Clone)]
pub struct FnIds {
    pub node: NodeId,
    pub name: NodeId,
    pub params: Vec<NodeId>,
    pub body: BlockIds,
}

#[derive(Debug, Clone)]
pub struct IntentIds {
    pub node: NodeId,
    pub body: BlockIds,
}

#[derive(Debug, Clone)]
pub struct BlockIds {
    pub node: NodeId,
    pub stmts: Vec<StmtIds>,
}

#[derive(Debug, Clone)]
pub enum StmtIds {
    Let {
        node: NodeId,
        name: NodeId,
        expr: ExprIds,
    },
    Do {
        node: NodeId,
        expr: ExprIds,
    },
    Goal(NodeId),
    Assert {
        node: NodeId,
        expr: ExprIds,
    },
    Empty(NodeId),
}

#[derive(Debug, Clone)]
pub struct ExprIds {
    pub node: NodeId,
    pub kind: ExprIdsKind,
}

#[derive(Debug, Clone)]
pub enum ExprIdsKind {
    Leaf,
    Unary {
        op: NodeId,
        rhs: Box<ExprIds>,
    },
    Binary {
        op: NodeId,
        lhs: Box<ExprIds>,
        rhs: Box<ExprIds>,
    },
    Call {
        callee: NodeId,
        arg_list: NodeId,
        args: Vec<ExprIds>,
    },
}

/// Sub-index data for a statement (cover [`NodeId`] allocated by caller).
#[derive(Debug, Clone)]
enum StmtSub {
    Let { name_id: NodeId, expr_ids: ExprIds },
    Do(ExprIds),
    Assert(ExprIds),
    Bare,
}

/// Parse result: AST + collected diagnostics (never fatal, never panics).
#[derive(Debug, Clone)]
pub struct ParseResult {
    pub root: GreenNode,
    pub program: Program,
    pub errors: Vec<Diagnostic>,
    pub spans: SpanTable,
    pub index: ProgramIds,
}

impl ParseResult {
    /// Structural shape string (no spans) for round-trip equivalence.
    pub fn shape(&self) -> String {
        format!("{:?}", self.program)
    }
    /// Canonical formatter output.
    pub fn formatted(&self) -> String {
        fmt_program(&self.program)
    }
}

// (AST nodes carry no spans, so `{:?}` of `Program` is the shape.)

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Sig {
    text: String,
    span: ByteSpan,
}

struct Parser {
    file: FileId,
    toks: Vec<(TokenKind, String, ByteSpan)>,
    pos: usize,
    errors: Vec<Diagnostic>,
    flags: FeatureFlags,
    next_id: u32,
    spans: SpanTable,
}

impl Parser {
    fn new(file: FileId, bytes: &[u8], flags: FeatureFlags) -> Self {
        let raw = lex(file, bytes);
        let mut toks = Vec::new();
        let mut errors = Vec::new();
        for t in raw {
            match t.kind {
                TokenKind::Whitespace | TokenKind::LineComment | TokenKind::BlockComment => {}
                TokenKind::Error => {
                    errors.push(
                        Diagnostic::new(ErrorCode::InvalidByte, "invalid byte in input")
                            .with_span(t.span),
                    );
                }
                _ => {
                    let s =
                        String::from_utf8_lossy(&bytes[t.span.start as usize..t.span.end as usize])
                            .into_owned();
                    toks.push((t.kind, s, t.span));
                }
            }
        }
        Self {
            file,
            toks,
            pos: 0,
            errors,
            flags,
            next_id: 0,
            spans: SpanTable::new(),
        }
    }

    /// Allocate the next [`NodeId`] for a node covering `span`.
    fn alloc(&mut self, span: ByteSpan) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        self.spans.insert(id, span);
        id
    }

    /// Covering span of tokens consumed in `[start, self.pos)`.
    fn cover(&self, start: usize) -> ByteSpan {
        let lo = self.toks.get(start).map(|t| t.2.start).unwrap_or(0);
        let hi = self.toks[..self.pos.min(self.toks.len())]
            .iter()
            .next_back()
            .map(|t| t.2.end)
            .unwrap_or(lo);
        // Fall back to the token at `start` when nothing was consumed.
        let (lo, hi) = if self.pos <= start {
            match self.toks.get(start) {
                Some(t) => (t.2.start, t.2.end),
                None => (0, 0),
            }
        } else {
            (lo, hi.max(lo))
        };
        ByteSpan {
            file: self.file,
            start: lo,
            end: hi,
        }
    }

    /// Allocate a [`NodeId`] covering tokens consumed since `start`.
    fn alloc_cover(&mut self, start: usize) -> NodeId {
        let sp = self.cover(start);
        self.alloc(sp)
    }

    /// Allocate a [`NodeId`] for a single token span (names, operators,
    /// delimiters). When `opt` is `None` (e.g. missing closer on EOF),
    /// records an empty span at end of input.
    fn alloc_token(&mut self, opt: Option<ByteSpan>) -> NodeId {
        let sp = opt.unwrap_or(ByteSpan {
            file: self.file,
            start: self.toks.last().map(|t| t.2.end).unwrap_or(0),
            end: self.toks.last().map(|t| t.2.end).unwrap_or(0),
        });
        self.alloc(sp)
    }

    /// Span of the token at `idx` without consuming it.
    fn peek_span(&self) -> Option<ByteSpan> {
        self.toks.get(self.pos).map(|t| t.2)
    }

    /// Index node for a synthesized recovery expression (no source span).
    fn dummy_ids(&mut self) -> ExprIds {
        let sp = ByteSpan {
            file: self.file,
            start: self.toks.last().map(|t| t.2.end).unwrap_or(0),
            end: self.toks.last().map(|t| t.2.end).unwrap_or(0),
        };
        ExprIds {
            node: self.alloc(sp),
            kind: ExprIdsKind::Leaf,
        }
    }

    fn peek(&self) -> Option<&(TokenKind, String, ByteSpan)> {
        self.toks.get(self.pos)
    }

    fn at_word(&self, w: &str) -> bool {
        matches!(self.peek(), Some((TokenKind::Word, s, _)) if s == w)
    }

    fn at_punct(&self, p: &str) -> bool {
        matches!(self.peek(), Some((TokenKind::Punct, s, _)) if s == p)
    }

    fn bump(&mut self) -> Option<Sig> {
        if self.pos < self.toks.len() {
            let (_, s, sp) = self.toks[self.pos].clone();
            self.pos += 1;
            Some(Sig { text: s, span: sp })
        } else {
            None
        }
    }

    fn expect_word(&mut self, ctx: &str) -> Option<Sig> {
        match self.peek() {
            Some((TokenKind::Word, _, _)) => self.bump(),
            Some((_, _, sp)) => {
                let sp = *sp;
                let got = self.toks[self.pos].1.clone();
                self.errors.push(
                    Diagnostic::new(
                        ErrorCode::UnexpectedToken,
                        format!("expected {ctx}, found `{got}`"),
                    )
                    .with_span(sp),
                );
                self.bump()
            }
            None => {
                self.errors.push(Diagnostic::new(
                    ErrorCode::UnexpectedEof,
                    format!("expected {ctx}, found end of input"),
                ));
                None
            }
        }
    }

    fn expect_punct(&mut self, p: &str) -> bool {
        match self.peek() {
            Some((TokenKind::Punct, s, _)) if s == p => {
                self.bump();
                true
            }
            Some((_, _, sp)) => {
                let sp = *sp;
                let got = self.toks[self.pos].1.clone();
                self.errors.push(
                    Diagnostic::new(
                        ErrorCode::UnexpectedToken,
                        format!("expected `{p}`, found `{got}`"),
                    )
                    .with_span(sp),
                );
                // Recover: skip the offending token.
                self.bump();
                false
            }
            None => {
                self.errors.push(Diagnostic::new(
                    ErrorCode::UnexpectedEof,
                    format!("expected `{p}`, found end of input"),
                ));
                false
            }
        }
    }

    fn parse_program(&mut self) -> (Program, ProgramIds) {
        let start = self.pos;
        let mut items = Vec::new();
        let mut ids = Vec::new();
        while self.pos < self.toks.len() {
            // Skip stray semicolons / closers with recovery.
            if self.at_punct(";") {
                self.bump();
                continue;
            }
            if self.at_punct("}") {
                let sp = self.toks[self.pos].2;
                self.errors.push(
                    Diagnostic::new(ErrorCode::UnexpectedToken, "unexpected `}`".to_string())
                        .with_span(sp),
                );
                self.bump();
                continue;
            }
            if self.at_word("fn") {
                self.bump();
                let (f, fid) = self.parse_fn_decl();
                items.push(Item::Fn(f));
                ids.push(ItemIds::Fn(fid));
            } else if self.at_word("intent") {
                let kw = self.bump().unwrap();
                if self.flags.intent {
                    let (b, bid) = self.parse_intent_after_kw();
                    items.push(Item::Intent(b));
                    ids.push(ItemIds::Intent(bid));
                } else {
                    self.errors.push(
                        Diagnostic::new(
                            ErrorCode::UnexpectedToken,
                            "experimental `intent` block is gated (enable `intent` feature)"
                                .to_string(),
                        )
                        .with_span(kw.span)
                        .with_note("pass FeatureFlags { intent: true, .. } to enable"),
                    );
                    // Skip the intent block to recover.
                    self.skip_balanced_block();
                }
            } else {
                // Unexpected top-level token.
                let is_eof = self.pos >= self.toks.len();
                if is_eof {
                    break;
                }
                let sp = self.toks[self.pos].2;
                let got = self.toks[self.pos].1.clone();
                // Special-case: `let`/`do` at top level reads better.
                let msg = format!("expected item (`fn`), found `{got}`");
                self.errors
                    .push(Diagnostic::new(ErrorCode::UnexpectedToken, msg).with_span(sp));
                self.bump();
            }
        }
        let node = self.alloc_cover(start);
        (Program { items }, ProgramIds { node, items: ids })
    }

    fn parse_fn_decl(&mut self) -> (FnDecl, FnIds) {
        let start = self.pos.saturating_sub(1);
        let name_sig = self.expect_word("function name");
        let name_span = name_sig.as_ref().map(|s| s.span);
        let name = name_sig.map(|s| s.text).unwrap_or_default();
        let name_id = self.alloc_token(name_span);
        // Distinguish EOF (`(` missing at end) from wrong token.
        match self.peek() {
            Some((TokenKind::Punct, s, _)) if s == "(" => {
                self.bump();
            }
            Some((_, _, sp)) => {
                let sp = *sp;
                let got = self.toks[self.pos].1.clone();
                self.errors.push(
                    Diagnostic::new(
                        ErrorCode::UnexpectedToken,
                        format!("expected `(`, found `{got}`"),
                    )
                    .with_span(sp),
                );
            }
            None => {
                self.errors.push(Diagnostic::new(
                    ErrorCode::UnexpectedEof,
                    "expected `(`, found end of input",
                ));
            }
        }
        let mut params = Vec::new();
        let mut param_ids = Vec::new();
        loop {
            match self.peek() {
                Some((TokenKind::Punct, s, _)) if s == ")" => {
                    self.bump();
                    break;
                }
                Some((TokenKind::Word, _, _)) => {
                    let w = self.bump().unwrap();
                    let pid = self.alloc(w.span);
                    // Reject keywords as param names.
                    if matches!(
                        w.text.as_str(),
                        "fn" | "let" | "do" | "true" | "false" | "null"
                    ) {
                        self.errors.push(
                            Diagnostic::new(
                                ErrorCode::UnexpectedToken,
                                format!("expected parameter name, found keyword `{}`", w.text),
                            )
                            .with_span(w.span),
                        );
                    } else {
                        params.push(w.text);
                        param_ids.push(pid);
                    }
                    if self.at_punct(",") {
                        self.bump();
                    } else if self.at_punct(")") {
                        continue;
                    } else if self.pos >= self.toks.len() {
                        self.errors.push(Diagnostic::new(
                            ErrorCode::UnexpectedEof,
                            "expected `)`, found end of input",
                        ));
                        break;
                    } else {
                        let sp = self.toks[self.pos].2;
                        let got = self.toks[self.pos].1.clone();
                        self.errors.push(
                            Diagnostic::new(
                                ErrorCode::UnexpectedToken,
                                format!("expected `,` or `)`, found `{got}`"),
                            )
                            .with_span(sp),
                        );
                        self.bump();
                    }
                }
                Some((_, _, _)) => {
                    let sp = self.toks[self.pos].2;
                    let got = self.toks[self.pos].1.clone();
                    self.errors.push(
                        Diagnostic::new(
                            ErrorCode::UnexpectedToken,
                            format!("expected parameter name or `)`, found `{got}`"),
                        )
                        .with_span(sp),
                    );
                    self.bump();
                }
                None => {
                    self.errors.push(Diagnostic::new(
                        ErrorCode::UnexpectedEof,
                        "expected `)`, found end of input",
                    ));
                    break;
                }
            }
        }
        let (body, body_ids) = self.parse_block();
        let node = self.alloc_cover(start);
        (
            FnDecl { name, params, body },
            FnIds {
                node,
                name: name_id,
                params: param_ids,
                body: body_ids,
            },
        )
    }

    fn parse_intent_after_kw(&mut self) -> (IntentBlock, IntentIds) {
        // Optional string description.
        let start = self.pos.saturating_sub(1);
        let mut desc = String::new();
        if matches!(self.peek(), Some((TokenKind::StrLit, _, _))) {
            let s = self.bump().unwrap();
            desc = unquote(&s.text);
        }
        let (body, body_ids) = self.parse_block();
        let node = self.alloc_cover(start);
        (
            IntentBlock { desc, body },
            IntentIds {
                node,
                body: body_ids,
            },
        )
    }

    fn parse_block(&mut self) -> (Block, BlockIds) {
        let start = self.pos;
        if !self.expect_punct("{") {
            let node = self.alloc_cover(start);
            return (
                Block { stmts: vec![] },
                BlockIds {
                    node,
                    stmts: vec![],
                },
            );
        }
        let mut stmts = Vec::new();
        let mut stmt_ids = Vec::new();
        loop {
            match self.peek() {
                None => {
                    self.errors.push(Diagnostic::new(
                        ErrorCode::UnexpectedEof,
                        "expected `}`, found end of input",
                    ));
                    break;
                }
                Some((TokenKind::Punct, s, _)) if s == "}" => {
                    self.bump();
                    break;
                }
                _ => {
                    if let Some((st, sid)) = self.parse_stmt() {
                        stmts.push(st);
                        stmt_ids.push(sid);
                    }
                }
            }
        }
        let node = self.alloc_cover(start);
        (
            Block { stmts },
            BlockIds {
                node,
                stmts: stmt_ids,
            },
        )
    }

    /// Statement parse: returns the AST plus its index node.
    fn parse_stmt(&mut self) -> Option<(Stmt, StmtIds)> {
        let start = self.pos;
        let (st, sub) = self.parse_stmt_inner()?;
        let node = self.alloc_cover(start);
        Some(match (st, sub) {
            (Stmt::Let { name, expr }, StmtSub::Let { name_id, expr_ids }) => (
                Stmt::Let { name, expr },
                StmtIds::Let {
                    node,
                    name: name_id,
                    expr: expr_ids,
                },
            ),
            (Stmt::Do(e), StmtSub::Do(expr_ids)) => (
                Stmt::Do(e),
                StmtIds::Do {
                    node,
                    expr: expr_ids,
                },
            ),
            (Stmt::Goal(g), StmtSub::Bare) => (Stmt::Goal(g), StmtIds::Goal(node)),
            (Stmt::Assert(e), StmtSub::Assert(expr_ids)) => (
                Stmt::Assert(e),
                StmtIds::Assert {
                    node,
                    expr: expr_ids,
                },
            ),
            (Stmt::Empty, StmtSub::Bare) => (Stmt::Empty, StmtIds::Empty(node)),
            _ => unreachable!("stmt/index kind mismatch"),
        })
    }

    fn parse_stmt_inner(&mut self) -> Option<(Stmt, StmtSub)> {
        if self.at_punct(";") {
            self.bump();
            return Some((Stmt::Empty, StmtSub::Bare));
        }
        if self.at_word("let") {
            self.bump();
            let name_sig = self.expect_word("variable name");
            let name_span = name_sig.as_ref().map(|s| s.span);
            let name = name_sig.map(|s| s.text).unwrap_or_default();
            let name_id = self.alloc_token(name_span);
            if !self.expect_punct("=") {
                // Recover to next `;` or `}`.
                self.recover_to_semi_or_brace();
                // Still produce the let with a dummy expr so shape is stable.
                let expr_ids = self.dummy_ids();
                return Some((
                    Stmt::Let {
                        name,
                        expr: Expr::Int(0),
                    },
                    StmtSub::Let { name_id, expr_ids },
                ));
            }
            let (e, e_ids) = self.parse_expr();
            if !self.expect_punct(";") {
                self.recover_to_semi_or_brace();
                if self.at_punct(";") {
                    self.bump();
                }
            }
            return Some((
                Stmt::Let { name, expr: e },
                StmtSub::Let {
                    name_id,
                    expr_ids: e_ids,
                },
            ));
        }
        if self.at_word("do") {
            self.bump();
            let (e, e_ids) = self.parse_expr();
            if !self.expect_punct(";") {
                self.recover_to_semi_or_brace();
                if self.at_punct(";") {
                    self.bump();
                }
            }
            return Some((Stmt::Do(e), StmtSub::Do(e_ids)));
        }
        if self.at_word("goal") {
            let kw = self.bump().unwrap();
            if !self.flags.goal {
                self.errors.push(
                    Diagnostic::new(
                        ErrorCode::UnexpectedToken,
                        "experimental `goal` field is gated (enable `goal` feature)".to_string(),
                    )
                    .with_span(kw.span),
                );
                self.recover_to_semi_or_brace();
                if self.at_punct(";") {
                    self.bump();
                }
                return Some((Stmt::Empty, StmtSub::Bare));
            }
            if !self.expect_punct("=") {
                self.recover_to_semi_or_brace();
                return Some((Stmt::Goal(String::new()), StmtSub::Bare));
            }
            let val = match self.peek() {
                Some((TokenKind::StrLit, _, _)) => {
                    let s = self.bump().unwrap();
                    unquote(&s.text)
                }
                Some((_, _, sp)) => {
                    let sp = *sp;
                    let got = self.toks[self.pos].1.clone();
                    self.errors.push(
                        Diagnostic::new(
                            ErrorCode::UnexpectedToken,
                            format!("expected string literal, found `{got}`"),
                        )
                        .with_span(sp),
                    );
                    self.bump();
                    String::new()
                }
                None => {
                    self.errors.push(Diagnostic::new(
                        ErrorCode::UnexpectedEof,
                        "expected string literal, found end of input",
                    ));
                    String::new()
                }
            };
            if !self.expect_punct(";") {
                self.recover_to_semi_or_brace();
                if self.at_punct(";") {
                    self.bump();
                }
            }
            return Some((Stmt::Goal(val), StmtSub::Bare));
        }
        if self.at_word("assert") {
            let kw = self.bump().unwrap();
            if !self.flags.assert_stmt {
                self.errors.push(
                    Diagnostic::new(
                        ErrorCode::UnexpectedToken,
                        "experimental `assert` is gated (enable `assert_stmt` feature)".to_string(),
                    )
                    .with_span(kw.span),
                );
                self.recover_to_semi_or_brace();
                if self.at_punct(";") {
                    self.bump();
                }
                return Some((Stmt::Empty, StmtSub::Bare));
            }
            let (e, e_ids) = self.parse_expr();
            if !self.expect_punct(";") {
                self.recover_to_semi_or_brace();
                if self.at_punct(";") {
                    self.bump();
                }
            }
            return Some((Stmt::Assert(e), StmtSub::Assert(e_ids)));
        }
        if self.at_punct("}") {
            // Let parse_block handle it; shouldn't happen here.
            return None;
        }
        // Expression statement without `do`? Not stable M1: report + recover.
        let sp = self.toks.get(self.pos).map(|t| t.2);
        let got = self
            .toks
            .get(self.pos)
            .map(|t| t.1.clone())
            .unwrap_or_default();
        if let Some(sp) = sp {
            self.errors.push(
                Diagnostic::new(
                    ErrorCode::UnexpectedToken,
                    format!("expected statement (`let` or `do`), found `{got}`"),
                )
                .with_span(sp),
            );
        }
        // Try to parse as `do <expr>;` for recovery, else skip one token.
        let save = self.pos;
        let (e, e_ids) = self.parse_expr();
        if self.pos == save {
            self.bump();
            self.recover_to_semi_or_brace();
            if self.at_punct(";") {
                self.bump();
            }
            return Some((Stmt::Empty, StmtSub::Bare));
        }
        if !self.expect_punct(";") {
            self.recover_to_semi_or_brace();
            if self.at_punct(";") {
                self.bump();
            }
        }
        Some((Stmt::Do(e), StmtSub::Do(e_ids)))
    }

    fn recover_to_semi_or_brace(&mut self) {
        let mut depth = 0usize;
        while self.pos < self.toks.len() {
            if self.at_punct("{") || self.at_punct("(") {
                depth += 1;
                self.bump();
            } else if self.at_punct("}") || self.at_punct(")") {
                if depth == 0 {
                    break;
                }
                depth -= 1;
                self.bump();
            } else if depth == 0 && self.at_punct(";") {
                break;
            } else {
                self.bump();
            }
        }
    }

    fn skip_balanced_block(&mut self) {
        // Skip optional string desc, then either a balanced `{...}` or to `;`.
        if matches!(self.peek(), Some((TokenKind::StrLit, _, _))) {
            self.bump();
        }
        if self.at_punct("{") {
            let mut depth = 0usize;
            while self.pos < self.toks.len() {
                if self.at_punct("{") {
                    depth += 1;
                } else if self.at_punct("}") {
                    depth -= 1;
                    self.bump();
                    if depth == 0 {
                        break;
                    }
                    continue;
                }
                self.bump();
            }
        } else {
            self.recover_to_semi_or_brace();
            if self.at_punct(";") {
                self.bump();
            }
        }
    }

    // ---- expressions (precedence climbing) ----
    //
    // Each level returns `(Expr, ExprIds)`: the span-free AST plus the
    // parallel index node. Operator spans are recorded from the operator
    // token; node covers from tokens consumed since `start`.
    fn parse_expr(&mut self) -> (Expr, ExprIds) {
        self.parse_or()
    }

    /// Fold helper for left-associative binary levels.
    fn fold_binary(
        &mut self,
        start: usize,
        lhs: (Expr, ExprIds),
        op: BinOp,
        op_span: Option<ByteSpan>,
        rhs: (Expr, ExprIds),
    ) -> (Expr, ExprIds) {
        let (l, l_ids) = lhs;
        let (r, r_ids) = rhs;
        let op_id = self.alloc_token(op_span);
        let node = self.alloc_cover(start);
        (
            Expr::Binary {
                op,
                lhs: Box::new(l),
                rhs: Box::new(r),
            },
            ExprIds {
                node,
                kind: ExprIdsKind::Binary {
                    op: op_id,
                    lhs: Box::new(l_ids),
                    rhs: Box::new(r_ids),
                },
            },
        )
    }

    fn parse_or(&mut self) -> (Expr, ExprIds) {
        let start = self.pos;
        let mut lhs = self.parse_and();
        while self.at_punct("||") {
            let osp = self.peek_span();
            self.bump();
            let rhs = self.parse_and();
            lhs = self.fold_binary(start, lhs, BinOp::Or, osp, rhs);
        }
        lhs
    }
    fn parse_and(&mut self) -> (Expr, ExprIds) {
        let start = self.pos;
        let mut lhs = self.parse_eq();
        while self.at_punct("&&") {
            let osp = self.peek_span();
            self.bump();
            let rhs = self.parse_eq();
            lhs = self.fold_binary(start, lhs, BinOp::And, osp, rhs);
        }
        lhs
    }
    fn parse_eq(&mut self) -> (Expr, ExprIds) {
        let start = self.pos;
        let mut lhs = self.parse_cmp();
        loop {
            let op = if self.at_punct("==") {
                BinOp::Eq
            } else if self.at_punct("!=") {
                BinOp::Ne
            } else {
                break;
            };
            let osp = self.peek_span();
            self.bump();
            let rhs = self.parse_cmp();
            lhs = self.fold_binary(start, lhs, op, osp, rhs);
        }
        lhs
    }
    fn parse_cmp(&mut self) -> (Expr, ExprIds) {
        let start = self.pos;
        let mut lhs = self.parse_add();
        loop {
            // Check two-char ops before one-char prefixes.
            let op = if self.at_punct("<=") {
                BinOp::Le
            } else if self.at_punct(">=") {
                BinOp::Ge
            } else if self.at_punct("<") {
                BinOp::Lt
            } else if self.at_punct(">") {
                BinOp::Gt
            } else {
                break;
            };
            let osp = self.peek_span();
            self.bump();
            let rhs = self.parse_add();
            lhs = self.fold_binary(start, lhs, op, osp, rhs);
        }
        lhs
    }
    fn parse_add(&mut self) -> (Expr, ExprIds) {
        let start = self.pos;
        let mut lhs = self.parse_mul();
        loop {
            let op = if self.at_punct("+") {
                BinOp::Add
            } else if self.at_punct("-") {
                BinOp::Sub
            } else {
                break;
            };
            let osp = self.peek_span();
            self.bump();
            let rhs = self.parse_mul();
            lhs = self.fold_binary(start, lhs, op, osp, rhs);
        }
        lhs
    }
    fn parse_mul(&mut self) -> (Expr, ExprIds) {
        let start = self.pos;
        let mut lhs = self.parse_unary();
        loop {
            let op = if self.at_punct("*") {
                BinOp::Mul
            } else if self.at_punct("/") {
                BinOp::Div
            } else if self.at_punct("%") {
                BinOp::Mod
            } else {
                break;
            };
            let osp = self.peek_span();
            self.bump();
            let rhs = self.parse_unary();
            lhs = self.fold_binary(start, lhs, op, osp, rhs);
        }
        lhs
    }
    fn parse_unary(&mut self) -> (Expr, ExprIds) {
        let start = self.pos;
        if self.at_punct("-") {
            let osp = self.peek_span();
            self.bump();
            let (rhs, rhs_ids) = self.parse_unary();
            let op_id = self.alloc_token(osp);
            let node = self.alloc_cover(start);
            return (
                Expr::Unary {
                    op: UnOp::Neg,
                    rhs: Box::new(rhs),
                },
                ExprIds {
                    node,
                    kind: ExprIdsKind::Unary {
                        op: op_id,
                        rhs: Box::new(rhs_ids),
                    },
                },
            );
        }
        if self.at_punct("!") {
            let osp = self.peek_span();
            self.bump();
            let (rhs, rhs_ids) = self.parse_unary();
            let op_id = self.alloc_token(osp);
            let node = self.alloc_cover(start);
            return (
                Expr::Unary {
                    op: UnOp::Not,
                    rhs: Box::new(rhs),
                },
                ExprIds {
                    node,
                    kind: ExprIdsKind::Unary {
                        op: op_id,
                        rhs: Box::new(rhs_ids),
                    },
                },
            );
        }
        self.parse_postfix()
    }
    fn parse_postfix(&mut self) -> (Expr, ExprIds) {
        let start = self.pos;
        let (mut e, mut e_ids) = self.parse_primary();
        // Chained calls: f()() rejected — only ident calls; loop handles f(a)(b)
        // by reporting unexpected but recovering.
        loop {
            if self.at_punct("(") {
                // Callee span: the identifier token just before `(`.
                let callee_id = e_ids.node;
                match &e {
                    Expr::Ident(name) => {
                        let name = name.clone();
                        let open_span = self.peek_span();
                        self.bump();
                        let mut args = Vec::new();
                        let mut arg_ids = Vec::new();
                        if self.at_punct(")") {
                            self.bump();
                        } else {
                            loop {
                                if self.pos >= self.toks.len() {
                                    self.errors.push(Diagnostic::new(
                                        ErrorCode::UnexpectedEof,
                                        "expected `)`, found end of input",
                                    ));
                                    break;
                                }
                                if self.at_punct(")") {
                                    self.bump();
                                    break;
                                }
                                let (a, a_ids) = self.parse_expr();
                                args.push(a);
                                arg_ids.push(a_ids);
                                if self.at_punct(",") {
                                    self.bump();
                                } else if self.at_punct(")") {
                                    continue;
                                } else if self.pos >= self.toks.len() {
                                    self.errors.push(Diagnostic::new(
                                        ErrorCode::UnexpectedEof,
                                        "expected `,` or `)`, found end of input",
                                    ));
                                    break;
                                } else {
                                    let sp = self.toks[self.pos].2;
                                    let got = self.toks[self.pos].1.clone();
                                    self.errors.push(
                                        Diagnostic::new(
                                            ErrorCode::UnexpectedToken,
                                            format!("expected `,` or `)`, found `{got}`"),
                                        )
                                        .with_span(sp),
                                    );
                                    self.bump();
                                }
                            }
                        }
                        // Argument-list span covers `(`..`)`.
                        let hi = self.toks[..self.pos.min(self.toks.len())]
                            .iter()
                            .next_back()
                            .map(|t| t.2.end);
                        let arg_list_sp = match (open_span, hi) {
                            (Some(o), Some(h)) => ByteSpan {
                                file: self.file,
                                start: o.start,
                                end: h.max(o.start),
                            },
                            _ => self.cover(start),
                        };
                        let arg_list_id = self.alloc(arg_list_sp);
                        let node = self.alloc_cover(start);
                        e = Expr::Call { callee: name, args };
                        e_ids = ExprIds {
                            node,
                            kind: ExprIdsKind::Call {
                                callee: callee_id,
                                arg_list: arg_list_id,
                                args: arg_ids,
                            },
                        };
                    }
                    _ => break,
                }
            } else {
                break;
            }
        }
        (e, e_ids)
    }
    fn parse_primary(&mut self) -> (Expr, ExprIds) {
        let start = self.pos;
        // Parenthesized: no new node, return the inner expression as-is.
        if self.at_punct("(") {
            self.bump();
            let e = self.parse_expr();
            if !self.expect_punct(")") {
                // recover: continue with inner expr
            }
            return e;
        }
        let e: Expr = match self.peek() {
            Some((TokenKind::Number, _, _)) => {
                let s = self.bump().unwrap();
                // Integer literal; overflow / float-suffix handled as errors.
                match s.text.parse::<i64>() {
                    Ok(v) => Expr::Int(v),
                    Err(_) => {
                        self.errors.push(
                            Diagnostic::new(
                                ErrorCode::ParseError,
                                format!("invalid integer literal `{}`", s.text),
                            )
                            .with_span(s.span),
                        );
                        Expr::Int(0)
                    }
                }
            }
            Some((TokenKind::StrLit, _, _)) => {
                let s = self.bump().unwrap();
                if !s.text.ends_with('"') || s.text.len() < 2 {
                    self.errors.push(
                        Diagnostic::new(ErrorCode::UnexpectedEof, "unterminated string literal")
                            .with_span(s.span),
                    );
                    Expr::Str(unquote_terminated(&s.text))
                } else {
                    Expr::Str(unquote(&s.text))
                }
            }
            Some((TokenKind::Word, s, _)) if s == "true" => {
                self.bump();
                Expr::Bool(true)
            }
            Some((TokenKind::Word, s, _)) if s == "false" => {
                self.bump();
                Expr::Bool(false)
            }
            Some((TokenKind::Word, s, _)) if s == "null" => {
                self.bump();
                Expr::Null
            }
            Some((TokenKind::Word, _, _)) => {
                let w = self.bump().unwrap();
                if matches!(w.text.as_str(), "fn" | "let" | "do") {
                    let sp = w.span;
                    self.errors.push(
                        Diagnostic::new(
                            ErrorCode::UnexpectedToken,
                            format!("expected expression, found keyword `{}`", w.text),
                        )
                        .with_span(sp),
                    );
                    Expr::Int(0)
                } else {
                    Expr::Ident(w.text)
                }
            }
            Some((_, _, sp)) => {
                let sp = *sp;
                let got = self.toks[self.pos].1.clone();
                self.errors.push(
                    Diagnostic::new(
                        ErrorCode::UnexpectedToken,
                        format!("expected expression, found `{got}`"),
                    )
                    .with_span(sp),
                );
                self.bump();
                Expr::Int(0)
            }
            None => {
                self.errors.push(Diagnostic::new(
                    ErrorCode::UnexpectedEof,
                    "expected expression, found end of input",
                ));
                Expr::Int(0)
            }
        };
        let node = self.alloc_cover(start);
        (
            e,
            ExprIds {
                node,
                kind: ExprIdsKind::Leaf,
            },
        )
    }
}

fn unquote(s: &str) -> String {
    let inner = s.strip_prefix('"').unwrap_or(s);
    let inner = inner.strip_suffix('"').unwrap_or(inner);
    unescape(inner)
}

fn unquote_terminated(s: &str) -> String {
    let inner = s.strip_prefix('"').unwrap_or(s);
    unescape(inner)
}

fn unescape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => o.push('\n'),
                Some('t') => o.push('\t'),
                Some('r') => o.push('\r'),
                Some('"') => o.push('"'),
                Some('\\') => o.push('\\'),
                Some(x) => {
                    o.push('\\');
                    o.push(x);
                }
                None => o.push('\\'),
            }
        } else {
            o.push(c);
        }
    }
    o
}

fn escape_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c => o.push(c),
        }
    }
    o
}

// ---------------------------------------------------------------------------
// Public parse entry points
// ---------------------------------------------------------------------------

/// Parse with default flags (experimental items gated off).
pub fn parse(file: FileId, bytes: &[u8]) -> ParseResult {
    parse_with_flags(file, bytes, FeatureFlags::default())
}

/// Parse with explicit feature flags. Total: never panics.
pub fn parse_with_flags(file: FileId, bytes: &[u8], flags: FeatureFlags) -> ParseResult {
    // M0 compat: token count from raw lex.
    let token_count = lex(file, bytes).len();
    let mut p = Parser::new(file, bytes, flags);
    // Empty input parses clean to an empty program.
    let (program, index) = p.parse_program();
    let spans = std::mem::take(&mut p.spans);
    // Unterminated block comment is a lex-level error: report E0003.
    // Detect: trailing `/*` without `*/`.
    let errs = p.errors;
    ParseResult {
        root: GreenNode { token_count },
        program,
        errors: errs,
        spans,
        index,
    }
}

// ---------------------------------------------------------------------------
// Formatter (canonical pretty-printer)
// ---------------------------------------------------------------------------

/// Canonical formatter. Idempotent: `fmt(fmt(x)) == fmt(x)`.
pub fn format_program(p: &Program) -> String {
    fmt_program(p)
}

/// M1 byte-level formatter entry point (lossless-CST backed, driver-facing).
///
/// Pure, total, idempotent: never panics on any input (empty, arbitrary or
/// invalid-UTF-8 bytes). Parses with default flags; a clean parse emits the
/// canonical pretty-printer output, while inputs with diagnostics pass
/// through byte-for-byte (lossless, a fixed point). Both branches are
/// idempotent: canonical output re-parses clean to itself.
pub fn format(file: FileId, bytes: &[u8]) -> Vec<u8> {
    let r = parse(file, bytes);
    if r.errors.is_empty() {
        fmt_program(&r.program).into_bytes()
    } else {
        bytes.to_vec()
    }
}

fn fmt_program(p: &Program) -> String {
    let mut o = String::new();
    for (i, it) in p.items.iter().enumerate() {
        if i > 0 {
            o.push('\n');
        }
        match it {
            Item::Fn(f) => {
                o.push_str("fn ");
                o.push_str(&f.name);
                o.push('(');
                for (j, pr) in f.params.iter().enumerate() {
                    if j > 0 {
                        o.push_str(", ");
                    }
                    o.push_str(pr);
                }
                o.push_str(") ");
                o.push_str(&fmt_block(&f.body, 0));
                o.push('\n');
            }
            Item::Intent(b) => {
                o.push_str("intent \"");
                o.push_str(&escape_str(&b.desc));
                o.push_str("\" ");
                o.push_str(&fmt_block(&b.body, 0));
                o.push('\n');
            }
        }
    }
    o
}

fn fmt_block(b: &Block, indent: usize) -> String {
    let pad = "    ".repeat(indent);
    let inner = "    ".repeat(indent + 1);
    if b.stmts.is_empty() {
        return "{}".to_string();
    }
    let mut o = String::from("{\n");
    for s in &b.stmts {
        o.push_str(&inner);
        o.push_str(&fmt_stmt(s, indent + 1));
        o.push('\n');
    }
    o.push_str(&pad);
    o.push('}');
    o
}

fn fmt_stmt(s: &Stmt, _indent: usize) -> String {
    match s {
        Stmt::Let { name, expr } => format!("let {name} = {};", fmt_expr(expr, 0)),
        Stmt::Do(e) => format!("do {};", fmt_expr(e, 0)),
        Stmt::Goal(g) => format!("goal = \"{}\";", escape_str(g)),
        Stmt::Assert(e) => format!("assert {};", fmt_expr(e, 0)),
        Stmt::Empty => ";".to_string(),
    }
}

fn fmt_expr(e: &Expr, parent_prec: u8) -> String {
    let (s, prec) = match e {
        Expr::Int(v) => (v.to_string(), 100),
        Expr::Str(s) => (format!("\"{}\"", escape_str(s)), 100),
        Expr::Bool(b) => (b.to_string(), 100),
        Expr::Null => ("null".to_string(), 100),
        Expr::Ident(n) => (n.clone(), 100),
        Expr::Call { callee, args } => {
            let a: Vec<_> = args.iter().map(|x| fmt_expr(x, 0)).collect();
            (format!("{callee}({})", a.join(", ")), 100)
        }
        Expr::Unary { op, rhs } => {
            let o = match op {
                UnOp::Neg => "-",
                UnOp::Not => "!",
            };
            (format!("{o}{}", fmt_expr(rhs, 90)), 90)
        }
        Expr::Binary { op, lhs, rhs } => {
            let p = bin_prec(*op);
            // Left-assoc: right side gets stricter parent.
            let l = fmt_expr(lhs, p);
            let r = fmt_expr(rhs, p + 1);
            (format!("{l} {} {r}", op.as_str()), p)
        }
    };
    if prec < parent_prec {
        format!("({s})")
    } else {
        s
    }
}

fn bin_prec(op: BinOp) -> u8 {
    match op {
        BinOp::Or => 10,
        BinOp::And => 20,
        BinOp::Eq | BinOp::Ne => 30,
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 40,
        BinOp::Add | BinOp::Sub => 50,
        BinOp::Mul | BinOp::Div | BinOp::Mod => 60,
    }
}

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", fmt_program(self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_on_arbitrary_bytes() {
        let mut all: Vec<u8> = (0u8..=255u8).collect();
        all.extend_from_slice(&[0xE2, 0x82]);
        let r = parse(FileId(0), &all);
        assert_eq!(r.root.token_count, lex(FileId(0), &all).len());
        let e = parse(FileId(0), b"");
        assert!(e.errors.is_empty() && e.root.token_count == 0);
        for len in 0..all.len() {
            let _ = parse(FileId(7), &all[..len]);
        }
    }

    #[test]
    fn error_tokens_cover_invalid_bytes_losslessly() {
        let bytes = [0x41, 0xFF, 0x42];
        let toks = lex(FileId(0), &bytes);
        assert!(toks.iter().any(|t| t.kind == TokenKind::Error));
        let err = toks.iter().find(|t| t.kind == TokenKind::Error).unwrap();
        assert_eq!((err.span.start, err.span.end), (1, 2));
        let r = parse(FileId(0), &bytes);
        assert!(
            r.errors
                .iter()
                .filter(|d| d.render_human().contains("AION-E0001"))
                .count()
                >= 1
        );
        assert!(r
            .errors
            .iter()
            .any(|d| d.render_human().contains("AION-E0001")));
    }

    #[test]
    fn byte_format_is_total_and_idempotent() {
        // Total on all byte values + truncated sequence + empty; fixed point.
        let mut all: Vec<u8> = (0u8..=255u8).collect();
        all.extend_from_slice(&[0xE2, 0x82]);
        for len in 0..=all.len() {
            let once = format(FileId(3), &all[..len]);
            let twice = format(FileId(3), &once);
            assert_eq!(once, twice, "not idempotent at prefix {len}");
        }
        assert_eq!(format(FileId(0), b""), b"");
        // Clean parse normalizes trivia.
        assert_eq!(
            format(FileId(0), b"fn   main(){let x=1+2*3;do x;}"),
            b"fn main() {\n    let x = 1 + 2 * 3;\n    do x;\n}\n".to_vec()
        );
        // Inputs with diagnostics pass through losslessly.
        let bad = [0x41, 0xFF, 0x42];
        assert_eq!(format(FileId(0), &bad), bad.to_vec());
    }

    #[test]
    fn formatter_idempotent_smoke() {
        let r = parse(FileId(0), b"fn main(){let x=1+2*3;do x;}");
        assert!(r.errors.is_empty());
        let once = r.formatted();
        let twice = format_program(&parse(FileId(0), once.as_bytes()).program);
        assert_eq!(once, twice);
    }
}
