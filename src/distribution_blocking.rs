//! Native INNForJudges.xml editor. Changes retain other judges' entries.
use super::{escape, form, reply, Reply, MENU};
use crate::{
    ks::runtime::{
        xml_sax::{events, Event},
        Runtime,
    },
    server::RequestContext,
};
use std::{fmt::Write, path::Path, sync::Mutex};

#[derive(Default)]
struct Judge {
    attrs: Vec<(String, String)>,
    organizations: String,
}
impl Judge {
    fn get(&self, key: &str) -> &str {
        self.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map_or("", |(_, v)| v)
    }
    fn set(&mut self, key: &str, value: String) {
        if let Some((_, v)) = self.attrs.iter_mut().find(|(k, _)| k == key) {
            *v = value;
        } else {
            self.attrs.push((key.into(), value));
        }
    }
}
fn parse(bytes: &[u8]) -> Result<Vec<Judge>, String> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let mut judges = Vec::<Judge>::new();
    let mut depth = 0;
    for event in events(&crate::db::decode(bytes))? {
        match event {
            Event::Start(n, a) if depth == 0 && n == "JUDGES" && a.is_empty() => depth = 1,
            Event::Start(n, a) if depth == 1 && n == "JUDGE" => {
                let j = Judge {
                    attrs: a,
                    organizations: String::new(),
                };
                if j.get("oid")
                    .parse::<u32>()
                    .ok()
                    .filter(|n| *n > 0)
                    .is_none()
                    || judges.iter().any(|x| x.get("oid") == j.get("oid"))
                {
                    return Err("Повторный или неверный номер судьи в XML".into());
                }
                judges.push(j);
                depth = 2;
            }
            Event::Text(t) if depth == 2 => judges.last_mut().unwrap().organizations.push_str(&t),
            Event::Text(t) if t.trim().is_empty() => {}
            Event::End(_) => depth -= 1,
            _ => return Err("Неизвестный формат INNForJudges.xml".into()),
        }
    }
    Ok(judges)
}
fn encode(judges: &[Judge]) -> Result<Vec<u8>, String> {
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"windows-1251\"?>\r\n<JUDGES>\r\n");
    for j in judges {
        xml.push_str("<JUDGE");
        for (k, v) in &j.attrs {
            write!(xml, " {k}=\"{}\"", escape(v)).unwrap();
        }
        write!(xml, ">{}</JUDGE>\r\n", escape(&j.organizations)).unwrap();
    }
    xml.push_str("</JUDGES>\r\n");
    let (bytes, _, errors) = encoding_rs::WINDOWS_1251.encode(&xml);
    if errors {
        return Err("Символы не представимы в кодировке базы".into());
    }
    Ok(bytes.into_owned())
}
fn version(bytes: &[u8]) -> String {
    format!("{}-{:08x}", bytes.len(), crc32fast::hash(bytes))
}
fn values(req: &RequestContext, key: &str, inn: bool) -> Result<String, String> {
    let mut out = Vec::<String>::new();
    for (_, value) in req.form.iter().filter(|(k, _)| k == key) {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if value.contains("##")
            || value.chars().any(char::is_control)
            || (inn && !value.bytes().all(|c| c.is_ascii_digit()))
        {
            return Err("Некорректное значение ИНН или наименования".into());
        }
        if !out.iter().any(|v| v == value) {
            out.push(value.into());
        }
    }
    Ok(out.join("##"))
}
pub(super) fn page(
    root: &Path,
    runtime: &Runtime,
    req: &RequestContext,
    token: &str,
) -> Result<Reply, String> {
    // Serialize read/compare/replace across KS workers, including stale form checks.
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK
        .lock()
        .map_err(|_| "Редактор блокирования недоступен")?;
    let path = root.join("INNForJudges.xml");
    if path.exists()
        && path.canonicalize().map_err(|e| e.to_string())?.parent()
            != Some(root.canonicalize().map_err(|e| e.to_string())?.as_path())
    {
        return Err("Файл настроек находится вне корня базы".into());
    }
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.to_string()),
    };
    let mut judges = parse(&bytes)?;
    let choices = runtime
        .distribution_choices_where(7345, "WHERE \"Раздел\" IS NOT NULL AND \"Тип\"=739950069")?;
    let oid = form(req, "JudgeOid")
        .or_else(|| {
            req.target
                .split('&')
                .find_map(|p| p.strip_prefix("JudgeOid="))
        })
        .unwrap_or("");
    let name = choices
        .lines()
        .filter_map(|s| s.split_once('\t'))
        .find(|(id, _)| *id == oid)
        .map(|(_, s)| s);
    if req.method == "POST" {
        if form(req, "version") != Some(version(&bytes).as_str()) {
            return Ok(reply(
                409,
                "Настройки изменены другим пользователем. Откройте страницу заново.".into(),
            ));
        }
        let Some(name) = name else {
            return Ok(reply(400, "Выберите судью из справочника".into()));
        };
        let inn = match values(req, "nm__judge_inn", true) {
            Ok(v) => v,
            Err(e) => return Ok(reply(400, escape(&e))),
        };
        let org = match values(req, "nm__judge_orgname", false) {
            Ok(v) => v,
            Err(e) => return Ok(reply(400, escape(&e))),
        };
        let index = judges
            .iter()
            .position(|j| j.get("oid") == oid)
            .unwrap_or_else(|| {
                judges.push(Judge::default());
                judges.len() - 1
            });
        let j = &mut judges[index];
        j.set("oid", oid.into());
        j.set("name", name.into());
        j.set("inn", inn);
        j.organizations = org;
        if j.get("inn").is_empty() && j.organizations.is_empty() {
            judges.remove(index);
        }
        let next = match encode(&judges) {
            Ok(v) => v,
            Err(e) => return Ok(reply(400, escape(&e))),
        };
        if !bytes.is_empty() {
            crate::settings::atomic_write(&path.with_extension("xml.bak"), &bytes)
                .map_err(|e| e.to_string())?;
        }
        crate::settings::atomic_write(&path, &next).map_err(|e| e.to_string())?;
        return Ok((
            Vec::new(),
            303,
            vec![(
                "Location".into(),
                format!("/20a?RaspredSettingsBlockedInnForJudges&JudgeOid={oid}&saved=1"),
            )],
        ));
    }
    let mut html=String::from("<!doctype html><html lang=ru><head><meta charset=windows-1251><title>Настройки блокирования распределения заявлений судьям</title><link rel=stylesheet href='/20a?mp_css'><style>body{font:16px Arial;margin:1.5em}input,select,button{font:inherit}.distribution-menu{display:flex;flex-wrap:wrap;gap:1em;margin:1em 0}.entry{display:block;margin:.4em 0}.entry input{width:min(35em,75vw)}</style></head><body><a href='/20a?raspred_settings'>Настройки распределения</a><h1>Настройки блокирования распределения заявлений судьям</h1>");
    if req.target.split('&').any(|s| s == "saved=1") {
        html.push_str("<p role=status>Настройки сохранены.</p>");
    }
    html.push_str("<label>Выберите судью для редактирования: <select id=judge><option value=''>Выберите судью</option>");
    for (id, n) in choices.lines().filter_map(|s| s.split_once('\t')) {
        write!(
            html,
            "<option value='{id}' {}>{}</option>",
            if id == oid { "selected" } else { "" },
            escape(n)
        )
        .unwrap();
    }
    html.push_str("</select></label>");
    if name.is_some() {
        let empty = Judge::default();
        let j = judges
            .iter()
            .find(|j| j.get("oid") == oid)
            .unwrap_or(&empty);
        write!(html,"<form method=post><input type=hidden name=token value='{token}'><input type=hidden name=JudgeOid value='{oid}'><input type=hidden name=version value='{}'>",version(&bytes)).unwrap();
        for (id, label, key, value) in [
            (
                "inn",
                "Блокируемые ИНН участников спора в деле",
                "nm__judge_inn",
                j.get("inn"),
            ),
            (
                "org",
                "Блокируемые наименования участников спора в деле",
                "nm__judge_orgname",
                j.organizations.as_str(),
            ),
        ] {
            write!(html, "<h2>{label}</h2><div id='{id}'>").unwrap();
            for v in value.split("##") {
                write!(html,"<label class=entry><input name='{key}' aria-label='{label}' value='{}'><button type=button onclick='this.parentElement.remove()' aria-label='Убрать строку'>×</button></label>",escape(v)).unwrap();
            }
            write!(
                html,
                "</div><button type=button data-add='{id}'>Добавить {}</button>",
                if id == "inn" {
                    "ИНН"
                } else {
                    "наименование"
                }
            )
            .unwrap();
        }
        html.push_str("<p>ИНН сравнивается целиком. В наименованиях регистр и пробелы не учитываются; проверяется вхождение строки длиной от пяти символов.</p><button>Сохранить настройки для судьи</button></form>");
    }
    html.push_str(MENU);
    html.push_str("<script>document.getElementById('judge').onchange=function(){location.href='/20a?RaspredSettingsBlockedInnForJudges&JudgeOid='+encodeURIComponent(this.value)};document.querySelectorAll('[data-add]').forEach(b=>{const box=document.getElementById(b.dataset.add),template=box.firstElementChild.cloneNode(true);b.onclick=()=>{const row=template.cloneNode(true);row.querySelector('input').value='';box.append(row);row.querySelector('input').focus()}});</script></body></html>");
    Ok(reply(200, html))
}
