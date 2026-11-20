use super::*;
use crate::db::mainkey::MainKey;
use std::sync::Arc;

pub(crate) const NAMES: [&str; 2] = ["CountOfNumberDelo", "inputnumber"];

fn read(index: &mut MainKey, root: u32, oid: u32, name: &str) -> Result<(Record, String), String> {
    let entry = index.lookup(oid)?.ok_or("registration counter missing")?;
    let record = index.record(entry)?;
    if record.area != 621
        || record.integer(3) != Some(root)
        || !record
            .string(1)
            .is_some_and(|s| s.eq_ignore_ascii_case(name.as_bytes()))
        || !matches!(record.string(5), Some(b"REG_DWORD" | b"REG_SZ"))
        || record.fields.iter().filter(|f| f.id == 4).count() != 1
    {
        return Err(format!("invalid registry counter: {name}"));
    }
    let value = record
        .string(4)
        .and_then(|s| std::str::from_utf8(s).ok())
        .ok_or("invalid stored configuration value")?
        .to_owned();
    Ok((record, value))
}

pub(crate) fn save(
    path: &Path,
    root: u32,
    ids: [u32; 2],
    expected: [u32; 2],
    next: [u32; 2],
) -> Result<(), String> {
    if next.iter().any(|n| !(1..=i32::MAX as u32).contains(n)) {
        return Err("Номер вне допустимого диапазона.".into());
    }
    let updates = (0..2)
        .map(|i| {
            (
                ids[i],
                NAMES[i],
                expected[i].to_string(),
                next[i].to_string(),
            )
        })
        .collect::<Vec<_>>();
    save_values(path, root, &updates)
}

/// Replace existing numeric registry values under one key in a single
/// recoverable publication. The caller supplies the values it originally read.
pub(crate) fn save_values(
    path: &Path,
    root: u32,
    updates: &[(u32, &str, String, String)],
) -> Result<(), String> {
    ensure_recovered(path)?;
    let _mutation = crate::execution::lock(&MUTATIONS)?;
    let _publication = crate::execution::Publication::enter()?;
    let started = std::time::Instant::now();
    let _lock = volume_lock(path)?;
    recover_locked(path)?;
    check_volume_budget(path)?;
    let old = Arc::new(std::fs::read(path).map_err(|e| e.to_string())?);
    let mut index = MainKey::from_image(old.clone())?;
    let mut changes = Vec::new();
    for (oid, name, expected, next) in updates {
        let (mut record, current) = read(&mut index, root, *oid, name)?;
        if &current != expected {
            return Err("Настройки изменились. Откройте форму заново.".into());
        }
        if &current == next {
            continue;
        }
        if next.len() > 255
            || !next
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b';' | b':'))
        {
            return Err("Недопустимое значение параметра.".into());
        }
        let field = Arc::make_mut(&mut record.fields)
            .iter_mut()
            .find(|f| f.id == 4)
            .unwrap();
        field.value = Value::String(next.as_bytes().to_vec());
        changes.push(record);
    }
    if changes.is_empty() {
        return Ok(());
    }
    let candidate = if u16at(&old, 5) == 0x612 {
        legacy_image(&mut index, &old, &changes)?
    } else {
        index_delta::candidate_from_image(path, &changes, &[], old.clone())?
    };
    let bytes = Arc::new(candidate);
    let mut verify = MainKey::from_image(bytes.clone())?;
    for (oid, name, _, next) in updates {
        if read(&mut verify, root, *oid, name)?.1 != *next {
            return Err("registration counter verification failed".into());
        }
        if !verify
            .text_candidates(621, 4, |key| key == next.as_bytes())?
            .contains(oid)
        {
            return Err("registration counter value index verification failed".into());
        }
    }
    publish_checked(path, old, bytes, started)
}

// DB40 6.12 stores numeric registry values as decimal strings. Changing just
// their digits keeps membership, names, types, parents and OID presence intact.
fn legacy_image(index: &mut MainKey, old: &[u8], changes: &[Record]) -> Result<Vec<u8>, String> {
    let (header, mut directory) = index.writer_parts();
    let mut image = Image::for_version(old.to_vec(), 0x612)?;
    let mut main = index.writer_tree(65535, 0, Some(10))?;
    let mut values = index.writer_tree(621, 4, None)?;
    for record in changes {
        let oid = record.integer(1024).ok_or("registry counter OID missing")?;
        let entry = index.lookup(oid)?.ok_or("counter absent")?;
        let old_record = index.record(entry)?;
        let previous = old_record.string(4).ok_or("registry value absent")?;
        if index.word_candidates(previous, false, None)?.contains(&oid) {
            return Err("indexed full-text registry counter format is unsupported".into());
        }
        let mut found = values
            .items
            .iter_mut()
            .filter(|item| item.len() >= 4 && u32at(item, 0) == oid);
        let key = found.next().ok_or("registry value index entry absent")?;
        if found.next().is_some() || key[4..] != *previous {
            return Err("registry value index mismatch".into());
        }
        key.truncate(4);
        key.extend(record.string(4).ok_or("new registry value missing")?);
        let item = main
            .items
            .binary_search_by_key(&oid, |k| u32at(k, 0))
            .map_err(|_| "registry MainKey entry absent")?;
        image.release_frame(entry.block, b'C')?;
        let (block, crc) = image.frame(b'C', &encode_record(record)?)?;
        main.items[item][4..8].copy_from_slice(&block.to_le_bytes());
        main.items[item][8..10].copy_from_slice(&crc.to_le_bytes());
    }
    for block in main.blocks.iter().chain(&values.blocks) {
        image.release_frame(*block, b'L')?;
    }
    let (values, _) = sort_field_items(2, values.items)?;
    for (area, attr, keys, variable) in [(65535, 0, main.items, false), (621, 4, values, true)] {
        let descriptor = directory
            .chunks_exact_mut(10)
            .find(|d| u16at(d, 6) == area && u16at(d, 8) == attr)
            .ok_or("registry index descriptor missing")?;
        let block = image.tree(area, attr, &keys, variable)?;
        let generation = u16at(descriptor, 0).wrapping_add(changes.len() as u16);
        descriptor[..2].copy_from_slice(&generation.to_le_bytes());
        descriptor[2..6].copy_from_slice(&block.to_le_bytes());
    }
    image.release_frame(u32at(&header, 15), b'K')?;
    let block = image.frame(b'K', &directory)?.0;
    image.bytes[15..19].copy_from_slice(&block.to_le_bytes());
    Ok(image.bytes)
}
