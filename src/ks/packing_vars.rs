//! Native navigation parameters: a mutable map with a computed toString property.
use super::{Fault, Value};
use std::{cell::RefCell, collections::HashMap, rc::Rc};
pub(super) fn escape(value: &str) -> Result<String, Fault> {
    let (bytes, _, bad) = encoding_rs::WINDOWS_1251.encode(value);
    if bad {
        return Err("URL value cannot be encoded as CP1251".into());
    }
    Ok(bytes
        .iter()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"@*_-./".contains(b) {
                (*b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect())
}
pub(super) fn construct(values: &[Value]) -> Result<Value, Fault> {
    let [value] = values else {
        return Err("PackingVars expects an object or encoded parameters".into());
    };
    let mut fields = HashMap::new();
    match value {
        Value::Null => {}
        Value::String(s) => {
            for pair in s.split('&').filter(|s| !s.is_empty()) {
                let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                let key = crate::server::percent_decode(&key.replace('+', " "))?;
                if key.starts_with('\0') || key == "toString" {
                    continue;
                }
                fields.insert(
                    key,
                    Value::String(crate::server::percent_decode(&value.replace('+', " "))?),
                );
            }
        }
        Value::Object(o) | Value::Array(o) | Value::ReadOnlyObject(o) => {
            for (key, value) in o.borrow().iter() {
                if !key.starts_with('\0')
                    && key != "toString"
                    && matches!(
                        value,
                        Value::Null | Value::String(_) | Value::Number(_) | Value::Bool(_)
                    )
                {
                    fields.insert(key.clone(), value.clone());
                }
            }
        }
        _ => return Err("PackingVars expects an object or encoded parameters".into()),
    }
    fields.insert("\0packing".into(), Value::Bool(true));
    Ok(Value::Object(Rc::new(RefCell::new(fields))))
}
pub(super) fn serialize(fields: &HashMap<String, Value>) -> Result<String, Fault> {
    let mut keys = fields
        .keys()
        .filter(|k| !k.starts_with('\0') && k.as_str() != "toString")
        .collect::<Vec<_>>();
    keys.sort();
    let mut text = String::new();
    for key in keys {
        let value = &fields[key];
        if matches!(value, Value::Null) {
            continue;
        }
        text.push_str(&escape(key)?);
        text.push('=');
        text.push_str(&escape(&value.text())?);
        text.push('&');
    }
    Ok(text)
}
