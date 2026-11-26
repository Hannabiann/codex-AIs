//! Native home-page counters are a persistent cache, not an admin-settings form.
use super::*;
use crate::db::{
    registry::{Entry, Registry},
    writer::registry::Transaction,
};
use std::collections::BTreeMap;

pub(super) struct Cache {
    transaction: Transaction,
    before: Registry,
    items: BTreeMap<String, Item>,
    stale: bool,
}
struct Item {
    expected: Option<Vec<Entry>>,
    values: BTreeMap<String, (String, String)>,
}

pub(super) fn request_allowed(
    path: &str,
    command: &str,
    method: &str,
    authenticated: bool,
    scheduled: bool,
) -> bool {
    !scheduled
        && authenticated
        && matches!(method, "GET" | "POST")
        && matches!(path, "/20a" | "/20a/")
        && matches!(command, "" | "default")
}

fn values(reg: &Registry, path: &str) -> Result<Option<Vec<Entry>>, String> {
    reg.open(path)?
        .map(|id| Ok(reg.key(id)?.values.clone()))
        .transpose()
}

fn key_path(reg: &Registry, id: u32) -> Result<String, String> {
    let mut pending = vec![(reg.root, "SOFTWARE\\Kodeks\\Web".to_owned())];
    while let Some((current, path)) = pending.pop() {
        if current == id {
            return Ok(path);
        }
        for &child in &reg.key(current)?.children {
            pending.push((child, format!("{path}\\{}", reg.key(child)?.name)));
        }
    }
    Err("Registry cache parent absent".into())
}

// Restrict automatic writes to a counter leaf of the current catalog. A
// request cannot turn this capability into writes to registration/settings.
fn cache_path(reg: &Registry, directory: &str, path: &str) -> Result<bool, String> {
    let p: Vec<_> = path.split('\\').filter(|s| !s.is_empty()).collect();
    if p.iter().any(|s| matches!(*s, "." | "..")) {
        return Err("invalid registry path component".into());
    }
    reg.open(path)?;
    if p.len() < 8
        || p[4] != "Настройки"
        || p[5] != "Статистики сервера"
        || !((p[6] == "Общие" && p.len() == 8) || (p[6] == "Персонализированные" && p.len() == 9))
    {
        return Ok(false);
    }
    let catalog = format!("SOFTWARE\\Kodeks\\Web\\{}", p[3]);
    let Some(id) = reg.open(&catalog)? else {
        return Ok(false);
    };
    Ok(reg
        .query(id, "Path")?
        .is_some_and(|e| e.value.trim_end_matches('/') == directory.trim_end_matches('/')))
}

impl Runtime<'_> {
    pub(super) fn statistics_cache_call(
        &mut self,
        id: u32,
        method: &str,
        args: &[Value],
    ) -> Result<bool, Fault> {
        if !self.statistics_cache_allowed
            || self.settings_edit.is_some()
            || !matches!(method, "CreateKey" | "SetValue")
        {
            return Ok(false);
        }
        let reg = self.registry.as_ref().ok_or("Registry unavailable")?;
        let path = if method == "CreateKey" {
            let [Value::String(path)] = args else {
                return Ok(false);
            };
            if reg.open(path)?.is_some() {
                return Ok(false);
            }
            path.clone()
        } else {
            key_path(reg, id)?
        };
        if !cache_path(reg, &self.path, &path)? {
            return Ok(false);
        }
        let update = if method == "SetValue" {
            let [Value::String(name), Value::Number(kind), value] = args else {
                return Err("SetValue expects name, type, value".into());
            };
            if !matches!(
                name.as_str(),
                "Значение" | "Дата последнего обновления" | "С" | "По"
            ) {
                return Ok(false);
            }
            let kind = match *kind {
                1.0 => "REG_SZ",
                4.0 => "REG_DWORD",
                _ => return Err("Unsupported registry cache value type".into()),
            };
            Some((name.clone(), kind.to_owned(), value.text()))
        } else {
            None
        };
        if self.statistics_cache.is_none() {
            let file =
                std::path::Path::new(self.data_root.as_deref().ok_or("Data root unavailable")?)
                    .join("config.db4");
            let transaction = Transaction::open(&file, reg.root)?;
            self.statistics_cache = Some(Cache {
                before: reg.clone(),
                transaction,
                items: BTreeMap::new(),
                stale: false,
            });
        }
        let cache = self.statistics_cache.as_mut().unwrap();
        let expected = values(&cache.before, &path)?;
        if !cache.items.contains_key(&path)
            && values(&cache.transaction.registry, &path)? != expected
        {
            cache.stale = true;
        }
        cache.transaction.create_key(&path)?;
        let parent = cache
            .transaction
            .registry
            .open(&path)?
            .ok_or("Cache key absent")?;
        if let Some((name, kind, value)) = &update {
            cache.transaction.set(parent, name, kind, Some(value))?;
        }
        let item = cache.items.entry(path).or_insert_with(|| Item {
            expected,
            values: BTreeMap::new(),
        });
        if let Some((name, kind, value)) = update {
            item.values.insert(name, (kind, value));
        }
        self.registry = Some(cache.transaction.registry.clone());
        Ok(true)
    }

    pub fn commit_statistics_cache(&mut self) -> Result<(), String> {
        let Some(cache) = self.statistics_cache.take() else {
            return Ok(());
        };
        self.registry = Some(cache.before.clone());
        self.settings_generation = u64::MAX;
        let Cache {
            transaction,
            before,
            items,
            stale,
        } = cache;
        if !stale {
            match transaction.commit() {
                Ok(_) => return Ok(()),
                Err(e) if e == "Настройки изменились. Откройте форму заново." =>
                    {}
                Err(e) => return Err(e),
            }
        }
        let file = std::path::Path::new(self.data_root.as_deref().ok_or("Data root unavailable")?)
            .join("config.db4");
        loop {
            if crate::execution::cancelled() {
                return Err("Statistics cache cancelled".into());
            }
            let mut tx = Transaction::open(&file, before.root)?;
            for (path, item) in &items {
                // Another worker refreshed this counter while it was being
                // calculated. Keep its entire value/date pair. Unrelated
                // configuration changes can be merged without replaying KS.
                if values(&tx.registry, path)? != item.expected {
                    continue;
                }
                tx.create_key(path)?;
                let id = tx.registry.open(path)?.ok_or("Cache key absent")?;
                for (name, (kind, value)) in &item.values {
                    tx.set(id, name, kind, Some(value))?;
                }
            }
            match tx.commit() {
                Ok(_) => return Ok(()),
                Err(e) if e == "Настройки изменились. Откройте форму заново." => {
                    std::thread::yield_now()
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub(super) fn abort_statistics_cache(&mut self) {
        self.statistics_cache_allowed = false;
        if let Some(cache) = self.statistics_cache.take() {
            self.registry = Some(cache.before);
            self.settings_generation = u64::MAX;
        }
    }
}

#[cfg(test)]
#[path = "statistics_cache_tests.rs"]
mod tests;
