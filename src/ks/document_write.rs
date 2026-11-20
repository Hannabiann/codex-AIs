//! Request-scoped native document edits. Only Write publishes a DB4 transaction.
use super::{document_fields, Fault, InputStream, Runtime, Value};
use crate::db::fields::{Field, FileMetadata, Record, Value as Data};
use std::{cell::RefCell, collections::HashMap, rc::Rc};

pub(super) type Edit = Rc<RefCell<Draft>>;
#[derive(Debug)]
pub(super) struct Draft {
    pub scope: u32,
    pub oid: u32,
    pub area: u16,
    pub locked: bool,
    expected: Option<Record>,
    // Slots never move: a deleted field handle cannot start editing another field.
    fields: Vec<Option<Field>>,
    deleted_fields: HashMap<usize, (u16, u16)>,
    streams: HashMap<usize, Rc<RefCell<InputStream>>>,
}
impl Draft {
    fn new(scope: u32, record: Option<Record>) -> Self {
        Self {
            scope,
            oid: record.as_ref().and_then(|r| r.integer(1024)).unwrap_or(0),
            area: record.as_ref().map_or(0, |r| r.area),
            locked: false,
            fields: record
                .as_ref()
                .map(|r| r.fields.iter().cloned().map(Some).collect())
                .unwrap_or_default(),
            expected: record,
            streams: HashMap::new(),
            deleted_fields: HashMap::new(),
        }
    }
    pub fn record(&self) -> Record {
        let mut record = Record {
            area: self.area,
            flags: 0,
            fields: self
                .fields
                .iter()
                .enumerate()
                .filter_map(|(i, f)| {
                    let mut field = f.clone()?;
                    if let Some(stream) = self.streams.get(&i) {
                        if let Data::Binary { data, .. } = &mut field.value {
                            *data = stream.borrow().data.clone();
                        }
                    }
                    Some(field)
                })
                .collect::<Vec<_>>()
                .into(),
        };
        crate::db::writer::canonicalize(&mut record);
        record
    }
    fn field(&self, index: usize) -> Result<&Field, Fault> {
        self.fields
            .get(index)
            .and_then(Option::as_ref)
            .ok_or_else(|| "Document field was deleted".into())
    }
    fn check_lock(&self) -> Result<(), Fault> {
        if !self.locked {
            return Err("Document must be locked before editing".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub(super) struct WritableField {
    pub doc: Edit,
    pub index: usize,
}
impl WritableField {
    pub fn snapshot(&self) -> Result<document_fields::Field, Fault> {
        let d = self.doc.borrow();
        let mut field = d.field(self.index)?.clone();
        if let (Some(stream), Data::Binary { data, .. }) =
            (d.streams.get(&self.index), &mut field.value)
        {
            *data = stream.borrow().data.clone();
        }
        let display = match &field.value {
            Data::String(b) => crate::db::decode(b),
            Data::Integer(n) => n.to_string(),
            Data::Date(d) => format!(
                "{:02}.{:02}.{}",
                d[3],
                d[2],
                i16::from_le_bytes([d[0], d[1]])
            ),
            _ => String::new(),
        };
        Ok(document_fields::Field {
            display,
            oid: d.oid,
            scope: d.scope,
            record: Rc::new(Record {
                area: d.area,
                flags: 0,
                fields: vec![field].into(),
            }),
            index: 0,
        })
    }
    pub fn value(&self) -> Result<Value, Fault> {
        self.snapshot()?.value()
    }
}
fn integer(v: &Value, max: u32) -> Result<u32, Fault> {
    let n = v.number()?;
    if !n.is_finite() || n.fract() != 0.0 || n < 0.0 || n > max as f64 {
        return Err("invalid document integer".into());
    }
    Ok(n as u32)
}
fn scalar(kind: u16, value: &Value) -> Result<Data, Fault> {
    Ok(match kind {
        2 | 3 => Data::String(
            encoding_rs::WINDOWS_1251
                .encode(&value.text())
                .0
                .into_owned(),
        ),
        5 => Data::Integer(integer(value, u32::MAX)?),
        13 => Data::Decimal(super::decimal::Decimal::parse(&value.text())?.to_native()),
        1 | 12 => {
            let date = match value {
                Value::Date(d) => d.borrow().clone(),
                v => crate::ks::date::Date::from_text(&v.text())?,
            };
            let p = date.parts();
            if !(100..=9999).contains(&p[0]) {
                return Err("invalid stored calendar date".into());
            }
            if kind == 1 {
                let y = (p[0] as i16).to_le_bytes();
                Data::Date([y[0], y[1], (p[1] + 1) as u8, p[2] as u8])
            } else {
                let y = (p[0] as u16).to_be_bytes();
                Data::Timestamp([
                    y[0],
                    y[1],
                    (p[1] + 1) as u8,
                    p[2] as u8,
                    p[3] as u8,
                    p[4] as u8,
                    p[5] as u8,
                    (p[6] / 10) as u8,
                ])
            }
        }
        _ => {
            return Err(
                format!("Persistent field assignment type {kind} is not implemented").into(),
            )
        }
    })
}
impl Runtime<'_> {
    fn allow_document_write(&self) -> Result<(), Fault> {
        if !self.document_write_allowed {
            return Err(
                "Persistent writes require an authenticated document editing request".into(),
            );
        }
        Ok(())
    }
    pub(super) fn new_edit_document(&self) -> Result<Value, Fault> {
        self.new_edit_document_in(0)
    }
    pub(super) fn new_edit_document_in(&self, scope: u32) -> Result<Value, Fault> {
        self.allow_document_write()?;
        Ok(Value::EditDocument(Rc::new(RefCell::new(Draft::new(
            scope, None,
        )))))
    }
    pub(super) fn assign_stored_area(
        &self,
        oid: u32,
        scope: u32,
        value: &Value,
    ) -> Result<(), Fault> {
        self.allow_document_write()?;
        // The native DeleteObject form stages an area-0 overlay before placing
        // a tombstone over a record from a read-only historical volume.
        if integer(value, u16::MAX as u32)? != 0 {
            return Err(
                "Changing a stored document area is only supported for deletion overlays".into(),
            );
        }
        if self.document_edits.borrow().contains_key(&(scope, oid)) {
            return Err("Document already has an active edit handle".into());
        }
        let mut draft = Draft::new(scope, Some(self.read_document(oid, scope)?));
        draft.area = 0;
        self.document_edits
            .borrow_mut()
            .insert((scope, oid), Rc::new(RefCell::new(draft)));
        Ok(())
    }
    pub(super) fn edit_property(&self, doc: Edit, key: String) -> Result<Value, Fault> {
        let d = doc.borrow();
        Ok(match key.as_str() {
            "NumDoc" => Value::Number(d.oid as f64),
            "Area" => Value::Number(d.area as f64),
            "Locked" => Value::Bool(d.locked),
            "Name" => d
                .record()
                .string(1)
                .map(|b| Value::String(crate::db::decode(b)))
                .unwrap_or(Value::Null),
            "Base" => return self.database_for(d.scope),
            "AreaName" => {
                let s = self.member(self.database_for(d.scope)?, "Struct".into())?;
                let a = self.member(s, d.area.to_string())?;
                return self.member(a, "Name".into());
            }
            "Lock" | "Unlock" | "Write" | "NewField" | "DeleteField" | "Attr" | "Next" => {
                Value::EditMethod(doc.clone(), key)
            }
            _ if d.oid != 0 => return self.member(Value::Document(d.oid, d.scope), key),
            _ => Value::Null,
        })
    }
    pub(super) fn edit_set(&self, doc: Edit, key: &str, value: Value) -> Result<(), Fault> {
        self.allow_document_write()?;
        let mut d = doc.borrow_mut();
        if d.oid != 0 || d.locked {
            return Err("A stored document's area cannot be changed".into());
        }
        let area = match key {
            "Area" => integer(&value, u16::MAX as u32)? as u16,
            "AreaName" => {
                self.dbhost
                    .as_ref()
                    .ok_or("Document schema missing")?
                    .borrow()
                    .schema(d.scope)?
                    .area(&value.text())
                    .ok_or("Unknown document area")?
                    .id
            }
            _ => return Err(format!("Unsupported document assignment: {key}").into()),
        };
        if !self
            .dbhost
            .as_ref()
            .ok_or("Document schema missing")?
            .borrow()
            .schema(d.scope)?
            .areas
            .contains_key(&area)
        {
            return Err("Unknown document area".into());
        }
        d.area = area;
        Ok(())
    }
    pub(super) fn writable_set(
        &self,
        field: WritableField,
        key: &str,
        value: Value,
    ) -> Result<(), Fault> {
        self.allow_document_write()?;
        if !matches!(key, "Value" | "Link") {
            return Err(format!("Unsupported writable field property {key}").into());
        }
        let mut d = field.doc.borrow_mut();
        d.check_lock()?;
        let old = d.field(field.index)?;
        if old.id == 1024 {
            return Err("Document identity is immutable".into());
        }
        let data = scalar(old.wire_type, &value)?;
        d.fields[field.index].as_mut().unwrap().value = data;
        Ok(())
    }
    pub(super) fn writable_property(
        &self,
        field: WritableField,
        key: String,
    ) -> Result<Value, Fault> {
        if key == "Stream" {
            let mut d = field.doc.borrow_mut();
            d.check_lock()?;
            let data = match &d.field(field.index)?.value {
                Data::Binary { data, .. } => data.clone(),
                _ => return Ok(Value::Null),
            };
            let stream = d
                .streams
                .entry(field.index)
                .or_insert_with(|| match InputStream::value(&data) {
                    Value::ReadStream(s) => s,
                    _ => unreachable!(),
                })
                .clone();
            return Ok(Value::ReadStream(stream));
        }
        self.member(Value::StoredField(field.snapshot()?), key)
    }
    pub(super) fn edit_existing(
        &mut self,
        oid: u32,
        scope: u32,
        method: &str,
        args: &[Value],
    ) -> Result<Value, Fault> {
        self.allow_document_write()?;
        let existing = self.document_edits.borrow().get(&(scope, oid)).cloned();
        let doc = if let Some(d) = existing {
            d
        } else {
            if method != "Lock" {
                return Err("Document must be locked before editing".into());
            }
            let d = Rc::new(RefCell::new(Draft::new(
                scope,
                Some(self.read_document(oid, scope)?),
            )));
            self.document_edits
                .borrow_mut()
                .insert((scope, oid), d.clone());
            d
        };
        self.edit_call(doc, method, args)
    }
    pub(super) fn edit_call(
        &mut self,
        doc: Edit,
        method: &str,
        args: &[Value],
    ) -> Result<Value, Fault> {
        self.allow_document_write()?;
        if method == "Attr" || method == "Next" {
            return self.edit_attr(doc, method, args);
        }
        if matches!(method, "Lock" | "Unlock" | "Write") && !args.is_empty() {
            return Err("Document lock/write expects no arguments".into());
        }
        if method == "Lock" {
            let mut d = doc.borrow_mut();
            if d.locked {
                return Ok(Value::Bool(true));
            }
            if d.oid == 0 {
                let oid = self
                    .dbhost
                    .as_ref()
                    .ok_or("Document writer missing")?
                    .borrow_mut()
                    .reserve_document(d.scope, d.area)?;
                d.oid = oid;
                d.fields.push(Some(Field {
                    id: 1024,
                    wire_type: 5,
                    value: Data::Integer(oid),
                }));
            } else if let Some(expected) = &d.expected {
                if self
                    .dbhost
                    .as_ref()
                    .ok_or("Document reader missing")?
                    .borrow()
                    .read(d.scope, d.oid)?
                    .as_ref()
                    != Some(expected)
                {
                    return Err("Документ изменён. Откройте актуальную редакцию.".into());
                }
            }
            if self
                .document_edits
                .borrow()
                .get(&(d.scope, d.oid))
                .is_some_and(|other| !Rc::ptr_eq(other, &doc))
            {
                return Err("Document already has an active edit handle".into());
            }
            self.document_edits
                .borrow_mut()
                .insert((d.scope, d.oid), doc.clone());
            d.locked = true;
            return Ok(Value::Bool(true));
        }
        if method == "Unlock" {
            let mut d = doc.borrow_mut();
            d.locked = false;
            let mut edits = self.document_edits.borrow_mut();
            if edits
                .get(&(d.scope, d.oid))
                .is_some_and(|other| Rc::ptr_eq(other, &doc))
            {
                edits.remove(&(d.scope, d.oid));
            }
            return Ok(Value::Null);
        }
        if method == "Write" {
            let mut d = doc.borrow_mut();
            d.check_lock()?;
            let record = d.record();
            self.dbhost
                .as_ref()
                .ok_or("Document writer missing")?
                .borrow_mut()
                .write_document(d.scope, &record, d.expected.as_ref())?;
            d.expected = Some(record);
            return Ok(Value::Null);
        }
        if method == "NewField" {
            if !(1..=2).contains(&args.len()) {
                return Err("NewField expects attribute[, value]".into());
            }
            let mut d = doc.borrow_mut();
            d.check_lock()?;
            if d.fields.len() >= u16::MAX as usize {
                return Err("Document field limit".into());
            }
            let (id, kind) = self
                .field_definition(d.scope, d.area, &args[0])?
                .ok_or_else(|| {
                    Fault::Script(Value::ScriptException(format!(
                        "Unknown field '{}' in area {}",
                        args[0].text(),
                        d.area
                    )))
                })?;
            if id == 1024 {
                return Err("Document identity is immutable".into());
            }
            let value = if let Some(v) = args.get(1) {
                scalar(kind, v)?
            } else {
                match kind {
                    2 | 3 => Data::String(vec![]),
                    5 => Data::Integer(0),
                    4 => Data::Text(vec![]),
                    1 => Data::Date([0; 4]),
                    12 => Data::Timestamp([0; 8]),
                    8 | 14 => Data::Binary {
                        data: vec![],
                        kind: Some(0),
                        file: Some(FileMetadata {
                            timestamp: 0,
                            source: vec![],
                            name: vec![],
                        }),
                    },
                    _ => return Err(format!("NewField type {kind} needs a value").into()),
                }
            };
            let index = d.fields.len();
            d.fields.push(Some(Field {
                id,
                wire_type: kind,
                value,
            }));
            return Ok(Value::WritableField(WritableField {
                doc: doc.clone(),
                index,
            }));
        }
        if method == "DeleteField" {
            let [Value::WritableField(field)] = args else {
                return Err("DeleteField expects a writable field".into());
            };
            if !Rc::ptr_eq(&doc, &field.doc) {
                return Err("Field belongs to another document".into());
            }
            let mut d = doc.borrow_mut();
            d.check_lock()?;
            if d.field(field.index)?.id == 1024 {
                return Err("Document identity is immutable".into());
            }
            let old = d.field(field.index)?;
            let identity = (old.id, old.wire_type);
            d.deleted_fields.insert(field.index, identity);
            d.fields[field.index] = None;
            d.streams.remove(&field.index);
            return Ok(Value::Null);
        }
        Err(format!("Unsupported persistent document method {method}").into())
    }
    pub(super) fn edit_attr(
        &self,
        doc: Edit,
        method: &str,
        args: &[Value],
    ) -> Result<Value, Fault> {
        let d = doc.borrow();
        let (attr, kind, start, check) = if method == "Next" {
            if args.len() > 2 {
                return Err("Document.Next expects field[, checkLinks]".into());
            }
            let Some(Value::WritableField(f)) = args.first() else {
                return Ok(Value::Null);
            };
            if !Rc::ptr_eq(&doc, &f.doc) {
                return Err("Field belongs to another document".into());
            }
            let (id, kind) = match d.field(f.index) {
                Ok(field) => (field.id, field.wire_type),
                Err(_) => *d
                    .deleted_fields
                    .get(&f.index)
                    .ok_or("Unknown field cursor")?,
            };
            (
                id,
                kind,
                f.index + 1,
                !matches!(args.get(1), Some(Value::Bool(false))),
            )
        } else {
            if args.is_empty() || args.len() > 3 {
                return Err("Document.Attr expects attribute[, checkLinks[, type]]".into());
            }
            let definition = self.field_definition(d.scope, d.area, &args[0])?;
            let attr = match (&args[0], definition) {
                (Value::String(_), Some((id, _))) => id,
                (Value::String(_), None) => return Ok(Value::Null),
                (v, _) => integer(v, 65535)? as u16,
            };
            let kind = if let Some(v) = args.get(2) {
                integer(v, 65535)? as u16
            } else if let Some((_, k)) = definition {
                k
            } else if attr == 1100 {
                5
            } else {
                return Ok(Value::Null);
            };
            (attr, kind, 0, args.get(1).map_or(true, Value::truth))
        };
        // Do not hold a draft borrow while resolving a link to itself.
        let candidates = d
            .fields
            .iter()
            .enumerate()
            .skip(start)
            .filter_map(|(i, f)| {
                f.as_ref()
                    .filter(|f| f.id == attr && f.wire_type == kind)
                    .map(|f| (i, f.clone()))
            })
            .collect::<Vec<_>>();
        let scope = d.scope;
        drop(d);
        for (index, field) in candidates {
            if check {
                if let Data::Integer(oid) = field.value {
                    match self.read_document(oid, scope) {
                        Ok(_) => {}
                        Err(Fault::Script(_)) => continue,
                        Err(e) => return Err(e),
                    }
                }
            }
            return Ok(Value::WritableField(WritableField {
                doc: doc.clone(),
                index,
            }));
        }
        Ok(Value::Null)
    }
}
