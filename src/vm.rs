use crate::{
    db::{decode, fields::Record},
    ks,
};
#[path = "distribution.rs"]
pub(crate) mod distribution;
#[derive(Debug)]
pub struct VmError(pub String);
pub fn named_with_startup(
    engine: &crate::db::Db4Engine,
    dir: &crate::db::VirtualDir,
    name: &str,
) -> Result<Option<Record>, String> {
    create_runtime(engine, dir.clone())
        .map_err(|e| e.0)?
        .read_named_document(name)
}
pub fn read_with_startup(
    engine: &crate::db::Db4Engine,
    dir: &crate::db::VirtualDir,
    oid: u32,
) -> Result<Option<Record>, String> {
    create_runtime(engine, dir.clone())
        .map_err(|e| e.0)?
        .read_loaded_document(oid)
}

pub fn execute_with_startup(
    engine: &crate::db::Db4Engine,
    dir: &crate::db::VirtualDir,
    record: &Record,
    object: &str,
) -> Result<Vec<u8>, VmError> {
    let runtime = create_runtime(engine, dir.clone())?;
    let source = record
        .source()
        .ok_or_else(|| VmError(format!("{object}: KS source missing")))?;
    let ast = ks::parser::parse(&decode(source)).map_err(|e| VmError(format!("{object}: {e}")))?;
    let html = runtime
        .run(&ast)
        .map_err(|e| VmError(format!("{object}: {e}")))?;
    let (bytes, _, errors) = encoding_rs::WINDOWS_1251.encode(&html);
    if errors {
        return Err(VmError("output cannot be encoded as Windows-1251".into()));
    }
    Ok(bytes.into_owned())
}

fn create_runtime<'a>(
    engine: impl std::ops::Deref<Target = crate::db::Db4Engine> + Clone + 'a,
    dir: crate::db::VirtualDir,
) -> Result<ks::runtime::Runtime<'a>, VmError> {
    create_runtime_mode(engine, dir, false)
}
fn create_runtime_mode<'a>(
    engine: impl std::ops::Deref<Target = crate::db::Db4Engine> + Clone + 'a,
    dir: crate::db::VirtualDir,
    replica: bool,
) -> Result<ks::runtime::Runtime<'a>, VmError> {
    let root = engine
        .volumes
        .first()
        .and_then(|v| v.path.parent())
        .ok_or_else(|| VmError("data root missing".into()))?
        .to_owned();
    let schema = engine.schema_for_dir(&dir).map_err(VmError)?;
    let host = crate::db::context::Contexts::new(engine.clone(), &dir).map_err(VmError)?;
    let mut runtime = ks::runtime::Runtime::new(&dir.path)
        .with_schema(&schema)
        .with_directory_properties(&dir.properties)
        .with_data_root(&root.to_string_lossy())
        .with_database_host(host)
        .with_registry(&engine.registry)
        .with_application_config(&engine.web_config)
        .with_http_paths(&engine.virtual_dirs)
        .with_replica_startup(replica);
    if let Some(startup) = engine.named(&dir.mapped_to, "startup").map_err(VmError)? {
        let source = startup
            .source()
            .ok_or_else(|| VmError("startup has no KS source".into()))?;
        if let Err(e) = runtime.load(&decode(source)) {
            runtime.end_request();
            return Err(VmError(format!("{}/startup: {e}", dir.mapped_to)));
        }
        if std::env::var_os("KODEKS_TRACE").is_some_and(|s| s == "1") {
            eprintln!(
                "KS startup completed: {} instructions",
                runtime.instruction_count()
            );
        }
    }
    Ok(runtime)
}
#[derive(Default)]
pub struct Sessions {
    failures: std::collections::HashMap<String, String>,
    runtimes: std::collections::HashMap<String, ks::runtime::Runtime<'static>>,
    owner: Option<tokio::sync::mpsc::Sender<crate::db::VirtualDir>>,
}
impl Sessions {
    pub fn replica(owner: tokio::sync::mpsc::Sender<crate::db::VirtualDir>) -> Self {
        Self {
            owner: Some(owner),
            ..Default::default()
        }
    }
    pub fn warm(
        &mut self,
        engine: std::sync::Arc<crate::db::Db4Engine>,
        dir: &crate::db::VirtualDir,
    ) -> Result<(), VmError> {
        self.ensure_runtime(engine, dir)
    }
    pub fn scheduled(&self) -> Vec<String> {
        self.runtimes
            .values()
            .flat_map(|r| r.scheduler.snapshot())
            .collect()
    }
    pub fn read_named(
        &mut self,
        engine: std::sync::Arc<crate::db::Db4Engine>,
        dir: &crate::db::VirtualDir,
        name: &str,
    ) -> Result<Option<Record>, String> {
        self.ensure_runtime(engine, dir).map_err(|e| e.0)?;
        self.runtimes[&dir.path].read_named_document(name)
    }
    pub fn read_record(
        &mut self,
        engine: std::sync::Arc<crate::db::Db4Engine>,
        dir: &crate::db::VirtualDir,
        oid: u32,
    ) -> Result<Option<Record>, String> {
        self.ensure_runtime(engine, dir).map_err(|e| e.0)?;
        self.runtimes[&dir.path].read_loaded_document(oid)
    }
    pub fn execute_request(
        &mut self,
        engine: std::sync::Arc<crate::db::Db4Engine>,
        dir: &crate::db::VirtualDir,
        record: &Record,
        object: &str,
        request: &crate::server::RequestContext,
    ) -> Result<(Vec<u8>, u16, Vec<(String, String)>), VmError> {
        // admin/pref lists descriptors created by the configured users startup.
        // Do not invent filenames or share scope-bound KS functions/DB handles.
        if dir.mapped_to.eq_ignore_ascii_case("admin.dbs") && object == "pref" {
            if let Some(crate::db::ConfigValue::Text(script)) = engine.web_config.get("AuthScript")
            {
                if let Some(users) = engine
                    .virtual_dirs
                    .iter()
                    .find(|d| d.mapped_to.eq_ignore_ascii_case(script))
                {
                    self.ensure_runtime(engine.clone(), users)?;
                }
            }
        }
        self.ensure_runtime(engine.clone(), dir)?;
        if dir.mapped_to.eq_ignore_ascii_case("admin.dbs") && object == "pref" {
            let descriptors = self
                .runtimes
                .iter()
                .map(|(path, runtime)| (path.clone(), runtime.backup_descriptors()))
                .collect::<Vec<_>>();
            let runtime = self.runtimes.get_mut(&dir.path).unwrap();
            for (path, files) in descriptors {
                if !files.is_empty() {
                    runtime.set_backup_descriptors(&path, files);
                }
            }
        }
        let result = (|| {
            self.runtimes
                .get_mut(&dir.path)
                .unwrap()
                .begin_request(request)
                .map_err(VmError)?;
            if dir.path.trim_end_matches('/') == "/20a"
                && object == "RaspredSettingsBlockedInnForJudges"
                && record.source().is_none_or(|s| s.is_empty())
                && record.code().is_some()
            {
                return distribution::page(
                    &engine,
                    dir,
                    self.runtimes.get_mut(&dir.path).unwrap(),
                    request,
                )
                .map_err(VmError);
            }
            if dir.path.trim_end_matches('/') == "/20a"
                && matches!(
                    object,
                    "raspred_settings" | "raspredkategory" | "change_difficult_katsp" | "restorekp"
                )
            {
                let runtime = self.runtimes.get_mut(&dir.path).unwrap();
                if let Some(reply) = distribution::guard_child(&engine, request).map_err(VmError)? {
                    return Ok(reply);
                }
                runtime.allow_distribution_writes(request.method == "POST");
                if object == "raspred_settings" {
                    if let Some(reply) =
                        distribution::guard_compiled(&engine, runtime, request).map_err(VmError)?
                    {
                        return Ok(reply);
                    }
                }
            }
            let mut body = self.render_record(dir, record, object)?;
            if object == "raspred_settings" {
                body = distribution::decorate_compiled(
                    &engine,
                    &self.runtimes[&dir.path],
                    request,
                    body,
                )
                .map_err(VmError)?;
            }
            let (code, mut headers) = self.runtimes[&dir.path]
                .response_headers()
                .map_err(VmError)?;
            if object == "raspred_settings" {
                headers.push(("Cache-Control".into(), "no-store".into()));
            }
            Ok((body, code, headers))
        })();
        // Also restore on script/host errors, before any scheduled task can run.
        self.runtimes.get_mut(&dir.path).unwrap().end_request();
        if self.owner.is_some() && crate::execution::cancelled() {
            self.runtimes.remove(&dir.path);
        }
        result
    }
    pub fn execute_scheduled(
        &mut self,
        engine: std::sync::Arc<crate::db::Db4Engine>,
        dir: &crate::db::VirtualDir,
        record: &Record,
        object: &str,
        target: &str,
    ) -> Result<Vec<u8>, VmError> {
        self.ensure_runtime(engine, dir)?;
        let result = (|| {
            self.runtimes
                .get_mut(&dir.path)
                .unwrap()
                .begin_scheduled_request(target)
                .map_err(VmError)?;
            self.render_record(dir, record, object)
        })();
        // Drop the job's URI, User and locals on both success and failure.
        // Queue objects and their state remain in the same catalog runtime.
        if object == "~mp_at" {
            if let Err(VmError(message)) = &result {
                if let Err(error) = self
                    .runtimes
                    .get_mut(&dir.path)
                    .unwrap()
                    .fail_scheduled_job(message)
                {
                    eprintln!("At queue failure status: {error}");
                }
            }
        }
        self.runtimes.get_mut(&dir.path).unwrap().end_request();
        result
    }
    fn ensure_runtime(
        &mut self,
        engine: std::sync::Arc<crate::db::Db4Engine>,
        dir: &crate::db::VirtualDir,
    ) -> Result<(), VmError> {
        if let Some(error) = self.failures.get(&dir.path) {
            return Err(VmError(error.clone()));
        }
        if !self.runtimes.contains_key(&dir.path) {
            if self.runtimes.len() + self.failures.len() >= 32 {
                return Err(VmError("KS catalog context limit (32)".into()));
            }
            if let Some(owner) = &self.owner {
                owner
                    .try_send(dir.clone())
                    .map_err(|_| VmError("Catalog owner unavailable".into()))?;
            }
            let mut runtime =
                match create_runtime_mode(engine.clone(), dir.clone(), self.owner.is_some()) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("KS initialization failed for {}: {}", dir.path, e.0);
                        if !crate::execution::cancelled() {
                            self.failures.insert(dir.path.clone(), e.0.clone());
                        }
                        return Err(e);
                    }
                };
            if self.owner.is_some() {
                runtime.make_http_replica();
            }
            // Failed startup never activates its pending At tasks.
            self.runtimes.insert(dir.path.clone(), runtime);
        }
        if dir.path.trim_end_matches('/') == "/20a" && crate::db::writer::epoch() != 0 {
            distribution::refresh(&engine, dir, self.runtimes.get_mut(&dir.path).unwrap())
                .map_err(VmError)?;
        }
        Ok(())
    }
    fn render_record(
        &mut self,
        dir: &crate::db::VirtualDir,
        record: &Record,
        object: &str,
    ) -> Result<Vec<u8>, VmError> {
        let runtime = self.runtimes.get_mut(&dir.path).unwrap();
        if object == "raspred_settings" && record.source().is_none_or(|s| s.is_empty()) {
            let ast = ks::compiled::parse(
                record
                    .code()
                    .ok_or_else(|| VmError("compiled settings missing".into()))?,
            )
            .map_err(VmError)?;
            let html = runtime
                .render(&ast)
                .map_err(|e| VmError(format!("{object}: {e}")))?;
            let html = crate::classifier_html::adapt(html, object);
            let (bytes, _, bad) = encoding_rs::WINDOWS_1251.encode(&html);
            if bad {
                return Err(VmError(
                    "compiled output cannot be encoded as Windows-1251".into(),
                ));
            }
            return Ok(bytes.into_owned());
        }
        // Work size is determined by the connected court database. Ordinary
        // requests, cascades and reports have no instruction-count quota.
        let source = record
            .source()
            .ok_or_else(|| VmError(format!("{object}: compiled execution required")))?;
        let mut source = decode(source);
        source = ks::source_compat::request_source(object, &source).into_owned();
        if object == "StatOtchet221" {
            // The shipped section-2 query for condition 26 contains one unmatched ')'.
            // Correct the source expression before executing the same report logic.
            source = source.replace(
                "getColPage2(ascVid, '', '\"Отказано б\" '+sBetween +   \n\t\t') AND",
                "getColPage2(ascVid, '', '\"Отказано б\" '+sBetween +   \n\t\t' AND",
            );
        }
        if object == "classifforispr" {
            // The SQL generated by linked search can contain literal line
            // breaks. Its hash key is embedded in a JS string by legacy KS.
            // Base64 is a stable, JS-safe representation of that same key.
            source = regex::Regex::new(r"var sIdRequest\s*=.*?;").unwrap()
                .replace(&source, "var sIdRequest = ToBase64(requesttext ? requesttext : area + attr + nattr + context);")
                .into_owned();
        }
        if dir.mapped_to.eq_ignore_ascii_case("admin.dbs") && object == "bar" {
            source = source.replace("<A HREF=\" %pPath() title\"", "<A HREF=\" %pPath() status\" TARGET=\"target\">СОСТОЯНИЕ</A><BR>\n<A HREF=\" %pPath() title\"");
        }
        // The legacy version is retained for KS feature detection. System information
        // must identify this executable, not the emulated 32-bit API version.
        if dir.mapped_to.eq_ignore_ascii_case("admin.dbs") && object == "title" {
            source = source.replace("Application.Version", "Application.RuntimeVersion");
            source = source
                .replace(
                    "var localHost = new INetAddr();",
                    "var localHost = [Application.RuntimeListenAddress];",
                )
                .replace("IP-адрес:", "Адрес HTTP-сервера:")
                .replace("pPath() cookies", "pPath() status");
        }
        let ast = runtime
            .parse_cached(&source)
            .map_err(|e| VmError(e.to_string()))?;
        let rendered = runtime.render(&ast);
        // Failed reports must not leak their timings into the next request.
        runtime.finish_function_profile();
        let html = rendered.map_err(|e| VmError(format!("{object}: {e}")))?;
        if std::env::var_os("KODEKS_TRACE").is_some_and(|s| s == "1") {
            eprintln!(
                "KS request {object} completed: {} instructions",
                runtime.instruction_count()
            );
        }
        if let Some(bytes) = runtime.take_binary_output() {
            return Ok(bytes);
        }
        let html = crate::classifier_html::adapt(html, object);
        let html = if dir.path.trim_end_matches('/') == "/20a" && object == "adminsettings" {
            crate::server::registration::controls(html).map_err(VmError)?
        } else {
            html
        };
        let (bytes, _, bad) = encoding_rs::WINDOWS_1251.encode(&html);
        if bad {
            return Err(VmError("output cannot be encoded as Windows-1251".into()));
        }
        Ok(bytes.into_owned())
    }
    pub fn due(&mut self) -> Result<Vec<String>, String> {
        let (now, day, minute) = ks::scheduler::clock()?;
        Ok(self
            .runtimes
            .values_mut()
            .flat_map(|r| r.scheduler.due(now, day, minute))
            .collect())
    }
}
