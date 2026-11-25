use super::*;
use crate::db::writer::registry::Transaction;

pub(super) struct Settings {
    pub transaction: Transaction,
    pub objects: Vec<(Object, u32, HashMap<String, Value>)>,
    pub saved: bool,
    pub scheduler: super::super::scheduler::Scheduler,
}
fn scalar(value: &Value) -> Result<Option<(String, String)>, String> {
    Ok(match value {
        // Native Config/SaveState serializes null as an empty REG_SZ value.
        // DeleteValue is a distinct registry operation.
        Value::Null => Some(("REG_SZ".into(), String::new())),
        Value::String(s) => Some(("REG_SZ".into(), s.clone())),
        Value::Number(n)
            if n.is_finite()
                && *n >= i32::MIN as f64
                && *n <= i32::MAX as f64
                && n.fract() == 0.0 =>
        {
            Some(("REG_DWORD".into(), value.text()))
        }
        Value::Bool(b) => Some(("REG_DWORD".into(), u8::from(*b).to_string())),
        _ => return Err("Настройка должна быть строкой или целым числом".into()),
    })
}
impl Runtime<'_> {
    pub fn load_startup(&mut self, source: &str) -> Result<(), String> {
        self.startup_registry_defaults = true;
        let result = self.load(source);
        self.startup_registry_defaults = false;
        result
    }
    pub(super) fn initialize_startup_export_date(
        &mut self,
        parent: u32,
        values: &[Value],
    ) -> Result<bool, Fault> {
        if !self.startup_registry_defaults
            || self.request_baseline.is_some()
            || self.path.trim_end_matches('/') != "/20a"
        {
            return Ok(false);
        }
        let [Value::String(name), Value::Number(1.0), Value::String(value)] = values else {
            return Ok(false);
        };
        const NAME: &str = "Дата последнего успешного экспорта";
        const KEY: &str = "SOFTWARE\\Kodeks\\Web\\Настройки\\Обмен с БРАС";
        if name != NAME {
            return Ok(false);
        }
        // Native startup initializes this empty date even when KAD exchange is
        // disabled. It is a scheduling default, not a completed export.
        if !value
            .strip_prefix("\"\\/Date(")
            .and_then(|s| s.strip_suffix(")\\/\""))
            .and_then(|s| s.parse::<f64>().ok())
            .is_some_and(f64::is_finite)
        {
            return Err("Invalid startup export date".into());
        }
        let reg = self.registry.as_ref().ok_or("Registry unavailable")?;
        if reg.open(KEY)? != Some(parent) {
            return Ok(false);
        }
        if !reg
            .query(parent, NAME)?
            .is_some_and(|e| e.kind == "REG_SZ" && e.value.is_empty())
        {
            return Ok(false);
        }
        let root = reg.root;
        let path = std::path::Path::new(self.data_root.as_deref().ok_or("Data root unavailable")?)
            .join("config.db4");
        // All startup replicas can reach this point. Compare the complete image
        // at publication, then adopt the first initialized value on a retry.
        loop {
            if crate::execution::cancelled() {
                return Err("Startup cancelled".into());
            }
            let mut tx = Transaction::open(&path, root)?;
            let Some(old) = tx.registry.query(parent, NAME)? else {
                return Ok(false);
            };
            if old.kind != "REG_SZ" {
                return Ok(false);
            }
            if !old.value.is_empty() {
                self.registry = Some(tx.registry.clone());
                self.settings_generation = u64::MAX;
                return Ok(true);
            }
            tx.set(parent, NAME, "REG_SZ", Some(value))?;
            let next = tx.registry.clone();
            match tx.commit() {
                Ok(_) => {
                    self.registry = Some(next);
                    self.settings_generation = u64::MAX;
                    return Ok(true);
                }
                Err(e) if e == "Настройки изменились. Откройте форму заново." => {
                    std::thread::yield_now()
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
    pub fn load_system_settings_view(&mut self) -> Result<(), String> {
        self.settings_generation = u64::MAX;
        self.refresh_system_settings()
    }
    pub fn refresh_system_settings(&mut self) -> Result<(), String> {
        if self.settings_edit.is_some() {
            return Ok(());
        }
        // A standalone KS runtime may have an in-memory registry for file
        // access without an Application.Config or a mounted config.db4.
        if self.config.is_none() {
            return Ok(());
        }
        let Some(reg) = self.registry.as_ref() else {
            return Ok(());
        };
        let Some(root) = &self.data_root else {
            return Ok(());
        };
        let path = std::path::Path::new(root)
            .join("config.db4")
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let generation = crate::db::writer::generation(&path);
        if generation == self.settings_generation {
            return Ok(());
        }
        let transaction = Transaction::open(&path, reg.root)?;
        self.settings_revision = transaction.revision();
        self.apply_registry(&transaction.registry)?;
        self.settings_generation = generation;
        Ok(())
    }
    fn apply_registry(&mut self, reg: &crate::db::registry::Registry) -> Result<(), String> {
        let old = self.registry.as_ref().ok_or("Registry unavailable")?;
        let apply = |object: &Object, id: u32| -> Result<(), String> {
            let mut o = object.borrow_mut();
            for e in &old.key(id)?.values {
                o.remove(&e.name);
            }
            let mut names = std::collections::HashSet::new();
            for e in &reg.key(id)?.values {
                // QueryValue selects the first OID when native catalogs retain
                // duplicate names. Refresh must use the same precedence.
                if !names.insert(e.name.to_lowercase()) {
                    continue;
                }
                let v = if e.kind == "REG_DWORD" {
                    Value::Number(e.value.parse::<i32>().map_err(|_| "invalid REG_DWORD")? as f64)
                } else {
                    Value::String(e.value.clone())
                };
                o.insert(e.name.clone(), v);
            }
            Ok(())
        };
        if let Some(o) = &self.config {
            apply(o, reg.root)?;
        }
        let current = self.get("this").map_err(|e| e.to_string())?;
        let paths = self.settings_paths();
        for &id in &reg.key(reg.root)?.children {
            let Some(e) = reg.query(id, "Path")? else {
                continue;
            };
            if e.value.trim_end_matches('/') == self.path.trim_end_matches('/') {
                if let Value::Object(o) = &current {
                    apply(o, id)?;
                }
            }
            for o in &paths {
                if o.borrow().get("Path").is_some_and(|v| v.text() == e.value) {
                    apply(o, id)?;
                }
            }
        }
        self.registry = Some(reg.clone());
        Ok(())
    }
    pub(super) fn settings_paths(&self) -> Vec<Object> {
        let Ok(Value::Application(app)) = self.get("Application") else {
            return vec![];
        };
        let Some(Value::Array(paths)) = app.borrow().get("Paths").cloned() else {
            return vec![];
        };
        let result = paths
            .borrow()
            .values()
            .filter_map(|v| match v {
                Value::ReadOnlyObject(o) | Value::Object(o) => Some(o.clone()),
                _ => None,
            })
            .collect();
        result
    }
    pub fn prepare_system_settings(&mut self, revision: &str) -> Result<(), String> {
        let root = self.registry.as_ref().ok_or("Registry unavailable")?.root;
        let path = std::path::Path::new(self.data_root.as_deref().ok_or("Data root unavailable")?)
            .join("config.db4");
        let transaction = Transaction::open(&path, root)?;
        if transaction.revision() != revision {
            return Err("Настройки изменились. Откройте форму заново.".into());
        }
        let mut objects = Vec::new();
        // Only genuine host configuration objects may write; request fields
        // cannot supply a registry parent or impersonate SaveState's receiver.
        if let Some(config) = &self.config {
            objects.push((config.clone(), root, config.borrow().clone()));
        }
        let directory = self
            .request_baseline
            .as_ref()
            .ok_or("Settings require HTTP request")?
            .1
            .clone();
        for &id in &transaction.registry.key(root)?.children {
            let Some(e) = transaction.registry.query(id, "Path")? else {
                continue;
            };
            if e.value.trim_end_matches('/') == self.path.trim_end_matches('/') {
                objects.push((directory.clone(), id, directory.borrow().clone()));
                directory.borrow_mut().insert(
                    "SaveState".into(),
                    Value::Native("settings.SaveState".into()),
                );
            }
            if e.value == "/99" {
                for o in self.settings_paths() {
                    if o.borrow().get("Path").is_some_and(|v| v.text() == "/99") {
                        objects.push((o.clone(), id, o.borrow().clone()));
                        o.borrow_mut().insert(
                            "SaveState".into(),
                            Value::Native("settings.SaveState".into()),
                        );
                    }
                }
            }
        }
        self.settings_edit = Some(Settings {
            transaction,
            objects,
            saved: false,
            scheduler: self.scheduler.clone(),
        });
        Ok(())
    }
    pub(super) fn assign_config_setting(
        &mut self,
        o: &Object,
        key: &str,
        v: Value,
    ) -> Result<(), Fault> {
        let edit = self.settings_edit.as_mut().ok_or(
            "Application.Config writes require an authenticated system-settings submission",
        )?;
        let parent = edit
            .objects
            .iter()
            .find(|(obj, _, _)| Rc::ptr_eq(obj, o))
            .map(|x| x.1)
            .ok_or("Read-only host object")?;
        // PathPool properties are flushed by SaveState; Config writes are staged immediately.
        if parent == edit.transaction.registry.root {
            let next = scalar(&v)?;
            edit.transaction.set_host_property(
                parent,
                key,
                next.as_ref().map_or("REG_SZ", |p| p.0.as_str()),
                next.as_ref().map(|p| p.1.as_str()),
            )?;
            self.registry = Some(edit.transaction.registry.clone());
        }
        o.borrow_mut().insert(key.into(), v);
        Ok(())
    }
    pub(super) fn save_system_path(
        &mut self,
        receiver: Option<Value>,
        args: &[Value],
    ) -> Result<Value, Fault> {
        let Some(Value::Object(o) | Value::ReadOnlyObject(o)) = receiver else {
            return Err("SaveState requires a host directory".into());
        };
        let edit = self
            .settings_edit
            .as_mut()
            .ok_or("Settings POST required")?;
        if !args.is_empty() {
            return Err("SaveState expects no arguments".into());
        }
        let (_, parent, before) = edit
            .objects
            .iter()
            .find(|(obj, id, _)| Rc::ptr_eq(obj, &o) && *id != edit.transaction.registry.root)
            .ok_or("Invalid settings directory")?;
        let parent = *parent;
        for (name, value) in o.borrow().iter() {
            if !matches!(
                value,
                Value::Null | Value::Number(_) | Value::String(_) | Value::Bool(_)
            ) || name.starts_with('\0')
            {
                continue;
            }
            if before.get(name).is_some_and(|v| equal(v, value))
                && edit.transaction.registry.query(parent, name)?.is_none()
            {
                continue;
            }
            if matches!(value, Value::Null)
                && edit.transaction.registry.query(parent, name)?.is_none()
            {
                continue;
            }
            let next = scalar(value)?;
            edit.transaction.set_host_property(
                parent,
                name,
                next.as_ref().map_or("REG_SZ", |p| p.0.as_str()),
                next.as_ref().map(|p| p.1.as_str()),
            )?;
        }
        edit.saved = true;
        self.registry = Some(edit.transaction.registry.clone());
        Ok(Value::Null)
    }
    pub fn commit_system_settings(&mut self) -> Result<(), String> {
        let Some(edit) = self.settings_edit.take() else {
            return Ok(());
        };
        // Restore host views even on a failed or cancelled publication.
        for (o, _, before) in &edit.objects {
            *o.borrow_mut() = before.clone();
        }
        self.settings_generation = u64::MAX;
        if !edit.saved {
            return Err("Настройки не сохранены: штатный обработчик не завершил SaveState".into());
        }
        self.settings_revision = edit.transaction.commit()?;
        self.scheduler = edit.scheduler;
        Ok(())
    }
    pub(super) fn settings_at(
        &mut self,
        receiver: Option<Value>,
        args: &[Value],
    ) -> Result<Value, Fault> {
        let Some(Value::ReadOnlyObject(o)) = receiver else {
            return Err("PathPool At receiver required".into());
        };
        if self.http_replica {
            return Err("This KS action requires the catalog owner executor".into());
        }
        if !self.settings_paths().iter().any(|p| Rc::ptr_eq(p, &o)) {
            return Err("Invalid At directory".into());
        }
        let [Value::String(script), Value::String(time)] = args else {
            return Err("Directory At expects script and time".into());
        };
        let Some(when) = super::super::scheduler::parse_time(time) else {
            return Ok(Value::Bool(false));
        };
        let path = o
            .borrow()
            .get("Path")
            .ok_or("PathPool directory absent")?
            .text();
        let (now, day, minute) = super::super::scheduler::clock()?;
        self.settings_edit
            .as_mut()
            .ok_or("Settings POST required")?
            .scheduler
            .schedule(
                format!("{}/{script}", path.trim_end_matches('/')),
                when,
                now,
                day,
                minute,
            )?;
        Ok(Value::Bool(true))
    }
    pub(super) fn abort_system_settings(&mut self) {
        if let Some(edit) = self.settings_edit.take() {
            for (o, _, before) in edit.objects {
                *o.borrow_mut() = before;
            }
            self.settings_generation = u64::MAX;
        }
    }
}
