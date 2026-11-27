//! Legacy `666 Kodeks archive` container. No DOS executable is required.
//! Blocks use a CRC-16/0x1021 recurrence and Cx modes 4..7: LZ references,
//! optional ranked lengths, and adaptive Shannon–Fano symbol tables.
use super::Fault;
const MAGIC: &[u8] = b"666 Kodeks archive \x1a";
fn u16_at(b: &[u8], p: usize) -> Result<u16, Fault> {
    Ok(u16::from_le_bytes(
        b.get(p..p + 2)
            .ok_or("Обрезанный заголовок .pkg")?
            .try_into()
            .unwrap(),
    ))
}
fn u32_at(b: &[u8], p: usize) -> Result<u32, Fault> {
    Ok(u32::from_le_bytes(
        b.get(p..p + 4)
            .ok_or("Обрезанный заголовок .pkg")?
            .try_into()
            .unwrap(),
    ))
}
fn crc(bytes: &[u8]) -> u16 {
    let mut result = 0u16;
    for &byte in bytes {
        let mut upper = result & 0xff00;
        for _ in 0..8 {
            upper = if upper & 0x8000 != 0 {
                (upper << 1) ^ 0x1021
            } else {
                upper << 1
            };
        }
        result = upper ^ (result << 8) ^ u16::from(byte);
    }
    result
}
struct Bits<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl Bits<'_> {
    fn get(&mut self, count: usize) -> Result<usize, Fault> {
        if count > 16 || self.position + count > self.bytes.len() * 8 {
            return Err("Обрезанный сжатый блок .pkg".into());
        }
        let mut value = 0;
        for i in 0..count {
            value |= usize::from((self.bytes[self.position / 8] >> (self.position % 8)) & 1) << i;
            self.position += 1;
        }
        Ok(value)
    }
    fn number(&mut self) -> Result<usize, Fault> {
        if self.get(1)? != 0 {
            return Ok(0);
        }
        let mut bits = 1;
        while bits < 8 && self.get(1)? == 0 {
            bits += 1;
        }
        Ok(self.get(bits)? + (1 << bits) - 1)
    }
}
struct Model {
    frequencies: [usize; 257],
    symbols: [usize; 257],
    total: usize,
    count: usize,
    limit: usize,
}
impl Model {
    fn new(limit: usize) -> Self {
        Self {
            frequencies: [1; 257],
            symbols: std::array::from_fn(|i| i),
            total: 257,
            count: 0,
            limit,
        }
    }
    fn update(&mut self, mut index: usize, amount: usize) {
        self.total += amount;
        if self.total >= 65502 {
            for f in &mut self.frequencies {
                *f = f.div_ceil(2);
            }
            self.total = self.frequencies.iter().sum::<usize>() + amount;
        }
        let value = self.frequencies[index] + amount;
        let symbol = self.symbols[index];
        // Mode 5 increments by one and exchanges equal-rank symbols. Modes
        // 6/7 add 32 and stably insert the symbol into its new position.
        if amount == 1 {
            let old = index;
            while index > 0 && self.frequencies[index - 1] < value {
                index -= 1;
            }
            self.frequencies[old] = self.frequencies[index];
            self.symbols[old] = self.symbols[index];
        } else {
            while index > 0 && self.frequencies[index - 1] < value {
                self.frequencies[index] = self.frequencies[index - 1];
                self.symbols[index] = self.symbols[index - 1];
                index -= 1;
            }
        }
        self.frequencies[index] = value;
        self.symbols[index] = symbol;
    }
    fn decode(&mut self, bits: &mut Bits<'_>) -> Result<usize, Fault> {
        let mut start = 0;
        let mut weight = self.total;
        while self.frequencies[start] < weight {
            let mut middle = start;
            let mut left = 0;
            while left < weight / 2 {
                left += self.frequencies[middle];
                middle += 1;
            }
            if bits.get(1)? != 0 {
                start = middle;
                weight -= left;
            } else {
                weight = left;
            }
        }
        let symbol = self.symbols[start];
        if self.count < self.limit {
            self.update(start, 32);
        }
        self.count += 1;
        Ok(symbol)
    }
}
fn decompress(bytes: &[u8], expected: usize) -> Result<Vec<u8>, Fault> {
    let mode = bytes.first().ok_or("Пустой сжатый блок .pkg")? & 7;
    if !(4..=7).contains(&mode) {
        return Err(format!("Неизвестный режим сжатия .pkg: {mode}").into());
    }
    let mut bits = Bits { bytes, position: 3 };
    let mut lengths = Model::new(3000);
    let mut offsets = Model::new(5000);
    let mut literals = Model::new(3000);
    let mut out = Vec::with_capacity(expected);
    while out.len() < expected {
        if out.len() % 1024 == 0 {
            crate::execution::check()?;
        }
        let count = if mode <= 5 {
            if bits.get(1)? == 0 {
                out.push(bits.get(8)? as u8);
                continue;
            }
            let mut number = bits.number()?;
            if mode == 5 {
                let index = if number < 257 { number } else { 0 };
                number = lengths.symbols[index];
                lengths.update(index, 1);
            }
            number + 2
        } else {
            lengths.decode(&mut bits)? + 2
        };
        if count == 258 {
            break;
        }
        if count == 257 && mode >= 6 {
            let value = if mode == 7 {
                literals.decode(&mut bits)?
            } else {
                bits.get(8)?
            };
            if value > 255 {
                return Err("Некорректный литерал .pkg".into());
            }
            out.push(value as u8);
            continue;
        }
        if count > 257 {
            return Err("Некорректная длина ссылки .pkg".into());
        }
        let high = if count > 2 {
            if mode >= 6 {
                offsets.decode(&mut bits)?
            } else {
                bits.number()?
            }
        } else {
            0
        };
        let distance = high * 256 + bits.get(8)?;
        if distance == 0 || distance > out.len() {
            return Err("Ссылка за границы распакованного блока .pkg".into());
        }
        for _ in 0..count.min(expected - out.len()) {
            out.push(out[out.len() - distance]);
        }
    }
    if out.len() != expected {
        return Err("Размер распакованного блока .pkg не совпадает с заголовком".into());
    }
    Ok(out)
}
pub(super) fn decode(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, Fault> {
    if bytes.len() < 66 || !bytes.starts_with(MAGIC) {
        return Err("Неизвестный формат .pkg".into());
    }
    if bytes[38] != 1 || bytes[39] != 1 {
        return Err(
            "Нужен полный одиночный том .pkg; многотомный контейнер Packer не поддерживается"
                .into(),
        );
    }
    if u32_at(bytes, 34)? as usize != bytes.len() {
        return Err("Размер контейнера .pkg не совпадает с заголовком".into());
    }
    let mut pos = 66;
    let mut total = 0;
    let mut files = Vec::new();
    let mut names = std::collections::HashSet::new();
    loop {
        crate::execution::check()?;
        let header = bytes
            .get(pos..pos + 23)
            .ok_or("Обрезанный заголовок файла .pkg")?;
        pos += 23;
        if header.iter().all(|v| *v == 0) {
            if pos != bytes.len() {
                return Err("Лишние данные после конца .pkg".into());
            }
            break;
        }
        let size = u32_at(header, 6)? as usize;
        total = usize::checked_add(total, size).ok_or("Переполнение размера .pkg")?;
        if size > 64 * 1024 * 1024 || total > 512 * 1024 * 1024 || files.len() >= 10000 {
            return Err("Превышен размер XML/пакета .pkg".into());
        }
        let len = usize::from(header[22]);
        let name = bytes
            .get(pos..pos + len)
            .ok_or("Обрезанное имя файла .pkg")?;
        pos += len;
        if name.last() != Some(&0) {
            return Err("Неверное имя файла .pkg".into());
        }
        let name = std::str::from_utf8(&name[..name.len() - 1])
            .map_err(|_| "Некорректное имя XML .pkg")?
            .to_ascii_lowercase();
        if !super::archive::xml_name(&name) || !names.insert(name.clone()) {
            return Err(
                "Пакет .pkg должен содержать уникальные a_N.xml без вложенных путей".into(),
            );
        }
        let mut data = Vec::with_capacity(size);
        loop {
            crate::execution::check()?;
            let raw = u16_at(bytes, pos)? as usize;
            let packed = u16_at(bytes, pos + 2)? as usize;
            let check = u16_at(bytes, pos + 4)?;
            pos += 6;
            if raw == 0 && packed == 0 && check == 0 {
                break;
            }
            if raw == 0 || packed == 0 || data.len() + raw > size {
                return Err("Неверный размер блока .pkg".into());
            }
            let block = bytes.get(pos..pos + packed).ok_or("Обрезанный блок .pkg")?;
            pos += packed;
            if crc(block) != check {
                return Err("Контрольная сумма блока .pkg не совпадает".into());
            }
            if raw == packed {
                data.extend_from_slice(block);
            } else {
                data.extend(decompress(block, raw)?);
            }
        }
        if data.len() != size {
            return Err("Неполный XML в пакете .pkg".into());
        }
        files.push((name, data));
    }
    if files.is_empty() {
        return Err("В .pkg нет XML объектов".into());
    }
    Ok(files)
}
pub(super) fn encode(files: &[(String, Vec<u8>)]) -> Result<Vec<u8>, Fault> {
    let mut result = vec![0; 66];
    result[..MAGIC.len()].copy_from_slice(MAGIC);
    result[38] = 1;
    result[39] = 1;
    result[40..44].copy_from_slice(&[0xb8, 0x89, 0xb8, 0x89]);
    result[44] = 3;
    let mut total = 0usize;
    for (name, bytes) in files {
        if !super::archive::xml_name(name) || name.len() > 199 || bytes.len() > 64 * 1024 * 1024 {
            return Err("Недопустимый XML для .pkg".into());
        }
        total += bytes.len();
        if total > 512 * 1024 * 1024 {
            return Err("Пакет .pkg превышает 512 МиБ".into());
        }
        let mut header = [0u8; 23];
        header[0] = 0x20;
        header[6..10].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
        header[22] = (name.len() + 1) as u8;
        result.extend(header);
        result.extend(name.to_ascii_uppercase().bytes());
        result.push(0);
        for block in bytes.chunks(32768) {
            crate::execution::check()?;
            let length = (block.len() as u16).to_le_bytes();
            result.extend(length);
            result.extend(length);
            result.extend(crc(block).to_le_bytes());
            result.extend(block);
        }
        result.extend([0; 6]);
    }
    result.extend([0; 23]);
    let length = result.len() as u32;
    result[34..38].copy_from_slice(&length.to_le_bytes());
    result[46..50].copy_from_slice(&(total as u32).to_le_bytes());
    Ok(result)
}
