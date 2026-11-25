//! Native DocTextField objects (0062dbc0, 0062ded0, 0062a830).
//! Offsets are lengths in the original CP1251 text, not UTF-8 offsets.
use crate::{db::fields::Field, document};
#[path = "rich_text_rtf.rs"]
mod rtf_writer;
pub use rtf_writer::rtf;

#[derive(Clone, Debug)]
pub enum Item {
    Inline {
        text: Box<Item>,
        pictures: Vec<(usize, std::sync::Arc<Item>)>,
    },
    Table {
        rows: Vec<Row>,
        children: Vec<Item>,
    },
    Text {
        format: u16,
        start: usize,
        length: usize,
        styles: Vec<u32>,
    },
    Picture {
        width: u16,
        height: u16,
        flags: u16,
        kind: u16,
        data: Vec<u8>,
    },
}
#[derive(Clone, Debug)]
pub struct Cell {
    pub width: u16,
    pub count: usize,
    pub borders: u8,
}
#[derive(Clone, Debug)]
pub struct Row {
    pub cells: Vec<Cell>,
    pub padding: u16,
    pub margin: i16,
}
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).ok_or("rich text size overflow")?;
        let b = self
            .bytes
            .get(self.at..end)
            .ok_or("truncated rich text metadata")?;
        self.at = end;
        Ok(b)
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
}
pub fn unpack(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut c = Cursor { bytes, at: 0 };
    let mut result = Vec::new();
    while c.at < bytes.len() {
        let size = c.u16()? as usize;
        let packed = c.u16()? as usize;
        c.u16()?;
        if size == 0 || size > 65000 || packed == 0 || packed > size {
            return Err("invalid metadata block".into());
        }
        let data = c.take(packed)?;
        if result.len() + size > crate::db::fields::MAX_FIELD {
            return Err("rich text metadata limit".into());
        }
        if size == packed {
            result.extend_from_slice(data)
        } else {
            result
                .extend(crate::db::fields::decompress_mode4(data, size).map_err(|e| e.to_string())?)
        }
    }
    Ok(result)
}
pub fn items(field: &Field) -> Result<Vec<Item>, String> {
    let text = document::bytes(field)?;
    let metadata = document::metadata(field);
    if metadata.is_empty() {
        return Ok(if text.is_empty() {
            vec![]
        } else {
            vec![Item::Text {
                format: 0,
                start: 0,
                length: text.len(),
                styles: vec![],
            }]
        });
    }
    let data = unpack(metadata)?;
    let mut c = Cursor {
        bytes: &data,
        at: 0,
    };
    let mut start = 0usize;
    let out = read_items(&mut c, text.len(), &mut start, 0, &mut 10000, false)?;
    if c.at != data.len() || start != text.len() {
        return Err(format!(
            "rich text metadata/text length mismatch: {} of {}, {start} of {}",
            c.at,
            data.len(),
            text.len()
        ));
    }
    Ok(out)
}
/// Sum the Text streams exposed by the top-level native field enumerator.
/// Tables/pictures have no Text property. The property editor does not need to
/// render hyperlinks, OLE data or the optional style dictionary after the runs.
pub fn attribute_text_size(field: &Field) -> Result<usize, String> {
    let text = document::bytes(field)?;
    if text.is_empty() || document::metadata(field).is_empty() {
        return Ok(text.len());
    }
    let data = unpack(document::metadata(field))?;
    let mut c = Cursor {
        bytes: &data,
        at: 0,
    };
    let mut start = 0;
    let objects = read_items(&mut c, text.len(), &mut start, 0, &mut 10000, true)?;
    if start != text.len() {
        return Err("text object lengths do not cover field".into());
    }
    Ok(objects
        .iter()
        .map(|item| match item {
            Item::Text { length, .. } => *length,
            Item::Inline { text, .. } => match text.as_ref() {
                Item::Text { length, .. } => *length,
                _ => 0,
            },
            _ => 0,
        })
        .sum())
}
/// Text controls 0/1 delimit emphasis; literal markup remains escaped.
fn read_items(
    c: &mut Cursor<'_>,
    text_len: usize,
    start: &mut usize,
    depth: usize,
    budget: &mut usize,
    size_only: bool,
) -> Result<Vec<Item>, String> {
    if depth > 16 {
        return Err("table nesting limit".into());
    }
    let count = c.u16()? as usize;
    if count > 10000 {
        return Err("rich text object limit".into());
    }
    let mut out = Vec::new();

    for _ in 0..count {
        if *budget == 0 {
            return Err("rich text object limit".into());
        }
        *budget -= 1;
        match c.u16()? {
            1 => {
                let format = c.u16()?;
                let length = c.u32()? as usize;
                let attrs = c.u16()? as usize;
                if attrs > 4096 {
                    return Err("rich text attribute limit".into());
                }
                let mut styles = Vec::new();
                let mut embedded = Vec::new();
                for _ in 0..attrs {
                    let attr = c.u16()?;
                    match attr & 255 {
                        9 => {
                            let header = c.u16()?;
                            let data = c.take((header >> 4) as usize)?;
                            styles.push(if data.len() >= 4 {
                                u32::from_le_bytes(data[..4].try_into().unwrap())
                            } else {
                                0
                            });
                        }
                        8 => {
                            embedded.push((styles.len(), c.u32()? as usize));
                            styles.push(0);
                        }
                        5 if size_only => {
                            // Native link descriptor: DWORD + 32-byte target (006293d0).
                            c.take(36)?;
                        }
                        5 => return Err("rich text linked attribute is not implemented".into()),
                        _ => {
                            c.u32()?;
                        }
                    }
                }
                if start
                    .checked_add(length)
                    .filter(|n| *n <= text_len)
                    .is_none()
                {
                    return Err("rich text range exceeds field".into());
                }
                let item = Item::Text {
                    format,
                    start: *start,
                    length,
                    styles,
                };
                *start += length;
                // Attribute 8 is followed by a counted object stream, even
                // when the paragraph has no printable text (a picture anchor).
                if embedded.is_empty() {
                    out.push(item);
                } else {
                    let objects = read_items(c, text_len, start, depth + 1, budget, size_only)?;
                    if objects.iter().any(|o| !matches!(o, Item::Picture { .. })) {
                        return Err("embedded non-picture text object is not implemented".into());
                    }
                    let objects: Vec<_> = objects.into_iter().map(std::sync::Arc::new).collect();
                    let pictures = embedded
                        .into_iter()
                        .map(|(run, index)| {
                            objects
                                .get(index)
                                .cloned()
                                .map(|p| (run, p))
                                .ok_or_else(|| "embedded picture index outside stream".to_string())
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    out.push(Item::Inline {
                        text: Box::new(item),
                        pictures,
                    });
                }
            }
            2 => {
                let width = c.u16()?;
                let height = c.u16()?;
                let flags = c.u16()?;
                let kind = c.u16()?;
                let size = c.u32()? as usize;
                if flags & 0x1000 != 0 {
                    return Err("external rich text picture is not implemented".into());
                }
                if size > crate::db::fields::MAX_FIELD {
                    return Err("rich text picture limit".into());
                }
                out.push(Item::Picture {
                    width,
                    height,
                    flags,
                    kind,
                    data: c.take(size)?.to_vec(),
                });
            }
            7 => {
                // DocObjTable 0062e6d0; TableRow 0062d1d0.
                let header = c.u32()? as usize;
                if !(4..=65536).contains(&header) {
                    return Err("table header size".into());
                }
                c.take(header)?;
                let nrows = c.u32()? as usize;
                if nrows > 1000 {
                    return Err("table row limit".into());
                }
                let mut rows = Vec::new();
                let mut total = 0usize;
                for _ in 0..nrows {
                    let n = c.u32()? as usize;
                    if n > 1000 {
                        return Err("table cell limit".into());
                    }
                    let mut cells = Vec::new();
                    for _ in 0..n {
                        let width = c.u16()?;
                        let count = c.u16()? as usize;
                        total = total.checked_add(count).ok_or("table count overflow")?;
                        cells.push(Cell {
                            width,
                            count,
                            borders: 0,
                        });
                    }
                    let style = c.u32()? as usize;
                    if !(6..=65536).contains(&style) {
                        return Err("table row style size".into());
                    }
                    let data = c.take(style)?;
                    let padding = u16::from_le_bytes([data[4], data[5]]);
                    if style > 6 {
                        if style - 6 < (n + 1) / 2 {
                            return Err("truncated table borders".into());
                        }
                        for (i, cell) in cells.iter_mut().enumerate() {
                            cell.borders = (data[6 + i / 2] >> ((i % 2) * 4)) & 15;
                        }
                    }
                    // Row format occupies the first WORD, followed by the
                    // signed left margin and the cell padding, in screen units.
                    let margin = i16::from_le_bytes([data[2], data[3]]);
                    rows.push(Row {
                        cells,
                        padding,
                        margin,
                    });
                }
                let children = read_items(c, text_len, start, depth + 1, budget, size_only)?;
                if total != children.len() {
                    return Err("table cell object count mismatch".into());
                }
                out.push(Item::Table { rows, children });
            }
            kind => return Err(format!("rich text object type {kind} is not implemented")),
        }
    }
    Ok(out)
}
pub fn fragment(text: &str) -> String {
    let mut s = String::new();
    let mut space = false;
    for ch in text.chars() {
        match ch {
            '\0' => s.push_str("<B>"),
            '\x01' => s.push_str("</B>"),
            '\n' => {
                s.push_str("<BR>\n");
                space = true;
                continue;
            }
            '\r' => continue,
            ' ' if space => s.push_str("&nbsp;"),
            '<' => s.push_str("&lt;"),
            '>' => s.push_str("&gt;"),
            '&' => s.push_str("&amp;"),
            '"' => s.push_str("&quot;"),
            ch if ch.is_control() && ch != '\t' => {}
            ch => s.push(ch),
        }
        space = ch == ' ';
    }
    s
}
pub fn item_at(items: &[Item], index: usize) -> Option<&Item> {
    fn walk<'a>(items: &'a [Item], index: usize, at: &mut usize) -> Option<&'a Item> {
        for item in items {
            if *at == index {
                return Some(item);
            }
            *at += 1;
            if let Item::Table { children, .. } = item {
                if let Some(found) = walk(children, index, at) {
                    return Some(found);
                }
            }
            if let Item::Inline { pictures, .. } = item {
                for (_, picture) in pictures {
                    if let Some(found) = walk(std::slice::from_ref(picture.as_ref()), index, at) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }
    walk(items, index, &mut 0)
}
fn styled_fragment(text: &str, styles: &[u32]) -> String {
    styled_fragment_images(text, styles, &[])
}
fn styled_fragment_images(text: &str, styles: &[u32], pictures: &[(usize, String)]) -> String {
    let mut result = String::new();
    let mut at = 0;
    let mut next_style = styles.iter().enumerate();
    let mut stack: Vec<Vec<&str>> = Vec::new();
    for (offset, c) in text.char_indices() {
        if c != '\0' && c != '\x01' {
            continue;
        }
        result.push_str(&fragment(&text[at..offset]));
        if c == '\0' {
            let (run, flags) = next_style
                .next()
                .map(|(i, f)| (i, *f))
                .unwrap_or((usize::MAX, 1));
            if let Some((_, image)) = pictures.iter().find(|(i, _)| *i == run) {
                result.push_str(image);
                at = offset + 1;
                continue;
            }
            let tags: Vec<_> = ["B", "I", "U", "STRIKE"]
                .into_iter()
                .enumerate()
                .filter_map(|(bit, tag)| (flags & (1 << bit) != 0).then_some(tag))
                .collect();
            for tag in &tags {
                result.push_str(&format!("<{tag}>"));
            }
            stack.push(tags);
        } else if let Some(tags) = stack.pop() {
            for tag in tags.iter().rev() {
                result.push_str(&format!("</{tag}>"));
            }
        }
        at = offset + 1;
    }
    result.push_str(&fragment(&text[at..]));
    for tags in stack.iter().rev() {
        for tag in tags.iter().rev() {
            result.push_str(&format!("</{tag}>"));
        }
    }
    result
}
pub fn html(field: &Field, mut picture_url: impl FnMut(usize) -> String) -> Result<String, String> {
    fn render(
        items: &[Item],
        bytes: &[u8],
        picture_url: &mut impl FnMut(usize) -> String,
        index: &mut usize,
    ) -> Result<String, String> {
        let mut html = String::new();
        for item in items {
            let i = *index;
            *index += 1;
            match item {
                Item::Inline { text, pictures } => {
                    let Item::Text {
                        format,
                        start,
                        length,
                        styles,
                    } = text.as_ref()
                    else {
                        return Err("invalid embedded paragraph".into());
                    };
                    let mut images = Vec::new();
                    for (run, picture) in pictures {
                        let Item::Picture { width, height, .. } = picture.as_ref() else {
                            return Err("invalid embedded picture".into());
                        };
                        images.push((
                            *run,
                            format!(
                                "<IMG border=\"0\" src=\"{}\" height={height} width={width}>",
                                crate::monitor::escape(&picture_url(*index))
                            ),
                        ));
                        *index += 1;
                    }
                    let align = ["left", "right", "center", "justify"][(format & 3) as usize];
                    html.push_str(&format!(
                        "<P align={align}>\r\n{}<BR>",
                        styled_fragment_images(
                            &crate::db::decode(&bytes[*start..start + length]),
                            styles,
                            &images
                        )
                    ));
                }
                Item::Text {
                    format,
                    start,
                    length,
                    styles,
                } => {
                    let align = match format & 3 {
                        1 => " align=right",
                        2 => " align=center",
                        3 => " align=justify",
                        _ => "",
                    };
                    html.push_str(&format!(
                        "<P{align}>\r\n{}",
                        styled_fragment(&crate::db::decode(&bytes[*start..start + length]), styles)
                    ));
                    html.push_str("<BR>");
                }
                Item::Picture {
                    width,
                    height,
                    flags,
                    ..
                } => {
                    let image = format!(
                        "<IMG border=\"0\" src=\"{}\" height={height} width={width}>",
                        crate::monitor::escape(&picture_url(i))
                    );
                    if flags & 3 == 2 {
                        html.push_str(&format!("<BR><center>{image}</center><BR>"));
                    } else {
                        html.push_str(&format!("<BR>{image}<BR>"));
                    }
                }
                Item::Table { rows, children } => {
                    // Native HTML writer 0060c701: width * 92 / table width.
                    let padding = rows.first().map_or(0, |r| r.padding);
                    // Native 0060cfc0 uses the first row as the table width,
                    // and the union of all row boundaries as the column grid.
                    let width = rows.first().map_or(0, |r| {
                        r.cells.iter().map(|c| c.width as usize).sum::<usize>()
                    });
                    let mut boundaries = std::collections::BTreeSet::new();
                    for row in rows {
                        let mut end = 0usize;
                        for cell in &row.cells {
                            end += cell.width as usize;
                            boundaries.insert(end);
                        }
                    }
                    let boundaries: Vec<_> = boundaries.into_iter().collect();
                    html.push_str(&format!(
                        "</P><TABLE width=92% cellpadding={padding} rowpadding={padding}>"
                    ));
                    let mut at = 0;
                    for row in rows {
                        html.push_str("<TR>");
                        let mut end = 0usize;
                        let mut column = 0usize;
                        for (cell_index, cell) in row.cells.iter().enumerate() {
                            end += cell.width as usize;
                            let next = if cell_index + 1 == row.cells.len() {
                                boundaries.len()
                            } else {
                                boundaries.partition_point(|v| *v <= end)
                            };
                            let span = next.saturating_sub(column).max(1);
                            column = next;
                            let colspan = if span > 1 {
                                format!(" colspan={span}")
                            } else {
                                String::new()
                            };
                            let percent = if width == 0 {
                                92 / row.cells.len().max(1)
                            } else {
                                92 * cell.width as usize / width
                            };
                            let mut borders = String::new();
                            for (bit, side) in ["top", "left", "bottom", "right"].iter().enumerate()
                            {
                                let line = if cell.borders & (1 << bit) != 0 {
                                    "solid"
                                } else {
                                    "none"
                                };
                                borders.push_str(&format!("border-{side}: 1px {line} black; "));
                            }
                            html.push_str(&format!(
                                "<TD width={percent}%{colspan} valign=top style=\"{borders}\">"
                            ));
                            html.push_str(&render(
                                &children[at..at + cell.count],
                                bytes,
                                picture_url,
                                index,
                            )?);
                            if cell.count == 0 {
                                html.push_str("&nbsp;");
                            }
                            html.push_str("</TD>");
                            at += cell.count;
                        }
                        html.push_str("</TR>");
                    }
                    html.push_str("</TABLE>");
                }
            }
        }
        Ok(html)
    }
    render(
        &items(field)?,
        document::bytes(field)?,
        &mut picture_url,
        &mut 0,
    )
}
/// Embedded WMF bitmap records are emitted as BMP without running GDI or an
/// external converter. Vector-only metafiles require a separate renderer.
pub fn picture(item: &Item) -> Result<(&'static str, Vec<u8>), String> {
    let Item::Picture { kind, data, .. } = item else {
        return Err("object is not a picture".into());
    };
    if data.starts_with(b"GIF8") {
        return Ok(("image/gif", data.clone()));
    }
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok(("image/png", data.clone()));
    }
    if data.starts_with(b"\xff\xd8\xff") {
        return Ok(("image/jpeg", data.clone()));
    }
    if data.starts_with(b"BM") {
        return Ok(("image/bmp", data.clone()));
    }
    if *kind != 0 || data.len() < 18 {
        return Err(format!("picture encoding {kind} is not implemented"));
    }
    let u16at = |p| u16::from_le_bytes(data[p..p + 2].try_into().unwrap());
    let u32at = |p| u32::from_le_bytes(data[p..p + 4].try_into().unwrap());
    if !matches!(u16at(0), 1 | 2) || u16at(2) != 9 || u32at(6) as usize * 2 != data.len() {
        return Err("invalid WMF header".into());
    }
    let mut at = 18;
    let mut dib = None;
    while at + 6 <= data.len() {
        let size = (u32at(at) as usize)
            .checked_mul(2)
            .ok_or("WMF size overflow")?;
        if size < 6 || at.checked_add(size).filter(|n| *n <= data.len()).is_none() {
            return Err("invalid WMF record".into());
        }
        let opcode = u16at(at + 4);
        let skip = match opcode {
            0x0b41 => Some(26),
            0x0f43 => Some(28),
            _ => None,
        };
        if let Some(skip) = skip {
            if dib.is_some() {
                return Err("multiple WMF bitmaps require compositing".into());
            }
            dib = Some(
                data.get(at + skip..at + size)
                    .ok_or("truncated WMF bitmap")?,
            );
        }
        at += size;
    }
    if at != data.len() {
        return Err("trailing WMF bytes".into());
    }
    let d = dib.ok_or("vector WMF renderer not implemented")?;
    if d.len() < 40 || u32::from_le_bytes(d[0..4].try_into().unwrap()) != 40 {
        return Err("unsupported DIB header".into());
    }
    let bits = u16::from_le_bytes(d[14..16].try_into().unwrap());
    let compression = u32::from_le_bytes(d[16..20].try_into().unwrap());
    if compression != 0 || !matches!(bits, 1 | 4 | 8 | 16 | 24 | 32) {
        return Err("unsupported DIB compression/depth".into());
    }
    let colors = u32::from_le_bytes(d[32..36].try_into().unwrap()) as usize;
    let colors = if colors == 0 && bits <= 8 {
        1usize << bits
    } else {
        colors
    };
    let pixel_offset = 40usize
        .checked_add(colors.checked_mul(4).ok_or("palette overflow")?)
        .ok_or("palette overflow")?;
    if pixel_offset > d.len() {
        return Err("truncated bitmap palette".into());
    }
    let mut out = b"BM".to_vec();
    out.extend(((d.len() + 14) as u32).to_le_bytes());
    out.extend([0; 4]);
    out.extend(((pixel_offset + 14) as u32).to_le_bytes());
    out.extend(d);
    Ok(("image/bmp", out))
}
