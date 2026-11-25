//! One config.db4 publication for a native system-settings submission.
use super::*;
use crate::db::{
    mainkey::MainKey,
    registry::{Entry, Registry},
};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

pub(crate) struct Transaction {
    path: PathBuf,
    before: Arc<Vec<u8>>,
    records: BTreeMap<u32, Record>,
    changes: BTreeMap<u32, Record>,
    deleted: BTreeMap<u32, bool>,
    unindexed_titles: std::collections::BTreeSet<u32>,
    next: u32,
    pub registry: Registry,
}

fn encoded(s: &str) -> Result<Vec<u8>, String> {
    let (bytes, _, bad) = encoding_rs::WINDOWS_1251.encode(s);
    if bad || s.contains('\0') {
        return Err("Настройка содержит недопустимые символы".into());
    }
    Ok(bytes.into_owned())
}
fn string(id: u16, text: &[u8]) -> Field {
    Field {
        id,
        wire_type: 2,
        value: Value::String(text.to_vec()),
    }
}
fn link(id: u16, value: u32) -> Field {
    Field {
        id,
        wire_type: 5,
        value: Value::Integer(value),
    }
}

impl Transaction {
    pub fn open(path: &Path, root: u32) -> Result<Self, String> {
        let path = path.canonicalize().map_err(|e| e.to_string())?;
        ensure_recovered(&path)?;
        let _mutation = crate::execution::lock(&MUTATIONS)?;
        let _lock = volume_lock(&path)?;
        recover_locked(&path)?;
        check_volume_budget(&path)?;
        let before = Arc::new(std::fs::read(&path).map_err(|e| e.to_string())?);
        let mut index = MainKey::from_image(before.clone())?;
        let entries = index.entries(100_000)?;
        let next = entries
            .iter()
            .map(|e| e.oid)
            .max()
            .ok_or("empty config.db4")?;
        // Superseded MainKey records are not active registry values.
        let active = index
            .text_candidates(621, 1, |_| true)?
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        let mut records = BTreeMap::new();
        for entry in entries {
            if let Ok(r) = index.record(entry) {
                if r.area == 620 || r.area == 621 && active.contains(&entry.oid) {
                    records.insert(entry.oid, r);
                }
            } else if active.contains(&entry.oid) {
                return Err("active registry record cannot be read".into());
            }
        }
        let registry =
            Registry::from_records(&records.values().cloned().collect::<Vec<_>>(), root)?;
        Ok(Self {
            path,
            before,
            records,
            changes: BTreeMap::new(),
            deleted: BTreeMap::new(),
            unindexed_titles: Default::default(),
            next,
            registry,
        })
    }
    pub fn revision(&self) -> String {
        format!("{:08x}", crc32fast::hash(&self.before))
    }
    fn allocate(&mut self) -> Result<u32, String> {
        self.next = self.next.checked_add(1).ok_or("registry OID overflow")?;
        Ok(self.next)
    }
    pub fn create_key(&mut self, path: &str) -> Result<(), String> {
        self.registry.open(path)?; // Validate the absolute Web subtree first.
        let parts: Vec<_> = path.split('\\').filter(|s| !s.is_empty()).collect();
        if parts.iter().any(|part| matches!(*part, "." | "..")) {
            return Err("invalid registry path component".into());
        }
        let mut current = self.registry.root;
        for name in &parts[3..] {
            if let Some(id) = self.registry.child(current, name)? {
                current = id;
                continue;
            }
            let name_bytes = encoded(name)?;
            if name_bytes.is_empty() || name_bytes.len() > 255 {
                return Err("invalid registry key name".into());
            }
            let id = self.allocate()?;
            let mut record = Record {
                area: 620,
                flags: 5,
                fields: vec![string(1, &name_bytes), link(3, current), link(1024, id)].into(),
            };
            canonicalize(&mut record);
            self.registry.insert_key(current, id, name)?;
            self.changes.insert(id, record.clone());
            self.records.insert(id, record);
            current = id;
        }
        Ok(())
    }
    pub fn set(
        &mut self,
        parent: u32,
        name: &str,
        kind: &str,
        value: Option<&str>,
    ) -> Result<(), String> {
        self.registry.key(parent)?;
        let name_bytes = encoded(name)?;
        if name_bytes.is_empty()
            || name_bytes.len() > 255
            || !matches!(kind, "REG_SZ" | "REG_DWORD")
        {
            return Err("invalid registry value name/type".into());
        }
        let old = self.registry.query(parent, name)?.cloned();
        if old.as_ref().map(|e| (e.kind.as_str(), e.value.as_str())) == value.map(|v| (kind, v)) {
            return Ok(());
        }
        let Some(value) = value else {
            if let Some(old) = old {
                self.changes.remove(&old.oid);
                self.deleted.insert(old.oid, false);
                self.records.remove(&old.oid);
                self.registry.remove_value(parent, name)?;
            }
            return Ok(());
        };
        let bytes = encoded(value)?;
        if bytes.len() > 65536 {
            return Err("registry value exceeds 64 KiB".into());
        }
        if kind == "REG_DWORD" && value.parse::<i32>().is_err() {
            return Err("REG_DWORD requires a 32-bit integer".into());
        }
        if parent == self.registry.root
            && counters::NAMES.iter().any(|n| n.eq_ignore_ascii_case(name))
            && !value.parse::<i32>().is_ok_and(|n| n > 0)
        {
            return Err("Номер должен быть от 1 до 2147483647".into());
        }
        let oid = match &old {
            Some(e) => e.oid,
            None => self.allocate()?,
        };
        self.unindexed_titles.remove(&oid);
        let mut record = if let Some(r) = self.records.get(&oid) {
            r.clone()
        } else {
            Record {
                area: 621,
                flags: 5,
                fields: vec![string(1, &name_bytes), link(3, parent), link(1024, oid)].into(),
            }
        };
        let fields = Arc::make_mut(&mut record.fields);
        fields.retain(|f| f.id != 4 && f.id != 5);
        fields.push(string(5, kind.as_bytes()));
        if bytes.is_empty() {
            fields.push(string(4, &[]));
        } else {
            for chunk in bytes.chunks(255) {
                fields.push(string(4, chunk));
            }
        }
        canonicalize(&mut record);
        self.registry.insert_value(
            parent,
            Entry {
                oid,
                name: old.map_or_else(|| name.into(), |e| e.name),
                kind: kind.into(),
                value: value.into(),
            },
        )?;
        self.changes.insert(oid, record.clone());
        self.records.insert(oid, record);
        Ok(())
    }
    /// Native Config assignment and directory SaveState update registry indexes
    /// without adding title-word postings. RegistryRoot.SetValue does add them.
    pub fn set_host_property(
        &mut self,
        parent: u32,
        name: &str,
        kind: &str,
        value: Option<&str>,
    ) -> Result<(), String> {
        self.set(parent, name, kind, value)?;
        if let Some(entry) = self.registry.query(parent, name)? {
            self.unindexed_titles.insert(entry.oid);
            // Some native configurations retain several active records for a
            // host property. QueryValue takes the first, but startup property
            // enumeration can take the last. Keep their values consistent so
            // both servers reopen the setting that was actually saved.
            let saved = self
                .records
                .get(&entry.oid)
                .ok_or("saved host setting absent")?
                .clone();
            let duplicates = self
                .records
                .iter()
                .filter_map(|(&oid, r)| {
                    (oid != entry.oid
                        && r.area == 621
                        && r.integer(3) == Some(parent)
                        && r.string(1).is_some_and(|s| {
                            crate::db::decode(s).to_lowercase() == name.to_lowercase()
                        }))
                    .then_some(oid)
                })
                .collect::<Vec<_>>();
            for oid in duplicates {
                let record = self.records.get_mut(&oid).unwrap();
                let before = record.fields.clone();
                let fields = Arc::make_mut(&mut record.fields);
                fields.retain(|f| !matches!(f.id, 4 | 5));
                fields.extend(
                    saved
                        .fields
                        .iter()
                        .filter(|f| matches!(f.id, 4 | 5))
                        .cloned(),
                );
                canonicalize(record);
                if record.fields != before {
                    self.changes.insert(oid, record.clone());
                    self.unindexed_titles.insert(oid);
                }
            }
        }
        Ok(())
    }
    pub fn commit(self) -> Result<String, String> {
        if self.changes.is_empty() && self.deleted.is_empty() {
            return Ok(self.revision());
        }
        let _mutation = crate::execution::lock(&MUTATIONS)?;
        let _publication = crate::execution::Publication::enter()?;
        let started = std::time::Instant::now();
        let _lock = volume_lock(&self.path)?;
        recover_locked(&self.path)?;
        let actual = std::fs::read(&self.path).map_err(|e| e.to_string())?;
        if actual.as_slice() != self.before.as_slice() {
            return Err("Настройки изменились. Откройте форму заново.".into());
        }
        let changes = self.changes.values().cloned().collect::<Vec<_>>();
        let deletions = self.deleted.into_iter().collect::<Vec<_>>();
        let mut index = MainKey::from_image(self.before.clone())?;
        let candidate = if u16at(&self.before, 5) == 0x612 {
            settings::legacy(
                &mut index,
                &self.before,
                &changes,
                &deletions,
                &self.unindexed_titles,
            )?
        } else {
            index_delta::candidate_from_image(
                &self.path,
                &changes,
                &deletions,
                self.before.clone(),
            )?
        };
        let candidate = Arc::new(candidate);
        let mut verify = MainKey::from_image(candidate.clone())?;
        for r in &changes {
            let oid = r.integer(1024).ok_or("registry OID absent")?;
            let e = verify.lookup(oid)?.ok_or("saved setting absent")?;
            let saved = verify.record(e)?;
            if saved.area != r.area || saved.fields != r.fields {
                return Err("saved registry verification failed".into());
            }
            let key = text_key(r.string(1).ok_or("setting name absent")?);
            if !verify
                .text_candidates(r.area, 1, |s| s == key)?
                .contains(&oid)
            {
                return Err("saved registry name index mismatch".into());
            }
        }
        for (oid, _) in &deletions {
            if verify.lookup(*oid)?.is_some() {
                return Err("deleted registry value remains indexed".into());
            }
        }
        let revision = format!("{:08x}", crc32fast::hash(&candidate));
        publish_checked(&self.path, self.before, candidate, started)?;
        Ok(revision)
    }
}
