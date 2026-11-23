//! Ready-to-send public asset bytes only. Never execute KS or cache its output.
//! Authentication and source validation precede every hit, including HTTP 304.
use super::{
    message, object_name, percent_decode, physical_path, physical_resource, RequestContext,
    Response,
};
use crate::db::Db4Engine;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_ENTRIES: usize = 1024;
const MAX_ASSET: usize = 4 * 1024 * 1024;
type Stamp = (PathBuf, u64, u64, SystemTime);
struct Entry {
    stamp: Stamp,
    body: Vec<u8>,
    mime: &'static str,
    etag: String,
}
#[derive(Default)]
struct Entries {
    items: HashMap<String, (Arc<Entry>, u64)>,
    bytes: usize,
    tick: u64,
}
pub(super) struct Cache {
    entries: Mutex<Entries>,
    nonce: String,
}
impl Default for Cache {
    fn default() -> Self {
        Self {
            entries: Mutex::new(Entries::default()),
            nonce: format!(
                "{:x}-{:x}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
                std::process::id()
            ),
        }
    }
}
fn asset_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(name.as_str(), "mp_css" | "mp_js")
        || [
            ".css", ".js", ".png", ".gif", ".jpg", ".jpeg", ".svg", ".ico",
        ]
        .iter()
        .any(|ext| name.ends_with(ext))
}
pub(super) fn candidate(request: &RequestContext) -> bool {
    if request.method != "GET" || !request.body.is_empty() || !request.form.is_empty() {
        return false;
    }
    let (path, query) = request
        .target
        .split_once('?')
        .unwrap_or((&request.target, ""));
    // The original pages use both /20a/image.png and /20a/?image.png.
    // Parameters (nd, mode, user, versions, ...) are never cache keys.
    let action = query.trim_end_matches('&');
    if action.contains(['&', '=']) {
        return false;
    }
    percent_decode(if action.is_empty() { path } else { action }).is_ok_and(|s| asset_name(&s))
}
fn matches_etag(condition: &str, etag: &str) -> bool {
    condition.trim() == "*"
        || condition
            .split(',')
            .any(|v| v.trim().strip_prefix("W/").unwrap_or(v.trim()) == etag)
}
fn response(entry: &Entry, request: &RequestContext) -> Response {
    let unchanged = matches_etag(&request.if_none_match, &entry.etag);
    Response {
        status: if unchanged {
            "304 Not Modified"
        } else {
            "200 OK"
        },
        mime: entry.mime,
        body: if unchanged {
            Vec::new()
        } else {
            entry.body.clone()
        },
        location: None,
        // The browser can retain bytes but must revalidate access and revision.
        headers: vec![
            ("ETag".into(), entry.etag.clone()),
            ("Cache-Control".into(), "private, no-cache".into()),
        ],
    }
}
impl Cache {
    fn get(&self, key: &str, stamp: &Stamp) -> Option<Arc<Entry>> {
        let mut cache = self.entries.lock().unwrap();
        cache.tick += 1;
        let tick = cache.tick;
        if let Some((entry, touched)) = cache.items.get_mut(key) {
            if entry.stamp == *stamp {
                *touched = tick;
                return Some(entry.clone());
            }
        }
        if let Some((old, _)) = cache.items.remove(key) {
            cache.bytes -= old.body.len();
        }
        None
    }
    fn insert(&self, key: &str, stamp: Stamp, r: Response) -> Arc<Entry> {
        let mut cache = self.entries.lock().unwrap();
        // Concurrent misses share one validator for the same validated revision.
        if let Some((entry, _)) = cache.items.get(key) {
            if entry.stamp == stamp {
                return entry.clone();
            }
        }
        cache.tick += 1;
        let tick = cache.tick;
        let entry = Arc::new(Entry {
            stamp,
            body: r.body,
            mime: r.mime,
            etag: format!("\"codex-{}-{tick:x}\"", self.nonce),
        });
        if let Some((old, _)) = cache.items.remove(key) {
            cache.bytes -= old.body.len();
        }
        // Oversized assets still work; these are cache budgets, not HTTP limits.
        if entry.body.len() <= MAX_ASSET {
            while cache.bytes + entry.body.len() > MAX_BYTES || cache.items.len() >= MAX_ENTRIES {
                let oldest = cache
                    .items
                    .iter()
                    .min_by_key(|(_, (_, touched))| touched)
                    .map(|(key, _)| key.clone())
                    .unwrap();
                let (old, _) = cache.items.remove(&oldest).unwrap();
                cache.bytes -= old.body.len();
            }
            cache.bytes += entry.body.len();
            cache.items.insert(key.to_owned(), (entry.clone(), tick));
        }
        entry
    }
    pub(super) fn serve(
        &self,
        engine: &Db4Engine,
        request: &RequestContext,
        authorization: Option<&str>,
    ) -> Option<Response> {
        if !candidate(request) {
            return None;
        }
        match self.read(engine, request, authorization) {
            Ok(response) => response,
            Err(e) => Some(message("422 Unprocessable Content", e)),
        }
    }
    fn read(
        &self,
        engine: &Db4Engine,
        request: &RequestContext,
        authorization: Option<&str>,
    ) -> Result<Option<Response>, String> {
        let (path, query) = request
            .target
            .split_once('?')
            .unwrap_or((&request.target, ""));
        let path = percent_decode(path)?;
        let Some(dir) = engine.resolve_virtual_dir(&path) else {
            return Ok(None);
        };
        let access = match crate::auth::check(engine, dir, authorization) {
            Ok(access) => access,
            Err(e) => return Ok(Some(message("501 Not Implemented", e))),
        };
        match access {
            crate::auth::Access::Denied => {
                let mut r = message("401 Unauthorized", "Authorization required");
                r.headers
                    .push(("WWW-Authenticate".into(), "BASIC realm=\"KODEKS\"".into()));
                return Ok(Some(r));
            }
            crate::auth::Access::Authenticated(user) => {
                if let Some((monitor, id)) = &request.monitor {
                    monitor.user(*id, &user.login);
                }
            }
            crate::auth::Access::Anonymous => {}
        }
        let name = object_name(&path, query, dir)?;
        let stamp = || -> Result<Option<Stamp>, String> {
            if dir.kind == 3 {
                return engine.resource_stamp(&dir.mapped_to);
            }
            if dir.kind != 1 {
                return Ok(None);
            }
            // Resolve and check containment again on every hit, including symlinks.
            let file = match physical_path(engine, dir, &path) {
                Ok(file) => file,
                Err(_) => return Ok(None),
            };
            if !file
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(asset_name)
            {
                return Ok(None);
            }
            let meta = std::fs::metadata(&file).map_err(|e| e.to_string())?;
            Ok(meta
                .is_file()
                .then(|| {
                    meta.modified()
                        .map(|modified| (file, 0, meta.len(), modified))
                })
                .transpose()
                .map_err(|e| e.to_string())?)
        };
        let Some(before) = stamp()? else {
            return Ok(None);
        };
        // Include the canonical source path even though a listener has one root.
        let key = format!("{}\n{}", before.0.display(), request.target);
        if let Some(entry) = self.get(&key, &before) {
            if let Some((monitor, id)) = &request.monitor {
                monitor.group(*id, "Статика (кэш)");
            }
            return Ok(Some(response(&entry, request)));
        }
        let r = if dir.kind == 1 {
            physical_resource(engine, dir, &path)?
        } else {
            let Some(record) = engine.named(&dir.mapped_to, &name)? else {
                return Ok(None);
            };
            if record.code().is_some() || record.source().is_some_and(|s| !s.is_empty()) {
                return Ok(None);
            }
            let Some(data) = crate::document::resource_bytes(&record, 4)? else {
                return Ok(None);
            };
            if crate::document::has_rtf_header(data) {
                return Ok(None);
            }
            Response {
                status: "200 OK",
                mime: super::mime(&name, data),
                body: data.to_vec(),
                location: None,
                headers: Vec::new(),
            }
        };
        if r.status != "200 OK" {
            return Ok(Some(r));
        }
        // Never associate bytes read across a publication with either generation.
        if stamp()?.as_ref() != Some(&before) {
            return Ok(None);
        }
        let entry = self.insert(&key, before, r);
        if let Some((monitor, id)) = &request.monitor {
            monitor.group(*id, "Статика");
        }
        Ok(Some(response(&entry, request)))
    }
}
