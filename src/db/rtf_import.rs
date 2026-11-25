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
    table_depth: usize,
    caps: bool,
    font: u32,
    font_table: bool,
}
#[derive(Default)]
struct Paragraph {
    bytes: Vec<u8>,
    attributes: Vec<Attribute>,
    pictures: Vec<Picture>,
    active: u32,
    align: u16,
}
enum Attribute {
    Style(u32),
    Picture(u32),
}
struct Picture {
    width: u16,
    height: u16,
    flags: u16,
    kind: u16,
    data: Vec<u8>,
}
struct Part {
    meta: Vec<u8>,
    text: Vec<u8>,
}
#[derive(Default)]
struct Row {
    widths: Vec<u16>,
    cells: Vec<Vec<Part>>,
    // Keep the source offset in twips until column boundaries are converted.
    left: i32,
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
                self.attributes.push(Attribute::Style(state.flags));
            }
            self.active = state.flags;
        }
        self.align = state.align;
        if state.caps {
            // Leave the legacy line-drawing codes untouched.
            self.bytes.extend(bytes.iter().map(|b| match *b {
                b'a'..=b'z' | 0xe0..=0xff => b - 32,
                0xb8 => 0xa8,
                _ => *b,
            }));
        } else {
            self.bytes.extend_from_slice(bytes);
        }
    }
    fn put_picture(&mut self, mut picture: Picture, state: &State) {
        // Images consume their own text marker, outside any active style run.
        // The next text run reopens the surrounding RTF style if necessary.
        if self.active != 0 {
            self.bytes.push(1);
            self.active = 0;
        }
        self.align = state.align;
        picture.flags = state.align;
        self.bytes.push(0);
        self.attributes
            .push(Attribute::Picture(self.pictures.len() as u32));
        self.pictures.push(picture);
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
        word(&mut m, p.attributes.len() as u16);
        for attribute in p.attributes {
            match attribute {
                Attribute::Style(flags) => {
                    word(&mut m, 9);
                    word(&mut m, 0x40);
                    long(&mut m, flags);
                }
                Attribute::Picture(index) => {
                    word(&mut m, 8);
                    long(&mut m, index);
                }
            }
        }
        if !p.pictures.is_empty() {
            word(&mut m, p.pictures.len() as u16);
            for picture in p.pictures {
                word(&mut m, 2);
                word(&mut m, picture.width);
                word(&mut m, picture.height);
                word(&mut m, picture.flags);
                word(&mut m, picture.kind);
                long(&mut m, picture.data.len() as u32);
                m.extend(picture.data);
            }
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
    // Native RTF import clips the displayed margin at zero. A negative margin
    // in DB4 causes the original RTF writer to fail when opening the table.
    word(
        &mut meta,
        i16::try_from((row.left / 15).max(0)).map_err(|_| "RTF row margin")? as u16,
    );
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
fn picture(input: &[u8], i: &mut usize) -> Result<Option<Picture>, String> {
    let mut data = Vec::new();
    let mut hex = None;
    let mut depth = 0usize;
    let mut wmf = false;
    let mut bitmap = false;
    let mut goal = [2880i32; 2];
    let mut scale = [100i32; 2];
    while *i < input.len() {
        let b = input[*i];
        if b == b'}' && depth == 0 {
            break;
        }
        *i += 1;
        if b == b'{' {
            depth += 1;
            if depth > 512 {
                return Err("RTF picture nesting limit".into());
            }
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
                    if hex.is_some() || end - *i > 16 * 1024 * 1024 - data.len() {
                        return Err("RTF picture binary size or alignment".into());
                    }
                    data.extend_from_slice(&input[*i..end]);
                }
                *i = end;
                continue;
            }
            if depth == 0 {
                match ctl {
                    "wmetafile" => wmf = true,
                    "picbmp" => bitmap = true,
                    "picwgoal" => goal[0] = n,
                    "pichgoal" => goal[1] = n,
                    "picscalex" => scale[0] = n,
                    "picscaley" => scale[1] = n,
                    _ => {}
                }
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
    // Native FromStream uses the WMF fallback, including its picbmp extension.
    // Direct PNG/JPEG/DIB remain in the separate, unchanged RTF attachment.
    if !wmf {
        return Ok(None);
    }
    let dimension = |axis: usize| -> Result<u16, String> {
        if goal[axis] < 0 || scale[axis] < 0 {
            return Err("Invalid RTF picture dimensions".into());
        }
        u16::try_from(i64::from(goal[axis]) * i64::from(scale[axis]) / 1500)
            .map_err(|_| "RTF picture dimensions overflow".into())
    };
    Ok(Some(Picture {
        width: dimension(0)?,
        height: dimension(1)?,
        flags: 0,
        kind: u16::from(bitmap),
        data,
    }))
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
    let mut parent_rows: Vec<(Row, Vec<Part>)> = Vec::new();
    let mut i = 0;
    let mut fallback = 0;
    let mut codepage = 1252;
    let mut default_font = 0;
    let mut font_pages = std::collections::HashMap::new();
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
                        put_encoded(
                            &mut p,
                            v,
                            *font_pages.get(&state.font).unwrap_or(&codepage),
                            &state,
                        )?;
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
                if state.star && !matches!(control, "fldrslt" | "nesttableprops") {
                    state.skip = true;
                }
                state.star = false;
                if control == "fonttbl" {
                    state.font_table = true;
                }
                if state.font_table {
                    if control == "f" {
                        state.font =
                            u32::try_from(n.unwrap_or(-1)).map_err(|_| "Invalid RTF font")?;
                    }
                    if control == "fcharset" {
                        match n {
                            Some(204) => {
                                font_pages.insert(state.font, 1251);
                            }
                            Some(0 | 1) => {
                                font_pages.insert(state.font, codepage);
                            }
                            _ => {}
                        }
                    }
                    if control == "cpg" {
                        font_pages.insert(state.font, n.ok_or("Missing RTF font code page")?);
                    }
                }
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
                        | "shppict"
                        | "nonesttables"
                ) {
                    state.skip = true;
                }
                if state.skip {
                    continue;
                }
                let v = n.unwrap_or(1);
                match control {
                    "ansicpg" => codepage = v,
                    "deff" => {
                        default_font = u32::try_from(v).map_err(|_| "Invalid RTF default font")?;
                        state.font = default_font;
                    }
                    "f" => state.font = u32::try_from(v).map_err(|_| "Invalid RTF font")?,
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
                            put_unicode(&mut p, c, &state)?;
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
                    "plain" => {
                        state.flags = 0;
                        state.caps = false;
                        state.font = default_font;
                    }
                    "caps" => state.caps = v != 0,
                    "pard" => {
                        state.align = 0;
                        state.table_depth = 0;
                    }
                    "intbl" => {
                        state.table_depth = state.table_depth.max(1);
                        // Subsequent Word rows may put their properties after
                        // the cell content, without an opening trowd.
                        if row.is_none() {
                            flush(&mut p, &mut parts, &mut cell, false, false);
                            row = Some(Row::default());
                        }
                    }
                    "itap" => {
                        let depth = usize::try_from(v)
                            .ok()
                            .filter(|d| *d <= 16)
                            .ok_or("RTF table nesting limit")?;
                        state.table_depth = depth;
                        if depth > 1 && depth > parent_rows.len() + 1 {
                            if depth != parent_rows.len() + 2 {
                                return Err("RTF table nesting gap".into());
                            }
                            let outer = row.take().ok_or("Nested RTF table has no parent row")?;
                            flush(&mut p, &mut parts, &mut cell, true, false);
                            parent_rows.push((outer, std::mem::take(&mut cell)));
                            row = Some(Row::default());
                        }
                    }
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
                        if let Some(r) = row.as_mut() {
                            // Word repeats row properties after the cell content.
                            // Replace the properties, retaining the completed cells.
                            r.widths.clear();
                            r.left = 0;
                            r.gap = 0;
                        } else {
                            flush(&mut p, &mut parts, &mut cell, false, false);
                            row = Some(Row::default());
                        }
                    }
                    "trleft" => {
                        if let Some(r) = &mut row {
                            r.left = v;
                        }
                    }
                    "trgaph" => {
                        if let Some(r) = &mut row {
                            r.gap = (v / 15).try_into().map_err(|_| "RTF row gap")?;
                        }
                    }
                    "cellx" => {
                        let r = row
                            .as_mut()
                            .ok_or_else(|| format!("RTF cell properties outside row at {i}"))?;
                        let end = (i64::from(v) - i64::from(r.left)) / 15;
                        let old = r.widths.iter().map(|v| i64::from(*v)).sum::<i64>();
                        r.widths
                            .push((end - old).try_into().map_err(|_| "RTF cell width")?);
                    }
                    "cell" | "nestcell" => {
                        if control == "nestcell" && parent_rows.is_empty() {
                            return Err("RTF nested cell has no parent".into());
                        }
                        let empty = cell.is_empty();
                        flush(&mut p, &mut parts, &mut cell, true, empty);
                        let r = row
                            .as_mut()
                            .ok_or_else(|| format!("RTF cell outside row at {i}"))?;
                        r.cells.push(std::mem::take(&mut cell));
                    }
                    "row" | "nestrow" => {
                        if !p.bytes.is_empty() || !cell.is_empty() {
                            return Err("Unclosed RTF cell".into());
                        }
                        let part = table(row.take().ok_or("RTF row missing")?)?;
                        if control == "nestrow" {
                            let (outer, mut outer_cell) =
                                parent_rows.pop().ok_or("RTF nested row has no parent")?;
                            outer_cell.push(part);
                            cell = outer_cell;
                            row = Some(outer);
                        } else {
                            if !parent_rows.is_empty() {
                                return Err("RTF outer row ends inside nested table".into());
                            }
                            parts.push(part);
                        }
                    }
                    "pict" => {
                        if let Some(picture) = picture(input, &mut i)? {
                            if p.align != state.align {
                                flush(&mut p, &mut parts, &mut cell, row.is_some(), false);
                            }
                            p.put_picture(picture, &state);
                        }
                    }
                    "object" => return Err(format!("Unsupported RTF object: {control}")),
                    _ => {}
                }
            }
            b => {
                if fallback > 0 {
                    fallback -= 1;
                } else if !state.skip {
                    put_encoded(
                        &mut p,
                        b,
                        *font_pages.get(&state.font).unwrap_or(&codepage),
                        &state,
                    )?;
                }
            }
        }
        if parts.len() > 10000
            || cell.len() > 10000
            || row
                .as_ref()
                .is_some_and(|r| r.cells.len() > 1000 || r.widths.len() > 1000)
            || p.bytes.len() > 16 * 1024 * 1024
            || p.attributes.len() > 4096
        {
            return Err("RTF object limit".into());
        }
    }
    if !stack.is_empty() || row.is_some() || !parent_rows.is_empty() || surrogate.is_some() {
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
fn put_unicode(p: &mut Paragraph, c: char, state: &State) -> Result<(), String> {
    // Native text has a legacy box-drawing alphabet in the 0x80..0x8f range.
    // Preserve those codes rather than substituting RTF's lossy ANSI fallback.
    let special = match c {
        '\u{2002}' => Some(b' '),
        '\u{2500}' => Some(0x83),
        '\u{2518}' => Some(0x85),
        '\u{2514}' => Some(0x86),
        '\u{2534}' => Some(0x87),
        '\u{2510}' => Some(0x89),
        '\u{250c}' => Some(0x8a),
        '\u{252c}' => Some(0x8b),
        '\u{2502}' => Some(0x8c),
        '\u{2524}' => Some(0x8d),
        '\u{251c}' => Some(0x8e),
        '\u{253c}' => Some(0x8f),
        _ => None,
    };
    if let Some(b) = special {
        p.put(&[b], state);
        return Ok(());
    }
    let mut buf = [0; 4];
    let (bytes, _, bad) = encoding_rs::WINDOWS_1251.encode(c.encode_utf8(&mut buf));
    if bad {
        return Err(format!(
            "RTF character U+{:04X} is not representable in native text",
            c as u32
        ));
    }
    p.put(&bytes, state);
    Ok(())
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
