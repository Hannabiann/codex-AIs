//! Registry updates used by an authenticated virtual-directory SaveState.
use super::*;
use crate::db::{mainkey::MainKey, registry::Entry};
use std::sync::Arc;

fn find(index: &mut MainKey, parent: u32, name: &str) -> Result<Option<Record>, String> {
    let key = text_key(name.as_bytes());
    let ids = index.text_candidates(621, 1, |s| s == key)?;
    let mut found = None;
    for oid in ids {
        let entry = index
            .lookup(oid)?
            .ok_or("registry name index points to absent object")?;
        let record = index.record(entry)?;
        if record.area == 621 && record.integer(3) == Some(parent) {
            if found.replace(record).is_some() {
                return Err("duplicate registry setting".into());
            }
        }
    }
    Ok(found)
}
pub(crate) fn read(path: &Path, parent: u32, names: &[&str]) -> Result<Vec<Entry>, String> {
    let mut index = MainKey::open(path)?;
    let mut result = Vec::new();
    for name in names {
        if let Some(record) = find(&mut index, parent, name)? {
            result.push(Entry {
                oid: record.integer(1024).ok_or("registry OID missing")?,
                name: crate::db::decode(record.string(1).ok_or("registry name missing")?),
                kind: crate::db::decode(record.string(5).ok_or("registry type missing")?),
                value: crate::db::registry::string_value(&record)
                    .ok_or("registry value missing")?,
            });
        }
    }
    Ok(result)
}
pub(crate) struct Update {
    pub name: String,
    pub expected: Option<String>,
    pub value: Option<String>,
    pub kind: String,
}
pub(crate) fn save(path: &Path, parent: u32, updates: &[Update]) -> Result<(), String> {
    // Publication generations and snapshot invalidation use canonical paths.
    let canonical = path.canonicalize().map_err(|e| e.to_string())?;
    let path = canonical.as_path();
    ensure_recovered(path)?;
    let _mutation = crate::execution::lock(&MUTATIONS)?;
    let _publication = crate::execution::Publication::enter()?;
    let started = std::time::Instant::now();
    let _lock = volume_lock(path)?;
    recover_locked(path)?;
    check_volume_budget(path)?;
    let old = Arc::new(std::fs::read(path).map_err(|e| e.to_string())?);
    let mut index = MainKey::from_image(old.clone())?;
    let mut next_oid = parent;
    let mut changes = Vec::new();
    let mut deletions = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for u in updates {
        if !seen.insert(u.name.to_ascii_lowercase())
            || u.name.is_empty()
            || !u
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || u.value.as_ref().is_some_and(|v| {
                v.len() > 255
                    || !v
                        .bytes()
                        .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b';' | b':'))
            })
            || !matches!(u.kind.as_str(), "REG_SZ" | "REG_DWORD")
        {
            return Err("invalid numeric directory setting".into());
        }
        let current = find(&mut index, parent, &u.name)?;
        let current_value = current.as_ref().and_then(crate::db::registry::string_value);
        if current_value != u.expected {
            return Err("Настройки изменились. Откройте форму заново.".into());
        }
        if current_value == u.value {
            continue;
        }
        let Some(next) = &u.value else {
            if let Some(record) = current {
                deletions.push((record.integer(1024).ok_or("registry OID missing")?, false));
            }
            continue;
        };
        let mut record = if let Some(record) = current {
            if record.string(5) != Some(u.kind.as_bytes()) {
                return Err("registry type changed".into());
            }
            record
        } else {
            loop {
                next_oid = next_oid.checked_add(1).ok_or("registry OID overflow")?;
                if index.lookup(next_oid)?.is_none() {
                    break;
                }
            }
            Record {
                area: 621,
                flags: 5,
                fields: vec![
                    Field {
                        id: 1,
                        wire_type: 2,
                        value: Value::String(u.name.as_bytes().to_vec()),
                    },
                    Field {
                        id: 4,
                        wire_type: 2,
                        value: Value::String(vec![]),
                    },
                    Field {
                        id: 5,
                        wire_type: 2,
                        value: Value::String(u.kind.as_bytes().to_vec()),
                    },
                    Field {
                        id: 3,
                        wire_type: 5,
                        value: Value::Integer(parent),
                    },
                    Field {
                        id: 1024,
                        wire_type: 5,
                        value: Value::Integer(next_oid),
                    },
                ]
                .into(),
            }
        };
        let fields = Arc::make_mut(&mut record.fields);
        if fields.iter().filter(|f| f.id == 4).count() != 1 {
            return Err("unsupported multivalue numeric setting".into());
        }
        fields.iter_mut().find(|f| f.id == 4).unwrap().value =
            Value::String(next.as_bytes().to_vec());
        changes.push(record);
    }
    if changes.is_empty() && deletions.is_empty() {
        return Ok(());
    }
    let candidate = if u16at(&old, 5) == 0x612 {
        legacy(&mut index, &old, &changes, &deletions)?
    } else {
        index_delta::candidate_from_image(path, &changes, &deletions, old.clone())?
    };
    let candidate = Arc::new(candidate);
    let mut verify = MainKey::from_image(candidate.clone())?;
    for u in updates {
        let r = find(&mut verify, parent, &u.name)?;
        if r.as_ref().and_then(crate::db::registry::string_value) != u.value
            || r.as_ref()
                .is_some_and(|r| r.string(5) != Some(u.kind.as_bytes()))
        {
            return Err("registry setting verification failed".into());
        }
    }
    publish_checked(path, old, candidate, started)
}
fn legacy(
    index: &mut MainKey,
    old: &[u8],
    changes: &[Record],
    deletions: &[(u32, bool)],
) -> Result<Vec<u8>, String> {
    let (header, mut directory) = index.writer_parts();
    let mut image = Image::for_version(old.to_vec(), 0x612)?;
    let mut main = index.writer_tree(65535, 0, Some(10))?;
    let mut indexes = Vec::new();
    for attr in [1, 3, 4, 5] {
        indexes.push((
            attr,
            index.writer_tree(621, attr, if attr == 3 { Some(8) } else { None })?,
        ));
    }
    for oid in changes
        .iter()
        .map(|r| r.integer(1024).ok_or("setting OID missing"))
        .chain(deletions.iter().map(|(oid, _)| Ok(*oid)))
    {
        let oid = oid?;
        if let Some(entry) = index.lookup(oid)? {
            let previous = index.record(entry)?;
            let replacement = changes.iter().find(|r| r.integer(1024) == Some(oid));
            for f in previous.fields.iter() {
                // Unchanged name/type fields keep their existing postings.
                // Only removal or replacement of a string requires this check.
                if replacement.is_some_and(|r| {
                    r.fields.iter().any(|next| {
                        next.id == f.id && next.wire_type == f.wire_type && next.value == f.value
                    })
                }) {
                    continue;
                }
                if let Value::String(s) = &f.value {
                    // Numeric values may be empty or a list of identifiers.
                    // Separators are not part of full-text number tokens.
                    let words = if f.id == 4
                        && s.iter().all(|b| b.is_ascii_digit() || b".;:".contains(b))
                    {
                        s.split(|b| !b.is_ascii_digit())
                            .filter(|w| !w.is_empty())
                            .collect::<Vec<_>>()
                    } else {
                        vec![s.as_slice()]
                    };
                    for word in words {
                        if index
                            .word_candidates(&text_key(word), false, None)
                            .map_err(|e| {
                                format!("registry OID {oid} field {} full-text check: {e}", f.id)
                            })?
                            .contains(&oid)
                        {
                            return Err("full-text indexed registry setting is unsupported".into());
                        }
                    }
                }
            }
            image.release_frame(entry.block, b'C')?;
        }
        main.items.retain(|k| u32at(k, 0) != oid);
        for (attr, tree) in &mut indexes {
            tree.items
                .retain(|k| u32at(k, if *attr == 3 { 4 } else { 0 }) != oid);
        }
        let Some(r) = changes.iter().find(|r| r.integer(1024) == Some(oid)) else {
            continue;
        };
        let (block, crc) = image.frame(b'C', &encode_record(r)?)?;
        let mut key = oid.to_le_bytes().to_vec();
        key.extend(block.to_le_bytes());
        key.extend(crc.to_le_bytes());
        main.items.push(key);
        for (attr, tree) in &mut indexes {
            if *attr == 3 {
                tree.items.retain(|k| u32at(k, 4) != oid);
                let mut key = r
                    .integer(3)
                    .ok_or("setting parent absent")?
                    .to_le_bytes()
                    .to_vec();
                key.extend(oid.to_le_bytes());
                tree.items.push(key);
            } else {
                tree.items.retain(|k| u32at(k, 0) != oid);
                let mut key = oid.to_le_bytes().to_vec();
                key.extend(text_key(r.string(*attr).ok_or("setting attribute absent")?));
                tree.items.push(key);
            }
        }
    }
    main.items.sort_by_key(|k| u32at(k, 0));
    // DB40 6.12 also keeps a flat OID-presence list in the D stream.
    // Native registry queries intersect their index results with this list.
    // Updating MainKey alone makes a new value invisible to kodweb.
    let mut present = if let Some(bytes) = index.writer_presence()? {
        if bytes.len() < 4 {
            return Err("truncated legacy OID list".into());
        }
        let count = u32at(&bytes, 0) as usize;
        if count > (bytes.len() - 4) / 4 {
            return Err("invalid legacy OID list length".into());
        }
        let ids = bytes[4..4 + count * 4]
            .chunks_exact(4)
            .map(|b| u32at(b, 0))
            .collect::<Vec<_>>();
        if ids.windows(2).any(|p| p[0] >= p[1]) {
            return Err("unordered legacy OID list".into());
        }
        ids.into_iter().collect::<std::collections::BTreeSet<_>>()
    } else {
        main.items.iter().map(|k| u32at(k, 0)).collect()
    };
    for record in changes {
        present.insert(record.integer(1024).ok_or("setting OID missing")?);
    }
    for (oid, _) in deletions {
        present.remove(oid);
    }
    let mut presence = (present.len() as u32).to_le_bytes().to_vec();
    for oid in present {
        presence.extend(oid.to_le_bytes());
    }
    presence.resize((presence.len() + 7).div_ceil(128) * 128 - 7, 0);
    let mut trees = vec![(65535, 0, main, false)];
    for (attr, mut tree) in indexes {
        if attr == 3 {
            tree.items.sort_by_key(|k| (u32at(k, 0), u32at(k, 4)));
        } else {
            tree.items = sort_field_items(2, tree.items)?.0;
        }
        trees.push((621, attr, tree, attr != 3));
    }
    for (area, attr, tree, variable) in trees {
        for block in tree.blocks {
            image.release_frame(block, b'L')?;
        }
        let root = image.tree(area, attr, &tree.items, variable)?;
        let d = directory
            .chunks_exact_mut(10)
            .find(|d| u16at(d, 6) == area && u16at(d, 8) == attr)
            .ok_or("registry index missing")?;
        let generation = u16at(d, 0).wrapping_add((changes.len() + deletions.len()) as u16);
        d[..2].copy_from_slice(&generation.to_le_bytes());
        d[2..6].copy_from_slice(&root.to_le_bytes());
    }
    image.release_frame(u32at(&header, 15), b'K')?;
    let root = image.frame(b'K', &directory)?.0;
    image.bytes[15..19].copy_from_slice(&root.to_le_bytes());
    let old_presence = u32at(&header, 43);
    if old_presence != 0 {
        image.release_frame(old_presence, b'D')?;
    }
    let root = image.frame(b'D', &presence)?.0;
    image.bytes[43..47].copy_from_slice(&root.to_le_bytes());
    Ok(image.bytes)
}
