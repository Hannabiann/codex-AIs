//! Bounded XML SAX adapter for the original database's FindSetting document.
//! No DTD, external entities, file access or network resolution.
use super::*;
#[derive(Debug, PartialEq)]
pub(crate) enum Event {
    Start(String, Vec<(String, String)>),
    End(String),
    Text(String),
}
fn decode(s: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p + 1..];
        let end = rest.find(';').ok_or("XML unterminated entity")?;
        let e = &rest[..end];
        out.push(match e {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let n = if let Some(v) = e.strip_prefix("#x") {
                    u32::from_str_radix(v, 16).ok()
                } else {
                    e.strip_prefix('#').and_then(|v| v.parse().ok())
                };
                n.and_then(char::from_u32)
                    .filter(|c| *c == '\n' || *c == '\r' || *c == '\t' || *c >= ' ')
                    .ok_or("XML unknown/invalid entity")?
            }
        });
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}
fn name(rest: &mut &str) -> Result<String, String> {
    let end = rest
        .find(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | ':' | '.')))
        .unwrap_or(rest.len());
    if end == 0 {
        return Err("XML name expected".into());
    }
    let n = rest[..end].to_string();
    *rest = &rest[end..];
    Ok(n)
}
pub(crate) fn events(source: &str) -> Result<Vec<Event>, String> {
    if source.len() > 4 * 1024 * 1024 {
        return Err("XML size limit".into());
    }
    let mut rest = source.trim_start_matches('\u{feff}');
    let mut stack = Vec::new();
    let mut result = Vec::new();
    let mut roots = 0;
    while !rest.is_empty() {
        if result.len() > 100000 {
            return Err("XML event limit".into());
        }
        if let Some(r) = rest.strip_prefix("<!--") {
            let n = r.find("-->").ok_or("XML unclosed comment")?;
            rest = &r[n + 3..];
            continue;
        }
        if let Some(r) = rest.strip_prefix("<?") {
            let n = r.find("?>").ok_or("XML unclosed declaration")?;
            rest = &r[n + 2..];
            continue;
        }
        if let Some(r) = rest.strip_prefix("<![CDATA[") {
            let n = r.find("]]>").ok_or("XML unclosed CDATA")?;
            if stack.is_empty() {
                return Err("XML CDATA outside root".into());
            }
            result.push(Event::Text(r[..n].into()));
            rest = &r[n + 3..];
            continue;
        }
        if rest.starts_with("<!") {
            return Err("XML DTD/declarations are not supported".into());
        }
        if let Some(r) = rest.strip_prefix("</") {
            rest = r;
            let n = name(&mut rest)?;
            rest = rest.trim_start();
            rest = rest.strip_prefix('>').ok_or("XML closing tag")?;
            if stack.pop().as_ref() != Some(&n) {
                return Err("XML mismatched closing tag".into());
            }
            result.push(Event::End(n));
            continue;
        }
        if let Some(r) = rest.strip_prefix('<') {
            rest = r;
            let n = name(&mut rest)?;
            let mut attrs = Vec::new();
            loop {
                let before = rest;
                rest = rest.trim_start();
                if rest.starts_with('>') || rest.starts_with("/>") {
                    break;
                }
                if before.len() == rest.len() {
                    return Err("XML attribute separator expected".into());
                }
                let key = name(&mut rest)?;
                rest = rest.trim_start();
                rest = rest
                    .strip_prefix('=')
                    .ok_or("XML attribute equals expected")?
                    .trim_start();
                let q = rest.chars().next().ok_or("XML attribute missing")?;
                if q != '\'' && q != '"' {
                    return Err("XML quoted attribute expected".into());
                }
                rest = &rest[1..];
                let end = rest.find(q).ok_or("XML unclosed attribute")?;
                let val = decode(&rest[..end])?;
                rest = &rest[end + 1..];
                if attrs.len() >= 256 || attrs.iter().any(|(k, _)| k == &key) {
                    return Err("XML duplicate/too many attributes".into());
                }
                attrs.push((key, val));
            }
            if stack.is_empty() {
                roots += 1;
                if roots > 1 {
                    return Err("XML multiple roots".into());
                }
            }
            result.push(Event::Start(n.clone(), attrs));
            if let Some(r) = rest.strip_prefix("/>") {
                rest = r;
                result.push(Event::End(n));
            } else {
                rest = &rest[1..];
                stack.push(n);
                if stack.len() > 128 {
                    return Err("XML depth limit".into());
                }
            }
        } else {
            let end = rest.find('<').unwrap_or(rest.len());
            let text = decode(&rest[..end])?;
            rest = &rest[end..];
            if stack.is_empty() {
                if !text.trim().is_empty() {
                    return Err("XML text outside root".into());
                }
            } else {
                result.push(Event::Text(text));
            }
        }
    }
    if !stack.is_empty() || roots != 1 {
        return Err("XML incomplete document".into());
    }
    Ok(result)
}
impl Runtime<'_> {
    pub(super) fn xml_parse(
        &mut self,
        receiver: Option<Value>,
        values: &[Value],
    ) -> Result<Value, Fault> {
        if !values.is_empty() {
            return Err("XML.Parse expects no arguments".into());
        }
        let Some(Value::Object(parser)) = receiver else {
            return Err("XML parser receiver missing".into());
        };
        let stream = parser.borrow().get("\0stream").cloned();
        let Some(Value::ReadStream(stream)) = stream else {
            return Err("XML stream missing".into());
        };
        let source = {
            let s = stream.borrow();
            if s.closed {
                return Err("XML stream closed".into());
            }
            if s.charset == "utf-8" {
                String::from_utf8(s.data[s.position..].to_vec()).map_err(|_| "invalid XML UTF-8")?
            } else {
                crate::db::decode(&s.data[s.position..])
            }
        };
        let events = events(&source).map_err(|e| Fault::Script(Value::String(e)))?;
        for event in events {
            self.tick()?;
            let (method, args) = match event {
                Event::Start(n, attrs) => (
                    "StartElement",
                    vec![
                        Value::String(n),
                        Value::Object(Rc::new(RefCell::new(
                            attrs
                                .into_iter()
                                .map(|(k, v)| (k, Value::String(v)))
                                .collect(),
                        ))),
                    ],
                ),
                Event::End(n) => ("EndElement", vec![Value::String(n)]),
                Event::Text(s) => ("Characters", vec![Value::String(s)]),
            };
            let callback = parser.borrow().get(method).cloned();
            if let Some(Value::Function(f)) = callback {
                self.invoke(
                    f,
                    args,
                    Some(Value::Object(parser.clone())),
                    format!("XML.{method}"),
                )?;
            }
        }
        let mut s = stream.borrow_mut();
        s.position = s.data.len();
        Ok(Value::Null)
    }
}
