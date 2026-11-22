//! DB40 MainKey: native K root directory and L B-tree, recovered from
//! FUN_005f32a0, FUN_005f3c40, FUN_005cb020 and FUN_005c6e50.
#[path = "area_index.rs"]
mod area_index;
#[path = "date_index.rs"]
mod date_index;
#[path = "timestamp_index.rs"]
pub(crate) mod timestamp_index;
#[path = "word_index.rs"]
pub(super) mod word_index;
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};
#[derive(Debug, Clone, Copy)]
pub struct Entry {
    pub oid: u32,
    pub block: u32,
    pub checksum: u16,
}
pub(super) struct WriterTree {
    pub blocks: Vec<u32>,
    pub items: Vec<Vec<u8>>,
}
#[derive(Clone)]
pub(super) struct WriterNode {
    pub keys: Vec<Vec<u8>>,
    pub children: Vec<u32>,
}
#[derive(Debug)]
pub struct MainKey {
    file: Option<File>,
    snapshot: std::sync::Arc<super::writer::snapshot::Snapshot>,
    size: u64,
    unit: u64,
    map_units: u64,
    root: u32,
    // Immutable snapshot metadata: every index probe used to reread these.
    // The directory is bounded to 1 MiB by open(), independently of node caches.
    header: [u8; 289],
    directory: std::sync::Arc<[u8]>,
    pub reads: u64,
    pub bytes: u64,
    node_cache: Option<(HashMap<u32, Node>, usize)>,
    block_cache: Option<BlockCache>,
    // Only scalar scans use read-ahead. Eight 64 KiB pages bound memory while
    // adjacent records share disk reads; the reader is discarded after writes.
    scalar_pages: std::collections::VecDeque<(u64, Vec<u8>)>,
}
type BlockKey = (u32, u8, usize);
#[derive(Debug, Default)]
struct BlockCache {
    items: HashMap<BlockKey, (Vec<u8>, u64)>,
    age: std::collections::BTreeSet<(u64, BlockKey)>,
    clock: u64,
    bytes: usize,
}
impl BlockCache {
    fn get(&mut self, key: BlockKey) -> Option<Vec<u8>> {
        let (data, stamp) = self.items.get_mut(&key)?;
        self.age.remove(&(*stamp, key));
        self.clock += 1;
        *stamp = self.clock;
        self.age.insert((*stamp, key));
        Some(data.clone())
    }
    fn insert(&mut self, key: BlockKey, data: &[u8]) {
        if data.len() > 1024 * 1024 {
            return;
        }
        while self.bytes + data.len() > 4 * 1024 * 1024 || self.items.len() >= 4096 {
            let (_, old) = self.age.pop_first().unwrap();
            self.bytes -= self.items.remove(&old).unwrap().0.len();
        }
        self.clock += 1;
        self.bytes += data.len();
        self.items.insert(key, (data.to_vec(), self.clock));
        self.age.insert((self.clock, key));
    }
}
#[derive(Clone, Debug)]
struct Node {
    branch: bool,
    entries: Vec<Entry>,
    children: Vec<u32>,
}
fn u16at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes(b[i..i + 2].try_into().unwrap())
}
fn u32at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
}
impl MainKey {
    pub(super) fn has_field_index(&self, area: u16, attr: u16) -> bool {
        self.directory
            .chunks_exact(10)
            .any(|entry| u16at(entry, 6) == area && u16at(entry, 8) == attr)
    }

    pub(super) fn snapshot_size(&self) -> u64 {
        self.size
    }
    pub(super) fn writer_parts(&self) -> ([u8; 289], Vec<u8>) {
        (self.header, self.directory.to_vec())
    }
    pub(super) fn writer_presence(&mut self) -> Result<Option<Vec<u8>>, String> {
        let root = u32at(&self.header, 43);
        if root == 0 {
            Ok(None)
        } else {
            self.block(root, b'D', 4 * 1024 * 1024).map(Some)
        }
    }
    pub(super) fn writer_node(
        &mut self,
        id: u32,
        area: u16,
        attr: u16,
        size: Option<usize>,
    ) -> Result<WriterNode, String> {
        let b = self.block(id, b'L', 1024 * 1024)?;
        if b.len() < 7 || u16at(&b, 0) != attr || u16at(&b, 2) != area || b[6] > 1 {
            return Err("DB40 index node identity mismatch".into());
        }
        let count = u16at(&b, 4) as usize;
        let branch = b[6] == 1;
        let base = if size.is_some() { 7 } else { 7 + 2 * count };
        if base > b.len() {
            return Err("truncated DB40 index offsets".into());
        }
        let mut keys = Vec::new();
        let mut children = Vec::new();
        let mut previous = 0;
        for i in 0..count {
            let end = if let Some(size) = size {
                (i + 1) * (size + if branch { 4 } else { 0 })
            } else {
                u16at(&b, 7 + i * 2) as usize
            };
            let start = previous + if branch { 4 } else { 0 };
            if end <= start || base + end > b.len() {
                return Err("invalid DB40 index entry".into());
            }
            if branch {
                children.push(u32at(&b, base + previous));
            }
            keys.push(b[base + start..base + end].to_vec());
            previous = end;
        }
        if branch {
            if base + previous + 4 > b.len() {
                return Err("truncated DB40 right child".into());
            }
            children.push(u32at(&b, base + previous));
            if children.contains(&0) {
                return Err("null DB40 child".into());
            }
        }
        Ok(WriterNode { keys, children })
    }
    pub(crate) fn max_oid(&mut self) -> Result<Option<u32>, String> {
        if self.root == 0 {
            return Ok(None);
        }
        let mut id = self.root;
        let mut seen = HashSet::new();
        let mut maximum = None;
        loop {
            if seen.len() >= 32 || !seen.insert(id) {
                return Err("MainKey maximum cycle/depth".into());
            }
            let n = self.node(id)?;
            if let Some(last) = n.entries.last() {
                maximum = Some(maximum.map_or(last.oid, |old: u32| old.max(last.oid)));
            }
            if !n.branch {
                return Ok(maximum);
            }
            id = *n.children.last().ok_or("MainKey right child absent")?;
        }
    }
    /// Read a native index in order, retaining the exact frames replaced by a
    /// write. This does not decode document bodies or unrelated field indexes.
    pub(super) fn writer_tree(
        &mut self,
        area: u16,
        attr: u16,
        key_size: Option<usize>,
    ) -> Result<WriterTree, String> {
        let roots = self
            .directory
            .chunks_exact(10)
            .filter(|d| u16at(d, 6) == area && u16at(d, 8) == attr)
            .map(|d| u32at(d, 2))
            .collect::<Vec<_>>();
        if roots.len() > 1 {
            return Err("duplicate index during DB40 write".into());
        }
        let mut out = WriterTree {
            blocks: Vec::new(),
            items: Vec::new(),
        };
        fn visit(
            tree: &mut MainKey,
            id: u32,
            depth: usize,
            area: u16,
            attr: u16,
            size: Option<usize>,
            seen: &mut HashSet<u32>,
            out: &mut WriterTree,
        ) -> Result<(), String> {
            if id == 0 || depth >= 32 || seen.len() >= 100000 || !seen.insert(id) {
                return Err("DB40 replacement index child/cycle/budget".into());
            }
            let b = tree.block(id, b'L', 1024 * 1024)?;
            if b.len() < 7 || u16at(&b, 0) != attr || u16at(&b, 2) != area || b[6] > 1 {
                return Err("DB40 replacement index identity mismatch".into());
            }
            out.blocks.push(id);
            let count = u16at(&b, 4) as usize;
            let child_bytes = if b[6] == 1 { 4 } else { 0 };
            let base = if size.is_some() { 7 } else { 7 + count * 2 };
            if base > b.len() {
                return Err("truncated replacement index offsets".into());
            }
            let mut previous = 0;
            for n in 0..count {
                let end = if let Some(size) = size {
                    (n + 1) * (size + child_bytes)
                } else {
                    u16at(&b, 7 + n * 2) as usize
                };
                if end <= previous + child_bytes || base + end > b.len() {
                    return Err("invalid replacement index entry bounds".into());
                }
                if child_bytes != 0 {
                    visit(
                        tree,
                        u32at(&b, base + previous),
                        depth + 1,
                        area,
                        attr,
                        size,
                        seen,
                        out,
                    )?;
                }
                if out.items.len() >= 1_000_000 {
                    return Err("DB40 replacement index item budget".into());
                }
                out.items
                    .push(b[base + previous + child_bytes..base + end].to_vec());
                previous = end;
            }
            if child_bytes != 0 {
                if base + previous + 4 > b.len() {
                    return Err("truncated replacement index child".into());
                }
                visit(
                    tree,
                    u32at(&b, base + previous),
                    depth + 1,
                    area,
                    attr,
                    size,
                    seen,
                    out,
                )?;
            }
            Ok(())
        }
        if let Some(root) = roots.first().copied().filter(|b| *b != 0) {
            visit(
                self,
                root,
                0,
                area,
                attr,
                key_size,
                &mut HashSet::new(),
                &mut out,
            )?;
        }
        Ok(out)
    }

    pub fn open(path: &Path) -> Result<Self, String> {
        crate::execution::check()?;
        super::writer::ensure_recovered(path)?;
        let publication = super::writer::open_guard()?;
        let file = File::open(path).map_err(|e| e.to_string())?;
        let size = file.metadata().map_err(|e| e.to_string())?.len();
        let snapshot = super::writer::snapshot::register(&file)?;
        drop(publication);
        Self::initialize(Some(file), size, snapshot)
    }
    pub(super) fn from_image(bytes: std::sync::Arc<Vec<u8>>) -> Result<Self, String> {
        let snapshot = std::sync::Arc::new(super::writer::snapshot::Snapshot::default());
        let size = bytes.len() as u64;
        snapshot.bytes.set(bytes).unwrap();
        Self::initialize(None, size, snapshot)
    }
    fn initialize(
        file: Option<File>,
        size: u64,
        snapshot: std::sync::Arc<super::writer::snapshot::Snapshot>,
    ) -> Result<Self, String> {
        let mut r = Self {
            file,
            snapshot,
            size,
            unit: 0,
            map_units: 0,
            root: 0,
            header: [0; 289],
            directory: Vec::new().into(),
            reads: 0,
            bytes: 0,
            node_cache: None,
            block_cache: None,
            scalar_pages: std::collections::VecDeque::new(),
        };
        let h = r.read(0, 289)?;
        if &h[..4] != b"DB40" {
            return Err("not a DB40 volume".into());
        }
        let version = u16at(&h, 5);
        if !matches!(version, 0x611 | 0x612 | 0x614) {
            return Err(format!(
                "indexed reader: unsupported DB40 version {version:#x}"
            ));
        }
        r.unit = u16at(&h, 19) as u64;
        r.map_units = u16at(&h, 21) as u64;
        if r.unit == 0 || r.map_units == 0 {
            return Err("zero allocation geometry".into());
        }
        let roots = r.block(u32at(&h, 15), b'K', 1024 * 1024)?;
        if roots.len() % 10 != 0 {
            return Err("invalid index directory size".into());
        }
        let mut found = false;
        for chunk in roots.chunks_exact(10) {
            if u16at(chunk, 6) == 0xffff && u16at(chunk, 8) == 0 {
                if found {
                    return Err("duplicate MainKey root".into());
                }
                found = true;
                r.root = u32at(chunk, 2);
            }
        }
        if !found {
            return Err("MainKey root descriptor missing".into());
        }
        r.header = h.try_into().unwrap();
        r.directory = roots.into();
        Ok(r)
    }
    /// A bounded cache for a reader held by the immutable mounted snapshot.
    pub(super) fn cache_nodes(&mut self) {
        self.node_cache = Some((HashMap::new(), 0));
        self.block_cache = Some(BlockCache::default());
    }
    pub(super) fn validate_size(&self, expected: u64) -> Result<(), String> {
        if self.size == expected && self.snapshot.bytes.get().is_some() {
            return Ok(());
        }
        let _publication = super::writer::open_guard()?;
        let current = if self.snapshot.bytes.get().is_some() {
            self.size
        } else {
            self.file
                .as_ref()
                .ok_or("DB40 snapshot file missing")?
                .metadata()
                .map_err(|e| e.to_string())?
                .len()
        };
        if self.size != expected || current != expected {
            return Err("volume size changed; restart using a stable snapshot".into());
        }
        Ok(())
    }
    pub(super) fn record(&mut self, entry: Entry) -> Result<super::fields::Record, String> {
        let offset = self.record_offset(entry)?;
        let header = self.header;
        let payload =
            super::payload_from_reader(&header, self.size, offset, |p, n| self.read(p, n))?;
        let record = super::fields::parse_record(&payload)
            .map_err(|e| format!("OID {} block {}: {e}", entry.oid, entry.block))?;
        if record.integer(0x400) != Some(entry.oid) {
            return Err("record OID changed".into());
        }
        Ok(record)
    }
    /// Read scalar links without inflating the trailing CTF/RTF streams.
    pub(super) fn record_links(&mut self, entry: Entry) -> Result<(u16, Vec<(u16, u32)>), String> {
        self.record_scalars(entry)
            .map(|(metadata, links)| (metadata.area, links))
    }
    pub(super) fn record_metadata(
        &mut self,
        entry: Entry,
    ) -> Result<super::fields::RecordMetadata, String> {
        self.record_scalars(entry).map(|(metadata, _)| metadata)
    }
    fn record_scalars(
        &mut self,
        entry: Entry,
    ) -> Result<(super::fields::RecordMetadata, Vec<(u16, u32)>), String> {
        crate::execution::check()?;
        if entry.block == u32::MAX {
            return Err("deleted MainKey entry".into());
        }
        let offset = self.offset(entry.block);
        let header = self.header;
        let span = (self.unit * 8 + 1) * self.unit * self.map_units;
        let until_map = span - (offset - 289) % span;
        let available = self
            .size
            .checked_sub(offset)
            .ok_or("record outside volume")?;
        let first = self.read_scalar(offset, available.min(until_map).min(2055) as usize)?;
        if first.len() < 13 || first[4] != b'C' || u16at(&first, 5) != entry.checksum {
            return Err(format!(
                "MainKey/record header mismatch for OID {}",
                entry.oid
            ));
        }
        let length = u32at(&first, 0) as usize;
        if !(6..=super::fields::MAX_FIELD).contains(&length) {
            return Err("record length out of range".into());
        }
        if let Ok(Some((metadata, links))) =
            super::fields::scalar_prefix(&first[7..(7 + length).min(first.len())])
        {
            if links
                .iter()
                .find(|&&(id, _)| id == 1024)
                .is_some_and(|&(_, oid)| oid == entry.oid)
            {
                return Ok((metadata, links));
            }
        }
        let mut maximum = 2048;
        loop {
            let payload =
                super::payload_prefix_from_reader(&header, self.size, offset, maximum, |p, n| {
                    self.read(p, n)
                })?;
            match super::fields::scalar_prefix(&payload) {
                Ok(Some((metadata, links)))
                    if links
                        .iter()
                        .find(|&&(id, _)| id == 1024)
                        .is_some_and(|&(_, oid)| oid == entry.oid) =>
                {
                    return Ok((metadata, links))
                }
                Ok(None) if payload.len() == maximum && maximum < super::fields::MAX_FIELD => {
                    maximum = (maximum * 2).min(super::fields::MAX_FIELD);
                }
                _ => {
                    let r = self.record(entry)?;
                    return Ok((
                        super::fields::RecordMetadata::from(&r),
                        r.fields
                            .iter()
                            .filter_map(|f| match f.value {
                                super::fields::Value::Integer(n) if f.wire_type == 5 => {
                                    Some((f.id, n))
                                }
                                _ => None,
                            })
                            .collect(),
                    ));
                }
            }
        }
    }
    fn read_scalar(&mut self, at: u64, len: usize) -> Result<Vec<u8>, String> {
        const PAGE: u64 = 64 * 1024;
        if at
            .checked_add(len as u64)
            .filter(|&end| end <= self.size)
            .is_none()
        {
            return Err("scalar read outside volume".into());
        }
        let mut result = Vec::with_capacity(len);
        while result.len() < len {
            let position = at + result.len() as u64;
            let base = position / PAGE * PAGE;
            let entry = if let Some(i) = self.scalar_pages.iter().position(|(p, _)| *p == base) {
                self.scalar_pages.remove(i).unwrap()
            } else {
                (
                    base,
                    self.read(base, (self.size - base).min(PAGE) as usize)?,
                )
            };
            let offset = (position - base) as usize;
            let take = (len - result.len()).min(entry.1.len() - offset);
            result.extend_from_slice(&entry.1[offset..offset + take]);
            self.scalar_pages.push_back(entry);
            if self.scalar_pages.len() > 8 {
                self.scalar_pages.pop_front();
            }
        }
        Ok(result)
    }

    pub fn text_candidates(
        &mut self,
        area: u16,
        attribute: u16,
        matches: impl Fn(&[u8]) -> bool,
    ) -> Result<Vec<u32>, String> {
        self.text_candidates_seek(area, attribute, matches, None)
    }
    pub fn text_candidates_seek(
        &mut self,
        area: u16,
        attribute: u16,
        matches: impl Fn(&[u8]) -> bool,
        compare: Option<&dyn Fn(&[u8]) -> std::cmp::Ordering>,
    ) -> Result<Vec<u32>, String> {
        let roots = self.directory.clone();
        let mut root = None;
        for c in roots.chunks_exact(10) {
            if u16at(c, 6) == area && u16at(c, 8) == attribute {
                if root.is_some() {
                    return Err("duplicate area/name index".into());
                }
                root = Some(u32at(c, 2));
            }
        }
        let root = root.ok_or_else(|| format!("area {area}: native name index missing"))?;
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let initial = self.bytes;
        if root != 0 {
            self.walk_names(
                root,
                area,
                attribute,
                0,
                initial,
                &|_, key| matches(key),
                compare,
                &mut out,
                &mut seen,
            )?;
        }
        out.sort_unstable();
        out.dedup();
        Ok(out)
    }
    fn walk_names(
        &mut self,
        id: u32,
        area: u16,
        attribute: u16,
        depth: usize,
        initial: u64,
        matches: &impl Fn(u32, &[u8]) -> bool,
        compare: Option<&dyn Fn(&[u8]) -> std::cmp::Ordering>,
        out: &mut Vec<u32>,
        seen: &mut HashSet<u32>,
    ) -> Result<(), String> {
        // The full participant name index exceeds 64 MiB (the data volume is
        // 931 MiB). Stream its bounded tree rather than scanning C records.
        if depth >= 32 || seen.len() >= 32768 || !seen.insert(id) {
            return Err(format!(
                "name index depth/node budget/cycle: {} nodes, {} bytes",
                seen.len(),
                self.bytes - initial
            ));
        }
        let b = self.block(id, b'L', 1024 * 1024)?;
        if self.bytes - initial > 128 * 1024 * 1024 {
            return Err("name index byte budget exceeded".into());
        }
        if b.len() < 7 || u16at(&b, 0) != attribute || u16at(&b, 2) != area {
            return Err(format!("block {id}: not area {area} name index"));
        }
        let count = u16at(&b, 4) as usize;
        let branch = b[6] != 0;
        let base = 7 + count * 2;
        if base > b.len() {
            return Err("truncated name index offset table".into());
        }
        let mut previous = 0;
        let mut lower: Option<&[u8]> = None;
        for i in 0..count {
            let end = u16at(&b, 7 + i * 2) as usize;
            let start = previous + if branch { 4 } else { 0 };
            if end < start + 4 || base + end > b.len() {
                return Err("invalid name index entry bounds".into());
            }
            let key = &b[base + start + 4..base + end];
            let may_descend = compare.map_or(true, |cmp| {
                !cmp(key).is_lt() && lower.map_or(true, |lo| !cmp(lo).is_gt())
            });
            if branch && may_descend {
                let child = u32at(&b, base + previous);
                if child == 0 {
                    return Err("null name index child".into());
                }
                self.walk_names(
                    child,
                    area,
                    attribute,
                    depth + 1,
                    initial,
                    matches,
                    compare,
                    out,
                    seen,
                )?;
            }
            if matches(u32at(&b, base + start), key) {
                if out.len() >= crate::ks::sql::MAX_INDEX_CANDIDATES {
                    return Err(format!("name index match limit (8000000): area {area}, attribute {attribute}, bytes {}, nodes {}", self.bytes - initial, seen.len()));
                }
                out.push(u32at(&b, base + start));
            }
            previous = end;
            lower = Some(key);
        }
        if branch && compare.map_or(true, |cmp| lower.map_or(true, |lo| !cmp(lo).is_gt())) {
            if base + previous + 4 > b.len() {
                return Err("truncated final name index child".into());
            }
            let child = u32at(&b, base + previous);
            if child == 0 {
                return Err("null name index child".into());
            }
            self.walk_names(
                child,
                area,
                attribute,
                depth + 1,
                initial,
                matches,
                compare,
                out,
                seen,
            )?;
        }
        Ok(())
    }
    pub(super) fn indexed_strings(
        &mut self,
        area: u16,
        attribute: u16,
        ids: &std::collections::BTreeSet<u32>,
    ) -> Result<std::collections::BTreeMap<u32, Vec<Vec<u8>>>, String> {
        let root = self
            .directory
            .chunks_exact(10)
            .find(|c| u16at(c, 6) == area && u16at(c, 8) == attribute)
            .map(|c| u32at(c, 2))
            .ok_or("numeric projection index missing")?;
        let values =
            std::cell::RefCell::new(std::collections::BTreeMap::<u32, Vec<Vec<u8>>>::new());
        if root != 0 {
            let bytes = std::cell::Cell::new(0usize);
            self.walk_names(
                root,
                area,
                attribute,
                0,
                self.bytes,
                &|oid, key| {
                    if ids.contains(&oid) {
                        bytes.set(bytes.get().saturating_add(key.len()));
                        if bytes.get() <= 4 * 1024 * 1024 {
                            values
                                .borrow_mut()
                                .entry(oid)
                                .or_default()
                                .push(key.to_vec());
                        }
                    }
                    false
                },
                None,
                &mut Vec::new(),
                &mut HashSet::new(),
            )?;
            if bytes.get() > 4 * 1024 * 1024 {
                return Err("SQL string result byte limit".into());
            }
        }
        Ok(values.into_inner())
    }
    pub fn schema_payload(&mut self) -> Result<(u16, Vec<u8>), String> {
        let h = self.header;
        let version = u16at(&h, 5);
        let id = u32at(&h, 11);
        Ok((version, self.block(id, b'A', 4 * 1024 * 1024)?))
    }
    pub fn offset(&self, block: u32) -> u64 {
        let capacity = self.unit * self.map_units * 8;
        let span = (self.unit * 8 + 1) * self.unit * self.map_units;
        289 + (block as u64 / capacity) * span
            + (self.map_units + block as u64 % capacity) * self.unit
    }
    pub(super) fn read(&mut self, at: u64, len: usize) -> Result<Vec<u8>, String> {
        crate::execution::check()?;
        if at
            .checked_add(len as u64)
            .filter(|&end| end <= self.size)
            .is_none()
        {
            return Err(format!(
                "indexed read outside volume at {at:#x}, length {len}"
            ));
        }
        let b = if let Some(bytes) = self.snapshot.bytes.get() {
            bytes[at as usize..at as usize + len].to_vec()
        } else {
            let _publication = super::writer::open_guard()?;
            if let Some(bytes) = self.snapshot.bytes.get() {
                bytes[at as usize..at as usize + len].to_vec()
            } else {
                let file = self.file.as_mut().ok_or("DB40 snapshot file missing")?;
                file.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
                let mut bytes = vec![0; len];
                file.read_exact(&mut bytes).map_err(|e| e.to_string())?;
                bytes
            }
        };
        self.reads += 1;
        self.bytes += len as u64;
        Ok(b)
    }
    fn block(&mut self, id: u32, tag: u8, max: usize) -> Result<Vec<u8>, String> {
        // Only enabled on immutable mounted readers. A write drops the reader
        // together with every index cache. Keep tag/limit in the cache key so
        // a previous permissive read cannot bypass a stricter validation.
        let key = (id, tag, max);
        if let Some(data) = self.block_cache.as_mut().and_then(|cache| cache.get(key)) {
            return Ok(data);
        }
        let data = self.read_block(id, tag, max)?;
        if let Some(cache) = &mut self.block_cache {
            cache.insert(key, &data);
        }
        Ok(data)
    }
    fn read_block(&mut self, id: u32, tag: u8, max: usize) -> Result<Vec<u8>, String> {
        let at = self.offset(id);
        let h = self.read(at, 7)?;
        let compressed = tag == b'L' && h[4] == b'S';
        if h[4] != tag && !compressed {
            return Err(format!(
                "block {id} at {at:#x}: expected {}, got {}",
                tag as char, h[4] as char
            ));
        }
        let len = u32at(&h, 0) as usize;
        if len > max {
            return Err(format!("index block too large: {len}"));
        }
        // Index blocks in this format fit a single allocation-map interval.
        let span = (self.unit * 8 + 1) * self.unit * self.map_units;
        let remaining = span - (at - 289) % span;
        if len as u64 + 7 > remaining {
            return Err("index block crosses allocation map; unsupported layout".into());
        }
        let data = self.read(at + 7, len)?;
        if !compressed {
            return Ok(data);
        }
        // Native 005c80c0: S nodes store plain/packed u16 lengths followed
        // immediately by the compressed payload. The outer frame CRC covers
        // those four bytes plus packed data, excluding allocation padding.
        if data.len() < 4 {
            return Err("truncated compressed index header".into());
        }
        let plain = u16at(&data, 0) as usize;
        let packed = u16at(&data, 2) as usize;
        if plain == 0
            || plain > 65000
            || plain > max
            || packed == 0
            || packed > plain
            || packed + 4 > data.len()
        {
            return Err("invalid compressed index sizes".into());
        }
        let mut crc = 0u16;
        for &byte in &data[..packed + 4] {
            let mut high = crc & 0xff00;
            for _ in 0..8 {
                high = if high & 0x8000 != 0 {
                    high.wrapping_shl(1) ^ 0x1021
                } else {
                    high.wrapping_shl(1)
                };
            }
            crc = crc.wrapping_shl(8) ^ high ^ byte as u16;
        }
        if crc != u16at(&h, 5) {
            return Err("compressed index checksum mismatch".into());
        }
        if plain == packed {
            Ok(data[4..packed + 4].to_vec())
        } else {
            super::fields::decompress_mode4(&data[4..packed + 4], plain).map_err(|e| e.to_string())
        }
    }
    fn node(&mut self, id: u32) -> Result<Node, String> {
        if let Some(node) = self
            .node_cache
            .as_ref()
            .and_then(|(nodes, _)| nodes.get(&id))
        {
            return Ok(node.clone());
        }
        let b = self.block(id, b'L', 1024 * 1024)?;
        if b.len() < 7 || u16at(&b, 0) != 0 || u16at(&b, 2) != 0xffff {
            return Err("not a MainKey node".into());
        }
        let count = u16at(&b, 4) as usize;
        let branch = b[6] != 0;
        let stride = if !branch { 10 } else { 14 };
        let start = if !branch { 7 } else { 11 };
        if start + count * stride > b.len() {
            return Err("truncated MainKey node".into());
        }
        let mut entries = Vec::with_capacity(count);
        let mut children = Vec::new();
        for i in 0..count {
            let at = start + i * stride;
            let e = Entry {
                oid: u32at(&b, at),
                block: u32at(&b, at + 4),
                checksum: u16at(&b, at + 8),
            };
            if entries.last().map(|p: &Entry| p.oid >= e.oid) == Some(true) {
                return Err("unordered MainKey node".into());
            }
            entries.push(e);
        }
        if branch {
            for i in 0..=count {
                let c = u32at(&b, 7 + i * 14);
                if c == 0 {
                    return Err("null child in MainKey".into());
                }
                children.push(c);
            }
        }
        let node = Node {
            branch,
            entries,
            children,
        };
        if let Some((nodes, bytes)) = &mut self.node_cache {
            let size =
                node.entries.len() * std::mem::size_of::<Entry>() + node.children.len() * 4 + 96;
            if *bytes + size <= 1024 * 1024 {
                nodes.insert(id, node.clone());
                *bytes += size;
            }
        }
        Ok(node)
    }
    pub fn lookup(&mut self, oid: u32) -> Result<Option<Entry>, String> {
        Ok(self
            .lookup_including_deleted(oid)?
            .filter(|e| e.block != u32::MAX))
    }
    /// Keep tombstones distinct from absent keys when resolving mounted volumes.
    pub fn lookup_including_deleted(&mut self, oid: u32) -> Result<Option<Entry>, String> {
        if self.root == 0 {
            return Ok(None);
        }
        let mut id = self.root;
        let mut visited = HashSet::new();
        loop {
            if visited.len() >= 32 || !visited.insert(id) {
                return Err("cycle/depth limit in MainKey".into());
            }
            let n = self.node(id)?;
            match n.entries.binary_search_by_key(&oid, |e| e.oid) {
                Ok(i) => return Ok(Some(n.entries[i])),
                Err(i) => {
                    if !n.branch {
                        return Ok(None);
                    }
                    id = n.children[i];
                }
            }
        }
    }
    /// Batched point lookup, pruning branches by sorted requested OIDs.
    /// Includes tombstones, without decoding any document bodies.
    pub fn lookup_many(&mut self, ids: &[u32]) -> Result<Vec<Entry>, String> {
        if ids.windows(2).any(|w| w[0] >= w[1]) {
            return Err("MainKey batch must be sorted and unique".into());
        }
        fn visit(
            index: &mut MainKey,
            node: u32,
            ids: &[u32],
            depth: usize,
            seen: &mut HashSet<u32>,
            out: &mut Vec<Entry>,
        ) -> Result<(), String> {
            if ids.is_empty() {
                return Ok(());
            }
            if depth >= 32 || seen.len() >= 100000 || !seen.insert(node) {
                return Err("MainKey batch tree budget/cycle".into());
            }
            let n = index.node(node)?;
            let mut at = 0;
            for (i, e) in n.entries.iter().enumerate() {
                let begin = at;
                while at < ids.len() && ids[at] < e.oid {
                    at += 1;
                }
                if n.branch && at > begin {
                    visit(index, n.children[i], &ids[begin..at], depth + 1, seen, out)?;
                }
                if at < ids.len() && ids[at] == e.oid {
                    out.push(*e);
                    at += 1;
                }
                if at == ids.len() {
                    break;
                }
            }
            if n.branch && at < ids.len() {
                visit(
                    index,
                    *n.children.last().ok_or("missing MainKey child")?,
                    &ids[at..],
                    depth + 1,
                    seen,
                    out,
                )?;
            }
            Ok(())
        }
        let mut out = Vec::new();
        if self.root != 0 {
            visit(self, self.root, ids, 0, &mut HashSet::new(), &mut out)?;
        }
        Ok(out)
    }
    pub fn entries(&mut self, limit: usize) -> Result<Vec<Entry>, String> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        if self.root != 0 {
            self.walk(self.root, 0, limit, &mut out, &mut seen)?;
        }
        if out.windows(2).any(|v| v[0].oid >= v[1].oid) {
            return Err("overlapping MainKey subtrees".into());
        }
        Ok(out)
    }
    fn walk(
        &mut self,
        id: u32,
        depth: usize,
        limit: usize,
        out: &mut Vec<Entry>,
        seen: &mut HashSet<u32>,
    ) -> Result<(), String> {
        if seen.len() >= 4096 || !seen.insert(id) {
            return Err("MainKey traversal node budget/cycle".into());
        }
        if depth >= 32 {
            return Err("MainKey depth limit".into());
        }
        let n = self.node(id)?;
        for i in 0..n.entries.len() {
            if n.branch {
                self.walk(n.children[i], depth + 1, limit, out, seen)?;
            }
            if out.len() >= limit {
                return Err("MainKey enumeration entry budget exceeded; use OID lookup".into());
            }
            out.push(n.entries[i]);
        }
        if n.branch {
            self.walk(*n.children.last().unwrap(), depth + 1, limit, out, seen)?;
        }
        Ok(())
    }
    pub fn record_offset(&mut self, e: Entry) -> Result<u64, String> {
        if e.block == u32::MAX {
            return Err("deleted MainKey entry".into());
        }
        let at = self.offset(e.block);
        let h = self.read(at, 7)?;
        if h[4] != b'C' || u16at(&h, 5) != e.checksum {
            return Err(format!("MainKey/record header mismatch for OID {}", e.oid));
        }
        Ok(at)
    }
}
