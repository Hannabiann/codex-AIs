//! DB40 Timestamp keys: OID (4 LE bytes), calendar/time (8 BE bytes).
//! Verified against workbase/base.db4, work.db4 and old/base00001.db4.
//! Branch entries have a preceding child pointer and a final right child.
use super::{u16at, u32at, MainKey};
use crate::ks::sql::Comparison;
use std::collections::HashSet;
type Key = (u64, u32);
pub fn calendar(bytes: &[u8]) -> u64 {
    let mut n = u16::from_be_bytes([bytes[0], bytes[1]]) as u64;
    for &byte in &bytes[2..7] {
        n = n * 100 + byte as u64;
    }
    n * 1000 + bytes[7] as u64 * 10
}
/// Bound SQL timestamps are normalized to YYYY-MM-DD HH:MM:SS.mmm.
pub fn bound(value: &str) -> Result<u64, String> {
    if value.len() != 23
        || !value.bytes().enumerate().all(|(i, b)| match i {
            4 | 7 => b == b'-',
            10 => b == b' ',
            13 | 16 => b == b':',
            19 => b == b'.',
            _ => b.is_ascii_digit(),
        })
    {
        return Err("invalid bound timestamp".into());
    }
    value
        .bytes()
        .filter(u8::is_ascii_digit)
        .try_fold(0u64, |v, b| {
            v.checked_mul(10)
                .and_then(|v| v.checked_add((b - b'0') as u64))
        })
        .ok_or_else(|| "timestamp overflow".into())
}
fn intersects(low: Option<Key>, high: Option<Key>, filters: &[(Comparison, u64)]) -> bool {
    filters.iter().all(|(op, value)| match op {
        Comparison::Eq => {
            low.map_or(true, |k| k.0 <= *value) && high.map_or(true, |k| k.0 >= *value)
        }
        Comparison::Lt => low.map_or(true, |k| k.0 < *value),
        Comparison::Le => low.map_or(true, |k| k.0 <= *value),
        Comparison::Gt => high.map_or(true, |k| k.0 > *value),
        Comparison::Ge => high.map_or(true, |k| k.0 >= *value),
        Comparison::Ne => true,
    })
}
impl MainKey {
    pub fn timestamp_candidates(
        &mut self,
        area: u16,
        attr: u16,
        filters: &[(Comparison, u64)],
    ) -> Result<Vec<u32>, String> {
        if filters.len() > 1 {
            return super::date_index::intersect_filters(filters, |filter| {
                self.timestamp_candidates(area, attr, &[filter])
            });
        }
        self.timestamp_range_candidates(area, attr, filters)
    }
    pub fn timestamp_range_candidates(
        &mut self,
        area: u16,
        attr: u16,
        filters: &[(Comparison, u64)],
    ) -> Result<Vec<u32>, String> {
        let roots = self.directory.clone();
        let mut root = None;
        for c in roots.chunks_exact(10) {
            if u16at(c, 6) == area && u16at(c, 8) == attr {
                if root.replace(u32at(c, 2)).is_some() {
                    return Err("duplicate timestamp index".into());
                }
            }
        }
        let root = root.ok_or("timestamp index missing")?;
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let start = self.bytes;
        if root != 0 {
            self.walk_timestamps(
                root,
                (area, attr),
                filters,
                None,
                None,
                0,
                start,
                &mut seen,
                &mut out,
            )?;
        }
        out.sort_unstable();
        out.dedup();
        Ok(out)
    }
    fn walk_timestamps(
        &mut self,
        id: u32,
        index: (u16, u16),
        filters: &[(Comparison, u64)],
        low: Option<Key>,
        high: Option<Key>,
        depth: usize,
        start: u64,
        seen: &mut HashSet<u32>,
        out: &mut Vec<u32>,
    ) -> Result<(), String> {
        if id == 0 || depth >= 32 || seen.len() >= 32768 || !seen.insert(id) {
            return Err("timestamp index child/cycle/node budget".into());
        }
        let b = self.block(id, b'L', 1024 * 1024)?;
        if self.bytes - start > 64 * 1024 * 1024 {
            return Err("timestamp index byte budget".into());
        }
        if b.len() < 7 || u16at(&b, 0) != index.1 || u16at(&b, 2) != index.0 {
            return Err("timestamp index node mismatch".into());
        }
        let count = u16at(&b, 4) as usize;
        let branch = b[6] != 0;
        let step = if branch { 16 } else { 12 };
        if 7 + count * step + if branch { 4 } else { 0 } > b.len() {
            return Err("truncated fixed timestamp index".into());
        }
        let mut previous = low;
        for i in 0..count {
            let at = 7 + i * step;
            let key_at = at + if branch { 4 } else { 0 };
            let key = (calendar(&b[key_at + 4..key_at + 12]), u32at(&b, key_at));
            if previous.is_some_and(|p| p >= key) || high.is_some_and(|p| key >= p) {
                return Err("unordered timestamp index keys".into());
            }
            if branch && intersects(previous, Some(key), filters) {
                self.walk_timestamps(
                    u32at(&b, at),
                    index,
                    filters,
                    previous,
                    Some(key),
                    depth + 1,
                    start,
                    seen,
                    out,
                )?;
            }
            if filters.iter().all(|(op, v)| op.accepts(key.0.cmp(v))) {
                if out.len() >= crate::ks::sql::MAX_INDEX_CANDIDATES {
                    return Err("timestamp index match limit (8000000)".into());
                }
                out.push(key.1);
            }
            previous = Some(key);
        }
        if branch && intersects(previous, high, filters) {
            self.walk_timestamps(
                u32at(&b, 7 + count * step),
                index,
                filters,
                previous,
                high,
                depth + 1,
                start,
                seen,
                out,
            )?;
        }
        Ok(())
    }
}
