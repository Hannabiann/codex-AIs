//! RTF output for transient report tables. Page and style settings come from
//! the registry definition, while the data is produced by the original KS.
use super::{report_document as doc, *};
use std::fmt::Write;

pub(super) fn styles() -> Value {
    doc::collection(
        "styles",
        (0..6)
            .map(|i| {
                doc::object(HashMap::from([
                    (
                        "localName".into(),
                        Value::String(
                            [
                                "Normal",
                                "Normal",
                                "Heading",
                                "Preformat",
                                "Hyperlink",
                                "Context",
                            ][i]
                                .into(),
                        ),
                    ),
                    ("additionalWords".into(), Value::String(String::new())),
                    (
                        "font".into(),
                        doc::object(HashMap::from([
                            (
                                "name".into(),
                                Value::String(if i == 3 { "Courier New" } else { "Arial" }.into()),
                            ),
                            (
                                "size".into(),
                                Value::Number([9.0, 9.0, 11.0, 10.0, 10.0, 10.0][i]),
                            ),
                            ("bold".into(), Value::Bool(i == 2)),
                            ("underline".into(), Value::Bool(i >= 4)),
                        ])),
                    ),
                    (
                        "color".into(),
                        doc::object(HashMap::from([
                            ("red".into(), Value::Number(0.0)),
                            (
                                "green".into(),
                                Value::Number(if i == 5 { 255.0 } else { 0.0 }),
                            ),
                            (
                                "blue".into(),
                                Value::Number(if i == 4 { 255.0 } else { 0.0 }),
                            ),
                        ])),
                    ),
                ]))
            })
            .collect(),
    )
}
fn controls(s: &str) -> Result<&str, Fault> {
    if s.len() > 4096
        || !s
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"\\ -\r\n".contains(&c))
    {
        return Err("Invalid RTF properties".into());
    }
    Ok(s)
}
fn escape(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '\\' | '{' | '}' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\line "),
            '\t' => out.push_str("\\tab "),
            '\r' | '\0' | '\u{1}' => {}
            ' '..='~' => out.push(c),
            _ => {
                for unit in c.encode_utf16(&mut [0; 2]) {
                    write!(out, "\\u{}?", *unit as i16).unwrap();
                }
            }
        }
    }
}
fn bounded(n: f64, min: f64, max: f64) -> Result<i64, Fault> {
    if !n.is_finite() || n < min || n > max {
        return Err("Invalid RTF dimension".into());
    }
    Ok(n.round() as i64)
}
fn font_flags(out: &mut String, m: &Object) {
    let m = m.borrow();
    for (key, ctl) in [
        ("bold", "b"),
        ("italic", "i"),
        ("underline", "ul"),
        ("strikeout", "strike"),
    ] {
        if let Some(value) = m.get(key) {
            write!(out, "\\{ctl}{}", if value.truth() { 1 } else { 0 }).unwrap();
        }
    }
}
struct Writer {
    out: String,
    styles: Vec<Value>,
    paragraph: String,
}
impl Writer {
    fn parts(&mut self, value: &Value, in_table: bool, depth: usize) -> Result<(), Fault> {
        if depth > 32 {
            return Err("Report nesting limit".into());
        }
        for part in doc::elements(value)? {
            let m = doc::map(&part)?;
            match doc::number(&m, "Type", 1.0)? as u32 {
                1 => self.text(&m, in_table)?,
                7 if !in_table => self.table(&m, depth + 1)?,
                kind => {
                    return Err(format!(
                        "RTF report part {kind} is not implemented at this position"
                    )
                    .into())
                }
            }
            if self.out.len() > 64 * 1024 * 1024 {
                return Err("RTF output size limit".into());
            }
        }
        Ok(())
    }
    fn text(&mut self, m: &Object, in_table: bool) -> Result<(), Fault> {
        let format = doc::number(m, "Format", 0x3000 as f64)? as u32;
        let style = match format & 0xf000 {
            0x4000 => 2,
            0x5000 => 3,
            _ => 0,
        };
        let definition = doc::map(&self.styles[style])?;
        let font = doc::map(&definition.borrow()["font"])?;
        let size = bounded(doc::number(&font, "size", 9.0)? * 2.0, 1.0, 3276.0)?;
        write!(
            self.out,
            "\\pard\\plain\\f{style}\\fs{size}\\cf{}\\{}{}",
            style + 1,
            ["ql", "qr", "qc", "qj"][(format & 3) as usize],
            if in_table { "\\intbl" } else { "" }
        )
        .unwrap();
        font_flags(&mut self.out, &font);
        self.out.push_str(&self.paragraph);
        if let Some(v) = definition.borrow().get("additionalWords") {
            self.out.push_str(controls(&v.text())?);
        }
        if let Some(v) = m.borrow().get("\0font_default") {
            font_flags(&mut self.out, &doc::map(v)?);
        }
        self.out.push(' ');
        let m = m.borrow();
        let text = doc::text_value(m.get("Text").ok_or("Report text missing")?)?;
        // Stream.Print terminates a paragraph. Its final newline is already
        // represented by \par below; internal newlines remain explicit breaks.
        let text = text.strip_suffix('\n').unwrap_or(&text);
        let chars = text.chars().collect::<Vec<_>>();
        let mut flags = vec![None::<u8>; chars.len()];
        if let Some(ranges) = m.get("\0font_ranges") {
            for range in doc::elements(ranges)? {
                let r = doc::map(&range)?;
                let start =
                    bounded(doc::number(&r, "start", 0.0)?, 0.0, chars.len() as f64)? as usize;
                let end = bounded(
                    start as f64 + doc::number(&r, "count", 0.0)?,
                    0.0,
                    (chars.len() + 1) as f64,
                )? as usize;
                for flag in flags
                    .get_mut(start..end.min(chars.len()))
                    .ok_or("Report font range outside text")?
                {
                    let mut bits = flag.unwrap_or(0);
                    for (i, key) in ["bold", "italic", "underline", "strikeout"]
                        .iter()
                        .enumerate()
                    {
                        if let Some(v) = r.borrow().get(*key) {
                            if v.truth() {
                                bits |= 1 << i
                            } else {
                                bits &= !(1 << i)
                            }
                        }
                    }
                    *flag = Some(bits);
                }
            }
        }
        let mut start = 0;
        while start < chars.len() {
            let mut end = start + 1;
            while end < chars.len() && flags[end] == flags[start] {
                end += 1;
            }
            if let Some(bits) = flags[start] {
                self.out.push('{');
                for (i, ctl) in ["b", "i", "ul", "strike"].iter().enumerate() {
                    write!(
                        self.out,
                        "\\{ctl}{}",
                        if bits & (1 << i) != 0 { 1 } else { 0 }
                    )
                    .unwrap();
                }
                self.out.push(' ');
            }
            escape(&mut self.out, &chars[start..end].iter().collect::<String>());
            if flags[start].is_some() {
                self.out.push('}');
            }
            start = end;
        }
        self.out.push_str("\\par\n");
        Ok(())
    }
    fn table(&mut self, m: &Object, depth: usize) -> Result<(), Fault> {
        let rows = m
            .borrow()
            .get("Rows")
            .cloned()
            .ok_or("Report table rows missing")?;
        for row in doc::elements(&rows)? {
            let r = doc::map(&row)?;
            let cells = doc::elements(&row)?;
            if cells.is_empty() {
                continue;
            }
            let left = bounded(doc::number(&r, "Margin", 0.0)? * 15.0, -31680.0, 31680.0)?;
            let gap = bounded(doc::number(&r, "Gap", 0.0)? * 15.0, 0.0, 31680.0)?;
            write!(self.out, "\\trowd\\trleft{left}\\trgaph{gap}").unwrap();
            let mut end = left;
            for cell in &cells {
                let c = doc::map(cell)?;
                let width = bounded(doc::number(&c, "Width", 100.0)? * 15.0, 1.0, 31680.0)?;
                end += width;
                for (key, side) in [
                    ("BorderTop", "t"),
                    ("BorderLeft", "l"),
                    ("BorderBottom", "b"),
                    ("BorderRight", "r"),
                ] {
                    let border = bounded(doc::number(&c, key, 0.0)?, 0.0, 255.0)?;
                    if border != 0 {
                        write!(self.out, "\\clbrdr{side}\\brdrs\\brdrw10").unwrap();
                    }
                }
                write!(self.out, "\\cellx{end}").unwrap();
            }
            self.out.push('\n');
            for cell in &cells {
                if doc::elements(cell)?.is_empty() {
                    self.out.push_str("\\pard\\intbl ");
                }
                self.parts(cell, true, depth)?;
                if self.out.ends_with("\\par\n") {
                    self.out.truncate(self.out.len() - 5);
                }
                self.out.push_str("\\cell\n");
            }
            self.out.push_str("\\row\n");
        }
        self.out.push_str("\\pard\n");
        Ok(())
    }
}
pub(super) fn render(field: &Value, writer: &Object) -> Result<Vec<u8>, Fault> {
    let writer = writer.borrow();
    let area = doc::map(
        writer
            .get("documentArea")
            .ok_or("RTF document area missing")?,
    )?;
    let area = area.borrow();
    let page = area
        .get("propertiesWords")
        .map(Value::text)
        .unwrap_or_default();
    let paragraph = area
        .get("paragraphPropertiesWords")
        .map(Value::text)
        .unwrap_or_default();
    controls(&page)?;
    controls(&paragraph)?;
    let styles = doc::elements(writer.get("\0styles").ok_or("RTF styles missing")?)?;
    let mut out = String::from("{\\rtf1\\ansi\\ansicpg1251\\uc1\\deff0{\\fonttbl");
    for (i, style) in styles.iter().enumerate() {
        let style = doc::map(style)?;
        let font = doc::map(&style.borrow()["font"])?;
        write!(out, "{{\\f{i}\\fnil ").unwrap();
        escape(
            &mut out,
            &font
                .borrow()
                .get("name")
                .map(Value::text)
                .unwrap_or_else(|| "Arial".into())
                .replace(';', ""),
        );
        out.push_str(";}");
    }
    out.push_str("}{\\colortbl;");
    for style in &styles {
        let style = doc::map(style)?;
        let color = doc::map(&style.borrow()["color"])?;
        for key in ["red", "green", "blue"] {
            write!(
                out,
                "\\{key}{}",
                bounded(doc::number(&color, key, 0.0)?, 0.0, 255.0)?
            )
            .unwrap();
        }
        out.push(';');
    }
    out.push_str("}\n");
    out.push_str(&page);
    out.push('\n');
    let mut writer = Writer {
        out,
        styles,
        paragraph,
    };
    writer.parts(field, false, 0)?;
    writer.out.push('}');
    Ok(writer.out.into_bytes())
}
