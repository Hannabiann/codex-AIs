//! Editor for the court distribution settings stored under the /20a path key.
use crate::{
    auth,
    db::{self, Db4Engine, VirtualDir},
    ks::runtime::Runtime,
    server::RequestContext,
};
use std::fmt::Write;
#[path = "distribution_rules.rs"]
mod rules;

#[derive(Clone, Copy)]
enum Kind {
    Flag,
    Count,
    Factor,
    Seconds,
    Oids,
    RegistryOid,
    Times,
    Choice(&'static [(&'static str, &'static str)]),
}
use Kind::*;
const FIELDS: &[(&str, &str, Kind)] = &[
    ("AutoRaspred", "Автоматическое распределение", Flag),
    ("KolRaspred", "Распределение по коллегиям", Flag),
    ("SostRaspred", "Распределение по составам", Flag),
    ("BokRaspred", "Распределение боковых судей", Flag),
    ("BankRaspred", "Распределение банкротных дел", Flag),
    ("ConfRaspred", "Распределение конфиденциальных дел", Flag),
    (
        "AddJudeToStat",
        "Добавлять судью в статистическую карту",
        Flag,
    ),
    ("ChangeJudeKP", "Изменять КП судей при распределении", Flag),
    ("showcheckjudes", "Показывать кнопку «Проверить»", Flag),
    ("showallraspred", "Два распределения в одном", Flag),
    (
        "Novye_Obstoyat",
        "Сохранять прежний состав по новым обстоятельствам",
        Flag,
    ),
    ("threejudraspred", "Распределение тройке судей", Flag),
    (
        "Raspred_AutoBok",
        "Автоматически распределять боковых судей",
        Flag,
    ),
    (
        "Raspred_AutoBok_Kategory",
        "Распределять боковых судей по категориям споров",
        Flag,
    ),
    ("Raspred_ShowDateAndPom", "Выводить помощника и дату", Flag),
    ("ShowDay", "За сколько дней показывать документы", Count),
    ("KPUKP", "Коэффициент плавного увеличения КП", Factor),
    ("RaznBal", "Коэффициент разности КП", Count),
    (
        "TTRobesp",
        "Обновление страницы распределения обеспечений, секунд",
        Seconds,
    ),
    (
        "Raspred_ShowPereraspedJude",
        "При перераспределении доступны судьи только своего состава",
        Flag,
    ),
    (
        "Raspred_ShowJudesFrom",
        "Показывать судей",
        Choice(&[
            ("0", "Всех"),
            ("1", "Своего состава"),
            ("2", "Своей коллегии"),
        ]),
    ),
    (
        "Raspred_P_Old_Jude",
        "Отражать текущего судью при перераспределении",
        Flag,
    ),
    (
        "Raspred_Full_Pereraspred",
        "Пропускать проверку последующих стадий при перераспределении",
        Flag,
    ),
    (
        "Raspred_Sort",
        "Сортировать документы по",
        Choice(&[
            ("1", "Номеру дела первой инстанции"),
            ("167", "Номеру апелляционной жалобы"),
            ("198", "Номеру кассационной жалобы"),
            ("79", "Входящему номеру"),
        ]),
    ),
    ("JudeCodFreeze", "Фиксировать код судьи", Flag),
    (
        "JudeCodFreezeSod",
        "Содержание документов для фиксации кода судьи (номера через ;)",
        Oids,
    ),
    (
        "ReestrJudeRaspred",
        "Реестр по судьям (номер объекта реестра)",
        RegistryOid,
    ),
    (
        "ReestrBankRaspred",
        "Реестр банкротных дел (номер объекта реестра)",
        RegistryOid,
    ),
    (
        "Raspred_TimeToNazn",
        "Время назначения (ЧЧ:ММ через ; или 0)",
        Times,
    ),
    ("ShowPrNumber", "Выводить номер производства", Flag),
    ("Raspred_ShowDeclarant", "Выводить заявителя", Flag),
    (
        "RaspredReadOnly",
        "Запретить менять результат автоматического распределения",
        Flag,
    ),
    (
        "PereraspredReadOnly",
        "Запретить менять результат автоматического перераспределения",
        Flag,
    ),
    ("Raspred_Random", "Случайность в распределении", Flag),
    (
        "RepeatDelKP",
        "Сбрасывать КП судей через указанное число дней",
        Count,
    ),
    (
        "Raspred_UseBlockInnForJudges",
        "Учитывать блокировку по ИНН",
        Flag,
    ),
];

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn normalize(kind: Kind, s: &str) -> Option<String> {
    match kind {
        Oids if s.is_empty()
            || s.len() <= 255
                && s.trim_end_matches(';').split(';').all(|n| {
                    n.parse::<u32>().is_ok_and(|n| n > 0) && n.bytes().all(|b| b.is_ascii_digit())
                }) =>
        {
            Some(s.into())
        }
        RegistryOid
            if s.is_empty()
                || s.bytes().all(|b| b.is_ascii_digit()) && s.parse::<u32>().is_ok() =>
        {
            Some(s.into())
        }
        Times
            if s == "0"
                || s.is_empty()
                || s.len() <= 255
                    && s.split(';').all(|t| {
                        t.split_once(':').is_some_and(|(h, m)| {
                            (1..=2).contains(&h.len())
                                && m.len() == 2
                                && h.bytes().chain(m.bytes()).all(|c| c.is_ascii_digit())
                                && h.parse::<u8>().is_ok_and(|x| x < 24)
                                && m.parse::<u8>().is_ok_and(|x| x < 60)
                        })
                    }) =>
        {
            Some(s.into())
        }
        Flag if matches!(s, "0" | "1") => Some(s.into()),
        Choice(options) if options.iter().any(|(v, _)| *v == s) => Some(s.into()),
        Factor
            if !s.is_empty()
                && s.len() <= 24
                && s.bytes().all(|c| c.is_ascii_digit() || c == b'.')
                && s.parse::<f64>().is_ok_and(|v| v.is_finite()) =>
        {
            Some(s.into())
        }
        Count | Seconds if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => {
            let v = s
                .parse::<u32>()
                .ok()?
                .checked_mul(if matches!(kind, Seconds) { 1000 } else { 1 })?;
            (v <= i32::MAX as u32).then(|| v.to_string())
        }
        _ => None,
    }
}
fn branch(engine: &Db4Engine, dir: &VirtualDir) -> Result<u32, String> {
    let r = &engine.registry;
    let mut found = None;
    for &id in &r.key(r.root)?.children {
        if r.query(id, "Path")?.is_some_and(|e| {
            e.value
                .trim_end_matches('/')
                .eq_ignore_ascii_case(dir.path.trim_end_matches('/'))
        }) {
            if found.replace(id).is_some() {
                return Err("Неоднозначный каталог распределения".into());
            }
        }
    }
    found.ok_or_else(|| "Каталог распределения отсутствует в config.db4".into())
}
type Reply = (Vec<u8>, u16, Vec<(String, String)>);
pub(super) fn refresh(
    engine: &Db4Engine,
    dir: &VirtualDir,
    runtime: &mut Runtime,
) -> Result<(), String> {
    let Some(config) = engine.volumes.first() else {
        return Ok(());
    };
    let generation = db::writer::generation(&config.path);
    if generation == runtime.distribution_generation {
        return Ok(());
    }
    let key = branch(engine, dir)?;
    for &(name, _, _) in FIELDS {
        let Some(e) = engine.registry.query(key, name)? else {
            continue;
        };
        if let Some(record) = engine.read_oid(e.oid, "config.db4")? {
            if let Some(v) = db::registry::string_value(&record) {
                runtime.update_directory_setting(&e.name, &v, e.kind == "REG_DWORD")?
            }
        }
    }
    runtime.distribution_generation = generation;
    Ok(())
}
fn reply(code: u16, html: String) -> Reply {
    let bytes = encoding_rs::WINDOWS_1251.encode(&html).0.into_owned();
    (
        bytes,
        code,
        vec![("Cache-Control".into(), "no-store".into())],
    )
}
fn form<'a>(request: &'a RequestContext, name: &str) -> Option<&'a str> {
    let mut it = request.form.iter().filter(|(k, _)| k == name);
    let v = it.next()?.1.as_str();
    it.next().is_none().then_some(v)
}
pub(super) fn page(
    engine: &Db4Engine,
    dir: &VirtualDir,
    runtime: &mut Runtime,
    request: &RequestContext,
) -> Result<Reply, String> {
    if !matches!(request.method.as_str(), "GET" | "POST" | "HEAD") {
        return Ok(reply(405, "Требуется GET или POST".into()));
    }
    let admin = engine
        .resolve_virtual_dir("/admin/")
        .ok_or("Каталог администратора отсутствует")?;
    let auth::Access::Authenticated(user) =
        auth::check(engine, admin, request.authorization.as_deref())?
    else {
        return Ok(reply(403, "Требуются права администратора".into()));
    };
    let ast = runtime.parse_cached(
        r#"mp_loadLibs("libassign");
        if (!mp_CMode_isAdmin()) return;
        if (la__isPassRequired() && (CheckSession == null || !la_isSecureAccessGranted())) {
            la_passRequest("raspred_settings",inst); return;
        }
        {/CODEX_DISTRIBUTION_GRANTED/}"#,
    )?;
    let guard = runtime.render(&ast)?;
    if guard.trim() != "CODEX_DISTRIBUTION_GRANTED" {
        return Ok(reply(if guard.is_empty() { 403 } else { 200 }, guard));
    }
    if let Some(catalog) = request
        .target
        .split('&')
        .find_map(|p| p.strip_prefix("catalog="))
    {
        if request.method != "GET" {
            return Ok(reply(405, "Требуется GET".into()));
        }
        let id = match catalog {
            "judges" => 7345,
            "bank" => 7348,
            "content" => 7347,
            _ => return Ok(reply(400, "Неизвестный справочник".into())),
        };
        let mut html = String::from("<select multiple>");
        for row in runtime.distribution_choices(id)?.lines() {
            if let Some((oid, name)) = row.split_once('\t') {
                write!(html, "<option value='{oid}'>{}</option>", escape(name)).unwrap()
            }
        }
        html.push_str("</select>");
        return Ok(reply(200, html));
    }
    let key = branch(engine, dir)?;
    let path = &engine.volumes.first().ok_or("Отсутствует config.db4")?.path;
    static TOKENS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<u32, String>>> =
        std::sync::OnceLock::new();
    let token = {
        let mut map = TOKENS
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| "Таблица сеансов недоступна")?;
        if let std::collections::hash_map::Entry::Vacant(slot) = map.entry(user.oid) {
            slot.insert(crate::settings::nonce().map_err(|e| e.to_string())?);
        }
        map[&user.oid].clone()
    };
    if request.method == "POST" && form(request, "token") != Some(token.as_str()) {
        return Ok(reply(403, "Откройте форму заново".into()));
    }
    if request.method == "POST" && form(request, "section") != Some("rules") {
        let Some(&(name, _, kind)) = FIELDS
            .iter()
            .find(|(k, _, _)| Some(*k) == form(request, "setting"))
        else {
            return Ok(reply(400, "Неизвестный параметр".into()));
        };
        let Some(entry) = engine.registry.query(key, name)? else {
            return Ok(reply(
                409,
                "Параметр отсутствует в текущей конфигурации".into(),
            ));
        };
        let Some(value) = form(request, "value").and_then(|v| normalize(kind, v)) else {
            return Ok(reply(400, "Недопустимое значение".into()));
        };
        let Some(previous) = form(request, "previous") else {
            return Ok(reply(400, "Отсутствует исходное значение".into()));
        };
        if let Err(e) = db::writer::counters::save_values(
            path,
            key,
            &[(entry.oid, name, previous.into(), value)],
        ) {
            return Ok(reply(409, escape(&e)));
        }
    }
    let mut html=String::from("<!doctype html><html lang=ru><head><meta charset=windows-1251><title>Настройки распределения дел</title><style>body{font:16px Arial,sans-serif;max-width:68rem;margin:2rem auto;padding:0 1rem}table{border-collapse:collapse;width:100%}th,td{padding:.4rem;border-bottom:1px solid #aaa;text-align:left}input,select,button{font:inherit}button{margin-left:.5rem}</style></head><body><a href='/20a?adminsettings'>Общие настройки системы</a><h1>Настройки распределения дел</h1>");
    if request.method == "POST" {
        html.push_str("<p role=status>Значение сохранено.</p>");
    }
    html.push_str("<table><tr><th>Параметр</th><th>Значение</th></tr>");
    for &(name, label, kind) in FIELDS {
        let Some(entry) = engine.registry.query(key, name)? else {
            continue;
        };
        let rec = engine
            .read_oid(entry.oid, "config.db4")?
            .ok_or("Параметр отсутствует в томе")?;
        let value = db::registry::string_value(&rec).ok_or("Параметр без значения")?;
        write!(html,"<tr><td><label for='{name}'>{label}</label></td><td><form method='post'><input type=hidden name=token value='{token}'><input type=hidden name=setting value='{name}'><input type=hidden name=previous value='{}'>",escape(&value)).unwrap();
        let options = match kind {
            Flag => Some(&[("1", "Да"), ("0", "Нет")][..]),
            Choice(opts) => Some(opts),
            _ => None,
        };
        if let Some(options) = options {
            write!(html, "<select id='{name}' name=value>").unwrap();
            for &(v, title) in options {
                write!(
                    html,
                    "<option value='{v}' {}>{title}</option>",
                    if v == value { "selected" } else { "" }
                )
                .unwrap()
            }
            html.push_str("</select>");
        } else {
            let shown = if matches!(kind, Seconds) {
                value
                    .parse::<u32>()
                    .map(|n| (n / 1000).to_string())
                    .unwrap_or_else(|_| value.clone())
            } else {
                value.clone()
            };
            write!(
                html,
                "<input id='{name}' name=value size=15 value='{}'>",
                escape(&shown)
            )
            .unwrap();
        }
        if name == "JudeCodFreezeSod" {
            html.push_str("<button type=button data-catalog=content aria-label='Выбрать содержание'>…</button>")
        }
        html.push_str("<button>Сохранить</button></form></td></tr>");
    }
    html.push_str("</table>");
    if let Some(doc) = runtime.distribution_document()? {
        if let Some(result) = rules::section(&mut html, runtime, &doc, request, &token)? {
            return Ok(result);
        }
    } else if request.method == "POST" && form(request, "section") == Some("rules") {
        return Ok(reply(409, "Служебный документ правил отсутствует".into()));
    }
    html.push_str(include_str!("distribution_picker.html"));
    html.push_str("</body></html>");
    Ok(reply(200, html))
}
