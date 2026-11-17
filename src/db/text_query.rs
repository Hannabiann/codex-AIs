//! Bounded context expressions. Phrase verification uses the original field
//! text after the word index has reduced the candidate set.
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Word {
    pub text: String,
    pub prefix: bool,
}
impl Word {
    pub fn matches(&self, token: &str) -> bool {
        if self.prefix {
            token.starts_with(&self.text)
        } else {
            token == self.text
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Word(Word),
    Phrase(Vec<Word>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
}
#[derive(Clone, Debug)]
enum Token {
    Word(Word),
    Phrase(Vec<Word>),
    And,
    Or,
    Not,
    Open,
    Close,
}

pub fn words(text: &str) -> Vec<String> {
    // Inline formatting controls are zero-width, unlike paragraph boundaries.
    let text: String = text
        .chars()
        .filter(|c| !matches!(c, '\0' | '\x01'))
        .collect();
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_uppercase().replace('Ё', "Е"))
        .collect()
}
fn word(text: &str) -> Result<Word, String> {
    let text = text.to_uppercase().replace('Ё', "Е");
    let (text, prefix) = text
        .strip_suffix('*')
        .map_or((text.as_str(), false), |w| (w, true));
    let text = text.strip_suffix('!').unwrap_or(text);
    let (bytes, _, bad) = encoding_rs::WINDOWS_1251.encode(text);
    if text.is_empty() || !text.chars().all(char::is_alphanumeric) || bytes.len() > 32 || bad {
        return Err("invalid context word (1..32 CP1251 letters/digits, optional final *)".into());
    }
    Ok(Word {
        text: text.into(),
        prefix,
    })
}
pub fn parse(text: &str) -> Result<Expr, String> {
    if text.len() > 8192 {
        return Err("context query size limit".into());
    }
    let mut input = text.chars().peekable();
    let mut tokens = Vec::new();
    while let Some(c) = input.next() {
        if c.is_whitespace() {
            continue;
        }
        tokens.push(match c {
            '(' => Token::Open,
            ')' => Token::Close,
            '&' => Token::And,
            '|' => Token::Or,
            '~' => Token::Not,
            '"' => {
                let mut s = String::new();
                loop {
                    match input.next() {
                        Some('"') => break,
                        Some(c) => s.push(c),
                        None => return Err("context phrase closing quote missing".into()),
                    }
                }
                let phrase = s
                    .split(|c: char| !c.is_alphanumeric() && c != '*' && c != '!')
                    .filter(|s| !s.is_empty())
                    .map(word)
                    .collect::<Result<Vec<_>, _>>()?;
                if phrase.is_empty() {
                    return Err("empty context phrase".into());
                }
                Token::Phrase(phrase)
            }
            c if c.is_alphanumeric() => {
                let mut s = c.to_string();
                while input
                    .peek()
                    .is_some_and(|c| c.is_alphanumeric() || *c == '*' || *c == '!')
                {
                    s.push(input.next().unwrap());
                }
                match s.to_uppercase().as_str() {
                    "AND" | "И" => Token::And,
                    "OR" | "ИЛИ" => Token::Or,
                    "NOT" | "НЕ" => Token::Not,
                    _ => Token::Word(word(&s)?),
                }
            }
            _ => return Err(format!("unsupported context operator {c}")),
        });
        if tokens.len() > 128 {
            return Err("context query token limit".into());
        }
    }
    struct Parser {
        tokens: Vec<Token>,
        at: usize,
    }
    impl Parser {
        fn atom(&mut self, depth: usize) -> Result<Expr, String> {
            if depth > 24 {
                return Err("context query nesting limit".into());
            }
            let token = self
                .tokens
                .get(self.at)
                .ok_or("context operand missing")?
                .clone();
            self.at += 1;
            Ok(match token {
                Token::Word(w) => Expr::Word(w),
                Token::Phrase(w) => Expr::Phrase(w),
                Token::Not => Expr::Not(Box::new(self.atom(depth + 1)?)),
                Token::Open => {
                    let expr = self.expr(depth + 1)?;
                    if !matches!(self.tokens.get(self.at), Some(Token::Close)) {
                        return Err("context closing parenthesis missing".into());
                    }
                    self.at += 1;
                    expr
                }
                _ => return Err("context operand missing".into()),
            })
        }
        fn and(&mut self, depth: usize) -> Result<Expr, String> {
            let mut expr = self.atom(depth)?;
            loop {
                match self.tokens.get(self.at) {
                    Some(Token::And) => {
                        self.at += 1;
                    }
                    Some(Token::Word(_) | Token::Phrase(_) | Token::Open | Token::Not) => {}
                    _ => break,
                }
                expr = Expr::And(Box::new(expr), Box::new(self.atom(depth)?));
            }
            Ok(expr)
        }
        fn expr(&mut self, depth: usize) -> Result<Expr, String> {
            let mut expr = self.and(depth)?;
            while matches!(self.tokens.get(self.at), Some(Token::Or)) {
                self.at += 1;
                expr = Expr::Or(Box::new(expr), Box::new(self.and(depth)?));
            }
            Ok(expr)
        }
    }
    let mut p = Parser { tokens, at: 0 };
    let expr = p.expr(0)?;
    if p.at != p.tokens.len() {
        return Err("unexpected context query suffix".into());
    }
    Ok(expr)
}
impl Expr {
    pub fn candidates(
        &self,
        restriction: Option<&BTreeSet<u32>>,
        lookup: &mut impl FnMut(&Word, Option<&BTreeSet<u32>>) -> Result<BTreeSet<u32>, String>,
    ) -> Result<BTreeSet<u32>, String> {
        if restriction.is_some_and(BTreeSet::is_empty) {
            return Ok(BTreeSet::new());
        }
        match self {
            Self::Word(w) => lookup(w, restriction),
            Self::Phrase(ws) => {
                let mut words = ws.clone();
                words.sort_by_key(|w| std::cmp::Reverse(w.text.len()));
                let mut result = restriction.cloned();
                let mut pending = Vec::new();
                for w in &words {
                    match lookup(w, result.as_ref()) {
                        Ok(ids) => result = Some(ids),
                        Err(e) if e.contains("limit") => pending.push(w),
                        Err(e) => return Err(e),
                    }
                }
                for w in pending {
                    result = Some(lookup(w, result.as_ref())?);
                }
                result.ok_or_else(|| "context phrase has no words".into())
            }
            Self::And(a, b) => match a.candidates(restriction, lookup) {
                Ok(ids) => b.candidates(Some(&ids), lookup),
                Err(e) if e.contains("limit") || e.contains("positive condition") => {
                    let ids = b.candidates(restriction, lookup)?;
                    a.candidates(Some(&ids), lookup)
                }
                Err(e) => Err(e),
            },
            Self::Or(a, b) => {
                let mut ids = a.candidates(restriction, lookup)?;
                ids.extend(b.candidates(restriction, lookup)?);
                if ids.len() > crate::ks::sql::MAX_INDEX_CANDIDATES {
                    return Err("context candidate limit (8000000); narrow the search".into());
                }
                Ok(ids)
            }
            Self::Not(_) => {
                let universe = restriction
                    .ok_or("context NOT requires a positive condition or attribute restriction")?;
                // A phrase candidate is a superset. Subtracting it would lose
                // documents whose words occur in a different order. Verify
                // the negative expression against the fields below instead.
                Ok(universe.clone())
            }
        }
    }
    pub fn positive_words(&self) -> BTreeSet<Word> {
        match self {
            Self::Word(w) => BTreeSet::from([w.clone()]),
            Self::Phrase(w) => w.iter().cloned().collect(),
            Self::And(a, b) | Self::Or(a, b) => a
                .positive_words()
                .union(&b.positive_words())
                .cloned()
                .collect(),
            Self::Not(_) => BTreeSet::new(),
        }
    }
    pub fn matches(&self, fields: &[Vec<String>]) -> bool {
        match self {
            Self::Word(w) => fields.iter().flatten().any(|t| w.matches(t)),
            Self::Phrase(ws) => fields.iter().any(|f| {
                f.windows(ws.len())
                    .any(|ts| ws.iter().zip(ts).all(|(w, t)| w.matches(t)))
            }),
            Self::And(a, b) => a.matches(fields) && b.matches(fields),
            Self::Or(a, b) => a.matches(fields) || b.matches(fields),
            Self::Not(a) => !a.matches(fields),
        }
    }
}
