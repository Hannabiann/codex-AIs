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
        fn unpack<const N: usize>(src: &[u8]) -> [u8; N] {
            std::array::from_fn(|i| if i == N - 1 { src[i] } else { src[i ^ 1] })
        }
        let m = unpack::<25>(&bytes[..26]);
        let e = unpack::<7>(&bytes[26..]);
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
        let mut magnitude: [u8; 24] = m[1..].try_into().unwrap();
        if m[0] == 255 {
            let mut carry = 1u16;
            for b in magnitude.iter_mut().rev() {
                let n = (!*b) as u16 + carry;
                *b = n as u8;
                carry = n >> 8;
            }
        }
        if magnitude.iter().all(|&byte| byte == 0) {
            return Ok(Self {
                coefficient: 0,
                scale: 0,
            });
        }
        if exponent <= 127 {
            // Round (magnitude * 10^scale) / 2^(192-exponent) directly.
            // Five binary limbs cover the complete 192-bit mantissa and all
            // 28 decimal places. No floating point or intermediate rounding.
            let mut limbs = [0u64; 5];
            for (i, chunk) in magnitude.rchunks_exact(8).enumerate() {
                limbs[i] = u64::from_be_bytes(chunk.try_into().unwrap());
            }
            fn shifted(limbs: &[u64; 5], shift: usize) -> u128 {
                let word = shift / 64;
                let bit = shift % 64;
                let get = |n| limbs.get(n).copied().unwrap_or(0);
                let part = |n| {
                    (get(n) >> bit)
                        | if bit == 0 {
                            0
                        } else {
                            get(n + 1) << (64 - bit)
                        }
                };
                part(word) as u128 | ((part(word + 1) as u128) << 64)
            }
            let shift = (192 - exponent) as usize;
            let mut integer = shifted(&limbs, shift);
            let mut integer_digits = 0u32;
            while integer != 0 {
                integer /= 10;
                integer_digits += 1;
            }
            let scale = 28.min(38u32.saturating_sub(integer_digits));
            for _ in 0..scale {
                let mut carry = 0u128;
                for limb in &mut limbs {
                    let product = *limb as u128 * 10 + carry;
                    *limb = product as u64;
                    carry = product >> 64;
                }
                debug_assert_eq!(carry, 0);
            }
            let round =
                (limbs.get((shift - 1) / 64).copied().unwrap_or(0) >> ((shift - 1) % 64)) & 1;
            let coefficient = shifted(&limbs, shift)
                .checked_add(round as u128)
                .and_then(|n| i128::try_from(n).ok())
                .ok_or("LongDouble precision overflow")?;
            return Ok(Self {
                coefficient: if m[0] == 255 {
                    -coefficient
                } else {
                    coefficient
                },
                scale,
            }
            .normalize());
        }
        // Keep the general conversion for non-normalized values whose binary
        // exponent is larger than the integer range handled above.
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
            "*" => self.multiply(other)?,
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
    fn multiply(self, other: Self) -> Result<Self, String> {
        if let Some(coefficient) = self.coefficient.checked_mul(other.coefficient) {
            let value = Self {
                coefficient,
                scale: self.scale + other.scale,
            }
            .normalize();
            if value.scale <= 28 {
                return Ok(value);
            }
        }
        // Keep the complete intermediate product: fractional coefficients can
        // overflow i128 even when the value (for example a probability) is small.
        let a = self.coefficient.unsigned_abs().to_string();
        let b = other.coefficient.unsigned_abs().to_string();
        let mut digits = vec![0u32; a.len() + b.len()];
        for (i, x) in a.bytes().rev().enumerate() {
            for (j, y) in b.bytes().rev().enumerate() {
                digits[i + j] += u32::from(x - b'0') * u32::from(y - b'0');
            }
        }
        for i in 0..digits.len() - 1 {
            digits[i + 1] += digits[i] / 10;
            digits[i] %= 10;
        }
        while digits.len() > 1 && digits.last() == Some(&0) {
            digits.pop();
        }
        let scale = self.scale + other.scale;
        let drop = scale
            .saturating_sub(28)
            .max(digits.len().saturating_sub(38) as u32);
        if drop > scale {
            return Err("LongDouble precision overflow".into());
        }
        let mut coefficient = 0i128;
        for &digit in digits.iter().skip(drop as usize).rev() {
            coefficient = coefficient
                .checked_mul(10)
                .and_then(|n| n.checked_add(digit as i128))
                .ok_or("LongDouble precision overflow")?;
        }
        if drop > 0 && digits.get(drop as usize - 1).copied().unwrap_or(0) >= 5 {
            coefficient = coefficient
                .checked_add(1)
                .ok_or("LongDouble precision overflow")?;
        }
        if (self.coefficient < 0) != (other.coefficient < 0) {
            coefficient = -coefficient;
        }
        Ok(Self {
            coefficient,
            scale: scale - drop,
        }
        .normalize())
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
