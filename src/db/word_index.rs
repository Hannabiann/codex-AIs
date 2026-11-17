//! WordsPositionsDocs K(ffff,2), native 0081fc00 / 0081f960.
//! Keys are a length-prefixed word, first OID, count, BE128 OID
//! deltas, and per-document frequencies. Compressed S frames use MainKey.
use super::{u16at, u32at, MainKey};
use std::collections::{BTreeMap, BTreeSet, HashSet};

fn number(data: &[u8], at: &mut usize) -> Result<u32, String> {
    let mut value = 0u32;
    for _ in 0..5 {
        let b = *data.get(*at).ok_or("truncated word index integer")?;
        *at += 1;
        value = value
            .checked_mul(128)
            .and_then(|v| v.checked_add((b & 127) as u32))
            .ok_or("word index integer overflow")?;
        if b & 128 == 0 {
            return Ok(value);
        }
    }
    Err("word index integer overflow".into())
}
fn key(data: &[u8]) -> Result<(&[u8], u32), String> {
    let n = (*data.first().ok_or("empty word index entry")? & 127) as usize;
    if n == 0 || n > 32 || data.len() < n + 6 {
        return Err("invalid word index key".into());
    }
    Ok((&data[1..n + 1], u32at(data, n + 1)))
}
fn postings(data: &[u8]) -> Result<Vec<(u32, u32)>, String> {
    let (word, first) = key(data)?;
    let count = data[word.len() + 5] as usize;
    if count == 0 || count > 250 {
        return Err("invalid word index document count".into());
    }
    let mut at = word.len() + 6;
    let mut ids = vec![first];
    for _ in 1..count {
        let delta = number(data, &mut at)?;
        if delta == 0 {
            return Err("duplicate word index document".into());
        }
        ids.push(
            ids.last()
                .unwrap()
                .checked_add(delta)
                .ok_or("word index OID overflow")?,
        );
    }
    let mut frequencies = vec![255; count];
    if data[0] & 128 == 0 {
        for f in &mut frequencies {
            *f = number(data, &mut at)?;
        }
    }
    if at != data.len() {
        return Err("trailing word index entry bytes".into());
    }
    Ok(ids.into_iter().zip(frequencies).collect())
}

impl MainKey {
    pub fn word_candidates(
        &mut self,
        word: &[u8],
        prefix: bool,
        restrict: Option<&BTreeSet<u32>>,
    ) -> Result<Vec<u32>, String> {
        Ok(self.word_hits(word, prefix, None, restrict, 10000)?.0)
    }
    pub fn word_hits(
        &mut self,
        word: &[u8],
        prefix: bool,
        morphology: Option<&BTreeSet<String>>,
        restrict: Option<&BTreeSet<u32>>,
        limit: usize,
    ) -> Result<(Vec<u32>, Vec<u32>), String> {
        let (ids, statistics, _) =
            self.word_weights(word, prefix, morphology, restrict, limit, None)?;
        Ok((ids, statistics))
    }
    pub fn word_weights(
        &mut self,
        word: &[u8],
        prefix: bool,
        morphology: Option<&BTreeSet<String>>,
        restrict: Option<&BTreeSet<u32>>,
        limit: usize,
        exact: Option<&[u8]>,
    ) -> Result<(Vec<u32>, Vec<u32>, BTreeMap<u32, u32>), String> {
        if word.is_empty() || word.len() > 32 {
            return Err("context search word length must be 1..32 bytes".into());
        }
        let roots = self.directory.clone();
        let mut root = None;
        for c in roots.chunks_exact(10) {
            if u16at(c, 6) == 65535 && u16at(c, 8) == 2 {
                if root.replace(u32at(c, 2)).is_some() {
                    return Err("duplicate word index root".into());
                }
            }
        }
        let Some(root) = root else {
            return Ok((vec![], vec![], BTreeMap::new()));
        };
        let mut seen = HashSet::new();
        let mut out = BTreeSet::new();
        let initial = self.bytes;
        let mut total = Vec::new();
        let mut weights = BTreeMap::new();
        let mut titles = BTreeSet::new();
        if root != 0 {
            self.walk_words(
                root,
                word,
                prefix,
                morphology,
                restrict,
                limit,
                0,
                initial,
                &mut seen,
                &mut out,
                &mut total,
                &mut weights,
                &mut titles,
                exact,
            )?;
        }
        total.sort_unstable();
        total.dedup();
        for id in titles {
            if let Some(weight) = weights.get_mut(&id) {
                *weight *= 4;
            }
        }
        Ok((out.into_iter().collect(), total, weights))
    }
    fn walk_words(
        &mut self,
        id: u32,
        word: &[u8],
        prefix: bool,
        morphology: Option<&BTreeSet<String>>,
        restrict: Option<&BTreeSet<u32>>,
        limit: usize,
        depth: usize,
        initial: u64,
        seen: &mut HashSet<u32>,
        out: &mut BTreeSet<u32>,
        total: &mut Vec<u32>,
        weights: &mut BTreeMap<u32, u32>,
        titles: &mut BTreeSet<u32>,
        exact: Option<&[u8]>,
    ) -> Result<(), String> {
        if depth >= 32 || seen.len() >= 16384 || !seen.insert(id) {
            return Err("word index depth/node budget/cycle".into());
        }
        let b = self.block(id, b'L', 65000)?;
        if self.bytes - initial > 64 * 1024 * 1024 {
            return Err("word index byte budget exceeded".into());
        }
        if b.len() < 7 || u16at(&b, 0) != 2 || u16at(&b, 2) != 65535 {
            return Err("not a word index node".into());
        }
        let count = u16at(&b, 4) as usize;
        let branch = b[6] != 0;
        let base = 7 + count * 2;
        if base > b.len() {
            return Err("truncated word offset table".into());
        }
        let cmp = |s: &[u8]| {
            if prefix && s.starts_with(word) {
                std::cmp::Ordering::Equal
            } else {
                s.cmp(word)
            }
        };
        let mut previous = 0;
        let mut lower: Option<(&[u8], u32)> = None;
        for i in 0..count {
            let end = u16at(&b, 7 + 2 * i) as usize;
            let start = previous + if branch { 4 } else { 0 };
            if end < start || base + end > b.len() {
                return Err("invalid word entry bounds".into());
            }
            let entry = &b[base + start..base + end];
            let k = key(entry)?;
            if lower.is_some_and(|lo| lo >= k) {
                return Err("unordered word index".into());
            }
            if branch && !cmp(k.0).is_lt() && lower.map_or(true, |lo| !cmp(lo.0).is_gt()) {
                let child = u32at(&b, base + previous);
                if child == 0 {
                    return Err("null word child".into());
                }
                self.walk_words(
                    child,
                    word,
                    prefix,
                    morphology,
                    restrict,
                    limit,
                    depth + 1,
                    initial,
                    seen,
                    out,
                    total,
                    weights,
                    titles,
                    exact,
                )?;
            }
            if cmp(k.0).is_eq()
                && morphology.map_or(true, |forms| forms.contains(&crate::db::decode(k.0)))
            {
                for (oid, frequency) in postings(entry)? {
                    if morphology.is_some() {
                        total.push(oid);
                        if total.len() > 8_000_000 {
                            return Err("word statistics budget exceeded".into());
                        }
                    }
                    if restrict.map_or(true, |r| r.contains(&oid)) {
                        out.insert(oid);
                        if frequency != 65535 {
                            let mut weight = frequency;
                            if weight & 0x200 != 0 {
                                titles.insert(oid);
                                weight = if weight == 0x600 {
                                    254 / 4
                                } else {
                                    weight & !255
                                };
                            }
                            if exact.is_some_and(|w| w == k.0) {
                                weight = weight * 3 / 2;
                            }
                            *weights.entry(oid).or_default() += weight;
                        }
                        if out.len() > limit {
                            return Err(format!(
                                "context candidate limit ({limit}); narrow the search"
                            ));
                        }
                    }
                }
            }
            previous = end;
            lower = Some(k);
        }
        if branch && lower.map_or(true, |lo| !cmp(lo.0).is_gt()) {
            if base + previous + 4 > b.len() {
                return Err("truncated final word child".into());
            }
            let child = u32at(&b, base + previous);
            if child == 0 {
                return Err("null word child".into());
            }
            self.walk_words(
                child,
                word,
                prefix,
                morphology,
                restrict,
                limit,
                depth + 1,
                initial,
                seen,
                out,
                total,
                weights,
                titles,
                exact,
            )?;
        }
        Ok(())
    }
}
