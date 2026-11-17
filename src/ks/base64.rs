//! KS Base64 strings carry legacy CP1251 bytes (classifier SQL and titles).
pub fn encode(text: &str) -> Result<String, String> {
    let (bytes, _, bad) = encoding_rs::WINDOWS_1251.encode(text);
    if bad || bytes.len() > 4 * 1024 * 1024 {
        return Err("ToBase64 input encoding/size".into());
    }
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for b in bytes.chunks(3) {
        let n = (b[0] as u32) << 16
            | (b.get(1).copied().unwrap_or(0) as u32) << 8
            | b.get(2).copied().unwrap_or(0) as u32;
        for i in 0..4 {
            out.push(if i > b.len() {
                '='
            } else {
                alphabet[((n >> (18 - 6 * i)) & 63) as usize] as char
            });
        }
    }
    Ok(out)
}
pub fn decode(text: &str) -> Result<String, String> {
    if text.len() > 6 * 1024 * 1024 {
        return Err("FromBase64 input size".into());
    }
    let input: Vec<_> = text.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    if input.len() % 4 != 0 {
        return Err("FromBase64 invalid length".into());
    }
    let mut out = Vec::new();
    for (index, b) in input.chunks_exact(4).enumerate() {
        let padding = b.iter().rev().take_while(|c| **c == b'=').count();
        if padding > 2 || padding != 0 && (index + 1) * 4 != input.len() {
            return Err("FromBase64 invalid padding".into());
        }
        let mut n = 0u32;
        for &c in &b[..4 - padding] {
            n = (n << 6)
                | match c {
                    b'A'..=b'Z' => c - b'A',
                    b'a'..=b'z' => c - b'a' + 26,
                    b'0'..=b'9' => c - b'0' + 52,
                    b'+' => 62,
                    b'/' => 63,
                    _ => return Err("FromBase64 invalid character".into()),
                } as u32;
        }
        if padding > 0 && n & ((1 << (padding * 2)) - 1) != 0 {
            return Err("FromBase64 nonzero padding bits".into());
        }
        n <<= padding * 6;
        for i in 0..3 - padding {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    Ok(crate::db::decode(&out))
}
