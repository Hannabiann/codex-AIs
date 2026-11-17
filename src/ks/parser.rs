use std::{fmt, rc::Rc};
#[derive(Debug, Clone)]
pub struct Error {
    pub offset: usize,
    pub message: String,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "byte {}: {}", self.offset, self.message)
    }
}
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Word(String),
    String(String),
    Regex(String, String),
    Number(String),
    Sym(String),
    Template(String),
    End,
}
#[derive(Debug, Clone)]
struct Token {
    kind: Kind,
    start: usize,
    end: usize,
}
#[derive(Debug, Clone)]
pub enum Expr {
    Name(String),
    String(String),
    Regex(String, String),
    Number(f64),
    Unary(String, Box<Expr>),
    Binary(String, Box<Expr>, Box<Expr>),
    Conditional(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    Member(Box<Expr>, Box<Expr>),
    Array(Vec<Expr>),
    Object(Vec<(String, Expr)>),
    Function(Option<String>, Vec<String>, Rc<Vec<Stmt>>),
}
#[derive(Debug, Clone)]
pub enum Part {
    Text(String),
    Value(Expr),
}
#[derive(Debug, Clone)]
pub enum Stmt {
    With(Expr, Box<Stmt>),
    Empty,
    Expression(Expr),
    Block(Vec<Stmt>),
    Var(Vec<(String, Option<Expr>)>),
    Function(String, Vec<String>, Rc<Vec<Stmt>>),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    While(Expr, Box<Stmt>),
    Do(Box<Stmt>, Expr),
    For(Option<Box<Stmt>>, Option<Expr>, Option<Expr>, Box<Stmt>),
    ForIn(Box<Stmt>, Expr, Box<Stmt>),
    Return(Option<Expr>),
    Break,
    Continue,
    Throw(Expr),
    Rethrow,
    Try(Box<Stmt>, Option<Catch>, Option<Box<Stmt>>),
    Switch(Expr, Vec<(Option<Expr>, Vec<Stmt>)>),
    Template(Vec<Part>),
    Synchronized(Box<Stmt>),
}
#[derive(Debug, Clone)]
pub struct Catch {
    pub name: String,
    pub body: Box<Stmt>,
    pub declare: bool,
}
fn ident(c: char) -> bool {
    c == '_' || c == '$' || c.is_alphabetic()
}
fn lex(s: &str) -> Result<Vec<Token>, Error> {
    let mut out: Vec<Token> = Vec::new();
    let mut p = 0;
    while p < s.len() {
        let c = s[p..].chars().next().unwrap();
        if c.is_whitespace() || matches!(c, '\0' | '\x01') {
            p += c.len_utf8();
            continue;
        }
        if s[p..].starts_with("//") {
            p += s[p..].find('\n').unwrap_or(s.len() - p);
            continue;
        }
        if s[p..].starts_with("/*") {
            let n = s[p + 2..].find("*/").ok_or(Error {
                offset: p,
                message: "unterminated comment".into(),
            })?;
            p += n + 4;
            continue;
        }
        if c == '#'
            && s[..p]
                .trim_end_matches([' ', '\t'])
                .chars()
                .last()
                .is_some_and(|c| "()[]{} ,;:".contains(c))
        {
            p += s[p..].find('\n').unwrap_or(s.len() - p);
            continue;
        }
        if c == '#' {
            let end = p + s[p..].find('\n').unwrap_or(s.len() - p);
            let word = s[p..end].split_whitespace().next().unwrap_or("");
            if !matches!(word, "#endregion" | "#region") {
                return Err(Error {
                    offset: p,
                    message: "unsupported directive".into(),
                });
            }
            p = end;
            continue;
        }
        if s[p..].starts_with("{#") {
            out.push(Token {
                kind: Kind::Sym("{".into()),
                start: p,
                end: p + 2,
            });
            p += s[p..].find('\n').unwrap_or(s.len() - p);
            continue;
        }
        if s[p..].starts_with("}#") {
            out.push(Token {
                kind: Kind::Sym("}".into()),
                start: p,
                end: p + 2,
            });
            p += s[p..].find('\n').unwrap_or(s.len() - p);
            continue;
        }
        let start = p;
        let kind = if s[p..].starts_with("{/") {
            let n = s[p + 2..].find("/}").ok_or(Error {
                offset: p,
                message: "unterminated HTML block".into(),
            })?;
            let text = s[p + 2..p + 2 + n].to_string();
            p += n + 4;
            Kind::Template(text)
        } else if c == '/'
            && out.last().map_or(true, |t| match &t.kind {
                Kind::Sym(s) => matches!(
                    s.as_str(),
                    "(" | "[" | "{" | "," | "=" | ":" | "?" | ";" | "!" | "&&" | "||"
                ),
                Kind::Word(s) => matches!(s.as_str(), "return" | "throw" | "case"),
                _ => false,
            })
        {
            p += 1;
            let mut pattern = String::new();
            let mut class = false;
            let mut closed = false;
            while p < s.len() {
                let ch = s[p..].chars().next().unwrap();
                p += ch.len_utf8();
                if ch == '\n' || ch == '\r' {
                    return Err(Error {
                        offset: start,
                        message: "newline in regex literal".into(),
                    });
                }
                if ch == '\\' {
                    let next = s[p..].chars().next().ok_or(Error {
                        offset: start,
                        message: "unterminated regex escape".into(),
                    })?;
                    p += next.len_utf8();
                    if next != '/' {
                        pattern.push('\\');
                    }
                    pattern.push(next);
                } else if ch == '/' && !class {
                    closed = true;
                    break;
                } else {
                    if ch == '[' {
                        class = true;
                    } else if ch == ']' {
                        class = false;
                    }
                    pattern.push(ch);
                }
                if pattern.len() > 16384 {
                    return Err(Error {
                        offset: start,
                        message: "regex pattern limit".into(),
                    });
                }
            }
            if !closed {
                return Err(Error {
                    offset: start,
                    message: "unterminated regex literal".into(),
                });
            }
            let begin = p;
            while s.as_bytes().get(p).is_some_and(u8::is_ascii_alphabetic) {
                p += 1;
            }
            Kind::Regex(pattern, s[begin..p].into())
        } else if c == '\'' || c == '"' {
            p += 1;
            let mut value = String::new();
            let mut closed = false;
            while p < s.len() {
                let ch = s[p..].chars().next().unwrap();
                p += ch.len_utf8();
                if ch == c {
                    closed = true;
                    break;
                }
                if ch == '\\' && c == '"' {
                    let e = s[p..].chars().next().ok_or(Error {
                        offset: p,
                        message: "unterminated escape".into(),
                    })?;
                    p += e.len_utf8();
                    match e {
                        'n' => value.push('\n'),
                        'r' => value.push('\r'),
                        't' => value.push('\t'),
                        'b' => value.push('\u{8}'),
                        'f' => value.push('\u{c}'),
                        '\\' | '\'' | '"' | '/' => value.push(e),
                        '\n' => {}
                        '\r' => {
                            if s[p..].starts_with('\n') {
                                p += 1;
                            }
                        }
                        'x' | 'u' => {
                            let n = if e == 'x' { 2 } else { 4 };
                            let raw = s.get(p..p + n).ok_or(Error {
                                offset: p,
                                message: "short escape".into(),
                            })?;
                            let v = u32::from_str_radix(raw, 16)
                                .ok()
                                .and_then(char::from_u32)
                                .ok_or(Error {
                                    offset: p,
                                    message: "invalid character escape".into(),
                                })?;
                            value.push(v);
                            p += n;
                        }
                        _ => {
                            value.push('\\');
                            value.push(e);
                        }
                    }
                } else {
                    value.push(ch);
                }
            }
            if !closed {
                return Err(Error {
                    offset: start,
                    message: "unterminated string".into(),
                });
            }
            Kind::String(value)
        } else if ident(c) {
            p += c.len_utf8();
            while let Some(ch) = s[p..].chars().next() {
                if ident(ch) || ch.is_ascii_digit() {
                    p += ch.len_utf8();
                } else {
                    break;
                }
            }
            Kind::Word(s[start..p].into())
        } else if c.is_ascii_digit()
            || (c == '.' && s.as_bytes().get(p + 1).map(u8::is_ascii_digit) == Some(true))
        {
            if s[p..].starts_with("0x") || s[p..].starts_with("0X") {
                p += 2;
                while s.as_bytes().get(p).map(u8::is_ascii_hexdigit) == Some(true) {
                    p += 1;
                }
            } else {
                while s
                    .as_bytes()
                    .get(p)
                    .map(|x| x.is_ascii_digit() || *x == b'.')
                    == Some(true)
                {
                    p += 1;
                }
                if matches!(s.as_bytes().get(p), Some(b'e' | b'E')) {
                    p += 1;
                    if matches!(s.as_bytes().get(p), Some(b'+' | b'-')) {
                        p += 1;
                    }
                    while s.as_bytes().get(p).map(u8::is_ascii_digit) == Some(true) {
                        p += 1;
                    }
                }
            }
            Kind::Number(s[start..p].into())
        } else {
            let op = [
                "...", ">>>=", "===", "!==", ">>>", "<<=", ">>=", "++", "--", "+=", "-=", "*=",
                "/=", "%=", "==", "!=", "<=", ">=", "&&", "||", "<<", ">>", "&=", "|=", "^=",
            ]
            .into_iter()
            .find(|op| s[p..].starts_with(op));
            if let Some(op) = op {
                p += op.len();
                Kind::Sym(op.into())
            } else if "{}()[];:,.?+-*/%=!~<>&|^".contains(c) {
                p += 1;
                Kind::Sym(c.to_string())
            } else {
                return Err(Error {
                    offset: p,
                    message: format!("unexpected character {c:?}"),
                });
            }
        };
        out.push(Token {
            kind,
            start,
            end: p,
        });
        if out.len() > 300_000 {
            return Err(Error {
                offset: p,
                message: "token limit".into(),
            });
        }
    }
    out.push(Token {
        kind: Kind::End,
        start: p,
        end: p,
    });
    Ok(out)
}
pub struct Parser {
    source: String,
    tokens: Vec<Token>,
    at: usize,
    depth: usize,
    no_in: bool,
}
impl Parser {
    fn new(s: &str) -> Result<Self, Error> {
        Ok(Self {
            source: s.into(),
            tokens: lex(s)?,
            at: 0,
            depth: 0,
            no_in: false,
        })
    }
    fn err(&self, s: impl Into<String>) -> Error {
        Error {
            offset: self.tokens[self.at].start,
            message: s.into(),
        }
    }
    fn is(&self, s: &str) -> bool {
        matches!(&self.tokens[self.at].kind,Kind::Sym(x)|Kind::Word(x) if x==s)
    }
    fn eat(&mut self, s: &str) -> bool {
        if self.is(s) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn want(&mut self, s: &str) -> Result<(), Error> {
        if self.eat(s) {
            Ok(())
        } else {
            Err(self.err(format!("expected {s}, got {:?}", self.tokens[self.at].kind)))
        }
    }
    fn name(&mut self) -> Result<String, Error> {
        if let Kind::Word(s) = self.tokens[self.at].kind.clone() {
            self.at += 1;
            Ok(s)
        } else {
            Err(self.err("expected identifier"))
        }
    }
    fn end(&self) -> bool {
        self.tokens[self.at].kind == Kind::End
    }
    fn list(&mut self, end: &str) -> Result<Vec<Stmt>, Error> {
        let mut v = Vec::new();
        while !self.end() && !self.is(end) {
            let mut s = self.stmt()?;
            if self.eat("finally") {
                s = Stmt::Try(Box::new(s), None, Some(Box::new(self.stmt()?)));
            }
            v.push(s);
        }
        if !end.is_empty() {
            self.want(end)?;
        }
        Ok(v)
    }
    fn block(&mut self) -> Result<Vec<Stmt>, Error> {
        self.want("{")?;
        self.list("}")
    }
    fn newline(&self) -> bool {
        self.at > 0
            && self.source[self.tokens[self.at - 1].end..self.tokens[self.at].start].contains('\n')
    }
    fn semi(&mut self) -> Result<(), Error> {
        if self.eat(";")
            || self.end()
            || self.is("}")
            || self.newline()
            || ["else", "break", "continue", "case", "default"]
                .iter()
                .any(|s| self.is(s))
        {
            Ok(())
        } else {
            Err(self.err("expected semicolon"))
        }
    }
    fn params_body(&mut self) -> Result<(Vec<String>, Vec<Stmt>), Error> {
        self.want("(")?;
        let mut args = Vec::new();
        if !self.eat(")") {
            loop {
                args.push(self.name()?);
                if self.eat(")") {
                    break;
                }
                self.want(",")?;
            }
        }
        let b = if self.is("{") {
            self.block()?
        } else {
            if self.is(";") {
                return Err(self.err("ambiguous semicolon before function body"));
            }
            vec![self.stmt()?]
        };
        Ok((args, b))
    }
    // KS permits expression statements inside an if condition. The native
    // compiler emits POP (0x0b) at ';', then evaluates the next expression.
    // Verified against libCommonJustice OID 730099378, code offset 0xab1ff.
    // Keep this separate from expr(): for-loop semicolons remain delimiters.
    fn condition(&mut self) -> Result<Expr, Error> {
        self.want("(")?;
        let mut e = self.expr(0)?;
        let mut count = 1;
        while self.eat(";") {
            count += 1;
            if count > 128 {
                return Err(self.err("condition sequence limit"));
            }
            e = Expr::Binary(",".into(), Box::new(e), Box::new(self.expr(0)?));
        }
        self.want(")")?;
        Ok(e)
    }
    fn paren(&mut self) -> Result<Expr, Error> {
        self.want("(")?;
        let e = self.expr(0)?;
        self.want(")")?;
        Ok(e)
    }
    fn vars(&mut self) -> Result<Stmt, Error> {
        let is_var = self.is("var");
        self.at += 1;
        let mut v = Vec::new();
        let mut statements = Vec::new();
        loop {
            let n = self.name()?;
            // Native KS permits `var existing[index] = value` in registry
            // helpers. It assigns a member; it does not redeclare the array.
            if is_var && (self.is("[") || self.is(".")) {
                self.at -= 1;
                let assignment = self.expr(2)?;
                if !matches!(&assignment, Expr::Binary(op, lhs, _) if matches!(op.as_str(), "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "&=" | "|=" | "^=" | "<<=" | ">>=" | ">>>=") && matches!(lhs.as_ref(), Expr::Member(..)))
                {
                    return Err(self.err("var member requires assignment"));
                }
                if !v.is_empty() {
                    statements.push(Stmt::Var(std::mem::take(&mut v)));
                }
                statements.push(Stmt::Expression(assignment));
                if self.eat(",") {
                    continue;
                }
                break;
            }
            let e = if self.eat("=") {
                Some(self.expr(2)?)
            } else if is_var
                && [
                    "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<=", ">>=", ">>>=",
                ]
                .iter()
                .any(|op| self.is(op))
            {
                let Kind::Sym(op) = self.tokens[self.at].kind.clone() else {
                    unreachable!()
                };
                self.at += 1;
                Some(Expr::Binary(
                    op,
                    Box::new(Expr::Name(n.clone())),
                    Box::new(self.expr(2)?),
                ))
            } else {
                None
            };
            v.push((n, e));
            if !self.eat(",") {
                break;
            }
        }
        if statements.is_empty() {
            return Ok(Stmt::Var(v));
        }
        if !v.is_empty() {
            statements.push(Stmt::Var(v));
        }
        Ok(Stmt::Block(statements))
    }
    fn stmt(&mut self) -> Result<Stmt, Error> {
        self.depth += 1;
        if self.depth > 128 {
            return Err(self.err("syntax nesting limit"));
        }
        let r = self.statement();
        self.depth -= 1;
        r
    }
    fn statement(&mut self) -> Result<Stmt, Error> {
        if self.eat(";") {
            return Ok(Stmt::Empty);
        }
        if let Kind::Template(text) = self.tokens[self.at].kind.clone() {
            let off = self.tokens[self.at].start + 2;
            self.at += 1;
            return Ok(Stmt::Template(template(&text).map_err(|mut e| {
                e.offset += off;
                e
            })?));
        }
        if self.is("{") {
            return Ok(Stmt::Block(self.block()?));
        }
        if self.eat("with") {
            let e = self.paren()?;
            return Ok(Stmt::With(e, Box::new(self.stmt()?)));
        }
        if self.eat("synchronized") {
            return Ok(Stmt::Synchronized(Box::new(self.stmt()?)));
        }
        if self.eat("function") {
            let n = self.name()?;
            let (a, b) = self.params_body()?;
            return Ok(Stmt::Function(n, a, Rc::new(b)));
        }
        if self.is("var") || self.is("const") {
            let v = self.vars()?;
            self.semi()?;
            return Ok(v);
        }
        if self.eat("if") {
            let c = self.condition()?;
            let a = Box::new(self.stmt()?);
            let b = if self.eat("else") {
                Some(Box::new(self.stmt()?))
            } else {
                None
            };
            return Ok(Stmt::If(c, a, b));
        }
        if self.eat("while") {
            let c = self.paren()?;
            return Ok(Stmt::While(c, Box::new(self.stmt()?)));
        }
        if self.eat("do") {
            let b = Box::new(self.stmt()?);
            self.want("while")?;
            let c = self.paren()?;
            self.eat(";");
            return Ok(Stmt::Do(b, c));
        }
        if self.eat("for") {
            self.want("(")?;
            self.no_in = true;
            let init = if self.is(";") {
                None
            } else if self.is("var") {
                Some(Box::new(self.vars()?))
            } else {
                Some(Box::new(Stmt::Expression(self.expr(0)?)))
            };
            self.no_in = false;
            if self.eat("in") {
                let e = self.expr(0)?;
                self.want(")")?;
                return Ok(Stmt::ForIn(
                    init.ok_or(self.err("missing for-in binding"))?,
                    e,
                    Box::new(self.stmt()?),
                ));
            }
            self.want(";")?;
            let c = if self.is(";") {
                None
            } else {
                Some(self.expr(0)?)
            };
            self.want(";")?;
            let step = if self.is(")") {
                None
            } else {
                Some(self.expr(0)?)
            };
            self.want(")")?;
            return Ok(Stmt::For(init, c, step, Box::new(self.stmt()?)));
        }
        if self.eat("return") {
            let e = if self.is(";") || self.is("}") || self.end() || self.newline() {
                None
            } else {
                Some(self.expr(0)?)
            };
            self.semi()?;
            return Ok(Stmt::Return(e));
        }
        if self.eat("break") {
            self.semi()?;
            return Ok(Stmt::Break);
        }
        if self.eat("continue") {
            self.semi()?;
            return Ok(Stmt::Continue);
        }
        if self.eat("throw") {
            if self.eat(";") {
                return Ok(Stmt::Rethrow);
            }
            let e = self.expr(0)?;
            self.semi()?;
            return Ok(Stmt::Throw(e));
        }
        if self.eat("try") {
            let b = Box::new(Stmt::Block(self.block()?));
            let c = if self.eat("catch") {
                self.want("(")?;
                let declare = self.eat("var");
                let n = if self.eat("...") {
                    if declare {
                        return Err(self.err("catch(var ...) is not a named binding"));
                    }
                    "...".into()
                } else {
                    self.name()?
                };
                self.want(")")?;
                Some(Catch {
                    name: n,
                    body: Box::new(Stmt::Block(self.block()?)),
                    declare,
                })
            } else {
                None
            };
            let f = if self.eat("finally") {
                Some(Box::new(self.stmt()?))
            } else {
                None
            };
            if c.is_none() && f.is_none() {
                return Err(self.err("try requires catch or finally"));
            }
            return Ok(Stmt::Try(b, c, f));
        }
        if self.eat("switch") {
            let e = self.paren()?;
            self.want("{")?;
            let mut cases = Vec::new();
            while !self.eat("}") {
                let c = if self.eat("case") {
                    Some(self.expr(2)?)
                } else {
                    self.want("default")?;
                    None
                };
                self.want(":")?;
                let mut body = Vec::new();
                while !self.is("case") && !self.is("default") && !self.is("}") && !self.end() {
                    body.push(self.stmt()?);
                }
                cases.push((c, body));
            }
            return Ok(Stmt::Switch(e, cases));
        }
        let e = self.expr(0)?;
        self.semi()?;
        Ok(Stmt::Expression(e))
    }
    fn expr(&mut self, min: u8) -> Result<Expr, Error> {
        self.depth += 1;
        if self.depth > 128 {
            return Err(self.err("expression nesting limit"));
        }
        let r = self.expression(min);
        self.depth -= 1;
        r
    }
    fn expression(&mut self, min: u8) -> Result<Expr, Error> {
        let t = self.tokens[self.at].kind.clone();
        self.at += 1;
        let mut lhs = match t {
            Kind::String(mut s) => {
                while let Kind::String(next) = self.tokens[self.at].kind.clone() {
                    s.push_str(&next);
                    self.at += 1;
                }
                Expr::String(s)
            }
            Kind::Number(s) => Expr::Number(if s.starts_with("0x") || s.starts_with("0X") {
                u64::from_str_radix(&s[2..], 16)
                    .map(|n| n as f64)
                    .map_err(|_| self.err("bad number"))?
            } else {
                s.parse().map_err(|_| self.err("bad number"))?
            }),
            Kind::Regex(pattern, flags) => Expr::Regex(pattern, flags),
            Kind::Word(s) if s == "function" => {
                let name = if self.is("(") {
                    None
                } else {
                    Some(self.name()?)
                };
                let (a, b) = self.params_body()?;
                Expr::Function(name, a, Rc::new(b))
            }
            Kind::Word(s) if s == "synchronized" => {
                self.want("function")?;
                let name = if self.is("(") {
                    None
                } else {
                    Some(self.name()?)
                };
                let (a, b) = self.params_body()?;
                Expr::Function(
                    name,
                    a,
                    Rc::new(vec![Stmt::Synchronized(Box::new(Stmt::Block(b)))]),
                )
            }
            Kind::Word(s) if s == "new" => {
                let mut ctor = Expr::Name(self.name()?);
                let mut parts = 0;
                while self.is(".") || self.is("[") {
                    parts += 1;
                    if parts > 128 {
                        return Err(self.err("constructor chain limit"));
                    }
                    let key = if self.eat(".") {
                        Expr::String(self.name()?)
                    } else {
                        self.want("[")?;
                        let e = self.expr(0)?;
                        self.want("]")?;
                        e
                    };
                    ctor = Expr::Member(Box::new(ctor), Box::new(key));
                }
                self.want("(")?;
                let mut args = Vec::new();
                if !self.eat(")") {
                    loop {
                        args.push(self.expr(2)?);
                        if self.eat(")") {
                            break;
                        }
                        self.want(",")?;
                    }
                }
                Expr::Unary(s, Box::new(Expr::Call(Box::new(ctor), args)))
            }
            Kind::Word(s) if matches!(s.as_str(), "typeof" | "delete" | "void") => {
                Expr::Unary(s, Box::new(self.expr(15)?))
            }
            Kind::Word(s) => Expr::Name(s),
            Kind::Sym(s) if matches!(s.as_str(), "!" | "~" | "+" | "-" | "++" | "--") => {
                Expr::Unary(s, Box::new(self.expr(15)?))
            }
            Kind::Sym(s) if s == "(" => {
                let e = self.expr(0)?;
                self.want(")")?;
                e
            }
            Kind::Sym(s) if s == "[" => {
                let mut v = Vec::new();
                while !self.eat("]") {
                    v.push(self.expr(2)?);
                    if self.eat("]") {
                        break;
                    }
                    self.want(",")?;
                }
                Expr::Array(v)
            }
            Kind::Sym(s) if s == "{" => {
                let mut v = Vec::new();
                while !self.eat("}") {
                    let key = match self.tokens[self.at].kind.clone() {
                        Kind::Word(s) | Kind::String(s) | Kind::Number(s) => s,
                        _ => return Err(self.err("object key")),
                    };
                    self.at += 1;
                    self.want(":")?;
                    v.push((key, self.expr(2)?));
                    if self.eat("}") {
                        break;
                    }
                    self.want(",")?;
                }
                Expr::Object(v)
            }
            _ => {
                self.at = self.at.saturating_sub(1);
                return Err(self.err("expected expression"));
            }
        };
        let mut chain = 0;
        loop {
            chain += 1;
            if chain > 128 {
                return Err(self.err("expression chain limit"));
            }
            if self.is("(") && 17 >= min {
                self.at += 1;
                let mut a = Vec::new();
                if !self.eat(")") {
                    loop {
                        a.push(self.expr(2)?);
                        if self.eat(")") {
                            break;
                        }
                        self.want(",")?;
                    }
                }
                lhs = Expr::Call(Box::new(lhs), a);
                continue;
            }
            if self.is(".") && 17 >= min {
                self.at += 1;
                let n = self.name()?;
                lhs = Expr::Member(Box::new(lhs), Box::new(Expr::String(n)));
                continue;
            }
            if self.is("[") && 17 >= min {
                self.at += 1;
                let n = self.expr(0)?;
                self.want("]")?;
                lhs = Expr::Member(Box::new(lhs), Box::new(n));
                continue;
            }
            if (self.is("++") || self.is("--")) && 16 >= min && !self.newline() {
                let op = if self.eat("++") {
                    "post++"
                } else {
                    self.at += 1;
                    "post--"
                };
                lhs = Expr::Unary(op.into(), Box::new(lhs));
                continue;
            }
            if self.is("?") && 3 >= min {
                self.at += 1;
                let a = self.expr(2)?;
                self.want(":")?;
                let b = self.expr(2)?;
                lhs = Expr::Conditional(Box::new(lhs), Box::new(a), Box::new(b));
                continue;
            }
            let op = match &self.tokens[self.at].kind {
                Kind::Sym(s) | Kind::Word(s) => s.clone(),
                _ => break,
            };
            let prec = match op.as_str() {
                "," => 1,
                "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "&=" | "|=" | "^=" | "<<=" | ">>="
                | ">>>=" => 2,
                "||" => 4,
                "&&" => 5,
                "|" => 6,
                "^" => 7,
                "&" => 8,
                "==" | "!=" | "===" | "!==" => 9,
                "<" | ">" | "<=" | ">=" | "instanceof" => 10,
                "in" if !self.no_in => 10,
                "<<" | ">>" | ">>>" => 11,
                "+" | "-" => 12,
                "*" | "/" | "%" => 13,
                _ => break,
            };
            if prec < min {
                break;
            }
            self.at += 1;
            let rhs = self.expr(if prec == 2 { prec } else { prec + 1 })?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }
}
// Native templates start a substitution after a space; trailing ASCII
// whitespace also terminates it (users.dbs groups/users end %help with CRLF).
// ` %expr ` or ` %(expression) `. The delimiter spaces are consumed.
// Confirmed against original /20a/: un-delimited %(this.border_color); is literal.
fn template(s: &str) -> Result<Vec<Part>, Error> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut p = 0;
    while p < s.len() {
        if s.as_bytes()[p] != b'%' || p == 0 || s.as_bytes()[p - 1] != b' ' {
            p += s[p..].chars().next().unwrap().len_utf8();
            continue;
        }
        let Some(c) = s[p + 1..].chars().next() else {
            break;
        };
        if !(ident(c) || c == '(') {
            p += 1;
            continue;
        }
        // Lex only the expression prefix: HTML beyond it may contain non-KS chars.
        let mut end = p + 1;
        let mut stack = Vec::new();
        let mut quote = None;
        let mut escape = false;
        for (i, ch) in s[p + 1..].char_indices() {
            if let Some(q) = quote {
                end = p + 1 + i + ch.len_utf8();
                if escape {
                    escape = false;
                } else if ch == '\\' && q == '"' {
                    escape = true;
                } else if ch == q {
                    quote = None;
                }
                continue;
            }
            if !stack.is_empty() && (ch == '\'' || ch == '"') {
                quote = Some(ch);
                end = p + 1 + i + 1;
                continue;
            }
            if ch == '(' || ch == '[' {
                stack.push(ch);
            } else if ch == ')' || ch == ']' {
                if stack.pop() != Some(if ch == ')' { '(' } else { '[' }) {
                    return Err(Error {
                        offset: p,
                        message: "unbalanced template expression".into(),
                    });
                }
            } else if stack.is_empty() && !(ident(ch) || ch.is_ascii_digit() || ch == '.') {
                break;
            }
            end = p + 1 + i + ch.len_utf8();
        }
        if !stack.is_empty() || quote.is_some() {
            return Err(Error {
                offset: p,
                message: "unterminated template expression".into(),
            });
        }
        if !s.as_bytes().get(end).is_some_and(u8::is_ascii_whitespace) {
            p = end;
            continue;
        }
        out.push(Part::Text(s[start..p - 1].into()));
        let mut parser = Parser::new(&s[p + 1..end]).map_err(|mut e| {
            e.offset += p + 1;
            e
        })?;
        let e = parser.expr(15).map_err(|mut e| {
            e.offset += p + 1;
            e
        })?;
        if !parser.end() {
            return Err(Error {
                offset: p,
                message: "template expression suffix".into(),
            });
        }
        out.push(Part::Value(e));
        p = end + 1;
        start = p;
    }
    out.push(Part::Text(s[start..].into()));
    Ok(out)
}
pub fn parse(s: &str) -> Result<Vec<Stmt>, Error> {
    if s.len() > 2 * 1024 * 1024 {
        return Err(Error {
            offset: 0,
            message: "source limit 2 MiB".into(),
        });
    }
    Parser::new(s)?.list("")
}
