//! Transient formatted cells for native registry callbacks in HTML output.
//! Stored documents remain read-only; these objects exist for one cell only.
use super::*;

impl Runtime<'_> {
    pub(super) fn registry_is_filter_only(&self, owner: &Value) -> Result<bool, Fault> {
        let Value::Document(oid, scope) = self.member(owner.clone(), "dRegistry".into())? else {
            return Ok(false);
        };
        // These vendor leaf records exist only to expose date controls in the
        // parent form. Their generation query ignores the date and selects the
        // whole document area, although they have no heading, columns or child
        // reports. Check the record shape so later edited definitions still run.
        if !matches!(oid, 705000905 | 705000963) {
            return Ok(false);
        }
        let record = self.read_document(oid, scope)?;
        if record.area != 6350 || record.fields.iter().any(|f| matches!(f.id, 10 | 12 | 13)) {
            return Ok(false);
        }
        for field in record.fields.iter().filter(|f| matches!(f.id, 4 | 9)) {
            if field.wire_type == 4 {
                if !crate::document::bytes(field)?.is_empty() {
                    return Ok(false);
                }
            } else {
                return Ok(false);
            }
        }
        Ok(true)
    }
    pub(super) fn registry_cell_target(
        &self,
        values: &mut Vec<Value>,
    ) -> Result<Option<Value>, Fault> {
        if values.get(2).is_some_and(|v| !matches!(v, Value::Null)) {
            return Ok(None);
        }
        let mode = self.get("g_mode")?;
        if matches!(mode, Value::Null) || !self.member(mode, "isDirectHTML".into())?.truth() {
            return Ok(None);
        }
        let Some(cell) = values.first() else {
            return Ok(None);
        };
        if matches!(
            self.member(cell.clone(), "getFunctionValue".into())?,
            Value::Null
        ) {
            return Ok(None);
        }
        let field = Value::RichField(Rc::new(RefCell::new(form_doc::RichText::default())));
        values.resize(3, Value::Null);
        values[2] = field.clone();
        Ok(Some(field))
    }

    pub(super) fn report_set_font_attributes(
        &self,
        receiver: Option<Value>,
        values: &[Value],
    ) -> Result<Value, Fault> {
        let Some(Value::Object(object)) = receiver else {
            return Err("text font receiver missing".into());
        };
        if !(2..=3).contains(&values.len()) {
            return Err("SetFontAttributes expects attributes, start[, count]".into());
        }
        let Value::Object(attributes) = &values[0] else {
            return Err("font attributes expected".into());
        };
        let length = match object.borrow().get("Text") {
            Some(Value::TextStream(text) | Value::RichStream(text)) => {
                text.borrow().chars().count()
            }
            _ => return Err("text stream missing".into()),
        };
        let start = values[1].number()?;
        let count = values
            .get(2)
            .map(Value::number)
            .transpose()?
            .unwrap_or(length as f64 - start);
        if !start.is_finite()
            || !count.is_finite()
            || start < 0.0
            || count < 0.0
            || start.fract() != 0.0
            || count.fract() != 0.0
            || start + count > length as f64
        {
            return Err("font range outside text".into());
        }
        let mut run = attributes.borrow().clone();
        run.insert("start".into(), Value::Number(start));
        run.insert("count".into(), Value::Number(count));
        let mut object = object.borrow_mut();
        let runs = object.entry("\0font_ranges".into()).or_insert_with(|| {
            Value::Array(Rc::new(RefCell::new(HashMap::from([(
                "length".into(),
                Value::Number(0.0),
            )]))))
        });
        let Value::Array(runs) = runs else {
            return Err("invalid font ranges".into());
        };
        let mut runs = runs.borrow_mut();
        let n = runs["length"].number()? as usize;
        if n >= 1000 {
            return Err("text font range limit".into());
        }
        runs.insert(n.to_string(), Value::Object(Rc::new(RefCell::new(run))));
        runs.insert("length".into(), Value::Number(n as f64 + 1.0));
        Ok(Value::Null)
    }
}

pub(super) fn formatted_text(text: &str, ranges: Option<&Value>) -> Result<String, Fault> {
    let Some(Value::Array(ranges)) = ranges else {
        return Ok(escape_text(text));
    };
    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() > 4 * 1024 * 1024 {
        return Err("formatted text length limit".into());
    }
    let mut flags = vec![0u8; chars.len()];
    let ranges = ranges.borrow();
    for i in 0..ranges["length"].number()? as usize {
        let Some(Value::Object(run)) = ranges.get(&i.to_string()) else {
            return Err("invalid font range".into());
        };
        let run = run.borrow();
        let start = run["start"].number()? as usize;
        let count = run["count"].number()? as usize;
        let end = start.checked_add(count).ok_or("font range overflow")?;
        let span = flags.get_mut(start..end).ok_or("font range outside text")?;
        for (bit, name) in ["bold", "italic", "underline", "strikeout"]
            .iter()
            .enumerate()
        {
            if let Some(value) = run.get(*name) {
                for flag in span.iter_mut() {
                    if value.truth() {
                        *flag |= 1 << bit
                    } else {
                        *flag &= !(1 << bit)
                    }
                }
            }
        }
    }
    let mut out = String::new();
    let tags = ["B", "I", "U", "S"];
    let mut start = 0;
    while start < chars.len() {
        let mut end = start + 1;
        while end < chars.len() && flags[end] == flags[start] {
            end += 1;
        }
        for (bit, tag) in tags.iter().enumerate() {
            if flags[start] & (1 << bit) != 0 {
                out.push_str(&format!("<{tag}>"));
            }
        }
        out.push_str(&escape_text(&chars[start..end].iter().collect::<String>()));
        for (bit, tag) in tags.iter().enumerate().rev() {
            if flags[start] & (1 << bit) != 0 {
                out.push_str(&format!("</{tag}>"));
            }
        }
        start = end;
    }
    Ok(out)
}
fn escape_text(text: &str) -> String {
    static BREAKS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let text = BREAKS
        .get_or_init(|| regex::Regex::new(r"(?i)<br\s*/?>").unwrap())
        .replace_all(text, "\n");
    crate::monitor::escape(&text).replace('\n', "<BR>")
}
