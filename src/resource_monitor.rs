//! A single background sampler. Rendering never queries the OS or waits for KS.
use std::{
    collections::VecDeque,
    sync::{mpsc, Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use sysinfo::{MemoryRefreshKind, Pid, ProcessRefreshKind, ProcessesToUpdate, System};

const INTERVAL: Duration = Duration::from_secs(1);
const HISTORY: usize = 120;

#[derive(Clone)]
struct Sample {
    at: Instant,
    sequence: u64,
    logical_cpus: usize,
    process_cpu: Option<f32>,
    system_cpu: Option<f32>,
    resident: Option<u64>,
    total: Option<u64>,
    available: Option<u64>,
}
#[derive(Default, Clone)]
struct Snapshot {
    samples: VecDeque<Sample>,
    failed: bool,
}
#[derive(Default)]
pub struct Resources {
    snapshot: Arc<Mutex<Snapshot>>,
    // Dropping the sender wakes the sampler; no detached, permanent loop.
    stop: OnceLock<mpsc::Sender<()>>,
}

fn percent(value: f32) -> Option<f32> {
    (value.is_finite() && value >= 0.0).then(|| value.min(100.0))
}
fn process_percent(raw: f32, logical: usize) -> Option<f32> {
    if logical == 0 { None } else { percent(raw / logical as f32) }
}
fn ratio(bytes: Option<u64>, total: Option<u64>) -> Option<f32> {
    let (bytes, total) = (bytes?, total?);
    (total > 0).then(|| (bytes as f64 / total as f64 * 100.0).min(100.0) as f32)
}
fn collect(system: &mut System, sequence: u64) -> Sample {
    system.refresh_cpu_usage();
    system.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
    let pid = Pid::from_u32(std::process::id());
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]), true,
        ProcessRefreshKind::nothing().with_cpu().with_memory().without_tasks(),
    );
    let cpus = system.cpus().len();
    let process = system.process(pid);
    let total = (system.total_memory() > 0).then(|| system.total_memory());
    Sample {
        at: Instant::now(), sequence, logical_cpus: cpus,
        // The first CPU refresh establishes a baseline, not a real measurement.
        process_cpu: (sequence > 1).then(|| process.and_then(|p| process_percent(p.cpu_usage(),cpus))).flatten(),
        system_cpu: (sequence > 1 && cpus > 0).then(|| percent(system.global_cpu_usage())).flatten(),
        resident: process.map(|p| p.memory()), total,
        available: total.filter(|n| system.available_memory() <= *n).map(|_| system.available_memory()),
    }
}
impl Resources {
    pub fn start(&self) {
        self.stop.get_or_init(|| {
            let (tx, rx) = mpsc::channel();
            let shared = self.snapshot.clone();
            let worker_shared = shared.clone();
            let started = std::thread::Builder::new().name("resource-monitor".into()).spawn(move || {
                let result = std::panic::catch_unwind(move || {
                    if !sysinfo::IS_SUPPORTED_SYSTEM { return false; }
                    let mut system = System::new();
                    let interval = INTERVAL.max(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
                    for sequence in 1.. {
                        let sample = collect(&mut system, sequence);
                        {
                            let mut state = worker_shared.lock().unwrap_or_else(|p| p.into_inner());
                            state.samples.push_back(sample);
                            while state.samples.len() > HISTORY { state.samples.pop_front(); }
                        }
                        if rx.recv_timeout(interval) != Err(mpsc::RecvTimeoutError::Timeout) { return true; }
                    }
                    true
                });
                if !matches!(result, Ok(true)) {
                    shared.lock().unwrap_or_else(|p| p.into_inner()).failed = true;
                }
            });
            if started.is_err() {
                self.snapshot.lock().unwrap_or_else(|p| p.into_inner()).failed = true;
            }
            tx
        });
    }
    pub fn html(&self) -> String {
        // A bounded copy under a separate lock, released before HTML formatting.
        let state = self.snapshot.lock().unwrap_or_else(|p| p.into_inner()).clone();
        render(&state)
    }
}
fn cpu_text(value: Option<f32>) -> String {
    value.map(|n| format!("{n:.1}%")).unwrap_or_else(|| "—".into())
}
fn memory_text(value: Option<u64>) -> String {
    value.map(|n| format!("{:.1} МиБ",n as f64/1048576.0)).unwrap_or_else(|| "—".into())
}

fn memory_short(value: Option<u64>) -> String {
    value.map(|n| if n >= 1073741824 { format!("{:.2} ГиБ", n as f64 / 1073741824.0) }
        else { memory_text(Some(n)) }).unwrap_or_else(|| "—".into())
}
pub(crate) fn load_level(value: Option<f32>) -> &'static str {
    match value.and_then(percent) {
        Some(n) if n >= 80.0 => "high",
        Some(n) if n >= 50.0 => "medium",
        Some(_) => "low",
        None => "unknown",
    }
}
fn card(system: bool, value: String, note: String, usage: Option<f32>) -> String {
    let level=load_level(usage);
    format!("<div class='resource-card{}' data-load='{level}'><h3>{}</h3><strong>{value}</strong><p>{note}</p></div>",
        if system {" system"} else {""}, if system {"Вся система"} else {"codex-AIs"})
}
const CHART_SECONDS: f64 = 120.0;
fn chart_value(s: &Sample, memory: bool, process: bool) -> Option<f64> {
    if memory {
        if process { s.resident.map(|n| n as f64 / 1073741824.0) }
        else { s.total.zip(s.available).and_then(|(total, available)|total.checked_sub(available)).map(|n| n as f64 / 1073741824.0) }
    } else { (if process {s.process_cpu} else {s.system_cpu}).map(f64::from) }
}
fn series(samples: &VecDeque<Sample>, memory: bool, process: bool, upper: f64) -> String {
    let Some(last) = samples.back() else { return String::new() };
    let mut path = String::new();
    let mut joined = false;
    for sample in samples {
        let age = last.at.saturating_duration_since(sample.at).as_secs_f64();
        if age > CHART_SECONDS { joined = false; continue; }
        let Some(value) = chart_value(sample,memory,process).filter(|v|v.is_finite() && *v>=0.0) else {
            joined = false; continue;
        };
        let x = 600.0 * (1.0 - age / CHART_SECONDS);
        let y = 132.0 * (1.0 - (value / upper).clamp(0.0,1.0));
        path.push_str(&format!("{}{x:.1},{y:.1} ",if joined {'L'} else {'M'}));
        joined = true;
    }
    path
}
fn chart(samples: &VecDeque<Sample>, memory: bool) -> String {
    // CPU and RAM share a fixed 120-second window. RAM is absolute GiB on
    // one common scale, never separately normalized process/system curves.
    let upper = if memory {
        samples.iter().filter_map(|s|s.total).max().map(|n|(n as f64/1073741824.0).ceil().max(1.0))
    } else {Some(100.0)};
    let label = |fraction: f64| upper.map(|n| if memory {format!("{:.1}",n*fraction).trim_end_matches('0').trim_end_matches('.').to_owned()} else {format!("{:.0}%",n*fraction)}).unwrap_or_else(||"—".into());
    let kind = if memory {"memory"} else {"cpu"};
    let name = if memory {"История ОЗУ, ГиБ"} else {"История загрузки ЦП, проценты"};
    let mut out = format!("<div class=history-chart id='{kind}-chart'><div class=chart-axis aria-hidden=true><span>{}</span><span>{}</span><span>{}</span></div><svg class=plot viewBox='0 0 600 132' preserveAspectRatio=none role=img aria-label='{name}; последние 120 секунд'><title>{name}; сплошная — codex-AIs, пунктир — вся система; зелёный до 50%, жёлтый до 80%, красный от 80%</title>",label(1.0),label(0.5),label(0.0));
    // Thresholds use actual physical RAM, not the rounded graph upper bound.
    let full = if memory {samples.back().and_then(|s|s.total).map(|n|n as f64/1073741824.0)} else {Some(100.0)};
    let scale=full.zip(upper).map(|(full,upper)|(full/upper).clamp(0.0,1.0)).unwrap_or(1.0);
    let high=100.0*(1.0-0.8*scale);
    let medium=100.0*(1.0-0.5*scale);
    out.push_str(&format!("<defs><linearGradient id='{kind}-load' gradientUnits=userSpaceOnUse x1=0 y1=0 x2=0 y2=132><stop offset='0%' class='chart-high'/><stop offset='{high:.4}%' class='chart-high'/><stop offset='{high:.4}%' class='chart-medium'/><stop offset='{medium:.4}%' class='chart-medium'/><stop offset='{medium:.4}%' class='chart-low'/><stop offset='100%' class='chart-low'/></linearGradient></defs>"));
    out.push_str("<path class=chart-grid d='M0 0H600 M0 66H600 M0 132H600'/><path class=chart-grid d='M150 0V132 M300 0V132 M450 0V132' stroke-dasharray='2 4'/>");
    for process in [false,true] {
        let path = upper.map(|n|series(samples,memory,process,n)).unwrap_or_default();
        out.push_str(&format!("<path class='series-{}' stroke='url(#{kind}-load)' d='{path}'/>",if process {"process"}else{"system"}));
    }
    out.push_str("</svg><div class=chart-ticks><span>2 мин назад</span><span>1 мин</span><span>Сейчас</span></div></div>");
    out
}
fn render(state: &Snapshot) -> String {
    let mut out = String::from("<section class=resources id=resources aria-label='ЦП и оперативная память'>");
    let Some(s) = state.samples.back() else {
        out.push_str(if state.failed {"<p class=resource-stale>Системные метрики недоступны. Обработка запросов продолжается.</p></section>"} else {"<p class=monitor-note>Собирается первый отсчёт. Показатели появятся автоматически.</p></section>"});
        return out;
    };
    let used = s.total.zip(s.available).and_then(|(total,available)|total.checked_sub(available));
    out.push_str(&format!("<div id=resource-sample data-sequence='{}' data-age-seconds='{:.2}' data-logical-cpus='{}'",s.sequence,s.at.elapsed().as_secs_f64(),s.logical_cpus));
    for (name,value) in [("process-cpu",s.process_cpu),("system-cpu",s.system_cpu)] {
        if let Some(value)=value {out.push_str(&format!(" data-{name}='{value:.3}'"));}
    }
    for (name,value) in [("process-rss",s.resident),("memory-total",s.total),("memory-available",s.available)] {
        if let Some(value)=value {out.push_str(&format!(" data-{name}='{value}'"));}
    }
    out.push('>');
    if state.failed || s.at.elapsed() > Duration::from_secs(10) {
        out.push_str("<p class=resource-stale>Новые отсчёты не поступают. Показаны последние полученные данные.</p>");
    }
    out.push_str("<div class=resource-panels><section class=resource-panel id=panel-cpu><div class=panel-title><h2>Процессор</h2><span class=period>ЦП · %</span></div><div class=resource-grid>");
    out.push_str(&card(false,cpu_text(s.process_cpu),format!("PID {}",std::process::id()),s.process_cpu));
    out.push_str(&card(true,cpu_text(s.system_cpu),format!("Логических ЦП: {}",s.logical_cpus),s.system_cpu));
    out.push_str("</div>");out.push_str(&chart(&state.samples,false));
    out.push_str("</section><section class=resource-panel id=panel-memory><div class=panel-title><h2>Оперативная память</h2><span class=period>ОЗУ · ГиБ</span></div><div class=resource-grid>");
    out.push_str(&card(false,memory_short(s.resident),format!("Рабочий набор · {} всей ОЗУ",cpu_text(ratio(s.resident,s.total))),ratio(s.resident,s.total)));
    out.push_str(&card(true,memory_short(used),format!("{} из {} · свободно {}",cpu_text(ratio(used,s.total)),memory_short(s.total),memory_short(s.available)),ratio(used,s.total)));
    out.push_str("</div>");out.push_str(&chart(&state.samples,true));
    out.push_str("</section></div></div></section>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signal_thresholds_use_physical_ram_and_missing_is_not_green() {
        for (n,expected) in [(None,"unknown"),(Some(f32::NAN),"unknown"),(Some(-1.0),"unknown"),(Some(0.0),"low"),(Some(49.99),"low"),(Some(50.0),"medium"),(Some(79.99),"medium"),(Some(80.0),"high"),(Some(100.0),"high")] {
            assert_eq!(load_level(n),expected);
        }
        let samples=VecDeque::from([Sample {at:Instant::now(),sequence:2,logical_cpus:12,
            process_cpu:Some(50.0),system_cpu:Some(80.0),resident:Some(1024),total:Some(30*1073741824+536870912),available:Some(0)}]);
        let output=chart(&samples,true);
        // 30.5 GiB physical memory, 31 GiB graph: red begins at 24.4 GiB.
        assert!(output.contains("offset='21.2903%' class='chart-high'"));
        assert!(output.contains("offset='50.8065%' class='chart-medium'"));
        let output=render(&Snapshot {samples,failed:false});
        assert!(output.contains("data-load='medium'"));
        assert!(output.contains("data-load='high'"));
    }
    #[test]
    fn memory_and_cpu_share_time_axis_without_inventing_missing_values() {
        let at=Instant::now();
        let sample=|seconds, value|Sample {at:at-Duration::from_secs(seconds),sequence:1,logical_cpus:12,
            process_cpu:value,system_cpu:Some(50.0),resident:value.map(|_|4*1073741824),
            total:Some(32*1073741824),available:Some(16*1073741824)};
        let samples=VecDeque::from([sample(121,Some(20.0)),sample(120,Some(25.0)),sample(60,None),sample(0,Some(50.0))]);
        assert_eq!(chart_value(samples.back().unwrap(),true,true),Some(4.0));
        assert_eq!(chart_value(samples.back().unwrap(),true,false),Some(16.0));
        assert_eq!(series(&samples,false,true,100.0),"M0.0,99.0 M600.0,66.0 ");
        assert_eq!(series(&samples,true,true,32.0),"M0.0,115.5 M600.0,115.5 ");
        assert_eq!(series(&samples,true,false,32.0),"M0.0,66.0 L300.0,66.0 L600.0,66.0 ");
    }
    #[test]
    fn cpu_normalization_and_unavailable_values() {
        assert_eq!(process_percent(600.0,12),Some(50.0));
        assert_eq!(process_percent(1200.0,12),Some(100.0));
        assert_eq!(process_percent(10.0,0),None);
        assert_eq!(percent(f32::NAN),None);
        assert_eq!(percent(-1.0),None);
        assert_eq!(ratio(Some(512),Some(1024)),Some(50.0));
        assert_eq!(ratio(Some(512),Some(0)),None);
    }
    #[test]
    fn first_sample_and_stale_data_are_not_zero_usage() {
        let mut state=Snapshot::default();
        assert!(render(&state).contains("первый отсчёт"));
        state.samples.push_back(Sample {at:Instant::now()-Duration::from_secs(12),sequence:1,logical_cpus:12,
            process_cpu:None,system_cpu:None,resident:Some(1024),total:Some(4096),available:Some(2048)});
        let html=render(&state);
        assert!(!html.contains("data-process-cpu="));
        assert!(html.contains("Новые отсчёты не поступают"));
        assert!(html.contains("data-process-rss='1024'"));
    }
}
