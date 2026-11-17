//! Decimal arithmetic for KS LongDouble report totals. No binary floating point
//! is used for money. Values exceeding the supported precision fail explicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decimal {
    pub coefficient: i128,
    scale: u32,
}
impl Decimal {
    /// Native 192-bit binary fraction, rounded without a floating-point step.
    pub fn to_native(self) -> [u8; 34] {
        let mut fraction = [0u8; 25];
        let mut exponent = 0i64;
        let mut numerator = self.coefficient.unsigned_abs();
        let mut denominator = 10u128.pow(self.scale);
        if numerator != 0 {
            while numerator >= denominator {
                denominator *= 2;
                exponent += 1;
            }
            while numerator < denominator / 2 + denominator % 2 {
                numerator *= 2;
                exponent -= 1;
            }
            for bit in 0..192 {
                if numerator >= denominator - numerator {
                    numerator -= denominator - numerator;
                    fraction[1 + bit / 8] |= 128 >> (bit % 8);
                } else {
                    numerator *= 2;
                }
            }
            if numerator >= denominator - numerator {
                let mut carry = true;
                for b in fraction[1..].iter_mut().rev() {
                    if !carry {
                        break;
                    }
                    (*b, carry) = b.overflowing_add(1);
                }
                if carry {
                    fraction[1] = 128;
                    exponent += 1;
                }
            }
            if self.coefficient < 0 {
                let mut carry = 1u16;
                for b in fraction.iter_mut().rev() {
                    let n = (!*b) as u16 + carry;
                    *b = n as u8;
                    carry = n >> 8;
                }
            }
        }
        let e = exponent.to_be_bytes();
        let mut out = [0u8; 34];
        for (src, offset) in [(&fraction[..], 0), (&e[1..], 26)] {
            for i in 0..src.len() {
                out[offset + if i == src.len() - 1 { i } else { i ^ 1 }] = src[i];
            }
        }
        out
    }
    // 005d8610/005d8890: 25-byte signed binary fraction and 7-byte
    // signed binary exponent, each serialized as little-endian 16-bit units.
    pub fn from_native(bytes: &[u8; 34]) -> Result<Self, String> {
        fn unpack(src: &[u8], n: usize) -> Vec<u8> {
            (0..n)
                .map(|i| if i == n - 1 { src[i] } else { src[i ^ 1] })
                .collect()
        }
        let m = unpack(&bytes[..26], 25);
        let e = unpack(&bytes[26..], 7);
        if !matches!(m[0], 0 | 255) || !matches!(e[0], 0 | 255) {
            return Err("invalid native LongDouble sign".into());
        }
        let mut exponent = 0i64;
        for b in &e[1..] {
            exponent = (exponent << 8) | *b as i64;
        }
        if e[0] == 255 {
            exponent -= 1i64 << 48;
        }
        if !(-512..=512).contains(&exponent) {
            return Err("native LongDouble exponent exceeds supported precision".into());
        }
        let mut magnitude = m[1..].to_vec();
        if m[0] == 255 {
            let mut carry = 1u16;
            for b in magnitude.iter_mut().rev() {
                let n = (!*b) as u16 + carry;
                *b = n as u8;
                carry = n >> 8;
            }
        }
        fn mul(d: &mut Vec<u8>, n: u32, mut carry: u32) {
            for v in d.iter_mut() {
                let z = *v as u32 * n + carry;
                *v = (z % 10) as u8;
                carry = z / 10;
            }
            while carry > 0 {
                d.push((carry % 10) as u8);
                carry /= 10;
            }
        }
        let mut digits = vec![0u8];
        for b in magnitude {
            mul(&mut digits, 256, b as u32);
        }
        if digits.iter().all(|&d| d == 0) {
            return Self::parse("0");
        }
        let shift = exponent - 192;
        let mut scale = if shift < 0 { (-shift) as usize } else { 0 };
        for _ in 0..shift.unsigned_abs() {
            mul(&mut digits, if shift < 0 { 5 } else { 2 }, 0);
        }
        while scale > 0 && digits.first() == Some(&0) {
            digits.remove(0);
            scale -= 1;
        }
        let precision = 28usize.min(38usize.saturating_sub(digits.len().saturating_sub(scale)));
        if scale > precision {
            let drop = scale - precision;
            let round = digits.get(drop - 1).copied().unwrap_or(0) >= 5;
            digits = digits.get(drop..).unwrap_or(&[]).to_vec();
            if digits.is_empty() {
                digits.push(0);
            }
            if round {
                mul(&mut digits, 1, 1);
            }
            scale = precision;
        }
        while digits.len() > 1 && digits.last() == Some(&0) {
            digits.pop();
        }
        while digits.len() <= scale {
            digits.push(0);
        }
        let mut text = digits
            .iter()
            .rev()
            .map(|&n| (b'0' + n) as char)
            .collect::<String>();
        if scale > 0 {
            text.insert(text.len() - scale, '.');
        }
        if m[0] == 255 {
            text.insert(0, '-');
        }
        Self::parse(&text)
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        // Native decimal conversion initializes an empty/undefined scalar to zero.
        if s.is_empty() {
            return Ok(Self {
                coefficient: 0,
                scale: 0,
            });
        }
        let (s, negative) = if let Some(s) = s.strip_prefix('-') {
            (s, true)
        } else {
            (s.strip_prefix('+').unwrap_or(s), false)
        };
        let mut coefficient = 0i128;
        let mut scale = 0;
        let mut fraction = false;
        let mut digits = 0;
        for c in s.chars() {
            if c == '.' && !fraction {
                fraction = true;
                continue;
            }
            let d = c.to_digit(10).ok_or("invalid LongDouble")?;
            coefficient = coefficient
                .checked_mul(10)
                .and_then(|n| n.checked_add(d as i128))
                .ok_or("LongDouble precision overflow")?;
            if fraction {
                scale += 1;
            }
            digits += 1;
        }
        if digits == 0 || scale > 28 {
            return Err("invalid LongDouble precision".into());
        }
        Ok(Self {
            coefficient: if negative { -coefficient } else { coefficient },
            scale,
        }
        .normalize())
    }
    fn normalize(mut self) -> Self {
        while self.scale > 0 && self.coefficient % 10 == 0 {
            self.coefficient /= 10;
            self.scale -= 1;
        }
        self
    }
    fn at(self, scale: u32) -> Result<i128, String> {
        self.coefficient
            .checked_mul(
                10i128
                    .checked_pow(scale - self.scale)
                    .ok_or("LongDouble precision overflow")?,
            )
            .ok_or_else(|| "LongDouble precision overflow".into())
    }
    pub fn cmp(self, other: Self) -> Result<std::cmp::Ordering, String> {
        let scale = self.scale.max(other.scale);
        Ok(self.at(scale)?.cmp(&other.at(scale)?))
    }
    pub fn op(self, op: &str, other: Self) -> Result<Self, String> {
        let overflow = || "LongDouble precision overflow".to_string();
        let result = match op {
            "+" | "-" | "%" => {
                let scale = self.scale.max(other.scale);
                let a = self.at(scale)?;
                let b = other.at(scale)?;
                let coefficient = match op {
                    "+" => a.checked_add(b),
                    "-" => a.checked_sub(b),
                    _ => a.checked_rem(b),
                }
                .ok_or_else(overflow)?;
                Self { coefficient, scale }
            }
            "*" => Self {
                coefficient: self
                    .coefficient
                    .checked_mul(other.coefficient)
                    .ok_or_else(overflow)?,
                scale: self.scale + other.scale,
            },
            "/" => {
                if other.coefficient == 0 {
                    return Err("LongDouble division by zero".into());
                }
                let mut scale = 18u32.max(self.scale.saturating_sub(other.scale));
                let coefficient = loop {
                    let power = scale + other.scale - self.scale;
                    if let Some(a) = 10i128
                        .checked_pow(power)
                        .and_then(|p| self.coefficient.checked_mul(p))
                    {
                        break a / other.coefficient;
                    }
                    if scale == 0 || scale + other.scale == self.scale {
                        return Err(overflow());
                    }
                    scale -= 1;
                };
                Self { coefficient, scale }
            }
            _ => return Err("unsupported LongDouble operator".into()),
        };
        let result = result.normalize();
        if result.scale > 28 {
            return Err(overflow());
        }
        Ok(result)
    }
    pub fn formatted(self, places: u32) -> Result<String, String> {
        if places > 28 {
            return Err("LongDouble decimal places limit".into());
        }
        let coefficient = if places >= self.scale {
            self.at(places)?
        } else {
            let divisor = 10i128.pow(self.scale - places);
            let half = divisor / 2;
            self.coefficient
                .checked_add(if self.coefficient < 0 { -half } else { half })
                .ok_or("LongDouble rounding overflow")?
                / divisor
        };
        let negative = coefficient < 0;
        let digits = coefficient.unsigned_abs().to_string();
        let digits = format!("{:0>width$}", digits, width = places as usize + 1);
        let at = digits.len() - places as usize;
        Ok(format!(
            "{}{}{}{}",
            if negative { "-" } else { "" },
            &digits[..at],
            if places > 0 { "." } else { "" },
            &digits[at..]
        ))
    }
    pub fn text(self) -> String {
        self.formatted(self.scale)
            .expect("existing decimal is representable")
    }
}
