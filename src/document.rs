//! Explicit typed attribute extraction. No assumption that attribute 3 is RTF or KS.
use crate::db::fields::{Field, Record, Value};

pub fn bytes(field: &Field) -> Result<&[u8], String> {
    match &field.value {
        Value::String(v)
        | Value::Text(v)
        | Value::TextWithMetadata { data: v, .. }
        | Value::Binary { data: v, .. } => Ok(v),
        Value::Integer(_) | Value::Date(_) | Value::Timestamp(_) | Value::Decimal(_) => {
            Err("integer attributes have no raw document payload".into())
        }
    }
}
pub fn metadata(field: &Field) -> &[u8] {
    match &field.value {
        Value::TextWithMetadata { metadata, .. } => metadata,
        _ => &[],
    }
}
/// Signature recognition only, not RTF validation or conversion of internal rich text.
pub fn has_rtf_header(data: &[u8]) -> bool {
    if !data.starts_with(b"{\\rtf") {
        return false;
    }
    let rest = &data[5..];
    let n = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    n > 0
        && rest
            .get(n)
            .map(|b| b.is_ascii_whitespace() || *b == b'\\' || *b == b'{')
            .unwrap_or(false)
}
pub fn select(record: &Record, id: u16, wire: u16) -> Result<&Field, String> {
    let mut matches = record
        .fields
        .iter()
        .filter(|f| f.id == id && f.wire_type == wire);
    let field = matches
        .next()
        .ok_or_else(|| format!("attribute {id}, wire type {wire} not found"))?;
    if matches.next().is_some() {
        return Err(format!(
            "ambiguous attribute {id}, wire type {wire}; refusing to choose an occurrence"
        ));
    }
    Ok(field)
}
pub fn export<'a>(
    record: &'a Record,
    id: u16,
    wire: u16,
    format: &str,
) -> Result<&'a [u8], String> {
    let field = select(record, id, wire)?;
    if format == "metadata" {
        return match &field.value {
            Value::Text(_) | Value::TextWithMetadata { .. } => Ok(metadata(field)),
            _ => Err("metadata export requires a text attribute (wire type 4)".into()),
        };
    }
    let data = bytes(field)?;
    match format {
        "raw" => Ok(data),
        "rtf" => {
            if matches!(field.value, Value::Binary { kind: Some(3), .. }) {
                return Err("compiled KS is not an RTF document".into());
            }
            if !has_rtf_header(data) {
                return Err(
                    "attribute has no RTF header; internal rich-text conversion is not implemented"
                        .into(),
                );
            }
            if !metadata(field).is_empty() {
                return Err("text has additional formatting metadata; export raw and metadata separately until native RTF conversion is implemented".into());
            }
            Ok(data)
        }
        _ => Err("format must be raw, metadata or rtf".into()),
    }
}
/// HTTP resource selection must not leak compiled scripts or pick duplicate fields.
pub fn resource_bytes(record: &Record, id: u16) -> Result<Option<&[u8]>, String> {
    let mut fields = record
        .fields
        .iter()
        .filter(|f| f.id == id && matches!(f.wire_type, 8 | 14));
    let Some(field) = fields.next() else {
        return Ok(None);
    };
    if fields.next().is_some() {
        return Err(format!("ambiguous binary attribute {id}"));
    }
    if matches!(field.value, Value::Binary { kind: Some(3), .. }) {
        return Err("compiled KS cannot be served as a document resource".into());
    }
    bytes(field).map(Some)
}
pub fn describe(record: &Record) -> String {
    let mut out = format!(
        "area={} flags={} attributes={}\nID\tWIRE\tKIND\tBYTES\tTEXT_METADATA\tRTF_HEADER\n",
        record.area,
        record.flags,
        record.fields.len()
    );
    for field in record.fields.iter() {
        let (kind, len) = match &field.value {
            Value::Decimal(_) => ("decimal-opaque".into(), 34),
            Value::Timestamp(_) => ("timestamp".into(), 8),
            Value::Date(_) => ("date".into(), 4),
            Value::Integer(_) => ("integer".into(), 4),
            Value::String(v) => (
                if field.wire_type == 3 {
                    "number-text"
                } else {
                    "string"
                }
                .into(),
                v.len(),
            ),
            Value::Text(v) | Value::TextWithMetadata { data: v, .. } => ("text".into(), v.len()),
            Value::Binary { data, kind, .. } => (format!("binary:{kind:?}"), data.len()),
        };
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            field.id,
            field.wire_type,
            kind,
            len,
            metadata(field).len(),
            bytes(field).map(has_rtf_header).unwrap_or(false)
        ));
    }
    out
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
/// Explicit opt-in plain view, not an implementation of the original KS document form.
pub fn plain_html(record: &Record) -> Result<String, String> {
    if record.area == 0x385 || record.code().is_some() {
        return Err("script objects are not plain documents".into());
    }
    let field = select(record, 3, 4)?;
    let data = bytes(field)?;
    if has_rtf_header(data) {
        return Err("plain view of literal RTF requires an RTF renderer".into());
    }
    // 00/01 surround spans in the supplied internal text; visual styling is not inferred.
    let text = crate::db::decode(data)
        .chars()
        .filter(|c| *c == '\n' || *c == '\r' || *c == '\t' || !c.is_control())
        .collect::<String>();
    let title = record
        .string(1)
        .map(crate::db::decode)
        .unwrap_or_else(|| "Документ".into());
    let oid = record.integer(0x400).ok_or("document OID missing")?;
    Ok(format!("<!doctype html><html lang=ru><meta charset=utf-8><title>{}</title><style>body{{max-width:1000px;margin:2rem auto;padding:0 1rem;font:18px Georgia,serif}}aside{{padding:1rem;background:#fff3cd;font:14px sans-serif}}pre{{white-space:pre-wrap;overflow-wrap:anywhere;font:inherit;line-height:1.5}}</style><h1>{}</h1><aside>Упрощённый просмотр Rust. OID: {oid}; область: {}. Оформление оригинала не восстановлено; служебные управляющие символы скрыты. Метаданные форматирования: {} байт. Это не оригинальная форма АИС.</aside><pre>{}</pre></html>",escape(&title),escape(&title),record.area,metadata(field).len(),escape(&text)))
}
