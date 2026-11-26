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
        if values.len()>3 || values.iter().take(2).any(|v|!matches!(v,Value::Null))
            || values.get(2).is_some_and(|v|!matches!(v,Value::Null|Value::Bool(_))) {
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
    pub(super) fn find_attrs(&mut self, values:&[Value])->Result<Value,Fault>{
        let [Value::Array(selected)]=values else{return Err("FindAttrs expects an array of search branches".into());};
        let Value::Application(app)=self.get("Application")? else{return Err("Application missing".into());};
        let pool=app.borrow().get("\0find_objects").cloned().ok_or("Search branches missing")?;
        let count=selected.borrow().get("length").ok_or("Array length missing")?.number()? as usize;
        let mut attrs=Vec::new();let mut area=None;let mut seen=std::collections::HashSet::new();
        for i in 0..count {
            let index=selected.borrow().get(&i.to_string()).cloned().unwrap_or(Value::Null).text();
            let branch=self.member(pool.clone(),index)?;
            let a=self.member(branch.clone(),"Area".into())?.number()? as u16;
            area=Some(if area.is_none_or(|old|old==a){a}else{u16::MAX});
            let fields=self.member(branch,"Fields".into())?;
            if let Value::Array(fields)=fields {
                let count=fields.borrow().get("length").ok_or("Array length missing")?.number()? as usize;
                for i in 0..count {
                    let item=fields.borrow().get(&i.to_string()).ok_or("Find attribute missing")?.text();
                    let (number,name)=item.split_once(';').unwrap_or((&item,""));
                    let attr=number.trim().parse::<u16>().map_err(|_|"Invalid find attribute")?;
                    if seen.insert((a,attr)){attrs.push(object(HashMap::from([("Name".into(),Value::String(name.trim().into())),("Attr".into(),Value::Number(attr as f64)),("Area".into(),Value::Number(a as f64))])));}
                }
            }
        }
        let result=array(attrs);
        if let Value::Array(m)=&result {
            let mut m=m.borrow_mut();m.insert("\0enumerate_numeric".into(),Value::Bool(true));
            m.insert("Area".into(),Value::Number(area.unwrap_or(u16::MAX) as f64));
            m.insert("GetLinks".into(),Value::Native("findAttrs.GetLinks".into()));
        }
        Ok(result)
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
        self.refresh_system_settings()?;
        self.scheduled_request = scheduled;
        let (path, query) = request
            .target
            .split_once('?')
            .unwrap_or((&request.target, ""));
        let command = query.split('&').next().unwrap_or("");
        let command = command.strip_suffix('=').unwrap_or(command);
        self.statistics_cache_allowed = statistics_cache::request_allowed(
            path, command, &request.method, request.user.is_some(), scheduled);
        let mut participant_mode = String::new();
        for (key, value) in query.split('&').filter_map(|p| p.split_once('=')) {
            if crate::server::percent_decode(key)?.eq_ignore_ascii_case("mode") {
                participant_mode = crate::server::percent_decode(&value.replace('+', " "))?;
            }
        }
        for (key, value) in &request.form {
            if key.eq_ignore_ascii_case("mode") {
                participant_mode = value.clone();
            }
        }
        // Native registration reserves a case and its number while opening the
        // form; subsequent POSTs attach parties and complete the registration.
        // The complete postal range scan is read-only and can be larger than
        // one million rows. Other reports retain their materialized SQL cells.
        self.cursor_projections = !scheduled && matches!(path, "/20a" | "/20a/")
            && command.eq_ignore_ascii_case("regletters") && participant_mode == "1";
        self.registration_write_allowed = !scheduled
            && request.user.is_some()
            && matches!(path, "/20a" | "/20a/")
            && matches!(command, "regdelo" | "regdoc");
        self.attachment_write_allowed = self.registration_write_allowed
            || !scheduled
                && request.user.is_some()
                && matches!(path, "/20a" | "/20a/")
                && command == "savetempfile"
            || !scheduled
                && request.user.is_some()
                && matches!(path, "/99" | "/99/")
                && matches!(request.method.as_str(), "PUT" | "POST")
                && matches!(command, "savebinpdf" | "savesigforpdf");
        self.document_write_allowed = !scheduled
            && request.user.is_some()
            && (self.registration_write_allowed || matches!(path, "/99" | "/99/")
                // /users is authenticated against its own configured ACL before
                // KS starts. Its native account form saves groups, H (disabled)
                // and expiration in UserBase via POST users?users.
                || matches!(path, "/users" | "/users/")
                    && request.method == "POST" && command == "users"
                // The native calendar creates a missing year on opening it.
                // Vacation edits are submitted by its separate staff form.
                || matches!(path, "/20a" | "/20a/")
                    && (command == "newJudAndHoliday"
                        // The bankruptcy schedule initializes its column settings
                        // on first opening; both native forms persist them in DB4.
                        || command == "bankrotanshlag"
                        || request.method == "POST" && command == "regletters"
                            && matches!(participant_mode.as_str(), "2" | "4" | "6")
                        || request.method == "POST" && command == "anshlag"
                        || request.method == "POST" && matches!(command, "makeVacation" | "regassessors")
                        // The original assessor form removes a period by AJAX GET.
                        || command == "regassessors" && request.method == "GET" && participant_mode == "5")
                || matches!(path, "/20a" | "/20a/") && command == "commonajaxpm"
                    && matches!(participant_mode.as_str(), "11"|"12"|"14"|"27"|"34")
                || matches!(path, "/20a" | "/20a/")
                    && request.method == "POST"
                    && matches!(command, "ispravlenieForm" | "DeleteObject" | "statcard171" | "statcard111"));
        self.schedule_export_allowed = self.document_write_allowed
            && matches!(path, "/20a" | "/20a/") && command == "anshlag"
            && request.method == "POST";
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
        if path.trim_end_matches('/') == "/20a" && command == "raspred_settings" {
            path_object
                .borrow_mut()
                .insert("SaveState".into(), Value::Native("path.SaveState".into()));
        }
        if path.trim_end_matches('/') == "/20a" && command == "makeVacation" {
            path_object.borrow_mut().insert(
                "SaveState".into(),
                Value::Native("path.SaveCalendarAccess".into()),
            );
        }
        let directory = Rc::new(RefCell::new(directory.borrow().clone()));
        directory
            .borrow_mut()
            .insert("\0typename".into(), Value::String("Connection".into()));
        self.scopes[0]
            .borrow_mut()
            .insert("this".into(), Value::Object(directory.clone()));
        self.http_stream = None;
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
        let query_fields = if command.eq_ignore_ascii_case("find") {
            crate::http_form::decode_for(&request.target, "application/x-www-form-urlencoded", query.as_bytes())?
        } else {
            query.split('&').filter_map(|p| p.split_once('=')).map(|(k,v)|
                Ok((crate::server::percent_decode(k)?,crate::server::percent_decode(&v.replace('+'," "))?))
            ).collect::<Result<Vec<_>,String>>()?
        };
        for (key, value) in query_fields {
            {
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
                    Value::String(value),
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
        for upload in &request.uploads {
            self.check_request_key(&upload.name)?;
            let value = InputStream::value(&upload.data);
            dir.borrow_mut().insert(upload.name.clone(), value.clone());
            self.scopes[0]
                .borrow_mut()
                .insert(upload.name.clone(), value);
            self.request_parameters.push(upload.name.clone());
        }
        if command == "raspred_settings" && request.method == "POST" {
            // Unchecked controls are absent HTTP arguments. Do not inherit the
            // previous directory value through Connection's global lookup.
            for name in crate::vm::distribution::setting_names() {
                if !request
                    .form
                    .iter()
                    .any(|(k, _)| k.eq_ignore_ascii_case(name))
                {
                    dir.borrow_mut().insert(name.into(), Value::Null);
                    self.scopes[0].borrow_mut().insert(name.into(), Value::Null);
                    self.request_parameters.push(name.into());
                }
            }
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
        self.abort_statistics_cache();
        self.abort_system_settings();
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
        self.cursor_projections = false;
        self.schedule_export_allowed = false;
        self.kod_snapshot_loaded = false;
        self.scheduled_request = false;
        self.registration_write_allowed = false;
        self.attachment_write_allowed = false;
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
        self.rtf_styles.borrow_mut().take();
        if let Some((globals, directory, properties)) = self.request_baseline.take() {
            *self.scopes[0].borrow_mut() = globals;
            *directory.borrow_mut() = properties;
        }
        self.request_parameters.clear();
        self.profile = None;
        self.output_stream = None;
        self.http_stream = None;
        self.release_unused_databases();
    }
    /// Only known native jobs with a recorded creator may write from At.
    /// Their submission and execution still pass through the native KS queue.
    pub(super) fn writable_job_principal(&self, script: &str) -> bool {
        let Ok(Value::Object(principal)) = self.get("mp_g_ascJobPrincipal") else {
            return false;
        };
        let principal = principal.borrow();
        let mode = principal.get("nJobPagemode").map(Value::text).unwrap_or_default();
        // Stored URL names can differ in case from the name used at submission
        // (regLetters in the volume, regletters in the HTTP request).
        self.path.trim_end_matches('/') == "/20a"
            && matches!(principal.get("sJobUrlName"), Some(Value::String(name)) if name.eq_ignore_ascii_case(script))
            && match script.to_ascii_lowercase().as_str() {
                "newjudandholiday" => mode == "3",
                "regletters" => matches!(mode.as_str(), "3" | "5" | "7"),
                _ => false,
            }
            && matches!(principal.get("oidJobCreator"), Some(Value::Number(oid)) if oid.is_finite() && *oid > 0.0)
    }
    /// The catalog owner cannot block waiting for its own At queue. Run the
    /// authenticated calendar job at this explicit join point, retaining the
    /// native job's status, completion signal and exception handling. Other
    /// queued work is left to the scheduler; HTTP metadata is restored even
    /// when the job fails.
    pub(super) fn run_waited_calendar_job(
        &mut self,
        args: &[Value],
        receiver: Option<&Value>,
    ) -> Result<Option<Value>, Fault> {
        if !self.document_write_allowed
            || self.scheduled_request
            || args.len() != 2
            || args[1].number()? != -1.0
        {
            return Ok(None);
        }
        let Some(Value::Object(queue)) = receiver else {
            return Ok(None);
        };
        let Some(Value::Array(jobs)) = queue.borrow().get("aJobs").cloned() else {
            return Ok(None);
        };
        let id = args[0].number()?;
        let job = jobs.borrow().values().find_map(|value| {
            let Value::Object(job) = value else { return None };
            let data = job.borrow();
            (matches!(data.get("idJob"), Some(Value::Number(n)) if *n == id)
                && matches!(data.get("sJobUrlName"), Some(Value::String(n)) if n == "newJudAndHoliday")
                && data.get("nJobPagemode").is_some_and(|mode| mode.text() == "3"))
                .then(|| job.clone())
        });
        let Some(job) = job else { return Ok(None) };
        let pending = self.get("MP_AT_JOB_STATE_PENDING")?.number()?;
        if !matches!(job.borrow().get("nJobState"), Some(Value::Number(n)) if *n == pending) {
            return Ok(None);
        }
        let execute = job
            .borrow()
            .get("_executeJob")
            .cloned()
            .ok_or("calendar job has no executor")?;
        let request = self.get("Request")?;
        let mut fields = match &request {
            Value::ReadOnlyObject(fields) | Value::Object(fields) => fields.borrow().clone(),
            _ => return Err("calendar job has no request context".into()),
        };
        let directory = match self.scopes[0].borrow().get("this").cloned() {
            Some(Value::Object(directory)) => directory,
            _ => return Err("calendar job has no directory context".into()),
        };
        let saved_globals = self.scopes[0].borrow().clone();
        let saved_directory = directory.borrow().clone();
        if let Some((globals, _, properties)) = &self.request_baseline {
            *self.scopes[0].borrow_mut() = globals.clone();
            *directory.borrow_mut() = properties.clone();
        }
        fields.insert("protocol".into(), Value::Null);
        fields.insert("method".into(), Value::Null);
        let running = self.get("MP_AT_JOB_STATE_EXECUTES")?;
        job.borrow_mut().insert("nJobState".into(), running);
        job.borrow_mut()
            .insert("sJobStatus".into(), Value::String("выполняется".into()));
        let mutable_request = Value::Object(Rc::new(RefCell::new(fields)));
        self.scopes[0]
            .borrow_mut()
            .insert("Request".into(), mutable_request.clone());
        directory
            .borrow_mut()
            .insert("Request".into(), mutable_request);
        self.scheduled_request = true;
        let result = self.call_host(
            execute,
            vec![],
            Some(Value::Object(job)),
            "_executeJob".into(),
        );
        self.scheduled_request = false;
        *self.scopes[0].borrow_mut() = saved_globals;
        *directory.borrow_mut() = saved_directory;
        result?;
        Ok(Some(Value::Bool(false)))
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
