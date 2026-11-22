//! JSON output for the native KJSON host object used by registration forms.
use super::*;

/// JSON submitted by the participant editor is data, never executable KS.
pub(super) fn parse(source: &str) -> Result<Value, String> {
    struct Parser<'a> {
        s: &'a [u8],
        at: usize,
    }
    impl Parser<'_> {
        fn ws(&mut self) {
            while self
                .s
                .get(self.at)
                .is_some_and(|b| matches!(b, b' ' | b'\r' | b'\n' | b'\t'))
            {
                self.at += 1;
            }
        }
        fn take(&mut self, b: u8) -> bool {
            if self.s.get(self.at) == Some(&b) {
                self.at += 1;
                true
            } else {
                false
            }
        }
        fn hex(&mut self) -> Result<u16, String> {
            let bytes = self
                .s
                .get(self.at..self.at + 4)
                .ok_or("Incomplete JSON escape")?;
            let mut v = 0;
            for b in bytes {
                v = v * 16
                    + match b {
                        b'0'..=b'9' => (b - b'0') as u16,
                        b'a'..=b'f' => (b - b'a' + 10) as u16,
                        b'A'..=b'F' => (b - b'A' + 10) as u16,
                        _ => return Err("Invalid JSON unicode escape".into()),
                    };
            }
            self.at += 4;
            Ok(v)
        }
        fn string(&mut self) -> Result<String, String> {
            if !self.take(b'"') {
                return Err("JSON string expected".into());
            }
            let mut out = Vec::new();
            loop {
                let b = *self.s.get(self.at).ok_or("Unterminated JSON string")?;
                self.at += 1;
                match b {
                    b'"' => return String::from_utf8(out).map_err(|_| "Invalid JSON UTF-8".into()),
                    b'\\' => {
                        let b = *self.s.get(self.at).ok_or("Incomplete JSON escape")?;
                        self.at += 1;
                        match b {
                            b'"' | b'\\' | b'/' => out.push(b),
                            b'b' => out.push(8),
                            b'f' => out.push(12),
                            b'n' => out.push(10),
                            b'r' => out.push(13),
                            b't' => out.push(9),
                            b'u' => {
                                let first = self.hex()?;
                                let cp = if (0xd800..=0xdbff).contains(&first) {
                                    if !self.take(b'\\') || !self.take(b'u') {
                                        return Err("Missing JSON surrogate".into());
                                    }
                                    let second = self.hex()?;
                                    if !(0xdc00..=0xdfff).contains(&second) {
                                        return Err("Invalid JSON surrogate".into());
                                    }
                                    0x10000
                                        + ((first as u32 - 0xd800) << 10)
                                        + (second as u32 - 0xdc00)
                                } else {
                                    first as u32
                                };
                                let ch = char::from_u32(cp).ok_or("Invalid JSON code point")?;
                                out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
                            }
                            _ => return Err("Invalid JSON escape".into()),
                        }
                    }
                    0..=31 => return Err("Unescaped JSON control character".into()),
                    _ => out.push(b),
                }
            }
        }
        fn value(&mut self, depth: usize) -> Result<Value, String> {
            crate::execution::check()?;
            if depth >= 128 {
                return Err("JSON nesting limit".into());
            }
            self.ws();
            Ok(match self.s.get(self.at).copied() {
                Some(b'"') => Value::String(self.string()?),
                Some(b'{') => {
                    self.at += 1;
                    self.ws();
                    let mut fields = HashMap::new();
                    if !self.take(b'}') {
                        loop {
                            self.ws();
                            let key = self.string()?;
                            if key.contains('\0') {
                                return Err("Invalid JSON object key".into());
                            }
                            self.ws();
                            if !self.take(b':') {
                                return Err("JSON colon expected".into());
                            }
                            fields.insert(key, self.value(depth + 1)?);
                            self.ws();
                            if self.take(b'}') {
                                break;
                            }
                            if !self.take(b',') {
                                return Err("JSON comma expected".into());
                            }
                        }
                    }
                    // Native KJSON data keeps its own key set: probing optional
                    // Attr/Link/oid members must not add registration attributes.
                    fields.insert("\0json".into(), Value::Bool(true));
                    Value::Object(Rc::new(RefCell::new(fields)))
                }
                Some(b'[') => {
                    self.at += 1;
                    self.ws();
                    let mut fields = HashMap::new();
                    let mut len = 0;
                    if !self.take(b']') {
                        loop {
                            fields.insert(len.to_string(), self.value(depth + 1)?);
                            len += 1;
                            self.ws();
                            if self.take(b']') {
                                break;
                            }
                            if !self.take(b',') {
                                return Err("JSON comma expected".into());
                            }
                        }
                    }
                    fields.insert("length".into(), Value::Number(len as f64));
                    Value::Array(Rc::new(RefCell::new(fields)))
                }
                Some(b't' | b'f' | b'n') => {
                    let (word, value) = match self.s[self.at] {
                        b't' => (b"true".as_slice(), Value::Bool(true)),
                        b'f' => (b"false".as_slice(), Value::Bool(false)),
                        _ => (b"null".as_slice(), Value::Null),
                    };
                    if !self.s[self.at..].starts_with(word) {
                        return Err("Invalid JSON literal".into());
                    }
                    self.at += word.len();
                    value
                }
                Some(b'-' | b'0'..=b'9') => {
                    let start = self.at;
                    self.take(b'-');
                    if !self.take(b'0') {
                        if !self
                            .s
                            .get(self.at)
                            .is_some_and(|b| matches!(b, b'1'..=b'9'))
                        {
                            return Err("Invalid JSON number".into());
                        }
                        while self.s.get(self.at).is_some_and(u8::is_ascii_digit) {
                            self.at += 1;
                        }
                    }
                    for exponent in [false, true] {
                        let present = if exponent {
                            self.take(b'e') || self.take(b'E')
                        } else {
                            self.take(b'.')
                        };
                        if present {
                            if exponent {
                                if !self.take(b'+') {
                                    self.take(b'-');
                                }
                            }
                            let digits = self.at;
                            while self.s.get(self.at).is_some_and(u8::is_ascii_digit) {
                                self.at += 1;
                            }
                            if self.at == digits {
                                return Err("Invalid JSON number".into());
                            }
                        }
                    }
                    let n = std::str::from_utf8(&self.s[start..self.at])
                        .unwrap()
                        .parse::<f64>()
                        .map_err(|_| "Invalid JSON number")?;
                    if !n.is_finite() {
                        return Err("JSON number out of range".into());
                    }
                    Value::Number(n)
                }
                _ => return Err("JSON value expected".into()),
            })
        }
    }
    if source.len() > 16 * 1024 * 1024 {
        return Err("JSON input limit".into());
    }
    let mut p = Parser {
        s: source.as_bytes(),
        at: 0,
    };
    let value = p.value(0)?;
    p.ws();
    if p.at != p.s.len() {
        return Err("Unexpected trailing JSON data".into());
    }
    Ok(value)
}

pub(super) fn stringify(value: &Value) -> Result<String, String> {
    fn quote(text: &str, out: &mut String) {
        use std::fmt::Write;
        out.push('"');
        for ch in text.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                ch if ch < ' ' || matches!(ch, '\u{2028}' | '\u{2029}' | '<') => {
                    write!(out, "\\u{:04x}", ch as u32).unwrap();
                }
                ch => out.push(ch),
            }
        }
        out.push('"');
    }
    fn write(value: &Value, out: &mut String, parents: &mut Vec<usize>) -> Result<(), String> {
        crate::execution::check()?;
        if parents.len() >= 128 || out.len() > 16 * 1024 * 1024 {
            return Err("KJSON.stringify nesting/output limit".into());
        }
        match value {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) if !n.is_finite() => out.push_str("null"),
            Value::Number(n) => out.push_str(&if *n == 0.0 { "0".into() } else { n.to_string() }),
            Value::String(s) => quote(s, out),
            Value::Array(map) | Value::Object(map) | Value::ReadOnlyObject(map) => {
                let identity = Rc::as_ptr(map) as usize;
                if parents.contains(&identity) {
                    return Err("KJSON.stringify circular reference".into());
                }
                parents.push(identity);
                let fields = map.borrow();
                if matches!(value, Value::Array(_)) {
                    let length = fields
                        .get("length")
                        .map(Value::number)
                        .transpose()?
                        .unwrap_or(0.0);
                    if !length.is_finite()
                        || length < 0.0
                        || length.fract() != 0.0
                        || length > 1_000_000.0
                    {
                        return Err("KJSON.stringify array length".into());
                    }
                    out.push('[');
                    for i in 0..length as usize {
                        if i != 0 {
                            out.push(',');
                        }
                        write(
                            fields.get(&i.to_string()).unwrap_or(&Value::Null),
                            out,
                            parents,
                        )?;
                    }
                    out.push(']');
                } else {
                    let mut keys = fields
                        .keys()
                        .filter(|k| !k.starts_with('\0'))
                        .collect::<Vec<_>>();
                    keys.sort();
                    out.push('{');
                    for (i, key) in keys.into_iter().enumerate() {
                        if i != 0 {
                            out.push(',');
                        }
                        quote(key, out);
                        out.push(':');
                        write(&fields[key], out, parents)?;
                    }
                    out.push('}');
                }
                parents.pop();
            }
            _ => return Err("KJSON.stringify unsupported value type".into()),
        }
        if out.len() > 16 * 1024 * 1024 {
            return Err("KJSON.stringify output limit".into());
        }
        Ok(())
    }
    let mut out = String::new();
    write(value, &mut out, &mut Vec::new())?;
    Ok(out)
}
