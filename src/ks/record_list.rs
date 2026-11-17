//! Read-only native List, backed by the same bounded database query path as SQL.
use super::{Fault, Runtime, Value};
use std::cell::RefCell;

#[derive(Debug)]
pub(super) struct RecordList {
    pub scope: u32,
    pub area: u16,
    pub order: Option<u16>,
    pub conditions: Vec<String>,
    pub query_all: bool,
    pub ids: RefCell<Option<Vec<u32>>>,
}
fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}
impl Runtime<'_> {
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
        // Area zero is an initially empty mixed list, not the system-script area.
        if list.area == 0 || (list.conditions.is_empty() && !list.query_all) {
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
        let ids = if let Some(attr) = list.order {
            self.ordered_ids(ids, list.scope, attr)?
        } else {
            ids
        };
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
