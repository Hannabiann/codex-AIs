//! Word RTF to the native DB40 text-object stream. The original RTF attachment
//! remains separate; this stream is used by document views and full-text search.
use super::super::fields::Value;

#[derive(Clone, Default)]
struct State {
    skip: bool,
    uc: usize,
    flags: u32,
    align: u16,
    star: bool,
}
#[derive(Default)]
struct Paragraph {
    bytes: Vec<u8>,
    styles: Vec<u32>,
    active: u32,
    align: u16,
}
struct Part {
    meta: Vec<u8>,
    text: Vec<u8>,
}
#[derive(Default)]
struct Row {
    widths: Vec<u16>,
    cells: Vec<Vec<Part>>,
    left: i16,
    gap: u16,
}
fn word(b: &mut Vec<u8>, n: u16) {
    b.extend(n.to_le_bytes());
}
fn long(b: &mut Vec<u8>, n: u32) {
    b.extend(n.to_le_bytes());
}
impl Paragraph {
    fn put(&mut self, bytes: &[u8], state: &State) {
        if bytes.is_empty() {
            return;
        }
        if self.active != state.flags {
            if self.active != 0 {
                self.bytes.push(1);
            }
            if state.flags != 0 {
                self.bytes.push(0);
                self.styles.push(state.flags);
            }
            self.active = state.flags;
        }
        self.align = state.align;
        self.bytes.extend_from_slice(bytes);
    }
    fn finish(&mut self) -> Part {
        let mut p = std::mem::take(self);
        if p.active != 0 {
            p.bytes.push(1);
        }
        let mut m = Vec::new();
        word(&mut m, 1);
        word(&mut m, p.align);
        long(&mut m, p.bytes.len() as u32);
        word(&mut m, p.styles.len() as u16);
        for flags in p.styles {
            word(&mut m, 9);
            word(&mut m, 0x40);
            long(&mut m, flags);
        }
        Part {
            meta: m,
            text: p.bytes,
        }
    }
}
fn table(row: Row) -> Result<Part, String> {
    if row.widths.len() != row.cells.len() || row.cells.is_empty() {
        return Err("RTF table column mismatch".into());
    }
    let mut meta = Vec::new();
    word(&mut meta, 7);
    long(&mut meta, 4);
    long(&mut meta, 0);
    long(&mut meta, 1);
    long(&mut meta, row.cells.len() as u32);
    for (width, cell) in row.widths.iter().zip(&row.cells) {
        word(&mut meta, *width);
        word(&mut meta, cell.len() as u16);
    }
    long(&mut meta, 6);
    word(&mut meta, 17);
    word(&mut meta, row.left as u16);
    word(&mut meta, row.gap);
    let count: usize = row.cells.iter().map(Vec::len).sum();
    if count > 10000 {
        return Err("RTF table object limit".into());
    }
    word(&mut meta, count as u16);
    let mut text = Vec::new();
    for part in row.cells.into_iter().flatten() {
        meta.extend(part.meta);
        text.extend(part.text);
    }
    Ok(Part { meta, text })
}
fn flush(p: &mut Paragraph, parts: &mut Vec<Part>, cell: &mut Vec<Part>, row: bool, empty: bool) {
    if empty || !p.bytes.is_empty() {
        if row {
            cell.push(p.finish());
        } else {
            parts.push(p.finish());
        }
    }
}
fn picture(input: &[u8], i: &mut usize) -> Result<(), String> {
    let mut data = Vec::new();
    let mut hex = None;
    let mut depth = 0usize;
    while *i < input.len() {
        let b = input[*i];
        if b == b'}' && depth == 0 {
            break;
        }
        *i += 1;
        if b == b'{' {
            depth += 1;
            continue;
        }
        if b == b'}' {
            depth -= 1;
            continue;
        }
        if b == b'\\' {
            let start = *i;
            while *i < input.len() && input[*i].is_ascii_alphabetic() {
                *i += 1;
            }
            let ctl = std::str::from_utf8(&input[start..*i]).unwrap();
            let ns = *i;
            if input.get(*i) == Some(&b'-') {
                *i += 1;
            }
            while *i < input.len() && input[*i].is_ascii_digit() {
                *i += 1;
            }
            let n = if *i > ns {
                std::str::from_utf8(&input[ns..*i])
                    .unwrap()
                    .parse::<i32>()
                    .map_err(|_| "Invalid RTF picture number")?
            } else {
                0
            };
            if input.get(*i) == Some(&b' ') {
                *i += 1;
            }
            if ctl.is_empty() && *i < input.len() {
                *i += 1;
            }
            if ctl == "bin" {
                let end = i
                    .checked_add(usize::try_from(n).map_err(|_| "Invalid picture size")?)
                    .filter(|e| *e <= input.len())
                    .ok_or("Truncated picture")?;
                if depth == 0 {
                    data.extend_from_slice(&input[*i..end]);
                }
                *i = end;
                continue;
            }
        } else if depth == 0 && !b.is_ascii_whitespace() {
            let v = (b as char).to_digit(16).ok_or("Invalid RTF picture hex")? as u8;
            if let Some(h) = hex.take() {
                data.push(h * 16 + v);
            } else {
                hex = Some(v);
            }
        }
        if data.len() > 16 * 1024 * 1024 {
            return Err("RTF picture limit".into());
        }
    }
    if *i == input.len() || hex.is_some() || data.is_empty() {
        return Err("Incomplete RTF picture".into());
    }
    // Native FromStream("RTF") omits pictures in this text representation.
    // The original image remains intact in the separate Word/RTF attachment.
    Ok(())
}
pub(crate) fn parse(input: &[u8]) -> Result<Value, String> {
    if !crate::document::has_rtf_header(input) {
        return Err("Invalid RTF header".into());
    }
    if input.len() > 64 * 1024 * 1024 {
        return Err("RTF input limit".into());
    }
    let mut state = State {
        uc: 1,
        ..State::default()
    };
    let mut stack = Vec::new();
    let mut p = Paragraph::default();
    let mut parts = Vec::new();
    let mut cell = Vec::new();
    let mut row: Option<Row> = None;
    let mut i = 0;
    let mut fallback = 0;
    let mut codepage = 1252;
    let mut surrogate = None;
    while i < input.len() {
        let b = input[i];
        i += 1;
        match b {
            b'{' => {
                if stack.len() >= 512 {
                    return Err("RTF nesting limit".into());
                }
                stack.push(state.clone());
                state.star = false;
            }
            b'}' => {
                state = stack.pop().ok_or("Unbalanced RTF group")?;
                if stack.is_empty() {
                    if input[i..]
                        .iter()
                        .any(|b| !b.is_ascii_whitespace() && *b != 0)
                    {
                        return Err("Data after RTF root".into());
                    }
                    i = input.len();
                }
            }
            b'\r' | b'\n' => {}
            b'\\' => {
                let b = *input.get(i).ok_or("Truncated RTF control")?;
                i += 1;
                if b == b'\'' {
                    let hex = input.get(i..i + 2).ok_or("Truncated RTF hex escape")?;
                    i += 2;
                    let h = std::str::from_utf8(hex).map_err(|_| "Invalid RTF hex")?;
                    let v = u8::from_str_radix(h, 16).map_err(|_| "Invalid RTF hex")?;
                    if fallback > 0 {
                        fallback -= 1;
                    } else if !state.skip {
                        put_encoded(&mut p, v, codepage, &state)?;
                    }
                    continue;
                }
                if !b.is_ascii_alphabetic() {
                    if b == b'*' {
                        state.star = true;
                        continue;
                    }
                    if fallback > 0 {
                        fallback -= 1;
                        continue;
                    }
                    if !state.skip {
                        match b {
                            b'\\' | b'{' | b'}' => p.put(&[b], &state),
                            b'~' => p.put(&[0xa0], &state),
                            b'-' => {}
                            b'_' => p.put(b"-", &state),
                            _ => {}
                        }
                    }
                    continue;
                }
                let start = i - 1;
                while i < input.len() && input[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let control = std::str::from_utf8(&input[start..i]).unwrap();
                let nstart = i;
                if input.get(i) == Some(&b'-') {
                    i += 1;
                }
                while i < input.len() && input[i].is_ascii_digit() {
                    i += 1;
                }
                let n = if i > nstart {
                    Some(
                        std::str::from_utf8(&input[nstart..i])
                            .unwrap()
                            .parse::<i32>()
                            .map_err(|_| "Invalid RTF number")?,
                    )
                } else {
                    None
                };
                if input.get(i) == Some(&b' ') {
                    i += 1;
                }
                if control == "bin" {
                    let n = usize::try_from(n.unwrap_or(-1))
                        .map_err(|_| "Invalid RTF binary length")?;
                    i = i
                        .checked_add(n)
                        .filter(|e| *e <= input.len())
                        .ok_or("Truncated RTF binary")?;
                    if !state.skip {
                        return Err("RTF binary object outside destination".into());
                    }
                    continue;
                }
                if state.star && !matches!(control, "fldrslt" | "shppict") {
                    state.skip = true;
                }
                state.star = false;
                if matches!(
                    control,
                    "fonttbl"
                        | "colortbl"
                        | "stylesheet"
                        | "info"
                        | "generator"
                        | "listtable"
                        | "listoverridetable"
                        | "revtbl"
                        | "rsidtbl"
                        | "xmlnstbl"
                        | "datastore"
                        | "themedata"
                        | "colorschememapping"
                        | "fldinst"
                        | "nonshppict"
                ) {
                    state.skip = true;
                }
                if state.skip {
                    continue;
                }
                let v = n.unwrap_or(1);
                match control {
                    "ansicpg" => codepage = v,
                    "uc" => {
                        state.uc = usize::try_from(v)
                            .ok()
                            .filter(|n| *n <= 16)
                            .ok_or("Invalid RTF uc")?
                    }
                    "u" => {
                        if !(-32768..=65535).contains(&v) {
                            return Err("Invalid RTF Unicode unit".into());
                        }
                        let unit = v as u16;
                        if surrogate.is_some() && !(0xdc00..=0xdfff).contains(&unit) {
                            return Err("Invalid RTF surrogate pair".into());
                        }
                        if (0xd800..=0xdbff).contains(&unit) {
                            surrogate = Some(unit);
                        } else {
                            let c = if let Some(high) = surrogate.take() {
                                char::from_u32(
                                    0x10000
                                        + ((high as u32 - 0xd800) << 10)
                                        + (unit as u32).wrapping_sub(0xdc00),
                                )
                            } else {
                                char::from_u32(unit as u32)
                            };
                            let c = c.ok_or("Invalid RTF Unicode")?;
                            let mut buf = [0; 4];
                            let (bytes, _, bad) =
                                encoding_rs::WINDOWS_1251.encode(c.encode_utf8(&mut buf));
                            if bad {
                                return Err("RTF character is not representable in native Windows-1251 text".into());
                            }
                            p.put(&bytes, &state);
                        }
                        fallback = state.uc;
                    }
                    "b" | "i" | "ul" | "strike" => {
                        let bit = match control {
                            "b" => 1,
                            "i" => 2,
                            "ul" => 4,
                            _ => 8,
                        };
                        if v == 0 {
                            state.flags &= !bit;
                        } else {
                            state.flags |= bit;
                        }
                    }
                    "ulnone" => state.flags &= !4,
                    "plain" => state.flags = 0,
                    "pard" => state.align = 0,
                    "ql" => state.align = 0,
                    "qr" => state.align = 1,
                    "qc" => state.align = 2,
                    "qj" => state.align = 3,
                    "par" => flush(&mut p, &mut parts, &mut cell, row.is_some(), true),
                    "line" => p.put(b"\n", &state),
                    "tab" => p.put(b"\t", &state),
                    "emdash" => p.put(&[0x97], &state),
                    "endash" => p.put(&[0x96], &state),
                    "bullet" => p.put(&[0x95], &state),
                    "lquote" => p.put(&[0x91], &state),
                    "rquote" => p.put(&[0x92], &state),
                    "ldblquote" => p.put(&[0x93], &state),
                    "rdblquote" => p.put(&[0x94], &state),
                    "trowd" => {
                        if row.is_some() {
                            return Err("Nested RTF tables are not supported".into());
                        }
                        flush(&mut p, &mut parts, &mut cell, false, false);
                        row = Some(Row::default());
                    }
                    "trleft" => {
                        if let Some(r) = &mut row {
                            r.left = (v / 15).try_into().map_err(|_| "RTF row margin")?;
                        }
                    }
                    "trgaph" => {
                        if let Some(r) = &mut row {
                            r.gap = (v / 15).try_into().map_err(|_| "RTF row gap")?;
                        }
                    }
                    "cellx" => {
                        let r = row.as_mut().ok_or("RTF cell outside row")?;
                        let end = v / 15;
                        let old = r.left as i32 + r.widths.iter().map(|v| *v as i32).sum::<i32>();
                        r.widths
                            .push((end - old).try_into().map_err(|_| "RTF cell width")?);
                    }
                    "cell" => {
                        let empty = cell.is_empty();
                        flush(&mut p, &mut parts, &mut cell, true, empty);
                        let r = row.as_mut().ok_or("RTF cell outside row")?;
                        r.cells.push(std::mem::take(&mut cell));
                    }
                    "row" => {
                        if !p.bytes.is_empty() || !cell.is_empty() {
                            return Err("Unclosed RTF cell".into());
                        }
                        parts.push(table(row.take().ok_or("RTF row missing")?)?);
                    }
                    "pict" => picture(input, &mut i)?,
                    "object" | "nesttableprops" => {
                        return Err(format!("Unsupported RTF object: {control}"))
                    }
                    _ => {}
                }
            }
            b => {
                if fallback > 0 {
                    fallback -= 1;
                } else if !state.skip {
                    put_encoded(&mut p, b, codepage, &state)?;
                }
            }
        }
        if parts.len() > 10000
            || cell.len() > 10000
            || row
                .as_ref()
                .is_some_and(|r| r.cells.len() > 1000 || r.widths.len() > 1000)
            || p.bytes.len() > 16 * 1024 * 1024
            || p.styles.len() > 4096
        {
            return Err("RTF object limit".into());
        }
    }
    if !stack.is_empty() || row.is_some() || surrogate.is_some() {
        return Err("Incomplete RTF document".into());
    }
    let empty = parts.is_empty();
    flush(&mut p, &mut parts, &mut cell, false, empty);
    if parts.len() > 10000 {
        return Err("RTF object limit".into());
    }
    let mut stream = Vec::new();
    word(&mut stream, parts.len() as u16);
    let mut data = Vec::new();
    for part in parts {
        stream.extend(part.meta);
        data.extend(part.text);
    }
    let mut metadata = Vec::new();
    for chunk in stream.chunks(65000) {
        word(&mut metadata, chunk.len() as u16);
        word(&mut metadata, chunk.len() as u16);
        word(&mut metadata, super::crc16(chunk));
        metadata.extend(chunk);
    }
    Ok(Value::TextWithMetadata { data, metadata })
}
fn put_encoded(p: &mut Paragraph, b: u8, codepage: i32, state: &State) -> Result<(), String> {
    if b < 128 || codepage == 1251 {
        p.put(&[b], state);
        return Ok(());
    }
    let name = format!("windows-{codepage}");
    let enc =
        encoding_rs::Encoding::for_label(name.as_bytes()).ok_or("Unsupported RTF code page")?;
    let one = [b];
    let (s, _, bad) = enc.decode(&one);
    if bad {
        return Err("Invalid RTF character".into());
    }
    let (bytes, _, bad) = encoding_rs::WINDOWS_1251.encode(&s);
    if bad {
        return Err("RTF character outside native code page".into());
    }
    p.put(&bytes, state);
    Ok(())
}
