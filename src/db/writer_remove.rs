//! Local B-tree deletion. Underflow repair keeps every non-root node nonempty
//! and all leaves at the same depth, including variable-length native keys.
use super::*;

impl Tree<'_> {
    fn load_for_remove(&mut self, id: u32) -> Result<WriterNode, String> {
        if self.kind == 3 {
            self.load_native(id)
        } else {
            self.load(id)
        }
    }
    fn discard(&mut self, id: u32) -> Result<(), String> {
        self.image.release_frame(id, b'L')?;
        self.staged.remove(&id);
        Ok(())
    }
    pub(super) fn remove(&mut self, root: u32, key: &[u8]) -> Result<(u32, bool), String> {
        if root == 0 {
            return Ok((0, false));
        }
        let (mut root, removed) = if self.kind == 3 {
            // Native type 3 also holds identifiers such as number/year. Their
            // collation need not match SQL numeric/string comparison. Find the
            // exact stored key and preserve the native in-order sequence.
            self.remove_exact_at(root, key, 0, &mut BTreeSet::new())?
        } else {
            self.remove_at(root, key, 0, &mut BTreeSet::new())?
        };
        if removed {
            let n = self.load_for_remove(root)?;
            if n.keys.is_empty() {
                self.discard(root)?;
                root = n.children.first().copied().unwrap_or(0);
            }
        }
        Ok((root, removed))
    }
    fn maximum(&mut self, mut id: u32) -> Result<Vec<u8>, String> {
        let mut seen = BTreeSet::new();
        loop {
            if id == 0 || seen.len() >= 32 || !seen.insert(id) {
                return Err("DB40 delete predecessor cycle/depth".into());
            }
            let node = self.load_for_remove(id)?;
            if let Some(next) = node.children.last() {
                id = *next;
            } else {
                return node
                    .keys
                    .last()
                    .cloned()
                    .ok_or_else(|| "DB40 empty predecessor".into());
            }
        }
    }
    fn remove_exact_at(
        &mut self,
        id: u32,
        key: &[u8],
        depth: usize,
        seen: &mut BTreeSet<u32>,
    ) -> Result<(u32, bool), String> {
        if depth >= 32 || !seen.insert(id) {
            return Err("DB40 exact delete cycle/depth".into());
        }
        let mut n = self.load_for_remove(id)?;
        if let Some(at) = n.keys.iter().position(|k| k == key) {
            if n.children.is_empty() {
                n.keys.remove(at);
            } else {
                let replacement = self.maximum(n.children[at])?;
                let (child, removed) =
                    self.remove_exact_at(n.children[at], &replacement, depth + 1, seen)?;
                if !removed {
                    return Err("DB40 exact delete predecessor missing".into());
                }
                n.keys[at] = replacement;
                n.children[at] = child;
                self.repair_empty(&mut n, at)?;
            }
        } else {
            let mut found = false;
            for at in 0..n.children.len() {
                let (child, removed) =
                    self.remove_exact_at(n.children[at], key, depth + 1, seen)?;
                if removed {
                    n.children[at] = child;
                    self.repair_empty(&mut n, at)?;
                    found = true;
                    break;
                }
            }
            if !found {
                return Ok((id, false));
            }
        }
        self.discard(id)?;
        Ok((self.save(n)?, true))
    }
    fn remove_at(
        &mut self,
        id: u32,
        key: &[u8],
        depth: usize,
        seen: &mut BTreeSet<u32>,
    ) -> Result<(u32, bool), String> {
        if depth >= 32 || !seen.insert(id) {
            return Err("DB40 delete cycle/depth".into());
        }
        let mut n = self.load(id)?;
        match self.position(&n.keys, key)? {
            Ok(i) if n.children.is_empty() => {
                n.keys.remove(i);
            }
            Ok(i) => {
                let replacement = self.maximum(n.children[i])?;
                let (child, removed) =
                    self.remove_at(n.children[i], &replacement, depth + 1, seen)?;
                if !removed {
                    return Err("DB40 delete predecessor disappeared".into());
                }
                n.keys[i] = replacement;
                n.children[i] = child;
                self.repair_empty(&mut n, i)?;
            }
            Err(_) if n.children.is_empty() => return Ok((id, false)),
            Err(i) => {
                let (child, removed) = self.remove_at(n.children[i], key, depth + 1, seen)?;
                if !removed {
                    return Ok((id, false));
                }
                n.children[i] = child;
                self.repair_empty(&mut n, i)?;
            }
        }
        self.discard(id)?;
        Ok((self.save(n)?, true))
    }
    fn repair_empty(&mut self, parent: &mut WriterNode, at: usize) -> Result<(), String> {
        let child_id = parent.children[at];
        let mut child = self.load_for_remove(child_id)?;
        if !child.keys.is_empty() {
            return Ok(());
        }
        if parent.keys.is_empty() {
            return Ok(());
        } // root contraction in remove()
        let sibling_at = if at > 0 { at - 1 } else { 1 };
        let pivot = if at > 0 { at - 1 } else { at };
        let sibling_id = parent.children[sibling_at];
        let mut sibling = self.load_for_remove(sibling_id)?;
        if sibling.keys.is_empty() || child.children.is_empty() != sibling.children.is_empty() {
            return Err("DB40 delete sibling height/occupancy mismatch".into());
        }
        self.discard(child_id)?;
        self.discard(sibling_id)?;
        if sibling.keys.len() > 1 {
            if at > 0 {
                child.keys.push(std::mem::replace(
                    &mut parent.keys[pivot],
                    sibling.keys.pop().unwrap(),
                ));
                if !sibling.children.is_empty() {
                    child.children.insert(0, sibling.children.pop().unwrap());
                }
            } else {
                child.keys.push(std::mem::replace(
                    &mut parent.keys[pivot],
                    sibling.keys.remove(0),
                ));
                if !sibling.children.is_empty() {
                    child.children.push(sibling.children.remove(0));
                }
            }
            parent.children[at] = self.save(child)?;
            parent.children[sibling_at] = self.save(sibling)?;
        } else {
            let (mut left, right) = if at > 0 {
                (sibling, child)
            } else {
                (child, sibling)
            };
            left.keys.push(parent.keys.remove(pivot));
            left.keys.extend(right.keys);
            left.children.extend(right.children);
            parent.children[pivot] = self.save(left)?;
            parent.children.remove(pivot + 1);
        }
        Ok(())
    }
    fn unlink(&mut self, root: u32, group: u32, oid: u32) -> Result<(u32, bool), String> {
        let mut bound = group.to_le_bytes().to_vec();
        bound.extend(oid.to_le_bytes());
        bound.push(1);
        let Some(key) = self
            .predecessor(root, &bound)?
            .filter(|key| u32at(key, 0) == group)
        else {
            return Ok((root, false));
        };
        let mut ids = groups(&[key.clone()])?.remove(&group).unwrap();
        let Ok(at) = ids.binary_search(&oid) else {
            return Ok((root, false));
        };
        ids.remove(at);
        let (mut root, removed) = self.remove(root, &key)?;
        if !removed {
            return Err("DB40 missing link segment during removal".into());
        }
        for key in linked_items(BTreeMap::from([(group, ids)])) {
            root = self.upsert(root, key)?;
        }
        Ok((root, true))
    }
}

// Only the affected presence-map bucket is reencoded; other buckets are kept.
fn remove_presence(bytes: &[u8], oid: u32) -> Result<Vec<u8>, String> {
    let (mut pos, mut previous) = (0, None);
    loop {
        let start = pos;
        let raw = u32at(
            bytes
                .get(pos..pos + 4)
                .ok_or("DB40 truncated presence map")?,
            0,
        );
        if raw == u32::MAX {
            return Err("DB40 deleted OID absent from presence map".into());
        }
        let group = raw & 0x7fff_ffff;
        if previous.is_some_and(|p| p >= group) {
            return Err("DB40 unordered presence map".into());
        }
        previous = Some(group);
        pos += 4;
        let mut ids = Vec::new();
        if raw & 0x8000_0000 != 0 {
            let len = u16at(bytes.get(pos..pos + 2).ok_or("DB40 presence length")?, 0) as usize;
            pos += 2;
            let data = bytes.get(pos..pos + len).ok_or("DB40 presence segment")?;
            pos += len;
            if group == oid >> 11 {
                let (mut p, mut offset) = (0, 0u32);
                loop {
                    if *data.get(p).ok_or("DB40 presence terminator")? == 0 {
                        p += 1;
                        break;
                    }
                    let (mut delta, mut complete) = (0u32, false);
                    for _ in 0..5 {
                        let b = *data.get(p).ok_or("DB40 presence delta")?;
                        p += 1;
                        delta = delta
                            .checked_mul(128)
                            .and_then(|v| v.checked_add((b & 127) as u32))
                            .ok_or("DB40 presence delta overflow")?;
                        if b & 128 == 0 {
                            complete = true;
                            break;
                        }
                    }
                    if !complete || delta == 0 {
                        return Err("DB40 invalid presence delta".into());
                    }
                    offset = offset.checked_add(delta).ok_or("DB40 presence overflow")?;
                    if offset >= 2048 {
                        return Err("DB40 presence offset".into());
                    }
                    ids.push((group << 11) | offset);
                }
                if p != data.len() {
                    return Err("DB40 trailing presence bytes".into());
                }
            }
        } else {
            let bits = bytes.get(pos..pos + 256).ok_or("DB40 presence bitmap")?;
            pos += 256;
            if group == oid >> 11 {
                for n in 0..2048 {
                    if bits[n / 8] & (1 << (n % 8)) != 0 {
                        ids.push((group << 11) | n as u32);
                    }
                }
            }
        }
        if group == oid >> 11 {
            let at = ids
                .binary_search(&oid)
                .map_err(|_| "DB40 deleted OID absent from bucket")?;
            ids.remove(at);
            let mut out = bytes[..start].to_vec();
            if !ids.is_empty() {
                let bucket = oid_map(ids.into_iter());
                let len = if u32at(&bucket, 0) & 0x8000_0000 != 0 {
                    6 + u16at(&bucket, 4) as usize
                } else {
                    260
                };
                out.extend(&bucket[..len]);
            }
            out.extend(&bytes[pos..]);
            out.resize((out.len() + 7).div_ceil(1024) * 1024 - 7, 0);
            return Ok(out);
        }
        if group > oid >> 11 {
            return Err("DB40 deleted OID bucket missing".into());
        }
    }
}

pub(super) fn delete(
    path: &Path,
    oid: u32,
    hole: bool,
    base: std::sync::Arc<Vec<u8>>,
) -> Result<Option<Vec<u8>>, String> {
    let mut index = MainKey::from_image(base.clone())?;
    let Some(entry) = index.lookup_including_deleted(oid)? else {
        return Ok(None);
    };
    let previous = if entry.block == u32::MAX {
        None
    } else {
        Some(index.record(entry)?)
    };
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
        return Ok(None);
    }
    let new_presence = if hole {
        None
    } else {
        let Some(bytes) = index.writer_presence()? else {
            return Ok(None);
        };
        Some(remove_presence(&bytes, oid)?)
    };
    let schema = crate::db::schema::Schema::read(path)?;
    let mut image = Image::new(base.as_ref().clone())?;
    if let Some(record) = &previous {
        let area = schema
            .areas
            .get(&record.area)
            .ok_or("DB40 deleted area missing")?;
        let records = BTreeMap::from([(oid, record.clone())]);
        for field in area.fields.values().filter(|f| f.flags & 1 != 0) {
            let old = roots
                .get(&(record.area, field.id))
                .copied()
                .unwrap_or((0, 0));
            if field.kind == 4 {
                if old.1 != 0 {
                    return Err("DB40 populated CTF index rewrite is not implemented".into());
                }
                continue;
            }
            let (keys, _) = field_items(&records, record.area, field)?;
            if keys.is_empty() {
                continue;
            }
            let mut tree = Tree {
                index: &mut index,
                image: &mut image,
                area: record.area,
                attr: field.id,
                kind: field.kind,
                staged: BTreeMap::new(),
            };
            let (mut root, mut count) = (old.1, old.0);
            for key in keys {
                let (next, removed) = if field.kind == 5 {
                    tree.unlink(root, u32at(&key, 0), oid)?
                } else {
                    tree.remove(root, &key)?
                };
                if !removed {
                    return Err("DB40 deleted record/index mismatch".into());
                }
                root = next;
                count = count.wrapping_sub(1);
            }
            roots.insert((record.area, field.id), (count, root));
        }
        let old = roots
            .get(&(65535, 1))
            .copied()
            .ok_or("DB40 membership index missing")?;
        let mut tree = Tree {
            index: &mut index,
            image: &mut image,
            area: 65535,
            attr: 1,
            kind: 5,
            staged: BTreeMap::new(),
        };
        let (root, removed) = tree.unlink(old.1, record.area as u32, oid)?;
        if !removed {
            return Err("DB40 deleted record/membership mismatch".into());
        }
        roots.insert((65535, 1), (old.0.wrapping_sub(1), root));
        image.release_frame(entry.block, b'C')?;
    }
    let old = roots
        .get(&(65535, 0))
        .copied()
        .ok_or("DB40 MainKey missing")?;
    let mut tree = Tree {
        index: &mut index,
        image: &mut image,
        area: 65535,
        attr: 0,
        kind: 0,
        staged: BTreeMap::new(),
    };
    let mut key = oid.to_le_bytes().to_vec();
    key.extend(u32::MAX.to_le_bytes());
    key.extend(0u16.to_le_bytes());
    let root = if hole {
        tree.upsert(old.1, key)?
    } else {
        let (root, removed) = tree.remove(old.1, &key)?;
        if !removed {
            return Err("DB40 deleted MainKey entry absent".into());
        }
        root
    };
    roots.insert((65535, 0), (old.0.wrapping_add(1), root));
    image.release_frame(u32at(&header, 15), b'K')?;
    let mut directory = Vec::new();
    for ((area, attr), (count, root)) in roots {
        directory.extend(count.to_le_bytes());
        directory.extend(root.to_le_bytes());
        directory.extend(area.to_le_bytes());
        directory.extend(attr.to_le_bytes());
    }
    let root = image.frame(b'K', &directory)?.0;
    image.bytes[15..19].copy_from_slice(&root.to_le_bytes());
    if let Some(bytes) = new_presence {
        image.release_frame(u32at(&header, 43), b'D')?;
        let root = image.frame(b'D', &bytes)?.0;
        image.bytes[43..47].copy_from_slice(&root.to_le_bytes());
    }
    Ok(Some(image.bytes))
}
