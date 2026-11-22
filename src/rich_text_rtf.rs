//! RTF export of decoded native text objects. Unsupported structures fail before
//! output publication; they are never silently flattened or omitted.
use super::{items, Item};
use crate::{db::fields::Field, document};
use std::fmt::Write;

fn text(out: &mut String, bytes: &[u8], styles: &[u32]) {
    let mut styles = styles.iter();
    let mut depth = 0;
    for &b in bytes {
        match b {
            0 => {
                out.push('{');
                depth += 1;
                let flags = styles.next().copied().unwrap_or(1);
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
}
fn render(out: &mut String, parts: &[Item], bytes: &[u8], in_table: bool) -> Result<(), String> {
    for part in parts {
        match part {
            Item::Text {
                format,
                start,
                length,
                styles,
            } => {
                let align = ["ql", "qr", "qc", "qj"][(format & 3) as usize];
                write!(
                    out,
                    "\\pard\\plain\\f0\\fs18\\{align}{} ",
                    if in_table { "\\intbl" } else { "" }
                )
                .unwrap();
                text(
                    out,
                    bytes.get(*start..start + length).ok_or("RTF text range")?,
                    styles,
                );
                out.push_str("\\par\n");
            }
            Item::Table { rows, children } => {
                if in_table {
                    return Err("RTF export of nested native tables is not implemented".into());
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
                    write!(out, "\\trowd\\trgaph{}", row.padding as u32 * 15).unwrap();
                    let mut end = 0u64;
                    for cell in &row.cells {
                        end += cell.width as u64;
                        for (bit, side) in ["t", "l", "b", "r"].iter().enumerate() {
                            if cell.borders & (1 << bit) != 0 {
                                write!(out, "\\clbrdr{side}\\brdrs\\brdrw10").unwrap();
                            }
                        }
                        write!(out, "\\cellx{}", end * 10375 / width).unwrap();
                    }
                    out.push('\n');
                    for cell in &row.cells {
                        let slice = children
                            .get(at..at + cell.count)
                            .ok_or("RTF table children mismatch")?;
                        if slice.is_empty() {
                            out.push_str("\\pard\\intbl ");
                        }
                        render(out, slice, bytes, true)?;
                        out.push_str("\\cell\n");
                        at += cell.count;
                    }
                    out.push_str("\\row\n");
                }
                if at != children.len() {
                    return Err("RTF table has unassigned children".into());
                }
            }
            Item::Picture {
                width,
                height,
                flags,
                kind,
                data,
            } => {
                let (encoding, payload) = if data.starts_with(b"\x89PNG\r\n\x1a\n") {
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
                write!(out,"\\pard{}{} {{\\pict\\{encoding}\\picw{width}\\pich{height}\\picwgoal{}\\pichgoal{} ",
                    if in_table {"\\intbl"} else {""},if flags & 3 == 2 {"\\qc"} else {"\\ql"},*width as u32*15,*height as u32*15).unwrap();
                for b in payload {
                    write!(out, "{b:02x}").unwrap();
                }
                out.push_str("}\\par\n");
            }
        }
        if out.len() > 64 * 1024 * 1024 {
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
    let mut out = String::from("{\\rtf1\\ansi\\ansicpg1251\\deff0{\\fonttbl{\\f0\\fnil Arial;}}\n");
    out.push_str(page);
    out.push('\n');
    render(&mut out, &items(field)?, bytes, false)?;
    out.push('}');
    Ok(out.into_bytes())
}
