use super::*;
use std::collections::{BTreeMap, HashMap};

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
        .replace('"', "&quot;").replace('\'', "&#39;")
}
fn items(value: &Value) -> Result<Vec<Value>, Fault> {
    let Value::Array(a) = value else { return Err("P comparison array required".into()); };
    let a = a.borrow();
    let n = a.get("length").ok_or("P comparison length")?.number()?;
    if !(0.0..=65535.0).contains(&n) || n.fract() != 0.0 { return Err("P comparison length".into()); }
    Ok((0..n as usize).map(|i| a.get(&i.to_string()).cloned().unwrap_or(Value::Null)).collect())
}
struct Row { name: String, kind: String, current: Option<String>, display: String }

impl Runtime<'_> {
    // Only conflicts known to occur before publication are recoverable in the
    // form. I/O/journal failures must retain their original fatal error.
    pub(in crate::ks::runtime) fn p_edit_fault(&self, message: String) -> Fault {
        if self.p_edit_guard_required && self.p_edit_checked.is_some() && matches!(message.as_str(),
            "Документ изменён. Откройте актуальную редакцию." |
            "Документ уже изменён. Откройте актуальную редакцию." |
            "Документ уже изменён другим сохранением. Откройте актуальную редакцию.") {
            Fault::Script(Value::Object(Rc::new(RefCell::new(HashMap::from([
                ("codexPConflict".into(), Value::Bool(true)),
                ("Code".into(), Value::Number(409.0)),
            ])))))
        } else { Fault::Host(message) }
    }
    pub(in crate::ks::runtime) fn p_edit_prepare(&mut self, args: &[Value]) -> Result<Value, Fault> {
        if !self.document_write_allowed { return Err("P comparison requires authentication".into()); }
        let [id] = args else { return Err("P comparison identity".into()); };
        let n = id.number()?;
        if !n.is_finite() || n < 1.0 || n > u32::MAX as f64 || n.fract() != 0.0 {
            return Err("P comparison identity".into());
        }
        let oid = n as u32;
        self.document_edits.borrow_mut().remove(&(0, oid));
        self.p_edit_checked = None;
        self.p_edit_snapshot = None;
        let record = self.dbhost.as_ref().ok_or("P document reader missing")?.borrow().read(0, oid)?;
        if let Some(record) = record {
            self.p_edit_snapshot = Some((0, oid, record));
            Ok(Value::Document(oid, 0))
        } else { Ok(Value::Null) }
    }
    pub(in crate::ks::runtime) fn p_edit_conflict(&mut self, args: &[Value]) -> Result<Value, Fault> {
        if !self.document_write_allowed { return Err("P comparison requires authentication".into()); }
        let [id, rows] = args else { return Err("P comparison arguments".into()); };
        let oid = id.number()? as u32;
        let mut fields = BTreeMap::new();
        for row in items(rows)? {
            let r = items(&row)?;
            if r.len() != 6 { return Err("P comparison row".into()); }
            let attr = r[0].number()? as u16;
            fields.insert(attr, Row { name:r[1].text(), kind:r[2].text(),
                current:r[5].truth().then(||r[3].text()), display:r[4].text() });
        }
        // Include submitted attributes even when the current object/field no
        // longer exists. Never discard the rejected user's draft.
        for key in self.p_edit_submitted.keys() {
            if let Some(attr) = key.strip_prefix('a').and_then(|s|s.parse::<u16>().ok()) {
                fields.entry(attr).or_insert(Row {name:format!("Атрибут {attr}"),kind:String::new(),current:None,display:String::new()});
            }
        }
        let snapshot = self.p_edit_snapshot.as_ref().filter(|(scope,id,_)| *scope==0 && *id==oid);
        let mut page = String::from(include_str!("p_edit_conflict_head.html"));
        page.push_str(&format!("<p>Объект: <b>{oid}</b></p>"));
        if let Some((_,_,record)) = snapshot {
            page.push_str("<p>Ваши значения сохранены ниже. Отмечайте «Использовать мои» только у нужных атрибутов. Остальные останутся как в базе. Пустое значение отличается от удаления атрибута.</p>");
            page.push_str(&format!("<form id=\"comparison\" method=\"post\" enctype=\"multipart/form-data\" action=\"/20a?ispravlenieForm\" accept-charset=\"UTF-8\" data-oid=\"{oid}\">
                <input type=\"hidden\" name=\"fser\" value=\"1\"><input type=\"hidden\" name=\"bkod\" value=\"0\">
                <input type=\"hidden\" name=\"deepnumber\" value=\"{oid}\"><input type=\"hidden\" name=\"area\" value=\"{}\">
                <input type=\"hidden\" name=\"codex_p_merge\" value=\"1\"><input type=\"hidden\" name=\"codex_p_version\" value=\"{}\">",
                record.area, version(0,oid,record)?));
        } else {
            page.push_str("<p role=\"alert\">Объект удалён или недоступен. Черновик можно скопировать или скачать. Сохранение и автоматическое воссоздание объекта отключены.</p>");
            page.push_str(&format!("<section id=\"comparison\" data-oid=\"{oid}\">"));
        }
        page.push_str("<table><thead><tr><th>Атрибут</th><th>Сейчас в базе</th><th>Ваши значения</th><th>Что сохранить</th></tr></thead><tbody>");
        for (attr,row) in fields {
            let key=format!("a{attr}");
            let absent=self.p_edit_submitted.get(&format!("codex_absent_{key}")).is_some_and(|s|s=="1") || !self.p_edit_submitted.contains_key(&key);
            let draft=self.p_edit_submitted.get(&key).map(String::as_str).unwrap_or("");
            if row.current.is_none() && absent && draft.is_empty() { continue; }
            let different=row.current.as_deref()!=if absent {None}else{Some(draft)};
            let label=escape(&row.name);
            page.push_str(&format!("<tr data-attr=\"{key}\" class=\"{}\"><th scope=\"row\">{label}<small>№ {attr}; тип {}</small>{}</th><td>",
                if different {"different"}else{"equal"},escape(&row.kind),if different {"<strong>Отличается</strong>"}else{"<small>Совпадает</small>"}));
            if let Some(value)=&row.current {
                page.push_str(&format!("<pre data-current>{}</pre>",escape(value)));
                if row.display!=*value { page.push_str(&format!("<small>{}</small>",escape(&row.display))); }
            } else { page.push_str("<em>Атрибут отсутствует</em>"); }
            page.push_str(&format!("</td><td><textarea data-draft name=\"{key}\" aria-label=\"Ваши значения: {label}\" rows=\"3\">\n{}</textarea>",escape(draft)));
            page.push_str(&format!("<label><input type=\"checkbox\" name=\"codex_absent_{key}\" value=\"1\" {}>Удалить атрибут</label></td><td>",if absent {"checked"}else{""}));
            if snapshot.is_some() {
                page.push_str(&format!("<label><input type=\"radio\" name=\"codex_pick_{key}\" value=\"current\" checked>Оставить из базы</label><label><input type=\"radio\" name=\"codex_pick_{key}\" value=\"draft\">Использовать мои</label>"));
            } else { page.push_str("Сохранение недоступно"); }
            page.push_str("</td></tr>");
        }
        page.push_str("</tbody></table><details><summary>Дополнительный введённый текст</summary>");
        for (key,value) in &self.p_edit_submitted {
            if key.ends_with("txt") || key.ends_with("cont") {
                page.push_str(&format!("<label>{}<textarea data-draft name=\"{}\" readonly>\n{}</textarea></label>",escape(key),escape(key),escape(value)));
            }
        }
        page.push_str("</details><div class=\"actions\"><button type=\"button\" id=\"download\">Скачать черновик</button>");
        if snapshot.is_some() { page.push_str("<button type=\"submit\">Сохранить выбранные значения</button>"); }
        page.push_str("</div>");
        page.push_str(if snapshot.is_some(){"</form>"}else{"</section>"});
        page.push_str(include_str!("p_edit_conflict_tail.html"));
        self.http_output.borrow_mut().insert("CharSet".into(),Value::String("utf-8".into()));
        self.write(&page)?;
        Ok(Value::Null)
    }
}
