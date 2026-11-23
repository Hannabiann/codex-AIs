//! KS views over stored report headings. Mutations affect the view only.
use super::*;
fn array(items: Vec<Value>) -> Value {
    let mut map = HashMap::new();
    map.insert("length".into(), Value::Number(items.len() as f64));
    for (i, v) in items.into_iter().enumerate() {
        map.insert(i.to_string(), v);
    }
    Value::Array(Rc::new(RefCell::new(map)))
}
fn object(map: HashMap<String, Value>) -> Value {
    Value::ReadOnlyObject(Rc::new(RefCell::new(map)))
}
pub(super) fn item(item: &crate::rich_text::Item, bytes: &[u8]) -> Result<Value, Fault> {
    use crate::rich_text::Item;
    let mut map = HashMap::new();
    match item {
        Item::Text {
            format,
            start,
            length,
            styles,
        } => {
            let input = bytes
                .get(*start..start + length)
                .ok_or("report text outside field")?;
            let mut plain = Vec::new();
            let mut flags = Vec::new();
            let mut stack = vec![0u32];
            let mut styles = styles.iter();
            for &b in input {
                match b {
                    0 => stack.push(styles.next().copied().unwrap_or(1)),
                    1 => {
                        if stack.len() > 1 {
                            stack.pop();
                        }
                    }
                    _ => {
                        plain.push(b);
                        flags.push(stack.iter().fold(0, |a, b| a | b));
                    }
                }
            }
            let mut runs = Vec::new();
            let mut at = 0;
            if flags.iter().any(|&f| f != 0) {
                while at < flags.len() {
                    let f = flags[at];
                    let mut end = at + 1;
                    while end < flags.len() && flags[end] == f {
                        end += 1;
                    }
                    let attrs = ["bold", "italic", "underline", "strikeout"]
                        .iter()
                        .enumerate()
                        .map(|(i, n)| (n.to_string(), Value::Bool(f & (1 << i) != 0)))
                        .collect();
                    runs.push(object(HashMap::from([
                        // The registry reader consumes each run with
                        // Stream.read(pos + count - 1). These spans are adjacent;
                        // pos is relative to the previous span, not the full text.
                        ("pos".into(), Value::Number(1.0)),
                        ("count".into(), Value::Number((end - at) as f64)),
                        ("attributes".into(), object(attrs)),
                    ])));
                    at = end;
                }
            }
            map.insert("Type".into(), Value::Number(1.0));
            map.insert("Format".into(), Value::Number(*format as f64));
            let stream = InputStream::value(&plain);
            if let Value::ReadStream(s) = &stream {
                s.borrow_mut().encoded_extra = input.len() - plain.len();
            }
            map.insert("Text".into(), stream);
            map.insert("\0fonts".into(), array(runs));
            map.insert(
                "GetFontAttrArray".into(),
                Value::Native("report.text.fonts".into()),
            );
        }
        Item::Table { rows, children } => {
            let mut out = Vec::new();
            let mut at = 0;
            for row in rows {
                let mut cells = Vec::new();
                for cell in &row.cells {
                    let child = children
                        .get(at..at + cell.count)
                        .ok_or("report table child range")?;
                    at += cell.count;
                    let value = array(
                        child
                            .iter()
                            .map(|v| item_value(v, bytes))
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                    if let Value::Array(a) = &value {
                        a.borrow_mut()
                            .insert("\0enumerate_numeric".into(), Value::Bool(true));
                        a.borrow_mut()
                            .insert("Width".into(), Value::Number(cell.width as f64));
                        for (bit, name) in
                            ["BorderTop", "BorderLeft", "BorderBottom", "BorderRight"]
                                .iter()
                                .enumerate()
                        {
                            a.borrow_mut().insert(
                                (*name).into(),
                                Value::Number(if cell.borders & (1 << bit) != 0 {
                                    1.0
                                } else {
                                    0.0
                                }),
                            );
                        }
                    }
                    cells.push(value);
                }
                let value = array(cells);
                if let Value::Array(a) = &value {
                    a.borrow_mut()
                        .insert("\0enumerate_numeric".into(), Value::Bool(true));
                    a.borrow_mut()
                        .insert("Gap".into(), Value::Number(row.padding as f64));
                    a.borrow_mut().insert("Margin".into(), Value::Number(row.margin as f64));
                }
                out.push(value);
            }
            if at != children.len() {
                return Err("report table has unassigned child objects".into());
            }
            map.insert("Type".into(), Value::Number(7.0));
            map.insert("Rows".into(), array(out));
        }
        Item::Picture { width, height, .. } => {
            map.insert("Type".into(), Value::Number(2.0));
            map.insert(
                "Image".into(),
                object(HashMap::from([
                    ("Width".into(), Value::Number(*width as f64)),
                    ("Height".into(), Value::Number(*height as f64)),
                ])),
            );
        }
    }
    Ok(object(map))
}
fn item_value(value: &crate::rich_text::Item, bytes: &[u8]) -> Result<Value, Fault> {
    item(value, bytes)
}
