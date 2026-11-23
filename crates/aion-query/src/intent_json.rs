//! Canonical `aion.intent/v1` envelope: hand-rolled emitter and reader.
//!
//! Key order is `schema`, `version`, `entry`, `intents`; omit-when-empty
//! throughout; single LF terminator. Canonical bytes are the contract;
//! `Debug` output is never contractual. Zero dependencies.

use crate::intents::{GoalRefView, GoalView, IntentView};
use aion_span::ByteSpan;
use aion_syntax::{BinOp, Expr, UnOp};

/// Parsed `aion.intent/v1` envelope: entry name plus owned intent views.
/// Re-emitting via [`intent_canonical_bytes`] yields the canonical form.
#[derive(Debug, Clone)]
pub struct ParsedIntent {
    pub entry: Option<String>,
    pub intents: Vec<IntentView>,
}

/// Canonical envelope bytes for an entry name plus intent views.
pub fn intent_canonical_bytes(entry: Option<&str>, intents: &[IntentView]) -> Vec<u8> {
    let mut s = String::from("{\"schema\":\"aion.intent/v1\",\"version\":1");
    if let Some(name) = entry {
        s.push_str(",\"entry\":{\"name\":\"");
        s.push_str(&esc(name));
        s.push_str("\"}");
    }
    if !intents.is_empty() {
        s.push_str(",\"intents\":[");
        for (i, v) in intents.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            emit_intent(&mut s, v);
        }
        s.push(']');
    }
    s.push_str("}\n");
    s.into_bytes()
}

fn emit_intent(s: &mut String, v: &IntentView) {
    s.push_str("{\"owner\":{\"item\":\"");
    s.push_str(&esc(&v.owner));
    s.push('"');
    if let Some(sp) = v.owner_span {
        s.push_str(",\"span\":");
        emit_span(s, sp);
    }
    s.push('}');
    if let Some(sp) = v.span {
        s.push_str(",\"span\":");
        emit_span(s, sp);
    }
    if !v.desc.is_empty() {
        s.push_str(",\"desc\":\"");
        s.push_str(&esc(&v.desc));
        s.push('"');
    }
    s.push_str(",\"goals\":[");
    for (i, g) in v.goals.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        emit_goal(s, g);
    }
    s.push_str("]}");
}

fn emit_goal(s: &mut String, g: &GoalView) {
    s.push_str("{\"goal_ast\":");
    emit_expr(s, &g.expr);
    if let Some(sp) = g.span {
        s.push_str(",\"goal_span\":");
        emit_span(s, sp);
    }
    s.push_str(",\"refs\":[");
    for (i, r) in g.refs.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        emit_ref(s, r);
    }
    s.push_str("]}");
}

fn emit_ref(s: &mut String, r: &GoalRefView) {
    s.push_str("{\"name\":\"");
    s.push_str(&esc(&r.name));
    if r.bound {
        s.push_str("\",\"status\":\"bound\"");
    } else {
        s.push_str("\",\"status\":\"unresolved\"");
    }
    if let Some(sp) = r.occurrence {
        s.push_str(",\"occurrence\":");
        emit_span(s, sp);
    }
    match (&r.decl_name, &r.decl_span) {
        (Some(n), Some(sp)) => {
            s.push_str(",\"decl\":{\"name\":\"");
            s.push_str(&esc(n));
            s.push_str("\",\"span\":");
            emit_span(s, *sp);
            s.push('}');
        }
        _ => {
            s.push_str(",\"decl\":null");
        }
    }
    if !r.bound {
        s.push_str(",\"diagnostic\":\"AION-E0011\"");
    }
    s.push('}');
}

fn emit_span(s: &mut String, sp: ByteSpan) {
    s.push_str(&format!(
        "{{\"file\":{},\"start\":{},\"end\":{}}}",
        sp.file.0, sp.start, sp.end
    ));
}

fn emit_expr(s: &mut String, e: &Expr) {
    match e {
        Expr::Int(n) => s.push_str(&format!("{{\"kind\":\"int\",\"value\":{n}}}")),
        Expr::Str(st) => {
            s.push_str("{\"kind\":\"str\",\"value\":\"");
            s.push_str(&esc(st));
            s.push_str("\"}");
        }
        Expr::Bool(b) => s.push_str(&format!(
            "{{\"kind\":\"bool\",\"value\":{}}}",
            if *b { "true" } else { "false" }
        )),
        Expr::Null => s.push_str("{\"kind\":\"null\"}"),
        Expr::Ident(n) => {
            s.push_str("{\"kind\":\"ident\",\"name\":\"");
            s.push_str(&esc(n));
            s.push_str("\"}");
        }
        Expr::Unary { op, rhs } => {
            let o = match op {
                UnOp::Neg => "-",
                UnOp::Not => "!",
            };
            s.push_str(&format!("{{\"kind\":\"unary\",\"op\":\"{o}\",\"rhs\":"));
            emit_expr(s, rhs);
            s.push('}');
        }
        Expr::Binary { op, lhs, rhs } => {
            let o = match op {
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
            };
            s.push_str(&format!("{{\"kind\":\"binary\",\"op\":\"{o}\",\"lhs\":"));
            emit_expr(s, lhs);
            s.push_str(",\"rhs\":");
            emit_expr(s, rhs);
            s.push('}');
        }
        Expr::Call { callee, args } => {
            s.push_str("{\"kind\":\"call\",\"callee\":\"");
            s.push_str(&esc(callee));
            s.push_str("\",\"args\":[");
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                emit_expr(s, a);
            }
            s.push_str("]}");
        }
    }
}

fn esc(v: &str) -> String {
    let mut o = String::with_capacity(v.len());
    for c in v.chars() {
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

/// Envelope reader: accepts bytes produced by [`intent_canonical_bytes`].
/// Checks `schema` and the major `version` (only `1` accepted; missing or
/// non-`1` versions are rejected naming the version), ignores unknown
/// object fields, and returns plain `String` errors.
pub fn parse_intent_envelope(bytes: &[u8]) -> Result<ParsedIntent, String> {
    let s = std::str::from_utf8(bytes).map_err(|e| format!("envelope is not UTF-8: {e}"))?;
    let body = s
        .strip_suffix('\n')
        .ok_or_else(|| "envelope missing LF terminator".to_string())?;
    if body.contains('\n') || body.contains('\r') {
        return Err("envelope must be a single line".to_string());
    }
    let v = JVal::parse(body)?;
    let o = v.as_obj()?;
    let schema = o
        .field("schema")
        .ok_or_else(|| "envelope missing schema".to_string())?;
    if schema.as_str()? != "aion.intent/v1" {
        return Err(format!("unsupported schema {:?}", schema.as_str()?));
    }
    let version = o
        .field("version")
        .ok_or_else(|| "envelope missing version".to_string())?;
    let major = version.as_num()?;
    if major != 1 {
        return Err(format!("unsupported version {major}"));
    }
    let entry = match o.field("entry") {
        None => None,
        Some(e) => {
            let eo = e.as_obj()?;
            let n = eo
                .field("name")
                .ok_or_else(|| "entry missing name".to_string())?;
            Some(n.as_str()?.to_string())
        }
    };
    let mut intents = Vec::new();
    if let Some(iv) = o.field("intents") {
        for item in iv.as_arr()? {
            intents.push(parse_intent(item)?);
        }
    }
    Ok(ParsedIntent { entry, intents })
}

fn parse_intent(v: &JVal) -> Result<IntentView, String> {
    let o = v.as_obj()?;
    let owner_v = o
        .field("owner")
        .ok_or_else(|| "intent missing owner".to_string())?;
    let oo = owner_v.as_obj()?;
    let item = oo
        .field("item")
        .ok_or_else(|| "owner missing item".to_string())?;
    let owner = item.as_str()?.to_string();
    let owner_span = match oo.field("span") {
        None => None,
        Some(s) => Some(parse_span(s)?),
    };
    let span = match o.field("span") {
        None => None,
        Some(s) => Some(parse_span(s)?),
    };
    let desc = match o.field("desc") {
        None => String::new(),
        Some(d) => d.as_str()?.to_string(),
    };
    let goals_v = o
        .field("goals")
        .ok_or_else(|| "intent missing goals".to_string())?;
    let mut goals = Vec::new();
    for g in goals_v.as_arr()? {
        goals.push(parse_goal(g)?);
    }
    Ok(IntentView {
        owner,
        owner_span,
        span,
        desc,
        goals,
    })
}

fn parse_goal(v: &JVal) -> Result<GoalView, String> {
    let o = v.as_obj()?;
    let ast = o
        .field("goal_ast")
        .ok_or_else(|| "goal missing goal_ast".to_string())?;
    let expr = parse_expr(ast)?;
    let span = match o.field("goal_span") {
        None => None,
        Some(s) => Some(parse_span(s)?),
    };
    let refs_v = o
        .field("refs")
        .ok_or_else(|| "goal missing refs".to_string())?;
    let mut refs = Vec::new();
    for r in refs_v.as_arr()? {
        refs.push(parse_ref(r)?);
    }
    Ok(GoalView { expr, span, refs })
}

fn parse_ref(v: &JVal) -> Result<GoalRefView, String> {
    let o = v.as_obj()?;
    let name = o
        .field("name")
        .ok_or_else(|| "ref missing name".to_string())?
        .as_str()?
        .to_string();
    let status = o
        .field("status")
        .ok_or_else(|| "ref missing status".to_string())?
        .as_str()?;
    let bound = match status {
        "bound" => true,
        "unresolved" => false,
        other => return Err(format!("unknown ref status {other:?}")),
    };
    let occurrence = match o.field("occurrence") {
        None => None,
        Some(s) => Some(parse_span(s)?),
    };
    let (decl_name, decl_span) = match o.field("decl") {
        None => (None, None),
        Some(d) => {
            if d.is_null() {
                (None, None)
            } else {
                let dob = d.as_obj()?;
                let n = dob
                    .field("name")
                    .ok_or_else(|| "decl missing name".to_string())?
                    .as_str()?
                    .to_string();
                let sp = parse_span(
                    dob.field("span")
                        .ok_or_else(|| "decl missing span".to_string())?,
                )?;
                (Some(n), Some(sp))
            }
        }
    };
    Ok(GoalRefView {
        name,
        bound,
        occurrence,
        decl_name,
        decl_span,
    })
}

fn parse_span(v: &JVal) -> Result<ByteSpan, String> {
    let o = v.as_obj()?;
    let num = |k: &str| -> Result<u32, String> {
        let n = ObjGet::field(o, k)
            .ok_or_else(|| format!("span missing {k}"))?
            .as_num()?;
        u32::try_from(n).map_err(|_| format!("span field {k} out of range: {n}"))
    };
    let (file, start, end) = (num("file")?, num("start")?, num("end")?);
    ByteSpan::new(aion_span::FileId(file), start, end)
        .ok_or_else(|| "span has start > end".to_string())
}

fn parse_expr(v: &JVal) -> Result<Expr, String> {
    let o = v.as_obj()?;
    let kind = o
        .field("kind")
        .ok_or_else(|| "expr missing kind".to_string())?
        .as_str()?;
    match kind {
        "int" => {
            let n = o
                .field("value")
                .ok_or_else(|| "int expr missing value".to_string())?
                .as_num()?;
            Ok(Expr::Int(n))
        }
        "str" => {
            let s = o
                .field("value")
                .ok_or_else(|| "str expr missing value".to_string())?
                .as_str()?
                .to_string();
            Ok(Expr::Str(s))
        }
        "bool" => {
            let b = o
                .field("value")
                .ok_or_else(|| "bool expr missing value".to_string())?
                .as_bool()?;
            Ok(Expr::Bool(b))
        }
        "null" => Ok(Expr::Null),
        "ident" => {
            let n = o
                .field("name")
                .ok_or_else(|| "ident expr missing name".to_string())?
                .as_str()?
                .to_string();
            Ok(Expr::Ident(n))
        }
        "unary" => {
            let op = o
                .field("op")
                .ok_or_else(|| "unary expr missing op".to_string())?
                .as_str()?;
            let op = match op {
                "-" => UnOp::Neg,
                "!" => UnOp::Not,
                other => return Err(format!("unknown unary op {other:?}")),
            };
            let rhs = parse_expr(
                o.field("rhs")
                    .ok_or_else(|| "unary expr missing rhs".to_string())?,
            )?;
            Ok(Expr::Unary {
                op,
                rhs: Box::new(rhs),
            })
        }
        "binary" => {
            let op = o
                .field("op")
                .ok_or_else(|| "binary expr missing op".to_string())?
                .as_str()?;
            let op = match op {
                "||" => BinOp::Or,
                "&&" => BinOp::And,
                "==" => BinOp::Eq,
                "!=" => BinOp::Ne,
                "<" => BinOp::Lt,
                "<=" => BinOp::Le,
                ">" => BinOp::Gt,
                ">=" => BinOp::Ge,
                "+" => BinOp::Add,
                "-" => BinOp::Sub,
                "*" => BinOp::Mul,
                "/" => BinOp::Div,
                "%" => BinOp::Mod,
                other => return Err(format!("unknown binary op {other:?}")),
            };
            let lhs = parse_expr(
                o.field("lhs")
                    .ok_or_else(|| "binary expr missing lhs".to_string())?,
            )?;
            let rhs = parse_expr(
                o.field("rhs")
                    .ok_or_else(|| "binary expr missing rhs".to_string())?,
            )?;
            Ok(Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            })
        }
        "call" => {
            let callee = o
                .field("callee")
                .ok_or_else(|| "call expr missing callee".to_string())?
                .as_str()?
                .to_string();
            let args_v = o
                .field("args")
                .ok_or_else(|| "call expr missing args".to_string())?;
            let mut args = Vec::new();
            for a in args_v.as_arr()? {
                args.push(parse_expr(a)?);
            }
            Ok(Expr::Call { callee, args })
        }
        other => Err(format!("unknown expr kind {other:?}")),
    }
}

// --- Minimal JSON value parser (envelope subset; unknown fields kept
// --- in the tree but ignored by the converters above). ---

#[derive(Debug, Clone)]
enum JVal {
    Obj(Vec<(String, JVal)>),
    Arr(Vec<JVal>),
    Str(String),
    Num(i64),
    Bool(bool),
    Null,
}

impl JVal {
    fn parse(s: &str) -> Result<JVal, String> {
        let b = s.as_bytes();
        let mut p = JCur { b, pos: 0 };
        p.ws();
        let v = p.value()?;
        p.ws();
        if p.pos != b.len() {
            return Err("envelope has trailing content".to_string());
        }
        Ok(v)
    }

    fn as_obj(&self) -> Result<&[(String, JVal)], String> {
        match self {
            JVal::Obj(o) => Ok(o),
            _ => Err("expected JSON object".to_string()),
        }
    }

    fn as_arr(&self) -> Result<&[JVal], String> {
        match self {
            JVal::Arr(a) => Ok(a),
            _ => Err("expected JSON array".to_string()),
        }
    }

    fn as_str(&self) -> Result<&str, String> {
        match self {
            JVal::Str(s) => Ok(s),
            _ => Err("expected JSON string".to_string()),
        }
    }

    fn as_num(&self) -> Result<i64, String> {
        match self {
            JVal::Num(n) => Ok(*n),
            _ => Err("expected JSON number".to_string()),
        }
    }

    fn as_bool(&self) -> Result<bool, String> {
        match self {
            JVal::Bool(b) => Ok(*b),
            _ => Err("expected JSON boolean".to_string()),
        }
    }

    fn is_null(&self) -> bool {
        matches!(self, JVal::Null)
    }
}

trait ObjGet {
    fn field(&self, key: &str) -> Option<&JVal>;
}

impl ObjGet for [(String, JVal)] {
    fn field(&self, key: &str) -> Option<&JVal> {
        self.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

struct JCur<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> JCur<'a> {
    fn ws(&mut self) {
        while self.pos < self.b.len() && matches!(self.b[self.pos], b' ' | b'\t') {
            self.pos += 1;
        }
    }

    fn err(&self, msg: &str) -> String {
        format!("{msg} at byte {}", self.pos)
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
            Err(self.err("expected delimiter"))
        }
    }

    fn value(&mut self) -> Result<JVal, String> {
        self.ws();
        if self.pos >= self.b.len() {
            return Err(self.err("unexpected end of envelope"));
        }
        match self.b[self.pos] {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => Ok(JVal::Str(self.string()?)),
            b't' => self.lit("true", JVal::Bool(true)),
            b'f' => self.lit("false", JVal::Bool(false)),
            b'n' => self.lit("null", JVal::Null),
            b'-' | b'0'..=b'9' => Ok(JVal::Num(self.number()?)),
            _ => Err(self.err("unexpected character")),
        }
    }

    fn lit(&mut self, word: &str, v: JVal) -> Result<JVal, String> {
        if self.b[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            Ok(v)
        } else {
            Err(self.err("invalid literal"))
        }
    }

    fn object(&mut self) -> Result<JVal, String> {
        self.expect(b'{')?;
        let mut out = Vec::new();
        self.ws();
        if self.eat(b'}') {
            return Ok(JVal::Obj(out));
        }
        loop {
            self.ws();
            if self.pos >= self.b.len() || self.b[self.pos] != b'"' {
                return Err(self.err("expected object key"));
            }
            let k = self.string()?;
            self.ws();
            self.expect(b':')?;
            let v = self.value()?;
            out.push((k, v));
            self.ws();
            if self.eat(b',') {
                continue;
            }
            self.ws();
            if self.eat(b'}') {
                break;
            }
            return Err(self.err("expected `,` or `}`"));
        }
        Ok(JVal::Obj(out))
    }

    fn array(&mut self) -> Result<JVal, String> {
        self.expect(b'[')?;
        let mut out = Vec::new();
        self.ws();
        if self.eat(b']') {
            return Ok(JVal::Arr(out));
        }
        loop {
            let v = self.value()?;
            out.push(v);
            self.ws();
            if self.eat(b',') {
                continue;
            }
            self.ws();
            if self.eat(b']') {
                break;
            }
            return Err(self.err("expected `,` or `]`"));
        }
        Ok(JVal::Arr(out))
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut o = String::new();
        loop {
            if self.pos >= self.b.len() {
                return Err(self.err("unterminated string"));
            }
            let c = self.b[self.pos];
            self.pos += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    if self.pos >= self.b.len() {
                        return Err(self.err("unterminated escape"));
                    }
                    let e = self.b[self.pos];
                    self.pos += 1;
                    match e {
                        b'"' => o.push('"'),
                        b'\\' => o.push('\\'),
                        b'/' => o.push('/'),
                        b'n' => o.push('\n'),
                        b'r' => o.push('\r'),
                        b't' => o.push('\t'),
                        b'b' => o.push('\u{0008}'),
                        b'f' => o.push('\u{000C}'),
                        b'u' => {
                            let cp = self.hex4()?;
                            o.push(char::from_u32(cp).ok_or_else(|| self.err("bad codepoint"))?);
                        }
                        _ => return Err(self.err("invalid escape")),
                    }
                }
                0x00..=0x1F => return Err(self.err("unescaped control character")),
                _ => {
                    let rest = &self.b[self.pos - 1..];
                    let ch = std::str::from_utf8(rest)
                        .map_err(|_| self.err("invalid UTF-8"))?
                        .chars()
                        .next()
                        .ok_or_else(|| self.err("invalid UTF-8"))?;
                    self.pos += ch.len_utf8() - 1;
                    o.push(ch);
                }
            }
        }
        Ok(o)
    }

    fn hex4(&mut self) -> Result<u32, String> {
        if self.pos + 4 > self.b.len() {
            return Err(self.err("truncated unicode escape"));
        }
        let mut v: u32 = 0;
        for i in 0..4 {
            let c = self.b[self.pos + i];
            let d = match c {
                b'0'..=b'9' => (c - b'0') as u32,
                b'a'..=b'f' => (c - b'a' + 10) as u32,
                b'A'..=b'F' => (c - b'A' + 10) as u32,
                _ => return Err(self.err("invalid unicode escape")),
            };
            v = v * 16 + d;
        }
        self.pos += 4;
        Ok(v)
    }

    fn number(&mut self) -> Result<i64, String> {
        let start = self.pos;
        if self.eat(b'-') {
            // sign consumed
        }
        let mut digits = 0;
        while self.pos < self.b.len() && self.b[self.pos].is_ascii_digit() {
            self.pos += 1;
            digits += 1;
        }
        if digits == 0 {
            return Err(self.err("invalid number"));
        }
        std::str::from_utf8(&self.b[start..self.pos])
            .map_err(|_| self.err("invalid number"))?
            .parse::<i64>()
            .map_err(|_| self.err("integer out of range"))
    }
}
