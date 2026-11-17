//! A mount table per KS DataBase. Remove detaches a mount, never deletes a file.
use super::{fields::Record, schema::Schema, Db4Engine, RecordLookup, VirtualDir};
use crate::ks::{
    database::{DatabaseHost, Volume},
    runtime::OpenError,
};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    ops::Deref,
    path::PathBuf,
};
#[derive(Default)]
struct Timings {
    schema: Cell<(u64, u128)>,
    record: Cell<(u64, u128)>,
    query: Cell<(u64, u128)>,
}
struct Measure<'a> {
    counter: &'a Cell<(u64, u128)>,
    start: std::time::Instant,
}
impl<'a> Measure<'a> {
    fn new(counter: &'a Cell<(u64, u128)>) -> Self {
        Self {
            counter,
            start: std::time::Instant::now(),
        }
    }
}
impl Drop for Measure<'_> {
    fn drop(&mut self) {
        let (n, t) = self.counter.get();
        self.counter
            .set((n + 1, t + self.start.elapsed().as_micros()));
    }
}
struct Mount {
    writable: bool,
    volume: Volume,
    key: String,
    schema: Schema,
}
fn conflict_text<'a>(
    scope: u32,
    conflict: &super::schema::MergeConflict,
    mounts: impl IntoIterator<Item = &'a Mount>,
) -> String {
    let definitions = mounts
        .into_iter()
        .filter_map(|mount| {
            let field = mount
                .schema
                .areas
                .get(&conflict.area)?
                .fields
                .get(&conflict.incoming.id)?;
            Some(format!(
                "{}: name={:?}, type={}, ref={}",
                mount.key, field.name, field.kind, field.reference
            ))
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!("DataBase({scope}) schema conflict: {conflict}; definitions in volume order: [{definitions}]")
}
pub struct Contexts<E> {
    writable: std::collections::BTreeSet<String>,
    write_epoch: Cell<u64>,
    engine: E,
    extra: Db4Engine,
    root: PathBuf,
    scopes: BTreeMap<u32, Vec<Mount>>,
    next_scope: u32,
    next_mount: u32,
    trace: bool,
    record_mounts: RefCell<BTreeMap<(u32, u32), Option<u32>>>,
    timings: Timings,
    schemas: RefCell<BTreeMap<u32, std::sync::Arc<Schema>>>,
    queries: RefCell<BTreeMap<(u32, String), Vec<u32>>>,
    records: RefCell<RecordCache>,
    fulltext: RefCell<BTreeMap<(u32, String), super::free_text::ResultData>>,
    word_statistics: RefCell<BTreeMap<(u32, String), u64>>,
}
#[derive(Default)]
struct RecordCache {
    items: BTreeMap<(u32, u32), Record>,
    bytes: usize,
    metadata: BTreeMap<(u32, u32), (super::fields::RecordMetadata, u32)>,
    metadata_bytes: usize,
}
impl RecordCache {
    fn insert_metadata(
        &mut self,
        key: (u32, u32),
        data: &super::fields::RecordMetadata,
        mount: u32,
    ) {
        let size = 128 + data.name.as_ref().map_or(0, Vec::len);
        // Admission bound, not a document limit. Retain earlier entries when
        // full: repeated visits through cycles should not flush the hot set.
        if self.metadata_bytes + size <= 32 * 1024 * 1024 && !self.metadata.contains_key(&key) {
            self.metadata_bytes += size;
            self.metadata.insert(key, (data.clone(), mount));
        }
    }
    fn insert(&mut self, key: (u32, u32), record: &Record) {
        use super::fields::Value;
        let size = 128
            + record
                .fields
                .iter()
                .map(|f| {
                    std::mem::size_of::<super::fields::Field>()
                        + match &f.value {
                            Value::String(v) | Value::Text(v) => v.len(),
                            Value::Binary { data, file, .. } => {
                                data.len()
                                    + file
                                        .as_ref()
                                        .map_or(0, |f| f.name.len() + f.source.len() + 4)
                            }
                            Value::TextWithMetadata { data, metadata } => {
                                data.len() + metadata.len()
                            }
                            _ => 0,
                        }
                })
                .sum::<usize>();
        if size > 1024 * 1024 || self.items.contains_key(&key) {
            return;
        }
        if self.items.len() >= 32768 || self.bytes + size > 128 * 1024 * 1024 {
            self.items.clear();
            self.bytes = 0;
        }
        self.bytes += size;
        self.items.insert(key, record.clone());
    }
}
impl<E: Deref<Target = Db4Engine>> Contexts<E> {
    pub fn new(engine: E, dir: &VirtualDir) -> Result<Self, String> {
        let root = engine
            .volumes
            .first()
            .and_then(|v| v.path.parent())
            .ok_or("data root missing")?
            .to_owned();
        let mut c = Self {
            writable: if matches!(dir.path.trim_matches('/'), "99" | "20a") {
                dir.bases
                    .iter()
                    .enumerate()
                    .filter(
                        |(i, _)| match dir.properties.get(&format!("Redact{}", i + 1)) {
                            Some(super::ConfigValue::Integer(n)) => *n != 0,
                            Some(super::ConfigValue::Text(s)) => s == "1",
                            _ => false,
                        },
                    )
                    .map(|(_, s)| super::volume_key(s))
                    .collect()
            } else {
                Default::default()
            },
            write_epoch: Cell::new(super::writer::epoch()),
            engine,
            extra: Db4Engine::default(),
            root,
            scopes: BTreeMap::from([(0, vec![])]),
            next_scope: 1,
            next_mount: 1,
            trace: std::env::var_os("KODEKS_TRACE").is_some_and(|s| s == "1"),
            record_mounts: RefCell::new(BTreeMap::new()),
            timings: Timings::default(),
            schemas: RefCell::new(BTreeMap::new()),
            queries: RefCell::new(BTreeMap::new()),
            fulltext: RefCell::new(BTreeMap::new()),
            word_statistics: RefCell::new(BTreeMap::new()),
            records: RefCell::new(RecordCache::default()),
        };
        // BaseN volumes are opened by the application's AddBases, not by discovery.
        // Application.DataBase and this.Base share this execution database.
        // The Web registry is read independently; config.db4 is not an app mount.
        let key = dir.mapped_to.as_str();
        let v = c
            .engine
            .volume_id(key)?
            .ok_or("initial database volume missing")?;
        let schema = Schema::read(&c.engine.volumes[v].path)?;
        let volume = super::volume::metadata(&c.engine.volumes[v].path, c.next_mount)?;
        c.scopes.get_mut(&0).unwrap().push(Mount {
            writable: false,
            volume,
            key: key.into(),
            schema,
        });
        c.next_mount += 1;
        Ok(c)
    }
    fn attach(&mut self, scope: u32, mount: Mount) -> Result<(), OpenError> {
        // Native 005e1ec0 checks the aggregate during Open; its exception path
        // at 005e212a removes a rejected mount and rebuilds the old schema.
        let mut schema = self.schema(scope).map_err(OpenError::Unsupported)?;
        if let Err(conflict) = schema.merge_native(&mount.schema) {
            let message = conflict_text(
                scope,
                &conflict,
                self.mounts(scope)
                    .unwrap()
                    .iter()
                    .chain(std::iter::once(&mount)),
            );
            if self.trace {
                eprintln!("KS DataBase({scope}).Open rejected: {message}");
            }
            return Err(OpenError::Schema(message));
        }
        self.scopes.get_mut(&scope).unwrap().push(mount);
        self.schemas
            .borrow_mut()
            .insert(scope, std::sync::Arc::new(schema));
        self.queries.borrow_mut().retain(|(s, _), _| *s != scope);
        self.fulltext.borrow_mut().retain(|(s, _), _| *s != scope);
        self.word_statistics
            .borrow_mut()
            .retain(|(s, _), _| *s != scope);
        *self.records.borrow_mut() = RecordCache::default();
        self.record_mounts
            .borrow_mut()
            .retain(|(s, _), _| *s != scope);
        Ok(())
    }
    fn mounts(&self, scope: u32) -> Result<&Vec<Mount>, String> {
        self.scopes
            .get(&scope)
            .ok_or_else(|| "unknown DataBase identity".into())
    }
    fn dir(&self, scope: u32) -> Result<VirtualDir, String> {
        Ok(VirtualDir {
            bases: self.mounts(scope)?.iter().map(|m| m.key.clone()).collect(),
            ..Default::default()
        })
    }
    fn record(&self, scope: u32, oid: u32) -> Result<Option<(Record, &Schema)>, String> {
        Ok(self.record_mount(scope, oid)?.map(|(r, m)| (r, &m.schema)))
    }
    fn record_mount(&self, scope: u32, oid: u32) -> Result<Option<(Record, &Mount)>, String> {
        self.refresh_writes();
        let _measure = Measure::new(&self.timings.record);
        // Read-only snapshot caches are invalidated by mount changes. Large
        // bodies are excluded; the decoded-record cache has a 32 MiB budget.
        if let Some(cached) = self.record_mounts.borrow().get(&(scope, oid)).copied() {
            let Some(id) = cached else { return Ok(None) };
            let mount = self
                .mounts(scope)?
                .iter()
                .find(|m| m.volume.id == id)
                .ok_or("stale record mount cache")?;
            if let Some(record) = self.records.borrow().items.get(&(scope, oid)) {
                return Ok(Some((record.clone(), mount)));
            }
            let record = match self.engine.lookup_record(oid, &mount.key)? {
                RecordLookup::Missing => self.extra.lookup_record(oid, &mount.key)?,
                other => other,
            };
            return match record {
                RecordLookup::Present(record) => {
                    self.records.borrow_mut().insert((scope, oid), &record);
                    Ok(Some((record, mount)))
                }
                _ => Err("selected record changed; restart using a stable snapshot".into()),
            };
        }
        // Native 005e0896..005e08d2 searches mounts from last to first.
        // 005e091e stops at a deletion marker; it must not expose an older copy.
        // Schema merging deliberately keeps its separate forward traversal.
        for m in self.mounts(scope)?.iter().rev() {
            let r = match self.engine.lookup_record(oid, &m.key)? {
                RecordLookup::Missing => self.extra.lookup_record(oid, &m.key)?,
                other => other,
            };
            match r {
                RecordLookup::Present(r) => {
                    self.cache_record_mount(scope, oid, Some(m.volume.id));
                    self.records.borrow_mut().insert((scope, oid), &r);
                    return Ok(Some((r, m)));
                }
                RecordLookup::Deleted => {
                    self.cache_record_mount(scope, oid, None);
                    return Ok(None);
                }
                RecordLookup::Missing => {}
            }
        }
        self.cache_record_mount(scope, oid, None);
        Ok(None)
    }
    fn cache_record_mount(&self, scope: u32, oid: u32, mount: Option<u32>) {
        let mut cache = self.record_mounts.borrow_mut();
        if cache.len() >= 100_000 && !cache.contains_key(&(scope, oid)) {
            cache.clear();
        }
        cache.insert((scope, oid), mount);
    }
    fn metadata_mount(
        &self,
        scope: u32,
        oid: u32,
    ) -> Result<Option<(super::fields::RecordMetadata, &Mount)>, String> {
        self.refresh_writes();
        if let Some((metadata, id)) = self.records.borrow().metadata.get(&(scope, oid)) {
            let mount = self
                .mounts(scope)?
                .iter()
                .find(|m| m.volume.id == *id)
                .ok_or("stale metadata mount cache")?;
            return Ok(Some((metadata.clone(), mount)));
        }
        let selected = self.record_mounts.borrow().get(&(scope, oid)).copied();
        if selected == Some(None) {
            return Ok(None);
        }
        let lookup = |mount: &Mount| match self.engine.lookup_metadata(oid, &mount.key)? {
            RecordLookup::Missing => self.extra.lookup_metadata(oid, &mount.key),
            other => Ok(other),
        };
        if let Some(Some(id)) = selected {
            let mount = self
                .mounts(scope)?
                .iter()
                .find(|m| m.volume.id == id)
                .ok_or("stale record mount cache")?;
            if let Some(record) = self.records.borrow().items.get(&(scope, oid)) {
                return Ok(Some((record.into(), mount)));
            }
            return match lookup(mount)? {
                RecordLookup::Present(metadata) => {
                    self.records.borrow_mut().insert_metadata(
                        (scope, oid),
                        &metadata,
                        mount.volume.id,
                    );
                    Ok(Some((metadata, mount)))
                }
                _ => Err("selected record changed; restart using a stable snapshot".into()),
            };
        }
        // The same overlay and tombstone rules as a full record read.
        for mount in self.mounts(scope)?.iter().rev() {
            match lookup(mount)? {
                RecordLookup::Present(metadata) => {
                    self.cache_record_mount(scope, oid, Some(mount.volume.id));
                    self.records.borrow_mut().insert_metadata(
                        (scope, oid),
                        &metadata,
                        mount.volume.id,
                    );
                    return Ok(Some((metadata, mount)));
                }
                RecordLookup::Deleted => {
                    self.cache_record_mount(scope, oid, None);
                    return Ok(None);
                }
                RecordLookup::Missing => {}
            }
        }
        self.cache_record_mount(scope, oid, None);
        Ok(None)
    }
    fn refresh_writes(&self) {
        let current = super::writer::epoch();
        if self.write_epoch.replace(current) != current {
            self.record_mounts.borrow_mut().clear();
            *self.records.borrow_mut() = RecordCache::default();
            self.queries.borrow_mut().clear();
            self.fulltext.borrow_mut().clear();
            self.word_statistics.borrow_mut().clear();
        }
    }
    fn write_path(&self, scope: u32, area: u16) -> Result<PathBuf, String> {
        let mount = self
            .mounts(scope)?
            .iter()
            .rev()
            .find(|m| m.writable && m.schema.areas.contains_key(&area))
            .ok_or("Нет доступного для записи тома DB4 для этой области")?;
        let path = super::script_volume_path(&self.root, &mount.key);
        let canonical = path.canonicalize().map_err(|e| e.to_string())?;
        if !canonical.starts_with(self.root.canonicalize().map_err(|e| e.to_string())?) {
            return Err("DB4 write outside data root".into());
        }
        Ok(path)
    }
}
impl<E: Deref<Target = Db4Engine>> DatabaseHost for Contexts<E> {
    fn volume_writable(&self, scope: u32, volume: u32) -> Result<bool, String> {
        Ok(self
            .mounts(scope)?
            .iter()
            .any(|m| m.volume.id == volume && m.writable))
    }
    fn select_indexed(
        &self,
        scope: u32,
        sql: &str,
    ) -> Result<Option<crate::ks::host::QueryData>, String> {
        use crate::ks::host::{Column, ColumnValues, QueryData};
        // Numeric DB4 indexes retain the original digits. Unlike text indexes,
        // no case folding destroys the projected value. Preserve every row and
        // stable OID tie order; only avoid loading unrelated document streams.
        if crate::ks::sql_join::parse(sql)?.is_some() {
            return Ok(None);
        }
        let mut q = crate::ks::sql::parse(sql)?;
        let [Column::Field(name)] = q.projection.0.as_slice() else {
            return Ok(None);
        };
        if q.order.len() != 1 || q.order[0].0 != *name {
            return Ok(None);
        }
        let schema = self.shared_schema(scope)?;
        let Some(area) = schema.area(&q.area) else {
            return Ok(None);
        };
        let Some(field) = area
            .fields
            .values()
            .find(|f| f.name == *name && f.kind == 3)
        else {
            return Ok(None);
        };
        if self
            .mounts(scope)?
            .iter()
            .filter_map(|m| m.schema.areas.get(&area.id))
            .any(|a| {
                a.fields
                    .get(&field.id)
                    .is_some_and(|f| f.kind != 3 || f.flags & 1 == 0)
            })
        {
            return Ok(None);
        }
        let descending = q.order[0].1;
        q.order.clear();
        let ids = self.query_select(scope, &q)?;
        let mut groups = BTreeMap::<u32, (std::collections::BTreeSet<u32>, &Mount)>::new();
        let mut pending = ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        for mount in self.mounts(scope)?.iter().rev() {
            if pending.is_empty() {
                break;
            }
            let wanted = pending.iter().copied().collect::<Vec<_>>();
            let entries = match self.engine.lookup_entries(&mount.key, &wanted)? {
                Some(e) => e,
                None => self
                    .extra
                    .lookup_entries(&mount.key, &wanted)?
                    .ok_or("projection mount missing")?,
            };
            for entry in entries {
                pending.remove(&entry.oid);
                if entry.block != u32::MAX {
                    groups
                        .entry(mount.volume.id)
                        .or_insert_with(|| (Default::default(), mount))
                        .0
                        .insert(entry.oid);
                }
            }
        }
        let mut values = BTreeMap::<u32, Vec<String>>::new();
        for (_, (oids, mount)) in groups {
            if !mount
                .schema
                .areas
                .get(&area.id)
                .is_some_and(|a| a.fields.contains_key(&field.id))
            {
                continue;
            }
            let path = super::script_volume_path(&self.root, &mount.key);
            let indexed =
                super::mainkey::MainKey::open(&path)?.indexed_strings(area.id, field.id, &oids)?;
            for (oid, raw) in indexed {
                let data = if raw.len() > 1 {
                    // Index order differs from repeated field storage order.
                    self.read(scope, oid)?
                        .ok_or("numeric projection document missing")?
                        .fields
                        .iter()
                        .filter(|f| f.id == field.id && f.wire_type == 3)
                        .filter_map(|f| match &f.value {
                            super::fields::Value::String(b) => Some(super::decode(b)),
                            _ => None,
                        })
                        .collect()
                } else {
                    raw.iter().map(|b| super::decode(b)).collect()
                };
                values.insert(oid, data);
            }
        }
        // The generic ORDER BY groups numeric values before malformed text.
        // Fall back for such legacy data instead of mixing numeric and lexical
        // comparisons in a non-transitive comparator.
        if values
            .values()
            .filter_map(|v| v.first())
            .any(|v| crate::ks::sql::numeric_sort_key(v).is_none())
        {
            return Ok(None);
        }
        let mut ids = ids;
        ids.sort_by(|a, b| {
            let av = values.get(a).and_then(|v| v.first());
            let bv = values.get(b).and_then(|v| v.first());
            match (av, bv) {
                (None, None) => std::cmp::Ordering::Equal,
                (None, _) => std::cmp::Ordering::Greater,
                (_, None) => std::cmp::Ordering::Less,
                (Some(a), Some(b)) => {
                    let c = crate::ks::sql::numeric_sort_key(a)
                        .cmp(&crate::ks::sql::numeric_sort_key(b));
                    if descending {
                        c.reverse()
                    } else {
                        c
                    }
                }
            }
        });
        let cells = ids
            .iter()
            .map(|id| vec![ColumnValues::Strings(values.remove(id).unwrap_or_default())])
            .collect();
        Ok(Some(QueryData {
            area: Some(area.id),
            oids: ids,
            cells,
            fulltext: Default::default(),
        }))
    }
    fn reserve_document(&mut self, scope: u32, area: u16) -> Result<u32, String> {
        super::writer::reserve(&self.write_path(scope, area)?)
    }
    fn write_document(
        &mut self,
        scope: u32,
        record: &Record,
        expected: Option<&Record>,
    ) -> Result<(), String> {
        let oid = record.integer(1024).ok_or("Document identity missing")?;
        self.refresh_writes();
        if self.read(scope, oid)?.as_ref() != expected {
            return Err("Документ уже изменён. Откройте актуальную редакцию.".into());
        }
        // DeleteObject stages an area-0 record over a read-only source. Area 0
        // exists in several writable volumes; retain the source area's routing
        // so that the following Delete writes its marker to the matching volume.
        let target_area = if record.area == 0 {
            expected.map_or(0, |original| original.area)
        } else {
            record.area
        };
        let path = self.write_path(scope, target_area)?;
        let in_target = self
            .record_mount(scope, oid)?
            .is_some_and(|(_, mount)| super::script_volume_path(&self.root, &mount.key) == path);
        super::writer::commit(&path, record, if in_target { expected } else { None })?;
        self.refresh_writes();
        Ok(())
    }
    fn schema_revision(&self) -> u64 {
        self.next_mount as u64
    }
    fn delete_document(
        &mut self,
        scope: u32,
        oid: u32,
        hole: bool,
        expected: &Record,
    ) -> Result<(), String> {
        self.refresh_writes();
        if self.read(scope, oid)?.as_ref() != Some(expected) {
            return Err("Документ уже изменён. Откройте актуальную редакцию.".into());
        }
        let (_, mount) = self
            .record_mount(scope, oid)?
            .ok_or("Document already deleted")?;
        let path = if mount.writable {
            super::script_volume_path(&self.root, &mount.key)
        } else if hole {
            self.write_path(scope, expected.area)?
        } else {
            return Err("Document volume is read-only; a deletion marker is required".into());
        };
        if !path
            .canonicalize()
            .map_err(|e| e.to_string())?
            .starts_with(self.root.canonicalize().map_err(|e| e.to_string())?)
        {
            return Err("DB4 deletion outside data root".into());
        }
        super::writer::delete(
            &path,
            oid,
            hole,
            if mount.writable { Some(expected) } else { None },
        )?;
        self.refresh_writes();
        Ok(())
    }
    fn finish_profile(&self) {
        let schema = self.timings.schema.replace((0, 0));
        let record = self.timings.record.replace((0, 0));
        let query = self.timings.query.replace((0, 0));
        if std::env::var_os("KODEKS_PROFILE").is_some_and(|v| v == "1")
            && schema.0 + record.0 + query.0 > 0
        {
            eprintln!(
                "DB profile (inclusive): schema={} / {} us, record={} / {} us, query={} / {} us",
                schema.0, schema.1, record.0, record.1, query.0, query.1
            );
        }
    }
    fn create(&mut self) -> Result<u32, String> {
        if self.scopes.len() >= 128 {
            return Err("DataBase context limit".into());
        }
        let id = self.next_scope;
        self.next_scope += 1;
        self.scopes.insert(id, vec![]);
        Ok(id)
    }
    fn open(&mut self, scope: u32, path: &str, flags: &[bool]) -> Result<Volume, OpenError> {
        self.mounts(scope).map_err(OpenError::Unsupported)?;
        if self.trace {
            eprintln!("KS DataBase({scope}).Open: {path}, flags={flags:?}");
        }
        if flags.get(1) == Some(&true) {
            return Err(OpenError::Unsupported(
                "Open third flag true is not implemented".into(),
            ));
        }
        if self.mounts(scope).unwrap().len() >= 256 {
            return Err(OpenError::Unsupported("DataBase mount limit".into()));
        }
        let resolved = super::script_volume_path(&self.root, path);
        std::fs::metadata(&resolved)
            .map_err(|e| OpenError::Io(format!("DataBase({scope}).Open {path}: {e}")))?;
        let (schema, key) = self
            .extra
            .mount_readonly(&self.root, path)
            .map_err(OpenError::Unsupported)?;
        let volume =
            super::volume::metadata(&resolved, self.next_mount).map_err(OpenError::Unsupported)?;
        self.attach(
            scope,
            Mount {
                writable: self.writable.contains(&super::volume_key(&key))
                    || scope != 0 && !self.writable.is_empty() && flags.first() == Some(&true),
                volume: volume.clone(),
                key,
                schema,
            },
        )?;
        self.next_mount += 1;
        Ok(volume)
    }
    fn remove(&mut self, scope: u32, volume: u32) -> Result<(), String> {
        let mounts = self
            .scopes
            .get_mut(&scope)
            .ok_or("unknown DataBase identity")?;
        let i = mounts
            .iter()
            .position(|m| m.volume.id == volume)
            .ok_or("Volume is not attached to this DataBase")?;
        if self.trace {
            eprintln!("KS DataBase({scope}).Remove: {}", mounts[i].volume.name);
        }
        mounts.remove(i);
        self.schemas.borrow_mut().remove(&scope);
        self.queries.borrow_mut().retain(|(s, _), _| *s != scope);
        self.fulltext.borrow_mut().retain(|(s, _), _| *s != scope);
        self.word_statistics
            .borrow_mut()
            .retain(|(s, _), _| *s != scope);
        *self.records.borrow_mut() = RecordCache::default();
        self.next_mount += 1;
        self.record_mounts
            .borrow_mut()
            .retain(|(s, _), _| *s != scope);
        Ok(())
    }
    fn volumes(&self, scope: u32) -> Result<Vec<Volume>, String> {
        Ok(self
            .mounts(scope)?
            .iter()
            .map(|m| m.volume.clone())
            .collect())
    }
    fn schema(&self, scope: u32) -> Result<Schema, String> {
        Ok((*self.shared_schema(scope)?).clone())
    }
    fn shared_schema(&self, scope: u32) -> Result<std::sync::Arc<Schema>, String> {
        let _measure = Measure::new(&self.timings.schema);
        if let Some(s) = self.schemas.borrow().get(&scope) {
            return Ok(s.clone());
        }
        let mut schema = Schema::default();
        let mounts = self.mounts(scope)?;
        for (i, m) in mounts.iter().enumerate() {
            schema
                .merge_native(&m.schema)
                .map_err(|conflict| conflict_text(scope, &conflict, &mounts[..=i]))?;
        }
        let schema = std::sync::Arc::new(schema);
        self.schemas.borrow_mut().insert(scope, schema.clone());
        Ok(schema)
    }
    fn read(&self, scope: u32, oid: u32) -> Result<Option<Record>, String> {
        Ok(self.record(scope, oid)?.map(|(r, _)| r))
    }
    fn metadata(
        &self,
        scope: u32,
        oid: u32,
    ) -> Result<Option<super::fields::RecordMetadata>, String> {
        Ok(self
            .metadata_mount(scope, oid)?
            .map(|(metadata, _)| metadata))
    }
    fn named(&self, scope: u32, name: &str) -> Result<Option<Record>, String> {
        let name = name.trim_start_matches('/').to_lowercase();
        for m in self.mounts(scope)?.iter().rev() {
            for area in [900, 901, 1010] {
                if !m
                    .schema
                    .areas
                    .get(&area)
                    .is_some_and(|a| a.fields.get(&1).is_some_and(|f| f.kind == 2))
                {
                    continue;
                }
                let predicate = crate::ks::sql::BoundPredicate::Text {
                    attribute: 1,
                    kind: 2,
                    pattern: crate::ks::sql::Matcher::Exact(name.clone()),
                    folded: name.clone(),
                    like: false,
                };
                let dir = VirtualDir {
                    mapped_to: m.key.clone(),
                    ..Default::default()
                };
                let mut ids = self.engine.query_indexed(&dir, area, &predicate)?;
                ids.extend(self.extra.query_indexed(&dir, area, &predicate)?);
                ids.sort_unstable();
                ids.dedup();
                for oid in ids {
                    if let Some((r, _)) = self.record(scope, oid)? {
                        if r.area == area
                            && r.string(1)
                                .map(super::decode)
                                .is_some_and(|s| s.eq_ignore_ascii_case(&name))
                        {
                            return Ok(Some(r));
                        }
                    }
                }
            }
        }
        Ok(None)
    }
    fn document_volume(&self, scope: u32, oid: u32) -> Result<Volume, String> {
        self.metadata_mount(scope, oid)?
            .map(|(_, mount)| mount.volume.clone())
            .ok_or_else(|| format!("Document({oid}) absent from DataBase({scope})"))
    }
    fn document_revision(&self, scope: u32, oid: u32) -> Result<Option<u16>, String> {
        let Some((_, mount)) = self.metadata_mount(scope, oid)? else {
            return Ok(None);
        };
        match self.engine.record_revision(oid, &mount.key)? {
            Some(v) => Ok(Some(v)),
            None => self.extra.record_revision(oid, &mount.key),
        }
    }
    fn attribute(&self, scope: u32, oid: u32, name: &str) -> Result<u16, String> {
        let (r, _) = self
            .metadata_mount(scope, oid)?
            .ok_or("document absent from its DataBase")?;
        let s = self.shared_schema(scope)?;
        s.areas
            .get(&r.area)
            .and_then(|a| a.fields.values().find(|f| f.name == name))
            .map(|f| f.id)
            .ok_or_else(|| format!("unknown attribute {name}"))
    }
    fn fulltext_result(
        &self,
        scope: u32,
        sql: &str,
    ) -> Result<super::free_text::ResultData, String> {
        self.refresh_writes();
        let key = (scope, format!("{:?}", crate::ks::sql::parse(sql)?));
        Ok(self
            .fulltext
            .borrow()
            .get(&key)
            .cloned()
            .unwrap_or_default())
    }
    fn query(&self, scope: u32, sql: &str) -> Result<Vec<u32>, String> {
        self.refresh_writes();
        self.query_select(scope, &crate::ks::sql::parse(sql)?)
    }
}
impl<E: Deref<Target = Db4Engine>> Contexts<E> {
    fn indexed_query(
        &self,
        scope: u32,
        area: u16,
        predicate: &crate::ks::sql::BoundPredicate,
    ) -> Result<Vec<u32>, String> {
        use std::collections::{BTreeMap, BTreeSet};
        let mut by_mount = BTreeMap::new();
        let mut pending = BTreeSet::new();
        for mount in self.mounts(scope)? {
            let ids = match self.engine.indexed_volume(&mount.key, area, predicate)? {
                Some(ids) => ids,
                None => self
                    .extra
                    .indexed_volume(&mount.key, area, predicate)?
                    .unwrap_or_default(),
            };
            pending.extend(ids.iter().copied());
            by_mount.insert(mount.volume.id, ids);
        }
        let mut live = Vec::new();
        // Resolve the union in reverse mount order, including deletion markers.
        // A match from an old volume must not survive a newer nonmatching copy.
        for mount in self.mounts(scope)?.iter().rev() {
            if pending.is_empty() {
                break;
            }
            let ids = pending.iter().copied().collect::<Vec<_>>();
            let entries = match self.engine.lookup_entries(&mount.key, &ids)? {
                Some(entries) => entries,
                None => self
                    .extra
                    .lookup_entries(&mount.key, &ids)?
                    .ok_or("mounted volume missing from indexed readers")?,
            };
            let hits = &by_mount[&mount.volume.id];
            for entry in entries {
                pending.remove(&entry.oid);
                if entry.block != u32::MAX && hits.binary_search(&entry.oid).is_ok() {
                    live.push(entry.oid);
                    self.cache_record_mount(scope, entry.oid, Some(mount.volume.id));
                }
            }
        }
        live.sort_unstable();
        if live.len() > crate::ks::sql::MAX_SQL_ROWS {
            return Err("SQL row limit (1000000)".into());
        }
        Ok(live)
    }
    fn query_select(&self, scope: u32, q: &crate::ks::sql::Select) -> Result<Vec<u32>, String> {
        self.refresh_writes();
        let key = (scope, format!("{q:?}"));
        if let Some(ids) = self.queries.borrow().get(&key) {
            return Ok(ids.clone());
        }
        let started = std::time::Instant::now();
        let result = self.query_select_uncached(scope, q);
        if started.elapsed().as_millis() >= 500
            && std::env::var_os("KODEKS_PROFILE").is_some_and(|v| v == "1")
        {
            eprintln!(
                "DB slow query: elapsed_ms={}, result={:?}, query={q:?}",
                started.elapsed().as_millis(),
                result.as_ref().map(Vec::len)
            );
        }
        let ids = result?;
        let mut cache = self.queries.borrow_mut();
        if cache.len() < 512 && cache.values().map(Vec::len).sum::<usize>() + ids.len() <= 2_000_000
        {
            cache.insert(key, ids.clone());
        }
        Ok(ids)
    }
    fn query_select_uncached(
        &self,
        scope: u32,
        q: &crate::ks::sql::Select,
    ) -> Result<Vec<u32>, String> {
        let _measure = Measure::new(&self.timings.query);
        if ["KODEKS_TRACE_SQL", "KODEKS_TRACE_SQL_REQUEST"]
            .iter()
            .any(|name| std::env::var_os(name).is_some_and(|v| v == "1"))
        {
            eprintln!(
                "DB query start: area={}, predicate={}",
                q.area,
                format!("{:?}", q.predicate)
                    .chars()
                    .take(1000)
                    .collect::<String>()
            );
        }
        let schema = self.shared_schema(scope)?;
        let area=schema.area(&q.area).ok_or_else(||format!("SQL area '{}' is absent from DataBase({scope}); mounted volumes: {}. Check the BaseN files and paths in config.db4",q.area,self.mounts(scope).unwrap().iter().map(|m|m.key.as_str()).collect::<Vec<_>>().join(", ")))?;
        q.fields(area)?;
        let driving_ids = if q.predicate.has_subquery() {
            let mut driving = q.clone();
            driving.predicate = q.predicate.without_subqueries().without_context();
            driving.projection = crate::ks::host::Projection(vec![crate::ks::host::Column::Oid]);
            driving.order.clear();
            if driving.predicate.bind(area)?.index_cost() <= 2 {
                match self.query_select(scope, &driving) {
                    Ok(ids) if ids.len() <= 4096 => Some(ids),
                    Ok(_) => None,
                    Err(e) if e.contains("limit") => None,
                    Err(e) => return Err(e),
                }
            } else {
                None
            }
        } else {
            None
        };
        let predicate = q.predicate.resolve(&|outer_field, nested| {
            let mut restricted = nested.clone();
            if let Some(driving) = &driving_ids {
                let projected = match nested.projection.0.as_slice() {
                    [crate::ks::host::Column::Oid] => Some("OID"),
                    [crate::ks::host::Column::Field(name)] => Some(name.as_str()),
                    _ => None,
                };
                if let Some(projected) = projected {
                    let attr = if outer_field.eq_ignore_ascii_case("OID") {
                        None
                    } else {
                        Some(
                            area.fields
                                .values()
                                .find(|f| f.name == outer_field && f.kind == 5)
                                .ok_or("IN requires link field")?
                                .id,
                        )
                    };
                    let mut targets = std::collections::BTreeSet::new();
                    for &oid in driving {
                        if let Some(attr) = attr {
                            if let Some(r) = self.read(scope, oid)? {
                                for f in
                                    r.fields.iter().filter(|f| f.id == attr && f.wire_type == 5)
                                {
                                    if let super::fields::Value::Integer(id) = f.value {
                                        targets.insert(id);
                                    }
                                }
                            }
                        } else {
                            targets.insert(oid);
                        }
                    }
                    restricted.predicate = crate::ks::sql::Predicate::And(
                        Box::new(restricted.predicate),
                        Box::new(crate::ks::sql::Predicate::oid_set(
                            projected,
                            &targets.into_iter().collect::<Vec<_>>(),
                        )),
                    );
                }
            }
            let ids = self.query_select(scope, &restricted)?;
            if let [crate::ks::host::Column::Field(name)] = nested.projection.0.as_slice() {
                let area = schema.area(&nested.area).ok_or("subquery area missing")?;
                let field = area
                    .fields
                    .values()
                    .find(|f| f.name == *name && f.kind == 5)
                    .ok_or("IN subquery column must be a link")?;
                let mut links = std::collections::BTreeSet::new();
                for oid in ids {
                    if let Some(record) = self.read(scope, oid)? {
                        for f in record
                            .fields
                            .iter()
                            .filter(|f| f.id == field.id && f.wire_type == 5)
                        {
                            if let super::fields::Value::Integer(id) = f.value {
                                links.insert(id);
                            }
                        }
                    }
                    if links.len() > crate::ks::sql::MAX_SQL_ROWS {
                        return Err("subquery link limit".into());
                    }
                }
                Ok(links.into_iter().collect())
            } else {
                Ok(ids)
            }
        })?;
        let fulltext = RefCell::new(super::free_text::ResultData::default());
        let predicate = if predicate.has_context() {
            let mut driving = q.clone();
            driving.predicate = predicate.without_context();
            driving.order.clear();
            driving.projection = crate::ks::host::Projection(vec![crate::ks::host::Column::Oid]);
            let restriction = if driving.predicate.bind(area)?.index_cost() < 100000 {
                match self.query_select(scope, &driving) {
                    Ok(ids) => Some(ids.into_iter().collect::<std::collections::BTreeSet<_>>()),
                    Err(e) if e.contains("limit") => None,
                    Err(e) => return Err(e),
                }
            } else {
                None
            };
            predicate.resolve_context(&|s, intelligent| {
                if intelligent {
                    let result = self.free_text_search(
                        scope,
                        area.id,
                        s,
                        restriction.as_ref(),
                        &if q.predicate.has_subquery() {
                            crate::ks::sql::BoundPredicate::All
                        } else {
                            driving.predicate.bind(area)?
                        },
                    )?;
                    let ids = result.ranks.keys().copied().collect();
                    let mut all = fulltext.borrow_mut();
                    for (id, rank) in result.ranks {
                        *all.ranks.entry(id).or_default() += rank;
                    }
                    all.statistics.extend(result.statistics);
                    Ok(ids)
                } else {
                    self.context_search(scope, area.id, s, restriction.as_ref())
                }
            })?
        } else {
            predicate
        }
        .bind(area)?;
        let (ids, indexed) = if let Some(ids) = predicate.oid_candidates() {
            (ids, false)
        } else {
            (self.indexed_query(scope, area.id, &predicate)?, true)
        };
        if ids.len() > crate::ks::sql::MAX_SQL_ROWS {
            return Err("SQL row limit".into());
        }
        if indexed
            && q.order
                .iter()
                .all(|(name, _)| name.eq_ignore_ascii_case("OID"))
        {
            let mut ids = ids;
            if q.order.first().is_some_and(|(_, desc)| *desc) {
                ids.reverse();
            }
            return Ok(ids);
        }
        // Old index hits cannot override an updated record or a tombstone.
        let mut live = Vec::new();
        for id in ids {
            if let Some(r) = self.read(scope, id)? {
                if r.area == area.id && predicate.matches_record(id, &r) {
                    live.push((id, r));
                }
            }
        }
        let mut fulltext = fulltext.into_inner();
        super::free_text::normalize(&mut fulltext.ranks);
        let mut order = Vec::new();
        for (name, descending) in &q.order {
            if name.eq_ignore_ascii_case("FREETEXT_RANK") {
                order.push((Some(u16::MAX), *descending));
                continue;
            }
            if name.eq_ignore_ascii_case("OID") {
                order.push((None, *descending));
                continue;
            }
            let f = area
                .fields
                .values()
                .find(|f| f.name == *name)
                .ok_or_else(|| format!("SQL sort field {name} absent"))?;
            if !matches!(f.kind, 1 | 2 | 3 | 5 | 12) {
                return Err(format!(
                    "SQL sorting native type {} is not implemented",
                    f.kind
                ));
            }
            order.push((Some(f.id), *descending));
        }
        // 006f3850 chooses an index walk per volume at 50 selected rows
        // (or at 10% density). Its link keys are big-endian; the small
        // in-memory sorter compares the stored little-endian link bytes.
        // MultiIndex merges those byte keys unchanged. Preserve this native
        // ordering, including mixed old/current volumes, without scanning
        // bodies or building an index for the entire database.
        let mut link_sort_keys = BTreeMap::new();
        if let Some((Some(attr), _)) = order.first() {
            if area.fields.get(attr).is_some_and(|f| f.kind == 5) {
                let mut groups = BTreeMap::<u32, (Vec<u32>, &Mount)>::new();
                for (oid, _) in &live {
                    if let Some((_, mount)) = self.record_mount(scope, *oid)? {
                        groups
                            .entry(mount.volume.id)
                            .or_insert_with(|| (Vec::new(), mount))
                            .0
                            .push(*oid);
                    }
                }
                for (_, (ids, mount)) in groups {
                    let indexed = mount
                        .schema
                        .areas
                        .get(&area.id)
                        .and_then(|a| a.fields.get(attr))
                        .is_some_and(|f| f.flags & 1 != 0);
                    let dense = if indexed && ids.len() <= 49 {
                        let mut tree = super::mainkey::MainKey::open(std::path::Path::new(
                            &mount.volume.name,
                        ))?;
                        match tree.area_candidates(area.id, 499) {
                            Ok(all) => all.len() / 10 <= ids.len(),
                            Err(e) if e == "SQL row limit: compressed link index exceeds 499; result not truncated" => false,
                            Err(e) => return Err(e),
                        }
                    } else {
                        false
                    };
                    let indexed_order = indexed && (ids.len() > 49 || dense);
                    for oid in ids {
                        link_sort_keys.insert(oid, indexed_order);
                    }
                }
            }
        }
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
        enum Key {
            Number(u32),
            Numeric(u64),
            Text(Vec<u8>),
            Date(i16, u8, u8),
            Stamp([u8; 8]),
            Missing,
        }
        let key = |id: u32, r: &Record, attr: Option<u16>| {
            let Some(attr) = attr else {
                return Key::Number(id);
            };
            if attr == u16::MAX {
                return Key::Number(fulltext.ranks.get(&id).copied().unwrap_or(0));
            }
            use super::fields::Value as F;
            match r.fields.iter().find(|f| f.id == attr).map(|f| &f.value) {
                Some(F::Integer(n)) => {
                    if order.first().is_some_and(|(first, _)| *first == Some(attr)) {
                        if let Some(indexed) = link_sort_keys.get(&id) {
                            return Key::Text(
                                if *indexed {
                                    n.to_be_bytes()
                                } else {
                                    n.to_le_bytes()
                                }
                                .to_vec(),
                            );
                        }
                    }
                    Key::Number(*n)
                }
                Some(F::String(v)) => {
                    // Native text sort uses case-folded CP1251 byte order (including Ё).
                    let lower = super::decode(v).to_lowercase();
                    if area.fields.get(&attr).is_some_and(|f| f.kind == 3) {
                        if let Some(key) = crate::ks::sql::numeric_sort_key(&lower) {
                            return Key::Numeric(key);
                        }
                    }
                    Key::Text(encoding_rs::WINDOWS_1251.encode(&lower).0.into_owned())
                }
                Some(F::Date(v)) => Key::Date(i16::from_le_bytes([v[0], v[1]]), v[2], v[3]),
                Some(F::Timestamp(v)) => Key::Stamp(*v),
                _ => Key::Missing,
            }
        };
        live.sort_by(|(ai, a), (bi, b)| {
            for (attr, desc) in &order {
                let ak = key(*ai, a, *attr);
                let bk = key(*bi, b, *attr);
                // Native ORDER BY leaves absent fields at the end in both
                // directions (the unsigned-document register uses DESC).
                if matches!(ak, Key::Missing) != matches!(bk, Key::Missing) {
                    return if matches!(ak, Key::Missing) {
                        std::cmp::Ordering::Greater
                    } else {
                        std::cmp::Ordering::Less
                    };
                }
                let c = ak.cmp(&bk);
                if !c.is_eq() {
                    return if *desc { c.reverse() } else { c };
                }
            }
            std::cmp::Ordering::Equal
        });
        let ids: Vec<_> = live.into_iter().map(|(id, _)| id).collect();
        if std::env::var_os("KODEKS_PROFILE").is_some_and(|v| v == "1")
            && _measure.start.elapsed().as_millis() > 100
        {
            let description = format!("{:?}", q.predicate)
                .chars()
                .take(500)
                .collect::<String>();
            eprintln!(
                "DB slow query: {} ms, {} rows, area={}, predicate={}",
                _measure.start.elapsed().as_millis(),
                ids.len(),
                q.area,
                description
            );
        }
        if !fulltext.statistics.is_empty() {
            let mut cache = self.fulltext.borrow_mut();
            if cache.len() >= 512 {
                cache.clear();
                self.queries.borrow_mut().clear();
            }
            cache.insert((scope, format!("{q:?}")), fulltext);
        }
        Ok(ids)
    }
    fn free_text_search(
        &self,
        scope: u32,
        area: u16,
        text: &str,
        restriction: Option<&std::collections::BTreeSet<u32>>,
        statistics_filter: &crate::ks::sql::BoundPredicate,
    ) -> Result<super::free_text::ResultData, String> {
        use std::collections::{BTreeSet, HashSet};
        let terms = super::free_text::terms(text)?;
        let dictionary = super::morphology::load(&self.root)?;
        let forms: Vec<_> = terms.iter().map(|word| dictionary.forms(word)).collect();
        let mut result = super::free_text::ResultData::default();
        let mut candidates: Option<BTreeSet<u32>> = restriction.cloned();
        let mut counts = Vec::new();
        let mut memberships: Vec<Vec<(u32, HashSet<u32>)>> = Vec::new();
        let mut term_weights = Vec::new();
        let mut membership_budget = 0;
        for (word, forms) in terms.iter().zip(&forms) {
            let mut ids = BTreeSet::new();
            let mut statistics = Vec::new();
            let mut membership = Vec::new();
            let mut stat_mounts = std::collections::BTreeMap::new();
            let mut weights_by_mount = BTreeMap::new();
            let stat_key = (
                scope,
                format!(
                    "{area}:{statistics_filter:?}:{}",
                    forms.iter().cloned().collect::<Vec<_>>().join(" ")
                ),
            );
            let prefixes = super::morphology::prefixes(forms);
            for mount in self.mounts(scope)? {
                if !mount.schema.areas.contains_key(&area) {
                    continue;
                }
                let path = super::script_volume_path(&self.root, &mount.key);
                let mut index = super::mainkey::MainKey::open(&path)?;
                let mut mount_hits = HashSet::new();
                let mut mount_statistics = HashSet::new();
                let mut mount_weights = BTreeMap::<u32, u32>::new();
                let exact = encoding_rs::WINDOWS_1251.encode(word).0.into_owned();
                for prefix in &prefixes {
                    let (bytes, _, _) = encoding_rs::WINDOWS_1251.encode(prefix);
                    let (hits, stat, weights) = index.word_weights(
                        &bytes,
                        true,
                        Some(forms),
                        candidates.as_ref(),
                        crate::ks::sql::MAX_INDEX_CANDIDATES,
                        Some(&exact),
                    )?;
                    for (oid, score) in weights {
                        *mount_weights.entry(oid).or_default() += score;
                    }
                    statistics.extend(stat.iter().copied());
                    mount_statistics.extend(stat);
                    mount_hits.extend(hits);
                    if statistics.len() > 16_000_000 {
                        return Err("FREETEXT statistics budget exceeded".into());
                    }
                }
                let allowed =
                    match self
                        .engine
                        .indexed_volume(&mount.key, area, statistics_filter)?
                    {
                        Some(ids) => ids,
                        None => self
                            .extra
                            .indexed_volume(&mount.key, area, statistics_filter)?
                            .unwrap_or_default(),
                    };
                mount_statistics.retain(|id| allowed.binary_search(id).is_ok());
                stat_mounts.insert(mount.volume.id, mount_statistics);
                weights_by_mount.insert(mount.volume.id, mount_weights);
                membership_budget += mount_hits.len();
                if membership_budget > 4_000_000 {
                    return Err("FREETEXT posting budget exceeded; narrow the search".into());
                }
                ids.extend(mount_hits.iter().copied());
                membership.push((mount.volume.id, mount_hits));
                if ids.len() > crate::ks::sql::MAX_INDEX_CANDIDATES {
                    return Err("FREETEXT candidate limit (8000000); narrow the search".into());
                }
            }
            memberships.push(membership);
            term_weights.push(weights_by_mount);
            candidates = Some(ids);
            statistics.sort_unstable();
            statistics.dedup();
            let cached = self.word_statistics.borrow().get(&stat_key).copied();
            let count = if let Some(count) = cached {
                count
            } else {
                let mut pending: BTreeSet<_> = statistics.into_iter().collect();
                let mut count = 0;
                for mount in self.mounts(scope)?.iter().rev() {
                    if pending.is_empty() {
                        break;
                    }
                    let path = super::script_volume_path(&self.root, &mount.key);
                    let hits = super::mainkey::MainKey::open(&path)?
                        .lookup_many(&pending.iter().copied().collect::<Vec<_>>())?;
                    for e in hits {
                        pending.remove(&e.oid);
                        if e.block != u32::MAX
                            && stat_mounts
                                .get(&mount.volume.id)
                                .is_some_and(|s| s.contains(&e.oid))
                        {
                            count += 1;
                        }
                    }
                }
                let mut cache = self.word_statistics.borrow_mut();
                if cache.len() >= 512 {
                    cache.clear();
                }
                cache.insert(stat_key, count);
                count
            };
            counts.push(count);
            result.statistics.push((word.clone(), count));
        }
        let candidates = candidates.unwrap_or_default();
        // Candidate/posting budgets are enforced while reading the indexes.
        // Apply the ordinary SQL result limit after winning-volume and area
        // filtering, without a separate 10,000-hit limit for full-text search.
        for id in candidates {
            if let Some((record, mount)) = self.record_mount(scope, id)? {
                // Validate winning-volume index membership only after intersection.
                // An old version's posting must not match a newer unindexed copy.
                if !memberships.iter().all(|term| {
                    term.iter()
                        .any(|(v, ids)| *v == mount.volume.id && ids.contains(&id))
                }) {
                    continue;
                }
                let rank = term_weights
                    .iter()
                    .zip(&counts)
                    .map(|(by_mount, count)| {
                        let weight = by_mount
                            .get(&mount.volume.id)
                            .and_then(|m| m.get(&id))
                            .copied()
                            .unwrap_or(0);
                        let rarity = if *count == 0 {
                            0
                        } else {
                            (31 - (64 - count.leading_zeros())).max(0)
                        };
                        weight.saturating_mul(rarity)
                    })
                    .sum::<u32>();
                if record.area == area && rank != 0 {
                    result.ranks.insert(id, rank);
                }
            }
        }
        // Native 00833500: the strongest 39 hits contribute one eighth of
        // their raw weight to matching documents referenced by their fields.
        let mut strongest = result
            .ranks
            .iter()
            .map(|(&id, &rank)| (id, rank))
            .collect::<Vec<_>>();
        strongest.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        strongest.truncate(39);
        for (id, score) in strongest {
            if score < 8 {
                continue;
            }
            if let Some(record) = self.read(scope, id)? {
                for field in record.fields.iter().filter(|f| f.wire_type == 5) {
                    if let super::fields::Value::Integer(target) = field.value {
                        if target != id {
                            if let Some(rank) = result.ranks.get_mut(&target) {
                                *rank += score / 8;
                            }
                        }
                    }
                }
            }
        }
        result
            .statistics
            .sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        Ok(result)
    }
    fn context_search(
        &self,
        scope: u32,
        area: u16,
        text: &str,
        restriction: Option<&std::collections::BTreeSet<u32>>,
    ) -> Result<Vec<u32>, String> {
        let expr = super::text_query::parse(text)?;
        let mut lookup =
            |w: &super::text_query::Word, restriction: Option<&std::collections::BTreeSet<u32>>| {
                self.context_word(scope, area, &w.text, w.prefix, restriction)
                    .map(|ids| ids.into_iter().collect())
            };
        let ids = expr.candidates(restriction, &mut lookup)?;
        let mut result = Vec::new();
        for id in ids {
            if let Some(record) = self.read(scope, id)? {
                let fields = record
                    .fields
                    .iter()
                    .filter(|f| matches!(f.wire_type, 2 | 3 | 4))
                    .filter_map(|f| crate::document::bytes(f).ok())
                    .map(|s| super::text_query::words(&super::decode(s)))
                    .collect::<Vec<_>>();
                if record.area == area && expr.matches(&fields) {
                    result.push(id);
                }
            }
        }
        Ok(result)
    }
    fn context_word(
        &self,
        scope: u32,
        area: u16,
        word: &str,
        prefix: bool,
        restriction: Option<&std::collections::BTreeSet<u32>>,
    ) -> Result<Vec<u32>, String> {
        if restriction.is_some_and(|r| r.is_empty()) {
            return Ok(vec![]);
        }
        let (bytes, _, bad) = encoding_rs::WINDOWS_1251.encode(word);
        if bad {
            return Err("context search word cannot be encoded as CP1251".into());
        }
        let mut out = std::collections::BTreeSet::new();
        for mount in self.mounts(scope)? {
            if !mount.schema.areas.contains_key(&area) {
                continue;
            }
            let path = super::script_volume_path(&self.root, &mount.key);
            let ids = super::mainkey::MainKey::open(&path)?
                .word_hits(&bytes, prefix, None, restriction, 500000)
                .map(|r| r.0)
                .map_err(|e| format!("{}: {e}", mount.key))?;
            out.extend(ids);
            if out.len() > 500000 {
                return Err("context candidate limit (500000); narrow the search".into());
            }
        }
        Ok(out.into_iter().collect())
    }
}
