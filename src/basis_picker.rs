//! Display-only details for the document-basis classifier. Native OIDs and
//! shortname attributes remain byte-for-byte unchanged for the popup callback.
use crate::{
    db::{
        decode,
        fields::{Record, Value},
    },
    ks::runtime::Runtime,
    server::RequestContext,
};
use std::collections::HashMap;

fn parameter(request: &RequestContext, name: &str) -> Option<String> {
    if let Some((_, value)) = request.form.iter().find(|(key, _)| key == name) {
        return Some(value.clone());
    }
    request
        .target
        .split_once('?')?
        .1
        .split('&')
        .filter_map(|p| p.split_once('='))
        .find_map(|(key, value)| {
            (key == name)
                .then(|| crate::server::percent_decode(value).ok())
                .flatten()
        })
}
fn applies(request: &RequestContext) -> bool {
    parameter(request, "area").as_deref() == Some("7341")
        && parameter(request, "attr").as_deref() == Some("24")
        && parameter(request, "nattr").is_none_or(|v| v.is_empty() || v == "1")
        && !parameter(request, "freturn").is_some_and(|v| v.eq_ignore_ascii_case("true"))
        && ["base", "requesttext", "sr", "ulist"]
            .iter()
            .all(|key| parameter(request, key).is_none_or(|v| v.is_empty()))
}
fn scalar(record: &Record, id: u16) -> Option<String> {
    record
        .string(id)
        .map(decode)
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            record
                .integer(id)
                .filter(|v| *v != 0)
                .map(|v| v.to_string())
        })
}
fn registration_date(record: &Record) -> Option<String> {
    record.fields.iter().find_map(|field| match &field.value {
        Value::Date(d) if field.id == 5 && d[2] != 0 && d[3] != 0 => Some(format!(
            "{:02}.{:02}.{:04}",
            d[3],
            d[2],
            i16::from_le_bytes([d[0], d[1]])
        )),
        _ => None,
    })
}
fn details(record: &Record, oid: u32, kind: Option<&str>) -> String {
    let mut parts = vec![kind
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("Тип не указан")
        .to_owned()];
    parts.push(format!(
        "регистрация: {}",
        registration_date(record).as_deref().unwrap_or("не указана")
    ));
    let identifier = scalar(record, 73);
    let incoming = scalar(record, 79);
    if let Some(mut number) = identifier.as_ref().cloned() {
        if let Some(year) = scalar(record, 74) {
            number.push_str(&format!("/{year}"));
        }
        if let Some(revision) = scalar(record, 75) {
            number.push_str(&format!(" (ред. {revision})"));
        }
        parts.push(format!("идентификатор: {number}"));
    }
    if let Some(number) = &incoming {
        parts.push(format!("входящий № {number}"));
    }
    if identifier.is_none() && incoming.is_none() {
        parts.push(
            scalar(record, 17)
                .map(|n| format!("номер документа: {n}"))
                .unwrap_or_else(|| format!("внутренний № {oid}")),
        );
    }
    parts.join(" · ")
}
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn enrich(
    html: &str,
    mut read: impl FnMut(u32) -> Result<Option<Record>, String>,
) -> Result<String, String> {
    // Only literal result cells; numeric identity excludes JavaScript templates.
    let cell = regex::Regex::new(r#"(<td\b[^>]*\bclass="value[^"<>]*"[^>]*\bvalue="([0-9]+)"[^>]*\bshortname="[^"<>]*"[^>]*>)([^<]*)(</td>)"#).unwrap();
    let mut labels = HashMap::<u32, Option<String>>::new();
    let mut types = HashMap::<u32, Option<String>>::new();
    let mut out = String::with_capacity(html.len());
    let mut end = 0;
    for c in cell.captures_iter(html) {
        crate::execution::check()?;
        let span = c.get(0).unwrap();
        out.push_str(&html[end..span.start()]);
        let oid = c[2].parse::<u32>().map_err(|e| e.to_string())?;
        if !labels.contains_key(&oid) {
            let label = if let Some(record) = read(oid)?.filter(|r| r.area == 7341) {
                let kind = if let Some(id) = record.integer(9).filter(|n| *n != 0) {
                    if !types.contains_key(&id) {
                        types.insert(id, read(id)?.and_then(|r| r.string(1).map(decode)));
                    }
                    types[&id].as_deref()
                } else {
                    None
                };
                Some(details(&record, oid, kind))
            } else {
                None
            };
            labels.insert(oid, label);
        }
        if let Some(label) = &labels[&oid] {
            // Do not put display details into value or shortname: native JS
            // returns these to a24; only the visible cell text is expanded.
            out.push_str(&c[1]);
            out.push_str(&c[3]);
            out.push_str(" — ");
            out.push_str(&escape(label));
            out.push_str(&c[4]);
        } else {
            out.push_str(span.as_str());
        }
        end = span.end();
    }
    out.push_str(&html[end..]);
    Ok(out)
}
pub(super) fn decorate(
    body: Vec<u8>,
    runtime: &Runtime,
    request: &RequestContext,
) -> Result<Vec<u8>, String> {
    if !applies(request) {
        return Ok(body);
    }
    let html = enrich(&decode(&body), |oid| runtime.read_loaded_document(oid))?;
    Ok(encoding_rs::WINDOWS_1251.encode(&html).0.into_owned())
}
