//! Select the KS pool once at startup; never change affinity or resize a live pool.
use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Auto,
    Manual(usize),
}
impl Mode {
    pub fn parse(value: &str) -> Result<Self, String> {
        if value.trim().eq_ignore_ascii_case("auto") {
            return Ok(Self::Auto);
        }
        match value.parse::<usize>() {
            Ok(n @ 1..=32) => Ok(Self::Manual(n)),
            _ => Err("Исполнители KS/DB: укажите auto или целое число 1–32".into()),
        }
    }
    pub fn resolve(self) -> Selection {
        match self {
            Self::Manual(workers) => Selection {
                workers,
                explanation: format!("Ручная настройка workers={workers}. Она имеет приоритет; автоматический подбор не применяется."),
            },
            Self::Auto => select_auto(CpuInfo::detect()),
        }
    }
}
impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => f.write_str("auto"),
            Self::Manual(n) => write!(f, "{n}"),
        }
    }
}
#[derive(Clone, Debug)]
pub struct Selection {
    pub workers: usize,
    pub explanation: String,
}
#[derive(Debug)]
struct CpuInfo {
    logical: Option<usize>,
    cores: Option<usize>,
    source: String,
}
fn select_auto(info: CpuInfo) -> Selection {
    let logical = info.logical.filter(|n| *n > 0);
    let cores = info.cores.filter(|n| *n > 0);
    let estimated = cores.is_none();
    let effective = cores
        .or_else(|| logical.map(|n| n.div_ceil(2)))
        .unwrap_or(1)
        .min(logical.unwrap_or(usize::MAX));
    let workers = match effective {
        0..=2 => 3,
        3..=5 => 4,
        _ => 6,
    };
    let topology = if estimated {
        if logical.is_some() {
            "Топология не определена; оценка: два логических CPU на ядро."
        } else {
            "Сведения о CPU недоступны; принят минимальный пул."
        }
    } else {
        "Учтены ядра и доступный параллелизм."
    };
    Selection {
        workers,
        explanation: format!(
            "Автоматически: {workers} {} всего. Доступный параллелизм CPU: {}; ядер для расчёта: {effective}. {topology} {} Правило: 1–2 ядра → 3 исполнителя, 3–5 → 4, от 6 → 6. Выбор сделан при запуске; память и текущая загрузка не определяют размер пула.",
            if workers == 6 { "исполнителей" } else { "исполнителя" },
            logical.map(|n| n.to_string()).unwrap_or_else(|| "не определён".into()),
            info.source,
        ),
    }
}
impl CpuInfo {
    fn detect() -> Self {
        let mut info = Self {
            logical: std::thread::available_parallelism().ok().map(|n| n.get()),
            cores: None,
            source: "Источник: оценка доступного параллелизма операционной системы.".into(),
        };
        platform(&mut info);
        info
    }
    #[allow(dead_code)]
    fn restrict_logical(&mut self, count: usize) {
        if count > 0 {
            self.logical = Some(self.logical.map_or(count, |n| n.min(count)));
        }
    }
}

#[cfg(windows)]
fn platform(info: &mut CpuInfo) {
    use std::ffi::c_void;
    #[repr(C)]
    #[derive(Default)]
    struct GroupAffinity {
        mask: usize,
        group: u16,
        reserved: [u16; 3],
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn GetCurrentThread() -> *mut c_void;
        fn GetProcessAffinityMask(
            process: *mut c_void,
            mask: *mut usize,
            system: *mut usize,
        ) -> i32;
        fn GetThreadGroupAffinity(thread: *mut c_void, affinity: *mut GroupAffinity) -> i32;
        fn GetLogicalProcessorInformationEx(
            kind: i32,
            buffer: *mut c_void,
            length: *mut u32,
        ) -> i32;
        fn GetActiveProcessorCount(group: u16) -> u32;
        fn QueryInformationJobObject(
            job: *mut c_void,
            class: i32,
            data: *mut c_void,
            length: u32,
            returned: *mut u32,
        ) -> i32;
    }
    let mut affinity = GroupAffinity::default();
    let (mut process, mut system) = (0usize, 0usize);
    // Current-process/current-thread pseudo-handles require no CloseHandle.
    let known = unsafe {
        GetThreadGroupAffinity(GetCurrentThread(), &mut affinity) != 0
            && GetProcessAffinityMask(GetCurrentProcess(), &mut process, &mut system) != 0
    };
    if known && process & affinity.mask != 0 {
        let allowed = (process & affinity.mask) as u64;
        info.restrict_logical(allowed.count_ones() as usize);
        info.source = format!(
            "Источник: Windows, affinity процесса и потока, группа {}.",
            affinity.group
        );
        let mut length = 0u32;
        unsafe {
            GetLogicalProcessorInformationEx(0, std::ptr::null_mut(), &mut length);
        }
        // Aligned storage for the variable-sized Windows structures; decode
        // fields from bytes instead of dereferencing packed native records.
        if (32..=4 * 1024 * 1024).contains(&length) {
            let mut buffer = vec![0u64; (length as usize).div_ceil(8)];
            let capacity = buffer.len() * 8;
            if unsafe {
                GetLogicalProcessorInformationEx(0, buffer.as_mut_ptr().cast(), &mut length)
            } != 0
                && length as usize <= capacity
            {
                let bytes = unsafe {
                    std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), length as usize)
                };
                info.cores =
                    windows_cores(bytes, std::mem::size_of::<usize>(), affinity.group, allowed);
            }
        }
    }
    // An immediate job's hard CPU cap can further reduce the estimate. Parent
    // job limits and hypervisor scheduling guarantees are not discoverable here.
    let mut rate = [0u32; 2];
    if unsafe {
        QueryInformationJobObject(
            std::ptr::null_mut(),
            15,
            rate.as_mut_ptr().cast(),
            8,
            std::ptr::null_mut(),
        )
    } != 0
    {
        let percent = if rate[0] & 5 == 5 {
            Some(rate[1])
        } else if rate[0] & 17 == 17 {
            Some(rate[1] >> 16)
        } else {
            None
        };
        if let Some(percent @ 1..=10000) = percent {
            let total = unsafe { GetActiveProcessorCount(u16::MAX) } as usize;
            if total > 0 {
                let limit = ((total as u64 * percent as u64) / 10000).max(1) as usize;
                info.restrict_logical(limit);
                info.source
                    .push_str(&format!(" Учтён CPU-лимит текущего Job Object: {limit}."));
            }
        }
    }
}

#[cfg(any(windows, test))]
fn windows_cores(bytes: &[u8], word: usize, group: u16, allowed: u64) -> Option<usize> {
    if !matches!(word, 4 | 8) || allowed == 0 {
        return None;
    }
    let (mut offset, mut cores, mut covered) = (0usize, 0usize, 0u64);
    while offset < bytes.len() {
        let header = bytes.get(offset..offset.checked_add(8)?)?;
        let size = u32::from_le_bytes(header[4..8].try_into().ok()?) as usize;
        if size < 8 {
            return None;
        }
        let record = bytes.get(offset..offset.checked_add(size)?)?;
        if u32::from_le_bytes(header[..4].try_into().ok()?) == 0 {
            let groups = u16::from_le_bytes(record.get(30..32)?.try_into().ok()?);
            let mut matching = 0;
            for i in 0..groups as usize {
                let base = 32 + i * (word + 8);
                let mask = if word == 8 {
                    u64::from_le_bytes(record.get(base..base + 8)?.try_into().ok()?)
                } else {
                    u32::from_le_bytes(record.get(base..base + 4)?.try_into().ok()?) as u64
                };
                let id =
                    u16::from_le_bytes(record.get(base + word..base + word + 2)?.try_into().ok()?);
                if id == group {
                    matching |= mask & allowed;
                }
            }
            if matching != 0 {
                cores += 1;
                covered |= matching;
            }
        }
        offset += size;
    }
    (cores > 0 && covered == allowed).then_some(cores)
}

#[cfg(any(target_os = "linux", test))]
fn cpu_list(text: &str) -> Option<std::collections::BTreeSet<usize>> {
    let mut ids = std::collections::BTreeSet::new();
    for item in text.trim().split(',') {
        let (first, last) = item.split_once('-').unwrap_or((item, item));
        let (first, last) = (first.parse::<usize>().ok()?, last.parse::<usize>().ok()?);
        if first > last || last >= 65536 {
            return None;
        }
        ids.extend(first..=last);
    }
    (!ids.is_empty()).then_some(ids)
}

#[cfg(target_os = "linux")]
fn platform(info: &mut CpuInfo) {
    let result = (|| {
        let status = std::fs::read_to_string("/proc/thread-self/status").ok()?;
        let allowed = cpu_list(
            status
                .lines()
                .find_map(|s| s.strip_prefix("Cpus_allowed_list:"))?
                .trim(),
        )?;
        info.restrict_logical(allowed.len());
        info.source =
            "Источник: доступный параллелизм Rust и Cpus_allowed_list Linux; топология sysfs."
                .into();
        let mut cores = std::collections::BTreeSet::new();
        for cpu in allowed {
            let base = format!("/sys/devices/system/cpu/cpu{cpu}/topology");
            let siblings = std::fs::read_to_string(format!("{base}/core_cpus_list"))
                .or_else(|_| std::fs::read_to_string(format!("{base}/thread_siblings_list")))
                .ok()?;
            let siblings = cpu_list(&siblings)?;
            if !siblings.contains(&cpu) {
                return None;
            }
            cores.insert(siblings);
        }
        Some(cores.len())
    })();
    info.cores = result;
}

#[cfg(target_os = "freebsd")]
fn platform(info: &mut CpuInfo) {
    fn sysctl(name: &std::ffi::CStr) -> Option<usize> {
        let mut value = 0i32;
        let mut size = std::mem::size_of_val(&value);
        let ok = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                (&mut value as *mut i32).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        (ok == 0 && size == std::mem::size_of_val(&value) && value > 0).then_some(value as usize)
    }
    let mut set: libc::cpuset_t = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::cpuset_getaffinity(
            libc::CPU_LEVEL_WHICH,
            libc::CPU_WHICH_PID,
            -1,
            std::mem::size_of_val(&set),
            &mut set,
        )
    } == 0
    {
        let count = (0..std::mem::size_of_val(&set) * 8)
            .filter(|&i| unsafe { libc::CPU_ISSET(i, &set) })
            .count();
        info.restrict_logical(count);
    }
    if let (Some(cores), Some(threads), Some(logical)) = (
        sysctl(c"kern.smp.cores"),
        sysctl(c"kern.smp.threads_per_core"),
        info.logical,
    ) {
        // Conservative estimate when a cpuset includes only some SMT siblings.
        info.cores = Some(cores.min(logical.div_ceil(threads)));
        info.source =
            "Источник: FreeBSD cpuset и sysctl; консервативная оценка ядер по числу SMT-потоков."
                .into();
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "freebsd")))]
fn platform(_: &mut CpuInfo) {}
