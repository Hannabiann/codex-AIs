//! Browser P cards carry a digest of the record they displayed. Pin that same
//! record through rendering/checking; Lock and Write still perform DB4 CAS.
use super::{document_write::Draft, Fault, Runtime, Value};
use std::{cell::RefCell, rc::Rc};
#[path = "source_fingerprint.rs"]
mod fingerprint;
#[path = "p_edit_conflict.rs"]
mod conflict;

pub(super) fn draft_key(key: &str) -> bool {
    let key = key.strip_prefix("codex_absent_").unwrap_or(key);
    let Some(rest) = key.strip_prefix('a') else { return false; };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && matches!(&rest[digits..], "" | "txt" | "cont")
}

fn version(scope: u32, oid: u32, record: &crate::db::fields::Record) -> Result<String, Fault> {
    let mut bytes = b"codex-P-card-v1".to_vec();
    bytes.extend_from_slice(&scope.to_le_bytes());
    bytes.extend_from_slice(&oid.to_le_bytes());
    bytes.extend(crate::db::writer::encode_record(record)?);
    Ok(fingerprint::sha256(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

impl Runtime<'_> {
    pub(super) fn p_edit_version(&mut self, args: &[Value]) -> Result<Value, Fault> {
        let [Value::Document(oid, scope)] = args else {
            return Err("P card requires a stored document".into());
        };
        let record = self.read_document(*oid, *scope)?;
        let token = version(*scope, *oid, &record)?;
        // Preserve stored-field iteration for the read-only form. A Draft
        // belongs only to a submission that has passed the version check.
        self.p_edit_snapshot = Some((*scope, *oid, record));
        Ok(Value::String(token))
    }

    pub(super) fn p_edit_check(&mut self, args: &[Value]) -> Result<Value, Fault> {
        self.p_edit_checked = None;
        if !self.document_write_allowed {
            return Err("P editing requires authentication".into());
        }
        let [id, token] = args else {
            return Err("P card version arguments".into());
        };
        let n = id.number()?;
        let token = token.text();
        let token = token.trim();
        if !n.is_finite()
            || n < 1.0
            || n > u32::MAX as f64
            || n.fract() != 0.0
            || token.len() != 64
            || !token.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Ok(Value::Bool(false));
        }
        let oid = n as u32;
        let record = self
            .dbhost
            .as_ref()
            .ok_or("P document reader missing")?
            .borrow()
            .read(0, oid)?;
        let Some(record) = record else {
            return Ok(Value::Bool(false));
        };
        if version(0, oid, &record)? != token {
            return Ok(Value::Bool(false));
        }
        // Retain this exact expected image, not a second read at Lock time.
        self.document_edits
            .borrow_mut()
            .insert((0, oid), Rc::new(RefCell::new(Draft::new(0, Some(record)))));
        self.p_edit_checked = Some((0, oid));
        Ok(Value::Bool(true))
    }
}
