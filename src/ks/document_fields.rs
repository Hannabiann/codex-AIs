//! Read-only Document.Attr / Next and SField wrappers.
//! Native dispatch: 0077fced, 0077ff05; wrapper/filter: 0077d220.
use super::{Fault, Runtime, Value};
use crate::db::fields::{Field as DbField, Record, Value as DbValue};
use std::{cell::RefCell, rc::Rc};

#[derive(Debug, Clone)]
pub(super) struct Field {
    pub display: String,
    pub oid: u32,
    pub scope: u32,
    pub record: Rc<Record>,
    pub index: usize,
}

impl Field {
    pub fn text_objects(&self) -> Result<Vec<Value>, Fault> {
        if self.data().wire_type != 4 {
            return Err("text object enumeration requires text field".into());
        }
        let bytes = crate::document::bytes(self.data())?;
        crate::rich_text::items(self.data())?
            .iter()
            .map(|item| super::report_text::item(item, bytes))
            .collect()
    }

    pub fn data(&self) -> &DbField {
        &self.record.fields[self.index]
    }
    pub fn value(&self) -> Result<Value, Fault> {
        // Native SStringField is used for both type 2 and type 3; numbers
        // stored as strings must not be silently converted to floating point.
        match (&self.data().value, self.data().wire_type) {
            (DbValue::String(bytes), 2 | 3) => Ok(Value::String(crate::db::decode(bytes))),
            (DbValue::Integer(oid), 5) => Ok(Value::Number(*oid as i32 as f64)),
            (DbValue::Decimal(bytes),13) => Ok(Value::Decimal(super::decimal::Decimal::from_native(bytes)?)),
            (DbValue::Binary { data, kind, .. }, 8 | 14) if *kind != Some(3) => Ok(super::InputStream::value(data)),
            (DbValue::Date(bytes), 1) => {
                // Native 005e51c0 / 00774d50: i16 year, i8 month, i8 day.
                let date = crate::ks::date::Date::from_parts([
                    i16::from_le_bytes([bytes[0], bytes[1]]) as i64,
                    bytes[2] as i8 as i64 - 1,
                    bytes[3] as i8 as i64,
                    0, 0, 0, 0,
                ])?;
                Ok(Value::Date(Rc::new(RefCell::new(date))))
            }
            // Native 007ed950 / 007eda30: big-endian year, month/day,
            // hours/minutes/seconds and hundredths of a second.
            (DbValue::Timestamp(b),12) => Ok(Value::Date(Rc::new(RefCell::new(
                crate::ks::date::Date::from_parts([u16::from_be_bytes([b[0],b[1]]) as i64,b[2] as i64-1,b[3] as i64,b[4] as i64,b[5] as i64,b[6] as i64,b[7] as i64*10])?
            )))),
            _ => Err(format!(
                "Field.Value conversion for native type {} is not implemented (Document({}), attribute {})",
                self.data().wire_type, self.oid, self.data().id
            ).into()),
        }
    }
}

fn u16_arg(v: &Value, what: &str) -> Result<u16, Fault> {
    let n = v.number()?;
    if !n.is_finite() || n.fract() != 0.0 || !(0.0..=65535.0).contains(&n) {
        return Err(Fault::Script(Value::String(format!("invalid {what}"))));
    }
    Ok(n as u16)
}

impl Runtime<'_> {
    pub(super) fn field_definition(
        &self,
        scope: u32,
        area: u16,
        key: &Value,
    ) -> Result<Option<(u16, u16)>, Fault> {
        // The Word attributes dialog posts the selected case under this
        // classifier alias. Some court schemas declare only the case link.
        if self.document_write_allowed
            && self.path.trim_end_matches('/') == "/99"
            && area == 7341
            && matches!(key, Value::String(s) if s=="Наим.классиф.")
        {
            if let Some(host) = &self.dbhost {
                let schema = host.borrow().shared_schema(scope)?;
                if let Some(fields) = schema.areas.get(&area).map(|a| &a.fields) {
                    if !fields.values().any(|f| f.name == "Наим.классиф.") {
                        if let Some(f) = fields.values().find(|f| {
                            f.kind == 5 && f.reference == 7340 && f.name == "Дело (1; Ndela)"
                        }) {
                            return Ok(Some((f.id, f.kind)));
                        }
                    }
                }
            }
        }
        let structure = self.member(self.database_for(scope)?, "Struct".into())?;
        let area = self.member(structure, area.to_string())?;
        if matches!(area, Value::Null) {
            return Ok(None);
        }
        let name = match key {
            Value::String(name) => name.clone(),
            value => u16_arg(value, "attribute number")?.to_string(),
        };
        let field = self.member(area, name)?;
        if matches!(field, Value::Null) {
            return Ok(None);
        }
        if let Value::String(name) = key {
            // String arguments are names, including digit-only strings.
            if self.member(field.clone(), "Name".into())?.text() != *name {
                return Ok(None);
            }
        }
        Ok(Some((
            u16_arg(
                &self.member(field.clone(), "Attr".into())?,
                "attribute number",
            )?,
            u16_arg(&self.member(field, "Type".into())?, "field type")?,
        )))
    }
    pub(super) fn document_attr(
        &mut self,
        oid: u32,
        scope: u32,
        args: &[Value],
    ) -> Result<Value, Fault> {
        let draft = self.document_edits.borrow().get(&(scope, oid)).cloned();
        if let Some(draft) = draft {
            return self.edit_attr(draft, "Attr", args);
        }
        if args.len() > 3 {
            return Err("Document.Attr expects [attribute[, checkLinks[, nativeType]]]".into());
        }
        let record = Rc::new(self.read_document(oid, scope)?);
        let key = args.first().cloned().unwrap_or(Value::Number(65535.0));
        if let Value::Number(n) = key {
            let tab = self
                .form_tabs
                .borrow()
                .get(&(scope, oid, n as u16))
                .cloned();
            if let Some(tab) = tab.filter(|t| t.kind != "text") {
                return self.tab_field(&tab);
            }
        }
        let definition = self.field_definition(scope, record.area, &key)?;
        let attr = match (&key, definition) {
            (Value::String(_), Some((id, _))) => id,
            (Value::String(_), None) => return Ok(Value::Null),
            (number, _) => u16_arg(number, "attribute number")?,
        };
        let kind = if let Some(explicit) = args.get(2) {
            u16_arg(explicit, "field type")?
        } else if let Some((_, kind)) = definition {
            kind
        } else if attr == 1100 {
            // Native special case 0077fe78: parent link can lack a definition.
            5
        } else {
            return Ok(Value::Null);
        };
        let check_links = args.get(1).map_or(true, Value::truth);
        let Some(index) = record
            .fields
            .iter()
            .position(|f| f.id == attr && f.wire_type == kind)
        else {
            return Ok(Value::Null);
        };
        self.wrap_field(
            Field {
                display: String::new(),
                oid,
                scope,
                record,
                index,
            },
            check_links,
        )
    }
    fn wrap_field(&self, mut field: Field, check_links: bool) -> Result<Value, Fault> {
        let attr = field.data().id;
        let kind = field.data().wire_type;
        while let Some(candidate) = field.record.fields.get(field.index) {
            if candidate.id != attr || candidate.wire_type != kind {
                break;
            }
            if check_links {
                if let DbValue::Integer(target) = &candidate.value {
                    match self.read_document(*target, field.scope) {
                        Ok(_) => {}
                        Err(Fault::Script(_)) => {
                            field.index += 1;
                            continue;
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
            field.display = match &candidate.value {
                DbValue::String(s) => crate::db::decode(s),
                DbValue::Decimal(d) => super::decimal::Decimal::from_native(d)?.text(),
                DbValue::Date(d) => format!(
                    "{:02}.{:02}.{:04}",
                    d[3],
                    d[2],
                    i16::from_le_bytes([d[0], d[1]])
                ),
                DbValue::Timestamp(b) => format!(
                    "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:02}",
                    u16::from_be_bytes([b[0], b[1]]),
                    b[2],
                    b[3],
                    b[4],
                    b[5],
                    b[6],
                    b[7]
                ),
                DbValue::Integer(target) => match self.read_document(*target, field.scope) {
                    Ok(r) => r
                        .string(1)
                        .map(crate::db::decode)
                        .unwrap_or_else(|| target.to_string()),
                    Err(Fault::Script(_)) => target.to_string(),
                    Err(e) => return Err(e),
                },
                _ => String::new(),
            };
            return Ok(Value::StoredField(field));
        }
        Ok(Value::Null)
    }
    pub(super) fn document_next(
        &self,
        oid: u32,
        scope: u32,
        args: &[Value],
    ) -> Result<Value, Fault> {
        let draft = self.document_edits.borrow().get(&(scope, oid)).cloned();
        if let Some(draft) = draft {
            return self.edit_attr(draft, "Next", args);
        }
        if args.is_empty() {
            return Ok(Value::Null);
        }
        if args.len() > 2 {
            return Err("Document.Next expects field[, checkLinks]".into());
        }
        let Value::StoredField(previous) = &args[0] else {
            return Err(Fault::Script(Value::String(
                "Document.Next requires a Field".into(),
            )));
        };
        if previous.oid != oid || previous.scope != scope {
            return Err(Fault::Script(Value::String(
                "Field belongs to another Document/DataBase".into(),
            )));
        }
        self.read_document(oid, scope)?;
        let mut next = previous.clone();
        next.index += 1;
        let Some(candidate) = next.record.fields.get(next.index) else {
            return Ok(Value::Null);
        };
        if candidate.id != previous.data().id || candidate.wire_type != previous.data().wire_type {
            return Ok(Value::Null);
        }
        // Native Next only overrides the default for a boolean second argument.
        self.wrap_field(next, !matches!(args.get(1), Some(Value::Bool(false))))
    }
}
