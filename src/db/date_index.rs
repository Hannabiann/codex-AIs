//! DB40 fixed-size date keys: local-calendar date (4 bytes), OID (4 bytes).
//! Branch entries have a preceding child pointer and a final right child.
use super::{u16at, u32at, MainKey};
use crate::ks::sql::Comparison;
use std::collections::HashSet;
type Key = (u32, u32);
pub(super) fn intersect_filters<T: Copy>(
    filters: &[(Comparison, T)],
    mut lookup: impl FnMut((Comparison, T)) -> Result<Vec<u32>, String>,
) -> Result<Vec<u32>, String> {
    let mut filters = filters.to_vec();
    filters.sort_by_key(|(op, _)| match op {
        Comparison::Eq => 0,
        Comparison::Ge | Comparison::Gt => 1,
        _ => 2,
    });
    let mut selected: Option<Vec<u32>> = None;
    for filter in filters {
        let ids = lookup(filter)?;
        if let Some(previous) = &mut selected {
            crate::db::indexed_query::intersect(previous, &ids);
        } else {
            selected = Some(ids);
        }
        if selected.as_ref().is_some_and(Vec::is_empty) {
            break;
        }
    }
    Ok(selected.unwrap_or_default())
}
pub fn calendar(bytes: &[u8]) -> u32 {
    u16at(bytes, 0) as u32 * 10000 + bytes[2] as u32 * 100 + bytes[3] as u32
}

fn intersects(low: Option<Key>, high: Option<Key>, filters: &[(Comparison, u32)]) -> bool {
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
    pub fn date_candidates(
        &mut self,
        area: u16,
        attr: u16,
        filters: &[(Comparison, u32)],
    ) -> Result<Vec<u32>, String> {
        // A record can contain several dates. SQL AND intersects record IDs;
        // its two comparisons need not match the same field occurrence.
        if filters.len() > 1 {
            return intersect_filters(filters, |filter| {
                self.date_candidates(area, attr, &[filter])
            });
        }
        self.date_range_candidates(area, attr, filters)
    }
    /// BETWEEN applies both bounds to one stored occurrence; separate SQL
    /// comparisons instead intersect the record sets in date_candidates.
    pub fn date_range_candidates(
        &mut self,
        area: u16,
        attr: u16,
        filters: &[(Comparison, u32)],
    ) -> Result<Vec<u32>, String> {
        let roots = self.directory.clone();
        let mut root = None;
        for c in roots.chunks_exact(10) {
            if u16at(c, 6) == area && u16at(c, 8) == attr {
                if root.replace(u32at(c, 2)).is_some() {
                    return Err("duplicate date index".into());
                }
            }
        }
        let root = root.ok_or("date index missing")?;
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let start = self.bytes;
        if root != 0 {
            self.walk_dates(
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
    fn walk_dates(
        &mut self,
        id: u32,
        index: (u16, u16),
        filters: &[(Comparison, u32)],
        low: Option<Key>,
        high: Option<Key>,
        depth: usize,
        start: u64,
        seen: &mut HashSet<u32>,
        out: &mut Vec<u32>,
    ) -> Result<(), String> {
        if id == 0 || depth >= 32 || seen.len() >= 32768 || !seen.insert(id) {
            return Err("date index child/cycle/node budget".into());
        }
        let b = self.block(id, b'L', 1024 * 1024)?;
        if self.bytes - start > 64 * 1024 * 1024 {
            return Err("date index byte budget".into());
        }
        if b.len() < 7 || u16at(&b, 0) != index.1 || u16at(&b, 2) != index.0 {
            return Err("date index node mismatch".into());
        }
        let count = u16at(&b, 4) as usize;
        let branch = b[6] != 0;
        let step = if branch { 12 } else { 8 };
        if 7 + count * step + if branch { 4 } else { 0 } > b.len() {
            return Err("truncated fixed date index".into());
        }
        let mut previous = low;
        for i in 0..count {
            let at = 7 + i * step;
            let key_at = at + if branch { 4 } else { 0 };
            let key = (calendar(&b[key_at..key_at + 4]), u32at(&b, key_at + 4));
            if previous.is_some_and(|p| p >= key) || high.is_some_and(|p| key >= p) {
                return Err("unordered date index keys".into());
            }
            if branch && intersects(previous, Some(key), filters) {
                self.walk_dates(
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
                    return Err("date index match limit (8000000)".into());
                }
                out.push(key.1);
            }
            previous = Some(key);
        }
        if branch && intersects(previous, high, filters) {
            self.walk_dates(
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
