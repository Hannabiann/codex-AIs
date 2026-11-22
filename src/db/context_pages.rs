//! Page-sized materialization of the native P-area object cursor.
use super::*;
use crate::db::mainkey::name_cursor::NameCursor;
use std::{cmp::Reverse, collections::BinaryHeap};

pub(super) struct ObjectPages {
    estimated: usize,
    rows: Vec<u32>,
    streams: Vec<NameCursor>,
    heap: BinaryHeap<Reverse<(Vec<u8>, u32, usize)>>,
    finished: bool,
}
impl ObjectPages {
    fn new(streams: Vec<NameCursor>, estimated: usize) -> Result<Self, String> {
        let mut result = Self {
            estimated,
            rows: Vec::new(),
            streams,
            heap: BinaryHeap::new(),
            finished: false,
        };
        for (i, stream) in result.streams.iter_mut().enumerate() {
            if let Some((key, oid)) = stream.next()? {
                result.heap.push(Reverse((key, oid, i)));
            }
        }
        Ok(result)
    }
    fn page(&mut self, start: usize, size: usize) -> Result<Vec<u32>, String> {
        let end = start
            .checked_add(size)
            .ok_or("object page range overflow")?;
        while self.rows.len() < end && !self.finished {
            crate::execution::check()?;
            if let Some(Reverse((_, oid, i))) = self.heap.pop() {
                self.rows.push(oid);
                if let Some((key, oid)) = self.streams[i].next()? {
                    self.heap.push(Reverse((key, oid, i)));
                }
            } else {
                let mut missing = self
                    .streams
                    .iter()
                    .flat_map(NameCursor::missing)
                    .collect::<Vec<_>>();
                missing.sort_unstable();
                self.rows.extend(missing);
                self.streams.clear();
                self.finished = true;
            }
        }
        Ok(self.rows[start.min(self.rows.len())..end.min(self.rows.len())].to_vec())
    }
}
impl<E: Deref<Target = Db4Engine>> Contexts<E> {
    pub(super) fn object_page_impl(
        &self,
        scope: u32,
        sql: &str,
        start: usize,
        size: usize,
    ) -> Result<crate::ks::database::ObjectPage, String> {
        use crate::ks::{host::Column, sql::BoundPredicate};
        self.refresh_writes();
        let q = crate::ks::sql::parse(sql)?;
        let schema = self.shared_schema(scope)?;
        let area = schema.area(&q.area).ok_or("object page area missing")?;
        let name = area
            .fields
            .get(&1)
            .ok_or("object page name field missing")?;
        if q.projection.0 != [Column::Object]
            || q.order != [(name.name.clone(), false)]
            || q.predicate.has_context()
            || q.predicate.has_subquery()
            || name.kind != 2
            || self
                .mounts(scope)?
                .iter()
                .filter_map(|m| m.schema.areas.get(&area.id))
                .any(|a| {
                    !a.fields
                        .get(&1)
                        .is_some_and(|f| f.kind == 2 && f.flags & 1 != 0)
                })
        {
            let rows = self.query(scope, sql)?;
            let end = start.saturating_add(size).min(rows.len());
            return Ok(crate::ks::database::ObjectPage {
                area: area.id,
                estimated: rows.len(),
                ids: rows[start.min(rows.len())..end].to_vec(),
                start,
            });
        }
        let key = (scope, sql.to_owned());
        let mut cache = self.object_pages.borrow_mut();
        if !cache.contains_key(&key) {
            let predicate: BoundPredicate = q.predicate.bind(area)?;
            let mut candidates = Vec::new();
            let mut by_mount = Vec::new();
            for mount in self.mounts(scope)? {
                let ids = match self
                    .engine
                    .indexed_volume(&mount.key, area.id, &predicate)?
                {
                    Some(ids) => ids,
                    None => self
                        .extra
                        .indexed_volume(&mount.key, area.id, &predicate)?
                        .unwrap_or_default(),
                };
                candidates.extend_from_slice(&ids);
                by_mount.push(ids);
            }
            candidates.sort_unstable();
            candidates.dedup();
            let mut estimated = 0;
            let mut streams = Vec::new();
            for (mount, hits) in self.mounts(scope)?.iter().zip(by_mount).rev() {
                if candidates.is_empty() {
                    break;
                }
                let entries = match self.engine.lookup_entries(&mount.key, &candidates)? {
                    Some(e) => e,
                    None => self
                        .extra
                        .lookup_entries(&mount.key, &candidates)?
                        .ok_or("object page mount missing")?,
                };
                let mut matched = Vec::new();
                let mut h = 0;
                for entry in &entries {
                    while h < hits.len() && hits[h] < entry.oid {
                        h += 1;
                    }
                    if entry.block != u32::MAX && hits.get(h) == Some(&entry.oid) {
                        matched.push(entry.oid);
                    }
                }
                let mut e = 0;
                candidates.retain(|oid| {
                    while e < entries.len() && entries[e].oid < *oid {
                        e += 1;
                    }
                    entries.get(e).is_none_or(|entry| entry.oid != *oid)
                });
                estimated += matched.len();
                if !matched.is_empty() {
                    streams.push(NameCursor::new(
                        super::super::mainkey::MainKey::open(&super::super::script_volume_path(
                            &self.root, &mount.key,
                        ))?,
                        area.id,
                        matched,
                    )?);
                }
            }
            // Bound live cursors/identity arrays, not the number of SQL results.
            // Filter changes discard the previous page cache for this scope.
            cache.retain(|(s, _), _| *s != scope);
            cache.insert(key.clone(), ObjectPages::new(streams, estimated)?);
        }
        let cursor = cache.get_mut(&key).unwrap();
        let estimated = cursor.estimated;
        match cursor.page(start, size) {
            Ok(ids) => Ok(crate::ks::database::ObjectPage {
                area: area.id,
                estimated,
                ids,
                start,
            }),
            Err(error) => {
                cache.remove(&key);
                Err(error)
            }
        }
    }
}
