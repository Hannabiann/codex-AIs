//! Request IDs are logical operations, never Tokio task or OS thread IDs.
//! The lock protects small snapshots only; KS and authentication never hold it.
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex, OnceLock},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
#[path = "monitor_html.rs"]
mod html;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Headers,
    Queued,
    Running,
    Status,
    Writing,
    Done,
    Error,
    Cancelling,
    Cancelled,
}
impl State {
    fn label(self) -> &'static str {
        match self {
            Self::Headers => "Чтение HTTP",
            Self::Queued => "Очередь",
            Self::Running => "Выполнение KS/DB",
            Self::Status => "Монитор: проверка доступа",
            Self::Writing => "Отправка HTTP",
            Self::Done => "Завершено",
            Self::Error => "Ошибка",
            Self::Cancelling => "Отмена запрошена",
            Self::Cancelled => "Отменено",
        }
    }
}
#[derive(Clone)]
struct Entry {
    id: u64,
    job: bool,
    url: String,
    source: String,
    user: String,
    start: u64,
    tick: Instant,
    elapsed: Option<u128>,
    state: State,
    result: String,
    disconnected: bool,
    executor: Option<usize>,
    group: &'static str,
    method: String,
    error_logged: bool,
    monitor_poll: bool,
}
#[derive(Default)]
struct Inner {
    next: u64,
    active: BTreeMap<u64, Entry>,
    history: VecDeque<Entry>,
    scheduled: Vec<String>,
    connections: usize,
}
pub struct Monitor {
    inner: Mutex<Inner>,
    resources: crate::resource_monitor::Resources,
    settings: crate::settings::Settings,
    workers: crate::worker_config::Selection,
    admission: OnceLock<[Arc<tokio::sync::Semaphore>; 3]>,
}
impl Default for Monitor {
    fn default() -> Self {
        let settings = crate::settings::Settings::default();
        let workers = settings.workers.resolve(settings.interface_workers);
        Self::new(settings, workers)
    }
}
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
pub fn is_status_snapshot(target: &str) -> bool {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    matches!(path, "/admin" | "/admin/")
        && query.split('&').next() == Some("status")
        && query.split('&').any(|p| p == "snapshot=1")
}
/// Only numeric document coordinates survive. Tokens, search terms and forms do not.
pub fn safe_url(s: &str) -> String {
    let (path, query) = s.split_once('?').unwrap_or((s, ""));
    let mut result = path
        .chars()
        .filter(|c| !c.is_control())
        .take(256)
        .collect::<String>();
    for (i, pair) in query
        .split('&')
        .filter(|s| !s.is_empty())
        .take(12)
        .enumerate()
    {
        result.push(if i == 0 { '?' } else { '&' });
        match pair.split_once('=') {
            Some((key, value))
                if matches!(key, "nd" | "nh" | "na" | "mode")
                    && value.len() <= 10
                    && value.bytes().all(|b| b.is_ascii_digit()) =>
            {
                result.push_str(pair)
            }
            Some((key, _)) => {
                result.extend(
                    key.chars()
                        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .take(32),
                );
                result.push_str("=[скрыто]");
            }
            None => result.extend(
                pair.chars()
                    .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '~' | '.'))
                    .take(64),
            ),
        }
    }
    result
}
impl Monitor {
    pub fn new(
        settings: crate::settings::Settings,
        workers: crate::worker_config::Selection,
    ) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            resources: crate::resource_monitor::Resources::default(),
            settings,
            workers,
            admission: OnceLock::new(),
        }
    }
    pub fn start_resources(&self) {
        self.resources.start();
    }
    pub fn bind_admission(&self, queues: [Arc<tokio::sync::Semaphore>; 3]) {
        let _ = self.admission.set(queues);
    }
    pub fn active_users(&self) -> usize {
        self.inner
            .lock()
            .unwrap()
            .active
            .values()
            .filter(|e| !e.user.is_empty())
            .map(|e| e.user.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }
    pub fn start(&self, url: &str, source: &str, job: bool) -> u64 {
        let mut m = self.inner.lock().unwrap();
        m.next += 1;
        let id = m.next;
        m.active.insert(
            id,
            Entry {
                id,
                job,
                url: safe_url(url),
                source: source.into(),
                user: String::new(),
                start: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                tick: Instant::now(),
                elapsed: None,
                state: if job { State::Queued } else { State::Headers },
                result: String::new(),
                disconnected: false,
                executor: None,
                group: if job { "Владелец" } else { "" },
                method: if job { "At".into() } else { String::new() },
                error_logged: false,
                monitor_poll: !job && is_status_snapshot(url),
            },
        );
        id
    }
    pub fn connected(&self) {
        self.inner.lock().unwrap().connections += 1;
    }
    fn connection_closed(&self) {
        let mut m = self.inner.lock().unwrap();
        m.connections = m.connections.saturating_sub(1);
    }
    pub fn executor(&self, id: u64, worker: usize) {
        if let Some(e) = self.inner.lock().unwrap().active.get_mut(&id) {
            e.executor = Some(worker);
        }
    }
    pub fn group(&self, id: u64, group: &'static str) {
        if let Some(e) = self.inner.lock().unwrap().active.get_mut(&id) {
            e.group = group;
        }
    }
    pub fn target(&self, id: u64, url: &str) {
        if let Some(e) = self.inner.lock().unwrap().active.get_mut(&id) {
            e.url = safe_url(url);
            e.monitor_poll = !e.job && is_status_snapshot(url);
        }
    }
    pub fn method(&self, id: u64, method: &str) {
        if let Some(e) = self.inner.lock().unwrap().active.get_mut(&id) { e.method = method.chars().take(16).collect(); }
    }
    pub fn response_error(&self, id: u64, response: &crate::server::Response) {
        if !matches!(response.status.as_bytes().first(), Some(b'4' | b'5')) || response.status.starts_with("401 ") { return; }
        let context = {
            let mut inner = self.inner.lock().unwrap();
            let Some(e) = inner.active.get_mut(&id) else { return };
            if e.error_logged { return; }
            e.error_logged = true;
            Self::log_context(e)
        };
        let detail = if response.mime.starts_with("text/plain") {
            let bytes = &response.body[..response.body.len().min(16384)];
            if response.mime.contains("windows-1251") { crate::db::decode(bytes) } else { String::from_utf8_lossy(bytes).into_owned() }
        } else { "Сервер вернул страницу ошибки".into() };
        crate::runtime_logs::error("request", &format!("{context} status={} error={detail}", response.status));
    }
    pub fn ks_error(&self, id: u64, target: &str, error: &str, detail: &str) {
        let context = {
            let mut inner = self.inner.lock().unwrap();
            let Some(e) = inner.active.get_mut(&id) else { return };
            e.error_logged = true;
            Self::log_context_url(e, &crate::runtime_logs::diagnostic_url(target))
        };
        crate::runtime_logs::error_detail("request KS", &format!("{context} error={error}"), detail);
    }
    fn log_context(e: &Entry) -> String {
        Self::log_context_url(e, &e.url)
    }
    fn log_context_url(e: &Entry, url: &str) -> String {
        let clean = crate::runtime_logs::field;
        format!("id={} kind={} client=\"{}\" user=\"{}\" executor={} method={} url=\"{}\" elapsed_ms={}",
            e.id, if e.job {"At"} else {"HTTP"}, clean(&e.source), clean(&e.user), e.executor.unwrap_or(0),
            clean(&e.method), clean(url), e.tick.elapsed().as_millis())
    }
    pub fn user(&self, id: u64, user: &str) {
        if let Some(e) = self.inner.lock().unwrap().active.get_mut(&id) {
            e.user = user.chars().take(128).collect();
        }
    }
    pub fn state(&self, id: u64, state: State) {
        if let Some(e) = self.inner.lock().unwrap().active.get_mut(&id) {
            e.state = state;
        }
    }
    pub fn finish(&self, id: u64, result: &str, state: State) {
        let mut m = self.inner.lock().unwrap();
        if let Some(mut e) = m.active.remove(&id) {
            // Poll failures remain visible; successful monitoring must not
            // evict real operations from the bounded history or fill access.log.
            if e.monitor_poll && result.starts_with("200 ") && state == State::Done {
                return;
            }
            e.elapsed = Some(e.tick.elapsed().as_millis());
            e.result = result.into();
            e.state = state;
            let line = format!("{} result=\"{}\" state={:?}", Self::log_context(&e), crate::runtime_logs::field(result), state);
            m.history.push_front(e);
            m.history.truncate(self.settings.history);
            drop(m);
            crate::runtime_logs::access(&line);
        }
    }
    pub fn closed(&self, id: u64) {
        let mut m = self.inner.lock().unwrap();
        if let Some(e) = m.active.get_mut(&id) {
            e.disconnected = true;
            if matches!(e.state, State::Queued | State::Running | State::Cancelling) {
                e.state = State::Cancelling;
                return;
            }
        }
        drop(m);
        self.finish(id, "клиент отключён", State::Cancelled);
    }
    pub fn schedules(&self, rows: Vec<String>) {
        self.inner.lock().unwrap().scheduled = rows;
    }

}
pub struct ConnectionGuard {
    pub monitor: std::sync::Arc<Monitor>,
}
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.monitor.connection_closed();
    }
}
pub struct RequestGuard {
    pub monitor: std::sync::Arc<Monitor>,
    pub id: u64,
    pub control: std::sync::Arc<crate::execution::Control>,
}
impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.control.cancel();
        self.monitor.closed(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn monitor(workers: usize, interfaces: usize) -> Monitor {
        let settings = crate::settings::Settings {
            workers: crate::worker_config::Mode::Manual(workers),
            interface_workers: interfaces,
            queue: 8,
            ..Default::default()
        };
        let selection = settings.workers.resolve(interfaces);
        Monitor::new(settings, selection)
    }
    #[test]
    fn status_polls_keep_failures_but_do_not_evict_request_history() {
        let m = monitor(4, 1);
        for url in ["/admin?status&snapshot=1", "/admin/?status&snapshot=1&x=y"] {
            let id = m.start("headers", "127.0.0.1:1", false);
            m.target(id, url);
            assert!(!m.snapshot_html().contains(&format!("row-active-http-{id}")));
            m.finish(id, "200 OK", State::Done);
        }
        assert!(m.inner.lock().unwrap().history.is_empty());
        let id = m.start("/admin?status&snapshot=1", "local", false);
        m.finish(id, "401 Unauthorized", State::Done);
        for url in ["/admin?status", "/20a?status&snapshot=1", "/admin?advanced&snapshot=1", "/admin?status&snapshot=10"] {
            assert!(!is_status_snapshot(url));
            let id = m.start(url, "local", false);
            m.finish(id, "200 OK", State::Done);
        }
        assert_eq!(m.inner.lock().unwrap().history.len(), 5);
        assert!(m.snapshot_html().contains("401 Unauthorized"));
    }
    #[test]
    fn capacity_counts_executing_workers_and_actual_admission_permits() {
        let m = monitor(6, 2);
        let queues = [Arc::new(tokio::sync::Semaphore::new(8)), Arc::new(tokio::sync::Semaphore::new(1)), Arc::new(tokio::sync::Semaphore::new(1))];
        m.bind_admission(queues.clone());
        let _common = queues[0].try_acquire_many(3).unwrap();
        let _reserve = queues[2].try_acquire().unwrap();
        for (worker, state) in [(1, State::Running), (5, State::Cancelling), (6, State::Writing)] {
            let id = m.start("/20a?test", "<&'", false);
            m.executor(id, worker);
            m.state(id, state);
        }
        // A cancelled request still waiting for a worker occupies no executor.
        let id = m.start("/20a?queued", "local", false);
        m.state(id, State::Cancelling);
        let html = m.snapshot_html();
        assert!(html.contains("id='capacity-ks' data-used='2' data-total='6'"));
        assert!(html.contains("id='capacity-queue' data-used='4' data-total='10'"));
        assert!(html.contains("class='worker cancelling' id='worker-5'"));
        assert!(html.contains("class='worker' id='worker-6'"));
        assert!(html.contains("&lt;&amp;&#39;"));
        assert!(html.contains("№3 · Интерфейс"));
        assert!(html.contains("№4 · Поиск"));
        assert_eq!(html.matches("<tbody>").count(), 4);
    }
    #[test]
    fn small_pool_roles_and_live_shell_are_consistent() {
        for (n, roles) in [(1, vec!["Все запросы"]), (2, vec!["Word / интерфейс", "Поиск / тяжёлые"]), (3, vec!["Word / состояние", "Интерфейс", "Поиск / тяжёлые"])] {
            let m = monitor(n, 1);
            let html = m.html();
            for (i, role) in roles.iter().enumerate() {
                assert!(html.contains(&format!("№{} · {role}", i + 1)));
            }
            assert!(!html.contains("http-equiv=refresh"));
            assert!(html.contains("?status&snapshot=1"));
            assert!(!m.snapshot_html().contains("<script>"));
        }
    }
}
