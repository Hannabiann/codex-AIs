//! Read-only view of the active config.db4 Web registry branch.
use super::{decode, fields::Record};
use std::collections::{BTreeMap, HashSet};

/// Registry strings are stored as consecutive 255-byte attribute-4 chunks.
/// Unlike an ordinary multivalued document field, QueryValue joins them.
pub(super) fn string_value(record: &Record) -> Option<String> {
    let mut chunks = record.fields.iter().filter_map(|f| match &f.value {
        super::fields::Value::String(bytes) if f.id == 4 && f.wire_type == 2 => {
            Some(bytes.as_slice())
        }
        _ => None,
    });
    let mut value = chunks.next()?.to_vec();
    for chunk in chunks {
        value.extend_from_slice(chunk);
    }
    Some(decode(&value))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub kind: String,
    pub value: String,
}
#[derive(Debug, Clone)]
pub struct Key {
    pub name: String,
    pub children: Vec<u32>,
    pub values: Vec<Entry>,
}
#[derive(Debug, Clone, Default)]
pub struct Registry {
    pub root: u32,
    keys: BTreeMap<u32, Key>,
}
impl Registry {
    pub fn from_records(records: &[Record], root: u32) -> Result<Self, String> {
        // Native QueryValue (0072ad50) intersects name and parent, then reads
        // result index zero (0072ae75). DB4 result OIDs are ascending.
        let mut records: Vec<_> = records.iter().collect();
        records.sort_by_key(|r| r.integer(0x400));
        let mut result = Self {
            root,
            ..Default::default()
        };
        let mut pending = vec![root];
        let mut seen = HashSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                return Err("registry key cycle or duplicate".into());
            }
            if seen.len() > 10000 {
                return Err("registry key limit".into());
            }
            let source = records
                .iter()
                .find(|r| r.integer(0x400) == Some(id))
                .ok_or("registry key absent")?;
            let name = source
                .string(1)
                .map(decode)
                .ok_or("registry key name missing")?;
            let mut key = Key {
                name,
                children: Vec::new(),
                values: Vec::new(),
            };
            for r in records.iter().filter(|r| r.integer(3) == Some(id)) {
                let name = r
                    .string(1)
                    .map(decode)
                    .ok_or("registry child name missing")?;
                let value = string_value(r);
                if let Some(value) = value {
                    key.values.push(Entry {
                        name,
                        kind: r
                            .string(5)
                            .map(decode)
                            .unwrap_or_else(|| "<missing>".into()),
                        value,
                    });
                } else {
                    let child = r.integer(0x400).ok_or("registry child OID missing")?;
                    key.children.push(child);
                    pending.push(child);
                }
            }
            result.keys.insert(id, key);
        }
        Ok(result)
    }
    pub fn key(&self, id: u32) -> Result<&Key, String> {
        self.keys
            .get(&id)
            .ok_or_else(|| "invalid registry key".into())
    }
    // Native OpenKey/CreateKey resolve from the database root, not the receiver.
    // Only the active Web subtree is exposed; other branches are not synthesized.
    pub fn open(&self, path: &str) -> Result<Option<u32>, String> {
        if path.len() > 4096 || path.contains('\0') {
            return Err("registry path limit/invalid path".into());
        }
        let parts: Vec<_> = path.split('\\').filter(|p| !p.is_empty()).collect();
        if parts.len() < 3
            || !parts[0].eq_ignore_ascii_case("SOFTWARE")
            || !parts[1].eq_ignore_ascii_case("Kodeks")
            || !parts[2].eq_ignore_ascii_case("Web")
        {
            return Err("registry access outside SOFTWARE\\Kodeks\\Web is not implemented".into());
        }
        let mut id = self.root;
        for part in &parts[3..] {
            if *part == "." || *part == ".." {
                return Err("invalid registry path component".into());
            }
            let mut next = None;
            for child in &self.key(id)?.children {
                if self.key(*child)?.name == *part {
                    if next.is_some() {
                        return Err(format!("ambiguous registry key: {part}"));
                    }
                    next = Some(*child);
                }
            }
            let Some(found) = next else {
                return Ok(None);
            };
            id = found;
        }
        Ok(Some(id))
    }
    pub fn query(&self, id: u32, name: &str) -> Result<Option<&Entry>, String> {
        // Native QueryValue resolves names case-insensitively. For example,
        // ~brasExchangeTasks asks for restarttime, stored as restartTime.
        // Preserve ascending-OID precedence when differently cased names repeat.
        let name = name.to_lowercase();
        Ok(self
            .key(id)?
            .values
            .iter()
            .find(|e| e.name.to_lowercase() == name))
    }
}
