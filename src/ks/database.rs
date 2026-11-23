//! Database identities shared by KS documents, volumes and SQL connections.
use crate::db::{fields::Record, schema::Schema};
#[derive(Clone, Debug)]
pub struct Volume {
    pub id: u32,
    pub name: String,
    pub title: String,
    pub update: u16,
}
pub struct ObjectPage {
    pub area: u16,
    pub ids: Vec<u32>,
    pub start: usize,
    pub estimated: usize,
}
pub enum DeleteError {
    Missing,
    ReadOnly,
    Failed(String),
}
impl From<String> for DeleteError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}
pub trait DatabaseHost {
    /// Locate a bounded cursor window without loading its records.
    fn prepare_cursor(&self, _scope: u32, _oids: &[u32]) -> Result<(), String> {
        Ok(())
    }
    fn object_page(
        &self,
        _scope: u32,
        _sql: &str,
        _start: usize,
        _size: usize,
    ) -> Result<ObjectPage, String> {
        Err("Paged object query is not configured".into())
    }
    fn delete_document_from_volume(
        &mut self,
        _scope: u32,
        _oid: u32,
        _hole: bool,
        _volume: u32,
    ) -> Result<(), DeleteError> {
        Err(DeleteError::Failed(
            "Volume-specific document deletion is not configured".into(),
        ))
    }
    fn distribution_judges(&self, _scope: u32, _sql: &str) -> Result<Vec<u32>, String> {
        Err("Historical distribution catalog is not configured".into())
    }
    fn delete_document(
        &mut self,
        _scope: u32,
        _oid: u32,
        _hole: bool,
        _expected: &Record,
    ) -> Result<(), String> {
        Err("Persistent document deletion is not configured".into())
    }
    fn volume_writable(&self, _scope: u32, _volume: u32) -> Result<bool, String> {
        Ok(false)
    }
    fn volume_schema(&self, _scope: u32, _volume: u32) -> Result<Schema, String> {
        Err("Volume schema requires a mounted database".into())
    }
    fn import_document(
        &mut self,
        _scope: u32,
        _volume: u32,
        _record: &Record,
    ) -> Result<(), String> {
        Err("KOD import requires a writable database".into())
    }
    fn select_indexed(
        &self,
        _scope: u32,
        _sql: &str,
    ) -> Result<Option<super::host::QueryData>, String> {
        Ok(None)
    }
    fn reserve_document(&mut self, _scope: u32, _area: u16) -> Result<u32, String> {
        Err("Persistent document writes are not configured".into())
    }
    fn write_document(
        &mut self,
        _scope: u32,
        _record: &Record,
        _expected: Option<&Record>,
    ) -> Result<(), String> {
        Err("Persistent document writes are not configured".into())
    }
    fn finish_profile(&self) {}
    /// Detach contexts no longer reachable from the interpreter. Scope zero
    /// belongs to the catalog and must always survive request cleanup.
    fn retain_scopes(&mut self, _live: &std::collections::BTreeSet<u32>) {}
    fn schema_revision(&self) -> u64 {
        0
    }
    fn document_revision(&self, _scope: u32, _oid: u32) -> Result<Option<u16>, String> {
        Ok(None)
    }
    fn create(&mut self) -> Result<u32, String>;
    fn open(
        &mut self,
        scope: u32,
        path: &str,
        flags: &[bool],
    ) -> Result<Volume, super::runtime::OpenError>;
    fn remove(&mut self, scope: u32, volume: u32) -> Result<(), String>;
    fn volumes(&self, scope: u32) -> Result<Vec<Volume>, String>;
    fn schema(&self, scope: u32) -> Result<Schema, String>;
    fn shared_schema(&self, scope: u32) -> Result<std::sync::Arc<Schema>, String> {
        Ok(std::sync::Arc::new(self.schema(scope)?))
    }
    fn read(&self, scope: u32, oid: u32) -> Result<Option<Record>, String>;
    fn metadata(
        &self,
        scope: u32,
        oid: u32,
    ) -> Result<Option<crate::db::fields::RecordMetadata>, String> {
        Ok(self.read(scope, oid)?.as_ref().map(Into::into))
    }
    fn named(&self, _scope: u32, _name: &str) -> Result<Option<Record>, String> {
        Err("named URL lookup requires a mounted database".into())
    }
    fn document_volume(&self, _scope: u32, _oid: u32) -> Result<Volume, String> {
        Err("document volume requires mounted database context".into())
    }
    fn attribute(&self, scope: u32, oid: u32, name: &str) -> Result<u16, String>;
    fn query(&self, scope: u32, sql: &str) -> Result<Vec<u32>, String>;
    fn fulltext_result(
        &self,
        _scope: u32,
        _sql: &str,
    ) -> Result<crate::db::free_text::ResultData, String> {
        Ok(Default::default())
    }
    /// Full list cursors retain identities and decode just their current row.
    /// Callers opt in only for read-only scans that do not require frozen cells.
    fn select_cursor(&self, scope: u32, sql: &str) -> Result<super::host::QueryData, String> {
        if super::sql_join::parse(sql)?.is_some() {
            return self.select(scope, sql);
        }
        let q = super::sql::parse(sql)?;
        if !q.projection.has_fields() {
            return self.select(scope, sql);
        }
        let schema = self.shared_schema(scope)?;
        let area = schema
            .area(&q.area)
            .ok_or_else(|| format!("Unknown SQL area {}", q.area))?;
        q.fields(area)?;
        Ok(super::host::QueryData {
            area: Some(area.id),
            oids: self.query(scope, sql)?,
            cells: Vec::new(),
            fulltext: self.fulltext_result(scope, sql)?,
        })
    }
    fn project_row(
        &self,
        scope: u32,
        oid: u32,
        area: u16,
        fields: &[Option<(u16, u16)>],
    ) -> Result<Vec<super::host::ColumnValues>, String> {
        use super::host::ColumnValues;
        crate::execution::check()?;
        let record = self
            .read(scope, oid)?
            .ok_or_else(|| format!("SQL result OID {oid} missing from DataBase({scope})"))?;
        if record.area != area {
            return Err(format!("SQL result area mismatch for OID {oid}"));
        }
        let mut row = Vec::with_capacity(fields.len());
        for field in fields {
            let Some((id, kind)) = *field else {
                row.push(ColumnValues::Identity);
                continue;
            };
            if matches!(kind, 4 | 8 | 14) {
                row.push(ColumnValues::StreamRef {
                    oid: oid,
                    field: id,
                    kind,
                });
                continue;
            }
            if kind == 1 || kind == 12 {
                let mut values = Vec::new();
                for f in record
                    .fields
                    .iter()
                    .filter(|f| f.id == id && f.wire_type == kind)
                {
                    let p = match &f.value {
                        crate::db::fields::Value::Date(b) => [
                            i16::from_le_bytes([b[0], b[1]]) as i64,
                            b[2] as i64 - 1,
                            b[3] as i64,
                            0,
                            0,
                            0,
                            0,
                        ],
                        crate::db::fields::Value::Timestamp(b) => [
                            u16::from_be_bytes([b[0], b[1]]) as i64,
                            b[2] as i64 - 1,
                            b[3] as i64,
                            b[4] as i64,
                            b[5] as i64,
                            b[6] as i64,
                            b[7] as i64 * 10,
                        ],
                        _ => return Err("SQL date field type mismatch".into()),
                    };
                    values.push(super::date::Date::from_parts(p)?.millis);
                }
                row.push(ColumnValues::Dates(values));
                continue;
            }
            if kind == 13 {
                let values = record
                    .fields
                    .iter()
                    .filter(|f| f.id == id && f.wire_type == kind)
                    .map(|f| match &f.value {
                        crate::db::fields::Value::Decimal(b) => Ok(*b),
                        _ => Err("SQL decimal field type mismatch".to_string()),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                row.push(ColumnValues::Decimals(values));
                continue;
            }
            if kind == 5 {
                let mut values = Vec::new();
                for f in record
                    .fields
                    .iter()
                    .filter(|f| f.id == id && f.wire_type == kind)
                {
                    let crate::db::fields::Value::Integer(value) = f.value else {
                        return Err(format!(
                            "SQL link projection type mismatch: OID {oid}, field {id}"
                        ));
                    };
                    if f.wire_type != kind {
                        return Err(format!("SQL field {id} wire/schema type mismatch"));
                    }
                    values.push(value);
                }
                row.push(ColumnValues::Links(values));
                continue;
            }
            // Keep storage order for array access. Scalar string access
            // chooses the final occurrence (verified with native reports).
            let mut values = Vec::new();
            for f in record
                .fields
                .iter()
                .filter(|f| f.id == id && f.wire_type == kind)
            {
                let crate::db::fields::Value::String(raw) = &f.value else {
                    return Err(format!(
                        "SQL string projection type mismatch: OID {oid}, field {id}"
                    ));
                };
                if f.wire_type != kind {
                    return Err(format!("SQL field {id} wire/schema type mismatch"));
                }
                // Keep the per-field guard; the total result has no 4 MiB cap.
                if raw.len() > 4 * 1024 * 1024 {
                    return Err("SQL string field byte limit".into());
                }
                let text = crate::db::decode(raw);
                values.push(text);
            }
            row.push(ColumnValues::Strings(values));
        }
        Ok(row)
    }
    fn select(&self, scope: u32, sql: &str) -> Result<super::host::QueryData, String> {
        if let Some(data) = self.select_indexed(scope, sql)? {
            return Ok(data);
        }
        if std::env::var_os("KODEKS_TRACE_SQL_REQUEST").is_some_and(|v| v == "1") {
            eprintln!(
                "SQL statement DataBase({scope}): {}",
                sql.chars().take(2048).collect::<String>()
            );
        }
        if let Some(join) = super::sql_join::parse(sql)? {
            return join.execute(self, scope);
        }
        let q = super::sql::parse(sql)?;
        if !q.projection.has_fields() {
            return Ok(super::host::QueryData {
                area: self.shared_schema(scope)?.area(&q.area).map(|area| area.id),
                oids: self.query(scope, sql)?,
                cells: vec![],
                fulltext: self.fulltext_result(scope, sql)?,
            });
        }
        let schema = self.shared_schema(scope)?;
        let area = schema
            .area(&q.area)
            .ok_or_else(|| format!("SQL area {} absent from DataBase({scope})", q.area))?;
        let fields = q.fields(area)?;
        let oids = self.query(scope, sql)?;
        if std::env::var_os("KODEKS_TRACE").is_some_and(|s| s == "1") {
            eprintln!(
                "KS SQL DataBase({scope}): area={}, rows={}, columns={}",
                q.area,
                oids.len(),
                fields.len()
            );
        }
        if oids.len() > super::sql::MAX_SQL_ROWS {
            return Err("SQL row limit".into());
        }
        // A complete postal-range list exceeds the old 4 MiB aggregate cap.
        // Preserve every projected row; allocation failures and cancellation
        // remain explicit errors rather than silently truncating the result.
        let mut cells = Vec::new();
        cells
            .try_reserve(oids.len())
            .map_err(|e| format!("SQL result allocation: {e}"))?;
        for oid in &oids {
            crate::execution::check()?;
            let row = self.project_row(scope, *oid, area.id, &fields)?;
            cells.push(row);
        }
        Ok(super::host::QueryData {
            area: Some(area.id),
            oids,
            cells,
            fulltext: self.fulltext_result(scope, sql)?,
        })
    }
}
