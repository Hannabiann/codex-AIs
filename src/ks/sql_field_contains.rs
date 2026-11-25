//! CONTAINS on a string attribute searches that value, not the document word
//! index. Separators in a case number remain significant within a term.
#[derive(Clone, Debug)]
pub enum Pattern {
    Term(regex::Regex),
    All(Vec<Pattern>),
    Any(Vec<Pattern>),
    Never,
}
impl Pattern {
    pub fn matches(&self, text: &str) -> bool {
        match self {
            Self::Term(re) => re.is_match(text),
            Self::All(items) => items.iter().all(|p| p.matches(text)),
            Self::Any(items) => items.iter().any(|p| p.matches(text)),
            Self::Never => false,
        }
    }
}
fn term(s: &str) -> Result<Pattern, String> {
    if s.contains('!') && (!s.ends_with('!') || s[..s.len() - 1].contains('!')) {
        return Err(format!("CONTAINS syntax (727):{s}"));
    }
    let s = s.strip_suffix('!').unwrap_or(s);
    let edge = |c: char| c.is_alphanumeric() || c == '*';
    if !s.starts_with(edge) || !s.ends_with(edge) {
        return Ok(Pattern::Never);
    }
    let mask = regex::escape(s).replace(r"\*", r"[\p{L}\p{N}]*");
    Ok(Pattern::Term(
        regex::RegexBuilder::new(&format!(r"\b{mask}\b"))
            .case_insensitive(true)
            .build()
            .map_err(|e| e.to_string())?,
    ))
}
pub fn parse(text: &str) -> Result<Pattern, String> {
    const ERROR: &str = "CONTAINS syntax (722)";
    if text.len() > 8192 {
        return Err("CONTAINS field query size limit".into());
    }
    #[derive(Clone)]
    enum Token {
        Value(Pattern),
        And,
        Or,
        Open,
        Close,
    }
    let mut tokens = Vec::new();
    let mut input = text.chars().peekable();
    while let Some(ch) = input.next() {
        let token = match ch {
            c if c.is_whitespace() || c == ',' => continue,
            '&' => Token::And,
            '|' => Token::Or,
            '(' => Token::Open,
            ')' => Token::Close,
            '%' => return Err(ERROR.into()),
            '"' => {
                let mut phrase = String::new();
                let mut closed = false;
                for c in input.by_ref() {
                    if c == '"' {
                        closed = true;
                        break;
                    }
                    phrase.push(c);
                }
                if !closed {
                    return Err("CONTAINS syntax (720)".into());
                }
                Token::Value(term(&phrase)?)
            }
            c => {
                let mut word = c.to_string();
                while input
                    .peek()
                    .is_some_and(|c| !c.is_whitespace() && !"&|(),\"%".contains(*c))
                {
                    word.push(input.next().unwrap());
                }
                if word.starts_with('-') {
                    continue;
                }
                if matches!(word.to_uppercase().as_str(), "AND" | "И") {
                    Token::And
                } else {
                    Token::Value(term(&word)?)
                }
            }
        };
        tokens.push(token);
        if tokens.len() > 256 {
            return Err("CONTAINS field token limit".into());
        }
    }
    fn atom(tokens: &[Token], at: &mut usize, depth: usize) -> Result<Pattern, String> {
        if depth > 24 {
            return Err("CONTAINS field nesting limit".into());
        }
        let token = tokens.get(*at).ok_or(ERROR)?.clone();
        *at += 1;
        match token {
            Token::Value(p) => Ok(p),
            Token::Open => {
                let p = expression(tokens, at, depth + 1)?;
                if !matches!(tokens.get(*at), Some(Token::Close)) {
                    return Err("CONTAINS syntax (721)".into());
                }
                *at += 1;
                Ok(p)
            }
            _ => Err(ERROR.into()),
        }
    }
    fn conjunction(tokens: &[Token], at: &mut usize, depth: usize) -> Result<Pattern, String> {
        let mut values = vec![atom(tokens, at, depth)?];
        loop {
            match tokens.get(*at) {
                Some(Token::And) => *at += 1,
                Some(Token::Value(_) | Token::Open) => {}
                _ => break,
            }
            values.push(atom(tokens, at, depth)?);
        }
        Ok(Pattern::All(values))
    }
    fn expression(tokens: &[Token], at: &mut usize, depth: usize) -> Result<Pattern, String> {
        let mut values = vec![conjunction(tokens, at, depth)?];
        while matches!(tokens.get(*at), Some(Token::Or)) {
            *at += 1;
            values.push(conjunction(tokens, at, depth)?);
        }
        Ok(Pattern::Any(values))
    }
    if tokens.is_empty() {
        return Err("CONTAINS syntax (726)".into());
    }
    let mut at = 0;
    let pattern = expression(&tokens, &mut at, 0)?;
    if at != tokens.len() {
        return Err(ERROR.into());
    }
    Ok(pattern)
}
