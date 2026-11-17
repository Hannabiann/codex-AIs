//! Request IDs are logical operations, never Tokio task or OS thread IDs.
//! The lock protects small snapshots only; KS and authentication never hold it.
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
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
}
#[derive(Default)]
struct Inner {
    next: u64,
    active: BTreeMap<u64, Entry>,
    history: VecDeque<Entry>,
    scheduled: Vec<String>,
    connections: usize,
}
#[derive(Default)]
pub struct Monitor {
    inner: Mutex<Inner>,
    settings: crate::settings::Settings,
}
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
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
    pub fn new(settings: crate::settings::Settings) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            settings,
        }
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
    pub fn target(&self, id: u64, url: &str) {
        if let Some(e) = self.inner.lock().unwrap().active.get_mut(&id) {
            e.url = safe_url(url);
        }
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
            e.elapsed = Some(e.tick.elapsed().as_millis());
            e.result = result.into();
            e.state = state;
            m.history.push_front(e);
            m.history.truncate(self.settings.history);
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
    pub fn html(&self) -> String {
        let (active, history, scheduled, connections) = {
            let m = self.inner.lock().unwrap();
            (
                m.active.values().cloned().collect::<Vec<_>>(),
                m.history.clone(),
                m.scheduled.clone(),
                m.connections,
            )
        };
        let queued = active
            .iter()
            .filter(|e| {
                e.state == State::Queued || e.state == State::Cancelling && e.executor.is_none()
            })
            .count();
        let running = active
            .iter()
            .filter(|e| {
                e.state == State::Running || e.state == State::Cancelling && e.executor.is_some()
            })
            .count();
        let connection_limit = self.settings.connections;
        let queue_limit = self.settings.queue + self.settings.workers.saturating_sub(1).min(2);
        let workers = self.settings.workers;
        let mut out=format!("<!doctype html><html lang=ru><meta charset=utf-8><meta http-equiv=refresh content=3><title>Состояние сервера</title><style>body{{background:#e3ddda;font:14px Arial;margin:20px}}table{{background:white;border-collapse:collapse;width:100%;min-width:1120px;table-layout:auto}}.table-scroll{{max-width:100%;overflow-x:auto}}th,td{{border:1px solid #aaa;padding:7px 10px;text-align:left;vertical-align:top}}th{{color:#00009a;white-space:nowrap}}td{{white-space:nowrap}}td:nth-child(4){{min-width:280px;max-width:640px;white-space:normal;overflow-wrap:anywhere}}td:nth-child(3),td:nth-child(6),td:nth-child(9){{max-width:260px;white-space:normal;overflow-wrap:break-word}}td:nth-child(1),td:nth-child(5),td:nth-child(7),td:nth-child(8){{font-variant-numeric:tabular-nums}}.lost{{color:#a00}}</style><h2>Состояние сервера · Tokio</h2><p>Соединения: {connections}/{connection_limit}. Очередь KS: {queued}/{queue_limit}. Исполнители KS/DB: {running}/{workers}, свободно: {}. Свободные слоты приёма HTTP: {}.</p><p>Сеть обслуживается Tokio. Состояние фоновых очередей и Word принадлежит исполнителю 1. При 3 и более исполнителях номер 2 обслуживает проверенные формы интерфейса /20a, остальные — удаления, поиск и отчёты. Неизвестные KS обслуживает владелец каталога. Монитор и проверка его доступа выполняются вне очереди KS.</p>",workers.saturating_sub(running),connection_limit.saturating_sub(connections));
        for (title, rows, job) in [
            ("Активные HTTP-запросы", active.clone(), false),
            ("Активные задания At", active.clone(), true),
            ("История HTTP", history.iter().cloned().collect(), false),
            ("История At", history.iter().cloned().collect(), true),
        ] {
            out.push_str(&format!("<h3>{title}</h3><div class=table-scroll><table><tr><th>ID</th><th>Состояние</th><th>Результат</th><th>URL / действие</th><th>Источник / IP:Port</th><th>Пользователь</th><th>Начало UTC (Unix)</th><th>Мс</th><th>Соединение</th><th>KS/DB</th></tr>"));
            for e in rows.iter().filter(|e| e.job == job) {
                out.push_str(&format!("<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td{}>{}</td><td>{}</td></tr>",e.id,e.state.label(),escape(&e.result),escape(&e.url),escape(&e.source),escape(&e.user),e.start,e.elapsed.unwrap_or_else(||e.tick.elapsed().as_millis()),if e.disconnected{" class=lost"}else{""},if e.disconnected {if e.state == State::Cancelling {"отключено; ожидание безопасной остановки"} else {"отключено"}}else if e.job{"—"}else{""},e.executor.map(|v|v.to_string()).unwrap_or_default()));
            }
            out.push_str("</table></div>");
        }
        out.push_str("<h3>Зарегистрированные задания At</h3><ul>");
        for row in scheduled {
            out.push_str(&format!("<li>{}</li>", escape(&row)));
        }
        out.push_str(&format!("</ul><p>История ограничена {} завершениями. При отключении клиента KS и чтение DB отменяются. Начатая атомарная запись DB4 завершается; уже сохранённые изменения не откатываются. Keep-alive не занимает исполнителя между запросами. Значения форм, токены и заголовки авторизации не сохраняются.</p></html>", self.settings.history));
        out
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
