//! Bounded form decoding for the original homepage's URL-encoded/multipart POSTs.
const MAX_FIELDS: usize = 16_384;
fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap_or_else(|_| crate::db::decode(bytes))
}
fn add(
    out: &mut Vec<(String, String)>,
    key: String,
    value: String,
    command: &str,
) -> Result<(), String> {
    if out.len() >= MAX_FIELDS
        || key.is_empty()
        || key.len() > 256
        || key.chars().any(char::is_control)
        || value.len() > 65536
    {
        return Err("form field limit/invalid name".into());
    }
    // The blocking editor uses independent rows, not a semicolon-delimited value.
    if command == "RaspredSettingsBlockedInnForJudges"
        && matches!(key.as_str(), "nm__judge_inn" | "nm__judge_orgname")
    {
        out.push((key, value));
        return Ok(());
    }
    if let Some((_, previous)) = out.iter_mut().find(|(k, _)| *k == key) {
        // Native multi-selection controls submit several values of one field.
        // Keep command/control parameters singular.
        let attribute = key.strip_prefix('a').and_then(|s| s.split_once("attr"));
        let category = command == "raspredkategory"
            && key.parse::<u32>().is_ok_and(|n| n != 0)
            && key.bytes().all(|c| c.is_ascii_digit())
            && value.parse::<u32>().is_ok_and(|n| n != 0)
            && previous
                .split(';')
                .all(|v| v.parse::<u32>().is_ok_and(|n| n != 0));
        let multi = category
            || key == "tipdoc"
            || key == "prevdoc"
                && value.parse::<u32>().is_ok_and(|n| n != 0)
                && previous
                    .split(';')
                    .all(|s| s.parse::<u32>().is_ok_and(|n| n != 0))
            || key == "nm_column" && value.bytes().all(|b| b.is_ascii_digit()) && !value.is_empty()
            || attribute.is_some_and(|(area, field)| {
                !area.is_empty()
                    && area.bytes().all(|c| c.is_ascii_digit())
                    && (field == "oid"
                        || !field.is_empty() && field.bytes().all(|c| c.is_ascii_digit()))
            });
        if !multi {
            return Err("repeated control parameter".into());
        }
        if previous.len() + value.len() + 1 > 65536 {
            return Err("form field value limit".into());
        }
        previous.push(';');
        previous.push_str(&value);
        return Ok(());
    }
    out.push((key, value));
    Ok(())
}
pub fn decode(content_type: &str, bytes: &[u8]) -> Result<Vec<(String, String)>, String> {
    decode_for("", content_type, bytes)
}
pub fn decode_for(
    target: &str,
    content_type: &str,
    bytes: &[u8],
) -> Result<Vec<(String, String)>, String> {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let command = if query.is_empty() {
        path.rsplit('/').next().unwrap_or("")
    } else {
        query.split('&').next().unwrap_or("")
    };
    let command = crate::server::percent_decode(command)?;
    if bytes.len() > 1024 * 1024 {
        return Err("form body limit".into());
    }
    let kind = content_type.split(';').next().unwrap_or("").trim();
    let mut out = Vec::new();
    if kind.eq_ignore_ascii_case("application/x-www-form-urlencoded") {
        for pair in text(bytes).split('&').filter(|s| !s.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            add(
                &mut out,
                crate::server::percent_decode(&k.replace('+', " "))?,
                crate::server::percent_decode(&v.replace('+', " "))?,
                &command,
            )?;
        }
    } else if kind.eq_ignore_ascii_case("multipart/form-data") {
        let boundary = content_type
            .split(';')
            .skip(1)
            .find_map(|p| p.trim().strip_prefix("boundary="))
            .ok_or("multipart boundary missing")?
            .trim_matches('"');
        if boundary.is_empty()
            || boundary.len() > 70
            || !boundary
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"'()+_,-./:=?".contains(&b))
        {
            return Err("invalid multipart boundary".into());
        }
        let body = text(bytes);
        let marker = format!("--{boundary}");
        let mut rest = body
            .strip_prefix(&format!("{marker}\r\n"))
            .ok_or("multipart opening boundary missing")?;
        loop {
            let (headers, part) = rest
                .split_once("\r\n\r\n")
                .ok_or("multipart headers missing")?;
            if headers.len() > 4096 {
                return Err("multipart header limit".into());
            }
            let disposition = headers
                .split("\r\n")
                .find_map(|h| {
                    h.split_once(':')
                        .filter(|(k, _)| k.eq_ignore_ascii_case("content-disposition"))
                        .map(|(_, v)| v.trim())
                })
                .ok_or("multipart disposition missing")?;
            if !disposition.starts_with("form-data;") || disposition.contains("filename=") {
                return Err("file uploads are not implemented".into());
            }
            let name = disposition
                .split(';')
                .skip(1)
                .find_map(|p| p.trim().strip_prefix("name="))
                .and_then(|s| s.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
                .ok_or("multipart field name missing")?;
            let (value, next) = part
                .split_once(&format!("\r\n{marker}"))
                .ok_or("multipart closing boundary missing")?;
            add(&mut out, name.into(), value.into(), &command)?;
            if next == "--" || next == "--\r\n" {
                break;
            }
            rest = next
                .strip_prefix("\r\n")
                .ok_or("invalid multipart boundary suffix")?;
        }
    } else {
        return Err("POST content type is not implemented".into());
    }
    Ok(out)
}
