//! KS local-calendar dates. Native setters return this, unlike JavaScript.
#[derive(Debug, Clone)]
pub struct Date {
    pub millis: f64,
}
const DAY: i64 = 86_400_000;
fn days(y: i64, m: i64, d: i64) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = y.div_euclid(400);
    let yo = y - era * 400;
    let mp = m + if m > 2 { -3 } else { 9 };
    era * 146097 + yo * 365 + yo / 4 - yo / 100 + (153 * mp + 2) / 5 + d - 1 - 719468
}
fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yo = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut y = yo + era * 400;
    let doy = doe - (365 * yo + yo / 4 - yo / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    y += if m <= 2 { 1 } else { 0 };
    (y, m, d)
}
impl Date {
    pub fn from_text(text: &str) -> Result<Self, String> {
        let text = text.trim();
        // Registry scripts pass Date.parse strings such as "Oct 01, 2026".
        // Normalize this explicit native format before numeric calendar parsing.
        let words = text
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        if words.len() == 3 {
            if let Some(month) = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ]
            .iter()
            .position(|m| m.eq_ignore_ascii_case(words[0]))
            {
                return Self::from_text(&format!("{}.{}.{}", words[1], month + 1, words[2]));
            }
        }
        let p = text
            .split(|c: char| matches!(c, '.' | '/' | '-' | ':' | ' ' | 'T'))
            .filter(|s| !s.is_empty())
            .map(str::parse::<i64>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "invalid Date string")?;
        if !(3..=7).contains(&p.len()) {
            return Err("invalid Date string parts".into());
        }
        let (year, month, day) = if text.as_bytes().get(4) == Some(&b'-') {
            (p[0], p[1], p[2])
        } else {
            (p[2], p[1], p[0])
        };
        let mut parts = [year, month - 1, day, 0, 0, 0, 0];
        parts[3..p.len()].copy_from_slice(&p[3..]);
        let date = Self::from_parts(parts)?;
        date.check_calendar()?;
        if date.parts() != parts {
            return Err("invalid Date calendar string".into());
        }
        Ok(date)
    }
    pub fn gmt_string(&self) -> Result<String, String> {
        self.check_calendar()?;
        let utc = Self::from_number(self.millis + Self::offset()? as f64 * 60000.0)?;
        let p = utc.parts();
        let weekday =
            ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][utc.get("getDay")? as usize];
        let month = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ][p[1] as usize];
        Ok(format!(
            "{weekday}, {:02} {month} {:04} {:02}:{:02}:{:02} GMT",
            p[2], p[0], p[3], p[4], p[5]
        ))
    }
    pub fn from_parts(p: [i64; 7]) -> Result<Self, String> {
        if p.iter().any(|n| !(-10_000_000..=10_000_000).contains(n)) {
            return Err("Date component range".into());
        }
        let y = p[0] + p[1].div_euclid(12);
        let m = p[1].rem_euclid(12) + 1;
        Self::from_number(
            (days(y, m, 1) * DAY
                + (p[2] - 1) * DAY
                + p[3] * 3600000
                + p[4] * 60000
                + p[5] * 1000
                + p[6]) as f64,
        )
    }
    pub fn from_number(n: f64) -> Result<Self, String> {
        // Native 005e79f0 stores a numeric date without calendar conversion.
        // mp_jobs uses Date(Number.MAX_value) as its upper-bound sentinel.
        if !n.is_finite() {
            return Err("non-finite Date is not implemented".into());
        }
        Ok(Self { millis: n.trunc() })
    }
    fn check_calendar(&self) -> Result<(), String> {
        if self.millis < days(100, 1, 1) as f64 * DAY as f64
            || self.millis >= days(10000, 1, 1) as f64 * DAY as f64
        {
            return Err("Date calendar outside supported years 100..9999".into());
        }
        Ok(())
    }
    pub fn parts(&self) -> [i64; 7] {
        let n = self.millis as i64;
        let (y, m, d) = civil(n.div_euclid(DAY));
        let t = n.rem_euclid(DAY);
        [
            y,
            m - 1,
            d,
            t / 3600000,
            t / 60000 % 60,
            t / 1000 % 60,
            t % 1000,
        ]
    }
    pub fn now() -> Result<Self, String> {
        #[cfg(unix)]
        let p = unsafe {
            let t = libc::time(std::ptr::null_mut());
            let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
            if libc::localtime_r(&t, tm.as_mut_ptr()).is_null() {
                return Err("Date localtime failed".into());
            }
            let t = tm.assume_init();
            [
                t.tm_year as i64 + 1900,
                t.tm_mon as i64,
                t.tm_mday as i64,
                t.tm_hour as i64,
                t.tm_min as i64,
                t.tm_sec as i64,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| e.to_string())?
                    .subsec_millis() as i64,
            ]
        };
        #[cfg(windows)]
        let p = unsafe {
            #[repr(C)]
            struct SystemTime {
                year: u16,
                month: u16,
                weekday: u16,
                day: u16,
                hour: u16,
                minute: u16,
                second: u16,
                millis: u16,
            }
            #[link(name = "kernel32")]
            extern "system" {
                fn GetLocalTime(t: *mut SystemTime);
            }
            let mut t = std::mem::MaybeUninit::<SystemTime>::uninit();
            GetLocalTime(t.as_mut_ptr());
            let t = t.assume_init();
            [
                t.year as i64,
                t.month as i64 - 1,
                t.day as i64,
                t.hour as i64,
                t.minute as i64,
                t.second as i64,
                t.millis as i64,
            ]
        };
        Self::from_parts(p)
    }
    pub fn offset() -> Result<i64, String> {
        #[cfg(unix)]
        unsafe {
            let t = libc::time(std::ptr::null_mut());
            let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
            if libc::localtime_r(&t, tm.as_mut_ptr()).is_null() {
                return Err("timezone lookup failed".into());
            }
            Ok(-(tm.assume_init().tm_gmtoff as i64) / 60)
        }
        #[cfg(windows)]
        unsafe {
            #[repr(C)]
            struct Zone {
                bias: i32,
                standard_name: [u16; 32],
                standard_date: [u16; 8],
                standard_bias: i32,
                daylight_name: [u16; 32],
                daylight_date: [u16; 8],
                daylight_bias: i32,
            }
            #[link(name = "kernel32")]
            extern "system" {
                fn GetTimeZoneInformation(t: *mut Zone) -> u32;
            }
            let mut z = std::mem::MaybeUninit::<Zone>::uninit();
            let state = GetTimeZoneInformation(z.as_mut_ptr());
            if state == u32::MAX {
                return Err("timezone lookup failed".into());
            }
            let z = z.assume_init();
            Ok(z.bias as i64
                + if state == 2 {
                    z.daylight_bias as i64
                } else {
                    0
                })
        }
    }
    pub fn get(&self, name: &str) -> Result<f64, String> {
        if name == "getTime" || name == "valueOf" {
            return Ok(self.millis);
        }
        self.check_calendar()?;
        if name == "getTimezoneOffset" {
            return Ok(Self::offset()? as f64);
        }
        if let Some(n) = name.strip_prefix("getUTC") {
            let utc = Self::from_number(self.millis + Self::offset()? as f64 * 60000.0)?;
            return utc.get(&format!("get{n}"));
        }
        let p = self.parts();
        Ok(match name {
            "getTime" | "valueOf" => self.millis,
            "getFullYear" => p[0] as f64,
            "getMonth" => p[1] as f64,
            "getDate" => p[2] as f64,
            "getHours" => p[3] as f64,
            "getMinutes" => p[4] as f64,
            "getSeconds" => p[5] as f64,
            "getMilliseconds" => p[6] as f64,
            "getDay" => ((self.millis as i64).div_euclid(DAY) + 4).rem_euclid(7) as f64,
            _ => return Err(format!("Date.{name} not implemented")),
        })
    }
    pub fn set(&mut self, name: &str, args: &[f64]) -> Result<(), String> {
        if let Some(n) = name.strip_prefix("setUTC") {
            let offset = Self::offset()? as f64 * 60000.0;
            let mut utc = Self::from_number(self.millis + offset)?;
            utc.set(&format!("set{n}"), args)?;
            *self = Self::from_number(utc.millis - offset)?;
            return Ok(());
        }
        if name == "setTime" {
            if args.len() != 1 {
                return Err("setTime requires one argument".into());
            }
            *self = Self::from_number(args[0])?;
            return Ok(());
        }
        self.check_calendar()?;
        let (start, max) = match name {
            "setFullYear" => (0, 3),
            "setMonth" => (1, 2),
            // Native KS also accepts hours, minutes, seconds, milliseconds.
            // ~brasExchangeTasks uses this to find tomorrow's maintenance time.
            "setDate" => (2, 5),
            "setHours" => (3, 4),
            "setMinutes" => (4, 3),
            "setSeconds" => (5, 2),
            "setMilliseconds" => (6, 1),
            _ => return Err(format!("Date.{name} not implemented")),
        };
        if args.is_empty()
            || args.len() > max
            || args
                .iter()
                .any(|n| !n.is_finite() || n.abs() > 10_000_000.0)
        {
            return Err("invalid Date setter arguments".into());
        }
        let mut p = self.parts();
        for (i, n) in args.iter().enumerate() {
            p[start + i] = n.trunc() as i64;
        }
        *self = Self::from_parts(p)?;
        Ok(())
    }
}
