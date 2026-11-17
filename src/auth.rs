//! Read-only local Basic authentication recovered from users.dbs/CheckUser.
//! Authorization is passed only to the transient legacy KS request, never logs.
use crate::db::{
    self,
    fields::{Record, Value},
    ConfigValue, Db4Engine, VirtualDir,
};

#[derive(Clone, Debug)]
pub struct User {
    pub login: String,
    pub oid: u32,
    pub full_name: String,
    pub groups: Vec<u32>,
}
pub enum Access {
    Anonymous,
    Authenticated(User),
    Denied,
}
pub fn config_text(value: Option<&ConfigValue>) -> Result<Option<String>, String> {
    match value {
        None => Ok(None),
        Some(ConfigValue::Text(s)) => Ok(Some(s.clone())),
        Some(ConfigValue::Integer(n)) => Ok(Some(n.to_string())),
        Some(ConfigValue::Invalid(s)) => Err(s.clone()),
    }
}
fn basic(header: &str) -> Option<(String, String)> {
    let (scheme, text) = header.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") || text.len() > 4096 {
        return None;
    }
    let text = text.trim();
    let padding = text.bytes().rev().take_while(|b| *b == b'=').count();
    if text.len() % 4 != 0 || padding > 2 {
        return None;
    }
    let mut bytes = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    let mut padded = false;
    for c in text.trim().bytes() {
        if c == b'=' {
            padded = true;
            continue;
        }
        if padded {
            return None;
        }
        let n = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | n as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((acc >> bits) as u8);
        }
    }
    if bits >= 6 || (bits > 0 && acc & ((1 << bits) - 1) != 0) {
        return None;
    }
    let text = String::from_utf8(bytes.clone()).unwrap_or_else(|_| db::decode(&bytes));
    let (login, password) = text.split_once(':')?;
    if login.trim().is_empty() || login.chars().any(char::is_control) {
        return None;
    }
    Some((login.into(), password.into()))
}
fn allowed(record: &Record, password: &str, acl: &[u32], now: f64) -> Result<bool, String> {
    let Some(stored) = record.string(3) else {
        return Ok(false);
    };
    let (supplied, _, invalid) = encoding_rs::WINDOWS_1251.encode(password);
    if invalid {
        return Ok(false);
    }
    let mut different = stored.len() ^ supplied.len();
    for (i, byte) in stored.iter().enumerate() {
        different |= (byte ^ supplied.get(i).copied().unwrap_or(0)) as usize;
    }
    if different != 0 {
        return Ok(false);
    }
    if record.string(9).is_some_and(|s| s.contains(&b'H')) {
        return Ok(false);
    }
    if let Some(field) = record
        .fields
        .iter()
        .find(|f| f.id == 11 && f.wire_type == 1)
    {
        let Value::Date(date) = &field.value else {
            return Err("Invalid user expiration field".into());
        };
        let expiration = crate::ks::date::Date::from_parts([
            i16::from_le_bytes([date[0], date[1]]) as i64,
            date[2] as i64 - 1,
            date[3] as i64,
            0,
            0,
            0,
            0,
        ])?;
        if expiration.millis < now {
            return Ok(false);
        }
    }
    Ok(record
        .fields
        .iter()
        .any(|f| matches!((&f.value,f.id,f.wire_type),(Value::Integer(g),6,5) if acl.contains(g))))
}
pub fn check(
    engine: &Db4Engine,
    dir: &VirtualDir,
    authorization: Option<&str>,
) -> Result<Access, String> {
    let Some(access) = config_text(dir.auth.get("Access"))?.or_else(|| dir.access.clone()) else {
        return Ok(Access::Anonymous);
    };
    if config_text(engine.web_config.get("AuthScript"))?.as_deref() != Some("users.dbs") {
        return Err("Configured authentication script is not supported".into());
    }
    let kind = config_text(dir.auth.get("Type"))?.unwrap_or_default();
    if !matches!(kind.as_str(), "" | "0" | "1") {
        return Err("External/NTLM authentication is not implemented".into());
    }
    if config_text(dir.auth.get("demo"))?.is_some_and(|s| !s.is_empty()) {
        return Err("Demo authentication mode is not implemented".into());
    }
    let Some((login, password)) = authorization.and_then(basic) else {
        return Ok(Access::Denied);
    };
    let acl = access
        .split(';')
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<u32>()
                .map_err(|_| "Invalid authentication ACL".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let root = engine
        .volumes
        .first()
        .and_then(|v| v.path.parent())
        .ok_or("data root missing")?;
    let path = root.join("ubase.db4");
    let mut tree = db::mainkey::MainKey::open(&path)?;
    let ids = tree.text_candidates(1200, 1, |key| db::decode(key).eq_ignore_ascii_case(&login))?;
    let now = crate::ks::date::Date::now()?.millis;
    for oid in ids {
        let mut r = db::read_file_oid(&path, oid)?.ok_or("User index points to missing record")?;
        if r.area != 1200 {
            return Err("User index area mismatch".into());
        }
        if !r
            .string(1)
            .is_some_and(|name| db::decode(name).eq_ignore_ascii_case(&login))
        {
            return Err("User index value mismatch".into());
        }
        // Native linked-field traversal skips deleted/missing targets.
        // A dangling group link must never grant access through a stale ACL.
        let mut valid_groups = std::collections::HashSet::new();
        for field in r.fields.iter() {
            if let (Value::Integer(group), 6, 5) = (&field.value, field.id, field.wire_type) {
                if db::read_file_oid(&path, *group)?.is_some() {
                    valid_groups.insert(*group);
                }
            }
        }
        std::sync::Arc::make_mut(&mut r.fields).retain(|f| !matches!((&f.value, f.id, f.wire_type), (Value::Integer(g),6,5) if !valid_groups.contains(g)));
        if allowed(&r, &password, &acl, now)? {
            let groups = r
                .fields
                .iter()
                .filter_map(|f| match (&f.value, f.id, f.wire_type) {
                    (Value::Integer(g), 6, 5) => Some(*g),
                    _ => None,
                })
                .collect();
            return Ok(Access::Authenticated(User {
                login,
                oid,
                groups,
                full_name: r.string(8).map(db::decode).unwrap_or_default(),
            }));
        }
    }
    Ok(Access::Denied)
}
