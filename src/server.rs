use crate::{
    db::{Db4Engine, VirtualDir},
    vm,
};
use std::{collections::HashMap, sync::Arc};
#[path = "registration_settings.rs"]
pub(crate) mod registration;
#[path = "static_cache.rs"]
mod static_cache;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
#[path = "http_stream.rs"]
pub(crate) mod streaming;
pub struct Response {
    pub status: &'static str,
    pub mime: &'static str,
    pub body: Vec<u8>,
    pub location: Option<String>,
    pub headers: Vec<(String, String)>,
}
#[derive(Clone, Default)]
pub struct RequestContext {
    pub response_stream: Option<streaming::Sender>,
    pub target: String,
    pub host: String,
    pub cookie: String,
    pub if_modified_since: String,
    pub if_none_match: String,
    pub user: Option<crate::auth::User>,
    pub form: Vec<(String, String)>,
    pub uploads: Vec<crate::http_form::Upload>,
    pub monitor: Option<(Arc<crate::monitor::Monitor>, u64)>,
    pub user_agent: String,
    pub method: String,
    pub protocol: String,
    // Transient legacy KS input; never added to monitor/history or diagnostics.
    pub authorization: Option<String>,
    pub body: Vec<u8>,
}
fn message(status: &'static str, body: impl Into<String>) -> Response {
    Response {
        status,
        mime: "text/plain; charset=utf-8",
        body: body.into().into_bytes(),
        location: None,
        headers: Vec::new(),
    }
}
pub(crate) fn percent_decode(s: &str) -> Result<String, String> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if i + 2 >= b.len() {
                return Err("invalid percent escape".into());
            }
            let digit = |b: u8| (b as char).to_digit(16).map(|v| v as u8);
            let a = digit(b[i + 1]).ok_or("invalid percent escape")?;
            let c = digit(b[i + 2]).ok_or("invalid percent escape")?;
            out.push(a * 16 + c);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    // UTF-8 URLs and legacy CP1251 query values both occur in this application.
    let s = String::from_utf8(out.clone()).unwrap_or_else(|_| crate::db::decode(&out));
    if s.chars().any(|c| c.is_control()) {
        return Err("control character in URI".into());
    }
    Ok(s)
}
fn mime(name: &str, data: &[u8]) -> &'static str {
    if crate::document::has_rtf_header(data) {
        "application/rtf"
    } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        "image/gif"
    } else if data.starts_with(b"\xff\xd8\xff") {
        "image/jpeg"
    } else if name.to_lowercase().ends_with(".css") || name.eq_ignore_ascii_case("mp_css") {
        "text/css; charset=windows-1251"
    } else if name.to_lowercase().ends_with(".js") || name.eq_ignore_ascii_case("mp_js") {
        "application/javascript; charset=windows-1251"
    } else if name.to_lowercase().ends_with(".html") || name.to_lowercase().ends_with(".htm") {
        "text/html; charset=windows-1251"
    } else {
        "application/octet-stream"
    }
}
fn redirect(target: &str) -> Response {
    if target.chars().any(|c| c.is_control()) {
        return message(
            "500 Internal Server Error",
            "Invalid redirect configuration",
        );
    }
    let location = if target.starts_with('/')
        || target.starts_with("http://")
        || target.starts_with("https://")
    {
        target.to_string()
    } else {
        format!("/{target}")
    };
    Response {
        status: "301 Moved Permanently",
        mime: "text/plain",
        body: Vec::new(),
        location: Some(location),
        headers: Vec::new(),
    }
}
fn physical_path(
    engine: &Db4Engine,
    dir: &VirtualDir,
    path: &str,
) -> Result<std::path::PathBuf, Response> {
    use std::path::{Component, Path};
    let data_root = engine
        .volumes
        .first()
        .and_then(|v| v.path.parent())
        .ok_or_else(|| message("422 Unprocessable Content", "data root missing"))?;
    let mapping = dir.mapped_to.replace('\\', "/");
    let relative = path
        .get(dir.path.trim_end_matches('/').len()..)
        .unwrap_or("")
        .trim_start_matches('/');
    if relative.contains(['\\', ':'])
        || Path::new(relative)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(message("403 Forbidden", "Invalid resource path"));
    }
    let folder = data_root
        .join(mapping)
        .canonicalize()
        .map_err(|e| message("422 Unprocessable Content", e.to_string()))?;
    if !folder.starts_with(data_root) {
        return Err(message(
            "403 Forbidden",
            "Resource directory outside data root",
        ));
    }
    let candidate = folder.join(if relative.is_empty() {
        "index.html"
    } else {
        relative
    });
    let file = match candidate.canonicalize() {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(message("404 Not Found", "Resource not found"))
        }
        Err(e) => return Err(message("422 Unprocessable Content", e.to_string())),
    };
    if !file.starts_with(&folder) {
        return Err(message(
            "403 Forbidden",
            "Resource outside configured directory",
        ));
    }
    Ok(file)
}
fn physical_resource(engine: &Db4Engine, dir: &VirtualDir, path: &str) -> Result<Response, String> {
    use std::io::Read;
    let file = match physical_path(engine, dir, path) {
        Ok(file) => file,
        Err(response) => return Ok(response),
    };
    let extension = file
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mime = match extension.as_str() {
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" => "application/javascript",
        "png" => "image/png",
        "gif" => "image/gif",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        _ => {
            return Ok(message(
                "403 Forbidden",
                "Only public web resources are supported",
            ))
        }
    };
    let reader = std::fs::File::open(&file).map_err(|e| e.to_string())?;
    let meta = reader.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > 16 * 1024 * 1024 {
        return Err("resource file size/type limit".into());
    }
    let mut body = Vec::with_capacity(meta.len() as usize);
    reader
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut body)
        .map_err(|e| e.to_string())?;
    if body.len() > 16 * 1024 * 1024 {
        return Err("resource file grew beyond limit".into());
    }
    Ok(Response {
        status: "200 OK",
        mime,
        body,
        location: None,
        headers: Vec::new(),
    })
}
fn object_name(path: &str, query: &str, dir: &VirtualDir) -> Result<String, String> {
    if let Some(action) = query.split('&').find(|s| !s.is_empty()) {
        // GET forms submit their command as an empty named input (`find=`).
        // A nonempty value is an ordinary parameter, not a script name.
        let action = action.strip_suffix('=').unwrap_or(action);
        if !action.is_empty() && !action.contains('=') {
            return percent_decode(action);
        }
    }
    let base = dir.path.trim_end_matches('/');
    let suffix = path.get(base.len()..).unwrap_or("").trim_matches('/');
    // Native W dispatches the stored Word script under a document filename.
    if dir.path.trim_matches('/') == "20a"
        && suffix
            .strip_prefix("word/")
            .and_then(|s| s.strip_suffix(".rtf"))
            .is_some_and(|id| !id.is_empty() && id.bytes().all(|c| c.is_ascii_digit()))
    {
        return Ok("Word".into());
    }
    Ok(if suffix.is_empty() {
        "default".into()
    } else {
        suffix.into()
    })
}
/// No global name fallback: requests cannot accidentally execute a module from another volume.
pub fn route(engine: &Db4Engine, target: &str) -> Response {
    if target == "/health" {
        return message(
            "200 OK",
            format!(
                "codex-AIs {}; HTTP listener ready; partial KS runtime; startup not checked",
                crate::product::version()
            ),
        );
    }
    match route_inner(
        engine,
        target,
        &mut |dir, record, name| vm::execute_with_startup(engine, dir, record, name),
        None,
        None,
    ) {
        Ok(r) => r,
        Err(e) => message("422 Unprocessable Content", e),
    }
}
fn route_inner(
    engine: &Db4Engine,
    target: &str,
    execute: &mut dyn FnMut(
        &VirtualDir,
        &crate::db::fields::Record,
        &str,
    ) -> Result<Vec<u8>, vm::VmError>,
    mut read_record: Option<
        &mut dyn FnMut(&VirtualDir, u32) -> Result<Option<crate::db::fields::Record>, String>,
    >,
    mut read_named: Option<
        &mut dyn FnMut(&VirtualDir, &str) -> Result<Option<crate::db::fields::Record>, String>,
    >,
) -> Result<Response, String> {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let path = percent_decode(path)?;
    let Some(dir) = engine.resolve_virtual_dir(&path) else {
        return Ok(message("404 Not Found", "Virtual directory not found"));
    };
    if dir.kind == 2 {
        return Ok(redirect(&dir.mapped_to));
    }
    if dir.kind == 1 {
        return physical_resource(engine, dir, &path);
    }
    if dir.kind != 3 {
        return Ok(message(
            "501 Not Implemented",
            "Physical-directory hosting is not implemented in this reader build",
        ));
    }
    let mut params = HashMap::new();
    for part in query.split('&') {
        if let Some((k, v)) = part.split_once('=') {
            params.insert(percent_decode(k)?, percent_decode(&v.replace('+', " "))?);
        }
    }
    let name = object_name(&path, query, dir)?;
    if matches!(name.as_str(), "_picture" | "m.gif") && params.contains_key("nd") {
        let oid = params
            .get("nd")
            .ok_or("picture nd required")?
            .parse()
            .map_err(|_| "invalid picture nd")?;
        let attr = params
            .get("f")
            .ok_or("picture f required")?
            .parse()
            .map_err(|_| "invalid picture f")?;
        let index = params
            .get("p")
            .ok_or("picture p required")?
            .parse::<usize>()
            .map_err(|_| "invalid picture p")?;
        let record = if let Some(reader) = &mut read_record {
            reader(dir, oid)?
        } else {
            vm::read_with_startup(engine, dir, oid)?
        };
        let Some(record) = record else {
            return Ok(message("404 Not Found", "Picture document not found"));
        };
        let field = crate::document::select(&record, attr, 4)?;
        let items = crate::rich_text::items(field)?;
        let Some(item) = crate::rich_text::item_at(&items, index) else {
            return Ok(message("404 Not Found", "Picture index not found"));
        };
        let (mime, body) = crate::rich_text::picture(item)?;
        return Ok(Response {
            status: "200 OK",
            mime,
            body,
            location: None,
            headers: vec![],
        });
    }
    if params.get("mode").map(String::as_str) == Some("4") && name == "default" {
        let oid = params
            .get("nd")
            .ok_or("nd is required")?
            .parse::<u32>()
            .map_err(|_| "invalid nd")?;
        let attr = params
            .get("na")
            .ok_or("na is required")?
            .parse::<u16>()
            .map_err(|_| "invalid na")?;
        let record = if let Some(reader) = &mut read_record {
            reader(dir, oid)?
        } else {
            vm::read_with_startup(engine, dir, oid)?
        };
        if let Some(rec) = record {
            if let Some(data) = crate::document::resource_bytes(&rec, attr)? {
                return Ok(Response {
                    status: "200 OK",
                    mime: mime("", data),
                    body: data.to_vec(),
                    location: None,
                    headers: Vec::new(),
                });
            }
        }
        return Ok(message("404 Not Found", "Binary attribute not found"));
    }
    let record = if params.get("view").map(String::as_str) == Some("plain") {
        let oid = params
            .get("nd")
            .ok_or("plain view requires explicit nd")?
            .parse()
            .map_err(|_| "invalid nd")?;
        if let Some(reader) = &mut read_record {
            reader(dir, oid)?
        } else {
            engine.oid_for_dir(dir, oid)?
        }
    } else {
        match engine.named(&dir.mapped_to, &name)? {
            Some(r) => Some(r),
            None => {
                if let Some(reader) = &mut read_named {
                    reader(dir, &name)?
                } else {
                    vm::named_with_startup(engine, dir, &name)?
                }
            }
        }
    };
    let Some(record) = record else {
        return Ok(message(
            "404 Not Found",
            format!(
                "Object {name} not found in {} or its Base volumes",
                dir.mapped_to
            ),
        ));
    };
    if params.get("view").map(String::as_str) == Some("plain") {
        if !params.contains_key("nd") {
            return Err("plain view requires explicit nd".into());
        }
        let body = crate::document::plain_html(&record)?.into_bytes();
        return Ok(Response {
            status: "200 OK",
            mime: "text/html; charset=utf-8",
            body,
            location: None,
            headers: Vec::new(),
        });
    }
    if record.source().map(|s| !s.is_empty()).unwrap_or(false)
        && !matches!(record.area, 900 | 901)
        && record.code().is_none()
    {
        return Ok(message("501 Not Implemented","Document form rendering is not implemented. Explicit view=plain is available for internal text documents; the record is not executed as KS."));
    }
    if record.source().map(crate::document::has_rtf_header) == Some(true) {
        return Ok(message("501 Not Implemented","RTF document rendering requires the native document pipeline; this attribute is not executed as KS"));
    }
    if record.code().is_some() || record.source().map(|s| !s.is_empty()).unwrap_or(false) {
        return Ok(match execute(dir, &record, &name) {
            Ok(html) => Response {
                status: "200 OK",
                mime: if name.to_ascii_lowercase().ends_with(".js")
                    || name.eq_ignore_ascii_case("mp_js")
                {
                    "application/javascript; charset=windows-1251"
                } else if name.to_ascii_lowercase().ends_with(".css")
                    || name.eq_ignore_ascii_case("mp_css")
                {
                    "text/css; charset=windows-1251"
                } else {
                    "text/html; charset=windows-1251"
                },
                body: html,
                location: None,
                headers: Vec::new(),
            },
            Err(e) => message(
                if e.0.contains("KS throw:") {
                    "500 Internal Server Error"
                } else {
                    "501 Not Implemented"
                },
                e.0,
            ),
        });
    }
    // Only an actual binary field is a resource. Never use the largest field.
    if let Some(data) = crate::document::resource_bytes(&record, 4)? {
        return Ok(Response {
            status: "200 OK",
            mime: mime(&name, data),
            body: data.to_vec(),
            location: None,
            headers: Vec::new(),
        });
    }
    Ok(message("404 Not Found", "Object has no resource field 4"))
}
struct RequestJob {
    id: u64,
    request: RequestContext,
    authorization: Option<String>,
    reply: tokio::sync::oneshot::Sender<Response>,
    control: Arc<crate::execution::Control>,
    queued: tokio::sync::OwnedSemaphorePermit,
}
#[cfg(test)]
#[path = "server_scheduling_tests.rs"]
mod scheduling_tests;
struct Worker {
    sender: tokio::sync::mpsc::Sender<RequestJob>,
}
type WorkerInbox = Arc<std::sync::Mutex<tokio::sync::mpsc::Receiver<RequestJob>>>;
struct InterfaceBusy<'a>(&'a std::sync::atomic::AtomicUsize);
impl<'a> InterfaceBusy<'a> {
    fn enter(busy: &'a std::sync::atomic::AtomicUsize) -> Self {
        busy.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Self(busy)
    }
}
impl Drop for InterfaceBusy<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

fn worker_channels(layout: crate::pool_layout::Layout, capacity: usize) -> (Vec<Worker>, Vec<WorkerInbox>) {
    let mut workers: Vec<Worker> = Vec::with_capacity(layout.total);
    let mut inboxes: Vec<WorkerInbox> = Vec::with_capacity(layout.total);
    for index in 0..layout.total {
        let receiver = layout.receiver(index);
        if receiver < index {
            workers.push(Worker { sender: workers[receiver].sender.clone() });
            inboxes.push(inboxes[receiver].clone());
        } else {
            let (sender, receiver) = tokio::sync::mpsc::channel(capacity);
            workers.push(Worker { sender });
            inboxes.push(Arc::new(std::sync::Mutex::new(receiver)));
        }
    }
    (workers, inboxes)
}
struct Dispatcher {
    layout: crate::pool_layout::Layout,
    workers: Vec<Worker>,
    queue: Arc<tokio::sync::Semaphore>,
    ais_paths: Vec<String>,
    owner_reserve: Arc<tokio::sync::Semaphore>,
    interface_reserve: Arc<tokio::sync::Semaphore>,
    static_cache: Arc<static_cache::Cache>,
    static_checks: Arc<tokio::sync::Semaphore>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lane {
    Owner,
    Interface,
    Heavy,
    Search,
}
impl Lane {
    fn label(self) -> &'static str {
        match self {
            Self::Owner => "Владелец",
            Self::Interface => "Интерфейс",
            Self::Heavy => "Тяжёлые",
            Self::Search => "Поиск",
        }
    }
}
fn next_job(
    primary: &WorkerInbox,
    search: Option<&WorkerInbox>,
    interface: Option<&WorkerInbox>,
) -> Result<RequestJob, tokio::sync::mpsc::error::TryRecvError> {
    // Release each lock before checking the other queue. The primary queue
    // always wins; an already executing UI request is never preempted.
    let next = primary.lock().unwrap().try_recv();
    match next {
        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
            if let Some(search) = search {
                if let Ok(job) = search.lock().unwrap().try_recv() { return Ok(job); }
            }
            if let Some(interface) = interface {
                match interface.lock().unwrap().try_recv() {
                    Ok(job) => return Ok(job),
                    Err(_) => {}
                }
            }
            next
        }
        _ => next,
    }
}
fn notice_calculation(query: &str, request: &RequestContext) -> bool {
    // Only the explicit HTML calculation is read-only. Opening the form may
    // migrate column settings; save/clear and XML exports stay on the owner.
    if !request.uploads.is_empty() || (!request.body.is_empty() && request.form.is_empty()) {
        return false;
    }
    let mut values = HashMap::new();
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let (Ok(key), Ok(value)) = (
            percent_decode(key),
            percent_decode(&value.replace('+', " ")),
        ) else {
            return false;
        };
        if matches!(key.as_str(), "go" | "goxml" | "mode") {
            values.insert(key, value);
        }
    }
    // Match KS request binding: form values follow and override query values.
    for (key, value) in &request.form {
        if matches!(key.as_str(), "go" | "goxml" | "mode") {
            values.insert(key.clone(), value.clone());
        }
    }
    matches!(values.get("go").map(String::as_str), Some("ok" | "OK"))
        && values.get("goxml").map(String::as_str) != Some("1")
        && matches!(
            values.get("mode").map(String::as_str),
            None | Some("" | "0")
        )
}
fn lane(target: &str, request: &RequestContext) -> Lane {
    let method = request.method.as_str();
    let has_body = !request.body.is_empty() || !request.form.is_empty();
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let Ok(path) = percent_decode(path) else {
        return Lane::Owner;
    };
    if path != "/20a" && !path.starts_with("/20a/") {
        return Lane::Owner;
    }
    // Use the execution router's action precedence and empty-query handling.
    // A path looking like a UI form may select a different action in the query.
    let dir = VirtualDir {
        path: "/20a".into(),
        ..Default::default()
    };
    let Ok(name) = object_name(&path, query, &dir) else {
        return Lane::Owner;
    };
    if name.eq_ignore_ascii_case("regletters") {
        // Only the read-only complete range list can leave the owner. The
        // form and mutation modes share its native background-job queue.
        let mut mode = String::new();
        for part in query.split('&') {
            if let Some((key, value)) = part.split_once('=') {
                let (Ok(key), Ok(value)) = (percent_decode(key), percent_decode(&value.replace('+', " "))) else {
                    return Lane::Owner;
                };
                if key.eq_ignore_ascii_case("mode") { mode = value; }
            }
        }
        for (key, value) in &request.form {
            if key.eq_ignore_ascii_case("mode") { mode = value.clone(); }
        }
        return if matches!(method, "GET" | "POST") && mode == "1"
            && request.uploads.is_empty() && (request.body.is_empty() || !request.form.is_empty()) {
            Lane::Heavy
        } else { Lane::Owner };
    }
    if name == "anshlag" {
        return if notice_calculation(query, request) {
            Lane::Heavy
        } else {
            Lane::Owner
        };
    }
    // The bare catalogue only renders the statistics menu. The same script
    // calculates reports when supplied query/form parameters, even on GET.
    if name == "statistica1"
        && method == "GET"
        && !has_body
        && !query.split('&').any(|part| part.contains('='))
    {
        return Lane::Interface;
    }
    // Only routes whose request-local state has been verified are replicated.
    // Job queues, custom scripts and the complete Word protocol keep one owner.
    // User administration (/99 and /users) also stays on that owner: duplicate
    // checks and the following DB4 writes must not interleave across workers.
    if matches!(name.as_str(), "pfind" | "find" | "freetext") {
        return Lane::Search;
    }
    if matches!(
        name.as_str(),
        "DeleteObject"
            // Exact report entry points from the judicial-statistics catalogue.
            // statcard* editors may create/save DB4 records and keep the owner.
            | "statistica1"
            | "StatOtchet1"
            | "StatOtchet111"
            | "StatOtchet221"
            | "StatOtchetBankr"
            | "StatOtchetBankr11"
            | "StatOtchetBankr16"
            | "StatOtchetBankr22"
            | "StatOtchet191"
            | "statNoReglament"
            | "statNoReglament2"
            | "statNoReglament24"
            | "checkStatCard2"
            | "checkStatCard3"
            | "statReportCard"
            | "stat3"
            | "dateandjudgeanshlag"
            | "reportForPostServices"
            | "convertToExcel"
            | "converttoexcel"
    ) {
        Lane::Heavy
    } else if method == "GET"
        && (matches!(
            name.as_str(),
            "default"
                | "d"
                | "hdoc"
                | "doc"
                | "Word"
                | "ispravlenieForm"
                | "classifforispr"
                | "gen_anshlag"
        ) || name.starts_with("css/")
            || name.starts_with("images/")
            || name.starts_with("word/"))
    {
        Lane::Interface
    } else {
        Lane::Owner
    }
}
impl Dispatcher {
    fn submit(
        &self,
        id: u64,
        request: RequestContext,
        authorization: Option<String>,
        reply: tokio::sync::oneshot::Sender<Response>,
        control: Arc<crate::execution::Control>,
    ) -> Result<(), ()> {
        let count = self.workers.len();
        let decoded =
            percent_decode(request.target.split('?').next().unwrap_or("")).unwrap_or_default();
        let query = request
            .target
            .split_once('?')
            .map(|(_, q)| format!("?{q}"))
            .unwrap_or_default();
        let route = self.ais_paths.iter().find_map(|path| {
            let tail = decoded.strip_prefix(path)?;
            (tail.is_empty() || tail.starts_with('/')).then(|| format!("/20a{tail}{query}"))
        });
        let lane = route.map(|r| lane(&r, &request)).unwrap_or(Lane::Owner);
        if let Some((monitor, id)) = &request.monitor {
            monitor.group(*id, lane.label());
        }
        // A full heavy queue must not consume the last admission slot for the
        // UI or Word. These reserves do not admit heavy requests.
        let queued = self
            .queue
            .clone()
            .try_acquire_owned()
            .or_else(|error| {
                let reserve = match (&lane, count) {
                    (Lane::Interface, n) if n >= 3 => &self.interface_reserve,
                    (Lane::Owner | Lane::Interface, n) if n >= 2 => &self.owner_reserve,
                    _ => return Err(error),
                };
                reserve.clone().try_acquire_owned()
            })
            .map_err(|_| ())?;
        let index = match lane {
            Lane::Owner => 0,
            Lane::Interface => self.layout.interface(),
            Lane::Search => self.layout.search,
            Lane::Heavy => self.layout.bulk,
        };
        let worker = &self.workers[index];
        worker
            .sender
            .try_send(RequestJob {
                id,
                request,
                authorization,
                reply,
                control,
                queued,
            })
            .map_err(|_| ())
    }
}
pub fn dispatch_http(
    engine: &Arc<Db4Engine>,
    sessions: &mut vm::Sessions,
    mut request: RequestContext,
    authorization: Option<&str>,
) -> Response {
    let path = request.target.split('?').next().unwrap_or("");
    let path = match percent_decode(path) {
        Ok(p) => p,
        Err(e) => return message("400 Bad Request", e),
    };
    let Some(dir) = engine.resolve_virtual_dir(&path) else {
        return message("404 Not Found", "Virtual directory not found");
    };
    match crate::auth::check(engine, dir, authorization) {
        Ok(crate::auth::Access::Anonymous) => {}
        Ok(crate::auth::Access::Authenticated(user)) => request.user = Some(user),
        Ok(crate::auth::Access::Denied) => {
            let mut response = message("401 Unauthorized", "Authorization required");
            response
                .headers
                .push(("WWW-Authenticate".into(), "BASIC realm=\"KODEKS\"".into()));
            return response;
        }
        Err(e) => return message("501 Not Implemented", e),
    }
    request.authorization = authorization.map(str::to_owned);
    if matches!(path.trim_end_matches('/'), "/20a")
        && request
            .target
            .split_once('?')
            .is_some_and(|(_, q)| q == "saveRegistrationNumbers")
    {
        return registration::save(engine, &request, authorization);
    }
    if let (Some((monitor, id)), Some(user)) = (&request.monitor, &request.user) {
        monitor.user(*id, &user.login);
    }
    let mut metadata = None;
    let sessions = std::cell::RefCell::new(sessions);
    let mut response = route_inner(
        engine,
        &request.target,
        &mut |dir, record, name| {
            let (body, code, headers) = sessions.borrow_mut().execute_request(
                engine.clone(),
                dir,
                record,
                name,
                &request,
            )?;
            metadata = Some((code, headers));
            Ok(body)
        },
        Some(&mut |dir, oid| sessions.borrow_mut().read_record(engine.clone(), dir, oid)),
        Some(&mut |dir, name| sessions.borrow_mut().read_named(engine.clone(), dir, name)),
    )
    .unwrap_or_else(|e| message("422 Unprocessable Content", e));
    if response.status == "200 OK" {
        if let Some((code, headers)) = metadata {
            response.status = match code {
                200 if dir.mapped_to.eq_ignore_ascii_case("spdas.dbs")
                    && response.body.is_empty() =>
                {
                    "204 No Content"
                }
                200 => "200 OK",
                204 if response.body.is_empty() => "204 No Content",
                301 => "301 Moved Permanently",
                302 => "302 Found",
                303 => "303 See Other",
                304 => "304 Not Modified",
                400 => "400 Bad Request",
                401 => "401 Unauthorized",
                403 => "403 Forbidden",
                404 => "404 Not Found",
                405 => "405 Method Not Allowed",
                409 => "409 Conflict",
                500 => "500 Internal Server Error",
                _ => {
                    return message(
                        "501 Not Implemented",
                        format!("KS HTTP status {code} is not implemented"),
                    )
                }
            };
            response.headers = headers;
        }
    }
    response
}
fn dispatch(engine: &Arc<Db4Engine>, sessions: &mut vm::Sessions, target: &str) -> Response {
    route_inner(
        engine,
        target,
        &mut |dir, record, name| {
            sessions.execute_scheduled(engine.clone(), dir, record, name, target)
        },
        None,
        None,
    )
    .unwrap_or_else(|e| message("422 Unprocessable Content", e))
}
fn start_dispatcher(
    engine: Arc<Db4Engine>,
    monitor: Arc<crate::monitor::Monitor>,
    settings: &crate::settings::Settings,
    worker_count: usize,
) -> std::io::Result<Arc<Dispatcher>> {
    let root = engine
        .volumes
        .first()
        .and_then(|v| v.path.parent())
        .ok_or_else(|| std::io::Error::other("data root missing"))?
        .to_owned();
    let (warm_tx, warm_rx) = tokio::sync::mpsc::channel(32);
    let mut warm_rx = Some(warm_rx);
    let layout = crate::pool_layout::Layout::new(worker_count, settings.interface_workers)
        .map_err(std::io::Error::other)?;
    let (workers, inboxes) = worker_channels(layout, settings.queue + 1);
    let interface = (worker_count >= 3).then(|| inboxes[layout.interface()].clone());
    let search = (worker_count >= 4).then(|| inboxes[layout.search].clone());
    let interface_busy = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let search_busy = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for (index, rx) in inboxes.into_iter().enumerate() {
        let interface = if layout.helps_interface(index) { interface.clone() } else { None };
        let search = if index >= layout.bulk && worker_count >= 4 { search.clone() } else { None };
        let interface_busy = interface_busy.clone();
        let search_busy = search_busy.clone();
        let root = root.clone();
        let monitor = monitor.clone();
        let owner = if index == 0 {
            None
        } else {
            Some(warm_tx.clone())
        };
        let mut warming = if index == 0 { warm_rx.take() } else { None };
        std::thread::Builder::new()
            .name(format!("ks-runtime-{}", index + 1))
            .spawn(move || {
                // Caches and Rc-based KS values belong to this OS thread. Only native
                // file publication, named locks and cancellation controls are shared.
                let engine = match Db4Engine::init_from_config_root(root) {
                    Ok(e) => Arc::new(e),
                    Err(e) => {
                        eprintln!("KS worker {} initialization: {e}", index + 1);
                        return;
                    }
                };
                let mut sessions = owner.map(vm::Sessions::replica).unwrap_or_default();
                let mut tick = std::time::Instant::now();
                loop {
                    if let Some(warming) = warming.as_mut() {
                        while let Ok(dir) = warming.try_recv() {
                            if let Err(e) = sessions.warm(engine.clone(), &dir) {
                                eprintln!("KS owner startup: {}", e.0);
                            }
                        }
                    }
                    // Drop the inbox lock before any KS/DB execution, waiting,
                    // cancellation or sleep. Runtime state stays on its thread.
                    // Let an idle dedicated UI worker take its own next request.
                    // Otherwise a cold helper can turn a fast page into startup.
                    let help = interface
                        .as_ref()
                        .filter(|_| interface_busy.load(std::sync::atomic::Ordering::Acquire) >= layout.interfaces);
                    let search_help = search.as_ref()
                        .filter(|_| search_busy.load(std::sync::atomic::Ordering::Acquire) > 0);
                    let next = next_job(&rx, search_help, help);
                    match next {
                        Ok(RequestJob {
                            id,
                            request,
                            authorization,
                            reply,
                            control,
                            queued,
                        }) => {
                            let _busy = layout.is_interface(index)
                                .then(|| InterfaceBusy::enter(&interface_busy));
                            let _search_busy = (index == layout.search && worker_count >= 4)
                                .then(|| InterfaceBusy::enter(&search_busy));
                            drop(queued);
                            let _scope = crate::execution::Scope::enter(control.clone());
                            if reply.is_closed() || control.cancelled() {
                                monitor.finish(
                                    id,
                                    "клиент отключён до исполнения",
                                    crate::monitor::State::Cancelled,
                                );
                                continue;
                            }
                            monitor.executor(id, index + 1);
                            monitor.state(id, crate::monitor::State::Running);
                            let r = dispatch_http(
                                &engine,
                                &mut sessions,
                                request,
                                authorization.as_deref(),
                            );
                            if control.cancelled() {
                                monitor.finish(
                                    id,
                                    &format!(
                                        "отменено; завершённых записей DB4: {}",
                                        control.commits()
                                    ),
                                    crate::monitor::State::Cancelled,
                                );
                                continue;
                            }
                            let status = r.status;
                            monitor.state(id, crate::monitor::State::Writing);
                            if reply.send(r).is_err() {
                                monitor.finish(
                                    id,
                                    status,
                                    if status.starts_with('2') {
                                        crate::monitor::State::Done
                                    } else {
                                        crate::monitor::State::Error
                                    },
                                );
                            }
                        }
                        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                            std::thread::sleep(std::time::Duration::from_millis(10))
                        }
                    }
                    // Exactly one runtime owns each catalog's At / mp_g_JobsQueue.
                    if index == 0 && tick.elapsed() >= std::time::Duration::from_millis(100) {
                        match sessions.due() {
                            Ok(paths) => {
                                for path in paths {
                                    let id = monitor.start(&path, "At", true);
                                    monitor.executor(id, index + 1);
                                    monitor.state(id, crate::monitor::State::Running);
                                    let r = dispatch(&engine, &mut sessions, &path);
                                    monitor.finish(
                                        id,
                                        r.status,
                                        if r.status.starts_with('2') {
                                            crate::monitor::State::Done
                                        } else {
                                            crate::monitor::State::Error
                                        },
                                    );
                                    if r.status != "200 OK" {
                                        eprintln!(
                                            "At {path}: {}: {}",
                                            r.status,
                                            String::from_utf8_lossy(&r.body)
                                        );
                                    }
                                }
                            }
                            Err(e) => eprintln!("At clock: {e}"),
                        }
                        monitor.schedules(sessions.scheduled());
                        tick = std::time::Instant::now();
                    }
                }
            })?;
    }
    Ok(Arc::new(Dispatcher {
        layout,
        workers,
        queue: Arc::new(tokio::sync::Semaphore::new(settings.queue)),
        ais_paths: engine
            .virtual_dirs
            .iter()
            .filter(|d| d.mapped_to.eq_ignore_ascii_case("spb.dbs"))
            .map(|d| d.path.trim_end_matches('/').to_string())
            .collect(),
        owner_reserve: Arc::new(tokio::sync::Semaphore::new(1)),
        interface_reserve: Arc::new(tokio::sync::Semaphore::new(1)),
        static_cache: Arc::new(static_cache::Cache::default()),
        static_checks: Arc::new(tokio::sync::Semaphore::new(4)),
    }))
}
async fn reject_overloaded(stream: &mut TcpStream) {
    let _ = tokio::time::timeout(std::time::Duration::from_millis(100), async {
        stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await?;
        // Closing a TCP socket with unread request bytes can reset the peer
        // before it receives 503. Send FIN, then drain until the client closes.
        // The whole rejection, including a stalled peer, keeps the 100 ms bound.
        stream.shutdown().await?;
        let mut discarded = [0u8; 1024];
        while stream.read(&mut discarded).await? != 0 {}
        Ok::<(), std::io::Error>(())
    }).await;
}

pub async fn run_server(engine: Arc<Db4Engine>, port: u16) -> std::io::Result<()> {
    let settings = Arc::new(crate::settings::Store::load()?);
    let host=settings.host.value.as_str();
    let listener = TcpListener::bind((host, port)).await?;
    let port = listener.local_addr()?.port();
    crate::listen_config::bound(listener.local_addr()?);
    eprintln!("codex-AIs HTTP server: http://{host}:{port}; KS compatibility runtime");
    eprintln!("HTTP interfaces: {} ({}); local browser: http://localhost:{port}/20a/",host,settings.host.source);
    let connections = Arc::new(tokio::sync::Semaphore::new(settings.active.connections));
    eprintln!("KS pool: {}", settings.worker_selection.explanation);
    let monitor = Arc::new(crate::monitor::Monitor::new(
        settings.active.clone(),
        settings.worker_selection.clone(),
    ));
    let status_checks = Arc::new(tokio::sync::Semaphore::new(4));
    let dispatcher = start_dispatcher(
        engine.clone(),
        monitor.clone(),
        &settings.active,
        settings.worker_selection.workers,
    )?;
    loop {
        let (mut stream, peer) = listener.accept().await?;
        let Ok(permit) = connections.clone().try_acquire_owned() else {
            reject_overloaded(&mut stream).await;
            continue;
        };
        monitor.connected();
        let monitor = monitor.clone();
        let status_checks = status_checks.clone();
        let settings = settings.clone();
        let e = engine.clone();
        let dispatcher = dispatcher.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _guard = crate::monitor::ConnectionGuard {
                monitor: monitor.clone(),
            };
            if let Err(err) = connection(
                stream,
                e,
                dispatcher,
                monitor.clone(),
                peer.to_string(),
                status_checks,
                settings,
            )
            .await
            {
                eprintln!("HTTP error: {err}");
            }
        });
    }
}
async fn connection(
    mut stream: TcpStream,
    engine: Arc<Db4Engine>,
    dispatcher: Arc<Dispatcher>,
    monitor: Arc<crate::monitor::Monitor>,
    peer: String,
    status_checks: Arc<tokio::sync::Semaphore>,
    settings: Arc<crate::settings::Store>,
) -> std::io::Result<()> {
    let mut pending = Vec::new();
    for sequence in 0..1000 {
        // Idle keep-alive sockets do not occupy a KS executor or an active request.
        if pending.is_empty() {
            let mut buffer = [0u8; 2048];
            let seconds = if sequence == 0 {
                settings.active.header_seconds
            } else {
                settings.active.keep_alive_seconds
            };
            match tokio::time::timeout(
                std::time::Duration::from_secs(seconds),
                stream.read(&mut buffer),
            )
            .await
            {
                Ok(Ok(0)) | Err(_) => return Ok(()),
                Ok(Err(e)) => return Err(e),
                Ok(Ok(n)) => pending.extend_from_slice(&buffer[..n]),
            }
        }
        let id = monitor.start("", &peer, false);
        let control = Arc::new(crate::execution::Control::default());
        let _guard = crate::monitor::RequestGuard {
            monitor: monitor.clone(),
            id,
            control: control.clone(),
        };
        match connection_request(
            &mut stream,
            &mut pending,
            &engine,
            &dispatcher,
            &monitor,
            id,
            &status_checks,
            &settings,
            control,
            sequence < 999,
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => return Ok(()),
            Err(e) => {
                monitor.finish(id, "ошибка передачи HTTP", crate::monitor::State::Error);
                return Err(e);
            }
        }
    }
    Ok(())
}
async fn disconnected(stream: &mut TcpStream, pending: &mut Vec<u8>) {
    let mut buffer = [0u8; 2048];
    loop {
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            // Preserve pipelined data and still detect a later FIN/RST. Bound
            // prefetch to the largest supported request body plus its headers.
            Ok(n) => {
                if pending.len() + n > 16 * 1024 * 1024 + 16384 {
                    return;
                }
                pending.extend_from_slice(&buffer[..n]);
            }
        }
    }
}
async fn connection_request(
    stream: &mut TcpStream,
    pending: &mut Vec<u8>,
    engine: &Arc<Db4Engine>,
    dispatcher: &Arc<Dispatcher>,
    monitor: &Arc<crate::monitor::Monitor>,
    id: u64,
    status_checks: &Arc<tokio::sync::Semaphore>,
    settings: &Arc<crate::settings::Store>,
    control: Arc<crate::execution::Control>,
    allow_keep_alive: bool,
) -> std::io::Result<bool> {
    let mut request = std::mem::take(pending);
    let mut keep_alive = false;
    let mut buffer = [0u8; 2048];
    let header_deadline = tokio::time::Instant::now()
        + std::time::Duration::from_secs(settings.active.header_seconds);
    let response = loop {
        if !request.windows(4).any(|s| s == b"\r\n\r\n") {
            if request.len() >= 16384 {
                break message(
                    "431 Request Header Fields Too Large",
                    "Request headers too large",
                );
            }
            let n = match tokio::time::timeout_at(header_deadline, stream.read(&mut buffer)).await {
                Ok(v) => v?,
                Err(_) => break message("408 Request Timeout", "Request timeout"),
            };
            if n == 0 {
                return Ok(false);
            }
            request.extend_from_slice(&buffer[..n]);
        }
        if request.windows(4).any(|s| s == b"\r\n\r\n") {
            let line = request.split(|&b| b == b'\n').next().unwrap_or(&[]);
            let text = String::from_utf8_lossy(line);
            let parts: Vec<_> = text.split_whitespace().collect();
            if parts.len() != 3
                || !parts[1].starts_with('/')
                || !matches!(parts[2], "HTTP/1.0" | "HTTP/1.1")
            {
                break message("400 Bad Request", "Invalid request line");
            }
            let word_path = parts[1]
                .split('?')
                .next()
                .is_some_and(|p| p.trim_end_matches('/') == "/99");
            if !matches!(parts[0], "GET" | "POST") && !(parts[0] == "PUT" && word_path) {
                break message(
                    "405 Method Not Allowed",
                    "Use GET, POST, or PUT for the Word catalog",
                );
            }
            let mut context = RequestContext {
                target: parts[1].to_string(),
                monitor: Some((monitor.clone(), id)),
                method: parts[0].to_string(),
                protocol: parts[2].to_string(),
                ..Default::default()
            };
            monitor.target(id, parts[1]);
            let mut authorization = None;
            let method = parts[0].to_owned();
            let header_end = request.windows(4).position(|s| s == b"\r\n\r\n").unwrap() + 4;
            if header_end > 16384 {
                break message(
                    "431 Request Header Fields Too Large",
                    "Request headers too large",
                );
            }
            let mut content_length = 0usize;
            let mut content_type = String::new();
            let mut connection_tokens = String::new();
            let text = String::from_utf8_lossy(&request);
            let mut seen = std::collections::HashSet::new();
            let mut invalid = false;
            for line in text.split("\r\n").skip(1).take_while(|s| !s.is_empty()) {
                let Some((name, value)) = line.split_once(':') else {
                    invalid = true;
                    break;
                };
                let key = name.to_ascii_lowercase();
                if !seen.insert(key.clone()) {
                    invalid = true;
                    break;
                }
                let value = value.trim();
                match key.as_str() {
                    "connection" => connection_tokens = value.to_ascii_lowercase(),
                    "authorization" => authorization = Some(value.to_owned()),
                    "host" => context.host = value.into(),
                    "cookie" => context.cookie = value.into(),
                    "user-agent" => context.user_agent = value.into(),
                    "if-modified-since" => context.if_modified_since = value.into(),
                    "if-none-match" => context.if_none_match = value.into(),
                    "content-length" => match value.parse::<usize>() {
                        Ok(n)
                            if n <= if crate::http_form::upload_route(&context.target) {
                                crate::http_form::MAX_UPLOAD_BODY
                            } else if word_path {
                                16 * 1024 * 1024
                            } else {
                                1024 * 1024
                            } =>
                        {
                            content_length = n
                        }
                        _ => invalid = true,
                    },
                    "content-type" => content_type = value.into(),
                    "transfer-encoding" => invalid = true,
                    _ => {}
                }
            }
            if invalid {
                break message("400 Bad Request", "Invalid or duplicate request headers");
            }
            if method == "GET" && content_length > 0 {
                break message("400 Bad Request", "GET body is not supported");
            }
            if matches!(method.as_str(), "POST" | "PUT") {
                let deadline = tokio::time::Instant::now()
                    + std::time::Duration::from_secs(settings.active.body_seconds);
                while request.len() < header_end + content_length {
                    let count = (header_end + content_length - request.len()).min(buffer.len());
                    match tokio::time::timeout_at(deadline, stream.read(&mut buffer[..count])).await
                    {
                        Ok(Ok(n)) if n > 0 => request.extend_from_slice(&buffer[..n]),
                        _ => {
                            invalid = true;
                            break;
                        }
                    }
                }
                if invalid {
                    break message("400 Bad Request", "Incomplete request body");
                }
                let raw_spd = method == "PUT";
                context.body = request[header_end..header_end + content_length].to_vec();
                context.form = if raw_spd {
                    Vec::new()
                } else {
                    match crate::http_form::decode_request(
                        &context.target,
                        &content_type,
                        &request[header_end..header_end + content_length],
                    ) {
                        Ok((fields, uploads)) => {
                            context.uploads = uploads;
                            fields
                        }
                        Err(e) => break message("400 Bad Request", e),
                    }
                };
            }
            *pending = request.split_off(header_end + content_length);
            let tokens: Vec<_> = connection_tokens.split(',').map(str::trim).collect();
            keep_alive = allow_keep_alive
                && settings.active.keep_alive_seconds > 0
                && !tokens.contains(&"close")
                && (context.protocol == "HTTP/1.1" || tokens.contains(&"keep-alive"));
            if context.target == "/health" {
                break route(engine, &context.target);
            }
            let (status_path, status_query) = context
                .target
                .split_once('?')
                .unwrap_or((&context.target, ""));
            if matches!(status_path, "/admin" | "/admin/")
                && matches!(status_query.split('&').next(), Some("status" | "advanced"))
            {
                let advanced = status_query.split('&').next() == Some("advanced");
                if !advanced && method != "GET" {
                    break message("405 Method Not Allowed", "Monitor is read-only");
                }
                let Ok(permit) = status_checks.clone().try_acquire_owned() else {
                    break message("503 Service Unavailable", "Monitor authentication limit");
                };
                monitor.state(id, crate::monitor::State::Status);
                let e = engine.clone();
                let access = tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    let dir = e
                        .resolve_virtual_dir("/admin/")
                        .ok_or("Admin directory missing")?;
                    crate::auth::check(&e, dir, authorization.as_deref())
                })
                .await;
                match access {
                    Ok(Ok(crate::auth::Access::Authenticated(user))) => {
                        monitor.user(id, &user.login)
                    }
                    Ok(Ok(_)) => {
                        let mut r =
                            message("401 Unauthorized", "Administrator authorization required");
                        r.headers
                            .push(("WWW-Authenticate".into(), "BASIC realm=\"KODEKS\"".into()));
                        break r;
                    }
                    _ => {
                        break message("500 Internal Server Error", "Monitor authentication failed")
                    }
                }
                if advanced {
                    let settings = settings.clone();
                    let engine=engine.clone();
                    break tokio::task::spawn_blocking(move || {
                        settings.advanced(&engine,&method, &context.form)
                    })
                    .await
                    .unwrap_or_else(|_| {
                        message("500 Internal Server Error", "Settings operation failed")
                    });
                }
                break Response {
                    status: "200 OK",
                    mime: "text/html; charset=utf-8",
                    body: monitor.html().into_bytes(),
                    location: None,
                    headers: vec![("Cache-Control".into(), "no-store".into())],
                };
            }
            if static_cache::candidate(&context) {
                // Bounded independently of monitor authentication and KS. On
                // saturation use the ordinary dispatcher, with its admission limits.
                if let Ok(permit) = dispatcher.static_checks.clone().try_acquire_owned() {
                    let cache = dispatcher.static_cache.clone();
                    let e = engine.clone();
                    let request = context.clone();
                    let auth = authorization.clone();
                    let scope = control.clone();
                    let answer = tokio::task::spawn_blocking(move || {
                        let _permit = permit;
                        let _scope = crate::execution::Scope::enter(scope);
                        cache.serve(&e, &request, auth.as_deref())
                    });
                    let result = tokio::select! {
                        result = answer => result,
                        _ = disconnected(stream, pending) => { control.cancel(); return Ok(false); }
                    };
                    match result {
                        Ok(Some(response)) => break response,
                        Ok(None) => {}
                        Err(_) => {
                            break message(
                                "500 Internal Server Error",
                                "Static resource worker failed",
                            )
                        }
                    }
                }
            }
            let (reply, answer) = tokio::sync::oneshot::channel();
            let (chunks, events) = streaming::channel();
            context.response_stream = Some(chunks);
            let http11 = context.protocol == "HTTP/1.1";
            monitor.state(id, crate::monitor::State::Queued);
            if dispatcher
                .submit(id, context, authorization, reply, control.clone())
                .is_err()
            {
                break message(
                    "503 Service Unavailable",
                    "KS queue full or dispatcher stopped",
                );
            }
            match streaming::wait(
                stream, pending, events, answer, keep_alive, http11,
                &settings.active, monitor, id, &control,
            ).await? {
                streaming::Answer::Buffered(response) => break response,
                streaming::Answer::Sent(keep_alive) => return Ok(keep_alive),
            }
        }
    };
    monitor.state(id, crate::monitor::State::Writing);
    let content_type = response
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.as_str())
        .unwrap_or(response.mime);
    let connection = if keep_alive { "keep-alive" } else { "close" };
    let mut header = format!("HTTP/1.1 {}\r\nContent-Type: {content_type}\r\nConnection: {connection}\r\nX-Content-Type-Options: nosniff\r\n", response.status);
    if !matches!(response.status, "204 No Content" | "304 Not Modified") {
        header.push_str(&format!("Content-Length: {}\r\n", response.body.len()));
    }
    if keep_alive {
        header.push_str(&format!(
            "Keep-Alive: timeout={}\r\n",
            settings.active.keep_alive_seconds
        ));
    }
    for (name, value) in &response.headers {
        if !name.eq_ignore_ascii_case("content-type") {
            header.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    if let Some(loc) = response.location {
        header.push_str(&format!("Location: {loc}\r\n"));
    }
    header.push_str("\r\n");
    tokio::time::timeout(
        std::time::Duration::from_secs(settings.active.write_seconds),
        async {
            stream.write_all(header.as_bytes()).await?;
            stream.write_all(&response.body).await?;
            if !keep_alive {
                stream.shutdown().await?;
            }
            Ok::<(), std::io::Error>(())
        },
    )
    .await
    .map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::TimedOut, "HTTP response write timeout")
    })??;
    monitor.finish(
        id,
        response.status,
        if response.status.starts_with('2') || response.status.starts_with('3') {
            crate::monitor::State::Done
        } else {
            crate::monitor::State::Error
        },
    );
    Ok(keep_alive)
}
