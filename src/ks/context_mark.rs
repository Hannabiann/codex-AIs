//! Per-request highlighting of positive context terms only.
use regex::Regex;

#[derive(Clone, Debug)]
pub struct Pattern {
    tokens: Regex,
    terms: Vec<crate::db::text_query::Word>,
    forms: std::collections::BTreeSet<String>,
}
impl Pattern {
    pub fn is_match(&self, text: &str) -> bool {
        let text = text.to_uppercase().replace('Ё', "Е");
        self.terms.iter().any(|w| w.matches(&text)) || self.forms.contains(&text)
    }
}

pub fn pattern_at(text: &str, root: Option<&str>) -> Result<Option<Pattern>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    // Natural-language FREETEXT can contain punctuation that is not a
    // CONTAINS operator. Marking must not prevent opening a found document.
    let terms = match crate::db::text_query::parse(text) {
        Ok(expr) => expr.positive_words().into_iter().collect(),
        Err(_) => crate::db::free_text::terms(text)?
            .into_iter()
            .map(|text| crate::db::text_query::Word {
                text,
                prefix: false,
            })
            .collect::<Vec<_>>(),
    };
    if terms.is_empty() {
        return Ok(None);
    }
    let dictionary = crate::db::morphology::load(std::path::Path::new(root.unwrap_or(".")))?;
    let forms = terms
        .iter()
        .filter(|w| !w.prefix)
        .flat_map(|w| dictionary.forms(&w.text))
        .collect();
    Ok(Some(Pattern {
        forms,
        tokens: Regex::new(r"[\p{L}\p{N}]+").unwrap(),
        terms,
    }))
}

// Input is the renderer's generated HTML. Never search tags, URLs or entities.
pub fn highlight(html: &str, pattern: &Pattern) -> Result<(String, usize), String> {
    let mut out = String::with_capacity(html.len());
    let mut count = 0;
    let mut rest = html;
    while !rest.is_empty() {
        if rest.starts_with('<') || rest.starts_with('&') {
            let end = rest
                .find(if rest.starts_with('<') { '>' } else { ';' })
                .map_or(rest.len(), |p| p + 1);
            out.push_str(&rest[..end]);
            rest = &rest[end..];
            continue;
        }
        let end = rest.find(['<', '&']).unwrap_or(rest.len());
        let text = &rest[..end];
        let mut at = 0;
        for m in pattern
            .tokens
            .find_iter(text)
            .filter(|m| pattern.is_match(m.as_str()))
        {
            if count >= 10000 {
                return Err("context highlight limit".into());
            }
            out.push_str(&text[at..m.start()]);
            out.push_str(&format!("<A NAME=\"C{count}\"></A><MARK>"));
            out.push_str(m.as_str());
            out.push_str("</MARK>");
            at = m.end();
            count += 1;
        }
        out.push_str(&text[at..]);
        rest = &rest[end..];
    }
    Ok((out, count))
}
