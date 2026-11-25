//! RTF export of decoded native text objects. Unsupported structures fail before
//! output publication; they are never silently flattened or omitted.
use super::{items, Item, Row};
use crate::{db::fields::Field, document};
use std::fmt::Write;
#[path = "rich_text_gif.rs"]
mod gif_picture;

const MAX_OUTPUT: usize = 64 * 1024 * 1024;

fn table_paragraph(out: &mut String, depth: usize) {
    if depth > 0 {
        write!(out, "\\intbl\\itap{depth}").unwrap();
    }
}

fn row_properties(out: &mut String, row: &Row, source_width: u64, available: u64) {
    write!(out, "\\trowd\\trgaph{}", row.padding as u32 * 15).unwrap();
    let mut end = 0u64;
    for cell in &row.cells {
        end += cell.width as u64;
        for (bit, side) in ["t", "l", "b", "r"].iter().enumerate() {
            if cell.borders & (1 << bit) != 0 {
                write!(out, "\\clbrdr{side}\\brdrs\\brdrw10").unwrap();
            }
        }
        write!(out, "\\cellx{}", end * available / source_width).unwrap();
    }
    out.push('\n');
}

fn text(
    out: &mut String,
    bytes: &[u8],
    styles: &[u32],
    pictures: &[(usize, std::sync::Arc<Item>)],
) -> Result<(), String> {
    let mut styles = styles.iter().enumerate();
    let mut depth = 0;
    for &b in bytes {
        match b {
            0 => {
                let (run, flags) = styles
                    .next()
                    .map(|(i, f)| (i, *f))
                    .unwrap_or((usize::MAX, 1));
                if let Some((_, embedded)) = pictures.iter().find(|(i, _)| *i == run) {
                    picture(out, embedded)?;
                    continue;
                }
                out.push('{');
                depth += 1;
                for (bit, control) in ["b", "i", "ul", "strike"].iter().enumerate() {
                    if flags & (1 << bit) != 0 {
                        write!(out, "\\{control}").unwrap();
                    }
                }
                out.push(' ');
            }
            1 if depth > 0 => {
                out.push('}');
                depth -= 1;
            }
            b'\\' | b'{' | b'}' => {
                out.push('\\');
                out.push(b as char);
            }
            b'\n' => out.push_str("\\line "),
            b'\t' => out.push_str("\\tab "),
            // Native text reserves these bytes for line drawing. Treating
            // them as CP1251 produces punctuation and Cyrillic letters.
            0x83 | 0x85..=0x87 | 0x89..=0x8f => {
                let ch = match b {
                    0x83 => '-',
                    0x8c => '|',
                    _ => '+',
                };
                write!(out, "{{\\f1 {ch}}}").unwrap();
            }
            32..=126 => out.push(b as char),
            128..=255 => {
                write!(out, "\\'{b:02x}").unwrap();
            }
            _ => {}
        }
    }
    for _ in 0..depth {
        out.push('}');
    }
    Ok(())
}
fn picture(out: &mut String, part: &Item) -> Result<(), String> {
    let Item::Picture {
        width,
        height,
        kind,
        data,
        ..
    } = part
    else {
        return Err("invalid embedded picture".into());
    };
    let converted;
    let mut pixel_width = *width;
    let mut pixel_height = *height;
    let (encoding, payload) = if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        (converted, pixel_width, pixel_height) = gif_picture::png(data)?;
        ("pngblip", converted.as_slice())
    } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        ("pngblip", data.as_slice())
    } else if data.starts_with(b"\xff\xd8\xff") {
        ("jpegblip", data.as_slice())
    } else if data.starts_with(b"BM") && data.len() >= 54 {
        ("dibitmap0", &data[14..])
    } else if *kind == 0
        && data.len() >= 18
        && matches!(u16::from_le_bytes([data[0], data[1]]), 1 | 2)
        && data[2..4] == [9, 0]
    {
        ("wmetafile8", data.as_slice())
    } else {
        return Err(format!("RTF picture format {kind} is not supported"));
    };
    if payload.len() > MAX_OUTPUT.saturating_sub(out.len()) / 2 {
        return Err("RTF output size limit".into());
    }
    write!(
        out,
        "{{\\pict\\{encoding}\\picw{pixel_width}\\pich{pixel_height}\\picwgoal{}\\pichgoal{} ",
        *width as u32 * 15,
        *height as u32 * 15
    )
    .unwrap();
    for b in payload {
        write!(out, "{b:02x}").unwrap();
    }
    out.push('}');
    Ok(())
}

fn render(
    out: &mut String,
    parts: &[Item],
    bytes: &[u8],
    depth: usize,
    available: u64,
) -> Result<(), String> {
    for part in parts {
        match part {
            Item::Inline {
                text: paragraph,
                pictures,
            } => {
                let Item::Text {
                    format,
                    start,
                    length,
                    styles,
                } = paragraph.as_ref()
                else {
                    return Err("invalid embedded paragraph".into());
                };
                let align = ["ql", "qr", "qc", "qj"][(format & 3) as usize];
                write!(out, "\\pard\\plain\\f0\\fs18\\{align}").unwrap();
                table_paragraph(out, depth);
                out.push(' ');
                text(
                    out,
                    bytes.get(*start..start + length).ok_or("RTF text range")?,
                    styles,
                    pictures,
                )?;
                out.push_str("\\par\n");
            }
            Item::Text {
                format,
                start,
                length,
                styles,
            } => {
                let align = ["ql", "qr", "qc", "qj"][(format & 3) as usize];
                write!(out, "\\pard\\plain\\f0\\fs18\\{align}",).unwrap();
                table_paragraph(out, depth);
                out.push(' ');
                text(
                    out,
                    bytes.get(*start..start + length).ok_or("RTF text range")?,
                    styles,
                    &[],
                )?;
                out.push_str("\\par\n");
            }
            Item::Table { rows, children } => {
                if depth >= 16 {
                    return Err("RTF table nesting limit".into());
                }
                let width = rows
                    .first()
                    .map(|r| r.cells.iter().map(|c| c.width as u64).sum::<u64>())
                    .unwrap_or(0);
                if width == 0 {
                    return Err("RTF table has zero width".into());
                }
                let mut at = 0;
                for row in rows {
                    // RTF nested row properties follow the cell content, in an
                    // ignorable destination. Outer rows declare them first.
                    if depth == 0 {
                        row_properties(out, row, width, available);
                    }
                    let mut nested = false;
                    for cell in &row.cells {
                        let slice = children
                            .get(at..at + cell.count)
                            .ok_or("RTF table children mismatch")?;
                        if slice.is_empty() {
                            out.push_str("\\pard");
                            table_paragraph(out, depth + 1);
                            out.push(' ');
                        }
                        nested |= slice.iter().any(|p| matches!(p, Item::Table { .. }));
                        let inner_width = (cell.width as u64 * available / width)
                            .saturating_sub(row.padding as u64 * 30)
                            .max(1);
                        render(out, slice, bytes, depth + 1, inner_width)?;
                        out.push_str(if depth == 0 {
                            "\\cell\n"
                        } else {
                            "\\nestcell\n"
                        });
                        at += cell.count;
                    }
                    if depth > 0 {
                        out.push_str("{\\*\\nesttableprops");
                        row_properties(out, row, width, available);
                        out.push_str("\\nestrow}\n");
                    } else {
                        // Nested rows replace the current row properties in
                        // Word; restore the enclosing row before ending it.
                        if nested {
                            row_properties(out, row, width, available);
                        }
                        out.push_str("\\row\n");
                    }
                }
                if at != children.len() {
                    return Err("RTF table has unassigned children".into());
                }
                if depth > 0 {
                    out.push_str("\\pard");
                    table_paragraph(out, depth);
                    out.push(' ');
                }
            }
            Item::Picture { flags, .. } => {
                out.push_str("\\pard");
                table_paragraph(out, depth);
                out.push_str(if flags & 3 == 2 { "\\qc " } else { "\\ql " });
                picture(out, part)?;
                out.push_str("\\par\n");
            }
        }
        if out.len() > MAX_OUTPUT {
            return Err("RTF output size limit".into());
        }
    }
    Ok(())
}
pub fn rtf(field: &Field, page: &str) -> Result<Vec<u8>, String> {
    let bytes = document::bytes(field)?;
    if document::has_rtf_header(bytes) {
        return Ok(bytes.to_vec());
    }
    if page.len() > 4096
        || !page
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"\\ -\r\n".contains(&c))
    {
        return Err("Invalid RTF page properties".into());
    }
    let mut out = String::from("{\\rtf1\\ansi\\ansicpg1251\\deff0{\\fonttbl{\\f0\\fnil Arial;}{\\f1\\fmodern Courier New;}}\n");
    out.push_str(page);
    out.push('\n');
    render(&mut out, &items(field)?, bytes, 0, 10375)?;
    out.push('}');
    Ok(out.into_bytes())
}
