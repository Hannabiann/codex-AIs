pub mod context;
mod decoded_cache;
pub mod fields;
pub mod free_text;
mod indexed_query;
pub mod mainkey;
pub mod morphology;
pub mod registry;
pub mod schema;
mod shared_query;
mod text_column;
pub mod text_query;
pub mod volume;
use encoding_rs::WINDOWS_1251;
use fields::{parse_record, Record};
use std::collections::{BTreeMap, HashMap, HashSet};

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const MAX_CANDIDATE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_INDEX_ENTRIES: usize = 50_000;
const MAX_TOTAL_ENTRIES: usize = 200_000;

pub fn decode(bytes: &[u8]) -> String {
    WINDOWS_1251.decode(bytes).0.into_owned()
}
#[derive(Debug, Clone)]
pub struct Location {
    pub volume: usize,
    pub offset: u64,
    pub oid: u32,
}
#[derive(Debug, Clone, Default)]
pub struct VirtualDir {
    pub path: String,
    pub title: String,
    pub kind: u32,
    pub mapped_to: String,
    pub bases: Vec<String>,
    pub access: Option<String>,
    pub auth: BTreeMap<String, ConfigValue>,
    pub properties: BTreeMap<String, ConfigValue>,
}
#[derive(Debug, Default)]
pub struct ScanStats {
    pub accepted: usize,
    pub rejected: usize,
}
#[derive(Debug, Default)]
struct VolumeIndex {
    names: HashMap<String, Option<Location>>,
    oids: HashMap<u32, Option<Location>>,
    stats: ScanStats,
}
#[derive(Debug)]
pub struct Volume {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    index: OnceLock<Result<VolumeIndex, String>>,
}
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigValue {
    Text(String),
    Integer(i32),
    Invalid(String),
}
#[derive(Debug, Default)]
pub struct Db4Engine {
    decoded: std::sync::Arc<std::sync::Mutex<decoded_cache::DecodedCache>>,
    pub volumes: Vec<Volume>,
    pub virtual_dirs: Vec<VirtualDir>,
    pub mount_report: Vec<String>,
    pub web_config: BTreeMap<String, ConfigValue>,
    pub registry: registry::Registry,
    scan_lock: Mutex<usize>,
    schemas: Mutex<BTreeMap<usize, std::sync::Arc<schema::Schema>>>,
    // At most 64 read-only handles, 64 MiB of MainKey nodes and 64 MiB of
    // index directories. Original and replacement servers must remain
    // mutually exclusive for this immutable snapshot.
    readers: Mutex<BTreeMap<usize, (u64, mainkey::MainKey)>>,
    write_epoch: std::sync::atomic::AtomicU64,
    // Immutable results, keyed by canonical volume path and generation rather
    // than an engine-local mount number. Request/KS state remains independent.
    queries: std::sync::Arc<shared_query::SharedQueries>,
}
pub enum RecordLookup<T = Record> {
    Missing,
    Deleted,
    Present(T),
}
fn le16(b: &[u8], p: usize) -> u16 {
    u16::from_le_bytes(b[p..p + 2].try_into().unwrap())
}
fn le32(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap())
}
fn path_key(s: &str) -> String {
    let p = format!("/{}", s.trim_matches('/'));
    p.to_lowercase()
}
/// Reconstruct a C stream, including allocation-map gaps for large records.
/// Offsets are physical byte offsets, NOT fixed 4096-byte page numbers.
fn payload_from_reader<F>(hdr: &[u8], size: u64, offset: u64, read: F) -> Result<Vec<u8>, String>
where
    F: FnMut(u64, usize) -> Result<Vec<u8>, String>,
{
    payload_prefix_from_reader(hdr, size, offset, usize::MAX, read)
}
fn payload_prefix_from_reader<F>(
    hdr: &[u8],
    size: u64,
    offset: u64,
    maximum: usize,
    mut read: F,
) -> Result<Vec<u8>, String>
where
    F: FnMut(u64, usize) -> Result<Vec<u8>, String>,
{
    if hdr.len() < 0x121 || &hdr[..4] != b"DB40" {
        return Err("not a DB40 volume".into());
    }
    let h = read(offset, 7)?;
    if h.len() != 7 || h[4] != b'C' {
        return Err("not a C record".into());
    }
    let length = le32(&h, 0) as usize;
    if !(6..=fields::MAX_FIELD).contains(&length) {
        return Err("record length out of range".into());
    }
    let unit = le16(hdr, 0x15) as u64;
    let mult = le16(hdr, 0x13) as u64;
    if unit == 0 || mult == 0 {
        return Err("invalid allocation geometry".into());
    }
    let skip = unit * mult;
    let span = (mult * 8 + 1) * skip;
    let segmented = le16(hdr, 5) > 0x611 && length as u64 > unit * mult * mult * 8;
    let length = length.min(maximum);
    let mut pos = offset.checked_add(7).ok_or("offset overflow")?;
    let mut out = Vec::with_capacity(length);
    while out.len() < length {
        let mut take = length - out.len();
        if segmented {
            let rel = pos.checked_sub(0x121).ok_or("invalid stream position")? % span;
            let rel = if rel == 0 {
                pos += skip;
                skip
            } else {
                rel
            };
            take = take.min((span - rel) as usize);
        }
        if pos
            .checked_add(take as u64)
            .filter(|&p| p <= size)
            .is_none()
        {
            return Err("truncated C stream".into());
        }
        let bytes = read(pos, take)?;
        if bytes.len() != take {
            return Err("short C stream read".into());
        }
        out.extend_from_slice(&bytes);
        pos += take as u64;
        if segmented && out.len() < length {
            pos += skip;
        }
    }
    Ok(out)
}

fn volume_key(s: &str) -> String {
    s.replace('\\', "/").trim_start_matches("./").to_lowercase()
}

fn indexed_catalog(
    volume: &Volume,
    v: usize,
    mut visit: impl FnMut(&Record),
) -> Result<VolumeIndex, String> {
    eprintln!(
        "Reading MainKey catalog {} ({} bytes; no file scan)",
        volume.name, volume.size
    );
    let mut tree = mainkey::MainKey::open(&volume.path)?;
    let entries = tree.entries(MAX_INDEX_ENTRIES)?;
    let hdr = tree.read(0, 289)?;
    let size = tree.snapshot_size();
    let mut budget = 0u64;
    let mut result = VolumeIndex::default();
    for entry in entries {
        if entry.block == u32::MAX {
            continue;
        }
        let off = match tree.record_offset(entry) {
            Ok(off) => off,
            Err(e) => {
                eprintln!("{}: skipped inconsistent indexed entry: {}", volume.name, e);
                result.stats.rejected += 1;
                continue;
            }
        };
        let head = tree.read(off, 7)?;
        budget += le32(&head, 0) as u64;
        if budget > MAX_CANDIDATE_BYTES {
            return Err(format!(
                "{}: named catalog byte budget exceeded; use indexed OID lookup",
                volume.name
            ));
        }
        let loc = Location {
            volume: v,
            offset: off,
            oid: entry.oid,
        };
        result.oids.insert(entry.oid, Some(loc.clone()));
        let parsed = payload_from_reader(&hdr, size, off, |p, n| tree.read(p, n))
            .and_then(|b| parse_record(&b).map_err(|e| e.to_string()));
        match parsed {
            Ok(rec) => {
                if rec.integer(0x400) != Some(entry.oid) {
                    return Err(format!("MainKey OID mismatch at {off:#x}"));
                }
                if rec.code().is_some() || rec.source().is_some() || rec.binary(4).is_some() {
                    if let Some(name) = rec.string(1).filter(|s| s.len() <= 1024) {
                        result
                            .names
                            .entry(decode(name).to_lowercase())
                            .and_modify(|e| *e = None)
                            .or_insert(Some(loc));
                    }
                }
                visit(&rec);
                result.stats.accepted += 1;
            }
            Err(_) => result.stats.rejected += 1,
        }
    }
    eprintln!(
        "MainKey {}: {} decoded records, {} unsupported records",
        volume.name, result.stats.accepted, result.stats.rejected
    );
    Ok(result)
}
/// Read one explicitly requested OID from one file; never creates a named catalog.
pub fn read_file_oid(path: &Path, oid: u32) -> Result<Option<Record>, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    let volume = Volume {
        path: path.to_owned(),
        name: "requested-volume".into(),
        size,
        index: OnceLock::new(),
    };
    let engine = Db4Engine {
        volumes: vec![volume],
        ..Default::default()
    };
    engine.read_oid(oid, "requested-volume")
}
/// KS often changes backslashes to slashes, including Windows verbatim paths
/// returned by canonicalize. Restore native separators before Path::join;
/// `//?/C:/...` is not a valid Win32 verbatim path. Callers still canonicalize
/// and enforce the data-root boundary before registering any volume.
pub(crate) fn script_volume_path(root: &Path, name: &str) -> std::path::PathBuf {
    #[cfg(windows)]
    let normalized = name.replace('/', "\\");
    #[cfg(not(windows))]
    let normalized = name.replace('\\', "/");
    root.join(normalized)
}
impl Db4Engine {
    fn cached_schema(&self, volume: usize) -> Result<std::sync::Arc<schema::Schema>, String> {
        let mut schemas = self.schemas.lock().map_err(|_| "schema cache poisoned")?;
        if let Some(schema) = schemas.get(&volume) {
            return Ok(schema.clone());
        }
        let schema = std::sync::Arc::new(schema::Schema::read(&self.volumes[volume].path)?);
        schemas.insert(volume, schema.clone());
        Ok(schema)
    }
    /// Register only the requested file. Validate its index and schema before publication.
    pub fn mount_readonly(
        &mut self,
        root: &Path,
        name: &str,
    ) -> Result<(schema::Schema, String), String> {
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        let path = script_volume_path(&root, name);
        let canonical = path
            .canonicalize()
            .map_err(|e| format!("Base.Open {name}: {e}"))?;
        if !canonical.starts_with(&root) {
            return Err("Base.Open outside data root".into());
        }
        mainkey::MainKey::open(&canonical)?;
        let schema = schema::Schema::read(&canonical)?;
        self.register(&root, &path, &mut HashSet::new())?;
        let key = self
            .volumes
            .iter()
            .find(|v| v.path == canonical)
            .ok_or("mounted volume registration missing")?
            .name
            .clone();
        Ok((schema, key))
    }

    pub fn init_from_config_root(root: impl AsRef<Path>) -> Result<Self, String> {
        let root = root.as_ref().canonicalize().map_err(|e| e.to_string())?;
        let mut engine = Self::default();
        engine.queries = shared_query::SharedQueries::for_root(&root)?;
        // Only bootstrap names are built in. Never enumerate all database files.
        let mut seen = HashSet::new();
        engine.register(&root, &root.join("config.db4"), &mut seen)?;
        let config_v = 0;
        let mut config = Vec::new();
        let config_index = indexed_catalog(&engine.volumes[config_v], config_v, |r| {
            config.push(r.clone())
        })?;
        // MainKey retains superseded registry rows. Native QueryValue selects
        // by the indexed name and parent; a historical row can have both the
        // same parent and spelling while being absent from the name index.
        let mut tree = mainkey::MainKey::open(&engine.volumes[config_v].path)?;
        if tree.has_field_index(621, 1) {
            let value_ids: HashSet<_> = tree
                .text_candidates(621, 1, |_| true)?
                .into_iter()
                .collect();
            config.retain(|r| {
                r.area != 621 || r.integer(0x400).is_some_and(|id| value_ids.contains(&id))
            });
        }
        *engine
            .scan_lock
            .get_mut()
            .map_err(|_| "scan lock poisoned")? = config_index.stats.accepted;
        engine.volumes[config_v]
            .index
            .set(Ok(config_index))
            .map_err(|_| "config index already initialized")?;
        let web = web_node(&config)?;
        engine.web_config = registry_config(&config, web);
        engine.registry = registry::Registry::from_records(&config, web)?;
        engine.virtual_dirs = registry_dirs(&config)?;
        let mut declarations = vec![("bootstrap".to_string(), "setspd.dbs".to_string())];
        for r in &config {
            if r.integer(3) == Some(web) && r.string(1).map(decode).as_deref() == Some("AuthScript")
            {
                if let Some(value) = r.string(4) {
                    declarations.push(("Web/AuthScript".into(), decode(value)));
                }
            }
        }
        for d in &engine.virtual_dirs {
            // Type 2 is a redirect; Type 1 is a filesystem directory.
            if d.kind == 3 {
                declarations.push((format!("{}/MappedTo", d.path), d.mapped_to.clone()));
                // Declared dependencies, NOT execution of script-specific BaseN semantics.
                for (n, base) in d.bases.iter().enumerate() {
                    declarations.push((
                        format!("{}/Base dependency #{}", d.path, n + 1),
                        base.clone(),
                    ));
                }
            }
        }
        for (origin, rel) in declarations {
            if rel.is_empty() {
                continue;
            }
            let normalized = rel.replace('\\', "/");
            let path = Path::new(&normalized);
            if path.is_absolute()
                || normalized.contains(':')
                || normalized.split('/').any(|p| p == "..")
            {
                return Err(format!(
                    "{origin}: unsupported path outside data root: {rel}"
                ));
            }
            let path = root.join(path);
            match std::fs::metadata(&path) {
                Ok(meta) if meta.is_file() => {
                    engine.register(&root, &path, &mut seen)?;
                    engine
                        .mount_report
                        .push(format!("available\t{origin}\t{rel}"));
                }
                Ok(_) => engine
                    .mount_report
                    .push(format!("not-file\t{origin}\t{rel}")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => engine
                    .mount_report
                    .push(format!("missing\t{origin}\t{rel}")),
                Err(e) => return Err(format!("{origin}: {rel}: {e}")),
            }
        }
        let missing = engine
            .mount_report
            .iter()
            .filter(|s| !s.starts_with("available\t"))
            .count();
        eprintln!("Registered {} declared volumes; {} missing dependencies. Only config.db4 indexed; KS startup is evaluated on script requests (partial runtime).",engine.volumes.len(),missing);
        Ok(engine)
    }
    fn register(
        &mut self,
        root: &Path,
        path: &Path,
        seen: &mut HashSet<PathBuf>,
    ) -> Result<(), String> {
        let canonical_root = root.canonicalize().map_err(|e| e.to_string())?;
        let canonical = path.canonicalize().map_err(|e| e.to_string())?;
        if !canonical.starts_with(&canonical_root) {
            return Err(format!("volume outside data root: {}", path.display()));
        }
        if self.volumes.iter().any(|v| v.path == canonical) || !seen.insert(canonical.clone()) {
            return Ok(());
        }
        let meta = std::fs::metadata(&canonical).map_err(|e| e.to_string())?;
        if !meta.is_file() {
            return Ok(());
        }
        // Preserve declared aliases when their spelling is relative to root. An
        // absolute Windows script path may omit the root's verbatim prefix;
        // in that case use the already validated canonical path, not the raw one.
        let relative = path
            .strip_prefix(root)
            .or_else(|_| canonical.strip_prefix(&canonical_root))
            .map_err(|e| {
                format!(
                    "volume relative path: resolved={}, root={}: {e}",
                    canonical.display(),
                    root.display()
                )
            })?;
        let name = volume_key(relative.to_str().ok_or("invalid filename")?);
        if self.volumes.iter().any(|v| v.name == name) {
            return Err(format!("duplicate relative volume path {name}"));
        }
        self.volumes.push(Volume {
            path: canonical,
            name,
            size: meta.len(),
            index: OnceLock::new(),
        });
        Ok(())
    }
    fn volume_id(&self, name: &str) -> Result<Option<usize>, String> {
        let key = volume_key(name);
        if let Some(v) = self.volumes.iter().position(|v| v.name == key) {
            return Ok(Some(v));
        }
        Ok(None) // No basename fallback: old/base.db4 is not base.db4.
    }
    fn index(&self, v: usize) -> Result<&VolumeIndex, String> {
        self.volumes[v]
            .index
            .get_or_init(|| {
                // Single scan in flight; cap the total cached index, not just each file.
                let mut total = self.scan_lock.lock().map_err(|_| "scan lock poisoned")?;
                if *total + MAX_INDEX_ENTRIES > MAX_TOTAL_ENTRIES {
                    return Err("total cached index limit reached".into());
                }
                let index = indexed_catalog(&self.volumes[v], v, |_| {})
                    .map_err(|e| format!("{}: {e}", self.volumes[v].name))?;
                *total += index.stats.accepted;
                Ok(index)
            })
            .as_ref()
            .map_err(Clone::clone)
    }
    pub fn resolve_virtual_dir(&self, path: &str) -> Option<&VirtualDir> {
        let request = path_key(path);
        self.virtual_dirs
            .iter()
            .filter(|d| {
                let p = path_key(&d.path);
                request == p || (p != "/" && request.starts_with(&(p + "/")))
            })
            .max_by_key(|d| d.path.len())
    }
    pub fn read_location(&self, loc: &Location) -> Result<Record, String> {
        let volume = self.volumes.get(loc.volume).ok_or("invalid volume index")?;
        let mut tree = mainkey::MainKey::open(&volume.path)?;
        let entry = tree.lookup(loc.oid)?.ok_or("record no longer exists")?;
        tree.record(entry)
    }
    pub fn schema_for_dir(&self, dir: &VirtualDir) -> Result<schema::Schema, String> {
        let v = self
            .volume_id(&dir.mapped_to)?
            .ok_or("mapped volume missing")?;
        self.cached_schema(v).map(|schema| (*schema).clone())
    }
    pub fn execute_attr(&self, dir: &VirtualDir, oid: u32, name: &str) -> Result<u16, String> {
        for volume in std::iter::once(&dir.mapped_to).chain(dir.bases.iter()) {
            let Some(v) = self.volume_id(volume)? else {
                continue;
            };
            if let Some(rec) = self.read_oid(oid, volume)? {
                let schema = self.cached_schema(v)?;
                return schema
                    .areas
                    .get(&rec.area)
                    .and_then(|a| a.fields.values().find(|f| f.name == name))
                    .map(|f| f.id)
                    .ok_or_else(|| format!("Document({oid}): unknown attribute {name}"));
            }
        }
        Err(format!("Document({oid}) not found"))
    }
    pub fn query_scripts(&self, dir: &VirtualDir, sql: &str) -> Result<Vec<u32>, String> {
        let q = crate::ks::host::script_query(sql)?;
        let mut result = Vec::new();
        for volume in std::iter::once(&dir.mapped_to).chain(dir.bases.iter()) {
            let Some(v) = self.volume_id(volume)? else {
                continue;
            };
            let schema = self.cached_schema(v)?;
            let Some(area) = schema.area(&q.area) else {
                continue;
            };
            q.fields(area)?;
            let candidates = self.predicate_candidates(v, area, &q.predicate.bind(area)?)?;
            for oid in candidates {
                if !result.contains(&oid) {
                    result.push(oid);
                }
            }
            if result.len() > 10000 {
                return Err("SQL row limit".into());
            }
        }
        Ok(result)
    }
    pub fn query_indexed(
        &self,
        dir: &VirtualDir,
        area: u16,
        predicate: &crate::ks::sql::BoundPredicate,
    ) -> Result<Vec<u32>, String> {
        self.refresh_writes()?;
        let mut result = Vec::new();
        for volume in std::iter::once(&dir.mapped_to).chain(dir.bases.iter()) {
            let Some(v) = self.volume_id(volume)? else {
                continue;
            };
            let schema = self.cached_schema(v)?;
            let Some(local) = schema.areas.get(&area) else {
                continue;
            };
            for oid in self.predicate_candidates(v, local, predicate)? {
                if !result.contains(&oid) {
                    result.push(oid);
                }
            }
            if result.len() > 10000 {
                return Err("SQL row limit".into());
            }
        }
        Ok(result)
    }
    fn predicate_candidates(
        &self,
        v: usize,
        area: &schema::Area,
        p: &crate::ks::sql::BoundPredicate,
    ) -> Result<Vec<u32>, String> {
        self.predicate_candidates_impl(v, area, p, false)
    }
    fn predicate_candidates_impl(
        &self,
        v: usize,
        area: &schema::Area,
        p: &crate::ks::sql::BoundPredicate,
        raw: bool,
    ) -> Result<Vec<u32>, String> {
        use crate::ks::sql::BoundPredicate as Predicate;
        match p {
            Predicate::None => Ok(Vec::new()),
            Predicate::Set {
                attribute: None,
                ids,
            } => Ok(ids.as_ref().clone()),
            Predicate::Set {
                attribute: Some(attribute),
                ids,
            } => {
                let mut result = std::collections::BTreeSet::new();
                for &oid in ids.iter() {
                    result.extend(self.predicate_candidates_impl(
                        v,
                        area,
                        &Predicate::Link {
                            attribute: *attribute,
                            oid,
                        },
                        raw,
                    )?);
                    if result.len() > crate::ks::sql::MAX_INDEX_CANDIDATES {
                        return Err("SQL set candidate limit".into());
                    }
                }
                Ok(result.into_iter().collect())
            }
            Predicate::Between {
                attribute,
                kind,
                lower,
                upper,
            } => {
                use crate::ks::sql::Comparison::{Ge, Le};
                if *kind == 1 {
                    let bound = |v: &str| {
                        v.replace('-', "")
                            .parse::<u32>()
                            .map_err(|_| "invalid bound date".to_string())
                    };
                    self.with_reader(v, |tree| {
                        tree.date_range_candidates(
                            area.id,
                            *attribute,
                            &[(Ge, bound(lower)?), (Le, bound(upper)?)],
                        )
                    })
                } else if *kind == 12 {
                    self.with_reader(v, |tree| {
                        tree.timestamp_range_candidates(
                            area.id,
                            *attribute,
                            &[
                                (Ge, mainkey::timestamp_index::bound(lower)?),
                                (Le, mainkey::timestamp_index::bound(upper)?),
                            ],
                        )
                    })
                } else {
                    let candidates = self.predicate_candidates_impl(
                        v,
                        area,
                        &Predicate::Compare {
                            attribute: *attribute,
                            kind: *kind,
                            op: Ge,
                            value: lower.clone(),
                        },
                        true,
                    )?;
                    let mut result = Vec::new();
                    for oid in candidates {
                        if let Some(record) = self.read_oid(oid, &self.volumes[v].name)? {
                            if p.matches_record(oid, &record) {
                                result.push(oid);
                            }
                        }
                    }
                    Ok(result)
                }
            }
            Predicate::Compare {
                attribute,
                kind: 12,
                op,
                value,
            } => self.with_reader(v, |tree| {
                tree.timestamp_candidates(
                    area.id,
                    *attribute,
                    &[(*op, mainkey::timestamp_index::bound(value)?)],
                )
            }),
            Predicate::Compare {
                attribute,
                kind: 1,
                op,
                value,
            } => {
                let date = value
                    .replace('-', "")
                    .parse::<u32>()
                    .map_err(|_| "invalid bound date")?;
                self.with_reader(v, |tree| {
                    tree.date_candidates(area.id, *attribute, &[(*op, date)])
                })
            }
            Predicate::Compare {
                attribute,
                kind,
                op,
                value,
            } if matches!(kind, 2 | 3) => self.with_reader(v, |tree| {
                tree.text_candidates(area.id, *attribute, |key| {
                    let text = decode(key);
                    let comparison = if *kind == 3 {
                        crate::ks::sql::numeric_cmp(&text, value)
                    } else {
                        Some(text.to_lowercase().cmp(&value.to_lowercase()))
                    };
                    comparison.is_some_and(|c| op.accepts(c))
                })
            }),
            Predicate::Compare { .. } => {
                let mut out = Vec::new();
                for oid in self.predicate_candidates(v, area, &Predicate::All)? {
                    if let Some(record) = self.read_oid(oid, &self.volumes[v].name)? {
                        if p.matches_record(oid, &record) {
                            out.push(oid);
                        }
                    }
                }
                Ok(out)
            }
            Predicate::Not(_) => {
                let mut out = Vec::new();
                for oid in self.predicate_candidates(v, area, &Predicate::All)? {
                    if let Some(r) = self.read_oid(oid, &self.volumes[v].name)? {
                        if p.matches_record(oid, &r) {
                            out.push(oid);
                        }
                    }
                }
                Ok(out)
            }
            Predicate::Oid(oid) => Ok(
                if self
                    .read_oid(*oid, &self.volumes[v].name)?
                    .is_some_and(|r| r.area == area.id)
                {
                    vec![*oid]
                } else {
                    vec![]
                },
            ),
            Predicate::IsNull {
                attribute,
                kind,
                negated,
            } => {
                let ids = self.predicate_candidates(v, area, &Predicate::All)?;
                let mut result = Vec::new();
                for oid in ids {
                    let r = self
                        .read_oid(oid, &self.volumes[v].name)?
                        .ok_or("area index points to missing document")?;
                    if r.fields
                        .iter()
                        .any(|f| f.id == *attribute && f.wire_type == *kind)
                        == *negated
                    {
                        result.push(oid);
                    }
                }
                Ok(result)
            }
            Predicate::Link {
                attribute,
                oid: target,
            } => {
                let Some(f) = area.fields.get(attribute) else {
                    return Ok(vec![]);
                };
                let volume = &self.volumes[v];
                if f.kind != 5 {
                    return Err(format!(
                        "SQL link field {}/{} has incompatible native type {} in {}",
                        area.id, attribute, f.kind, volume.name
                    ));
                }
                let candidates = self
                    .with_reader(v, |tree| {
                        tree.link_candidates(
                            area.id,
                            *attribute,
                            *target,
                            crate::ks::sql::MAX_INDEX_CANDIDATES,
                        )
                    })
                    .map_err(|e| {
                        format!("{}: link index {}/{}: {e}", volume.name, area.id, attribute)
                    })?;
                if raw {
                    return Ok(candidates);
                }
                for &oid in &candidates {
                    let r = self
                        .read_oid(oid, &volume.name)?
                        .ok_or("link index points to missing document")?;
                    if r.area != area.id {
                        return Err("link index area mismatch".into());
                    }
                    let mut matched = false;
                    for field in r.fields.iter().filter(|f| f.id == *attribute) {
                        match (&field.value, field.wire_type) {
                            (fields::Value::Integer(value), 5) => matched |= value == target,
                            _ => return Err("link index points to a non-link field".into()),
                        }
                    }
                    if !matched {
                        return Err(format!(
                            "link index value mismatch for OID {oid}, field {attribute}"
                        ));
                    }
                }
                Ok(candidates)
            }
            Predicate::All => {
                let volume = &self.volumes[v];
                self.with_reader(v, |tree| {
                    let ids = tree
                        .area_candidates(area.id, 10000)
                        .map_err(|e| format!("{}: {e}", volume.name))?;
                    // Validate membership against MainKey and the C-record prefix.
                    // Do not decompress document bodies just to enumerate OIDs.
                    for &oid in &ids {
                        tree.validate_area_member(oid, area.id)
                            .map_err(|e| format!("{}: {e}", volume.name))?;
                    }
                    Ok(ids)
                })
            }
            Predicate::And(a, b) => {
                // Drive conjunctions from an index, then inspect only that bounded
                // candidate set. IS NULL must not enumerate a multi-million-row area.
                fn conjuncts<'a>(p: &'a Predicate, out: &mut Vec<&'a Predicate>) {
                    if let Predicate::And(a, b) = p {
                        conjuncts(a, out);
                        conjuncts(b, out);
                    } else {
                        out.push(p);
                    }
                }
                let mut drivers = Vec::new();
                conjuncts(a, &mut drivers);
                conjuncts(b, &mut drivers);
                drivers.sort_by_key(|p| p.index_cost());
                let date_filters = drivers
                    .iter()
                    .filter_map(|p| match p {
                        Predicate::Compare {
                            attribute,
                            kind: 1,
                            op,
                            value,
                        } => Some(
                            value
                                .replace('-', "")
                                .parse::<u32>()
                                .map(|date| (*attribute, *op, date))
                                .map_err(|_| "invalid bound date".to_string()),
                        ),
                        _ => None,
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                let mut selected: Option<Vec<u32>> = None;
                let mut limit_error = None;
                for driver in drivers {
                    if selected.is_some() && driver.index_cost() >= 100000 {
                        continue;
                    }
                    let candidates = if let Predicate::Compare {
                        attribute, kind: 1, ..
                    } = driver
                    {
                        let filters = date_filters
                            .iter()
                            .filter(|(a, _, _)| a == attribute)
                            .map(|(_, op, v)| (*op, *v))
                            .collect::<Vec<_>>();
                        self.with_reader(v, |tree| {
                            tree.date_candidates(area.id, *attribute, &filters)
                        })
                    } else {
                        self.predicate_candidates_impl(v, area, driver, true)
                    };
                    match candidates {
                        Ok(ids) => {
                            if selected.as_ref().map_or(true, |old| ids.len() < old.len()) {
                                selected = Some(ids);
                            }
                            if selected.as_ref().is_some_and(|ids| ids.len() <= 64) {
                                break;
                            }
                        }
                        Err(e)
                            if e.contains("match limit")
                                || e.contains("row limit")
                                || e.contains("candidate limit") =>
                        {
                            limit_error = Some(e);
                        }
                        Err(e) => return Err(e),
                    }
                }
                let ids = selected
                    .ok_or_else(|| limit_error.unwrap_or_else(|| "no bounded SQL index".into()))?;
                let mut result = Vec::new();
                for oid in ids {
                    if let Some(r) = self.read_oid(oid, &self.volumes[v].name)? {
                        if p.matches_record(oid, &r) {
                            result.push(oid);
                        }
                    }
                }
                Ok(result)
            }
            Predicate::Or(a, b) => {
                let mut a = self.predicate_candidates_impl(v, area, a, raw)?;
                let b = self.predicate_candidates_impl(v, area, b, raw)?;
                if matches!(p, Predicate::And(_, _)) {
                    a.retain(|id| b.contains(id));
                } else {
                    for id in b {
                        if !a.contains(&id) {
                            a.push(id);
                        }
                    }
                }
                Ok(a)
            }
            Predicate::Text {
                attribute,
                kind,
                pattern,
                folded,
                like,
            } => {
                let Some(f) = area.fields.get(attribute) else {
                    return Ok(Vec::new());
                };
                if f.kind != *kind {
                    return Err(format!(
                        "SQL field {}/{} has incompatible native type {} in {}",
                        area.id, attribute, f.kind, self.volumes[v].name
                    ));
                }
                let prefix = if *like && *kind == 2 {
                    folded.split(['*', '?']).next().unwrap_or("")
                } else {
                    folded.as_str()
                };
                let uppercase = prefix.to_uppercase();
                let (seek, _, bad) = encoding_rs::WINDOWS_1251.encode(&uppercase);
                let seek = seek.into_owned();
                let compare = |key: &[u8]| {
                    if *like && key.starts_with(&seek) {
                        return std::cmp::Ordering::Equal;
                    }
                    if *kind == 3 {
                        if let (Ok(a), Ok(b)) = (decode(key).parse::<f64>(), folded.parse::<f64>())
                        {
                            return a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal);
                        }
                    }
                    key.cmp(&seek)
                };
                // Index strings are folded by the native collation; verify raw values afterwards.
                let candidates = self
                    .with_reader(v, |tree| {
                        tree.text_candidates_seek(
                            area.id,
                            f.id,
                            |key| {
                                if *like {
                                    pattern.is_index_match(&decode(key))
                                } else {
                                    decode(key).to_lowercase() == *folded
                                }
                            },
                            if !bad && (!like || *kind == 2 && !seek.is_empty()) {
                                Some(&compare)
                            } else {
                                None
                            },
                        )
                    })
                    .map_err(|e| {
                        format!(
                            "{}: text index {}/{}: {e}",
                            self.volumes[v].name, area.id, f.id
                        )
                    })?;
                if raw {
                    return Ok(candidates);
                }
                let mut result = Vec::new();
                for oid in candidates {
                    let r = self
                        .read_oid(oid, &self.volumes[v].name)?
                        .ok_or("attribute index points to missing document")?;
                    if r.area != area.id {
                        return Err("attribute index area mismatch".into());
                    }
                    for field in r.fields.iter().filter(|field| field.id == f.id) {
                        let fields::Value::String(bytes) = &field.value else {
                            return Err("string index points to a non-string field".into());
                        };
                        if pattern.is_match(&decode(bytes)) {
                            result.push(oid);
                            break;
                        }
                    }
                }
                Ok(result)
            }
        }
    }
    pub fn named(&self, volume: &str, name: &str) -> Result<Option<Record>, String> {
        let Some(v) = self.volume_id(volume)? else {
            return Ok(None);
        };
        let index = self.index(v)?;
        let name = name.trim_start_matches('/').to_lowercase();
        for candidate in [
            name.clone(),
            format!("{name}.ks"),
            format!("scriptlibrary {name}.ks"),
        ] {
            if let Some(loc) = index.names.get(&candidate) {
                return self
                    .read_location(
                        loc.as_ref()
                            .ok_or("ambiguous named object in recovery scan")?,
                    )
                    .map(Some);
            }
        }
        Ok(None)
    }
    /// A static-resource cache must recover a pending journal before trusting
    /// its generation, even when it does not need to decode the record again.
    pub(crate) fn resource_stamp(
        &self,
        volume: &str,
    ) -> Result<Option<(PathBuf, u64, u64, std::time::SystemTime)>, String> {
        let Some(v) = self.volume_id(volume)? else {
            return Ok(None);
        };
        let path = &self.volumes[v].path;
        writer::ensure_recovered(path)?;
        let _publication = writer::open_guard()?;
        let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
        Ok(Some((
            path.clone(),
            writer::generation(path),
            meta.len(),
            meta.modified().map_err(|e| e.to_string())?,
        )))
    }
    pub fn read_oid(&self, oid: u32, preferred: &str) -> Result<Option<Record>, String> {
        Ok(match self.lookup_record(oid, preferred)? {
            RecordLookup::Present(record) => Some(record),
            RecordLookup::Missing | RecordLookup::Deleted => None,
        })
    }
    pub fn lookup_record(&self, oid: u32, preferred: &str) -> Result<RecordLookup, String> {
        let Some(v) = self.volume_id(preferred)? else {
            return Ok(RecordLookup::Missing);
        };
        self.with_reader(v, |index| {
            let Some(entry) = index.lookup_including_deleted(oid)? else {
                return Ok(RecordLookup::Missing);
            };
            if entry.block == u32::MAX {
                return Ok(RecordLookup::Deleted);
            }
            index.record(entry).map(RecordLookup::Present)
        })
    }
    fn visit_attributes(
        &self, preferred: &str, ids: &[u32],
        visit: &mut impl FnMut(u32, Option<fields::ScalarRecord>) -> Result<(), String>,
    ) -> Result<Option<()>, String> {
        let Some(v) = self.volume_id(preferred)? else { return Ok(None); };
        self.with_reader(v, |tree| {
            for entry in tree.lookup_many(ids)? {
                let record = if entry.block == u32::MAX { None }
                    else { Some(tree.record_attributes(entry)?) };
                visit(entry.oid, record)?;
            }
            Ok(Some(()))
        })
    }
    fn lookup_attributes(&self, oid: u32, preferred: &str) -> Result<RecordLookup<fields::ScalarRecord>, String> {
        let Some(v) = self.volume_id(preferred)? else { return Ok(RecordLookup::Missing) };
        self.with_reader(v, |index| {
            let Some(entry) = index.lookup_including_deleted(oid)? else { return Ok(RecordLookup::Missing) };
            if entry.block == u32::MAX { return Ok(RecordLookup::Deleted); }
            index.record_attributes(entry).map(RecordLookup::Present)
        })
    }
    pub fn lookup_metadata(
        &self,
        oid: u32,
        preferred: &str,
    ) -> Result<RecordLookup<fields::RecordMetadata>, String> {
        let Some(v) = self.volume_id(preferred)? else {
            return Ok(RecordLookup::Missing);
        };
        self.with_reader(v, |index| {
            let Some(entry) = index.lookup_including_deleted(oid)? else {
                return Ok(RecordLookup::Missing);
            };
            if entry.block == u32::MAX {
                return Ok(RecordLookup::Deleted);
            }
            index.record_metadata(entry).map(RecordLookup::Present)
        })
    }
    // Index probes need the same immutable-volume handles as record reads.
    // Annual reports perform tens of thousands of short queries; reopening
    // every mounted file at every predicate dominates those queries.
    // Keep the lock inside one non-recursive operation, never across a query
    // callback that may read records or probe another index.
    fn with_reader<T>(
        &self,
        v: usize,
        read: impl FnOnce(&mut mainkey::MainKey) -> Result<T, String>,
    ) -> Result<T, String> {
        crate::execution::check()?;
        self.refresh_writes()?;
        let mut readers = self
            .readers
            .lock()
            .map_err(|_| "volume reader cache poisoned")?;
        if !readers.contains_key(&v) {
            if readers.len() >= 64 {
                readers.clear();
            }
            let mut reader = mainkey::MainKey::open(&self.volumes[v].path)?;
            reader.cache_nodes();
            reader.cache_records(self.decoded.clone());
            let size = reader.snapshot_size();
            readers.insert(v, (size, reader));
        }
        let (size, index) = readers.get_mut(&v).unwrap();
        index.validate_size(*size)?;
        read(index)
    }
    fn refresh_writes(&self) -> Result<(), String> {
        use std::sync::atomic::Ordering;
        let current = writer::epoch();
        if self.write_epoch.load(Ordering::Acquire) != current {
            self.readers
                .lock()
                .map_err(|_| "reader cache poisoned")?
                .clear();
            self.write_epoch.store(current, Ordering::Release);
        }
        Ok(())
    }
    fn lookup_entries(
        &self,
        name: &str,
        ids: &[u32],
    ) -> Result<Option<Vec<mainkey::Entry>>, String> {
        let Some(v) = self.volume_id(name)? else {
            return Ok(None);
        };
        self.with_reader(v, |index| index.lookup_many(ids))
            .map(Some)
    }
    pub fn record_revision(&self, oid: u32, preferred: &str) -> Result<Option<u16>, String> {
        let Some(v) = self.volume_id(preferred)? else {
            return Ok(None);
        };
        let mut index = mainkey::MainKey::open(&self.volumes[v].path)?;
        Ok(index
            .lookup_including_deleted(oid)?
            .filter(|e| e.block != u32::MAX)
            .map(|e| e.checksum))
    }
    pub fn oid_for_dir(&self, dir: &VirtualDir, oid: u32) -> Result<Option<Record>, String> {
        for volume in std::iter::once(&dir.mapped_to).chain(dir.bases.iter()) {
            if let Some(record) = self.read_oid(oid, volume)? {
                return Ok(Some(record));
            }
        }
        Ok(None)
    }
    pub fn object_for_dir(&self, dir: &VirtualDir, name: &str) -> Result<Option<Record>, String> {
        for volume in std::iter::once(&dir.mapped_to).chain(dir.bases.iter()) {
            if let Some(r) = self.named(volume, name)? {
                return Ok(Some(r));
            }
        }
        Ok(None)
    }
}
// Resolve exact registry ancestry, not similarly named values elsewhere.
fn web_node(records: &[Record]) -> Result<u32, String> {
    let mut parents = HashSet::from([None]);
    for name in ["MAINKEY", "SOFTWARE", "Kodeks", "Web"] {
        parents = records
            .iter()
            .filter(|r| {
                r.string(4).is_none()
                    && parents.contains(&r.integer(3))
                    && r.string(1).map(decode).as_deref() == Some(name)
            })
            .filter_map(|r| r.integer(0x400).map(Some))
            .collect();
    }
    // Recovery scan can see stale empty branches. Only a populated Web branch
    // can supply routes. This is NOT proof of liveness; an index reader is needed.
    let populated: Vec<_> = parents
        .into_iter()
        .flatten()
        .filter(|id| {
            records
                .iter()
                .any(|r| r.integer(3) == Some(*id) && is_path_node(r))
        })
        .collect();
    match populated.as_slice(){[id]=>Ok(*id),_=>Err(format!("expected one populated MAINKEY/SOFTWARE/Kodeks/Web branch, found {}; indexed registry reader required",populated.len()))}
}
// InitObj loader FUN_006a5130 enumerates immediate Web values, REG_SZ/REG_DWORD only.
fn registry_config(records: &[Record], parent: u32) -> BTreeMap<String, ConfigValue> {
    let mut out = BTreeMap::new();
    for r in records.iter().filter(|r| r.integer(3) == Some(parent)) {
        let (Some(name), Some(value), Some(kind)) =
            (r.string(1), registry::string_value(r), r.string(5))
        else {
            continue;
        };
        let name = decode(name);
        let value = match decode(kind).as_str() {
            "REG_SZ" => ConfigValue::Text(value),
            "REG_DWORD" => match value.parse::<u32>() {
                Ok(n) => ConfigValue::Integer(n as i32),
                Err(_) => ConfigValue::Invalid(format!("invalid REG_DWORD for {name}")),
            },
            _ => continue,
        };
        match out.get(&name) {
            Some(old) if old != &value => {
                out.insert(
                    name.clone(),
                    ConfigValue::Invalid(format!("conflicting indexed registry values for {name}")),
                );
            }
            Some(_) => {}
            None => {
                out.insert(name, value);
            }
        }
    }
    out
}
fn is_path_node(r: &Record) -> bool {
    r.string(4).is_none()
        && r.string(1)
            .map(decode)
            .and_then(|s| s.strip_prefix("Path").map(str::to_owned))
            .map(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .unwrap_or(false)
}
fn registry_dirs(records: &[Record]) -> Result<Vec<VirtualDir>, String> {
    let web = web_node(records)?;
    let parents: HashSet<u32> = records
        .iter()
        .filter(|r| r.integer(3) == Some(web) && is_path_node(r))
        .filter_map(|r| r.integer(0x400))
        .collect();
    let mut groups: BTreeMap<u32, BTreeMap<String, String>> = BTreeMap::new();
    for r in records {
        let (Some(parent), Some(k), Some(v)) = (r.integer(3), r.string(1), r.string(4)) else {
            continue;
        };
        if !parents.contains(&parent) {
            continue;
        }
        let key = decode(k);
        let val = decode(v);
        if !matches!(
            key.as_str(),
            "Path" | "MappedTo" | "Type" | "Comment" | "Access"
        ) && !key
            .strip_prefix("Base")
            .map(|s| s.parse::<usize>().is_ok())
            .unwrap_or(false)
        {
            continue;
        }
        let entries = groups.entry(parent).or_default();
        if let Some(old) = entries.insert(key.clone(), val.clone()) {
            if old != val {
                return Err(format!("conflicting registry value {parent:#x}/{key}"));
            }
        }
    }
    let mut out = Vec::new();
    for (parent, entries) in groups {
        let (Some(path), Some(mapped)) = (entries.get("Path"), entries.get("MappedTo")) else {
            continue;
        };
        let kind = entries
            .get("Type")
            .and_then(|s| s.parse::<u32>().ok())
            .ok_or_else(|| format!("invalid Type for {path}"))?;
        let mut bases: Vec<_> = entries
            .iter()
            .filter_map(|(k, v)| {
                k.strip_prefix("Base")
                    .and_then(|s| s.parse::<usize>().ok())
                    .map(|n| (n, v.clone()))
            })
            .collect();
        bases.sort_by_key(|x| x.0);
        out.push(VirtualDir {
            path: path.clone(),
            title: entries.get("Comment").cloned().unwrap_or_default(),
            kind,
            mapped_to: mapped.clone(),
            bases: bases.into_iter().map(|x| x.1).collect(),
            properties: registry_config(records, parent),
            access: entries.get("Access").cloned(),
            auth: records
                .iter()
                .find(|r| {
                    r.integer(3) == Some(parent)
                        && r.string(1).map(decode).as_deref() == Some("AuthParam")
                        && r.string(4).is_none()
                })
                .and_then(|r| r.integer(0x400))
                .map(|id| registry_config(records, id))
                .unwrap_or_default(),
        });
    }
    Ok(out)
}

pub(crate) mod writer;
