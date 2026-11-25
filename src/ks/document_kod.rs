//! Native KOD snapshots: decode without writes, publish through one DB4 transaction.
use super::{document_write::Draft, Fault, Runtime, Value};
use std::{cell::RefCell, rc::Rc};

impl Runtime<'_> {
    pub(crate) fn kod_link_exists(&self, oid: u32) -> Result<bool, String> {
        self.dbhost
            .as_ref()
            .ok_or("Database missing")?
            .borrow()
            .metadata(0, oid)
            .map(|r| r.is_some())
    }
    pub(crate) fn kod_volumes(&self) -> Result<Vec<super::super::database::Volume>, String> {
        let host = self.dbhost.as_ref().ok_or("Database missing")?.borrow();
        host.volumes(0)?
            .into_iter()
            .filter_map(|v| match host.volume_writable(0, v.id) {
                Ok(true) => Some(Ok(v)),
                Ok(false) => None,
                Err(e) => Some(Err(e)),
            })
            .collect()
    }

    pub(crate) fn kod_target(
        &self,
        name: &str,
        records: &[crate::db::fields::Record],
    ) -> Result<std::path::PathBuf, String> {
        let host = self.dbhost.as_ref().ok_or("Database missing")?.borrow();
        let volume = host
            .volumes(0)?
            .into_iter()
            .find(|v| v.name == name)
            .ok_or("Выбранный том не подключён")?
            .id;
        let path = host.kod_target_path(0, volume)?;
        let schema = host.volume_schema(0, volume)?;
        for record in records {
            // The ordinary P editor excludes these areas. Do not turn the
            // batch uploader into an alternative script/user/URL editor.
            if matches!(
                record.area,
                5 | 8 | 900 | 901 | 1002 | 1003 | 1010 | 7410 | 7411 | 7412
            ) {
                return Err(format!(
                    "Область {} недоступна для пакетного восстановления",
                    record.area
                ));
            }
            let area = schema
                .areas
                .get(&record.area)
                .ok_or("Область объекта отсутствует в выбранном томе")?;
            for f in record.fields.iter() {
                if matches!(f.id, 1024 | 1100) && f.wire_type == 5 {
                    continue;
                }
                if !area
                    .fields
                    .get(&f.id)
                    .is_some_and(|d| d.kind == f.wire_type)
                {
                    return Err(format!(
                        "KOD: атрибут {} типа {} не соответствует структуре тома",
                        f.id, f.wire_type
                    ));
                }
            }
        }
        Ok(path)
    }
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
        let record = crate::db::writer::kod::decode_snapshot(bytes)?;
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
