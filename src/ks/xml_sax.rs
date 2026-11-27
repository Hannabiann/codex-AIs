//! Bounded XML SAX adapter for the original database's FindSetting document.
//! No DTD, external entities, file access or network resolution.
use super::*;
#[derive(Debug, PartialEq)]
pub(crate) enum Event {
    Start(String, Vec<(String, String)>),
    End(String),
    Text(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sax_constructor_accepts_native_encoding_and_decodes_cyrillic() {
        let mut runtime = Runtime::new("/test");
        for encoding in ["windows-1251"] {
            let source = format!("var s=new VirtualText(); s.CharSet='{encoding}'; s.Print('<root name=\"Суд\"/>'); s.reset(); var p=new XMLSAXparser(s, '{encoding}'); p.StartElement=function(n,a) {{ if(n!='root' || a.name!='Суд') throw 'bad XML text'; this.seen=true; }}; p.Parse(); if(!p.seen) throw 'missing callback';");
            let ast = runtime.parse_cached_named(&source, "sax-constructor-test").unwrap();
            runtime.render(&ast).unwrap();
        }
        let stream = InputStream::value("<root name=\"Суд\"/>".as_bytes());
        let parser = runtime.xml_sax_parser(&[stream.clone(), Value::String("utf-8".into())]).unwrap();
        runtime.xml_parse(Some(parser), &[]).unwrap();
        let Value::ReadStream(stream) = stream else { unreachable!() };
        assert_eq!(stream.borrow().charset, "utf-8");
        assert_eq!(stream.borrow().position, stream.borrow().data.len());
        let stream = InputStream::value(b"<root/>");
        assert!(runtime.xml_sax_parser(&[stream.clone(),Value::Null]).is_ok());
        assert!(runtime.xml_sax_parser(&[stream.clone()]).is_ok());
        assert!(runtime.xml_sax_parser(&[stream, Value::String("unknown".into())]).is_err());
    }
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
    events_bounded(source, 4 * 1024 * 1024)
}
pub(crate) fn events_bounded(source: &str, bytes: usize) -> Result<Vec<Event>, String> {
    if source.len() > bytes {
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
    pub(super) fn xml_sax_parser(&self, values: &[Value]) -> Result<Value, Fault> {
        if !(1..=2).contains(&values.len()) {
            return Err("XMLSAXparser expects stream[, encoding]".into());
        }
        let input = self.xml_input_stream(&values[0])?;
        let Value::ReadStream(stream) = &input else {unreachable!()};
        if stream.borrow().closed {
            return Err("XML stream closed".into());
        }
        if let Some(value) = values.get(1).filter(|v|!v.text().is_empty()) {
            let charset = value.text().to_ascii_lowercase();
            if !matches!(charset.as_str(), "windows-1251" | "utf-8") {
                return Err("unsupported XML charset".into());
            }
            stream.borrow_mut().charset = charset;
        } else {
            let mut input=stream.borrow_mut();
            if input.data.starts_with(b"\xef\xbb\xbf") || input.data.windows(5).take(128).any(|b|b.eq_ignore_ascii_case(b"utf-8")) {input.charset="utf-8".into();}
        }
        Ok(Value::Object(Rc::new(RefCell::new(HashMap::from([
            ("\0stream".into(), input),
            ("Parse".into(), Value::Native("xml.Parse".into())),
            ("SetEncoding".into(), Value::Native("xml.SetEncoding".into())),
        ])))))
    }

    pub(super) fn xml_input_stream(&self,value:&Value)->Result<Value,Fault>{
        use std::io::Read;
        match value {
            Value::ReadStream(_)=>Ok(value.clone()),
            Value::File(file)=>{
                if file.stream.borrow().is_none(){
                    let meta=std::fs::metadata(&file.path).map_err(|e|e.to_string())?;
                    if !meta.is_file()||meta.len()>64*1024*1024{return Err("XML input file size/type limit".into());}
                    let Value::ReadStream(stream)=InputStream::value(&std::fs::read(&file.path).map_err(|e|e.to_string())?) else{unreachable!()};
                    stream.borrow_mut().charset=file.charset.borrow().clone();
                    *file.stream.borrow_mut()=Some(stream);
                }
                Ok(Value::ReadStream(file.stream.borrow().as_ref().unwrap().clone()))
            },
            Value::LogFile(file)=>{
                let mut handle=file.file.borrow_mut();let handle=handle.as_mut().ok_or("XML input file closed")?;
                let mut bytes=Vec::new();handle.take(64*1024*1024+1).read_to_end(&mut bytes).map_err(|e|e.to_string())?;
                if bytes.len()>64*1024*1024{return Err("XML input file size limit".into());}
                let result=InputStream::value(&bytes);
                if let Value::ReadStream(s)=&result{s.borrow_mut().charset=file.charset.borrow().clone();}
                Ok(result)
            },
            _=>Err("XMLSAXparser requires a stream or an open file".into()),
        }
    }

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
        let events = events_bounded(&source, self.exchange_xml_limit()).map_err(|e| Fault::Script(Value::String(e)))?;
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
