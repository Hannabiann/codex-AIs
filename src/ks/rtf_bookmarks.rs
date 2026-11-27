//! Native replaceBkmarkCharsInRTF: replace bookmark text, retain RTF layout.
use super::{Fault, Runtime, Value};
use std::{collections::HashMap, fmt::Write};

struct Control {
    end: usize,
    text: bool,
    binary: bool,
}

fn control(input: &[u8], at: usize) -> Result<Control, String> {
    let next = *input.get(at + 1).ok_or("Truncated RTF control")?;
    let mut end = at + 2;
    if next == b'\'' {
        if !input
            .get(end..end + 2)
            .is_some_and(|v| v.iter().all(u8::is_ascii_hexdigit))
        {
            return Err("Invalid RTF hex escape".into());
        }
        return Ok(Control {
            end: end + 2,
            text: true,
            binary: false,
        });
    }
    if !next.is_ascii_alphabetic() {
        return Ok(Control {
            end,
            text: matches!(next, b'\\' | b'{' | b'}' | b'~' | b'_' | b'-'),
            binary: false,
        });
    }
    while input.get(end).is_some_and(u8::is_ascii_alphabetic) {
        end += 1;
    }
    let name = &input[at + 1..end];
    let number = end;
    if input.get(end) == Some(&b'-') {
        end += 1;
    }
    while input.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    let number_end = end;
    if input.get(end) == Some(&b' ') {
        end += 1;
    }
    let binary = name == b"bin";
    if binary {
        let count: usize = std::str::from_utf8(&input[number..number_end])
            .map_err(|_| "Invalid RTF binary length")?
            .parse()
            .map_err(|_| "Invalid RTF binary length")?;
        end = end
            .checked_add(count)
            .filter(|end| *end <= input.len())
            .ok_or("Truncated RTF binary data")?;
    }
    Ok(Control {
        end,
        text: matches!(name, b"u" | b"tab" | b"line"),
        binary,
    })
}

#[derive(Clone)]
struct Marker {
    start: usize,
    end: usize,
    name: String,
    opening: bool,
}

fn markers(input: &[u8]) -> Result<Vec<Marker>, String> {
    if !input.starts_with(b"{\\rtf") {
        return Err("Bookmark template is not RTF".into());
    }
    let mut stack = Vec::new();
    let mut result = Vec::new();
    let mut i = 0;
    while i < input.len() {
        if i % 65536 == 0 {
            crate::execution::check()?;
        }
        match input[i] {
            b'{' => {
                if stack.len() >= 1024 {
                    return Err("RTF nesting exceeds 1024 groups".into());
                }
                stack.push(i);
                i += 1;
            }
            b'}' => {
                let start = stack.pop().ok_or("Unbalanced RTF group")?;
                let body = input[start + 1..i].trim_ascii();
                for (prefix, opening) in [
                    (b"\\*\\bkmkstart ".as_slice(), true),
                    (b"\\*\\bkmkend ".as_slice(), false),
                ] {
                    if let Some(name) = body.strip_prefix(prefix) {
                        let name = name.trim_ascii();
                        if name.iter().any(|b| matches!(b, b'{' | b'}' | b'\\')) {
                            return Err("Encoded RTF bookmark names are not supported".into());
                        }
                        result.push(Marker {
                            start,
                            end: i + 1,
                            name: crate::db::decode(name),
                            opening,
                        });
                    }
                }
                i += 1;
            }
            b'\\' => i = control(input, i)?.end,
            _ => i += 1,
        }
    }
    if !stack.is_empty() {
        return Err("Unbalanced RTF group".into());
    }
    result.sort_by_key(|m| m.start);
    Ok(result)
}

fn text(value: &str, cp1251: bool) -> Vec<u8> {
    if value.is_empty() {
        return Vec::new();
    }
    let (encoded, _, lossy) = encoding_rs::WINDOWS_1251.encode(value);
    let mut out = String::new();
    if cp1251 && !lossy {
        for b in encoded.iter().copied() {
            match b {
                b'\\' | b'{' | b'}' => {
                    out.push('\\');
                    out.push(b as char);
                }
                b'\n' => out.push_str(" \\par "),
                b'\t' => out.push_str("\\tab "),
                b'\r' | 0 => (),
                32..=126 => out.push(b as char),
                _ => {
                    write!(out, "\\'{b:02x}").unwrap();
                }
            }
        }
    } else {
        out.push_str("{\\uc1 ");
        for c in value.chars() {
            match c {
                '\\' | '{' | '}' => {
                    out.push('\\');
                    out.push(c);
                }
                '\n' => out.push_str(" \\par "),
                '\t' => out.push_str("\\tab "),
                '\r' | '\0' => (),
                ' '..='~' => out.push(c),
                _ => {
                    for unit in c.encode_utf16(&mut [0; 2]) {
                        write!(out, "\\u{}?", *unit as i16).unwrap();
                    }
                }
            }
        }
        out.push('}');
    }
    out.extend(['\r', '\n']);
    out.into_bytes()
}

fn replace(input: &[u8], values: &HashMap<String, String>) -> Result<Vec<u8>, String> {
    let mut open = HashMap::<String, Marker>::new();
    let mut ranges = Vec::new();
    for marker in markers(input)? {
        if !values.contains_key(&marker.name) {
            continue;
        }
        if marker.opening {
            if open.insert(marker.name.clone(), marker).is_some() {
                return Err("Duplicate open RTF bookmark".into());
            }
        } else {
            let start = open
                .remove(&marker.name)
                .ok_or("RTF bookmark has no beginning")?;
            // Word may place the two markers in adjacent formatting groups.
            // The replacement below preserves their braces and controls.
            ranges.push((start.end, marker.start, marker.name));
        }
    }
    if !open.is_empty() {
        return Err("RTF bookmark has no end".into());
    }
    ranges.sort_by_key(|r| r.0);
    let cp1251 = input.windows(12).any(|b| b == b"\\ansicpg1251");
    let mut output = Vec::with_capacity(input.len());
    let mut cursor = 0;
    for (start, end, name) in ranges {
        crate::execution::check()?;
        if start < cursor {
            return Err("Overlapping RTF bookmarks are not supported".into());
        }
        output.extend_from_slice(&input[cursor..start]);
        output.extend(text(&values[&name], cp1251));
        // The native replacement retains group/control formatting and physical
        // line breaks from the template, replacing only the displayed text.
        let mut i = start;
        while i < end {
            match input[i] {
                b'\\' => {
                    let token = control(input, i)?;
                    if token.binary {
                        return Err("Binary data inside an RTF bookmark is not supported".into());
                    }
                    if !token.text {
                        output.extend_from_slice(&input[i..token.end]);
                    }
                    i = token.end;
                }
                b'{' | b'}' | b'\r' | b'\n' => {
                    output.push(input[i]);
                    i += 1;
                }
                _ => i += 1,
            }
        }
        cursor = end;
        if output.len() > 16 * 1024 * 1024 {
            return Err("RTF output exceeds stream size limit".into());
        }
    }
    output.extend_from_slice(&input[cursor..]);
    if output.len() > 16 * 1024 * 1024 {
        return Err("RTF output exceeds stream size limit".into());
    }
    Ok(output)
}

impl Runtime<'_> {
    pub(super) fn replace_rtf_bookmarks(&self, args: &[Value]) -> Result<Value, Fault> {
        let [Value::ReadStream(source), Value::ReadStream(target), Value::Array(items)] = args
        else {
            return Err("replaceBkmarkCharsInRTF expects two streams and a bookmark array".into());
        };
        if std::rc::Rc::ptr_eq(source, target) {
            return Err("RTF source and destination must differ".into());
        }
        let mut values = HashMap::new();
        let items = items.borrow();
        let count = items
            .get("length")
            .ok_or("Bookmark array has no length")?
            .number()? as usize;
        for i in 0..count {
            let Some(Value::Object(item)) = items.get(&i.to_string()) else {
                return Err("Bookmark must have name and text".into());
            };
            let item = item.borrow();
            let name = item.get("name").ok_or("Bookmark name missing")?.text();
            let value = item.get("text").ok_or("Bookmark text missing")?.text();
            values.insert(name, value);
        }
        let result = {
            let input = source.borrow();
            if input.closed {
                return Err("RTF source stream closed".into());
            }
            replace(&input.data, &values)?
        };
        let mut output = target.borrow_mut();
        if output.closed {
            return Err("RTF destination stream closed".into());
        }
        output.insert_text(&result)?;
        let mut input = source.borrow_mut();
        input.position = input.data.len();
        Ok(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bookmarks_preserve_layout_and_escape_values() {
        let input = br"{\rtf1\ansicpg1251 {\*\bkmkstart x}old\'e0{\b text}{\*\bkmkend x}\par end}";
        let out = replace(
            input,
            &HashMap::from([
                ("x".into(), "А{\\}".into()),
                ("absent".into(), "ignored".into()),
            ]),
        )
        .unwrap();
        assert_eq!(out,b"{\\rtf1\\ansicpg1251 {\\*\\bkmkstart x}\\'c0\\{\\\\\\}\r\n{\\b }{\\*\\bkmkend x}\\par end}");
        assert!(String::from_utf8(text("🙂", true))
            .unwrap()
            .contains("\\u-10179?\\u-8638?"));
    }
    #[test]
    fn bookmark_parser_ignores_binary_and_rejects_unbalanced_ranges() {
        let bin = br"{\rtf1\bin3 {}\ {\*\bkmkstart x}a{\*\bkmkend x}}";
        assert_eq!(markers(bin).unwrap().len(), 2);
        for input in [
            br"{\rtf1{\*\bkmkstart x}a}".as_slice(),
            br"{\rtf1{{\*\bkmkstart x}a}{\*\bkmkend x}",
            br"{\rtf1\bin999 x}",
        ] {
            assert!(replace(input, &HashMap::from([("x".into(), "new".into())])).is_err());
        }
    }
    #[test]
    fn bookmarks_cross_formatting_groups_and_keep_native_paragraphs() {
        let input = br"{\rtf1\ansicpg1251 {\b {\*\bkmkstart x}old}{\i other{\*\bkmkend x}}}";
        let out = replace(input, &HashMap::from([("x".into(), "one\r\ntwo".into())])).unwrap();
        assert_eq!(out, b"{\\rtf1\\ansicpg1251 {\\b {\\*\\bkmkstart x}one \\par two\r\n}{\\i {\\*\\bkmkend x}}}");
        assert_eq!(markers(&out).unwrap().len(), 2);
        let empty = replace(input, &HashMap::from([("x".into(), String::new())])).unwrap();
        assert_eq!(markers(&empty).unwrap().len(), 2);
        assert_eq!(
            empty,
            br"{\rtf1\ansicpg1251 {\b {\*\bkmkstart x}}{\i {\*\bkmkend x}}}"
        );
    }
}
