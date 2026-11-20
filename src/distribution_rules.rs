use super::{escape, form, reply, Reply};
use crate::{
    db::{self, fields::Record},
    ks::runtime::{
        xml_sax::{events, Event},
        Runtime,
    },
    server::RequestContext,
};
use std::fmt::Write;

const ACCESS: &[(&str, &str)] = &[
    ("JudeAccess", "Распределение по судьям"),
    ("ReJudeAccess", "Перераспределение по судьям"),
    ("KolAccess", "Распределение по коллегиям"),
    ("ReKolAccess", "Перераспределение по коллегиям"),
    ("SostAccess", "Распределение по составам"),
    ("ReSostAccess", "Перераспределение по составам"),
    ("BokAccess", "Боковые судьи"),
    ("ConfAccess", "Конфиденциальные дела"),
    ("BankAccess", "Банкротные дела"),
    ("BankSod", "Содержание банкротных документов"),
];
const COLUMNS: &[(&str, &str)] = &[
    ("otpuskb", "Длительность отпуска, дней"),
    ("otpusks", "Условие"),
    ("otpuske", "Верхняя граница, дней"),
    ("srok", "Срок рассмотрения"),
    ("kolleg", "Коллегия"),
    ("begin", "Прекратить за дней до начала"),
    ("end", "Возобновить за дней до окончания"),
];
type Attrs = Vec<(String, String)>;
fn attr<'a>(a: &'a Attrs, k: &str) -> &'a str {
    a.iter().find(|(n, _)| n == k).map_or("", |(_, v)| v)
}
fn set(a: &mut Attrs, k: &str, v: String) {
    if let Some(x) = a.iter_mut().find(|(n, _)| n == k) {
        x.1 = v
    } else {
        a.push((k.into(), v))
    }
}
fn start_tag(out: &mut String, tag: &str, a: &Attrs) {
    write!(out, "<{tag}").unwrap();
    for (k, v) in a {
        write!(out, " {k}=\"{}\"", escape(v)).unwrap()
    }
    out.push('>')
}
struct Rules {
    access: Attrs,
    groups: Vec<Attrs>,
}
impl Rules {
    fn read(xml: &str) -> Result<Self, String> {
        if xml.contains("<!") || xml.contains("<?") {
            return Err("Дополнительные секции XML требуют штатного редактора".into());
        }
        let mut access = None;
        let mut groups = Vec::new();
        let mut depth = 0;
        for event in events(xml)? {
            match event {
                Event::Start(n, a) if depth == 0 && n == "SODERG" => {
                    access = Some(a);
                    depth += 1;
                }
                Event::Start(n, a) if depth == 1 && n == "GROUP" => {
                    groups.push(a);
                    depth += 1;
                }
                Event::End(_) => depth -= 1,
                Event::Text(t) if t.trim().is_empty() => {}
                _ => return Err("Неизвестный формат правил распределения".into()),
            }
        }
        Ok(Self {
            access: access.ok_or("Нет секции SODERG")?,
            groups,
        })
    }
    fn xml(&self) -> String {
        let mut s = String::new();
        start_tag(&mut s, "SODERG", &self.access);
        s.push('\n');
        for a in &self.groups {
            start_tag(&mut s, "GROUP", a);
            s.push_str("</GROUP>\n")
        }
        s.push_str("</SODERG>\n");
        s
    }
}
fn oid(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && s.parse::<u32>().is_ok_and(|n| n > 0)
}
fn oid_list(s: &str) -> bool {
    s.len() < 16_384 && (s.is_empty() || s.trim_end_matches(';').split(';').all(oid))
}
fn apply(r: &mut Rules, req: &RequestContext) -> Result<(), String> {
    let action = form(req, "action").ok_or("Не указано действие")?;
    if action == "access" {
        let key = form(req, "key")
            .filter(|k| ACCESS.iter().any(|(name, _)| name == k))
            .ok_or("Неизвестная группа")?;
        let value = form(req, "value")
            .filter(|v| oid_list(v))
            .ok_or("Введите номера объектов через точку с запятой")?;
        set(&mut r.access, key, value.into());
        return Ok(());
    }
    let i = form(req, "row")
        .and_then(|v| v.parse::<usize>().ok())
        .ok_or("Неверная строка")?;
    if action == "remove" && i < r.groups.len() {
        r.groups.remove(i);
        return Ok(());
    }
    if action != "rule" || i > r.groups.len() {
        return Err("Правило отсутствует".into());
    }
    let mut values = Vec::new();
    for &(k, _) in COLUMNS {
        let v = form(req, k).ok_or("Не все поля правила переданы")?;
        let valid = match k {
            "otpusks" => matches!(v, "0" | "1" | "2" | "3"),
            "kolleg" => oid(v),
            "srok" => {
                !v.is_empty() && v.len() <= 80 && v.chars().all(|c| c.is_alphanumeric() || c == ' ')
            }
            _ => v.is_empty() || v.bytes().all(|b| b.is_ascii_digit()) && v.parse::<u32>().is_ok(),
        };
        if !valid {
            return Err("Проверьте срок, условие и номер коллегии".into());
        }
        values.push(v);
    }
    if values[0].is_empty() || (values[1] == "3" && values[2].is_empty()) {
        return Err("Заполните границы отпуска".into());
    }
    if i == r.groups.len() {
        r.groups.push(Vec::new())
    }
    for (&(k, _), v) in COLUMNS.iter().zip(&values) {
        set(&mut r.groups[i], k, (*v).into())
    }
    set(&mut r.groups[i], "fullstr", values.join("#"));
    Ok(())
}
fn options(out: &mut String, items: &[(&str, &str)], current: &str) {
    for (v, title) in items {
        write!(
            out,
            "<option value='{}' {}>{}</option>",
            escape(v),
            if *v == current { "selected" } else { "" },
            escape(title)
        )
        .unwrap()
    }
}
pub(super) fn section(
    out: &mut String,
    runtime: &Runtime,
    doc: &Record,
    req: &RequestContext,
    token: &str,
) -> Result<Option<Reply>, String> {
    let mut fields = doc.fields.iter().filter(|f| f.id == 4 && f.wire_type == 4);
    let f = fields.next().ok_or("Отсутствует XML правил")?;
    if fields.next().is_some() {
        return Err("Найдено несколько полей правил".into());
    }
    let bytes = crate::document::bytes(f)?;
    let xml = db::decode(bytes);
    let mut r = Rules::read(&xml)?;
    let version = format!(
        "{}-{}-{:08x}",
        doc.integer(1024).ok_or("Нет OID")?,
        bytes.len(),
        crc32fast::hash(bytes)
    );
    if req.method == "POST" && form(req, "section") == Some("rules") {
        if form(req, "version") != Some(&version) {
            return Ok(Some(reply(
                409,
                "Правила изменились. Откройте форму заново.".into(),
            )));
        }
        if let Err(e) = apply(&mut r, req) {
            return Ok(Some(reply(400, escape(&e))));
        }
        let data = r.xml();
        let (b, _, bad) = encoding_rs::WINDOWS_1251.encode(&data);
        if bad {
            return Ok(Some(reply(
                400,
                "Проверьте кодировку введённых значений".into(),
            )));
        }
        if b.as_ref() != bytes {
            if let Err(e) = runtime.save_distribution_xml(doc, b.into_owned()) {
                return Ok(Some(reply(409, escape(&e))));
            }
        }
        return Ok(Some((
            Vec::new(),
            303,
            vec![("Location".into(), "/20a?raspred_settings".into())],
        )));
    }
    let hidden=format!("<input type=hidden name=section value=rules><input type=hidden name=version value='{version}'><input type=hidden name=token value='{token}'>");
    out.push_str("<h2>Правила на время отпуска</h2><table><thead><tr>");
    for (_, label) in COLUMNS {
        write!(out, "<th>{label}</th>").unwrap()
    }
    out.push_str("<th></th></tr></thead><tbody>");
    for (i, a) in r
        .groups
        .iter()
        .chain(std::iter::once(&Vec::new()))
        .enumerate()
    {
        let adding = i == r.groups.len();
        out.push_str("<tr>");
        for &(k, label) in COLUMNS {
            let value = attr(a, k);
            if k == "otpusks" {
                write!(
                    out,
                    "<td><select form='rule{i}' aria-label='{label}' name='{k}'>"
                )
                .unwrap();
                options(
                    out,
                    &[("0", "Точно"), ("1", "По"), ("2", "С"), ("3", "С … по")],
                    value,
                );
                out.push_str("</select></td>");
            } else {
                let size = if matches!(k, "srok" | "kolleg") {
                    10
                } else {
                    4
                };
                write!(out,"<td><input form='rule{i}' aria-label='{label}' name='{k}' size='{size}' value='{}'>",escape(value)).unwrap();
                if k == "kolleg" {
                    if let Some(record) = value
                        .parse()
                        .ok()
                        .map(|n| runtime.read_loaded_document(n))
                        .transpose()?
                        .flatten()
                    {
                        if let Some(s) = record.string(1) {
                            write!(out, "<small>{}</small>", escape(&db::decode(s))).unwrap()
                        }
                    }
                }
                out.push_str("</td>");
            }
        }
        write!(out,"<td><form id='rule{i}' method=post>{hidden}<input type=hidden name=row value='{i}'><button name=action value=rule>{}</button>",if adding{"Добавить"}else{"Сохранить"}).unwrap();
        if !adding {
            out.push_str("<button name=action value=remove>Удалить правило</button>")
        }
        out.push_str("</form></td></tr>");
    }
    out.push_str("</tbody></table><h2>Доступ к распределению</h2><p>Номера объектов указываются через точку с запятой.</p>");
    for &(k, title) in ACCESS {
        write!(out,"<form method=post>{hidden}<input type=hidden name=key value='{k}'><label>{title} <input size=60 name=value value='{}'></label><button type=button data-catalog='{}' aria-label='Выбрать из справочника'>…</button><button name=action value=access>Сохранить</button></form>",escape(attr(&r.access,k)),if k=="BankSod"{"bank"}else{"judges"}).unwrap()
    }
    Ok(None)
}
