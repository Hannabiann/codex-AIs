//! The original KOD form replaces a same-name object. Publish its deletion and
//! the restored object together, so a failed import cannot lose the old object.
use super::*;

/// Native file wrapper used by mp_kodReadDocument (not a DB4 volume).
pub(crate) fn decode_file(bytes: &[u8]) -> Result<Record, String> {
    if bytes.get(..4) != Some(&123456789u32.to_le_bytes()) {
        return Err("Неверный заголовок KOD".into());
    }
    decode_snapshot(&bytes[4..])
}

pub(crate) fn decode_snapshot(bytes: &[u8]) -> Result<Record, String> {
    let id = bytes.get(..4).ok_or("Неполный файл KOD")?;
    let oid = u32::from_le_bytes(id.try_into().unwrap());
    let mut record = crate::db::fields::parse_record(&bytes[4..])
        .map_err(|e| format!("Неверный файл KOD: {e}"))?;
    if oid == 0
        || record.integer(1024) != Some(oid)
        || record
            .fields
            .iter()
            .filter(|f| f.id == 1024)
            .any(|f| f.wire_type != 5 || !matches!(f.value, Value::Integer(id) if id == oid))
    {
        return Err("KOD: внутренний номер не совпадает с записью".into());
    }
    encode_record(&record)?;
    let mut identity = false;
    record.fields = record
        .fields
        .iter()
        .filter(|f| {
            if f.id != 1024 {
                return true;
            }
            if identity {
                return false;
            }
            identity = true;
            true
        })
        .cloned()
        .collect::<Vec<_>>()
        .into();
    record.flags = 0; // Same editable snapshot as Document.ReadFromStream.
    canonicalize(&mut record);
    Ok(record)
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Conflict {
    pub current: Option<Record>,
    pub deleted: bool,
    pub names: Vec<Record>,
}
impl Conflict {
    pub fn exists(&self) -> bool {
        self.current.is_some() || !self.names.is_empty()
    }
    pub fn identical(&self, record: &Record) -> bool {
        self.current.as_ref() == Some(record)
            && self
                .names
                .iter()
                .all(|r| r.integer(1024) == record.integer(1024))
    }
}

fn inspect(reader: &mut crate::db::mainkey::MainKey, record: &Record) -> Result<Conflict, String> {
    let oid = record.integer(1024).ok_or("KOD identity missing")?;
    let entry = reader.lookup_including_deleted(oid)?;
    let deleted = entry.is_some_and(|e| e.block == u32::MAX);
    let current = entry
        .filter(|e| e.block != u32::MAX)
        .map(|e| reader.record(e))
        .transpose()?;
    let name = record
        .string(1)
        .map(crate::db::decode)
        .unwrap_or_default()
        .to_lowercase();
    let candidates = if reader.has_field_index(record.area, 1) {
        reader.text_candidates(record.area, 1, |b| {
            crate::db::decode(b).to_lowercase() == name
        })?
    } else {
        reader
            .entries(usize::MAX)?
            .into_iter()
            .filter(|e| e.block != u32::MAX)
            .map(|e| e.oid)
            .collect()
    };
    let mut ids = candidates;
    ids.sort_unstable();
    ids.dedup();
    let mut names = Vec::new();
    for id in ids {
        crate::execution::check()?;
        let Some(entry) = reader.lookup(id)? else {
            continue;
        };
        if entry.block == u32::MAX {
            continue;
        }
        let value = reader.record(entry)?;
        if value.area == record.area && value.fields.iter().any(|f| f.id == 1 && matches!(&f.value, Value::String(b) if crate::db::decode(b).to_lowercase() == name)) { names.push(value); }
    }
    Ok(Conflict {
        current,
        deleted,
        names,
    })
}

pub(crate) fn preview(path: &Path, records: &[Record]) -> Result<Vec<Conflict>, String> {
    ensure_recovered(path)?;
    let mut reader = crate::db::mainkey::MainKey::open(path)?;
    records.iter().map(|r| inspect(&mut reader, r)).collect()
}

/// A batch targets one volume. All conflicts are checked under the same writer
/// lock and all selected records are published by one existing undo/redo journal.
pub(crate) fn import_batch(
    path: &Path,
    records: &[Record],
    expected: &[Conflict],
    replace: bool,
) -> Result<(), String> {
    if records.is_empty() || records.len() != expected.len() {
        return Err("Пустой или неполный план восстановления".into());
    }
    let mut identities = std::collections::HashSet::new();
    let mut names = std::collections::HashSet::new();
    for r in records {
        encode_record(r)?;
        if matches!(r.area, 1002 | 1010) {
            return Err("Импорт исполняемых KS пока не поддерживается".into());
        }
        if !identities.insert(r.integer(1024).ok_or("KOD identity missing")?)
            || !names.insert((
                r.area,
                r.string(1)
                    .map(crate::db::decode)
                    .unwrap_or_default()
                    .to_lowercase(),
            ))
        {
            return Err("В выбранных файлах повторяется номер или наименование объекта".into());
        }
    }
    ensure_recovered(path)?;
    let _mutation = crate::execution::lock(&MUTATIONS)?;
    let _publication = crate::execution::Publication::enter()?;
    let started = std::time::Instant::now();
    let _lock = volume_lock(path)?;
    recover_locked(path)?;
    check_volume_budget(path)?;
    let old = std::sync::Arc::new(std::fs::read(path).map_err(|e| e.to_string())?);
    let mut reader = crate::db::mainkey::MainKey::from_image(old.clone())?;
    let mut deletes = std::collections::BTreeSet::new();
    for (record, expected) in records.iter().zip(expected) {
        let state = inspect(&mut reader, record)?;
        if &state != expected {
            return Err(
                "База изменилась после просмотра. Сформируйте предварительный список заново."
                    .into(),
            );
        }
        if state.names.len() > 1 {
            return Err("В выбранном томе несколько объектов с таким наименованием".into());
        }
        if !replace && state.exists() {
            return Err("Для существующих объектов требуется разрешить замену".into());
        }
        for r in state.names {
            let id = r.integer(1024).ok_or("Stored identity missing")?;
            if Some(id) != record.integer(1024) {
                if identities.contains(&id) {
                    return Err("Замена по имени удалит другой объект выбранного пакета".into());
                }
                deletes.insert((id, false));
            }
        }
    }
    let deletes: Vec<_> = deletes.into_iter().collect();
    let bytes = std::sync::Arc::new(index_delta::candidate_from_image(
        path,
        records,
        &deletes,
        old.clone(),
    )?);
    let mut check = crate::db::mainkey::MainKey::from_image(bytes.clone())?;
    for record in records {
        let entry = check
            .lookup(record.integer(1024).unwrap())?
            .ok_or("KOD transaction object missing")?;
        if check.record(entry)? != *record {
            return Err("KOD transaction verification failed".into());
        }
    }
    for (id, _) in deletes {
        if check.lookup(id)?.is_some() {
            return Err("KOD transaction replacement failed".into());
        }
    }
    publish_checked(path, old, bytes, started)
}

pub(crate) fn import(path: &Path, record: &Record) -> Result<(), String> {
    encode_record(record)?;
    let oid = record.integer(1024).ok_or("KOD identity missing")?;
    let name = record
        .string(1)
        .map(crate::db::decode)
        .unwrap_or_default()
        .to_lowercase();
    ensure_recovered(path)?;
    let _mutation = crate::execution::lock(&MUTATIONS)?;
    let _publication = crate::execution::Publication::enter()?;
    let started = std::time::Instant::now();
    let _lock = volume_lock(path)?;
    recover_locked(path)?;
    check_volume_budget(path)?;
    let old = std::sync::Arc::new(std::fs::read(path).map_err(|e| e.to_string())?);
    let mut reader = crate::db::mainkey::MainKey::from_image(old.clone())?;
    let candidates = if reader.has_field_index(record.area, 1) {
        reader.text_candidates(record.area, 1, |b| {
            crate::db::decode(b).to_lowercase() == name
        })?
    } else {
        reader
            .entries(usize::MAX)?
            .into_iter()
            .filter(|e| e.block != u32::MAX)
            .map(|e| e.oid)
            .collect()
    };
    let mut same = Vec::new();
    for id in candidates {
        let Some(entry) = reader.lookup(id)? else {
            continue;
        };
        if entry.block == u32::MAX {
            continue;
        }
        let current = reader.record(entry)?;
        if current.area == record.area && current.fields.iter().any(|f|
            f.id == 1 && matches!(&f.value, Value::String(b) if crate::db::decode(b).to_lowercase() == name)) {
            same.push(id);
        }
    }
    same.sort_unstable();
    same.dedup();
    if same.len() > 1 {
        return Err("В выбранном томе несколько объектов с таким наименованием".into());
    }
    let deletes = same
        .into_iter()
        .filter(|id| *id != oid)
        .map(|id| (id, false))
        .collect::<Vec<_>>();
    let bytes = std::sync::Arc::new(index_delta::candidate_from_image(
        path,
        &[record.clone()],
        &deletes,
        old.clone(),
    )?);
    let mut check = crate::db::mainkey::MainKey::from_image(bytes.clone())?;
    let entry = check.lookup(oid)?.ok_or("KOD transaction object missing")?;
    if check.record(entry)? != *record {
        return Err("KOD transaction verification failed".into());
    }
    for (id, _) in deletes {
        if check.lookup(id)?.is_some() {
            return Err("KOD transaction replacement failed".into());
        }
    }
    publish_checked(path, old, bytes, started)
}
