//! Database identities shared by KS documents, volumes and SQL connections.
use crate::db::{fields::Record, schema::Schema};
#[derive(Clone, Debug)]
pub struct Volume {
    pub id: u32,
    pub name: String,
    pub title: String,
    pub update: u16,
}
pub trait DatabaseHost {
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
        use super::host::ColumnValues;
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
        let mut cells = Vec::with_capacity(oids.len());
        let mut bytes = 0usize;
        for oid in &oids {
            let record = self
                .read(scope, *oid)?
                .ok_or_else(|| format!("SQL result OID {oid} missing from DataBase({scope})"))?;
            if record.area != area.id {
                return Err(format!("SQL result area mismatch for OID {oid}"));
            }
            let mut row = Vec::with_capacity(fields.len());
            for field in &fields {
                let Some((id, kind)) = *field else {
                    row.push(ColumnValues::Identity);
                    continue;
                };
                if matches!(kind, 8 | 14) {
                    row.push(ColumnValues::StreamRef {
                        oid: *oid,
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
                    bytes += values.len() * 8;
                    if bytes > 4 * 1024 * 1024 {
                        return Err("SQL result byte limit".into());
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
                    bytes += values.len() * 34;
                    if bytes > 4 * 1024 * 1024 {
                        return Err("SQL result byte limit".into());
                    }
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
                        bytes += 4;
                        if bytes > 4 * 1024 * 1024 {
                            return Err("SQL result byte limit".into());
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
                    if raw.len() > 4 * 1024 * 1024 - bytes {
                        return Err("SQL string result byte limit".into());
                    }
                    let text = crate::db::decode(raw);
                    bytes += text.len();
                    if bytes > 4 * 1024 * 1024 {
                        return Err("SQL string result byte limit".into());
                    }
                    values.push(text);
                }
                row.push(ColumnValues::Strings(values));
            }
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
