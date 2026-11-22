//! UserBase login postings. Other full-text document writes remain guarded.
use super::*;
use crate::db::{
    mainkey::{word_index, MainKey},
    schema::Schema,
};
use std::collections::BTreeSet;

// Native string indexing uses field 1 as a title (weight 0x600). The hard
// boundaries differ from punctuation: all contiguous punctuation-separated
// combinations are indexed, except title words containing digits, /, \ or -.
fn punctuation(b: u8) -> bool {
    b"'\".,:;!?~)([]/\\-^%{}=*&#$+<>|".contains(&b)
        || b == 16
        || (127..=191).contains(&b) && b != 185
}
fn words(bytes: &[u8]) -> BTreeSet<Vec<u8>> {
    let mut out = BTreeSet::new();
    let cleaned = bytes
        .iter()
        .copied()
        .filter(|b| !matches!(b, 0 | 1 | 4 | 5))
        .collect::<Vec<_>>();
    for token in cleaned.split(|b| b"_\t\n\r \x07\xa0".contains(b)) {
        let token = &token[..token.len().min(31)];
        let token = text_key(token);
        let bounds = std::iter::once(-1isize)
            .chain(
                token
                    .iter()
                    .enumerate()
                    .filter(|(_, b)| punctuation(**b))
                    .map(|(n, _)| n as isize),
            )
            .chain(std::iter::once(token.len() as isize))
            .collect::<Vec<_>>();
        for end in 1..bounds.len() {
            for start in 0..end {
                let word = &token[(bounds[start] + 1) as usize..bounds[end] as usize];
                if word.len() < 2
                    || punctuation(word[0])
                    || punctuation(*word.last().unwrap())
                    || word
                        .iter()
                        .any(|b| b.is_ascii_digit() || b"/\\-".contains(b))
                {
                    continue;
                }
                out.insert(word.to_vec());
            }
        }
    }
    out
}
fn title(record: &Record, schema: &Schema) -> Result<BTreeSet<Vec<u8>>, String> {
    let area = schema
        .areas
        .get(&record.area)
        .ok_or("UserBase area missing")?;
    // Do not extend this to documents or arbitrary full-text schemas without
    // implementing their body frequencies and custom indexing rules.
    if area.name != "Пользователи"
        || !area
            .fields
            .get(&1)
            .is_some_and(|f| f.kind == 2 && matches!(f.name.as_str(), "UserID" | "Наименование"))
        || !area
            .fields
            .values()
            .any(|f| f.kind == 2 && f.name == "Пароль")
        || area.fields.values().any(|f| f.id != 1 && f.flags & 32 != 0)
        || record
            .fields
            .iter()
            .any(|f| !matches!(f.wire_type, 1 | 2 | 3 | 5 | 12 | 13))
    {
        return Err(
            "DB40 global word index write is only supported for scalar UserBase records".into(),
        );
    }
    Ok(record
        .fields
        .iter()
        .filter(|f| f.id == 1 && f.wire_type == 2)
        .filter_map(|f| {
            if let Value::String(s) = &f.value {
                Some(s)
            } else {
                None
            }
        })
        .flat_map(|s| words(s))
        .collect())
}
fn number(out: &mut Vec<u8>, mut n: u32) {
    let mut buf = [0u8; 5];
    let mut at = 4;
    buf[at] = (n & 127) as u8;
    while n >= 128 {
        n >>= 7;
        at -= 1;
        buf[at] = (n as u8 & 127) | 128;
    }
    out.extend_from_slice(&buf[at..]);
}
fn encode(word: &[u8], postings: &[(u32, u32)]) -> Vec<u8> {
    let uniform = postings.iter().all(|(_, weight)| *weight == 255);
    let mut out = vec![word.len() as u8 | if uniform { 128 } else { 0 }];
    out.extend(word);
    out.extend(postings[0].0.to_le_bytes());
    out.push(postings.len() as u8);
    for pair in postings.windows(2) {
        number(&mut out, pair[1].0 - pair[0].0);
    }
    if !uniform {
        for (_, weight) in postings {
            number(&mut out, *weight);
        }
    }
    out
}
pub(super) fn update(
    index: &mut MainKey,
    image: &mut Image,
    roots: &mut BTreeMap<(u16, u16), (u16, u32)>,
    schema: &Schema,
    before: &BTreeMap<u32, Record>,
    after: &BTreeMap<u32, Record>,
    affected: &BTreeSet<u32>,
) -> Result<(), String> {
    let old = before
        .iter()
        .map(|(&id, r)| Ok((id, title(r, schema)?)))
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    let new = after
        .iter()
        .map(|(&id, r)| Ok((id, title(r, schema)?)))
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    // Password, name and group changes do not alter login postings.
    if old == new {
        return Ok(());
    }
    let tree = index.writer_tree(65535, 2, None)?;
    let mut entries = BTreeMap::<Vec<u8>, BTreeMap<u32, u32>>::new();
    let mut observed = BTreeMap::<u32, BTreeSet<Vec<u8>>>::new();
    let mut previous = None;
    for item in &tree.items {
        let (word, first) = word_index::key(item)?;
        if previous
            .as_ref()
            .is_some_and(|key| key >= &(word.to_vec(), first))
        {
            return Err("unordered UserBase word index".into());
        }
        previous = Some((word.to_vec(), first));
        let postings = entries.entry(word.to_vec()).or_default();
        for (oid, weight) in word_index::postings(item)? {
            if affected.contains(&oid) {
                if weight != 0x600 {
                    return Err("unsupported UserBase word weighting".into());
                }
                observed.entry(oid).or_default().insert(word.to_vec());
            }
            if postings.insert(oid, weight).is_some() {
                return Err("overlapping UserBase word postings".into());
            }
        }
    }
    for &oid in affected {
        if old.get(&oid).cloned().unwrap_or_default() != observed.remove(&oid).unwrap_or_default() {
            return Err(
                "UserBase login word index differs from the supported native profile".into(),
            );
        }
    }
    for postings in entries.values_mut() {
        postings.retain(|oid, _| !affected.contains(oid));
    }
    for (oid, words) in new {
        for word in words {
            entries.entry(word).or_default().insert(oid, 0x600);
        }
    }
    let mut items = Vec::new();
    for (word, postings) in entries {
        let postings = postings.into_iter().collect::<Vec<_>>();
        for chunk in postings.chunks(250) {
            items.push(encode(&word, chunk));
        }
    }
    for block in tree.blocks {
        image.release_frame(block, b'L')?;
    }
    let generation = roots
        .get(&(65535, 2))
        .map_or(0, |r| r.0)
        .wrapping_add(affected.len() as u16);
    roots.insert(
        (65535, 2),
        (generation, image.tree(65535, 2, &items, true)?),
    );
    Ok(())
}

/// UserBase uses DB40 6.12: fixed link keys, no area-membership tree, and a
/// flat OID-presence stream. Keep its version and all unrelated streams intact.
pub(super) fn legacy_candidate(
    path: &Path,
    changes: &[Record],
    deletions: &[(u32, bool)],
    base: std::sync::Arc<Vec<u8>>,
) -> Result<Vec<u8>, String> {
    check_volume_budget(path)?;
    let schema = Schema::read(path)?;
    let mut index = MainKey::from_image(base.clone())?;
    let (header, directory) = index.writer_parts();
    let mut roots = BTreeMap::new();
    for d in directory.chunks_exact(10) {
        if roots
            .insert((u16at(d, 6), u16at(d, 8)), (u16at(d, 0), u32at(d, 2)))
            .is_some()
        {
            return Err("duplicate UserBase index".into());
        }
    }
    let mut affected = BTreeSet::new();
    let mut after = BTreeMap::new();
    for record in changes {
        title(record, &schema)?;
        let oid = record
            .integer(1024)
            .filter(|id| *id != 0)
            .ok_or("UserBase OID missing")?;
        if !affected.insert(oid) {
            return Err("duplicate UserBase change".into());
        }
        after.insert(oid, record.clone());
    }
    for &(oid, _) in deletions {
        if oid == 0 || !affected.insert(oid) {
            return Err("invalid UserBase deletion".into());
        }
    }
    let mut before = BTreeMap::new();
    for &oid in &affected {
        if let Some(entry) = index.lookup(oid)? {
            let record = index.record(entry)?;
            title(&record, &schema)?;
            if after.get(&oid).is_some_and(|r| r.area != record.area) {
                return Err("UserBase cannot change object area".into());
            }
            before.insert(oid, record);
        }
    }
    let mut image = Image::for_version(base.as_ref().clone(), 0x612)?;
    update(
        &mut index, &mut image, &mut roots, &schema, &before, &after, &affected,
    )?;
    let mut fields = BTreeSet::new();
    for &oid in &affected {
        let old = before.get(&oid);
        let new = after.get(&oid);
        let Some(record) = old.or(new) else {
            continue;
        };
        for f in schema.areas[&record.area]
            .fields
            .values()
            .filter(|f| f.flags & 1 != 0)
        {
            let values = |r: Option<&Record>| {
                r.map(|r| {
                    r.fields
                        .iter()
                        .filter(|v| v.id == f.id)
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
            };
            if values(old) != values(new) {
                fields.insert((record.area, f.id));
            }
        }
    }
    for (area, attr) in fields {
        let field = &schema.areas[&area].fields[&attr];
        let size = match field.kind {
            1 | 5 => Some(8),
            2 | 3 => None,
            _ => return Err("unsupported legacy UserBase field index".into()),
        };
        let tree = index.writer_tree(area, attr, size)?;
        let offset = if matches!(field.kind, 2 | 3) { 0 } else { 4 };
        if tree.items.iter().any(|item| item.len() < offset + 4) {
            return Err("truncated legacy UserBase field index key".into());
        }
        let mut items = tree
            .items
            .into_iter()
            .filter(|item| !affected.contains(&u32at(item, offset)))
            .collect::<Vec<_>>();
        if field.kind == 5 {
            for (&oid, record) in &after {
                if record.area != area {
                    continue;
                }
                for f in record
                    .fields
                    .iter()
                    .filter(|f| f.id == attr && f.wire_type == 5)
                {
                    let Value::Integer(value) = f.value else {
                        return Err("invalid UserBase link".into());
                    };
                    let mut key = value.to_le_bytes().to_vec();
                    key.extend(oid.to_le_bytes());
                    items.push(key);
                }
            }
            items.sort_by_key(|item| (u32at(item, 0), u32at(item, 4)));
            items.dedup();
        } else {
            items.extend(field_items(&after, area, field)?.0);
            items = sort_field_items(field.kind, items)?.0;
        }
        for block in tree.blocks {
            image.release_frame(block, b'L')?;
        }
        let generation = roots
            .get(&(area, attr))
            .map_or(0, |r| r.0)
            .wrapping_add(affected.len() as u16);
        roots.insert(
            (area, attr),
            (generation, image.tree(area, attr, &items, size.is_none())?),
        );
    }
    let tree = index.writer_tree(65535, 0, Some(10))?;
    let mut main = BTreeMap::new();
    for item in tree.items {
        let oid = u32at(&item, 0);
        if affected.contains(&oid) {
            if u32at(&item, 4) != u32::MAX {
                image.release_frame(u32at(&item, 4), b'C')?;
            }
        } else {
            main.insert(oid, item);
        }
    }
    for (&oid, record) in &after {
        let (block, crc) = image.frame(b'C', &encode_record(record)?)?;
        let mut item = oid.to_le_bytes().to_vec();
        item.extend(block.to_le_bytes());
        item.extend(crc.to_le_bytes());
        main.insert(oid, item);
    }
    for &(oid, hole) in deletions {
        if hole {
            let mut item = oid.to_le_bytes().to_vec();
            item.extend(u32::MAX.to_le_bytes());
            item.extend(0u16.to_le_bytes());
            main.insert(oid, item);
        }
    }
    for block in tree.blocks {
        image.release_frame(block, b'L')?;
    }
    let generation = roots
        .get(&(65535, 0))
        .ok_or("UserBase MainKey missing")?
        .0
        .wrapping_add(affected.len() as u16);
    roots.insert(
        (65535, 0),
        (
            generation,
            image.tree(65535, 0, &main.into_values().collect::<Vec<_>>(), false)?,
        ),
    );
    let bytes = index
        .writer_presence()?
        .ok_or("UserBase presence stream missing")?;
    if bytes.len() < 4 || u32at(&bytes, 0) as usize > (bytes.len() - 4) / 4 {
        return Err("invalid UserBase presence stream".into());
    }
    let ids = bytes[4..4 + u32at(&bytes, 0) as usize * 4]
        .chunks_exact(4)
        .map(|b| u32at(b, 0))
        .collect::<Vec<_>>();
    if ids.windows(2).any(|p| p[0] >= p[1]) {
        return Err("unordered UserBase presence stream".into());
    }
    let mut ids = ids.into_iter().collect::<BTreeSet<_>>();
    ids.extend(after.keys().copied());
    for (oid, _) in deletions {
        ids.remove(oid);
    }
    let mut presence = (ids.len() as u32).to_le_bytes().to_vec();
    for oid in ids {
        presence.extend(oid.to_le_bytes());
    }
    presence.resize((presence.len() + 7).div_ceil(128) * 128 - 7, 0);
    image.release_frame(u32at(&header, 43), b'D')?;
    let block = image.frame(b'D', &presence)?.0;
    image.bytes[43..47].copy_from_slice(&block.to_le_bytes());
    let mut directory = Vec::new();
    for ((area, attr), (generation, block)) in roots {
        directory.extend(generation.to_le_bytes());
        directory.extend(block.to_le_bytes());
        directory.extend(area.to_le_bytes());
        directory.extend(attr.to_le_bytes());
    }
    image.release_frame(u32at(&header, 15), b'K')?;
    let block = image.frame(b'K', &directory)?.0;
    image.bytes[15..19].copy_from_slice(&block.to_le_bytes());
    Ok(image.bytes)
}
