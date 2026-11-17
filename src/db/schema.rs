//! Native DefAreas / DefFields: FUN_00601b20, 00602fc0, 00601960, 00600b70.
use std::{collections::BTreeMap, path::Path};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub id: u16,
    pub kind: u16,
    pub flags: u16,
    pub reference: u16,
    pub name: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Area {
    pub id: u16,
    pub flags: u16,
    pub name: String,
    pub fields: BTreeMap<u16, Field>,
}
#[derive(Clone, Debug, Default)]
pub struct Schema {
    pub areas: BTreeMap<u16, Area>,
}
#[derive(Debug)]
pub struct MergeConflict {
    pub area: u16,
    pub previous: Field,
    pub incoming: Field,
}
impl std::fmt::Display for MergeConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "field {}/{}: native type {} -> {}, reference {} -> {}",
            self.area,
            self.incoming.id,
            self.previous.kind,
            self.incoming.kind,
            self.previous.reference,
            self.incoming.reference
        )
    }
}
impl Schema {
    /// Native 005e1110 / 00600850: later names and flags replace earlier ones;
    /// a zero reference does not erase an existing nonzero reference.
    /// Different types or two different nonzero references are incompatible.
    pub fn merge_native(&mut self, incoming: &Schema) -> Result<(), MergeConflict> {
        // Validate before publishing any part of the incoming schema.
        for (id, area) in &incoming.areas {
            if let Some(previous) = self.areas.get(id) {
                for (fid, field) in &area.fields {
                    if let Some(old) = previous.fields.get(fid) {
                        if old.kind != field.kind
                            || (old.reference != 0
                                && field.reference != 0
                                && old.reference != field.reference)
                        {
                            return Err(MergeConflict {
                                area: *id,
                                previous: old.clone(),
                                incoming: field.clone(),
                            });
                        }
                    }
                }
            }
        }
        for (id, area) in &incoming.areas {
            if let Some(previous) = self.areas.get_mut(id) {
                previous.name.clone_from(&area.name);
                previous.flags = area.flags;
                for (fid, field) in &area.fields {
                    let mut field = field.clone();
                    if field.reference == 0 {
                        if let Some(old) = previous.fields.get(fid) {
                            field.reference = old.reference;
                        }
                    }
                    previous.fields.insert(*fid, field);
                }
            } else {
                self.areas.insert(*id, area.clone());
            }
        }
        Ok(())
    }
}
struct Cursor<'a> {
    b: &'a [u8],
    p: usize,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.p.checked_add(n).ok_or("schema offset overflow")?;
        let out = self.b.get(self.p..end).ok_or("truncated schema")?;
        self.p = end;
        Ok(out)
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn short(&mut self) -> Result<Vec<u8>, String> {
        let n = self.u16()? as usize;
        Ok(self.take(n)?.to_vec())
    }
    fn name(&mut self, modern: bool) -> Result<String, String> {
        let b = if modern {
            self.short()?
        } else {
            let n = self.b[self.p..]
                .iter()
                .position(|&b| b == 0)
                .ok_or("unterminated schema name")?;
            let b = self.take(n)?.to_vec();
            self.take(1)?;
            b
        };
        if b.len() > 4096 {
            return Err("schema name limit".into());
        }
        Ok(super::decode(&b))
    }
}
impl Schema {
    pub fn area(&self, key: &str) -> Option<&Area> {
        key.parse::<u16>()
            .ok()
            .and_then(|n| self.areas.get(&n))
            .or_else(|| self.areas.values().find(|a| a.name == key))
    }
    pub fn read(path: &Path) -> Result<Self, String> {
        let mut index = super::mainkey::MainKey::open(path)?;
        let (version, raw) = index.schema_payload()?;
        Self::parse(version, &raw)
    }
    pub fn parse(version: u16, raw: &[u8]) -> Result<Self, String> {
        let modern = version > 0x612;
        let decoded;
        let mut c = if modern {
            let mut packed = Cursor { b: raw, p: 0 };
            let mut out = Vec::new();
            while packed.p < raw.len() {
                let n = packed.u16()? as usize;
                if n == 0 {
                    break;
                }
                let k = packed.u16()? as usize;
                let _checksum = packed.u16()?;
                if n > 65000 || k == 0 || k > n {
                    return Err("invalid schema compression frame".into());
                }
                let data = packed.take(k)?;
                if n == k {
                    out.extend(data);
                } else {
                    out.extend(
                        super::fields::decompress_mode4(data, n).map_err(|e| e.to_string())?,
                    );
                }
                if out.len() > 4 * 1024 * 1024 {
                    return Err("schema size limit".into());
                }
            }
            decoded = out;
            Cursor { b: &decoded, p: 0 }
        } else {
            Cursor { b: raw, p: 0 }
        };
        let count = if modern {
            let _version = c.u16()?;
            let n = c.u32()? as usize;
            let bytes = c.take(n)?;
            c = Cursor { b: bytes, p: 0 };
            c.short()?;
            c.take(256)?;
            c.short()?;
            let signed = c.u16()?;
            if signed != 0 {
                c.short()?;
            }
            Some(c.u16()? as usize)
        } else {
            None
        };
        let mut areas = BTreeMap::new();
        let mut fields_total = 0;
        loop {
            if count == Some(areas.len()) {
                break;
            }
            let id = c.u16()?;
            if !modern && id == u16::MAX {
                break;
            }
            if areas.len() >= 10000 {
                return Err("schema area limit".into());
            }
            let flags = c.u16()?;
            let name = c.name(modern)?;
            let mut fields = BTreeMap::new();
            loop {
                let id = c.u16()?;
                if id == u16::MAX {
                    break;
                }
                fields_total += 1;
                if fields_total > 100000 {
                    return Err("schema field limit".into());
                }
                let kind = c.u16()?;
                let flags = c.u16()?;
                let reference = c.u16()?;
                let _size = c.u16()?;
                c.take(16)?;
                let name = c.name(modern)?;
                if fields
                    .insert(
                        id,
                        Field {
                            id,
                            kind,
                            flags,
                            reference,
                            name,
                        },
                    )
                    .is_some()
                {
                    return Err("duplicate schema field".into());
                }
            }
            if areas
                .insert(
                    id,
                    Area {
                        id,
                        flags,
                        name,
                        fields,
                    },
                )
                .is_some()
            {
                return Err("duplicate schema area".into());
            }
        }
        if c.p != c.b.len() {
            return Err(format!("unparsed schema bytes: {}", c.b.len() - c.p));
        }
        Ok(Self { areas })
    }
}
