//! Set operations on native attribute indexes. Document bodies are only read
//! for predicates whose field has no native index, or for small residual sets.
use super::{
    schema::Area,
    shared_query::{path_key, BuildKey},
    Db4Engine,
};
use crate::ks::sql::{BoundPredicate as P, MAX_INDEX_CANDIDATES};
use std::{path::PathBuf, sync::Arc};

type RangeKey = (PathBuf, u64, u16, String);
#[derive(Debug, Default)]
pub(super) struct ReverseLinks {
    entries:
        std::collections::BTreeMap<(PathBuf, u16), (u64, std::sync::Arc<Vec<(u16, u32, u32)>>)>,
    count: usize,
    targets: std::collections::BTreeMap<
        (PathBuf, u16, u32),
        (
            u64,
            std::sync::Arc<std::collections::BTreeMap<u16, Vec<u32>>>,
            usize,
        ),
    >,
    target_count: usize,
    fields: std::collections::BTreeMap<(PathBuf, u16, u16), (u64, std::sync::Arc<Vec<(u32, u32)>>)>,
    field_count: usize,
}
enum ReverseHit {
    Target(Arc<std::collections::BTreeMap<u16, Vec<u32>>>),
    Field(Arc<Vec<(u32, u32)>>),
    Full(Arc<Vec<(u16, u32, u32)>>),
}
impl ReverseHit {
    fn ids(self, attribute: u16, target: u32) -> Vec<u32> {
        match self {
            Self::Target(items) => items.get(&attribute).cloned().unwrap_or_default(),
            Self::Field(items) => {
                let start = items.partition_point(|&(value, _)| value < target);
                items[start..]
                    .iter()
                    .take_while(|&&(value, _)| value == target)
                    .map(|&(_, oid)| oid)
                    .collect()
            }
            Self::Full(items) => {
                let start = items
                    .partition_point(|&(field, value, _)| (field, value) < (attribute, target));
                items[start..]
                    .iter()
                    .take_while(|&&(field, value, _)| (field, value) == (attribute, target))
                    .map(|&(_, _, oid)| oid)
                    .collect()
            }
        }
    }
}
impl ReverseLinks {
    fn get(
        &self,
        path: &PathBuf,
        area: u16,
        attribute: u16,
        target: u32,
        revision: u64,
    ) -> Option<ReverseHit> {
        if let Some((r, items, _)) = self.targets.get(&(path.clone(), area, target)) {
            if *r == revision {
                return Some(ReverseHit::Target(items.clone()));
            }
        }
        if let Some((r, items)) = self.fields.get(&(path.clone(), area, attribute)) {
            if *r == revision {
                return Some(ReverseHit::Field(items.clone()));
            }
        }
        self.entries
            .get(&(path.clone(), area))
            .filter(|(r, _)| *r == revision)
            .map(|(_, items)| ReverseHit::Full(items.clone()))
    }
}
/// Complete results for immutable volume indexes, never a restricted subset.
/// Reports reuse historical date ranges for each judge. Evict the least recently
/// used range instead of discarding every hot range when one new range arrives.
#[derive(Debug, Default)]
pub(super) struct RangeCache {
    entries: std::collections::BTreeMap<RangeKey, (Arc<Vec<u32>>, u64)>,
    ids: usize,
    clock: u64,
}
impl RangeCache {
    fn get(&mut self, key: &RangeKey) -> Option<Arc<Vec<u32>>> {
        let entry = self.entries.get_mut(key)?;
        self.clock += 1;
        entry.1 = self.clock;
        Some(entry.0.clone())
    }
    fn contains_key(&self, key: &RangeKey) -> bool {
        self.entries.contains_key(key)
    }
    fn insert(&mut self, key: RangeKey, ids: Vec<u32>) {
        self.insert_bounded(key, ids, 2048, 16_000_000);
    }
    fn insert_bounded(&mut self, key: RangeKey, ids: Vec<u32>, max_entries: usize, max_ids: usize) {
        if ids.len() > max_ids || max_entries == 0 {
            return;
        }
        if let Some(old) = self.entries.remove(&key) {
            self.ids -= old.0.len();
        }
        while self.entries.len() >= max_entries || self.ids + ids.len() > max_ids {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.1)
                .map(|(key, _)| key.clone())
                .unwrap();
            self.ids -= self.entries.remove(&oldest).unwrap().0.len();
        }
        self.clock += 1;
        self.ids += ids.len();
        self.entries.insert(key, (Arc::new(ids), self.clock));
    }
}

pub(super) fn intersect(a: &mut Vec<u32>, b: &[u32]) {
    let mut j = 0;
    a.retain(|id| {
        while j < b.len() && b[j] < *id {
            j += 1;
        }
        j < b.len() && b[j] == *id
    });
}
fn subtract(a: &mut Vec<u32>, b: &[u32]) {
    let mut j = 0;
    a.retain(|id| {
        while j < b.len() && b[j] < *id {
            j += 1;
        }
        j == b.len() || b[j] != *id
    });
}
// Schemas merged for SQL binding can advertise indexes absent from older
// volumes. An unindexed date must never outrank an available link index.
fn volume_cost(p: &P, area: &Area) -> u32 {
    match p {
        P::And(a, b) => volume_cost(a, area).min(volume_cost(b, area)),
        P::Or(a, b) => volume_cost(a, area).saturating_add(volume_cost(b, area)),
        P::Link { attribute, .. }
        | P::Text { attribute, .. }
        | P::Compare { attribute, .. }
        | P::Between { attribute, .. }
        | P::Set {
            attribute: Some(attribute),
            ..
        } => match area.fields.get(attribute) {
            None => 0,
            Some(f) if f.flags & 1 != 0 && matches!(f.kind, 1 | 2 | 3 | 5 | 12) => p.index_cost(),
            _ => 100000,
        },
        _ => p.index_cost(),
    }
}
fn needs_native_text_index(p: &P, area: &Area) -> bool {
    match p {
        P::Text {
            attribute, pattern, ..
        } => {
            pattern.requires_index_semantics()
                && area.fields.get(attribute).is_some_and(|f| f.flags & 1 != 0)
        }
        P::And(a, b) | P::Or(a, b) => {
            needs_native_text_index(a, area) || needs_native_text_index(b, area)
        }
        P::Not(inner) => needs_native_text_index(inner, area),
        _ => false,
    }
}
impl Db4Engine {
    fn query_generation(&self, v: usize) -> Result<u64, String> {
        let path = &self.volumes[v].path;
        super::writer::ensure_recovered(path)?;
        let _publication = super::writer::open_guard()?;
        Ok(super::writer::generation(path))
    }

    fn reverse_cached(
        &self,
        v: usize,
        area: u16,
        attribute: u16,
        target: u32,
        revision: u64,
    ) -> Result<Option<Vec<u32>>, String> {
        let path = path_key(&self.volumes[v].path);
        let hit = self
            .queries
            .reverse_links
            .lock()
            .map_err(|_| "reverse link cache poisoned")?
            .get(&path, area, attribute, target, revision);
        Ok(hit.map(|hit| hit.ids(attribute, target)))
    }

    fn reverse_link_matches(
        &self,
        v: usize,
        area: &Area,
        attribute: u16,
        target: u32,
    ) -> Result<Option<Vec<u32>>, String> {
        use std::collections::{BTreeMap, BTreeSet};
        const MAX: usize = 8_000_000;
        const PER_VOLUME: usize = 1_000_000;
        let path = path_key(&self.volumes[v].path);
        loop {
            crate::execution::check()?;
            let revision = self.query_generation(v)?;
            if let Some(ids) = self.reverse_cached(v, area.id, attribute, target, revision)? {
                return Ok(Some(ids));
            }
            let _build = self
                .queries
                .build(BuildKey::Reverse(path.clone(), area.id, revision))?;
            // A waiter may wake after publication or after another builder has
            // completed the index. Never build a new version under an old gate.
            if self.query_generation(v)? != revision {
                continue;
            }
            if let Some(ids) = self.reverse_cached(v, area.id, attribute, target, revision)? {
                return Ok(Some(ids));
            }
            let started = std::time::Instant::now();
            let fields = area
                .fields
                .values()
                .filter(|f| f.kind == 5 && f.flags & 1 == 0)
                .map(|f| f.id)
                .collect::<BTreeSet<_>>();
            let mut matched = BTreeMap::<u16, Vec<u32>>::new();
            let mut full = Some(Vec::new());
            let mut field_index = Some(Vec::new());
            let mut match_count = 0usize;
            // Pin one immutable file handle for the whole scan. Another worker
            // may publish a new DB4 image between batches without invalidating it.
            let examined = self.with_reader(v, |tree| {
                let ids = tree.area_candidates(area.id, MAX_INDEX_CANDIDATES)?;
                for batch in ids.chunks(1024) {
                    for entry in tree.lookup_many(batch)? {
                        if entry.block == u32::MAX {
                            continue;
                        }
                        let (record_area, links) = tree.record_links(entry)?;
                        if record_area != area.id {
                            return Err("reverse link area mismatch".into());
                        }
                        for (field, value) in
                            links.into_iter().filter(|(id, _)| fields.contains(id))
                        {
                            if field == attribute {
                                if field_index
                                    .as_ref()
                                    .is_some_and(|items| items.len() >= PER_VOLUME)
                                {
                                    field_index = None;
                                }
                                if let Some(items) = &mut field_index {
                                    items.push((value, entry.oid));
                                }
                            }
                            if value == target {
                                match_count += 1;
                                if match_count > MAX_INDEX_CANDIDATES {
                                    return Err("reverse link match budget".into());
                                }
                                matched.entry(field).or_default().push(entry.oid);
                            }
                            if full.as_ref().is_some_and(|items| items.len() >= PER_VOLUME) {
                                full = None;
                            }
                            if let Some(items) = &mut full {
                                items.push((field, value, entry.oid));
                            }
                        }
                    }
                }
                Ok(ids.len())
            })?;

            // Sorting and I/O never hold the shared cache/publication locks.
            let full = full.map(|mut items| {
                items.sort_unstable();
                items.dedup();
                Arc::new(items)
            });
            let field_index = field_index.map(|mut items| {
                items.sort_unstable();
                items.dedup();
                Arc::new(items)
            });
            for ids in matched.values_mut() {
                ids.sort_unstable();
                ids.dedup();
            }
            let result = matched.get(&attribute).cloned().unwrap_or_default();
            let count = matched.values().map(Vec::len).sum::<usize>();
            let saved_full = full.is_some();
            super::writer::ensure_recovered(&self.volumes[v].path)?;
            {
                let _publication = super::writer::open_guard()?;
                if revision == super::writer::generation(&self.volumes[v].path) {
                    let mut cache = self
                        .queries
                        .reverse_links
                        .lock()
                        .map_err(|_| "reverse link cache poisoned")?;
                    if let Some(items) = field_index {
                        let key = (path.clone(), area.id, attribute);
                        if let Some((_, old)) = cache.fields.remove(&key) {
                            cache.field_count -= old.len();
                        }
                        while cache.field_count + items.len() > 4_000_000
                            || cache.fields.len() >= 4096
                        {
                            let Some((_, (_, old))) = cache.fields.pop_first() else {
                                break;
                            };
                            cache.field_count -= old.len();
                        }
                        cache.field_count += items.len();
                        cache.fields.insert(key, (revision, items));
                    }
                    if let Some(items) = full {
                        let key = (path.clone(), area.id);
                        if let Some((_, old)) = cache.entries.remove(&key) {
                            cache.count -= old.len();
                        }
                        while cache.count + items.len() > MAX || cache.entries.len() >= 4096 {
                            let Some((_, (_, old))) = cache.entries.pop_first() else {
                                break;
                            };
                            cache.count -= old.len();
                        }
                        cache.count += items.len();
                        cache.entries.insert(key, (revision, items));
                    }
                    // When the complete index is too dense, a target's other
                    // unindexed fields are still complete and reusable.
                    if count <= 2_000_000 {
                        let key = (path.clone(), area.id, target);
                        if let Some((_, _, old)) = cache.targets.remove(&key) {
                            cache.target_count -= old;
                        }
                        while cache.targets.len() >= 16384 || cache.target_count + count > 2_000_000
                        {
                            let Some((_, (_, _, old))) = cache.targets.pop_first() else {
                                break;
                            };
                            cache.target_count -= old;
                        }
                        cache.target_count += count;
                        cache
                            .targets
                            .insert(key, (revision, Arc::new(matched), count));
                    }
                }
            }
            if started.elapsed().as_millis() > 200
                && std::env::var_os("KODEKS_PROFILE").is_some_and(|v| v == "1")
            {
                eprintln!("DB shared reverse link scan: volume={}, area={}, examined={}, matches={}, full_cache={}, elapsed_ms={}", self.volumes[v].name,area.id,examined,match_count,saved_full,started.elapsed().as_millis());
            }
            return Ok(Some(result));
        }
    }
    // A residual scan already has its complete candidate list. Resolve a batch
    // through MainKey once, rather than repeating the root-to-leaf lookup and
    // file metadata check for every document. Bodies still use the strict full
    // decoder; missing keys and tombstones have the same read_oid semantics.
    fn residual_matches(
        &self,
        v: usize,
        area: &Area,
        predicate: &P,
        mut candidates: Vec<u32>,
    ) -> Result<Vec<u32>, String> {
        candidates.sort_unstable();
        candidates.dedup();
        let mut result = Vec::new();
        for batch in candidates.chunks(1024) {
            self.with_reader(v, |tree| {
                for entry in tree.lookup_many(batch)? {
                    if entry.block == u32::MAX {
                        continue;
                    }
                    let record = tree.record(entry)?;
                    if record.area == area.id && predicate.matches_record(entry.oid, &record) {
                        result.push(entry.oid);
                    }
                }
                Ok(())
            })?;
        }
        Ok(result)
    }
    fn range_cached(&self, v: usize, area: u16, predicate: String) -> Result<bool, String> {
        let key = (
            path_key(&self.volumes[v].path),
            self.query_generation(v)?,
            area,
            predicate,
        );
        Ok(self
            .queries
            .ranges
            .lock()
            .map_err(|_| "range cache poisoned")?
            .contains_key(&key))
    }
    fn cached_index_bounds(
        &self,
        key: (usize, u16, String),
        compute: impl FnOnce() -> Result<Vec<u32>, String>,
    ) -> Result<Vec<u32>, String> {
        let (v, area, predicate) = key;
        let path = path_key(&self.volumes[v].path);
        loop {
            crate::execution::check()?;
            let revision = self.query_generation(v)?;
            let key = (path.clone(), revision, area, predicate.clone());
            let cached = self
                .queries
                .ranges
                .lock()
                .map_err(|_| "range cache poisoned")?
                .get(&key);
            if let Some(ids) = cached {
                return Ok((*ids).clone());
            }
            let _build = self.queries.build(BuildKey::Range(
                path.clone(),
                area,
                revision,
                predicate.clone(),
            ))?;
            if self.query_generation(v)? != revision {
                continue;
            }
            let cached = self
                .queries
                .ranges
                .lock()
                .map_err(|_| "range cache poisoned")?
                .get(&key);
            if let Some(ids) = cached {
                return Ok((*ids).clone());
            }
            let ids = compute()?;
            super::writer::ensure_recovered(&self.volumes[v].path)?;
            let _publication = super::writer::open_guard()?;
            if revision == super::writer::generation(&self.volumes[v].path) {
                self.queries
                    .ranges
                    .lock()
                    .map_err(|_| "range cache poisoned")?
                    .insert(key, ids.clone());
            }
            return Ok(ids);
        }
    }
    fn unindexed_range(&self, v: usize, area: &Area, predicate: &P) -> Result<Vec<u32>, String> {
        self.cached_index_bounds((v, area.id, format!("{predicate:?}")), || {
            let started = std::time::Instant::now();
            let candidates = self.with_reader(v, |tree| tree.area_candidates(area.id, MAX_INDEX_CANDIDATES))?;
            let examined = candidates.len();
            let ids = self.residual_matches(v, area, predicate, candidates)?;
            if started.elapsed().as_millis() > 200 && std::env::var_os("KODEKS_PROFILE").is_some_and(|v| v == "1") {
                eprintln!("DB shared unindexed date scan: volume={}, area={}, examined={examined}, matched={}, elapsed_ms={}, predicate={predicate:?}", self.volumes[v].name, area.id, ids.len(), started.elapsed().as_millis());
            }
            Ok(ids)
        })
    }

    pub(super) fn indexed_volume(
        &self,
        name: &str,
        area: u16,
        p: &P,
    ) -> Result<Option<Vec<u32>>, String> {
        self.refresh_writes()?;
        let Some(v) = self.volume_id(name)? else {
            return Ok(None);
        };
        let schema = self.cached_schema(v)?;
        let Some(area) = schema.areas.get(&area) else {
            return Ok(Some(vec![]));
        };
        let tracing = std::env::var_os("KODEKS_TRACE_SQL").is_some_and(|v| v == "1");
        let started = std::time::Instant::now();
        if tracing {
            eprintln!(
                "DB index start: volume={name}, area={}, predicate={p:?}",
                area.id
            );
        }
        let result = self.indexed_set(v, area, p, None);
        if tracing {
            eprintln!(
                "DB index finish: volume={name}, elapsed_ms={}, rows={:?}",
                started.elapsed().as_millis(),
                result.as_ref().map(Vec::len)
            );
        }
        result.map(Some)
    }
    fn indexed_set(
        &self,
        v: usize,
        area: &Area,
        p: &P,
        restriction: Option<&[u32]>,
    ) -> Result<Vec<u32>, String> {
        if !std::env::var_os("KODEKS_TRACE_SQL_STEPS").is_some_and(|v| v == "1") {
            return self.indexed_set_impl(v, area, p, restriction);
        }
        let started = std::time::Instant::now();
        let result = self.indexed_set_impl(v, area, p, restriction);
        eprintln!(
            "DB index step: volume={}, area={}, restricted={:?}, rows={:?}, elapsed_ms={}, predicate={p:?}",
            self.volumes[v].name,
            area.id,
            restriction.map(<[u32]>::len),
            result.as_ref().map(Vec::len),
            started.elapsed().as_millis()
        );
        result
    }
    fn indexed_set_impl(
        &self,
        v: usize,
        area: &Area,
        p: &P,
        restriction: Option<&[u32]>,
    ) -> Result<Vec<u32>, String> {
        if restriction.is_some_and(|ids| ids.is_empty()) {
            return Ok(vec![]);
        }
        let universe = || -> Result<Vec<u32>, String> {
            if let Some(ids) = restriction {
                return Ok(ids.to_vec());
            }
            self.with_reader(v, |tree| {
                tree.area_candidates(area.id, MAX_INDEX_CANDIDATES)
            })
        };
        let residual = |ids| self.residual_matches(v, area, p, ids);
        let mut ids = match p {
            P::None => vec![],
            P::All => universe()?,
            P::Oid(oid) => vec![*oid],
            P::Set {
                attribute: None,
                ids,
            } => ids.as_ref().clone(),
            P::And(_, _) => {
                fn flatten<'a>(p: &'a P, out: &mut Vec<&'a P>) {
                    if let P::And(a, b) = p {
                        flatten(a, out);
                        flatten(b, out);
                    } else {
                        out.push(p);
                    }
                }
                let mut parts = Vec::new();
                flatten(p, &mut parts);
                let mut dates = std::collections::BTreeMap::<u16, Vec<_>>::new();
                let mut timestamps = std::collections::BTreeMap::<u16, Vec<_>>::new();
                for part in &parts {
                    if let P::Compare {
                        attribute,
                        kind: 12,
                        op,
                        value,
                    } = part
                    {
                        if area.fields.get(attribute).is_some_and(|f| f.flags & 1 != 0) {
                            timestamps
                                .entry(*attribute)
                                .or_default()
                                .push((*op, super::mainkey::timestamp_index::bound(value)?));
                        }
                    }
                    if let P::Compare {
                        attribute,
                        kind: 1,
                        op,
                        value,
                    } = part
                    {
                        if area.fields.get(attribute).is_some_and(|f| f.flags & 1 != 0) {
                            dates.entry(*attribute).or_default().push((
                                *op,
                                value
                                    .replace('-', "")
                                    .parse::<u32>()
                                    .map_err(|_| "invalid date filter")?,
                            ));
                        }
                    }
                }
                parts.sort_by_key(|p| if matches!(p,P::Compare{attribute,kind:1,..} if dates.get(attribute).is_some_and(|d|d.len()>1)) || matches!(p,P::Compare{attribute,kind:12,..} if timestamps.get(attribute).is_some_and(|d|d.len()>1)) {1} else {volume_cost(p, area)});
                // Combine both bounds of an unindexed date before scanning. Reuse the
                // complete per-volume result across reports with other predicates.
                let mut unindexed = std::collections::BTreeMap::<u16, Vec<P>>::new();
                for part in &parts {
                    if let P::Compare {
                        attribute,
                        kind: 1 | 12,
                        ..
                    } = part
                    {
                        if area.fields.get(attribute).is_some_and(|f| f.flags & 1 == 0) {
                            unindexed
                                .entry(*attribute)
                                .or_default()
                                .push((*part).clone());
                        }
                    }
                }
                let unindexed = unindexed
                    .into_iter()
                    .filter(|(_, p)| p.len() > 1)
                    .map(|(id, p)| {
                        (
                            id,
                            p.into_iter()
                                .reduce(|a, b| P::And(Box::new(a), Box::new(b)))
                                .unwrap(),
                        )
                    })
                    .collect::<std::collections::BTreeMap<_, _>>();
                let mut unindexed_done = std::collections::HashSet::new();
                let mut selected = restriction.map(<[u32]>::to_vec);
                let mut date_done = std::collections::HashSet::new();
                let mut stamp_done = std::collections::HashSet::new();
                for part in parts {
                    if let P::Compare { attribute, .. } = part {
                        if let Some(range) = unindexed.get(attribute) {
                            if unindexed_done.contains(attribute) {
                                continue;
                            }
                            let cached = self.range_cached(v, area.id, format!("{range:?}"))?;
                            if cached || !selected.as_ref().is_some_and(|ids| ids.len() <= 100000) {
                                unindexed_done.insert(*attribute);
                                let mut ids = self.unindexed_range(v, area, range)?;
                                if let Some(previous) = &selected {
                                    intersect(&mut ids, previous);
                                }
                                selected = Some(ids);
                                continue;
                            }
                        }
                    }
                    // Native LIKE switches from residual comparison to its
                    // index mask at ten candidates. The distinction is visible
                    // for lowercase ё (7/8-party vs 10/15-party native probes).
                    if selected.as_ref().is_some_and(|s| {
                        s.len() < 10 || s.len() <= 64 && !needs_native_text_index(p, area)
                    }) {
                        return residual(selected.unwrap());
                    }
                    if let P::Compare {
                        attribute,
                        kind: 12,
                        ..
                    } = part
                    {
                        if let Some(filters) = timestamps.get(attribute).filter(|d| d.len() > 1) {
                            if !stamp_done.insert(*attribute) {
                                continue;
                            }
                            let mut ids = self.cached_index_bounds(
                                (v, area.id, format!("timestamp:{attribute}:{filters:?}")),
                                || {
                                    self.with_reader(v, |tree| {
                                        tree.timestamp_candidates(area.id, *attribute, filters)
                                    })
                                },
                            )?;
                            if let Some(previous) = &selected {
                                intersect(&mut ids, previous);
                            }
                            selected = Some(ids);
                            continue;
                        }
                    }
                    if let P::Compare {
                        attribute, kind: 1, ..
                    } = part
                    {
                        if let Some(filters) = dates.get(attribute).filter(|d| d.len() > 1) {
                            if !date_done.insert(*attribute) {
                                continue;
                            }
                            let mut ids = self.cached_index_bounds(
                                (v, area.id, format!("date:{attribute}:{filters:?}")),
                                || {
                                    self.with_reader(v, |tree| {
                                        tree.date_candidates(area.id, *attribute, filters)
                                    })
                                },
                            )?;
                            if let Some(previous) = &selected {
                                intersect(&mut ids, previous);
                            }
                            selected = Some(ids);
                            continue;
                        }
                    }
                    selected = Some(self.indexed_set(v, area, part, selected.as_deref())?);
                }
                selected.unwrap_or_default()
            }
            P::Or(a, b) => {
                // A union of unindexed terms is one residual predicate. Scanning
                // the same area once per IN-list element is quadratic work.
                if volume_cost(a, area) >= 100000 && volume_cost(b, area) >= 100000 {
                    return residual(universe()?);
                }
                let mut a = self.indexed_set(v, area, a, restriction)?;
                a.extend(self.indexed_set(v, area, b, restriction)?);
                a.sort_unstable();
                a.dedup();
                a
            }
            P::Not(inner) => {
                let mut a = universe()?;
                let b = self.indexed_set(v, area, inner, Some(&a))?;
                subtract(&mut a, &b);
                a
            }
            P::IsNull {
                attribute,
                kind,
                negated,
            } => {
                let mut a = universe()?;
                let present = match area.fields.get(attribute) {
                    None => vec![],
                    Some(f) if f.flags & 1 != 0 && matches!(kind, 1 | 2 | 3 | 12) => self
                        .cached_index_bounds(
                            (v, area.id, format!("present:{attribute}:{kind}")),
                            || {
                                self.with_reader(v, |tree| {
                                    if *kind == 1 {
                                        tree.date_candidates(area.id, *attribute, &[])
                                    } else if *kind == 12 {
                                        tree.timestamp_candidates(area.id, *attribute, &[])
                                    } else {
                                        tree.text_candidates(area.id, *attribute, |_| true)
                                    }
                                })
                            },
                        )?,
                    _ => return residual(a),
                };
                if *negated {
                    intersect(&mut a, &present);
                } else {
                    subtract(&mut a, &present);
                }
                a
            }
            P::Link { attribute, .. }
            | P::Text { attribute, .. }
            | P::Compare { attribute, .. }
            | P::Between { attribute, .. }
            | P::Set {
                attribute: Some(attribute),
                ..
            } => {
                let Some(f) = area.fields.get(attribute) else {
                    return Ok(vec![]);
                };
                if f.flags & 1 == 0
                    || !matches!(f.kind, 1 | 2 | 3 | 5 | 12)
                    || matches!(p, P::Set { ids, .. } if ids.len() > 256)
                {
                    if let P::Link { attribute, oid } = p {
                        if restriction.map_or(true, |ids| ids.len() > 1024) {
                            if let Some(mut ids) =
                                self.reverse_link_matches(v, area, *attribute, *oid)?
                            {
                                if let Some(restriction) = restriction {
                                    intersect(&mut ids, restriction);
                                }
                                return Ok(ids);
                            }
                        }
                    }
                    if matches!(p, P::Between { kind: 1 | 12, .. })
                        && restriction.map_or(true, |ids| ids.len() > 100000)
                    {
                        let mut ids = self.unindexed_range(v, area, p)?;
                        if let Some(restriction) = restriction {
                            intersect(&mut ids, restriction);
                        }
                        return Ok(ids);
                    }
                    return residual(universe()?);
                }
                if matches!(
                    p,
                    P::Compare { kind: 1 | 12, .. } | P::Between { kind: 1 | 12, .. }
                ) {
                    self.cached_index_bounds((v, area.id, format!("{p:?}")), || {
                        self.predicate_candidates_impl(v, area, p, true)
                    })?
                } else {
                    self.predicate_candidates_impl(v, area, p, true)?
                }
            }
        };
        ids.sort_unstable();
        ids.dedup();
        if let Some(restriction) = restriction {
            intersect(&mut ids, restriction);
        }
        if ids.len() > MAX_INDEX_CANDIDATES {
            return Err("SQL index candidate limit".into());
        }
        Ok(ids)
    }
}
