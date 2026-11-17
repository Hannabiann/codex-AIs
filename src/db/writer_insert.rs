//! Copy-on-write insertion into native B-trees. Only nodes on changed paths are
//! replaced. Additive updates use the same path; key removal and deletions use
//! the index-delta builder.
use super::*;
use crate::db::mainkey::{MainKey, WriterNode};
use std::cmp::Ordering;
#[path = "writer_remove.rs"]
mod remove;
pub(super) fn delete(
    path: &Path,
    oid: u32,
    hole: bool,
    base: std::sync::Arc<Vec<u8>>,
) -> Result<Option<Vec<u8>>, String> {
    // A variable-length separator can grow during deletion and overflow its
    // parent. Rebuild that unusual index using the existing checked path.
    match remove::delete(path, oid, hole, base) {
        Err(e) if e == "DB40 index node overflow" => Ok(None),
        other => other,
    }
}

fn compare(kind: u16, a: &[u8], b: &[u8]) -> Result<Ordering, String> {
    let min = match kind {
        0 => 10,
        1 => 8,
        2 | 3 => 4,
        5 => 9,
        12 => 12,
        13 => 36,
        _ => return Err("unsupported insert index kind".into()),
    };
    if a.len() < min || b.len() < min {
        return Err("short insert index key".into());
    }
    Ok(match kind {
        0 => u32at(a, 0).cmp(&u32at(b, 0)),
        5 => (u32at(a, 0), u32at(a, 4)).cmp(&(u32at(b, 0), u32at(b, 4))),
        1 => (u16at(a, 0), a[2], a[3], u32at(a, 4)).cmp(&(u16at(b, 0), b[2], b[3], u32at(b, 4))),
        2 => a[4..]
            .cmp(&b[4..])
            .then_with(|| u32at(a, 0).cmp(&u32at(b, 0))),
        3 => crate::ks::sql::numeric_cmp(&crate::db::decode(&a[4..]), &crate::db::decode(&b[4..]))
            .ok_or("invalid numeric insert index key")?
            .then_with(|| u32at(a, 0).cmp(&u32at(b, 0))),
        12 => a[4..]
            .cmp(&b[4..])
            .then_with(|| u32at(a, 0).cmp(&u32at(b, 0))),
        13 => decimal_cmp(&a[4..], &b[4..]).then_with(|| u32at(a, 0).cmp(&u32at(b, 0))),
        _ => unreachable!(),
    })
}
struct Tree<'a> {
    index: &'a mut MainKey,
    image: &'a mut Image,
    area: u16,
    attr: u16,
    kind: u16,
    staged: BTreeMap<u32, WriterNode>,
}
impl Tree<'_> {
    fn size(&self) -> Option<usize> {
        match self.kind {
            0 => Some(10),
            1 => Some(8),
            12 => Some(12),
            13 => Some(36),
            _ => None,
        }
    }
    fn load(&mut self, id: u32) -> Result<WriterNode, String> {
        let node = self.load_native(id)?;
        for pair in node.keys.windows(2) {
            if compare(self.kind, &pair[0], &pair[1])? != Ordering::Less {
                return Err(format!(
                    "unordered insert index node {id}, area {}, attribute {}, kind {}",
                    self.area, self.attr, self.kind
                ));
            }
        }
        Ok(node)
    }
    fn load_native(&mut self, id: u32) -> Result<WriterNode, String> {
        let node = if let Some(n) = self.staged.get(&id) {
            n.clone()
        } else {
            self.index
                .writer_node(id, self.area, self.attr, self.size())?
        };
        Ok(node)
    }
    fn save(&mut self, n: WriterNode) -> Result<u32, String> {
        let id = self.image.node(
            self.area,
            self.attr,
            &n.keys,
            &n.children,
            self.size().is_none(),
        )?;
        self.staged.insert(id, n);
        Ok(id)
    }
    fn position(&self, keys: &[Vec<u8>], key: &[u8]) -> Result<Result<usize, usize>, String> {
        let (mut low, mut high) = (0, keys.len());
        while low < high {
            let mid = low + (high - low) / 2;
            match compare(self.kind, &keys[mid], key)? {
                Ordering::Less => low = mid + 1,
                Ordering::Greater => high = mid,
                Ordering::Equal => return Ok(Ok(mid)),
            }
        }
        Ok(Err(low))
    }
    fn upsert(&mut self, root: u32, key: Vec<u8>) -> Result<u32, String> {
        let (left, split) = self.insert(root, key, 0, &mut BTreeSet::new())?;
        if let Some((pivot, right)) = split {
            self.save(WriterNode {
                keys: vec![pivot],
                children: vec![left, right],
            })
        } else {
            Ok(left)
        }
    }
    fn insert(
        &mut self,
        id: u32,
        key: Vec<u8>,
        depth: usize,
        seen: &mut BTreeSet<u32>,
    ) -> Result<(u32, Option<(Vec<u8>, u32)>), String> {
        if depth >= 32 || (id != 0 && !seen.insert(id)) {
            return Err("insert index cycle/depth".into());
        }
        if id == 0 {
            return Ok((
                self.save(WriterNode {
                    keys: vec![key],
                    children: vec![],
                })?,
                None,
            ));
        }
        let mut n = self.load(id)?;
        match self.position(&n.keys, &key)? {
            Ok(i) => {
                if n.keys[i] == key {
                    return Ok((id, None));
                }
                n.keys[i] = key;
            }
            Err(i) if n.children.is_empty() => n.keys.insert(i, key),
            Err(i) => {
                let (child, split) = self.insert(n.children[i], key, depth + 1, seen)?;
                if child == n.children[i] && split.is_none() {
                    return Ok((id, None));
                }
                n.children[i] = child;
                if let Some((pivot, right)) = split {
                    n.keys.insert(i, pivot);
                    n.children.insert(i + 1, right);
                }
            }
        }
        self.image.release_frame(id, b'L')?;
        self.staged.remove(&id);
        let bytes = 7
            + n.keys.iter().map(Vec::len).sum::<usize>()
            + if self.size().is_none() {
                n.keys.len() * 2
            } else {
                0
            }
            + n.children.len() * 4;
        if bytes <= 4089 {
            return Ok((self.save(n)?, None));
        }
        let middle = n.keys.len() / 2;
        if middle == 0 || middle + 1 >= n.keys.len() {
            return Err("DB40 inserted key exceeds node capacity".into());
        }
        let right_keys = n.keys.split_off(middle + 1);
        let pivot = n.keys.pop().unwrap();
        let right_children = if n.children.is_empty() {
            vec![]
        } else {
            n.children.split_off(middle + 1)
        };
        let left = self.save(n)?;
        let right = self.save(WriterNode {
            keys: right_keys,
            children: right_children,
        })?;
        Ok((left, Some((pivot, right))))
    }
    fn predecessor(&mut self, mut id: u32, key: &[u8]) -> Result<Option<Vec<u8>>, String> {
        let mut seen = BTreeSet::new();
        let mut found = None;
        while id != 0 {
            if seen.len() >= 32 || !seen.insert(id) {
                return Err("insert predecessor cycle/depth".into());
            }
            let n = self.load(id)?;
            let at = match self.position(&n.keys, key)? {
                Ok(i) => i + 1,
                Err(i) => i,
            };
            if at > 0 {
                found = Some(n.keys[at - 1].clone());
            }
            if n.children.is_empty() {
                break;
            }
            id = n.children[at];
        }
        Ok(found)
    }
    fn link(&mut self, mut root: u32, group: u32, oid: u32) -> Result<u32, String> {
        let mut bound = group.to_le_bytes().to_vec();
        bound.extend(oid.to_le_bytes());
        bound.push(1);
        let previous = self
            .predecessor(root, &bound)?
            .filter(|v| u32at(v, 0) == group);
        let mut ids = if let Some(p) = previous {
            groups(&[p])?.remove(&group).unwrap()
        } else {
            vec![]
        };
        match ids.binary_search(&oid) {
            Ok(_) => return Ok(root),
            Err(at) => ids.insert(at, oid),
        }
        for key in linked_items(BTreeMap::from([(group, ids)])) {
            root = self.upsert(root, key)?;
        }
        Ok(root)
    }
}

fn presence(bytes: Option<&[u8]>, oid: u32) -> Result<Vec<u8>, String> {
    let Some(bytes) = bytes else {
        return Ok(oid_map(std::iter::once(oid)));
    };
    let mut at = 0;
    let mut previous = None;
    let mut prefix = 0;
    let mut ids = Vec::new();
    loop {
        let head = bytes.get(at..at + 4).ok_or("truncated OID presence map")?;
        let raw = u32at(head, 0);
        if raw == u32::MAX {
            prefix = prefix.max(at);
            break;
        }
        let group = raw & 0x7fff_ffff;
        if previous.is_some_and(|p| p >= group) || group > oid >> 11 {
            return Err("unordered insert OID presence map".into());
        }
        previous = Some(group);
        let start = at;
        at += 4;
        if raw & 0x8000_0000 != 0 {
            let count = u16at(
                bytes
                    .get(at..at + 2)
                    .ok_or("truncated OID presence length")?,
                0,
            ) as usize;
            at += 2;
            let data = bytes
                .get(at..at + count)
                .ok_or("truncated compressed OID map")?;
            at += count;
            if group == oid >> 11 {
                let (mut pos, mut offset) = (0, 0u32);
                loop {
                    if *data.get(pos).ok_or("OID map terminator absent")? == 0 {
                        pos += 1;
                        break;
                    }
                    let mut delta = 0u32;
                    let mut done = false;
                    for _ in 0..5 {
                        let b = *data.get(pos).ok_or("truncated OID map delta")?;
                        pos += 1;
                        delta = delta
                            .checked_mul(128)
                            .and_then(|n| n.checked_add((b & 127) as u32))
                            .ok_or("OID delta overflow")?;
                        if b & 128 == 0 {
                            done = true;
                            break;
                        }
                    }
                    if !done || delta == 0 {
                        return Err("invalid OID presence delta".into());
                    }
                    offset = offset.checked_add(delta).ok_or("OID offset overflow")?;
                    if offset >= 2048 {
                        return Err("OID offset outside bucket".into());
                    }
                    ids.push((group << 11) | offset);
                }
                if pos != data.len() {
                    return Err("trailing OID presence delta".into());
                }
            }
        } else {
            let bits = bytes.get(at..at + 256).ok_or("truncated OID bitmap")?;
            at += 256;
            if group == oid >> 11 {
                for n in 0..2048 {
                    if bits[n / 8] & (1 << (n % 8)) != 0 {
                        ids.push((group << 11) | n as u32);
                    }
                }
            }
        }
        if group == oid >> 11 {
            prefix = start;
            break;
        }
    }
    if ids.last().is_some_and(|p| *p >= oid) {
        return Err("duplicate/out-of-order OID presence insert".into());
    }
    ids.push(oid);
    let tail = oid_map(ids.into_iter());
    let length = if u32at(&tail, 0) & 0x8000_0000 != 0 {
        6 + u16at(&tail, 4) as usize
    } else {
        260
    };
    let mut out = bytes[..prefix].to_vec();
    out.extend(&tail[..length]);
    out.extend(u32::MAX.to_le_bytes());
    out.resize((out.len() + 7).div_ceil(1024) * 1024 - 7, 0);
    Ok(out)
}

pub(super) fn append(
    path: &Path,
    record: &Record,
    base: std::sync::Arc<Vec<u8>>,
) -> Result<Option<Vec<u8>>, String> {
    check_volume_budget(path)?;
    let oid = record
        .integer(1024)
        .filter(|v| *v != 0)
        .ok_or("DB40 insert OID missing")?;
    let mut index = MainKey::from_image(base.clone())?;
    let entry = index.lookup_including_deleted(oid)?;
    if entry.is_none() && index.max_oid()?.is_some_and(|max| oid <= max) {
        return Ok(None);
    }
    let previous = entry
        .filter(|e| e.block != u32::MAX)
        .map(|e| index.record(e))
        .transpose()?;
    if previous.as_ref().is_some_and(|r| r.area != record.area) {
        return Err("DB40 cannot change object area".into());
    }
    let (header, directory) = index.writer_parts();
    let mut roots = BTreeMap::new();
    for d in directory.chunks_exact(10) {
        if roots
            .insert((u16at(d, 6), u16at(d, 8)), (u16at(d, 0), u32at(d, 2)))
            .is_some()
        {
            return Err("duplicate DB40 index directory".into());
        }
    }
    if roots.get(&(65535, 2)).is_some_and(|r| r.1 != 0) {
        return Err("DB40 write with a populated global word index is not implemented".into());
    }
    let schema = crate::db::schema::Schema::read(path)?;
    let area = schema
        .areas
        .get(&record.area)
        .ok_or("DB40 target area missing")?;
    let one = BTreeMap::from([(oid, record.clone())]);
    let old_records = previous
        .as_ref()
        .map(|r| BTreeMap::from([(oid, r.clone())]))
        .unwrap_or_default();
    let mut additions = Vec::new();
    for field in area.fields.values().filter(|f| f.flags & 1 != 0) {
        let values = |r: &Record| {
            r.fields
                .iter()
                .filter(|f| f.id == field.id && f.wire_type == field.kind)
                .cloned()
                .collect::<Vec<_>>()
        };
        let old_values = previous.as_ref().map(values).unwrap_or_default();
        let new_values = values(record);
        if old_values == new_values {
            continue;
        }
        let old_root = roots
            .get(&(record.area, field.id))
            .copied()
            .unwrap_or((0, 0));
        if field.kind == 4 {
            if old_root.1 != 0 {
                return Err("DB40 populated CTF index rewrite is not implemented".into());
            }
            continue;
        }
        let (old_items, _) = field_items(&old_records, record.area, field)?;
        let (new_items, _) = field_items(&one, record.area, field)?;
        if old_items.iter().any(|key| !new_items.contains(key)) {
            return Ok(None);
        }
        let delta = if field.kind == 5 {
            (old_values.len(), new_values.len())
        } else {
            (old_items.len(), new_items.len())
        };
        let keys = new_items
            .into_iter()
            .filter(|key| !old_items.contains(key))
            .collect::<Vec<_>>();
        additions.push((field, old_root, delta, keys));
    }
    let mut image = Image::new(base.as_ref().clone())?;
    if image.bytes[..289] != header {
        return Err("DB40 header changed during insert".into());
    }
    let new_presence = if entry.is_none() {
        Some(presence(index.writer_presence()?.as_deref(), oid)?)
    } else {
        None
    };
    if let Some(old) = entry.filter(|e| e.block != u32::MAX) {
        image.release_frame(old.block, b'C')?;
    }
    let (block, crc) = image.frame(b'C', &encode_record(record)?)?;
    let mut key = oid.to_le_bytes().to_vec();
    key.extend(block.to_le_bytes());
    key.extend(crc.to_le_bytes());
    let mut tree = Tree {
        index: &mut index,
        image: &mut image,
        area: 65535,
        attr: 0,
        kind: 0,
        staged: BTreeMap::new(),
    };
    let generation = roots
        .get(&(65535, 0))
        .ok_or("MainKey root missing")?
        .0
        .wrapping_add(1);
    let root = tree.upsert(roots[&(65535, 0)].1, key)?;
    roots.insert((65535, 0), (generation, root));
    if previous.is_none() {
        let old = roots.get(&(65535, 1)).copied().unwrap_or((0, 0));
        let mut tree = Tree {
            index: &mut index,
            image: &mut image,
            area: 65535,
            attr: 1,
            kind: 5,
            staged: BTreeMap::new(),
        };
        let root = tree.link(old.1, record.area as u32, oid)?;
        roots.insert((65535, 1), (old.0.wrapping_add(1), root));
    }
    for (field, old, (removed, added), items) in additions {
        let mut tree = Tree {
            index: &mut index,
            image: &mut image,
            area: record.area,
            attr: field.id,
            kind: field.kind,
            staged: BTreeMap::new(),
        };
        let mut root = old.1;
        for item in items {
            root = if field.kind == 5 {
                tree.link(root, u32at(&item, 0), oid)?
            } else {
                tree.upsert(root, item)?
            };
        }
        roots.insert(
            (record.area, field.id),
            (
                old.0
                    .wrapping_sub(removed as u16)
                    .wrapping_add(added as u16),
                root,
            ),
        );
    }
    image.release_frame(u32at(&header, 15), b'K')?;
    if new_presence.is_some() && u32at(&header, 43) != 0 {
        image.release_frame(u32at(&header, 43), b'D')?;
    }
    let mut directory = Vec::new();
    for ((area, attr), (count, root)) in roots {
        directory.extend(count.to_le_bytes());
        directory.extend(root.to_le_bytes());
        directory.extend(area.to_le_bytes());
        directory.extend(attr.to_le_bytes());
    }
    let root = image.frame(b'K', &directory)?.0;
    image.bytes[15..19].copy_from_slice(&root.to_le_bytes());
    if let Some(payload) = new_presence {
        let root = image.frame(b'D', &payload)?.0;
        image.bytes[43..47].copy_from_slice(&root.to_le_bytes());
    }
    Ok(Some(image.bytes))
}
