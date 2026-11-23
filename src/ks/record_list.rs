//! Read-only native List, backed by the same bounded database query path as SQL.
use super::{Fault, Runtime, Value};
use std::{
    cell::{Ref, RefCell, RefMut},
    collections::HashSet,
};

/// Ordered List contents with a lazy membership index. Append must not copy
/// and rehash the entire list for each document in a report loop.
#[derive(Debug)]
pub(super) struct ListIds {
    values: RefCell<Option<Vec<u32>>>,
    members: RefCell<Option<HashSet<u32>>>,
}
impl ListIds {
    pub(super) fn new(values: Option<Vec<u32>>) -> Self {
        Self {
            values: RefCell::new(values),
            members: RefCell::new(None),
        }
    }
    pub(super) fn borrow(&self) -> Ref<'_, Option<Vec<u32>>> {
        self.values.borrow()
    }
    pub(super) fn borrow_mut(&self) -> RefMut<'_, Option<Vec<u32>>> {
        // Replacement, deletion, filtering and sorting all go through here.
        *self.members.borrow_mut() = None;
        self.values.borrow_mut()
    }
    pub(super) fn contains(&self, id: u32) -> bool {
        let values = self.values.borrow();
        let Some(ids) = values.as_ref() else {
            return false;
        };
        if ids.len() < 128 {
            return ids.contains(&id);
        }
        self.members
            .borrow_mut()
            .get_or_insert_with(|| ids.iter().copied().collect())
            .contains(&id)
    }
    pub(super) fn append(&self, incoming: &[u32]) -> Result<(), Fault> {
        let mut values = self.values.borrow_mut();
        let ids = values.get_or_insert_with(Vec::new);
        let mut members = self.members.borrow_mut();
        if members.is_none() && ids.len().saturating_add(incoming.len()) >= 128 {
            *members = Some(ids.iter().copied().collect());
        }
        let mut added = Vec::new();
        for &id in incoming {
            let unique = if let Some(seen) = members.as_mut() {
                seen.insert(id)
            } else {
                !ids.contains(&id) && !added.contains(&id)
            };
            if unique {
                added.push(id)
            }
        }
        if ids.len().saturating_add(added.len()) > crate::ks::sql::MAX_SQL_ROWS {
            // A failed append must not leave phantom members in the index.
            *members = None;
            return Err("List row limit".into());
        }
        ids.extend(added);
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct RecordList {
    pub scope: u32,
    pub area: u16,
    pub order: Option<Vec<u16>>,
    pub conditions: Vec<String>,
    pub query_all: bool,
    pub ids: ListIds,
}
fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}
impl Runtime<'_> {
    pub(super) fn list_len(&self, list: &RecordList) -> Result<usize, Fault> {
        if let Some(ids) = list.ids.borrow().as_ref() {
            return Ok(ids.len());
        }
        Ok(self.list_ids(list)?.len())
    }
    pub(super) fn list_item(&self, list: &RecordList, index: usize) -> Result<Option<u32>, Fault> {
        if let Some(ids) = list.ids.borrow().as_ref() {
            return Ok(ids.get(index).copied());
        }
        Ok(self.list_ids(list)?.get(index).copied())
    }
    pub(super) fn list_area(&self, value: &Value, scope: u32) -> Result<u16, Fault> {
        let area = match value {
            Value::String(name) if !name.is_empty() && name.parse::<f64>().is_err() => {
                let structure = self.member(self.database_for(scope)?, "Struct".into())?;
                let definition = self.member(structure, name.clone())?;
                if matches!(definition, Value::Null) {
                    return Err(format!("List area absent: {name}").into());
                }
                self.member(definition, "Area".into())?.number()?
            }
            _ => value.number()?,
        };
        if !area.is_finite() || !(0.0..=65535.0).contains(&area) || area.fract() != 0.0 {
            return Err("List area invalid".into());
        }
        Ok(area as u16)
    }
    pub(super) fn list_set(
        &self,
        list: &mut RecordList,
        key: &str,
        value: Value,
    ) -> Result<(), Fault> {
        fn field(value: &Value) -> Result<u16, Fault> {
            let n = value.number()?;
            if !n.is_finite() || n < 0.0 || n > u16::MAX as f64 || n.fract() != 0.0 {
                return Err("Invalid List field or area number".into());
            }
            Ok(n as u16)
        }
        match key {
            "Area" => {
                list.area = self.list_area(&value, list.scope)?;
                *list.ids.borrow_mut() = None;
            }
            "Order" => {
                let fields = if let Value::Array(a) = &value {
                    let a = a.borrow();
                    let len = a.get("length").ok_or("Array length missing")?.number()? as usize;
                    (0..len)
                        .map(|i| field(a.get(&i.to_string()).unwrap_or(&Value::Null)))
                        .collect::<Result<Vec<_>, _>>()?
                } else {
                    vec![field(&value)?]
                };
                // Stable sorts from last to first implement native multi-field ordering.
                for &attr in fields.iter().rev() {
                    self.list_sort(list, attr)?;
                }
                list.order = Some(fields);
            }
            _ => return Err("Unsupported List property".into()),
        }
        Ok(())
    }
    pub(super) fn list_sort(&self, list: &mut RecordList, attr: u16) -> Result<(), Fault> {
        // Setting the order on a newly constructed list must not search the
        // entire area. Native 00794120 only invalidates the sort; 00795120
        // materializes it on access. Add switches to explicit contents instead.
        if list.ids.borrow().is_none() && list.conditions.is_empty() {
            return Ok(());
        }
        // A mixed-area native List has no attribute schema: Order retains OID order.
        let ids = if list.area == 0 {
            let mut ids = self.list_ids(list)?;
            ids.sort_unstable();
            ids
        } else {
            self.ordered_ids(self.list_ids(list)?, list.scope, attr)?
        };
        *list.ids.borrow_mut() = Some(ids);
        Ok(())
    }
    fn ordered_ids(&self, ids: Vec<u32>, scope: u32, attr: u16) -> Result<Vec<u32>, Fault> {
        let mut keyed = Vec::new();
        for id in ids {
            let r = self.read_document(id, scope)?;
            let key = match r.fields.iter().find(|f| f.id == attr).map(|f| &f.value) {
                Some(crate::db::fields::Value::String(b)) => crate::db::decode(b).to_lowercase(),
                Some(crate::db::fields::Value::Date(b)) => format!(
                    "{:04}{:02}{:02}",
                    i16::from_le_bytes([b[0], b[1]]),
                    b[2],
                    b[3]
                ),
                Some(crate::db::fields::Value::Timestamp(b)) => format!(
                    "{:04}{:02}{:02}{:02}{:02}{:02}{:02}",
                    u16::from_be_bytes([b[0], b[1]]),
                    b[2],
                    b[3],
                    b[4],
                    b[5],
                    b[6],
                    b[7]
                ),
                None => String::new(),
                _ => return Err("List sort type not implemented".into()),
            };
            keyed.push((key, id));
        }
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(keyed.into_iter().map(|(_, id)| id).collect())
    }
    pub(super) fn list_empty_condition(
        &self,
        list: &mut RecordList,
        values: &[Value],
    ) -> Result<Value, Fault> {
        if values.len() != 1 {
            return Err("List.AddEmptyCondition expects an attribute".into());
        }
        let host = self
            .dbhost
            .as_ref()
            .ok_or("List requires database")?
            .borrow();
        let schema = host.schema(list.scope)?;
        let area = schema.areas.get(&list.area).ok_or("List area absent")?;
        let attr = values[0].number()? as u16;
        let field = area.fields.get(&attr).ok_or("List field absent")?;
        if list.conditions.len() >= 64 {
            return Err("List condition limit".into());
        }
        list.conditions
            .push(format!("{} IS NULL", quoted(&field.name)));
        *list.ids.borrow_mut() = None;
        Ok(Value::Null)
    }
    pub(super) fn list_ids(&self, list: &RecordList) -> Result<Vec<u32>, Fault> {
        if let Some(ids) = list.ids.borrow().as_ref() {
            return Ok(ids.clone());
        }
        // Area zero also contains named service documents. A blank mixed list
        // stays empty; explicit conditions search the service area.
        if list.conditions.is_empty() && (list.area == 0 || !list.query_all) {
            return Ok(Vec::new());
        }
        let host = self
            .dbhost
            .as_ref()
            .ok_or("List requires a database")?
            .borrow();
        let schema = host.schema(list.scope)?;
        let Some(area) = schema.areas.get(&list.area) else {
            return Ok(Vec::new());
        };
        let mut sql = format!("SELECT OID FROM {}", quoted(&area.name));
        if !list.conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&list.conditions.join(" AND "));
        }
        if std::env::var_os("KODEKS_TRACE_SQL").is_some_and(|v| v == "1") {
            eprintln!("KS List query: {sql}");
        }
        let ids = host.query(list.scope, &sql)?;
        drop(host);
        let mut ids = ids;
        if let Some(attrs) = &list.order {
            for &attr in attrs.iter().rev() {
                ids = self.ordered_ids(ids, list.scope, attr)?;
            }
        }
        *list.ids.borrow_mut() = Some(ids.clone());
        Ok(ids)
    }
    pub(super) fn list_condition(
        &self,
        list: &mut RecordList,
        values: &[Value],
    ) -> Result<Value, Fault> {
        if !(2..=3).contains(&values.len()) {
            return Err("List.AddCondition expects field, value[, mode]".into());
        }
        let mode = values.get(2).map(Value::number).transpose()?.unwrap_or(1.0);
        if list.conditions.len() >= 64 {
            return Err("List condition limit".into());
        }
        let host = self
            .dbhost
            .as_ref()
            .ok_or("List requires a database")?
            .borrow();
        let schema = host.schema(list.scope)?;
        let Some(area) = schema.areas.get(&list.area) else {
            *list.ids.borrow_mut() = Some(Vec::new());
            return Ok(Value::Null);
        };
        let field = match &values[0] {
            Value::String(name) => area.fields.values().find(|f| f.name == *name),
            v => area.fields.get(&(v.number()? as u16)),
        }
        .ok_or("List field absent")?;
        let op = match (field.kind, mode as i32) {
            (2, 0) => "LIKE",
            (_, 0) => "=",
            (1, 1) => "<=",
            (1, 2) => ">=",
            (_, 1) => "=",
            (_, 2) => "<>",
            (2, 4) => "LIKE",
            _ => {
                return Err(format!(
                    "List condition mode {mode} for type {} is not implemented",
                    field.kind
                )
                .into())
            }
        };
        if let Value::List(other) = &values[1] {
            if field.kind != 5 {
                return Err("List condition with a List requires a link field".into());
            }
            let ids = self.list_ids(&other.borrow())?;
            list.conditions.push(if ids.is_empty() {
                "OID=0".into()
            } else {
                format!(
                    "{} {}IN ({})",
                    quoted(&field.name),
                    if mode == 2.0 { "NOT " } else { "" },
                    ids.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
                )
            });
            *list.ids.borrow_mut() = None;
            return Ok(Value::Null);
        }
        let literal = match field.kind {
            1 => {
                let Value::Date(d) = &values[1] else {
                    return Err("List date condition requires Date".into());
                };
                let p = d.borrow().parts();
                format!("{{d '{:04}-{:02}-{:02}'}}", p[0], p[1] + 1, p[2])
            }
            2 | 3 => format!(
                "'{}{}'",
                values[1].text().replace('\'', "''"),
                if mode == 4.0 { "*" } else { "" }
            ),
            5 => {
                let n = values[1].number()?;
                if !n.is_finite() || n.fract() != 0.0 || n < 0.0 || n > u32::MAX as f64 {
                    return Err("List link value invalid".into());
                }
                format!("{n}")
            }
            _ => return Err("List equality for this field type is not implemented".into()),
        };
        list.conditions
            .push(format!("{} {op} {literal}", quoted(&field.name)));
        *list.ids.borrow_mut() = None;
        Ok(Value::Null)
    }
}
