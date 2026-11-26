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
    pub fn resolve(self, interface_workers: usize) -> Selection {
        match self {
            Self::Manual(workers) => Selection {
                workers,
                explanation: format!("Ручная настройка workers={workers}. Она имеет приоритет; автоматический подбор не применяется."),
            },
            Self::Auto => select_auto(CpuInfo::detect(), interface_workers),
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
fn select_auto(info: CpuInfo, interface_workers: usize) -> Selection {
    // Settings validates 1..=8 before resolving the pool. Preserve the saved
    // interface count and reserve one owner and one search worker even on one CPU.
    assert!((1..=8).contains(&interface_workers));
    let logical = info.logical.filter(|n| *n > 0);
    let cores = info.cores.filter(|n| *n > 0);
    let estimated = cores.is_none();
    let effective = cores
        .or_else(|| logical.map(|n| n.div_ceil(2)))
        .unwrap_or(1)
        .min(logical.unwrap_or(usize::MAX));
    let reserved = interface_workers + 2;
    let heavy = effective.min(32 - reserved);
    let workers = heavy + reserved;
    let topology = if estimated {
        if logical.is_some() {
            "Топология не определена; оценка: два логических CPU на ядро."
        } else {
            "Сведения о CPU недоступны; принят один тяжёлый исполнитель."
        }
    } else {
        "Учтены физические ядра и доступный параллелизм; SMT/HT не считаются отдельными ядрами."
    };
    let cap = if heavy < effective {
        format!(" Сработал предел 32 исполнителя: тяжёлых {heavy} вместо {effective}.")
    } else {
        String::new()
    };
    Selection {
        workers,
        explanation: format!(
            "Автоматически: всего {workers}; тяжёлых — {heavy}; владелец/Word — 1; интерфейс — {interface_workers}; поиск — 1. Доступный параллелизм CPU: {}; ядер для расчёта: {effective}. {topology} {} Правило: число тяжёлых равно доступным физическим ядрам; остальные места добавляются отдельно.{cap} Выбор сделан при запуске; память и текущая загрузка не определяют размер пула.",
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

#[cfg(test)]
mod tests {
    use super::*;

    fn info(logical: Option<usize>, cores: Option<usize>) -> CpuInfo {
        CpuInfo { logical, cores, source: "test topology".into() }
    }

    #[test]
    fn auto_counts_physical_cores_and_keeps_three_dedicated_roles() {
        for (logical, cores, total) in [(1,1,4),(2,1,4),(2,2,5),(4,4,7),(12,6,9),(6,6,9),(16,8,11)] {
            let selection=select_auto(info(Some(logical),Some(cores)),1);
            assert_eq!(selection.workers,total);
            let layout=crate::pool_layout::Layout::new(total,1).unwrap();
            assert_eq!(layout.interfaces,1);
            assert_eq!(layout.search,2);
            assert_eq!(total-layout.bulk,cores);
            assert!(selection.explanation.contains("SMT/HT"));
        }
    }

    #[test]
    fn auto_preserves_configured_interfaces_and_caps_total() {
        for interfaces in 1..=8 {
            let selection=select_auto(info(Some(16),Some(8)),interfaces);
            let layout=crate::pool_layout::Layout::new(selection.workers,interfaces).unwrap();
            assert_eq!(selection.workers,8+interfaces+2);
            assert_eq!(selection.workers-layout.bulk,8);
            let limited=select_auto(info(Some(128),Some(64)),interfaces);
            let layout=crate::pool_layout::Layout::new(limited.workers,interfaces).unwrap();
            assert_eq!(limited.workers,32);
            assert_eq!(32-layout.bulk,30-interfaces);
            assert!(limited.explanation.contains("предел 32"));
        }
    }

    #[test]
    fn auto_respects_cpu_limits_and_explains_fallback() {
        assert_eq!(select_auto(info(Some(2),Some(8)),1).workers,5);
        let estimated=select_auto(info(Some(5),None),1);
        assert_eq!(estimated.workers,6);
        assert!(estimated.explanation.contains("Топология не определена"));
        let unknown=select_auto(info(None,None),1);
        assert_eq!(unknown.workers,4);
        assert!(unknown.explanation.contains("Сведения о CPU недоступны"));
        assert_eq!(select_auto(info(Some(0),Some(0)),1).workers,4);
    }

    #[test]
    fn manual_total_is_not_increased_or_replaced_by_auto() {
        for workers in 1..=32 {
            for interfaces in 1..=8 {
                let selection=Mode::Manual(workers).resolve(interfaces);
                assert_eq!(selection.workers,workers);
                assert!(selection.explanation.contains("Ручная настройка"));
            }
        }
        assert_eq!(Mode::parse("auto").unwrap(),Mode::Auto);
        assert_eq!(Mode::Auto.to_string(),"auto");
    }

    #[test]
    fn windows_topology_counts_unique_cores_in_allowed_mask() {
        fn record(mask:u64) -> Vec<u8> {
            let mut r=vec![0u8;48];
            r[4..8].copy_from_slice(&48u32.to_le_bytes());
            r[30..32].copy_from_slice(&1u16.to_le_bytes());
            r[32..40].copy_from_slice(&mask.to_le_bytes());
            r
        }
        let bytes=[record(0b0011),record(0b1100)].concat();
        assert_eq!(windows_cores(&bytes,8,0,0b1111),Some(2));
        assert_eq!(windows_cores(&bytes,8,0,0b0011),Some(1));
        assert_eq!(windows_cores(&bytes,8,0,0b0101),Some(2));
        assert_eq!(windows_cores(&bytes,8,0,0b10000),None);
        assert_eq!(windows_cores(&bytes[..47],8,0,0b1111),None);
    }
}
