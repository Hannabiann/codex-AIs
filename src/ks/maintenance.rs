//! Portable KOD packages for the stored maintenance form. No shell or SMTP.
use super::{Fault, Runtime, Value};
use crate::db::{fields::Record, writer};
use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use zip::{write::SimpleFileOptions, ZipArchive, ZipWriter};

const MAX_RECORD: u64 = 64 * 1024 * 1024;
const MAX_PACKAGE: u64 = 2 * 1024 * 1024 * 1024;
static SERIAL: AtomicU64 = AtomicU64::new(0);

fn escaped(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn read_entry<R: Read + Seek>(archive: &mut ZipArchive<R>, index: usize) -> Result<Record, String> {
    crate::execution::check()?;
    let mut entry = archive.by_index(index).map_err(|e| format!("ZIP: {e}"))?;
    let name = entry.name().map_err(|e| format!("ZIP: {e}"))?;
    if entry.enclosed_name().is_none()
        || name
            .replace('\\', "/")
            .split('/')
            .any(|s| s == ".." || s.contains(':'))
        || entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 != 0 && mode & 0o170000 != 0o100000)
        || !name.to_ascii_lowercase().ends_with(".kod")
    {
        return Err(
            "ZIP должен содержать только обычные файлы .kod с относительными именами".into(),
        );
    }
    if entry.size() > MAX_RECORD {
        return Err("Файл KOD в ZIP превышает 64 МиБ".into());
    }
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        crate::execution::check()?;
        let n = entry.read(&mut chunk).map_err(|e| format!("ZIP: {e}"))?;
        if n == 0 {
            break;
        }
        if bytes.len() as u64 + n as u64 > MAX_RECORD {
            return Err("Файл KOD в ZIP превышает 64 МиБ".into());
        }
        bytes.extend_from_slice(&chunk[..n]);
    }
    writer::kod::decode_file(&bytes)
}

fn encode_record(record: &Record) -> Result<Vec<u8>, String> {
    let mut bytes = 123456789u32.to_le_bytes().to_vec();
    bytes.extend(
        record
            .integer(1024)
            .ok_or("KOD: отсутствует внутренний номер")?
            .to_le_bytes(),
    );
    bytes.extend(writer::encode_record(record)?);
    Ok(bytes)
}

fn upload_root(data: &Path) -> Result<PathBuf, String> {
    let root = data.canonicalize().map_err(|e| e.to_string())?;
    let output = root.join("Upload");
    match std::fs::symlink_metadata(&output) {
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(&output).map_err(|e| e.to_string())?;
        }
        Err(e) => return Err(e.to_string()),
    }
    let output = output.canonicalize().map_err(|e| e.to_string())?;
    if output.parent() != Some(root.as_path()) || !output.is_dir() {
        return Err("Upload должен находиться непосредственно в каталоге базы".into());
    }
    Ok(output)
}

impl Runtime<'_> {
    pub(super) fn maintenance_export(&mut self, args: &[Value]) -> Result<Value, Fault> {
        if !self.maintenance_allowed {
            return Err("Выгрузка доступна через авторизованную форму mgmt".into());
        }
        if args.len() != 8 {
            return Err("mgmt export expects case and seven object lists".into());
        }
        let (oid, scope) = match &args[0] {
            Value::Document(oid, scope) => (*oid, *scope),
            Value::EditDocument(doc) => {
                let d = doc.borrow();
                (d.oid, d.scope)
            }
            _ => return Err("Судебное дело не найдено".into()),
        };
        let case = self.read_document(oid, scope)?;
        let mut objects = vec![("Case", scope, oid)];
        for (prefix, value) in [
            "Document",
            "File",
            "Letter",
            "Side",
            "StatCard",
            "Participant",
            "Adress",
        ]
        .into_iter()
        .zip(&args[1..])
        {
            match value {
                Value::Null => (),
                Value::Array(items) => {
                    let items = items.borrow();
                    let count = items
                        .get("length")
                        .ok_or("mgmt array length missing")?
                        .number()? as usize;
                    for i in 0..count {
                        let item = items.get(&i.to_string()).ok_or("mgmt array item missing")?;
                        match item {
                            Value::Document(id, scope) => objects.push((prefix, *scope, *id)),
                            Value::EditDocument(doc) => {
                                let d = doc.borrow();
                                objects.push((prefix, d.scope, d.oid));
                            }
                            _ => return Err("mgmt export expects document arrays".into()),
                        }
                    }
                }
                Value::Host(handle) => {
                    let (scope, _, ids) = handle.list_snapshot()?;
                    objects.extend(ids.into_iter().map(|id| (prefix, scope, id)));
                }
                _ => return Err("mgmt export expects SQL object lists".into()),
            }
        }
        let directory = upload_root(Path::new(
            self.data_root.as_deref().ok_or("data root missing")?,
        ))?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_millis();
        let base = format!(
            "case-{oid}-{stamp}-{}",
            SERIAL.fetch_add(1, Ordering::Relaxed)
        );
        let target = directory.join(format!("{base}.zip"));
        let temporary = directory.join(format!("{base}.part"));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        let result = (|| -> Result<(), Fault> {
            let mut archive = ZipWriter::new(file);
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            let folder = crate::db::decode(case.string(1).unwrap_or_default())
                .chars()
                .map(|c| {
                    if c.is_control() || "\\/:*?\"<>|".contains(c) {
                        '-'
                    } else {
                        c
                    }
                })
                .collect::<String>();
            let mut seen = HashSet::new();
            for (prefix, scope, oid) in objects {
                crate::execution::check()?;
                if !seen.insert((scope, oid)) {
                    continue;
                }
                let record = self.read_document(oid, scope)?;
                let bytes = encode_record(&record)?;
                archive
                    .start_file(format!("Дело {folder}/{prefix}_{oid}.kod"), options)
                    .map_err(|e| e.to_string())?;
                for chunk in bytes.chunks(65536) {
                    crate::execution::check()?;
                    archive.write_all(chunk).map_err(|e| e.to_string())?;
                }
            }
            archive
                .finish()
                .map_err(|e| e.to_string())?
                .sync_all()
                .map_err(|e| e.to_string())?;
            crate::execution::check()?;
            std::fs::rename(&temporary, &target).map_err(|e| e.to_string())?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result?;
        Ok(Value::String(format!("Upload/{base}.zip")))
    }

    pub(super) fn maintenance_import(&mut self, args: &[Value]) -> Result<Value, Fault> {
        if !self.maintenance_allowed || !self.document_write_allowed {
            return Err("Загрузка доступна через авторизованную форму mgmt".into());
        }
        let [Value::String(name), Value::String(temp)] = args else {
            return Err("Выберите ZIP с файлами .kod".into());
        };
        let mut name = super::super::path::script_path(name).replace('\\', "/");
        // CMultiFileClassifier submits the staging token returned by
        // savetempfile, not the client filename or an absolute server path.
        if !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit() || b == b'_') {
            let staged = format!("{}/{}", temp.trim_end_matches(['/', '\\']), name);
            let directory = self
                .attachment_destination(&staged)?
                .ok_or("Неизвестный каталог временной загрузки")?;
            let directory = directory.canonicalize().map_err(|e| e.to_string())?;
            let mut candidates = Vec::new();
            for entry in std::fs::read_dir(&directory).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                if entry.file_type().map_err(|e| e.to_string())?.is_file()
                    && entry
                        .path()
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
                {
                    let file = entry.path().canonicalize().map_err(|e| e.to_string())?;
                    if file.parent() != Some(directory.as_path()) {
                        return Err("ZIP вне временной загрузки".into());
                    }
                    candidates.push(file);
                }
            }
            if candidates.len() != 1 {
                return Err("Выберите один загруженный ZIP с файлами .kod".into());
            }
            name = candidates.remove(0).to_string_lossy().into_owned();
        }
        let data = Path::new(self.data_root.as_deref().ok_or("data root missing")?);
        let input = data
            .join(&name)
            .canonicalize()
            .map_err(|e| format!("ZIP: {e}"))?;
        let upload = upload_root(data)?;
        let attachment = self
            .attachment_destination(&name)?
            .map(|p| p.canonicalize())
            .transpose()
            .map_err(|e| e.to_string())?;
        if !input.starts_with(&upload) && attachment.as_ref() != Some(&input) {
            return Err("ZIP должен быть предварительно загружен через форму".into());
        }
        let mut archive = ZipArchive::new(File::open(&input).map_err(|e| e.to_string())?)
            .map_err(|e| format!("ZIP: {e}"))?;
        let host = self.dbhost.as_ref().ok_or("Database missing")?;
        let catalog = host.borrow().shared_schema(0)?;
        let volumes = host.borrow().volumes(0)?;
        let mut writable = Vec::new();
        for volume in volumes {
            if host.borrow().volume_writable(0, volume.id)? {
                let path = host.borrow().kod_target_path(0, volume.id)?;
                if !writer::kod::supports_case_target(&path)? {
                    continue;
                }
                let schema = host.borrow().volume_schema(0, volume.id)?;
                writable.push((volume, schema));
            }
        }
        let mut plan = Vec::new();
        let mut seen = HashSet::new();
        let mut total = 0u64;
        // Validate the entire package and destination schemas before the first
        // DB4 transaction. Decode again on import to keep memory to one record.
        for index in 0..archive.len() {
            {
                let entry = archive.by_index(index).map_err(|e| format!("ZIP: {e}"))?;
                if entry.is_dir() {
                    continue;
                }
                total = total.checked_add(entry.size()).ok_or("ZIP size overflow")?;
                if total > MAX_PACKAGE {
                    return Err("Распакованный пакет превышает 2 ГиБ".into());
                }
            }
            let record = read_entry(&mut archive, index)?;
            if matches!(record.area, 5 | 8 | 900 | 901 | 1002 | 1003 | 1010) {
                return Err(format!(
                    "Область {} не относится к данным судебного дела",
                    record.area
                )
                .into());
            }
            let oid = record.integer(1024).ok_or("KOD oid")?;
            if !seen.insert(oid) {
                return Err(format!("В пакете повторяется OID {oid}").into());
            }
            let (volume, schema) = writable
                .iter()
                .find(|(_, s)| s.areas.contains_key(&record.area))
                .ok_or_else(|| format!("Нет записываемого тома для области {}", record.area))?;
            let area = &schema.areas[&record.area];
            for field in record.fields.iter() {
                if matches!(field.id, 1024 | 1100) && field.wire_type == 5 {
                    continue;
                }
                if !area
                    .fields
                    .get(&field.id)
                    .or_else(|| {
                        catalog
                            .areas
                            .get(&record.area)
                            .and_then(|a| a.fields.get(&field.id))
                    })
                    .is_some_and(|f| f.kind == field.wire_type)
                {
                    return Err(format!(
                        "KOD {oid}: атрибут {} не соответствует структуре тома",
                        field.id
                    )
                    .into());
                }
            }
            plan.push((index, volume.clone()));
        }
        if plan.is_empty() {
            return Err("В ZIP нет файлов .kod".into());
        }
        let mut report = String::from("<table class=mp_extensions_registry_table><tr><th>№</th><th>Внутренний номер</th><th>Наименование</th><th>Том</th><th>Область</th></tr>");
        for (number, (index, volume)) in plan.iter().enumerate() {
            crate::execution::check()?;
            let record = read_entry(&mut archive, *index)?;
            host.borrow_mut().import_case_document(0, volume.id, &record)
                .map_err(|e| format!("Загружено {number} из {} файлов. OID {}: {e}. Уже завершённые записи сохранены; загрузку можно повторить.", plan.len(), record.integer(1024).unwrap()))?;
            report.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                number + 1,
                record.integer(1024).unwrap(),
                escaped(
                    &self
                        .member(
                            Value::Document(record.integer(1024).unwrap(), 0),
                            "Name".into()
                        )?
                        .text()
                ),
                escaped(&volume.name),
                record.area
            ));
        }
        report.push_str(&format!(
            "</table><p>Успешно загружено файлов: {}.</p>",
            plan.len()
        ));
        Ok(Value::String(report))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::fields::{Field, Value as Data};
    use std::io::Cursor;

    fn record() -> Record {
        Record {
            area: 42,
            flags: 0,
            fields: vec![
                Field {
                    id: 1,
                    wire_type: 2,
                    value: Data::String(b"same-name".to_vec()),
                },
                Field {
                    id: 1024,
                    wire_type: 5,
                    value: Data::Integer(12345),
                },
            ]
            .into(),
        }
    }
    fn package(name: &str, data: &[u8]) -> Vec<u8> {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file(
            name,
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(data).unwrap();
        zip.finish().unwrap().into_inner()
    }
    #[test]
    fn kod_zip_validates_payload_and_rejects_bad_entries() {
        let mut source = record();
        writer::canonicalize(&mut source);
        let data = encode_record(&source).unwrap();
        let bytes = package("case/Document_12345.kod", &data);
        let mut zip = ZipArchive::new(Cursor::new(bytes.clone())).unwrap();
        assert_eq!(read_entry(&mut zip, 0).unwrap(), source);
        for name in [
            "../document.kod",
            "case/../../document.kod",
            "C:/document.kod",
            "case/script.exe",
        ] {
            let bytes = package(name, &data);
            assert!(
                read_entry(&mut ZipArchive::new(Cursor::new(bytes)).unwrap(), 0).is_err(),
                "{name}"
            );
        }
        let mut damaged = bytes;
        let offset = damaged.windows(data.len()).position(|w| w == data).unwrap();
        damaged[offset + data.len() - 1] ^= 1;
        assert!(read_entry(&mut ZipArchive::new(Cursor::new(damaged)).unwrap(), 0).is_err());
        let bytes = package("Document_12345.kod", b"not a KOD snapshot");
        assert!(read_entry(&mut ZipArchive::new(Cursor::new(bytes)).unwrap(), 0).is_err());
    }
    #[test]
    fn maintenance_adapters_require_authenticated_post() {
        let mut runtime = Runtime::new("/20a");
        assert!(runtime.maintenance_export(&[]).is_err());
        assert!(runtime
            .maintenance_import(&[Value::String("Upload/test.zip".into())])
            .is_err());
    }
}
