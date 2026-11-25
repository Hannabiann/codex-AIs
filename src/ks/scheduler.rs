use std::collections::BTreeMap;
#[derive(Debug, Clone)]
struct Daily {
    minute: u16,
    last_day: i64,
}
#[derive(Debug, Default, Clone)]
pub struct Scheduler {
    daily: BTreeMap<String, Daily>,
    once: Vec<(u64, String)>,
}
pub enum When {
    Delay(u32),
    Daily(u16),
    Delete,
}
pub fn parse_time(s: &str) -> Option<When> {
    if s == "deletetask" {
        return Some(When::Delete);
    }
    let v: Vec<_> = s.split(':').collect();
    if !(v.len() == 2 || v.len() == 3)
        || v.iter()
            .any(|s| s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    let h = v[0].parse::<u16>().ok()?;
    let m = v[1].parse::<u16>().ok()?;
    if h >= 24 || m >= 60 || v.len() == 3 && v[2].parse::<u32>().ok()? != 0 {
        return None;
    }
    Some(When::Daily(h * 60 + m))
}
impl Scheduler {
    pub fn snapshot(&self) -> Vec<String> {
        self.daily
            .iter()
            .map(|(p, d)| {
                format!(
                    "{} — ежедневно {:02}:{:02}, местное время",
                    crate::monitor::safe_url(p),
                    d.minute / 60,
                    d.minute % 60
                )
            })
            .chain(
                self.once
                    .iter()
                    .map(|(t, p)| format!("{} — Unix UTC {t}", crate::monitor::safe_url(p))),
            )
            .collect()
    }
    pub fn schedule(
        &mut self,
        path: String,
        when: When,
        now: u64,
        day: i64,
        minute: u16,
    ) -> Result<(), String> {
        if path.len() > 4096 || path.chars().any(char::is_control) {
            return Err("invalid At path".into());
        }
        if matches!(when, When::Delete) {
            self.daily.remove(&path);
            return Ok(());
        }
        if self.daily.len() + self.once.len() >= 1024 {
            return Err("At task limit".into());
        }
        match when {
            When::Delay(s) => self.once.push((
                now.checked_add(s as u64).ok_or("At deadline overflow")?,
                path,
            )),
            When::Daily(m) => {
                self.daily.insert(
                    path,
                    Daily {
                        minute: m,
                        last_day: if minute >= m { day } else { day - 1 },
                    },
                );
            }
            When::Delete => unreachable!(),
        }
        Ok(())
    }
    pub fn due(&mut self, now: u64, day: i64, minute: u16) -> Vec<String> {
        let mut out = Vec::new();
        self.once.retain(|(t, p)| {
            if *t <= now {
                out.push(p.clone());
                false
            } else {
                true
            }
        });
        for (p, t) in &mut self.daily {
            if day > t.last_day && minute >= t.minute {
                t.last_day = day;
                out.push(p.clone());
            }
        }
        out
    }
}
pub fn clock() -> Result<(u64, i64, u16), String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    #[cfg(unix)]
    let (y, m, d, h, min) = unsafe {
        let t = libc::time_t::try_from(now).map_err(|_| "clock overflow")?;
        let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
        if libc::localtime_r(&t, tm.as_mut_ptr()).is_null() {
            return Err("localtime failed".into());
        }
        let t = tm.assume_init();
        (
            t.tm_year + 1900,
            t.tm_mon + 1,
            t.tm_mday,
            t.tm_hour,
            t.tm_min,
        )
    };
    #[cfg(windows)]
    let (y, m, d, h, min) = unsafe {
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
        (
            t.year as i32,
            t.month as i32,
            t.day as i32,
            t.hour as i32,
            t.minute as i32,
        )
    };
    Ok((
        now,
        y as i64 * 372 + m as i64 * 31 + d as i64,
        (h * 60 + min) as u16,
    ))
}
