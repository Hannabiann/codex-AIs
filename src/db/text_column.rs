//! Reuse decoded, unindexed strings across predicates on the same candidate
//! set. This is a bounded query snapshot, not a replacement DB4 index.
use super::{
    fields::Value,
    shared_query::{path_key, BuildKey},
    Db4Engine,
};
use crate::ks::sql::Matcher;
use std::{
    collections::{hash_map::DefaultHasher, BTreeMap},
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::Arc,
};

const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_COLUMN_BYTES: usize = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 16;
type Key = (PathBuf, u64, u16, u16, u64);

#[derive(Debug)]
struct Column {
    // Coverage includes missing records and tombstones. A digest alone cannot
    // establish equality, and a smaller candidate set is not a complete column.
    candidates: Vec<u32>,
    values: BTreeMap<String, Vec<u32>>,
    bytes: usize,
}
impl Column {
    fn new(candidates: &[u32]) -> Self {
        let candidates = candidates.to_vec();
        let bytes = candidates.capacity() * 4;
        Self {
            candidates,
            values: BTreeMap::new(),
            bytes,
        }
    }

    fn add(&mut self, oid: u32, value: String) -> bool {
        if !self.values.contains_key(&value) {
            // Include map/node overhead in the admission budget, in addition
            // to allocated string and posting capacities.
            let extra = value.capacity().saturating_add(128);
            if self.bytes.saturating_add(extra) > MAX_COLUMN_BYTES {
                return false;
            }
            self.bytes += extra;
            self.values.insert(value.clone(), Vec::new());
        }
        let ids = self.values.get_mut(&value).unwrap();
        if ids.last() == Some(&oid) {
            return true;
        }
        if ids.len() == ids.capacity() {
            let extra = ids.capacity().max(4);
            if self.bytes.saturating_add(extra * 4) > MAX_COLUMN_BYTES {
                return false;
            }
            let before = ids.capacity();
            ids.reserve_exact(extra);
            self.bytes += (ids.capacity() - before) * 4;
            if self.bytes > MAX_COLUMN_BYTES {
                return false;
            }
        }
        ids.push(oid);
        true
    }

    fn matches(&self, pattern: &Matcher) -> Result<Vec<u32>, String> {
        let mut ids = Vec::new();
        for (n, (value, postings)) in self.values.iter().enumerate() {
            if n % 256 == 0 {
                crate::execution::check()?;
            }
            // Retain the exact residual matcher (case, ё, wildcards, empty
            // strings). Indexed LIKE has different semantics in kodweb.
            if pattern.is_match(value) {
                ids.extend_from_slice(postings);
            }
        }
        ids.sort_unstable();
        ids.dedup();
        crate::execution::check()?;
        Ok(ids)
    }
}

#[derive(Debug, Default)]
pub(super) struct TextColumns {
    entries: BTreeMap<Key, (Arc<Column>, u64)>,
    bytes: usize,
    tick: u64,
}
impl TextColumns {
    fn get(&mut self, key: &Key, candidates: &[u32]) -> Option<Arc<Column>> {
        let (column, used) = self.entries.get_mut(key)?;
        if column.candidates != candidates {
            return None;
        }
        self.tick = self.tick.wrapping_add(1);
        *used = self.tick;
        Some(column.clone())
    }
    fn remove(&mut self, key: &Key) {
        if let Some((column, _)) = self.entries.remove(key) {
            self.bytes -= column.bytes;
        }
    }
    fn insert(&mut self, key: Key, column: Arc<Column>) {
        if column.bytes > MAX_COLUMN_BYTES {
            return;
        }
        let stale: Vec<_> = self
            .entries
            .keys()
            .filter(|k| k.0 == key.0 && k.1 != key.1)
            .cloned()
            .collect();
        for k in stale {
            self.remove(&k);
        }
        self.remove(&key);
        while self.entries.len() >= MAX_ENTRIES || self.bytes + column.bytes > MAX_BYTES {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(k, _)| k.clone());
            let Some(oldest) = oldest else {
                break;
            };
            self.remove(&oldest);
        }
        self.tick = self.tick.wrapping_add(1);
        self.bytes += column.bytes;
        self.entries.insert(key, (column, self.tick));
    }
}

impl Db4Engine {
    pub(super) fn cached_text_matches(
        &self,
        v: usize,
        area: u16,
        attribute: u16,
        pattern: &Matcher,
        candidates: &[u32],
    ) -> Result<Option<Vec<u32>>, String> {
        // Admission limits only choose the execution path; they never limit
        // SQL results. Small or oversized sets keep the ordinary residual scan.
        if !(512..=1_000_000).contains(&candidates.len()) {
            return Ok(None);
        }
        crate::execution::check()?;
        let mut hash = DefaultHasher::new();
        candidates.hash(&mut hash);
        let digest = hash.finish();
        let path = path_key(&self.volumes[v].path);
        loop {
            let revision = self.query_generation(v)?;
            let key = (path.clone(), revision, area, attribute, digest);
            let hit = crate::execution::lock(&self.queries.text_columns)?.get(&key, candidates);
            if let Some(column) = hit {
                return column.matches(pattern).map(Some);
            }
            let _build = self.queries.build(BuildKey::TextColumn(
                path.clone(),
                area,
                attribute,
                revision,
                digest,
            ))?;
            if self.query_generation(v)? != revision {
                continue;
            }
            let hit = crate::execution::lock(&self.queries.text_columns)?.get(&key, candidates);
            if let Some(column) = hit {
                return column.matches(pattern).map(Some);
            }
            let started = std::time::Instant::now();
            let Some(column) = self.read_text_column(v, area, attribute, candidates)? else {
                return Ok(None);
            };
            super::writer::ensure_recovered(&self.volumes[v].path)?;
            let publication = super::writer::open_guard()?;
            if super::writer::generation(&self.volumes[v].path) != revision {
                continue;
            }
            crate::execution::check()?;
            let column = Arc::new(column);
            crate::execution::lock(&self.queries.text_columns)?.insert(key, column.clone());
            drop(publication);
            if std::env::var_os("KODEKS_PROFILE").is_some_and(|v| v == "1") {
                eprintln!("DB shared text column: volume={}, area={area}, attribute={attribute}, candidates={}, values={}, bytes={}, elapsed_ms={}", self.volumes[v].name, candidates.len(), column.values.len(), column.bytes, started.elapsed().as_millis());
            }
            return column.matches(pattern).map(Some);
        }
    }

    fn read_text_column(
        &self,
        v: usize,
        area: u16,
        attribute: u16,
        candidates: &[u32],
    ) -> Result<Option<Column>, String> {
        let mut column = Column::new(candidates);
        for batch in candidates.chunks(1024) {
            let admitted = self.with_reader(v, |tree| {
                for entry in tree.lookup_many(batch)? {
                    if entry.block == u32::MAX {
                        continue;
                    }
                    let record = tree.record_attributes(entry)?.0;
                    if record.area != area {
                        continue;
                    }
                    for field in record.fields.iter() {
                        if field.id == attribute {
                            if let Value::String(value) = &field.value {
                                if !column.add(entry.oid, super::decode(value)) {
                                    return Ok(false);
                                }
                            }
                        }
                    }
                }
                Ok(true)
            })?;
            if !admitted {
                return Ok(None);
            }
        }
        Ok(Some(column))
    }
}
