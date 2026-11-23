//! Detached documents used by the native registry generator. Locking these
//! containers never reserves an OID, and they cannot publish a DB4 write.
use super::*;

pub(super) fn object(fields: HashMap<String, Value>) -> Value {
    Value::Object(Rc::new(RefCell::new(fields)))
}
pub(super) fn map(value: &Value) -> Result<Object, Fault> {
    match value {
        Value::Object(m) | Value::ReadOnlyObject(m) | Value::Array(m) => Ok(m.clone()),
        _ => Err("Report object expected".into()),
    }
}
pub(super) fn number(m: &Object, key: &str, default: f64) -> Result<f64, Fault> {
    match m.borrow().get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => Ok(v.number()?),
    }
}
pub(super) fn collection(kind: &str, values: Vec<Value>) -> Value {
    let mut m = HashMap::from([
        ("\0report_collection".into(), Value::String(kind.into())),
        ("\0enumerate_numeric".into(), Value::Bool(true)),
        ("length".into(), Value::Number(values.len() as f64)),
    ]);
    m.extend(
        values
            .into_iter()
            .enumerate()
            .map(|(i, v)| (i.to_string(), v)),
    );
    object(m)
}
pub(super) fn elements(value: &Value) -> Result<Vec<Value>, Fault> {
    if let Value::RichField(f) = value {
        return Ok(f
            .borrow()
            .parts
            .iter()
            .cloned()
            .map(Value::Object)
            .collect());
    }
    let m = map(value)?;
    let m = m.borrow();
    let n = m
        .get("length")
        .map(Value::number)
        .transpose()?
        .unwrap_or(0.0);
    if !n.is_finite() || n < 0.0 || n.fract() != 0.0 || n > u32::MAX as f64 {
        return Err("Invalid report collection length".into());
    }
    (0..n as usize)
        .map(|i| {
            m.get(&i.to_string())
                .cloned()
                .ok_or_else(|| "Missing report element".into())
        })
        .collect()
}
fn append(target: &Value, value: Value) -> Result<(), Fault> {
    if let Value::RichField(f) = target {
        f.borrow_mut().parts.push(map(&value)?);
    } else {
        let m = map(target)?;
        let n = number(&m, "length", 0.0)? as usize;
        let mut m = m.borrow_mut();
        m.insert(n.to_string(), value);
        m.insert("length".into(), Value::Number(n as f64 + 1.0));
    }
    Ok(())
}
fn text_object(ordered: bool) -> Value {
    object(HashMap::from([
        ("Type".into(), Value::Number(1.0)),
        ("Format".into(), Value::Number(0x3000 as f64)),
        (
            "Text".into(),
            if ordered {
                InputStream::value(&[])
            } else {
                Value::RichStream(Rc::new(RefCell::new(String::new())))
            },
        ),
        (
            "SetFontAttributes".into(),
            Value::Native("report.text.set_fonts".into()),
        ),
    ]))
}
pub(super) fn text_value(value: &Value) -> Result<String, Fault> {
    Ok(match value {
        Value::RichStream(s) | Value::TextStream(s) => s.borrow().clone(),
        Value::ReadStream(s) => crate::db::decode(&s.borrow().data),
        _ => return Err("Report text stream expected".into()),
    })
}
fn clone_part(value: &Value, depth: usize) -> Result<Value, Fault> {
    if depth > 32 {
        return Err("Report nesting limit".into());
    }
    let source = map(value)?;
    let source = source.borrow();
    match source
        .get("Type")
        .ok_or("Report part type missing")?
        .number()? as u32
    {
        1 => {
            let out = text_object(true);
            let m = map(&out)?;
            let mut m = m.borrow_mut();
            m.insert(
                "Format".into(),
                source.get("Format").cloned().unwrap_or(Value::Number(0.0)),
            );
            let stream = source.get("Text").ok_or("Report text missing")?;
            let text = match stream {
                Value::ReadStream(s) => {
                    let s = s.borrow();
                    let value = InputStream::value(&s.data);
                    if let Value::ReadStream(copy) = &value {
                        copy.borrow_mut().encoded_extra = s.encoded_extra;
                    }
                    value
                }
                _ => Value::RichStream(Rc::new(RefCell::new(text_value(stream)?))),
            };
            m.insert("Text".into(), text);
            if let Some(fonts) = source.get("\0fonts") {
                let mut start = 0.0;
                let mut ranges = Vec::new();
                for run in elements(fonts)? {
                    let run = map(&run)?;
                    let count = number(&run, "count", 0.0)?;
                    let attrs = map(run
                        .borrow()
                        .get("attributes")
                        .ok_or("Report font attributes missing")?)?;
                    let mut attrs = attrs.borrow().clone();
                    attrs.insert("start".into(), Value::Number(start));
                    attrs.insert("count".into(), Value::Number(count));
                    ranges.push(object(attrs));
                    start += count;
                }
                let Value::Object(ranges) = collection("fonts", ranges) else {
                    unreachable!()
                };
                m.insert("\0font_ranges".into(), Value::Array(ranges));
            }
            if let Some(ranges) = source.get("\0font_ranges") {
                m.insert("\0font_ranges".into(), ranges.clone());
            }
            drop(m);
            Ok(out)
        }
        7 => {
            let mut rows = Vec::new();
            for row in elements(source.get("Rows").ok_or("Report rows missing")?)? {
                let mut cells = Vec::new();
                for cell in elements(&row)? {
                    let parts = elements(&cell)?
                        .iter()
                        .map(|p| clone_part(p, depth + 1))
                        .collect::<Result<Vec<_>, _>>()?;
                    let copied = collection("parts", parts);
                    let source_cell = map(&cell)?;
                    let dest = map(&copied)?;
                    for key in [
                        "Width",
                        "BorderTop",
                        "BorderLeft",
                        "BorderBottom",
                        "BorderRight",
                    ] {
                        if let Some(v) = source_cell.borrow().get(key) {
                            dest.borrow_mut().insert(key.into(), v.clone());
                        }
                    }
                    cells.push(copied);
                }
                let copied = collection("cells", cells);
                let original = map(&row)?;
                for key in ["Gap", "Margin"] {
                    if let Some(v) = original.borrow().get(key) {
                        map(&copied)?.borrow_mut().insert(key.into(), v.clone());
                    }
                }
                rows.push(copied);
            }
            Ok(object(HashMap::from([
                ("Type".into(), Value::Number(7.0)),
                ("Rows".into(), collection("rows", rows)),
                (
                    "Format".into(),
                    source.get("Format").cloned().unwrap_or(Value::Number(0.0)),
                ),
            ])))
        }
        kind => Err(format!("Report insertion type {kind} is not implemented").into()),
    }
}
impl Runtime<'_> {
    pub(super) fn new_report_document(&self) -> Result<Value, Fault> {
        Ok(Value::TempDocument(Rc::new(RefCell::new(TempDocument {
            properties: Rc::new(RefCell::new(HashMap::from([
                ("\0report".into(), Value::Bool(true)),
                ("\0fields".into(), object(HashMap::new())),
                ("Base".into(), self.database_for(0)?),
                ("Area".into(), Value::Number(0.0)),
                ("NumDoc".into(), Value::Number(0.0)),
            ]))),
            fields: HashMap::new(),
        }))))
    }
    pub(super) fn report_document_call(
        &self,
        doc: Rc<RefCell<TempDocument>>,
        method: &str,
        args: &[Value],
    ) -> Result<Value, Fault> {
        let props = doc.borrow().properties.clone();
        if matches!(method, "Lock" | "Unlock") && args.is_empty() {
            props
                .borrow_mut()
                .insert("Locked".into(), Value::Bool(method == "Lock"));
            return Ok(Value::Bool(true));
        }
        if !matches!(method, "NewField" | "Attr") || args.is_empty() || args.len() > 2 {
            return Err(
                "Detached report documents cannot be saved; expected Attr or NewField".into(),
            );
        }
        let p = props.borrow();
        let structure = self.member(p["Base"].clone(), "Struct".into())?;
        let area = self.member(structure, p["Area"].text())?;
        let definition = self.member(area, args[0].text())?;
        let key = self.member(definition.clone(), "Attr".into())?.text();
        let kind = self.member(definition, "Type".into())?.number()? as u16;
        let fields = map(&p["\0fields"])?;
        if method == "Attr" {
            return Ok(fields.borrow().get(&key).cloned().unwrap_or(Value::Null));
        }
        let value = if kind == 4 {
            if args.get(1).is_some_and(|v| !matches!(v, Value::Null)) {
                return Err("Report text field requires InsertText".into());
            }
            Value::RichField(Rc::new(RefCell::new(form_doc::RichText {
                report: true,
                ..Default::default()
            })))
        } else {
            object(HashMap::from([(
                "Value".into(),
                args.get(1).cloned().unwrap_or(Value::Null),
            )]))
        };
        fields.borrow_mut().insert(key, value.clone());
        Ok(value)
    }
    pub(super) fn report_collection_call(
        &self,
        method: &str,
        target: Value,
        args: &[Value],
    ) -> Result<Value, Fault> {
        match method {
            "InsertText" | "InsertTable" | "Insert" => {
                let part = match (method, args) {
                    ("InsertText", []) => text_object(match &target {
                        Value::RichField(f) => f.borrow().report,
                        _ => true,
                    }),
                    ("InsertTable", []) => object(HashMap::from([
                        ("Type".into(), Value::Number(7.0)),
                        ("Rows".into(), collection("rows", vec![])),
                    ])),
                    ("Insert", [source]) => clone_part(source, 0)?,
                    _ => return Err("Invalid report insertion arguments".into()),
                };
                append(&target, part.clone())?;
                Ok(part)
            }
            "TextLength" if args.is_empty() => {
                fn length(v: &Value, depth: usize) -> Result<usize, Fault> {
                    if depth > 32 {
                        return Err("Report nesting limit".into());
                    }
                    let mut n = 0;
                    for part in elements(v)? {
                        let m = map(&part)?;
                        let m = m.borrow();
                        if let Some(t) = m.get("Text") {
                            n += text_value(t)?.len();
                        }
                        if let Some(rows) = m.get("Rows") {
                            for row in elements(rows)? {
                                for cell in elements(&row)? {
                                    n += length(&cell, depth + 1)?;
                                }
                            }
                        }
                    }
                    Ok(n)
                }
                Ok(Value::Number(length(&target, 0)? as f64))
            }
            "Add" | "Remove" => {
                let m = map(&target)?;
                let len = number(&m, "length", 0.0)? as usize;
                let index = |value: &Value| -> Result<usize, Fault> {
                    let n = value.number()?;
                    if !n.is_finite() || n < 0.0 || n.fract() != 0.0 || n > u32::MAX as f64 {
                        return Err("Invalid report row index".into());
                    }
                    Ok(n as usize)
                };
                let result = if method == "Remove" {
                    let [i] = args else {
                        return Err("Rows.Remove expects an index".into());
                    };
                    let i = index(i)?;
                    if i >= len {
                        return Err("Report row outside table".into());
                    }
                    let mut m = m.borrow_mut();
                    for at in i..len - 1 {
                        let value = m
                            .remove(&(at + 1).to_string())
                            .ok_or("Missing report row")?;
                        m.insert(at.to_string(), value);
                    }
                    m.remove(&(len - 1).to_string());
                    m.insert("length".into(), Value::Number((len - 1) as f64));
                    Value::Null
                } else {
                    if args.is_empty() || args.len() > 2 {
                        return Err("Rows.Add expects cells[, index]".into());
                    }
                    let n = index(&args[0])?;
                    if n > 4096 {
                        return Err("Report column count limit".into());
                    }
                    let pos = args.get(1).map(index).transpose()?.unwrap_or(len);
                    if pos > len {
                        return Err("Report insertion outside table".into());
                    }
                    let row = collection(
                        "cells",
                        (0..n).map(|_| collection("parts", vec![])).collect(),
                    );
                    let mut m = m.borrow_mut();
                    for at in (pos..len).rev() {
                        let value = m.remove(&at.to_string()).ok_or("Missing report row")?;
                        m.insert((at + 1).to_string(), value);
                    }
                    m.insert(pos.to_string(), row.clone());
                    m.insert("length".into(), Value::Number((len + 1) as f64));
                    row
                };
                Ok(result)
            }
            _ => Err("Unsupported report collection method".into()),
        }
    }
}
