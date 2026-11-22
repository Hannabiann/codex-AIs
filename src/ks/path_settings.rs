use super::*;

impl Runtime<'_> {
    pub(super) fn save_path_settings(
        &mut self,
        receiver: Option<Value>,
        values: &[Value],
        calendar_access: bool,
    ) -> Result<Value, Fault> {
        if !values.is_empty() || !self.document_write_allowed {
            return Err("SaveState requires authenticated settings POST".into());
        }
        let Some(Value::Object(object)) = receiver else {
            return Err("SaveState requires a directory object".into());
        };
        let Some((_, directory, baseline)) = &self.request_baseline else {
            return Err("SaveState requires a request".into());
        };
        if !Rc::ptr_eq(directory, &object) {
            return Err("SaveState receiver is not the request directory".into());
        }
        let registry = self.registry.as_ref().ok_or("registry not configured")?;
        let mut parent = None;
        for &id in &registry.key(registry.root)?.children {
            if registry.query(id, "Path")?.is_some_and(|e| {
                e.value
                    .trim_end_matches('/')
                    .eq_ignore_ascii_case(self.path.trim_end_matches('/'))
            }) {
                if parent.replace(id).is_some() {
                    return Err("ambiguous directory registry key".into());
                }
            }
        }
        let parent = parent.ok_or("directory registry key absent")?;
        let path = std::path::Path::new(self.data_root.as_deref().ok_or("data root missing")?)
            .join("config.db4");
        let object = object.borrow();
        let names = if calendar_access {
            vec!["AccessByUserForMakeVacation"]
        } else {
            crate::vm::distribution::setting_names()
        };
        let current = crate::db::writer::settings::read(&path, parent, &names)?;
        let mut updates = Vec::new();
        for name in names {
            let Some(value) = object.get(name) else {
                continue;
            };
            if !matches!(
                value,
                Value::Null | Value::Number(_) | Value::String(_) | Value::Bool(_)
            ) {
                continue;
            }
            let text = match value {
                Value::Bool(b) => u8::from(*b).to_string(),
                _ => value.text(),
            };
            let previous = current.iter().find(|e| e.name.eq_ignore_ascii_case(name));
            // Native SaveState does not materialize null/zero default values.
            if previous.is_none() && (text.is_empty() || text == "0") {
                continue;
            }
            if let Some(before) = baseline.get(name) {
                if before.text() == value.text() && previous.is_some() {
                    continue;
                }
                if previous.is_some_and(|e| e.value != before.text()) {
                    return Err("Настройки изменились. Откройте форму заново.".into());
                }
            } else if previous.is_some() {
                return Err("Настройки изменились. Откройте форму заново.".into());
            }
            updates.push(crate::db::writer::settings::Update {
                name: name.into(),
                expected: previous.map(|e| e.value.clone()),
                value: if matches!(value, Value::Null) {
                    // A disabled numeric flag is an explicit zero. Existing
                    // catalogs already use both zero and absent defaults;
                    // retaining the record also preserves its text postings.
                    Some(if previous.is_some_and(|e| e.kind == "REG_DWORD") {
                        "0".into()
                    } else {
                        String::new()
                    })
                } else {
                    Some(text)
                },
                kind: previous.map(|e| e.kind.clone()).unwrap_or_else(|| {
                    if matches!(value, Value::String(_)) {
                        "REG_SZ"
                    } else {
                        "REG_DWORD"
                    }
                    .into()
                }),
            });
        }
        crate::db::writer::settings::save(&path, parent, &updates)?;
        Ok(Value::Null)
    }
}
