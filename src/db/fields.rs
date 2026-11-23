//! Strict subset of DB4 fields verified against the supplied volumes.
//! Unknown types and compression modes are errors, never byte-scan recovery.
use std::fmt;

pub const MAX_FIELD: usize = 16 * 1024 * 1024;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError {
    pub offset: usize,
    pub message: String,
}
impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "offset 0x{:x}: {}", self.offset, self.message)
    }
}
impl std::error::Error for DecodeError {}
type Result<T> = std::result::Result<T, DecodeError>;
fn err(offset: usize, message: impl Into<String>) -> DecodeError {
    DecodeError {
        offset,
        message: message.into(),
    }
}
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| err(self.pos, "size overflow"))?;
        let out = self
            .data
            .get(self.pos..end)
            .ok_or_else(|| err(self.pos, "truncated field"))?;
        self.pos = end;
        Ok(out)
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn short_bytes(&mut self) -> Result<Vec<u8>> {
        let n = self.u16()? as usize;
        Ok(self.take(n)?.to_vec())
    }
}
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}
impl Bits<'_> {
    fn read(&mut self, n: usize) -> Result<usize> {
        if self.pos + n > self.data.len() * 8 {
            return Err(err(self.pos / 8, "truncated compressed bits"));
        }
        let mut value = 0;
        for i in 0..n {
            value |= (((self.data[(self.pos + i) / 8] >> ((self.pos + i) % 8)) & 1) as usize) << i;
        }
        self.pos += n;
        Ok(value)
    }
    fn gamma(&mut self) -> Result<usize> {
        let mut n = 1;
        while self.read(1)? == 0 {
            n += 1;
            if n > 9 {
                return Err(err(self.pos / 8, "invalid gamma code"));
            }
        }
        Ok((1 << n) - 1 + self.read(n)?)
    }
}
/// FUN_005dea40 mode 0 (low three bits == 4).
/// Other modes explicitly rejected. Checksums require a separately verified algorithm.
pub fn decompress_mode4(data: &[u8], expected: usize) -> Result<Vec<u8>> {
    if expected > 65000 {
        return Err(err(0, "compressed block too large"));
    }
    if data.first().map(|x| x & 7) != Some(4) {
        return Err(err(0, "unsupported compression mode (expected 4)"));
    }
    let mut bits = Bits { data, pos: 3 };
    let mut out = Vec::with_capacity(expected);
    while out.len() < expected {
        if bits.read(1)? == 0 {
            out.push(bits.read(8)? as u8);
            continue;
        }
        let n = if bits.read(1)? != 0 {
            2
        } else {
            bits.gamma()? + 2
        };
        if n == 258 {
            break;
        }
        if n > 257 {
            return Err(err(bits.pos / 8, "invalid match length"));
        }
        let high = if n == 2 || bits.read(1)? != 0 {
            0
        } else {
            bits.gamma()?
        };
        let distance = (high << 8) + bits.read(8)?;
        if distance == 0 || distance > out.len() {
            return Err(err(bits.pos / 8, "invalid back-reference"));
        }
        // The original decoder limits the last match to the output buffer.
        for _ in 0..n.min(expected - out.len()) {
            out.push(out[out.len() - distance]);
        }
    }
    if out.len() != expected {
        return Err(err(bits.pos / 8, "unpacked length mismatch"));
    }
    Ok(out)
}
fn text_stream(c: &mut Cursor<'_>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut total = None;
    loop {
        let at = c.pos;
        let plain = c.u16()? as usize;
        let packed = c.u16()? as usize;
        let _checksum = c.u16()?; // Not verified: the supplied checksum decompilation is incomplete.
        if plain == 0 || packed == 0 || packed > plain || plain > 65000 {
            return Err(err(at, "invalid text block sizes"));
        }
        let bytes = c.take(packed)?;
        if packed == plain {
            out.extend_from_slice(bytes);
        } else {
            out.extend(decompress_mode4(bytes, plain).map_err(|mut e| {
                e.offset += at + 6;
                e
            })?);
        }
        if total.is_none() {
            if out.len() < 4 {
                return Err(err(at, "missing total text length"));
            }
            let n = u32::from_le_bytes(out[..4].try_into().unwrap()) as usize;
            if n > MAX_FIELD {
                return Err(err(at, "text exceeds size limit"));
            }
            total = Some(n + 4);
        }
        let n = total.unwrap();
        if out.len() > n {
            return Err(err(at, "text exceeds declared length"));
        }
        if out.len() == n {
            return Ok(out.split_off(4));
        }
    }
}
// ZCmprBinField: FUN_0084cbb0 + FUN_0084d190. Framed zlib stream,
// each frame <=1024 bytes and a cumulative Adler-32, terminated by a short frame.
fn zcompressed(c: &mut Cursor<'_>) -> Result<Vec<u8>> {
    use std::io::Read;
    let at = c.pos;
    let expected = c.u32()? as usize;
    if expected > MAX_FIELD {
        return Err(err(at, "zlib output exceeds size limit"));
    }
    // Native empty ZCmprBinField ends at this length DWORD. No zlib frame
    // follows: the next bytes belong to the binary kind and file metadata.
    if expected == 0 {
        return Ok(Vec::new());
    }
    let mut packed = Vec::new();
    let (mut a, mut b) = (1u32, 0u32);
    loop {
        let n = c.u16()? as usize;
        let checksum = c.u32()?;
        if n > 1024 {
            return Err(err(c.pos - 6, "zlib frame exceeds 1024 bytes"));
        }
        if packed.len() + n > MAX_FIELD {
            return Err(err(at, "zlib input exceeds size limit"));
        }
        let frame = c.take(n)?;
        for byte in frame {
            a = (a + *byte as u32) % 65521;
            b = (b + a) % 65521;
        }
        if checksum != (b << 16) | a {
            return Err(err(c.pos - n - 4, "zlib frame checksum mismatch"));
        }
        packed.extend_from_slice(frame);
        if n < 1024 {
            break;
        }
    }
    let mut decoder = flate2::read::ZlibDecoder::new(packed.as_slice());
    let mut out = Vec::new();
    (&mut decoder)
        .take(expected as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| err(at, format!("zlib decode: {e}")))?;
    if out.len() != expected {
        return Err(err(at, "zlib decompressed length mismatch"));
    }
    if decoder.total_in() != packed.len() as u64 {
        return Err(err(at, "trailing bytes in zlib stream"));
    }
    Ok(out)
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMetadata {
    pub timestamp: u32,
    pub source: Vec<u8>,
    pub name: Vec<u8>,
}
fn file_metadata(c: &mut Cursor<'_>, kind: Option<u16>) -> Result<Option<FileMetadata>> {
    if kind != Some(0) {
        return Ok(None);
    }
    // Native 006296b1..0062975c: time, source (+58), filename (+54).
    // SBinField.FileName (0077538c) reads the second string, at +54.
    Ok(Some(FileMetadata {
        timestamp: c.u32()?,
        source: c.short_bytes()?,
        name: c.short_bytes()?,
    }))
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    String(Vec<u8>),
    Integer(u32),
    Date([u8; 4]),
    Timestamp([u8; 8]),
    Decimal([u8; 34]),
    Text(Vec<u8>),
    TextWithMetadata {
        data: Vec<u8>,
        metadata: Vec<u8>,
    },
    Binary {
        data: Vec<u8>,
        kind: Option<u16>,
        file: Option<FileMetadata>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub id: u16,
    pub wire_type: u16,
    pub value: Value,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub area: u16,
    pub flags: u16,
    pub fields: std::sync::Arc<Vec<Field>>,
}
/// A deliberately separate type: metadata must never enter the full-record
/// cache or be passed to the persistent writer as a complete document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordMetadata {
    pub area: u16,
    pub name: Option<Vec<u8>>,
}
impl From<&Record> for RecordMetadata {
    fn from(record: &Record) -> Self {
        Self {
            area: record.area,
            name: record.string(1).map(<[u8]>::to_vec),
        }
    }
}
impl Record {
    pub fn integer(&self, id: u16) -> Option<u32> {
        self.fields.iter().find_map(|f| {
            if f.id == id {
                if let Value::Integer(v) = f.value {
                    Some(v)
                } else {
                    None
                }
            } else {
                None
            }
        })
    }
    pub fn string(&self, id: u16) -> Option<&[u8]> {
        self.fields.iter().find_map(|f| {
            if f.id == id {
                if let Value::String(ref v) = f.value {
                    Some(v.as_slice())
                } else {
                    None
                }
            } else {
                None
            }
        })
    }
    pub fn code(&self) -> Option<&[u8]> {
        self.fields.iter().find_map(|f| match &f.value {
            Value::Binary {
                data,
                kind: Some(3),
                ..
            } if f.id == 3 => Some(data.as_slice()),
            _ => None,
        })
    }
    pub fn source(&self) -> Option<&[u8]> {
        self.fields.iter().find_map(|f| match &f.value {
            Value::Text(v) | Value::TextWithMetadata { data: v, .. } if f.id == 3 => {
                Some(v.as_slice())
            }
            _ => None,
        })
    }
    pub fn binary(&self, id: u16) -> Option<&[u8]> {
        self.fields.iter().find_map(|f| match &f.value {
            Value::Binary { data, .. } if f.id == id => Some(data.as_slice()),
            _ => None,
        })
    }
}
/// Input is the C-record payload AFTER its 7-byte framing header.
/// Its first six bytes are area/count/flags, not part of a field.
pub fn parse_record(payload: &[u8]) -> Result<Record> {
    let mut c = Cursor {
        data: payload,
        pos: 0,
    };
    let area = c.u16()?;
    let count = c.u16()? as usize;
    let flags = c.u16()?;
    // Native cases can contain tens of thousands of repeated participant links.
    // Count is a u16; the independent decoded-byte budget still bounds memory.
    if count == 0 {
        return Err(err(2, "invalid field count"));
    }
    let mut fields = Vec::with_capacity(count);
    let mut decoded_bytes = 0usize;
    for _ in 0..count {
        let at = c.pos;
        let wire_type = c.u16()?;
        let id = c.u16()?;
        let value = match wire_type {
            13 => Value::Decimal(c.take(34)?.try_into().unwrap()),
            14 => {
                let data = zcompressed(&mut c)?;
                let kind = c.u16()?;
                let file = file_metadata(&mut c, Some(kind))?;
                Value::Binary {
                    data,
                    kind: Some(kind),
                    file,
                }
            }
            12 => Value::Timestamp(c.take(8)?.try_into().unwrap()),
            1 => Value::Date(c.take(4)?.try_into().unwrap()),
            2 | 3 => Value::String(c.short_bytes()?),
            5 => Value::Integer(c.u32()?),
            4 => {
                let text = text_stream(&mut c)?;
                // FUN_0062dbe0: byte size of the following text-attributes stream.
                let n = c.u32()? as usize;
                if n > MAX_FIELD {
                    return Err(err(c.pos - 4, "text metadata exceeds size limit"));
                }
                let metadata = c.take(n)?.to_vec();
                if metadata.is_empty() {
                    Value::Text(text)
                } else {
                    Value::TextWithMetadata {
                        data: text,
                        metadata,
                    }
                }
            }
            8 => {
                let tagged = c.u32()?;
                let n = (tagged & 0x7fff_ffff) as usize;
                if n > MAX_FIELD {
                    return Err(err(c.pos - 4, "binary exceeds size limit"));
                }
                let data = c.take(n)?.to_vec();
                let kind = if tagged & 0x8000_0000 != 0 {
                    let k = c.u16()?;
                    Some(k)
                } else {
                    None
                };
                let file = file_metadata(&mut c, kind)?;
                Value::Binary { data, kind, file }
            }
            other => return Err(err(at, format!("unsupported field type {other}, id {id}"))),
        };
        decoded_bytes += match &value {
            Value::String(v) | Value::Text(v) => v.len(),
            Value::Binary { data, file, .. } => {
                data.len()
                    + file
                        .as_ref()
                        .map_or(0, |f| f.source.len() + f.name.len() + 4)
            }
            Value::Integer(_) | Value::Date(_) => 4,
            Value::Timestamp(_) => 8,
            Value::Decimal(_) => 34,
            Value::TextWithMetadata { data, metadata } => data.len() + metadata.len(),
        };
        if decoded_bytes > MAX_FIELD {
            return Err(err(at, "total decoded record size limit exceeded"));
        }
        fields.push(Field {
            id,
            wire_type,
            value,
        });
    }
    if c.pos != payload.len() {
        return Err(err(
            c.pos,
            format!("{} trailing bytes after fields", payload.len() - c.pos),
        ));
    }
    Ok(Record {
        area,
        flags,
        fields: fields.into(),
    })
}
/// Native records group scalar fields before streams; header.flags gives the
/// scalar count. Return None if a longer prefix is needed. Noncanonical old
/// layouts use the full decoder, never a guessed scan through stream bytes.
pub(super) fn scalar_prefix(
    payload: &[u8],
) -> std::result::Result<Option<(RecordMetadata, Vec<(u16, u32)>)>, String> {
    if payload.len() < 6 {
        return Ok(None);
    }
    let word = |p| u16::from_le_bytes([payload[p], payload[p + 1]]);
    let area = word(0);
    let count = word(2);
    let scalars = word(4);
    if count == 0 || scalars > count {
        return Err("noncanonical scalar header".into());
    }
    let mut pos = 6;
    let mut links = Vec::new();
    let mut name = None;
    for _ in 0..scalars {
        if pos + 4 > payload.len() {
            return Ok(None);
        }
        let kind = word(pos);
        let id = word(pos + 2);
        pos += 4;
        let length = match kind {
            1 | 5 => 4,
            12 => 8,
            13 => 34,
            2 | 3 => {
                if pos + 2 > payload.len() {
                    return Ok(None);
                }
                let n = word(pos) as usize;
                pos += 2;
                n
            }
            _ => return Err("noncanonical scalar order".into()),
        };
        if pos + length > payload.len() {
            return Ok(None);
        }
        if kind == 5 {
            links.push((
                id,
                u32::from_le_bytes(payload[pos..pos + 4].try_into().unwrap()),
            ));
        }
        if matches!(kind, 2 | 3) && id == 1 && name.is_none() {
            name = Some(payload[pos..pos + length].to_vec());
        }
        pos += length;
    }
    if scalars < count {
        if pos + 4 > payload.len() {
            return Ok(None);
        }
        if !matches!(word(pos), 4 | 8 | 14) {
            return Err("noncanonical stream boundary".into());
        }
    }
    Ok(Some((RecordMetadata { area, name }, links)))
}
