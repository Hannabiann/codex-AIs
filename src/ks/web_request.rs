//! Host objects for one authenticated HTTP request. Passwords never enter KS.
use super::*;

fn object(fields: HashMap<String, Value>) -> Value {
    Value::ReadOnlyObject(Rc::new(RefCell::new(fields)))
}
fn config(
    values: &std::collections::BTreeMap<String, crate::db::ConfigValue>,
) -> HashMap<String, Value> {
    values
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                match value {
                    crate::db::ConfigValue::Text(s) => Value::String(s.clone()),
                    crate::db::ConfigValue::Integer(n) => Value::Number(*n as f64),
                    crate::db::ConfigValue::Invalid(s) => Value::Unavailable(s.clone()),
                },
            )
        })
        .collect()
}
fn array(values: impl IntoIterator<Item = Value>) -> Value {
    let mut fields: HashMap<_, _> = values
        .into_iter()
        .enumerate()
        .map(|(i, v)| (i.to_string(), v))
        .collect();
    fields.insert("length".into(), Value::Number(fields.len() as f64));
    Value::Array(Rc::new(RefCell::new(fields)))
}
impl Runtime<'_> {
    pub(super) fn get_find_objects(&mut self, values: &[Value]) -> Result<Value, Fault> {
        if !values.is_empty() {
            return Err("GetFindObjects optional arguments not implemented".into());
        }
        let Value::Application(app) = self.get("Application")? else {
            return Err("FindObjects application missing".into());
        };
        let pool = array([]);
        app.borrow_mut()
            .insert("\0find_objects".into(), pool.clone());
        let ids = {
            let host = self
                .dbhost
                .as_ref()
                .ok_or("FindObjects requires database")?
                .borrow();
            let schema = host.schema(0)?;
            let area = schema
                .areas
                .get(&8)
                .ok_or("FindObjects topic area absent")?;
            host.query(
                0,
                &format!("SELECT OID FROM \"{}\"", area.name.replace('"', "\"\"")),
            )?
        };
        for id in ids {
            let record = self.read_document(id, 0)?;
            for field in record
                .fields
                .iter()
                .filter(|f| f.id == 20 && f.wire_type == 5)
            {
                if let crate::db::fields::Value::Integer(script) = field.value {
                    self.execute_document(script, 0, 3, Some(Value::Document(id, 0)))?;
                }
            }
        }
        Ok(pool)
    }
    pub(super) fn add_find_object(&mut self, values: Vec<Value>) -> Result<Value, Fault> {
        if !(6..=7).contains(&values.len()) {
            return Err("FindObjects.AddObject expects topic, section, area, name, conditions, parent[, fields]".into());
        }
        let Value::Application(app) = self.get("Application")? else {
            return Err("FindObjects application missing".into());
        };
        let Some(Value::Array(pool)) = app.borrow().get("\0find_objects").cloned() else {
            return Err("FindObjects collection is not active".into());
        };
        let length = pool.borrow()["length"].number()? as usize;
        if length >= 256 {
            return Err("FindObjects limit".into());
        }
        let level = if matches!(values[5], Value::Null) {
            0.0
        } else {
            self.member(values[5].clone(), "Level".into())?.number()? + 1.0
        };
        let item = object(HashMap::from([
            ("Topic".into(), values[0].clone()),
            ("Razdel".into(), values[1].clone()),
            ("Area".into(), values[2].clone()),
            ("Name".into(), values[3].clone()),
            ("Conditions".into(), values[4].clone()),
            ("Parent".into(), values[5].clone()),
            ("Level".into(), Value::Number(level)),
            (
                "Fields".into(),
                values.get(6).cloned().unwrap_or(Value::Null),
            ),
        ]));
        pool.borrow_mut().insert(length.to_string(), item.clone());
        pool.borrow_mut()
            .insert("length".into(), Value::Number((length + 1) as f64));
        Ok(item)
    }
    fn check_request_key(&self, key: &str) -> Result<(), String> {
        if key.starts_with('\0')
            || matches!(
                key,
                "this"
                    | "Base"
                    | "Path"
                    | "AuthParam"
                    | "Globals"
                    | "WorkJud"
                    | "WorkYear"
                    | "Request"
                    | "Server"
                    | "Application"
                    | "UserBase"
                    | "Output"
                    | "DriverManager"
                    | "RegistryRoot"
                    | "Document"
                    | "Array"
                    | "Object"
                    | "Math"
                    | "parseInt"
                    | "parseFloat"
                    | "String"
                    | "Number"
                    | "Boolean"
                    | "eval"
                    | "escape"
                    | "unescape"
                    | "InetConnection"
                    | "FindOrCreateHashObject"
                    | "toLog"
            )
        {
            return Err("reserved request parameter".into());
        }
        if self.scopes[0].borrow().get(key).is_some_and(|v| {
            !matches!(
                v,
                Value::Null | Value::String(_) | Value::Number(_) | Value::Bool(_)
            )
        }) {
            return Err("request parameter cannot replace a runtime object".into());
        }
        Ok(())
    }
    pub fn with_http_paths(self, dirs: &[crate::db::VirtualDir]) -> Self {
        if let Some(dir) = dirs.iter().find(|d| d.path == self.path) {
            if let Value::Object(this) = &self.scopes[0].borrow()["this"] {
                this.borrow_mut()
                    .insert("AuthParam".into(), object(config(&dir.auth)));
            }
        }
        // PathPool enumerates the routing table: longest paths first, then name.
        // This is observable in the original menu (/100..170 precede /10..98).
        let mut ordered: Vec<_> = dirs.iter().collect();
        ordered.sort_by(|a, b| {
            b.path
                .len()
                .cmp(&a.path.len())
                .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
        });
        let paths = array(ordered.into_iter().map(|dir| {
            let mut fields = config(&dir.properties);
            fields.insert("AuthParam".into(), object(config(&dir.auth)));
            // The original routing entries expose the configured BaseN mounts
            // as a one-based vol array. Export opens a separate database from
            // these descriptors; do not share foreign runtime volume handles.
            let mut volumes = HashMap::new();
            let mut index = 1usize;
            while let Some(crate::db::ConfigValue::Text(path)) =
                dir.properties.get(&format!("Base{index}"))
            {
                let path = self.data_root.as_ref().map_or_else(
                    || path.clone(),
                    |root| {
                        crate::db::script_volume_path(std::path::Path::new(root), path)
                            .to_string_lossy()
                            .into_owned()
                    },
                );
                volumes.insert(
                    index.to_string(),
                    object(HashMap::from([(
                        "vol".into(),
                        object(HashMap::from([
                            ("FileName".into(), Value::String(path)),
                            // All host mounts are read-only in this runtime.
                            ("readOnly".into(), Value::Bool(true)),
                        ])),
                    )])),
                );
                index += 1;
            }
            volumes.insert("length".into(), Value::Number(index as f64));
            fields.insert("vol".into(), Value::Array(Rc::new(RefCell::new(volumes))));
            object(fields)
        }));
        if let Some(Value::Application(app)) = self.scopes[0].borrow().get("Application") {
            app.borrow_mut().insert("Paths".into(), paths);
        }
        self
    }
    /// Only serializable backup descriptors cross catalog scopes. Functions and
    /// database handles belong to their original runtime and are never shared.
    pub fn backup_descriptors(&self) -> Vec<(String, String, String)> {
        let Ok(Value::Object(dir)) = self.get("this") else {
            return vec![];
        };
        let Some(Value::Array(files)) = dir.borrow().get("copy_files").cloned() else {
            return vec![];
        };
        let files = files.borrow();
        let count = files
            .get("length")
            .and_then(|v| v.number().ok())
            .unwrap_or(0.0) as usize;
        (0..count.min(64))
            .filter_map(|i| {
                let Value::Object(file) = files.get(&i.to_string())? else {
                    return None;
                };
                let file = file.borrow();
                Some((
                    file.get("Name")?.text(),
                    file.get("To")?.text(),
                    file.get("Desc")?.text(),
                ))
            })
            .collect()
    }
    pub fn set_backup_descriptors(&mut self, path: &str, files: Vec<(String, String, String)>) {
        let Ok(Value::Application(app)) = self.get("Application") else {
            return;
        };
        let Some(Value::Array(paths)) = app.borrow().get("Paths").cloned() else {
            return;
        };
        for value in paths.borrow().values() {
            let Value::ReadOnlyObject(dir) = value else {
                continue;
            };
            if dir.borrow().get("Path").map(Value::text).as_deref() != Some(path) {
                continue;
            }
            let descriptors = array(files.iter().map(|(name, to, desc)| {
                object(HashMap::from([
                    ("Name".into(), Value::String(name.clone())),
                    ("To".into(), Value::String(to.clone())),
                    ("Desc".into(), Value::String(desc.clone())),
                ]))
            }));
            dir.borrow_mut()
                .insert("copy_files".into(), descriptors.clone());
            dir.borrow_mut().insert(
                "Globals".into(),
                object(HashMap::from([("copy_files".into(), descriptors)])),
            );
        }
    }
    pub fn begin_request(&mut self, request: &crate::server::RequestContext) -> Result<(), String> {
        self.begin_connection(request, false)
    }
    /// Only the in-process At dispatcher calls this entry point. A scheduler
    /// Connection has a Request, but no HTTP protocol or authenticated user.
    pub fn begin_scheduled_request(&mut self, target: &str) -> Result<(), String> {
        self.begin_connection(
            &crate::server::RequestContext {
                target: target.into(),
                ..Default::default()
            },
            true,
        )
    }
    fn begin_connection(
        &mut self,
        request: &crate::server::RequestContext,
        scheduled: bool,
    ) -> Result<(), String> {
        self.end_request();
        let (path, query) = request
            .target
            .split_once('?')
            .unwrap_or((&request.target, ""));
        let command = query.split('&').next().unwrap_or("");
        self.document_write_allowed = !scheduled
            && request.user.is_some()
            && (matches!(path, "/99" | "/99/")
                || matches!(path, "/20a" | "/20a/")
                    && request.method == "POST"
                    && matches!(command, "ispravlenieForm" | "DeleteObject"));
        self.http_input = InputStream::value(&request.body);
        let Value::Object(directory) = self.get("this").map_err(|e| e.to_string())? else {
            return Err("catalog object missing".into());
        };
        self.request_baseline = Some((
            self.scopes[0].borrow().clone(),
            directory.clone(),
            directory.borrow().clone(),
        ));
        self.scopes[0]
            .borrow_mut()
            .insert("socket".into(), Value::HttpOutput);
        // Startup libraries keep references to the virtual directory. A request
        // is a distinct Connection, never a mutation of that persistent object.
        let path_object = directory.clone();
        let directory = Rc::new(RefCell::new(directory.borrow().clone()));
        directory
            .borrow_mut()
            .insert("\0typename".into(), Value::String("Connection".into()));
        self.scopes[0]
            .borrow_mut()
            .insert("this".into(), Value::Object(directory.clone()));
        self.http_output.borrow_mut().clear();
        // These values describe this HTTP listener, independent of the legacy
        // server's compression and trusted-proxy configuration.
        let listener_globals = object(HashMap::from([
            ("DeflateLevel".into(), Value::Number(0.0)),
            ("RealIP".into(), Value::Number(0.0)),
        ]));
        directory
            .borrow_mut()
            .insert("Globals".into(), listener_globals.clone());
        path_object
            .borrow_mut()
            .insert("Globals".into(), listener_globals);
        if let Some(Value::Application(app)) = self.scopes[0].borrow().get("Application") {
            app.borrow_mut().insert(
                "CurrentUsers".into(),
                Value::Number(
                    request
                        .monitor
                        .as_ref()
                        .map(|(m, _)| m.active_users())
                        .unwrap_or(usize::from(request.user.is_some())) as f64,
                ),
            );
        }
        self.http_output
            .borrow_mut()
            .insert("CharSet".into(), Value::String("windows-1251".into()));
        // This listener is mounted at the HTTP root; it has no reverse-proxy prefix.
        self.scopes[0].borrow_mut().insert(
            "Server".into(),
            object(HashMap::from([(
                "prefix".into(),
                Value::String(String::new()),
            )])),
        );
        let Value::Object(dir) = self.get("this").map_err(|e| e.to_string())? else {
            return Err("catalog object missing".into());
        };
        for name in self.request_parameters.drain(..) {
            dir.borrow_mut().remove(&name);
        }
        dir.borrow_mut().remove("mode");
        let (_, query) = request
            .target
            .split_once('?')
            .unwrap_or((&request.target, ""));
        for pair in query.split('&') {
            if let Some((key, value)) = pair.split_once('=') {
                let key = crate::server::percent_decode(key)?;
                self.check_request_key(&key)?;
                // Parameters cannot replace the execution database or directory metadata.
                if matches!(
                    key.as_str(),
                    "Base" | "Path" | "AuthParam" | "Globals" | "WorkJud" | "WorkYear"
                ) {
                    return Err("reserved request parameter".into());
                }
                dir.borrow_mut().insert(
                    key.clone(),
                    Value::String(crate::server::percent_decode(&value.replace('+', " "))?),
                );
                self.scopes[0]
                    .borrow_mut()
                    .insert(key.clone(), dir.borrow()[&key].clone());
                self.request_parameters.push(key);
            }
        }
        for (key, value) in &request.form {
            self.check_request_key(key)?;
            if matches!(
                key.as_str(),
                "Base" | "Path" | "AuthParam" | "Globals" | "WorkJud" | "WorkYear"
            ) {
                return Err("reserved form parameter".into());
            }
            dir.borrow_mut()
                .insert(key.clone(), Value::String(value.clone()));
            self.scopes[0]
                .borrow_mut()
                .insert(key.clone(), Value::String(value.clone()));
            self.request_parameters.push(key.clone());
        }
        // readVar(Connection) in the Word library assigns the keys of its
        // first input line to the request object, just like query parameters.
        if dir
            .borrow()
            .get("overfile")
            .is_some_and(|value| !matches!(value, Value::Null))
        {
            let line = request
                .body
                .split(|b| matches!(b, b'\r' | b'\n'))
                .next()
                .unwrap_or_default();
            for pair in line.split(|b| *b == b'&') {
                if let Some(at) = pair.iter().position(|b| *b == b'=') {
                    self.check_request_key(&crate::db::decode(&pair[..at]))?;
                }
            }
        }
        let user = if let Some(user) = &request.user {
            if self.user_scope.is_none() {
                let mut host = self
                    .dbhost
                    .as_ref()
                    .ok_or("user database host missing")?
                    .borrow_mut();
                let scope = host.create()?;
                let script = self
                    .config
                    .as_ref()
                    .and_then(|c| c.borrow().get("AuthScript").cloned())
                    .ok_or("AuthScript missing")?
                    .text();
                host.open(scope, &script, &[true])
                    .map_err(|e| format!("UserBase script: {e:?}"))?;
                host.open(scope, "ubase.db4", &[true])
                    .map_err(|e| format!("UserBase.Open: {e:?}"))?;
                self.user_scope = Some(scope);
            }
            let db = self
                .database_for(self.user_scope.unwrap())
                .map_err(|e| e.to_string())?;
            self.scopes[0]
                .borrow_mut()
                .insert("UserBase".into(), db.clone());
            if let Value::Application(app) = self.get("Application").map_err(|e| e.to_string())? {
                app.borrow_mut().insert(
                    "UserBase".into(),
                    object(HashMap::from([
                        ("Base".into(), db),
                        ("Path".into(), Value::String("/users".into())),
                    ])),
                );
            }
            object(HashMap::from([
                ("\0text".into(), Value::String(user.login.clone())),
                ("Name".into(), Value::String(user.login.clone())),
                ("FullName".into(), Value::String(user.full_name.clone())),
                ("LogID".into(), Value::Number(user.oid as f64)),
                (
                    "Groups".into(),
                    array(user.groups.iter().map(|n| Value::Number(*n as f64))),
                ),
            ]))
        } else {
            Value::Null
        };
        let fields = HashMap::from([
            (
                "AUTHORIZATION".into(),
                request
                    .authorization
                    .as_ref()
                    .map(|s| Value::String(s.clone()))
                    .unwrap_or(Value::Null),
            ),
            (
                "protocol".into(),
                if scheduled {
                    Value::Null
                } else {
                    Value::String(if request.protocol.is_empty() {
                        "HTTP/1.1".into()
                    } else {
                        request.protocol.clone()
                    })
                },
            ),
            (
                "method".into(),
                if scheduled {
                    Value::Null
                } else {
                    Value::String(if request.method.is_empty() {
                        "GET".into()
                    } else {
                        request.method.clone()
                    })
                },
            ),
            (
                "USER_AGENT".into(),
                Value::String(request.user_agent.clone()),
            ),
            (
                "nPath".into(),
                Value::Number(
                    request
                        .target
                        .split('?')
                        .next()
                        .unwrap_or("")
                        .bytes()
                        .filter(|b| *b == b'/')
                        .count() as f64
                        - 2.0,
                ),
            ),
            ("User".into(), user),
            (
                "user".into(),
                request
                    .user
                    .as_ref()
                    .map(|u| Value::String(u.login.clone()))
                    .unwrap_or(Value::Null),
            ),
            ("PathObject".into(), Value::Object(path_object)),
            ("URI".into(), Value::String(request.target.clone())),
            (
                "URL".into(),
                Value::String(if scheduled {
                    self.path.trim_end_matches('/').into()
                } else {
                    request.target.split('?').next().unwrap_or("").into()
                }),
            ),
            (
                "URLdir".into(),
                Value::String(self.path.trim_end_matches('/').into()),
            ),
            ("QueryArgs".into(), Value::String(query.into())),
            ("HOST".into(), Value::String(request.host.clone())),
            ("COOKIE".into(), Value::String(request.cookie.clone())),
            (
                "IF_MODIFIED_SINCE".into(),
                Value::String(request.if_modified_since.clone()),
            ),
        ]);
        // mp_jobs changes URI and User while dispatching a queued job. HTTP
        // request metadata stays read-only and cannot impersonate this context.
        let value = if scheduled {
            Value::Object(Rc::new(RefCell::new(fields)))
        } else {
            object(fields)
        };
        self.scopes[0].borrow_mut().insert("Request".into(), value);
        directory
            .borrow_mut()
            .insert("Request".into(), self.scopes[0].borrow()["Request"].clone());
        Ok(())
    }
    pub fn end_request(&mut self) {
        // Fatal cancellation bypasses KS finally; do not strand a named lock.
        self.mutex_handles.retain(|handle| {
            if let Some(handle) = handle.upgrade() {
                if crate::execution::cancelled() {
                    let _ = handle.borrow_mut().release();
                }
                true
            } else {
                false
            }
        });
        self.numeric_arrays.clear();
        self.schema_links.clear();
        self.document_write_allowed = false;
        for doc in self.document_edits.borrow().values() {
            doc.borrow_mut().locked = false;
        }
        self.document_edits.borrow_mut().clear();
        self.http_input = InputStream::value(&[]);
        if let Some(host) = &self.dbhost {
            host.borrow().finish_profile();
        }
        for scope in self.request_document_scopes.drain(..) {
            scope.borrow_mut().clear();
        }
        for object in self.document_vars.borrow().values() {
            object.borrow_mut().clear();
        }
        self.document_vars.borrow_mut().clear();
        self.closure_cells
            .borrow_mut()
            .retain(|cell| cell.strong_count() > 0);
        self.form_tabs.borrow_mut().clear();
        self.form_fields.borrow_mut().clear();
        self.context_marks.clear();
        self.rich_context_marks.clear();
        if let Some((globals, directory, properties)) = self.request_baseline.take() {
            *self.scopes[0].borrow_mut() = globals;
            *directory.borrow_mut() = properties;
        }
        self.request_parameters.clear();
        self.profile = None;
        self.output_stream = None;
    }
    /// Replica startup supplies functions and DB contexts, never another timer queue.
    /// Stateful job-control routes are always dispatched to the catalog owner.
    pub fn with_replica_startup(mut self, replica: bool) -> Self {
        self.http_replica = replica;
        self.replica_startup = replica;
        self
    }
    pub fn make_http_replica(&mut self) {
        self.scheduler = Default::default();
        self.http_replica = true;
        self.replica_startup = false;
        if let Ok(Value::Object(queue)) = self.get("mp_g_JobsQueue") {
            if let Some(Value::Array(jobs)) = queue.borrow().get("aJobs").cloned() {
                jobs.borrow_mut().clear();
                jobs.borrow_mut()
                    .insert("length".into(), Value::Number(0.0));
            }
        }
    }
    /// Fatal host faults intentionally bypass KS catch/finally. Keep the
    /// original queue's failed job visible instead of leaving it EXECUTES.
    /// Do not run arbitrary cleanup KS, retry, or report an export as successful.
    pub fn fail_scheduled_job(&mut self, message: &str) -> Result<(), String> {
        let Ok(Value::Object(queue)) = self.get("mp_g_JobsQueue") else {
            return Ok(());
        };
        let Some(Value::Array(jobs)) = queue.borrow().get("aJobs").cloned() else {
            return Ok(());
        };
        let running = self
            .get("MP_AT_JOB_STATE_EXECUTES")
            .map_err(|e| e.to_string())?
            .number()
            .map_err(|e| e.to_string())?;
        let crashed = self
            .get("MP_AT_JOB_STATE_CRASHED")
            .map_err(|e| e.to_string())?;
        let ended = Value::Date(Rc::new(RefCell::new(super::super::date::Date::now()?)));
        for value in jobs.borrow().values() {
            let Value::Object(job) = value else { continue };
            let mut job = job.borrow_mut();
            if !matches!(job.get("nJobState"), Some(Value::Number(state)) if *state == running) {
                continue;
            }
            job.insert("nJobState".into(), crashed.clone());
            job.insert("sJobException".into(), Value::String(message.into()));
            job.insert(
                "sJobStatus".into(),
                Value::String(format!("ошибка исполнения ({message})")),
            );
            job.insert("dtJobEnded".into(), ended.clone());
            if let Some(Value::Signal(signal)) = job.get("sigJobEnded") {
                signal.switch(true)?;
            }
        }
        Ok(())
    }
    pub fn response_headers(&self) -> Result<(u16, Vec<(String, String)>), String> {
        let fields = self.http_output.borrow();
        let code = match fields.get("Code") {
            None => 200,
            Some(Value::Number(n)) if n.fract() == 0.0 && *n >= 100.0 && *n <= 599.0 => *n as u16,
            _ => return Err("invalid HTTP response code".into()),
        };
        let mut headers = Vec::new();
        for (name, value) in fields.iter().filter(|(k, _)| {
            !matches!(
                k.as_str(),
                "Code" | "CharSet" | "Escape" | "escapeStr" | "\0buffering"
            )
        }) {
            let value = value.text();
            let name = if name.eq_ignore_ascii_case("Content_Type") {
                "Content-Type"
            } else {
                name.as_str()
            };
            if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                || value.chars().any(char::is_control)
            {
                return Err("invalid HTTP response header".into());
            }
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "content-length" | "transfer-encoding" | "connection"
            ) {
                return Err("transport header cannot be set by KS".into());
            }
            headers.push((name.to_owned(), value));
        }
        Ok((code, headers))
    }
}
