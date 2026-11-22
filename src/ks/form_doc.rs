//! Form definitions are read from area 1002 in the active database. Nothing here
//! embeds court-specific record IDs, field names or document contents.
use super::{Fault, Object, Runtime, Value};
use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::OnceLock};
pub(super) fn is_empty_html(html: &str) -> bool {
    static CONTENT: OnceLock<regex::Regex> = OnceLock::new();
    static TAGS: OnceLock<regex::Regex> = OnceLock::new();
    if CONTENT
        .get_or_init(|| regex::Regex::new(r"(?i)<(?:img|li|table)\b").unwrap())
        .is_match(html)
    {
        return false;
    }
    TAGS.get_or_init(|| regex::Regex::new(r"(?s)<[^>]*>").unwrap())
        .replace_all(html, "")
        .replace("&nbsp;", " ")
        .trim()
        .is_empty()
}
#[derive(Debug, Clone)]
pub(super) struct Tab {
    pub oid: u32,
    pub scope: u32,
    pub attr: u16,
    pub kind: String,
    pub name: String,
    pub source: String,
}
#[derive(Debug, Default)]
pub(super) struct RichText {
    pub parts: Vec<Object>,
}
#[derive(Debug)]
enum Part {
    Text(String),
    Field(String),
    Optional(Vec<Part>),
    List(u16),
    AutoList(u16, u16, Option<u16>),
    ScriptList(String, Option<u16>),
    Group(u16),
}

fn template(source: &str, sql_annotation: bool) -> Result<Vec<Part>, Fault> {
    fn whitespace(s: &str, at: &mut usize) {
        while *at < s.len() && s[*at..].chars().next().unwrap().is_whitespace() {
            *at += s[*at..].chars().next().unwrap().len_utf8();
        }
    }
    fn field(s: &str, at: &mut usize) -> Result<String, Fault> {
        whitespace(s, at);
        if !s[*at..].starts_with('%') {
            return Err("Form command requires %attribute".into());
        }
        *at += 1;
        let name = s[*at..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect::<String>();
        *at += name.len();
        if s[*at..].starts_with('(') {
            *at += 1;
            while *at < s.len() {
                let c = s[*at..].chars().next().unwrap();
                if matches!(c, '\n' | '%' | '<' | '>') {
                    break;
                }
                *at += c.len_utf8();
                if c == ')' {
                    while s[*at..].starts_with(')') {
                        *at += 1;
                    }
                    break;
                }
            }
        }
        Ok(name)
    }
    fn parts(
        s: &str,
        at: &mut usize,
        depth: usize,
        sql_annotation: bool,
    ) -> Result<Vec<Part>, Fault> {
        if depth > 32 {
            return Err("Form template nesting limit".into());
        }
        let mut out = Vec::new();
        while *at < s.len() {
            let tail = &s[*at..];
            let c = tail.chars().next().unwrap();
            match c {
                '>' => {
                    *at += 1;
                    if depth > 0 {
                        break;
                    }
                }
                '<' => {
                    *at += 1;
                    out.push(Part::Optional(parts(s, at, depth + 1, sql_annotation)?));
                }
                '%' => out.push(Part::Field(field(s, at)?)),
                '\\' => {
                    *at += 1;
                    let command = s[*at..]
                        .chars()
                        .take_while(|c| c.is_ascii_alphabetic())
                        .collect::<String>();
                    *at += command.len();
                    match command.as_str() {
                        "" => {
                            // SQL ANNOTATION consumes a bare control marker;
                            // FormDoc.abstract and document cards preserve it.
                            if s[*at..].starts_with('\\') {
                                *at += 1;
                                out.push(Part::Text("\\".into()));
                            } else if !sql_annotation || !s[*at..].starts_with(char::is_whitespace)
                            {
                                out.push(Part::Text("\\".into()));
                            }
                        }
                        "list" | "group" => {
                            let n = field(s, at)?
                                .parse()
                                .map_err(|_| "Form command attribute invalid")?;
                            out.push(if command == "list" {
                                Part::List(n)
                            } else {
                                Part::Group(n)
                            });
                        }
                        "listscript" => {
                            whitespace(s, at);
                            let name = s[*at..]
                                .chars()
                                .take_while(|c| c.is_alphanumeric() || *c == '_')
                                .collect::<String>();
                            *at += name.len();
                            whitespace(s, at);
                            let sort = if s[*at..].starts_with("\\sort") {
                                *at += 5;
                                Some(
                                    field(s, at)?
                                        .parse()
                                        .map_err(|_| "listscript sort invalid")?,
                                )
                            } else {
                                None
                            };
                            out.push(Part::ScriptList(name, sort));
                        }
                        "listauto" => {
                            let area =
                                field(s, at)?.parse().map_err(|_| "listauto area invalid")?;
                            let attr = field(s, at)?
                                .parse()
                                .map_err(|_| "listauto attribute invalid")?;
                            whitespace(s, at);
                            let mut sort = None;
                            if s[*at..].starts_with("\\sort") {
                                *at += 5;
                                sort = Some(
                                    field(s, at)?.parse().map_err(|_| "listauto sort invalid")?,
                                );
                            }
                            out.push(Part::AutoList(area, attr, sort));
                        }
                        _ => {
                            return Err(
                                format!("Form template command not implemented: {command}").into()
                            )
                        }
                    }
                }
                _ => {
                    let text = tail
                        .chars()
                        .take_while(|c| !matches!(c, '<' | '>' | '%' | '\\'))
                        .collect::<String>();
                    *at += text.len();
                    out.push(Part::Text(text));
                }
            }
            if out.len() > 16384 {
                return Err("Form template item limit".into());
            }
        }
        Ok(out)
    }
    parts(source, &mut 0, 0, sql_annotation)
}
fn object(values: impl IntoIterator<Item = (String, Value)>) -> Value {
    Value::Object(Rc::new(RefCell::new(values.into_iter().collect())))
}
fn array(values: Vec<Value>) -> Value {
    let n = values.len();
    let mut m: HashMap<_, _> = values
        .into_iter()
        .enumerate()
        .map(|(i, v)| (i.to_string(), v))
        .collect();
    m.insert("length".into(), Value::Number(n as f64));
    Value::Array(Rc::new(RefCell::new(m)))
}
impl Runtime<'_> {
    pub(super) fn format_object(&self, kind: &str, values: Vec<Value>) -> Result<Value, Fault> {
        let methods: &[&str] = match kind {
            "KodeksFormatParser" => &["parse", "setHandler", "setErrorHandler"],
            "KodeksDOMParser" => &["parse", "setHandler", "setErrorHandler"],
            "KodeksFormatRTFWriter" => &[],
            "KodeksFormatHTMLWriter" => &[
                "setHyperlinkConverter",
                "setPictureConverter",
                "setListConverter",
                "setContentsConverter",
                "setAnchorConverter",
                "setContextDecor",
                "setTitle",
            ],
            "node" => &["createRange"],
            "range" => &["selectNode"],
            "situations" => &["add", "pushElement", "addAttribute"],
            "KodeksTextSections" => &["searchForSectionByElement"],
            "attributes" => &["getNamedItem"],
            _ => return Err("Unknown format object".into()),
        };
        let mut fields = HashMap::from([("\0kind".into(), Value::String(kind.into()))]);
        if kind == "KodeksFormatRTFWriter" {
            if !matches!(values.as_slice(), [Value::HttpOutput]) {
                return Err("RTF writer requires HTTP Output()".into());
            }
            fields.insert(
                "documentArea".into(),
                object([("propertiesWords".into(), Value::String(String::new()))]),
            );
        }
        for method in methods {
            fields.insert((*method).into(), Value::Native(format!("format.{method}")));
        }
        if let Some(value) = values.first() {
            fields.insert("\0field".into(), value.clone());
        }
        if kind == "node" {
            fields.insert(
                "attributes".into(),
                self.format_object("attributes", values.clone())?,
            );
        }
        if kind == "KodeksTextSections" {
            fields.insert(
                "startSituations".into(),
                self.format_object("situations", vec![])?,
            );
            fields.insert(
                "finishSituations".into(),
                self.format_object("situations", vec![])?,
            );
            // Full-field conversion is bounded by the document field limit. No
            // synthetic chapter boundaries are inferred from plain line breaks.
            fields.insert("borders".into(), Value::Null);
        }
        Ok(object(fields))
    }
    pub(super) fn format_call(
        &mut self,
        name: &str,
        receiver: Option<Value>,
        values: Vec<Value>,
    ) -> Result<Value, Fault> {
        let Some(Value::Object(o)) = receiver else {
            return Err("format receiver missing".into());
        };
        let kind = o
            .borrow()
            .get("\0kind")
            .map(Value::text)
            .unwrap_or_default();
        let name = name.trim_start_matches("format.");
        match name {
            "searchForSectionByElement" if kind == "KodeksTextSections" => {
                if values.len() != 1 {
                    return Err("section search expects an element name".into());
                }
                // The renderer emits the full field (borders == null), so no
                // chapter index exists. The original KS selects full-text mode.
                Ok(Value::Null)
            }
            "getNamedItem" if kind == "attributes" => {
                if values.len() != 1 {
                    return Err("getNamedItem expects a name".into());
                }
                if values[0].text() != "context-count" {
                    return Ok(Value::Null);
                }
                let field = o
                    .borrow()
                    .get("\0field")
                    .cloned()
                    .ok_or("node field missing")?;
                let count = self.rich_html(&field)?.matches("<A NAME=\"C").count();
                Ok(object([("value".into(), Value::Number(count as f64))]))
            }
            "setContextDecor" if kind == "KodeksFormatHTMLWriter" => {
                if values.len() != 6 {
                    return Err("setContextDecor expects six arguments".into());
                }
                o.borrow_mut()
                    .insert("\0contextDecor".into(), array(values));
                Ok(Value::Null)
            }
            "parse" if kind == "KodeksFormatParser" => {
                if values.len() != 1 {
                    return Err("format parse expects field".into());
                }
                let handler = o.borrow().get("\0setHandler").cloned();
                // Preserve the node-returning form without an explicit handler.
                if let Some(Value::Object(writer)) = handler {
                    if writer.borrow().get("\0kind").map(Value::text).as_deref()
                        == Some("KodeksFormatRTFWriter")
                    {
                        let page = match writer.borrow().get("documentArea") {
                            Some(Value::Object(o)) => o
                                .borrow()
                                .get("propertiesWords")
                                .map(Value::text)
                                .unwrap_or_default(),
                            _ => String::new(),
                        };
                        let field = match &values[0] {
                            Value::StoredField(f) => f.clone(),
                            Value::WritableField(f) => f.snapshot()?,
                            _ => return Err("RTF export requires a stored text field".into()),
                        };
                        let bytes = crate::rich_text::rtf(field.data(), &page)?;
                        self.binary_output = Some(bytes);
                        self.output.clear();
                        return self.format_object("node", values);
                    }
                    let html = self.rich_html(&values[0])?;
                    let target = writer.borrow().get("\0field").cloned();
                    if let Some(stream) = target {
                        let print = self.member(stream.clone(), "Print".into())?;
                        self.call_host(
                            print,
                            vec![Value::String(html)],
                            Some(stream),
                            "Print".into(),
                        )?;
                    } else {
                        self.write(&html)?;
                    }
                }
                self.format_object("node", values)
            }
            "createRange" => {
                let field = o
                    .borrow()
                    .get("\0field")
                    .cloned()
                    .ok_or("node field missing")?;
                self.format_object("range", vec![field])
            }
            "selectNode" => {
                let Some(Value::Object(node)) = values.first() else {
                    return Err("selectNode expects node".into());
                };
                let field = node
                    .borrow()
                    .get("\0field")
                    .cloned()
                    .ok_or("node field missing")?;
                o.borrow_mut().insert("\0field".into(), field);
                Ok(Value::Null)
            }
            "parse" if kind == "KodeksDOMParser" => {
                let Some(Value::Object(range)) = values.first() else {
                    return Err("DOM parse expects range".into());
                };
                let field = range
                    .borrow()
                    .get("\0field")
                    .cloned()
                    .ok_or("range field missing")?;
                let html = self.rich_html(&field)?;
                self.write(&html)?;
                Ok(Value::Null)
            }
            "add" | "pushElement" | "addAttribute" => {
                let list = self.format_object("situations", values)?;
                let key = o.borrow().len().to_string();
                if o.borrow().len() > 1024 {
                    return Err("section rule limit".into());
                }
                o.borrow_mut().insert(key, list.clone());
                Ok(list)
            }
            name if name.starts_with("set") => {
                if values.is_empty() || values.len() > 2 {
                    return Err("format setter arguments".into());
                }
                o.borrow_mut()
                    .insert(format!("\0{name}"), values[0].clone());
                Ok(Value::Null)
            }
            _ => Err(format!("{kind}.{name} is not implemented").into()),
        }
    }
    pub(super) fn make_form(&mut self, args: Vec<Value>) -> Result<Value, Fault> {
        self.try_make_form(args)?
            .ok_or_else(|| "FormDoc: default form missing".into())
    }
    fn try_make_form(&mut self, args: Vec<Value>) -> Result<Option<Value>, Fault> {
        let Some(Value::Document(oid, scope)) = args.first().cloned() else {
            return Err("FormDoc expects a document".into());
        };
        let form_started = std::time::Instant::now();
        let record = self.read_document(oid, scope)?;
        let form_oid = if let Some(Value::Document(id, _)) = args.get(2) {
            *id
        } else if let Some(id) = record.fields.iter().find_map(|f| {
            if f.id == 1100 && f.wire_type == 5 {
                if let crate::db::fields::Value::Integer(id) = f.value {
                    return Some(id);
                }
            }
            None
        }) {
            id
        } else {
            let host = self
                .dbhost
                .as_ref()
                .ok_or("FormDoc requires a database")?
                .borrow();
            let ids = host.query(
                scope,
                &format!(
                    "SELECT OID FROM \"Формы\" WHERE \"Область\"={}",
                    65536 + u32::from(record.area)
                ),
            )?;
            let mut live = Vec::new();
            for id in ids {
                if let Some(r) = host.read(scope, id)? {
                    if r.area==1002&&r.fields.iter().any(|f|f.id==3&&matches!(f.value,crate::db::fields::Value::Integer(a) if a==65536+u32::from(record.area))){live.push(id);}
                }
            }
            live.sort_unstable();
            live.dedup();
            if live.is_empty() {
                return Ok(None);
            }
            if live.len() != 1 {
                return Err(format!(
                    "FormDoc: expected one default form for area {}, got {}",
                    record.area,
                    live.len()
                )
                .into());
            }
            live[0]
        };
        let form_selected = std::time::Instant::now();
        let definition = self.read_document(form_oid, scope)?;
        let form_loaded = std::time::Instant::now();
        let structure = definition
            .fields
            .iter()
            .find(|f| f.id == 4 && f.wire_type == 4)
            .ok_or("FormDoc structure missing")?;
        let structure = crate::db::decode(crate::document::bytes(structure)?);
        if let Some(script) = definition
            .fields
            .iter()
            .find(|f| f.id == 7 && f.wire_type == 4)
        {
            self.execute_code(
                &crate::db::decode(crate::document::bytes(script)?),
                Some(Value::Document(oid, scope)),
            )?;
        }
        let form_executed = std::time::Instant::now();
        static HOLDERS: OnceLock<regex::Regex> = OnceLock::new();
        let pattern = HOLDERS.get_or_init(|| {
            regex::Regex::new(r"\\(text|autotext|scripttext|form|script)\s+").unwrap()
        });
        let matches = pattern.captures_iter(&structure).collect::<Vec<_>>();
        let mut tabs = Vec::new();
        for (i, c) in matches.iter().enumerate() {
            let kind = &c[1];
            if matches!(kind, "script" | "form") {
                continue;
            }
            let start = c.get(0).unwrap().end();
            let end = matches
                .get(i + 1)
                .map(|m| m.get(0).unwrap().start())
                .unwrap_or(structure.len());
            let section =
                structure[start..end].trim_matches(|c| matches!(c, '\r' | '\n' | '<' | '>' | ' '));
            let (line, body) = section.split_once('\n').unwrap_or((section, ""));
            let (attr, name, source) = if kind == "text" {
                let (number, name) = line
                    .trim()
                    .split_once(char::is_whitespace)
                    .ok_or("text holder requires attribute and name")?;
                (
                    number
                        .parse::<u16>()
                        .map_err(|_| "invalid holder attribute")?,
                    name.trim().to_string(),
                    String::new(),
                )
            } else if kind == "scripttext" {
                let (function, name) = line
                    .trim()
                    .split_once(char::is_whitespace)
                    .unwrap_or((line.trim(), ""));
                if function.is_empty() {
                    return Err("scripttext requires function".into());
                }
                (
                    32768 + tabs.len() as u16,
                    name.trim().to_string(),
                    function.to_string(),
                )
            } else {
                let split = line.find(['%', '\\', '<']).unwrap_or(line.len());
                (
                    32768 + tabs.len() as u16,
                    line[..split].trim().to_string(),
                    format!("{}\n{body}", &line[split..]),
                )
            };
            let tab = Rc::new(Tab {
                oid,
                scope,
                attr,
                kind: kind.into(),
                name,
                source,
            });
            self.form_tabs
                .borrow_mut()
                .insert((scope, oid, attr), tab.clone());
            tabs.push(Value::FormTab(tab));
        }
        if self.profile_functions {
            for (name, elapsed) in [
                (
                    "[FormDoc select]",
                    form_selected.duration_since(form_started),
                ),
                ("[FormDoc load]", form_loaded.duration_since(form_selected)),
                (
                    "[FormDoc execute]",
                    form_executed.duration_since(form_loaded),
                ),
                ("[FormDoc tabs]", form_executed.elapsed()),
            ] {
                let entry = self.function_times.entry(name.into()).or_default();
                entry.0 += 1;
                entry.1 += elapsed.as_micros();
                entry.2 += elapsed.as_micros();
            }
        }
        if tabs.len() > 128 {
            return Err("FormDoc holder limit".into());
        }
        if self.profile_functions {
            let elapsed = form_started.elapsed().as_micros();
            let entry = self
                .function_times
                .entry("[FormDoc construction]".into())
                .or_default();
            entry.0 += 1;
            entry.1 += elapsed;
            entry.2 += elapsed;
        }
        Ok(Some(object([
            ("doc".into(), Value::Document(oid, scope)),
            (
                "abstract".into(),
                Value::DocumentMethod(oid, scope, "\0abstract".into()),
            ),
            ("\0definition".into(), Value::Number(form_oid as f64)),
            ("holder".into(), array(tabs)),
            ("reserved".into(), object([])),
            ("\0type".into(), Value::String("HashFormDoc".into())),
        ])))
    }
    pub(super) fn doc_object(&self, oid: u32, scope: u32) -> Object {
        self.document_vars
            .borrow_mut()
            .entry((scope, oid))
            .or_insert_with(|| Rc::new(RefCell::new(HashMap::new())))
            .clone()
    }
    pub(super) fn form_function(
        &mut self,
        oid: u32,
        scope: u32,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Value, Fault> {
        let object = self.doc_object(oid, scope);
        let function = object
            .borrow()
            .get(name)
            .cloned()
            .unwrap_or(self.get(name)?);
        let Value::Function(f) = function else {
            return Err(format!("Form function {name} unavailable").into());
        };
        for env in &f.env {
            if env.borrow().contains_key("_is_html") {
                env.borrow_mut()
                    .insert("_is_html".into(), Value::Bool(false));
            }
        }
        let result = self.invoke(f, args, Some(Value::Document(oid, scope)), name.into());
        if std::env::var_os("KODEKS_TRACE").is_some_and(|v| v == "1") {
            if let Err(e) = &result {
                eprintln!("Form function {name} document {oid}: {e:?}");
            }
        }
        result
    }
    pub(super) fn tab_field(&mut self, tab: &Tab) -> Result<Value, Fault> {
        if tab.kind == "text" {
            return self.document_attr(
                tab.oid,
                tab.scope,
                &[
                    Value::Number(tab.attr as f64),
                    Value::Bool(false),
                    Value::Number(4.0),
                ],
            );
        }
        let key = (tab.scope, tab.oid, tab.attr);
        if let Some(v) = self.form_fields.borrow().get(&key) {
            return Ok(v.clone());
        }
        let field = Rc::new(RefCell::new(RichText::default()));
        if tab.kind == "scripttext" {
            self.form_function(
                tab.oid,
                tab.scope,
                &tab.source,
                vec![Value::RichField(field.clone())],
            )?;
        } else {
            let html = self.expand_form(tab.oid, tab.scope, &tab.source, 0)?;
            field
                .borrow_mut()
                .parts
                .push(Rc::new(RefCell::new(HashMap::from([(
                    "\0html".into(),
                    Value::String(html),
                )]))));
        }
        let value = Value::RichField(field);
        self.form_fields.borrow_mut().insert(key, value.clone());
        Ok(value)
    }
    pub(super) fn rich_html(&mut self, field: &Value) -> Result<String, Fault> {
        match field {
            Value::RichField(text) => {
                let parts = text.borrow().parts.clone();
                let mut out = String::new();
                for part in parts {
                    let m = part.borrow().clone();
                    if let Some(Value::String(html)) = m.get("\0html") {
                        out.push_str(html);
                    } else if let Some(Value::List(list)) = m.get("List") {
                        out.push_str(&self.list_html(&list.borrow())?);
                    } else if let Some(Value::TextStream(text) | Value::RichStream(text)) =
                        m.get("Text")
                    {
                        let value = super::report_cells::formatted_text(
                            &text.borrow(),
                            m.get("\0font_ranges"),
                        )?;
                        let tag = match m.get("Format").and_then(|v| v.number().ok()).unwrap_or(0.0)
                            as u32
                        {
                            0x4002 => "H3",
                            0x4000 => "B",
                            _ => "P",
                        };
                        out.push_str(&format!("<{tag}>{value}</{tag}>"));
                    }
                }
                if let Some((_, pattern)) =
                    self.rich_context_marks.get(&(Rc::as_ptr(text) as usize))
                {
                    return Ok(super::context_mark::highlight(&out, pattern)?.0);
                }
                Ok(out)
            }
            Value::StoredField(f) => {
                let bytes = crate::document::bytes(f.data())?;
                if crate::document::has_rtf_header(bytes) {
                    return Err("Literal RTF requires a renderer".into());
                }
                let html = crate::rich_text::html(f.data(), |i| {
                    format!(
                        "{}?_picture&nd={}&f={}&p={i}",
                        self.path.trim_end_matches('/'),
                        f.oid,
                        f.data().id
                    )
                })?;
                if let Some(pattern) = self.context_marks.get(&(f.scope, f.oid, f.index)) {
                    return Ok(super::context_mark::highlight(&html, pattern)?.0);
                }
                Ok(html)
            }
            Value::Null => Ok(String::new()),
            _ => Err("Expected a text field".into()),
        }
    }
    pub(super) fn annotation_html(&mut self, oid: u32, scope: u32) -> Result<String, Fault> {
        self.annotation_html_options(oid, scope, true)
    }
    pub(super) fn abstract_html(&mut self, oid: u32, scope: u32) -> Result<String, Fault> {
        let html = self.annotation_html_options(oid, scope, true)?;
        // FormDoc.abstract is a formatted field, not the clickable SQL/list
        // annotation. Native HTMLWriter emits linked field titles as text.
        static LINKS: OnceLock<regex::Regex> = OnceLock::new();
        Ok(LINKS
            .get_or_init(|| regex::Regex::new(r"(?is)</?a\b[^>]*>").unwrap())
            .replace_all(&html, "")
            .into_owned())
    }
    pub(super) fn sql_annotation_html(&mut self, oid: u32, scope: u32) -> Result<String, Fault> {
        self.annotation_html_options(oid, scope, false)
    }
    fn annotation_html_options(
        &mut self,
        oid: u32,
        scope: u32,
        omit_empty: bool,
    ) -> Result<String, Fault> {
        if self.include_depth >= 24 {
            return Err("List annotation nesting limit".into());
        }
        self.include_depth += 1;
        let outer_annotation_depth = self.form_annotation_depth;
        self.form_annotation_depth = if omit_empty {
            outer_annotation_depth + 1
        } else {
            0
        };
        // Nested ANNOTATION must not overwrite the enclosing form function's
        // output mode (showLetters calls annotation functions through SQL).
        let html_modes = self
            .scopes
            .iter()
            .filter_map(|env| {
                let value = env.borrow().get("_is_html").cloned()?;
                Some((env.clone(), value))
            })
            .collect::<Vec<_>>();
        let result = (|| {
            let Some(form) = self.try_make_form(vec![Value::Document(oid, scope)])? else {
                let r = self.read_document(oid, scope)?;
                return Ok(r
                    .string(1)
                    .map(crate::db::decode)
                    .map(|name| crate::monitor::escape(&name))
                    .unwrap_or_default());
            };
            let definition = self.member(form, "\0definition".into())?.number()? as u32;
            let record = self.read_document(definition, scope)?;
            if let Some(f) = record.fields.iter().find(|f| f.id == 5 && f.wire_type == 4) {
                let source = crate::db::decode(crate::document::bytes(f)?);
                return self
                    .render_parts(oid, scope, &template(&source, !omit_empty)?, 0, None)
                    .map(|p| p.0);
            }
            Ok(String::new())
        })();
        for (env, value) in html_modes {
            env.borrow_mut().insert("_is_html".into(), value);
        }
        self.form_annotation_depth = outer_annotation_depth;
        self.include_depth -= 1;
        result
    }
    fn list_html(&mut self, list: &super::record_list::RecordList) -> Result<String, Fault> {
        let ids = self.list_ids(list)?;
        if ids.is_empty() {
            return Ok(String::new());
        }
        let count = ids.len();
        let noun = if (11..=14).contains(&(count % 100)) {
            "объектов"
        } else {
            match count % 10 {
                1 => "объект",
                2..=4 => "объекта",
                _ => "объектов",
            }
        };
        let mut out = format!("<B>{count} {noun}</B><UL>");
        for oid in ids {
            let annotation = self.annotation_html(oid, list.scope)?;
            let record = self.read_document(oid, list.scope)?;
            let title = record
                .string(1)
                .map(crate::db::decode)
                .unwrap_or_else(|| oid.to_string());
            static LINKS: OnceLock<regex::Regex> = OnceLock::new();
            let no_links = LINKS
                .get_or_init(|| regex::Regex::new(r"(?is)</?a\b[^>]*>").unwrap())
                .replace_all(&annotation, "")
                .into_owned();
            let (heading, body) = if no_links.trim().is_empty() {
                (crate::monitor::escape(&title), String::new())
            } else {
                let (head, body) = no_links.split_once("<BR>").unwrap_or((&no_links, ""));
                (head.to_owned(), body.to_owned())
            };
            out.push_str(&format!(
                "<LI><A HREF=\"{}?d&amp;nd={oid}\" TARGET=\"_top\">{heading}</A><BR>{body}</LI>",
                crate::monitor::escape(self.path.trim_end_matches('/')),
            ));
        }
        out.push_str("</UL>");
        Ok(out)
    }
    fn field_html_at(
        &self,
        oid: u32,
        scope: u32,
        attr: u16,
        occurrence: Option<usize>,
    ) -> Result<(String, bool), Fault> {
        let record = self.read_document(oid, scope)?;
        let mut result = Vec::new();
        if attr == 32766 {
            return Ok((oid.to_string(), true));
        }
        for (index, f) in record.fields.iter().filter(|f| f.id == attr).enumerate() {
            if occurrence.is_some_and(|n| n != index) {
                continue;
            }
            use crate::db::fields::Value as F;
            let html = match &f.value {
                F::String(b) => crate::monitor::escape(&crate::db::decode(b)),
                F::Date(d) => format!(
                    "{:02}.{:02}.{:04}",
                    d[3],
                    d[2],
                    i16::from_le_bytes([d[0], d[1]])
                ),
                F::Timestamp(b) => format!(
                    "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:02}",
                    u16::from_be_bytes([b[0], b[1]]),
                    b[2],
                    b[3],
                    b[4],
                    b[5],
                    b[6],
                    b[7]
                ),
                F::Integer(link) => {
                    let linked = match self.read_document(*link, scope) {
                        Ok(r) => r,
                        Err(Fault::Script(_)) => continue,
                        Err(e) => return Err(e),
                    };
                    let title = linked
                        .string(1)
                        .map(crate::db::decode)
                        .unwrap_or_else(|| link.to_string());
                    format!(
                        "<A HREF=\"{}?d&amp;nd={link}\" TARGET=\"_top\">{}</A>",
                        crate::monitor::escape(self.path.trim_end_matches('/')),
                        crate::monitor::escape(&title)
                    )
                }
                _ => {
                    return Err(format!(
                        "Form field {} type {} is not implemented",
                        f.id, f.wire_type
                    )
                    .into())
                }
            };
            // Native cards show present empty fields, annotations omit them.
            if !html.is_empty() || self.form_annotation_depth == 0 {
                result.push(html);
            }
        }
        let present = !result.is_empty();
        Ok((result.join(", "), present))
    }
    fn expand_form(
        &mut self,
        oid: u32,
        scope: u32,
        source: &str,
        depth: usize,
    ) -> Result<String, Fault> {
        self.render_parts(oid, scope, &template(source, false)?, depth, None)
            .map(|p| p.0)
    }
    fn render_parts(
        &mut self,
        oid: u32,
        scope: u32,
        parts: &[Part],
        depth: usize,
        occurrence: Option<usize>,
    ) -> Result<(String, bool), Fault> {
        if depth > 32 {
            return Err("Form rendering nesting limit".into());
        }
        let mut out = String::new();
        let mut present = false;
        for (index, part) in parts.iter().enumerate() {
            let (html, has_value) = match part {
                Part::Text(s) => (crate::rich_text::fragment(s), false),
                Part::Field(name) => {
                    if let Ok(attr) = name.parse::<u16>() {
                        self.field_html_at(oid, scope, attr, occurrence)?
                    } else {
                        let lookup = self
                            .doc_object(oid, scope)
                            .borrow()
                            .get(name)
                            .cloned()
                            .unwrap_or(self.get(name)?);
                        let value = if matches!(lookup, Value::Function(_)) {
                            // Native form substitutions leave a failed script field empty.
                            // A missing case link in FilesToDoc must not hide the card;
                            // execution limits and other runtime faults still propagate.
                            match self.form_function(oid, scope, name, vec![]) {
                                Ok(v) => v,
                                Err(Fault::Script(_)) => Value::Null,
                                Err(e) => return Err(e),
                            }
                        } else {
                            lookup.clone()
                        };
                        let text = if matches!(value, Value::Null) {
                            String::new()
                        } else {
                            value.text()
                        };

                        let html = match &lookup {
                            Value::Function(f) => f
                                .env
                                .iter()
                                .rev()
                                .find_map(|o| o.borrow().get("_is_html").map(Value::truth))
                                .unwrap_or(false),
                            _ => false,
                        };
                        (
                            if html {
                                text.clone()
                            } else {
                                crate::rich_text::fragment(&text)
                            },
                            !text.is_empty(),
                        )
                    }
                }
                Part::Optional(children) => {
                    let (html, has) =
                        self.render_parts(oid, scope, children, depth + 1, occurrence)?;
                    (if has { html } else { String::new() }, has)
                }
                Part::List(attr) => {
                    let record = self.read_document(oid, scope)?;
                    let ids = record
                        .fields
                        .iter()
                        .filter(|f| f.id == *attr && f.wire_type == 5)
                        .filter_map(|f| {
                            if let crate::db::fields::Value::Integer(id) = f.value {
                                Some(id)
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>();
                    let has = !ids.is_empty();
                    let list = super::record_list::RecordList {
                        scope,
                        area: 0,
                        order: None,
                        conditions: vec![],
                        query_all: false,
                        ids: RefCell::new(Some(ids)),
                    };
                    (self.list_html(&list)?, has)
                }
                Part::AutoList(area_id, attr, sort) => {
                    let ids = {
                        let host = self
                            .dbhost
                            .as_ref()
                            .ok_or("listauto requires database")?
                            .borrow();
                        let schema = host.schema(scope)?;
                        let area = schema.areas.get(area_id).ok_or("listauto area absent")?;
                        let field = area.fields.get(attr).ok_or("listauto field absent")?;
                        let quote = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
                        let mut sql = format!(
                            "SELECT OID FROM {} WHERE {}={oid}",
                            quote(&area.name),
                            quote(&field.name)
                        );
                        if let Some(attr) = sort {
                            let sort = area.fields.get(attr).ok_or("listauto sort field absent")?;
                            sql.push_str(&format!(" ORDER BY {}", quote(&sort.name)));
                        }
                        host.query(scope, &sql)?
                    };
                    let has = !ids.is_empty();
                    let list = super::record_list::RecordList {
                        scope,
                        area: *area_id,
                        order: None,
                        conditions: vec![],
                        query_all: false,
                        ids: RefCell::new(Some(ids)),
                    };
                    (self.list_html(&list)?, has)
                }
                Part::ScriptList(name, sort) => {
                    let value = self.form_function(oid, scope, name, vec![])?;
                    match value {
                        Value::List(l) => {
                            if let Some(attr) = sort {
                                self.list_sort(&mut l.borrow_mut(), *attr)?;
                            }
                            let html = self.list_html(&l.borrow())?;
                            let has = !html.is_empty();
                            (html, has)
                        }
                        Value::Null => (String::new(), false),
                        _ => return Err("listscript must return List".into()),
                    }
                }
                Part::Group(attr) => {
                    let count = self
                        .read_document(oid, scope)?
                        .fields
                        .iter()
                        .filter(|f| f.id == *attr)
                        .count();
                    for n in 0..count {
                        let (html, has) = self.field_html_at(oid, scope, *attr, Some(n))?;
                        out.push_str(&html);
                        present |= has;
                        let (html, has) =
                            self.render_parts(oid, scope, &parts[index + 1..], depth + 1, Some(n))?;
                        out.push_str(&html);
                        present |= has;
                        if n + 1 < count {
                            out.push_str("<BR>\n");
                        }
                    }
                    return Ok((out, present));
                }
            };
            out.push_str(&html);
            present |= has_value;
            if out.len() > 4 * 1024 * 1024 {
                return Err("Form output limit".into());
            }
        }
        Ok((out, present))
    }
}
