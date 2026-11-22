//! Native KOD snapshots: decode without writes, publish through one DB4 transaction.
use super::{document_write::Draft, Fault, Runtime, Value};
use std::{cell::RefCell, rc::Rc};

impl Runtime<'_> {
    pub(super) fn read_kod_document(&mut self, args: &[Value]) -> Result<Value, Fault> {
        let [Value::ReadStream(stream)] = args else {
            return Err("Document.ReadFromStream expects a binary stream".into());
        };
        let mut stream = stream.borrow_mut();
        if stream.closed {
            return Err("KOD stream is closed".into());
        }
        let bytes = stream
            .data
            .get(stream.position..)
            .ok_or("KOD stream position")?;
        let id = bytes.get(..4).ok_or("Неполный файл KOD")?;
        let oid = u32::from_le_bytes(id.try_into().unwrap());
        let mut record = crate::db::fields::parse_record(&bytes[4..])
            .map_err(|e| format!("Неверный файл KOD: {e}"))?;
        if oid == 0
            || record.integer(1024) != Some(oid)
            || record.fields.iter().filter(|f| f.id == 1024).any(|f| {
                f.wire_type != 5
                    || !matches!(f.value, crate::db::fields::Value::Integer(id) if id == oid)
            })
        {
            return Err("KOD: внутренний номер не совпадает с записью".into());
        }
        // Validate the native scalar header and all field encodings before preview.
        crate::db::writer::encode_record(&record)?;
        // Native ReadFromStream/Lock can append an identical identity field on
        // each re-import. Accept such archives, keeping one canonical identity.
        let mut identity = false;
        record.fields = record
            .fields
            .iter()
            .filter(|f| {
                if f.id != 1024 {
                    return true;
                }
                if identity {
                    return false;
                }
                identity = true;
                true
            })
            .cloned()
            .collect::<Vec<_>>()
            .into();
        crate::db::writer::canonicalize(&mut record);
        let mut draft = Draft::new(0, Some(record));
        draft.imported = true;
        stream.position = stream.data.len();
        self.kod_snapshot_loaded = true;
        Ok(Value::EditDocument(Rc::new(RefCell::new(draft))))
    }

    pub(super) fn import_kod_document(&self, args: &[Value]) -> Result<Value, Fault> {
        if !self.document_write_allowed {
            return Err("KOD import requires an authenticated editing request".into());
        }
        let [Value::EditDocument(doc), Value::Volume(scope, volume)] = args else {
            return Err("KOD import expects a snapshot and a selected volume".into());
        };
        let doc = doc.borrow();
        if !doc.imported {
            return Err("KOD snapshot required".into());
        }
        let record = doc.record();
        // Compiling editable KS to native bytecode is not implemented. Never
        // delete a stored form/script and then fail during compilation.
        if matches!(record.area, 1002 | 1010) {
            return Err(
                "Импорт исполняемых KS пока не поддерживается: требуется штатный компилятор".into(),
            );
        }
        self.dbhost
            .as_ref()
            .ok_or("Document writer missing")?
            .borrow_mut()
            .import_document(*scope, volume.id, &record)?;
        Ok(Value::Null)
    }
}
