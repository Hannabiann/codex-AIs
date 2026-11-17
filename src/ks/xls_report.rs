//! In-memory BIFF8 template substitution. The original workbook's styles,
//! formulas, drawings, print settings and other compound-file streams survive.
//! MS-XLS: SST, LabelSst, Index, DBCell. MS-CFB: FAT and directory entries.
use std::collections::{BTreeMap, HashSet};
const FREE: u32 = 0xffff_ffff;
const END: u32 = 0xffff_fffe;
const FAT: u32 = 0xffff_fffd;
const DIF: u32 = 0xffff_fffc;
const LIMIT: usize = 64 * 1024 * 1024;
type Result<T> = std::result::Result<T, String>;
fn u16at(b: &[u8], p: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        b.get(p..p + 2)
            .ok_or("truncated XLS u16")?
            .try_into()
            .unwrap(),
    ))
}
fn u32at(b: &[u8], p: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        b.get(p..p + 4)
            .ok_or("truncated XLS u32")?
            .try_into()
            .unwrap(),
    ))
}
fn put16(b: &mut [u8], p: usize, n: usize) -> Result<()> {
    let n = u16::try_from(n).map_err(|_| "XLS u16 offset overflow")?;
    b.get_mut(p..p + 2)
        .ok_or("XLS write outside record")?
        .copy_from_slice(&n.to_le_bytes());
    Ok(())
}
fn put32(b: &mut [u8], p: usize, n: u32) -> Result<()> {
    b.get_mut(p..p + 4)
        .ok_or("XLS write outside record")?
        .copy_from_slice(&n.to_le_bytes());
    Ok(())
}
struct Compound {
    bytes: Vec<u8>,
    size: usize,
    fat: Vec<u32>,
    fats: Vec<u32>,
    difs: Vec<u32>,
    directory: Vec<u8>,
    dirs: Vec<u32>,
    workbook_entry: usize,
}
impl Compound {
    fn sector(&self, id: u32) -> Result<&[u8]> {
        let start = (id as usize + 1)
            .checked_mul(self.size)
            .ok_or("CFB sector overflow")?;
        self.bytes
            .get(start..start + self.size)
            .ok_or_else(|| "CFB sector outside file".into())
    }
    fn chain(&self, mut id: u32) -> Result<Vec<u32>> {
        let mut ids = Vec::new();
        let mut seen = HashSet::new();
        while id != END {
            self.sector(id)?;
            if !seen.insert(id) {
                return Err("CFB chain cycle".into());
            }
            ids.push(id);
            id = *self.fat.get(id as usize).ok_or("CFB FAT entry missing")?;
        }
        Ok(ids)
    }
    fn stream(&self, ids: &[u32]) -> Result<Vec<u8>> {
        let mut b = Vec::new();
        for &id in ids {
            b.extend_from_slice(self.sector(id)?);
        }
        Ok(b)
    }
    fn open(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 512
            || bytes.len() > LIMIT
            || bytes[..8] != [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]
        {
            return Err("invalid XLS compound file".into());
        }
        let major = u16at(bytes, 26)?;
        let shift = u16at(bytes, 30)?;
        if !matches!((major, shift), (3, 9) | (4, 12)) || u16at(bytes, 28)? != 0xfffe {
            return Err("unsupported CFB version".into());
        }
        let size = 1usize << shift;
        if bytes.len() % size != 0 {
            return Err("unaligned CFB file".into());
        }
        let mut c = Self {
            bytes: bytes.to_vec(),
            size,
            fat: vec![],
            fats: vec![],
            difs: vec![],
            directory: vec![],
            dirs: vec![],
            workbook_entry: 0,
        };
        let count = u32at(bytes, 44)? as usize;
        if count > bytes.len() / size {
            return Err("invalid CFB FAT count".into());
        }
        for i in 0..109 {
            let id = u32at(bytes, 76 + 4 * i)?;
            if id != FREE {
                c.fats.push(id);
            }
        }
        let mut id = u32at(bytes, 68)?;
        let mut seen = HashSet::new();
        for _ in 0..u32at(bytes, 72)? {
            if !seen.insert(id) {
                return Err("CFB DIFAT cycle".into());
            }
            let sector = c.sector(id)?.to_vec();
            c.difs.push(id);
            for off in (0..size - 4).step_by(4) {
                let n = u32at(&sector, off)?;
                if n != FREE {
                    c.fats.push(n);
                }
            }
            id = u32at(&sector, size - 4)?;
        }
        if c.fats.len() != count || (!c.difs.is_empty() && id != END) {
            return Err("inconsistent CFB DIFAT".into());
        }
        let mut seen = HashSet::new();
        for &id in &c.fats {
            if !seen.insert(id) {
                return Err("duplicate CFB FAT sector".into());
            }
            let s = c.sector(id)?;
            let entries = s
                .chunks_exact(4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                .collect::<Vec<_>>();
            c.fat.extend(entries);
        }
        c.dirs = c.chain(u32at(bytes, 48)?)?;
        c.directory = c.stream(&c.dirs)?;
        let mut found = None;
        for (i, d) in c.directory.chunks_exact(128).enumerate() {
            if d[66] != 2 {
                continue;
            }
            let n = u16at(d, 64)? as usize;
            if !(2..=64).contains(&n) || n % 2 != 0 {
                return Err("invalid CFB stream name".into());
            }
            let name = String::from_utf16(
                &d[..n - 2]
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .collect::<Vec<_>>(),
            )
            .map_err(|_| "invalid CFB UTF-16")?;
            if name.eq_ignore_ascii_case("Workbook") || name.eq_ignore_ascii_case("Book") {
                if found.replace(i * 128).is_some() {
                    return Err("duplicate workbook stream".into());
                }
            }
        }
        c.workbook_entry = found.ok_or("XLS workbook stream missing")?;
        Ok(c)
    }
    fn workbook(&self) -> Result<Vec<u8>> {
        let p = self.workbook_entry;
        let n = u32at(&self.directory, p + 120)? as usize;
        if u32at(&self.directory, p + 124)? != 0 || n > LIMIT || n < 4096 {
            return Err("XLS workbook stream size unsupported".into());
        }
        let mut b = self.stream(&self.chain(u32at(&self.directory, p + 116)?)?)?;
        if n > b.len() {
            return Err("truncated workbook chain".into());
        }
        b.truncate(n);
        Ok(b)
    }
    fn allocate(&mut self) -> Result<u32> {
        if self.bytes.len() + self.size > LIMIT {
            return Err("CFB output size limit".into());
        }
        let id = (self.bytes.len() / self.size - 1) as u32;
        self.bytes.resize(self.bytes.len() + self.size, 0);
        if self.fat.len() <= id as usize {
            self.fat.resize(id as usize + 1, FREE);
        }
        self.fat[id as usize] = END;
        Ok(id)
    }
    fn write_sector(&mut self, id: u32, bytes: &[u8]) -> Result<()> {
        if bytes.len() > self.size {
            return Err("CFB oversized sector write".into());
        }
        let off = (id as usize + 1) * self.size;
        let dst = self
            .bytes
            .get_mut(off..off + self.size)
            .ok_or("CFB output sector missing")?;
        dst.fill(0);
        dst[..bytes.len()].copy_from_slice(bytes);
        Ok(())
    }
    fn replace(mut self, workbook: &[u8]) -> Result<Vec<u8>> {
        if workbook.len() < 4096 || workbook.len() > LIMIT {
            return Err("XLS replacement size unsupported".into());
        }
        let p = self.workbook_entry;
        let mut chain = self.chain(u32at(&self.directory, p + 116)?)?;
        let need = (workbook.len() + self.size - 1) / self.size;
        while chain.len() < need {
            chain.push(self.allocate()?);
        }
        for id in chain.drain(need..) {
            self.fat[id as usize] = FREE;
        }
        for (i, &id) in chain.iter().enumerate() {
            self.fat[id as usize] = chain.get(i + 1).copied().unwrap_or(END);
            self.write_sector(
                id,
                &workbook[i * self.size..workbook.len().min((i + 1) * self.size)],
            )?;
        }
        put32(&mut self.directory, p + 116, chain[0])?;
        put32(&mut self.directory, p + 120, workbook.len() as u32)?;
        put32(&mut self.directory, p + 124, 0)?;
        for (i, id) in self.dirs.clone().into_iter().enumerate() {
            let part = self.directory[i * self.size..(i + 1) * self.size].to_vec();
            self.write_sector(id, &part)?;
        }
        // FAT/DIFAT sectors contribute to the FAT size, so grow to a fixed point.
        loop {
            let need = ((self.bytes.len() / self.size - 1) * 4 + self.size - 1) / self.size;
            if self.fats.len() < need {
                let id = self.allocate()?;
                self.fats.push(id);
                continue;
            }
            let need =
                (self.fats.len().saturating_sub(109) + (self.size / 4 - 2)) / (self.size / 4 - 1);
            if self.difs.len() < need {
                let id = self.allocate()?;
                self.difs.push(id);
                continue;
            }
            break;
        }
        self.fat.resize(self.fats.len() * self.size / 4, FREE);
        for &id in &self.fats {
            self.fat[id as usize] = FAT;
        }
        for &id in &self.difs {
            self.fat[id as usize] = DIF;
        }
        for (i, id) in self.fats.clone().into_iter().enumerate() {
            let part = self.fat[i * self.size / 4..(i + 1) * self.size / 4]
                .iter()
                .flat_map(|n| n.to_le_bytes())
                .collect::<Vec<_>>();
            self.write_sector(id, &part)?;
        }
        for (i, id) in self.difs.clone().into_iter().enumerate() {
            let mut part = vec![255; self.size];
            for j in 0..self.size / 4 - 1 {
                put32(
                    &mut part,
                    4 * j,
                    self.fats
                        .get(109 + i * (self.size / 4 - 1) + j)
                        .copied()
                        .unwrap_or(FREE),
                )?;
            }
            put32(
                &mut part,
                self.size - 4,
                self.difs.get(i + 1).copied().unwrap_or(END),
            )?;
            self.write_sector(id, &part)?;
        }
        put32(&mut self.bytes, 44, self.fats.len() as u32)?;
        put32(
            &mut self.bytes,
            68,
            self.difs.first().copied().unwrap_or(END),
        )?;
        put32(&mut self.bytes, 72, self.difs.len() as u32)?;
        for i in 0..109 {
            put32(
                &mut self.bytes,
                76 + 4 * i,
                self.fats.get(i).copied().unwrap_or(FREE),
            )?;
        }
        Ok(self.bytes)
    }
}
struct Record {
    old: usize,
    at: usize,
    kind: u16,
    data: Vec<u8>,
}
fn records(b: &[u8]) -> Result<Vec<Record>> {
    let mut at = 0;
    let mut out = Vec::new();
    while at < b.len() {
        if b[at..].len() < 4 {
            return Err("truncated BIFF header".into());
        }
        let kind = u16at(b, at)?;
        let n = u16at(b, at + 2)? as usize;
        if kind == 0 && n == 0 && b[at..].iter().all(|&b| b == 0) {
            break;
        }
        let data = b
            .get(at + 4..at + 4 + n)
            .ok_or("truncated BIFF record")?
            .to_vec();
        out.push(Record {
            old: at,
            at,
            kind,
            data,
        });
        at += 4 + n;
        if out.len() > 1_000_000 {
            return Err("BIFF record count limit".into());
        }
    }
    Ok(out)
}
struct Segments<'a> {
    segments: Vec<&'a [u8]>,
    seg: usize,
    pos: usize,
}
impl Segments<'_> {
    fn raw(&mut self, n: usize) -> Result<Vec<u8>> {
        if n > LIMIT {
            return Err("SST length limit".into());
        }
        let mut b = Vec::with_capacity(n);
        while b.len() < n {
            let seg = self
                .segments
                .get(self.seg)
                .ok_or("SST truncated continuation")?;
            if self.pos == seg.len() {
                self.seg += 1;
                self.pos = 0;
                continue;
            }
            let take = (n - b.len()).min(seg.len() - self.pos);
            b.extend_from_slice(&seg[self.pos..self.pos + take]);
            self.pos += take;
        }
        Ok(b)
    }
    fn string(&mut self) -> Result<String> {
        let mut n = u16at(&self.raw(2)?, 0)? as usize;
        let flags = self.raw(1)?[0];
        let runs = if flags & 8 != 0 {
            u16at(&self.raw(2)?, 0)? as usize
        } else {
            0
        };
        let extra = if flags & 4 != 0 {
            u32at(&self.raw(4)?, 0)? as usize
        } else {
            0
        };
        let mut wide = flags & 1 != 0;
        let mut chars = Vec::with_capacity(n);
        while n > 0 {
            if self.pos
                == self
                    .segments
                    .get(self.seg)
                    .ok_or("SST truncated text")?
                    .len()
            {
                self.seg += 1;
                self.pos = 0;
                wide = self.raw(1)?[0] & 1 != 0;
            }
            let len = self
                .segments
                .get(self.seg)
                .ok_or("SST text segment missing")?
                .len();
            let take = n.min((len - self.pos) / if wide { 2 } else { 1 });
            if take == 0 {
                return Err("SST split UTF-16 code unit".into());
            }
            let bytes = self.raw(take * if wide { 2 } else { 1 })?;
            if wide {
                chars.extend(
                    bytes
                        .chunks_exact(2)
                        .map(|b| u16::from_le_bytes([b[0], b[1]])),
                );
            } else {
                chars.extend(bytes.iter().map(|&b| b as u16));
            }
            n -= take;
        }
        self.raw(runs * 4 + extra)?;
        String::from_utf16(&chars).map_err(|_| "invalid SST UTF-16".into())
    }
}
fn strings(records: &[Record]) -> Result<Vec<String>> {
    let Some(start) = records.iter().position(|r| r.kind == 0xfc) else {
        return Err("BIFF SST missing".into());
    };
    let mut parts = vec![records[start].data.as_slice()];
    for r in records
        .iter()
        .skip(start + 1)
        .take_while(|r| r.kind == 0x3c)
    {
        parts.push(&r.data);
    }
    let n = u32at(parts[0], 4)? as usize;
    if n > 1_000_000 {
        return Err("SST string count limit".into());
    }
    let mut reader = Segments {
        segments: parts,
        seg: 0,
        pos: 8,
    };
    (0..n).map(|_| reader.string()).collect()
}
fn number_rk(n: f64) -> Option<u32> {
    for (n, flag) in [(n, 0), (n * 100.0, 1)] {
        if n.fract() == 0.0 && (-536870912.0..=536870911.0).contains(&n) {
            return Some(((n as i32 as u32) << 2) | 2 | flag);
        }
        let bits = n.to_bits();
        if bits & 0x3ffffffff == 0 {
            return Some((bits >> 32) as u32 | flag);
        }
    }
    None
}
fn replace_cell(r: &mut Record, value: &str) -> Result<()> {
    let base = r.data.get(..6).ok_or("invalid LabelSst cell")?.to_vec();
    r.data = base;
    if value.is_empty() {
        r.kind = 0x201;
        return Ok(());
    }
    if let Ok(n) = value.parse::<f64>() {
        if n.is_finite() {
            if let Some(rk) = number_rk(n) {
                r.kind = 0x27e;
                r.data.extend_from_slice(&rk.to_le_bytes());
            } else {
                r.kind = 0x203;
                r.data.extend_from_slice(&n.to_le_bytes());
            }
            return Ok(());
        }
    }
    let text = value.encode_utf16().collect::<Vec<_>>();
    if text.len() > 4000 {
        return Err("XLS report cell text exceeds 4000 UTF-16 units".into());
    }
    r.kind = 0x204;
    r.data.extend_from_slice(&(text.len() as u16).to_le_bytes());
    r.data.push(1);
    for c in text {
        r.data.extend_from_slice(&c.to_le_bytes());
    }
    Ok(())
}
pub(super) fn expand(template: &[u8], values: &BTreeMap<String, String>) -> Result<Vec<u8>> {
    let c = Compound::open(template)?;
    let original = c.workbook()?;
    let mut rs = records(&original)?;
    let sst = strings(&rs)?;
    let mut replacements = 0;
    for r in &mut rs {
        if r.kind == 0xfd {
            let text = sst
                .get(u32at(&r.data, 6)? as usize)
                .ok_or("LabelSst string index outside SST")?;
            if let Some(path) = text.strip_prefix("xpath:") {
                if !path.starts_with('/')
                    || !path
                        .chars()
                        .all(|c| c.is_alphanumeric() || matches!(c, '/' | '_' | '-' | '.'))
                {
                    return Err(format!("unsupported XLS XPath {path}"));
                }
                replace_cell(r, values.get(path).map(String::as_str).unwrap_or(""))?;
                replacements += 1;
            }
        } else if r.kind == 0x0d && r.data.len() == 2 {
            put16(&mut r.data, 0, 1)?;
        }
        // automatic calculation
        else if r.kind == 0x5e && r.data.len() == 2 {
            put16(&mut r.data, 0, 1)?;
        }
        // recalculate before saving
        else if r.kind == 0x1c1 && r.data.len() == 8 {
            put32(&mut r.data, 4, 0)?;
        }
        // RecalcId engine mismatch
        else if r.kind == 6 && r.data.len() >= 16 {
            let flags = u16at(&r.data, 14)?;
            put16(&mut r.data, 14, (flags | 3) as usize)?;
        }
    }
    if replacements == 0 {
        return Err("XLS template has no XPath cells".into());
    }
    let mut map = BTreeMap::new();
    let mut at = 0;
    for r in &mut rs {
        r.at = at;
        map.insert(r.old, at);
        at += 4 + r.data.len();
    }
    map.insert(original.len(), at);
    let locate = |old: usize| {
        map.get(&old)
            .copied()
            .ok_or_else(|| format!("XLS pointer {old} is not a record boundary"))
    };
    for r in &mut rs {
        if r.kind == 0x85 {
            let old = u32at(&r.data, 0)?;
            put32(&mut r.data, 0, locate(old as usize)? as u32)?;
        }
        if r.kind == 0x20b {
            for p in (12..r.data.len()).step_by(4) {
                let old = u32at(&r.data, p)?;
                if old != 0 {
                    put32(&mut r.data, p, locate(old as usize)? as u32)?;
                }
            }
        }
        if r.kind == 0xd7 {
            let back = u32at(&r.data, 0)? as usize;
            if back == 0 {
                continue;
            }
            let row = r.old.checked_sub(back).ok_or("DBCell invalid row offset")?;
            put32(&mut r.data, 0, (r.at - locate(row)?) as u32)?;
            let row_end = row + 4 + u16at(&original, row + 2)? as usize;
            let mut before = row_end;
            for p in (4..r.data.len()).step_by(2) {
                let next = before + u16at(&r.data, p)? as usize;
                put16(
                    &mut r.data,
                    p,
                    locate(next)?
                        .checked_sub(locate(before)?)
                        .ok_or("DBCell offset order")?,
                )?;
                before = next;
            }
        }
    }
    let mut workbook = Vec::with_capacity(at);
    for r in rs {
        workbook.extend_from_slice(&r.kind.to_le_bytes());
        workbook.extend_from_slice(&(r.data.len() as u16).to_le_bytes());
        workbook.extend_from_slice(&r.data);
    }
    c.replace(&workbook)
}
