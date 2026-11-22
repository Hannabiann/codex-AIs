//! The original KOD form replaces a same-name object. Publish its deletion and
//! the restored object together, so a failed import cannot lose the old object.
use super::*;

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
