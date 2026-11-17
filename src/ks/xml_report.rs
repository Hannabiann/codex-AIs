//! Portable XML stream adapters used by the original report libraries.
use super::*;
fn escaped(s: &str, attribute: bool) -> String {
    let s = s
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    if attribute {
        s.replace('"', "&quot;")
    } else {
        s
    }
}
impl Runtime<'_> {
    pub(super) fn report_xml_document(&mut self, values: &[Value]) -> Result<Value, Fault> {
        let source = match values.first() {
            None | Some(Value::Null) => String::new(),
            Some(Value::ReadStream(s)) => {
                let s = s.borrow();
                if s.closed {
                    return Err("XML stream closed".into());
                }
                let bytes = &s.data[s.position..];
                if bytes.starts_with(b"\xef\xbb\xbf")
                    || s.charset == "utf-8"
                    || bytes
                        .windows(5)
                        .take(128)
                        .any(|w| w.eq_ignore_ascii_case(b"utf-8"))
                {
                    String::from_utf8(bytes.to_vec()).map_err(|_| "invalid XML UTF-8")?
                } else {
                    crate::db::decode(bytes)
                }
            }
            Some(Value::String(s)) if s.trim_start().starts_with('<') => s.clone(),
            _ => return Err("XML report source must be XML text or an in-memory stream".into()),
        };
        if !source.is_empty() {
            xml_sax::events(&source)?;
        }
        Ok(Value::ReadOnlyObject(Rc::new(RefCell::new(HashMap::from(
            [
                ("xml".into(), Value::String(source)),
                ("\0typename".into(), Value::String("XMLDocument".into())),
            ],
        )))))
    }
    pub(super) fn report_xml_writer(&self, name: &str, values: Vec<Value>) -> Result<Value, Fault> {
        let mut o = HashMap::new();
        if name == "XMLBaseTagWriter" {
            if !(1..=2).contains(&values.len())
                || !matches!(values[0], Value::ReadStream(_) | Value::HttpOutput)
            {
                return Err("XML writer requires output stream".into());
            }
            o.insert("\0target".into(), values[0].clone());
            o.insert("\0stack".into(), Value::String(String::new()));
        } else if !values.is_empty() {
            return Err("SAXTagFilter expects no arguments".into());
        }
        for method in [
            "StartElement",
            "EndElement",
            "Characters",
            "setHandler",
            "StartDocument",
            "EndDocument",
        ] {
            o.insert(method.into(), Value::Native(format!("xml.report.{method}")));
        }
        Ok(Value::Object(Rc::new(RefCell::new(o))))
    }
    pub(super) fn report_xml_call(
        &mut self,
        method: &str,
        receiver: Option<Value>,
        values: Vec<Value>,
    ) -> Result<Value, Fault> {
        let Some(Value::Object(o)) = receiver else {
            return Err("XML writer receiver missing".into());
        };
        if method == "setHandler" {
            if values.len() != 1 {
                return Err("SAX setHandler expects writer".into());
            }
            o.borrow_mut().insert("\0handler".into(), values[0].clone());
            return Ok(Value::Null);
        }
        let handler = o.borrow().get("\0handler").cloned();
        if let Some(handler) = handler {
            let f = self.member(handler.clone(), method.into())?;
            return self.call_host(f, values, Some(handler), method.into());
        }
        let target = o
            .borrow()
            .get("\0target")
            .cloned()
            .ok_or("XML writer target missing")?;
        let mut stack = o
            .borrow()
            .get("\0stack")
            .map(Value::text)
            .unwrap_or_default();
        let text = match method {
            "StartDocument" => String::new(),
            "EndDocument" if stack.is_empty() => String::new(),
            "StartElement" => {
                let name = values.first().ok_or("XML element name missing")?.text();
                if name.is_empty()
                    || !name
                        .chars()
                        .all(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '-' | '.'))
                {
                    return Err("invalid XML element name".into());
                }
                let mut text = format!("<{name}");
                if let Some(Value::Object(attrs) | Value::ReadOnlyObject(attrs)) = values.get(1) {
                    let attrs = attrs.borrow();
                    let mut keys = attrs.keys().collect::<Vec<_>>();
                    keys.sort();
                    for key in keys {
                        if !key.starts_with('\0') {
                            text.push_str(&format!(
                                " {key}=\"{}\"",
                                escaped(&attrs[key].text(), true)
                            ));
                        }
                    }
                }
                text.push('>');
                stack.push_str(&name);
                stack.push('\n');
                text
            }
            "EndElement" => {
                let name = stack
                    .trim_end_matches('\n')
                    .rsplit('\n')
                    .next()
                    .filter(|s| !s.is_empty())
                    .ok_or("unbalanced XML EndElement")?
                    .to_string();
                if values
                    .first()
                    .is_some_and(|v| !matches!(v, Value::Null) && v.text() != name)
                {
                    return Err("mismatched XML EndElement".into());
                }
                stack.truncate(stack.len() - name.len() - 1);
                format!("</{name}>")
            }
            "Characters" => escaped(&values.first().map(Value::text).unwrap_or_default(), false),
            _ => return Err(format!("XML writer method {method} is not implemented").into()),
        };
        o.borrow_mut()
            .insert("\0stack".into(), Value::String(stack));
        match target {
            Value::ReadStream(s) => {
                let mut s = s.borrow_mut();
                if s.closed {
                    return Err("XML output stream closed".into());
                }
                let (bytes, _, bad) = encoding_rs::WINDOWS_1251.encode(&text);
                if bad {
                    return Err("XML output encoding".into());
                }
                if s.data.len() + bytes.len() > 16 * 1024 * 1024 {
                    return Err("XML output size limit".into());
                }
                s.data.extend_from_slice(&bytes);
            }
            Value::HttpOutput => self.write(&text)?,
            _ => return Err("XML writer target is not a stream".into()),
        }
        Ok(Value::Null)
    }
}

impl Runtime<'_> {
    pub(super) fn report_excel(&mut self, values: &[Value]) -> Result<Value, Fault> {
        if !(2..=3).contains(&values.len()) {
            return Err("Excel template expander expects template, XML[, output]".into());
        }
        let Value::ReadStream(template) = &values[0] else {
            return Err("XLS template must be an in-memory stream".into());
        };
        let Value::ReadStream(xml) = &values[1] else {
            return Err("XLS data must be an XML stream".into());
        };
        let source = self.report_xml_document(&values[1..2])?;
        let text = self.member(source, "xml".into())?.text();
        let mut paths = std::collections::BTreeMap::new();
        let mut stack = Vec::new();
        for event in xml_sax::events(&text)? {
            match event {
                xml_sax::Event::Start(name, _) => stack.push(name),
                xml_sax::Event::End(_) => {
                    stack.pop();
                }
                xml_sax::Event::Text(text) => {
                    if !stack.is_empty() {
                        paths
                            .entry(format!("/{}", stack.join("/")))
                            .or_insert_with(String::new)
                            .push_str(&text);
                    }
                }
            }
        }
        let bytes = {
            let input = template.borrow();
            if input.closed {
                return Err("XLS template stream closed".into());
            }
            xls_report::expand(&input.data[input.position..], &paths)?
        };
        let target = match values.get(2) {
            None | Some(Value::Null) => InputStream::value(&[]),
            Some(Value::ReadStream(s)) if !Rc::ptr_eq(s, template) && !Rc::ptr_eq(s, xml) => {
                Value::ReadStream(s.clone())
            }
            _ => return Err("XLS output must be a distinct in-memory stream".into()),
        };
        let Value::ReadStream(out) = &target else {
            unreachable!()
        };
        {
            let mut out = out.borrow_mut();
            if out.closed {
                return Err("XLS output stream closed".into());
            }
            if out.data.len() + bytes.len() > 16 * 1024 * 1024 {
                return Err("XLS output stream size limit".into());
            }
            out.data.extend_from_slice(&bytes);
        }
        {
            let mut s = template.borrow_mut();
            s.position = s.data.len();
        }
        {
            let mut s = xml.borrow_mut();
            s.position = s.data.len();
        }
        Ok(target)
    }
}
