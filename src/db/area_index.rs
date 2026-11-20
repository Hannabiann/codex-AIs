//! DB40 0x614 compressed links: field indexes and K(0xffff,1) area membership.
//! 005f3cca / 005d1880 / 0081f6c0. No name-field or file-scan fallback.
use super::{u16at, u32at, MainKey};
use std::collections::HashSet;

type Key = (u32, u32); // key, first OID of a compressed segment
struct Segment<'a> {
    key: Key,
    bytes: &'a [u8],
    left: Option<u32>,
}
struct Node<'a> {
    segments: Vec<Segment<'a>>,
    right: Option<u32>,
}

fn node(
    bytes: &[u8],
    lower: Option<Key>,
    upper: Option<Key>,
    index: (u16, u16),
) -> Result<Node<'_>, String> {
    if bytes.len() < 7 || u16at(bytes, 0) != index.1 || u16at(bytes, 2) != index.0 {
        return Err("not a compressed link index node".into());
    }
    let count = u16at(bytes, 4) as usize;
    let branch = bytes[6] != 0;
    let base = 7 + count * 2;
    if base > bytes.len() {
        return Err("truncated compressed link index offset table".into());
    }
    let mut segments = Vec::with_capacity(count);
    let mut previous = 0;
    let mut previous_key = lower;
    for i in 0..count {
        let end = u16at(bytes, 7 + i * 2) as usize;
        let start = previous + if branch { 4 } else { 0 };
        if end < start + 9 || base + end > bytes.len() {
            return Err("invalid compressed link index entry bounds".into());
        }
        let key = (u32at(bytes, base + start), u32at(bytes, base + start + 4));
        if previous_key.is_some_and(|p| p >= key) || upper.is_some_and(|p| key >= p) {
            return Err("unordered compressed link index keys/subtrees".into());
        }
        let left = if branch {
            Some(child(bytes, base + previous)?)
        } else {
            None
        };
        segments.push(Segment {
            key,
            bytes: &bytes[base + start..base + end],
            left,
        });
        previous = end;
        previous_key = Some(key);
    }
    let right = if branch {
        Some(child(bytes, base + previous)?)
    } else {
        None
    };
    Ok(Node { segments, right })
}

fn child(bytes: &[u8], at: usize) -> Result<u32, String> {
    let value = bytes
        .get(at..at + 4)
        .ok_or("truncated compressed link index child")?;
    let id = u32at(value, 0);
    if id == 0 {
        return Err("null compressed link index child".into());
    }
    Ok(id)
}

fn delta(bytes: &[u8], at: &mut usize) -> Result<u32, String> {
    let mut n = 0u32;
    for _ in 0..5 {
        let b = *bytes.get(*at).ok_or("truncated area OID delta")?;
        *at += 1;
        n = n
            .checked_mul(128)
            .and_then(|n| n.checked_add((b & 127) as u32))
            .ok_or("area OID delta overflow")?;
        if b & 128 == 0 {
            return Ok(n);
        }
    }
    Err("overlong area OID delta".into())
}

fn append_segment(bytes: &[u8], out: &mut Vec<u32>, limit: usize) -> Result<(), String> {
    if bytes.len() < 9 {
        return Err("truncated area OID segment".into());
    }
    let count = bytes[8] as usize;
    if count == 0 {
        return Err("empty area OID segment".into());
    }
    if out
        .len()
        .checked_add(count)
        .filter(|n| *n <= limit)
        .is_none()
    {
        return Err(format!(
            "SQL row limit: compressed link index exceeds {limit}; result not truncated"
        ));
    }
    let mut oid = u32at(bytes, 4);
    let mut at = 9;
    for i in 0..count {
        if i > 0 {
            oid = oid
                .checked_add(delta(bytes, &mut at)?)
                .ok_or("area OID overflow")?;
        }
        if out.last().is_some_and(|p| *p >= oid) {
            return Err("unordered/overlapping area OID segments".into());
        }
        out.push(oid);
    }
    if at != bytes.len() {
        return Err("trailing bytes in area OID segment".into());
    }
    Ok(())
}

fn intersects(area: u32, lower: Option<Key>, upper: Option<Key>) -> bool {
    lower.map_or(true, |k| k.0 <= area) && upper.map_or(true, |k| area <= k.0)
}

struct Walk {
    index: (u16, u16),
    area: u32,
    limit: usize,
    initial_bytes: u64,
    seen: HashSet<u32>,
    oids: Vec<u32>,
}
impl Walk {
    fn visit(
        &mut self,
        tree: &mut MainKey,
        id: u32,
        depth: usize,
        lower: Option<Key>,
        upper: Option<Key>,
    ) -> Result<(), String> {
        if depth >= 32 || self.seen.len() >= 4096 || !self.seen.insert(id) {
            return Err("compressed link index depth/node budget/cycle".into());
        }
        let bytes = tree.block(id, b'L', 1024 * 1024)?;
        if tree.bytes - self.initial_bytes > 64 * 1024 * 1024 {
            return Err("compressed link index byte budget exceeded".into());
        }
        let node = node(&bytes, lower, upper, self.index)?;
        let mut previous = lower;
        for entry in node.segments {
            if let Some(child) = entry.left {
                if intersects(self.area, previous, Some(entry.key)) {
                    self.visit(tree, child, depth + 1, previous, Some(entry.key))?;
                }
            }
            if entry.key.0 == self.area {
                append_segment(entry.bytes, &mut self.oids, self.limit)?;
            }
            previous = Some(entry.key);
        }
        if let Some(child) = node.right {
            if intersects(self.area, previous, upper) {
                self.visit(tree, child, depth + 1, previous, upper)?;
            }
        }
        Ok(())
    }
}

impl MainKey {
    pub fn area_candidates(&mut self, area: u16, limit: usize) -> Result<Vec<u32>, String> {
        let h = self.header;
        if matches!(u16at(&h, 5), 0x611 | 0x612) {
            // Native 005f3c8e skips the area-membership index before 0x614.
            // Use the bounded MainKey tree and C-record prefixes; never scan bytes.
            let mut result = Vec::new();
            for entry in self.entries(limit)? {
                if entry.block == u32::MAX {
                    continue;
                }
                let at = self.record_offset(entry)?;
                let prefix = self.read(at, 13)?;
                if u32at(&prefix, 0) < 6 || !(1..=4096).contains(&u16at(&prefix, 9)) {
                    return Err("invalid legacy C-record prefix".into());
                }
                if u16at(&prefix, 7) == area {
                    result.push(entry.oid);
                }
            }
            return Ok(result);
        }
        self.compressed_candidates((0xffff, 1), area as u32, limit)
    }

    /// Native type 5 factory 006f8200, registered at 008dda30.
    pub fn link_candidates(
        &mut self,
        area: u16,
        attribute: u16,
        oid: u32,
        limit: usize,
    ) -> Result<Vec<u32>, String> {
        if matches!(u16at(&self.header, 5), 0x611 | 0x612) {
            let mut result = Vec::new();
            for member in self.area_candidates(area, limit)? {
                let entry = self.lookup(member)?.ok_or("legacy area member missing")?;
                let (_, links) = self.record_links(entry)?;
                if links.contains(&(attribute, oid)) {
                    result.push(member);
                }
            }
            return Ok(result);
        }
        self.compressed_candidates((area, attribute), oid, limit)
    }

    fn compressed_candidates(
        &mut self,
        index: (u16, u16),
        key: u32,
        limit: usize,
    ) -> Result<Vec<u32>, String> {
        let h = self.header;
        let version = u16at(&h, 5);
        if version != 0x614 {
            return Err(format!(
                "compressed link index for DB40 version {version:#x} is not implemented"
            ));
        }
        let roots = self.directory.clone();
        if roots.len() % 10 != 0 {
            return Err("invalid index directory size".into());
        }
        let mut root = None;
        for c in roots.chunks_exact(10) {
            if u16at(c, 6) == index.0 && u16at(c, 8) == index.1 {
                if root.replace(u32at(c, 2)).is_some() {
                    return Err("duplicate compressed link index root".into());
                }
            }
        }
        let root = root.ok_or("compressed link index root missing")?;
        let mut walk = Walk {
            index,
            area: key,
            limit,
            initial_bytes: self.bytes,
            seen: HashSet::new(),
            oids: Vec::new(),
        };
        if root != 0 {
            walk.visit(self, root, 0, None, None)?;
        }
        Ok(walk.oids)
    }

    pub fn validate_area_member(&mut self, oid: u32, area: u16) -> Result<(), String> {
        let entry = self
            .lookup(oid)?
            .ok_or_else(|| format!("compressed link index points to absent/deleted OID {oid}"))?;
        let at = self.record_offset(entry)?;
        let h = self.read(at, 13)?;
        if u32at(&h, 0) < 6 || !(1..=4096).contains(&u16at(&h, 9)) {
            return Err(format!("invalid C-record prefix for OID {oid}"));
        }
        if u16at(&h, 7) != area {
            return Err(format!(
                "compressed link index mismatch for OID {oid}: expected {area}, got {}",
                u16at(&h, 7)
            ));
        }
        Ok(())
    }
}
