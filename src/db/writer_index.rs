//! Build the replacement image from existing native indexes and changed records.
//! Unchanged document streams are copied verbatim, without decoding them.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
#[path = "writer_insert.rs"]
mod insert;

fn groups(items: &[Vec<u8>]) -> Result<BTreeMap<u32, Vec<u32>>, String> {
    let mut result = BTreeMap::<u32, Vec<u32>>::new();
    let mut previous_key = None;
    for item in items {
        if item.len() < 9 || item[8] == 0 {
            return Err("invalid DB40 link segment".into());
        }
        let key = (u32at(item, 0), u32at(item, 4));
        if previous_key.is_some_and(|p| p >= key) {
            return Err("unordered DB40 link segments".into());
        }
        previous_key = Some(key);
        let ids = result.entry(key.0).or_default();
        let mut oid = key.1;
        let mut at = 9;
        for n in 0..item[8] {
            if n != 0 {
                let mut delta = 0u32;
                let mut complete = false;
                for _ in 0..5 {
                    let b = *item.get(at).ok_or("truncated DB40 link delta")?;
                    at += 1;
                    delta = delta
                        .checked_mul(128)
                        .and_then(|v| v.checked_add((b & 127) as u32))
                        .ok_or("DB40 link delta overflow")?;
                    if b & 128 == 0 {
                        complete = true;
                        break;
                    }
                }
                if !complete || delta == 0 {
                    return Err("invalid DB40 link delta".into());
                }
                oid = oid.checked_add(delta).ok_or("DB40 link OID overflow")?;
            }
            if ids.last().is_some_and(|p| *p >= oid) {
                return Err("overlapping DB40 link segments".into());
            }
            ids.push(oid);
        }
        if at != item.len() {
            return Err("trailing DB40 link segment bytes".into());
        }
    }
    Ok(result)
}
fn remove_ids(groups: &mut BTreeMap<u32, Vec<u32>>, affected: &BTreeSet<u32>) {
    groups.retain(|_, ids| {
        ids.retain(|oid| !affected.contains(oid));
        !ids.is_empty()
    });
}
pub(super) fn candidate_changes(
    path: &Path,
    changes: &[Record],
    deletions: &[(u32, bool)],
) -> Result<Vec<u8>, String> {
    check_volume_budget(path)?;
    candidate_from_image(
        path,
        changes,
        deletions,
        std::sync::Arc::new(std::fs::read(path).map_err(|e| e.to_string())?),
    )
}
pub(super) fn candidate_from_image(
    path: &Path,
    changes: &[Record],
    deletions: &[(u32, bool)],
    base: std::sync::Arc<Vec<u8>>,
) -> Result<Vec<u8>, String> {
    if base.len() >= 7 && u16at(&base, 5) == 0x612 {
        return user_index::legacy_candidate(path, changes, deletions, base);
    }
    if changes.is_empty() && deletions.len() == 1 {
        let (oid, hole) = deletions[0];
        if let Some(image) = insert::delete(path, oid, hole, base.clone())? {
            return Ok(image);
        }
    }
    if changes.len() == 1 && deletions.is_empty() {
        match insert::append(path, &changes[0], base.clone()) {
            Ok(Some(image)) => return Ok(image),
            Ok(None) => {}
            Err(e) if e == insert::REBUILD_NUMERIC => {}
            Err(e) => return Err(e),
        }
    }
    check_volume_budget(path)?;
    let mut index = super::super::mainkey::MainKey::from_image(base.clone())?;
    let (header, directory) = index.writer_parts();
    let mut roots = BTreeMap::new();
    for d in directory.chunks_exact(10) {
        if roots
            .insert((u16at(d, 6), u16at(d, 8)), (u16at(d, 0), u32at(d, 2)))
            .is_some()
        {
            return Err("duplicate DB40 index directory entry".into());
        }
    }
    let schema = super::super::schema::Schema::read(path)?;
    let mut image = Image::new(base.as_ref().clone())?;
    if image.bytes[..289] != header {
        return Err("DB40 header changed during transaction".into());
    }
    let entries = index.entries(1_000_000)?;
    let mut main = entries
        .iter()
        .map(|e| (e.oid, (e.block, e.checksum)))
        .collect::<BTreeMap<_, _>>();
    let mut affected = BTreeSet::new();
    let mut replacements = BTreeMap::new();
    for r in changes {
        let oid = r
            .integer(1024)
            .filter(|v| *v != 0)
            .ok_or("DB40 write requires nonzero OID")?;
        if !affected.insert(oid) {
            return Err("duplicate DB40 change".into());
        }
        if !schema.areas.contains_key(&r.area) {
            return Err("DB40 target area missing".into());
        }
        replacements.insert(oid, r.clone());
    }
    for &(oid, _) in deletions {
        if oid == 0 || !affected.insert(oid) {
            return Err("invalid/duplicate DB40 deletion".into());
        }
    }
    let mut before = BTreeMap::new();
    for &oid in &affected {
        if let Some(entry) = entries
            .binary_search_by_key(&oid, |e| e.oid)
            .ok()
            .map(|n| entries[n])
        {
            if entry.block != u32::MAX {
                before.insert(oid, index.record(entry)?);
            }
        }
    }
    if roots.get(&(65535, 2)).is_some_and(|r| r.1 != 0) {
        user_index::update(&mut index, &mut image, &mut roots, &schema, &before, &replacements, &affected)?;
    }
    let mut fields = BTreeSet::new();
    for &oid in &affected {
        let old = before.get(&oid);
        let new = replacements.get(&oid);
        if let (Some(a), Some(b)) = (old, new) {
            if a.area != b.area {
                return Err("DB40 cannot change object area".into());
            }
        }
        let Some(area_id) = old.or(new).map(|r| r.area) else {
            continue;
        };
        let area = schema
            .areas
            .get(&area_id)
            .ok_or("DB40 target area missing")?;
        for field in area.fields.values().filter(|f| f.flags & 1 != 0) {
            let values = |r: Option<&Record>| {
                r.map(|r| {
                    r.fields
                        .iter()
                        .filter(|f| f.id == field.id && f.wire_type == field.kind)
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
            };
            if values(old) == values(new) {
                continue;
            }
            if field.kind == 4 {
                if roots.get(&(area_id, field.id)).is_some_and(|r| r.1 != 0) {
                    return Err("DB40 populated CTF index rewrite is not implemented".into());
                }
            } else {
                fields.insert((area_id, field.id));
            }
        }
    }
    let main_tree = index.writer_tree(65535, 0, Some(10))?;
    let area_tree = index.writer_tree(65535, 1, None)?;
    let mut members = groups(&area_tree.items)?;
    let mut member_set = BTreeSet::new();
    for (&area, ids) in &members {
        if area > u16::MAX as u32 || !schema.areas.contains_key(&(area as u16)) {
            return Err("DB40 membership area missing".into());
        }
        for oid in ids {
            if !member_set.insert(*oid) || !main.get(oid).is_some_and(|r| r.0 != u32::MAX) {
                return Err("DB40 membership/MainKey mismatch".into());
            }
        }
    }
    if member_set.len() != main.values().filter(|r| r.0 != u32::MAX).count() {
        return Err("DB40 incomplete area membership index".into());
    }
    remove_ids(&mut members, &affected);
    for (&oid, r) in &replacements {
        members.entry(r.area as u32).or_default().push(oid);
    }
    let mut updated = BTreeMap::new();
    let mut release = main_tree.blocks;
    release.extend(area_tree.blocks);
    for &(area, attr) in &fields {
        let field = &schema.areas[&area].fields[&attr];
        let size = match field.kind {
            1 => Some(8),
            12 => Some(12),
            13 => Some(36),
            _ => None,
        };
        let tree = index.writer_tree(area, attr, size)?;
        release.extend(tree.blocks);
        let (added, _) = field_items(&replacements, area, field)?;
        let (items, variable, count) = if field.kind == 5 {
            let mut links = groups(&tree.items)?;
            remove_ids(&mut links, &affected);
            for (key, ids) in groups(&added)? {
                links.entry(key).or_default().extend(ids);
            }
            let count: usize = links.values().map(Vec::len).sum();
            (linked_items(links), true, count)
        } else {
            let offset = if field.kind == 1 { 4 } else { 0 };
            let mut items = Vec::new();
            for item in tree.items {
                if item.len() < offset + 4 || size.is_some_and(|n| item.len() != n) {
                    return Err("invalid DB40 scalar index key".into());
                }
                if !affected.contains(&u32at(&item, offset)) {
                    items.push(item);
                }
            }
            items.extend(added);
            let (items, variable) = sort_field_items(field.kind, items)?;
            let count = items.len();
            (items, variable, count)
        };
        updated.insert((area, attr), (items, variable, count));
    }
    image.release_frame(u32at(&header, 15), b'K')?;
    if u32at(&header, 43) != 0 {
        image.release_frame(u32at(&header, 43), b'D')?;
    }
    for block in release {
        image.release_frame(block, b'L')?;
    }
    for &oid in &affected {
        if let Some(&(block, _)) = main.get(&oid) {
            if block != u32::MAX {
                image.release_frame(block, b'C')?;
            }
        }
    }
    for (&oid, record) in &replacements {
        main.insert(oid, image.frame(b'C', &encode_record(record)?)?);
    }
    for &(oid, hole) in deletions {
        main.remove(&oid);
        if hole {
            main.insert(oid, (u32::MAX, 0));
        }
    }
    let main_items = main
        .iter()
        .map(|(&oid, &(block, crc))| {
            let mut item = oid.to_le_bytes().to_vec();
            item.extend(block.to_le_bytes());
            item.extend(crc.to_le_bytes());
            item
        })
        .collect::<Vec<_>>();
    let generation = roots
        .get(&(65535, 0))
        .ok_or("MainKey root missing")?
        .0
        .wrapping_add(affected.len() as u16);
    roots.insert(
        (65535, 0),
        (generation, image.tree(65535, 0, &main_items, false)?),
    );
    let count: usize = members.values().map(Vec::len).sum();
    roots.insert(
        (65535, 1),
        (
            count as u16,
            image.tree(65535, 1, &linked_items(members), true)?,
        ),
    );
    for ((area, attr), (items, variable, count)) in updated {
        roots.insert(
            (area, attr),
            (count as u16, image.tree(area, attr, &items, variable)?),
        );
    }
    let mut directory = Vec::new();
    for ((area, attr), (count, block)) in roots {
        directory.extend(count.to_le_bytes());
        directory.extend(block.to_le_bytes());
        directory.extend(area.to_le_bytes());
        directory.extend(attr.to_le_bytes());
    }
    let root = image.frame(b'K', &directory)?.0;
    image.bytes[15..19].copy_from_slice(&root.to_le_bytes());
    let presence = image.frame(b'D', &oid_map(main.keys().copied()))?.0;
    image.bytes[43..47].copy_from_slice(&presence.to_le_bytes());
    Ok(image.bytes)
}
