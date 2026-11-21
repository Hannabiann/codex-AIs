//! Editor for the court distribution settings stored under the /20a path key.
use crate::{
    auth,
    db::{self, Db4Engine, VirtualDir},
    ks::runtime::Runtime,
    server::RequestContext,
};
#[path = "distribution_blocking.rs"]
mod blocking;

const MENU: &str = "<nav aria-label='Распределение судебных дел' class='distribution-menu'>\
<a class='mp_button' href='/20a?change_difficult_katsp'>Изменение сложности</a>\
<a class='mp_button' href='/20a?raspredkategory'>Установка категорий</a>\
<a class='mp_button' href='/20a?RaspredSettingsBlockedInnForJudges'>Настройка блокирования</a>\
<a class='mp_button' href='/20a?restorekp'>Восстановить КП судей</a></nav>";

const SETTING_NAMES: &[&str] = &[
    "AutoRaspred",
    "KolRaspred",
    "SostRaspred",
    "BokRaspred",
    "BankRaspred",
    "ConfRaspred",
    "AddJudeToStat",
    "ChangeJudeKP",
    "showcheckjudes",
    "showallraspred",
    "Novye_Obstoyat",
    "threejudraspred",
    "Raspred_AutoBok",
    "Raspred_AutoBok_Kategory",
    "Raspred_ShowDateAndPom",
    "ShowDay",
    "KPUKP",
    "RaznBal",
    "TTRobesp",
    "Raspred_ShowPereraspedJude",
    "Raspred_ShowJudesFrom",
    "Raspred_P_Old_Jude",
    "Raspred_Full_Pereraspred",
    "Raspred_Sort",
    "JudeCodFreeze",
    "JudeCodFreezeSod",
    "ReestrJudeRaspred",
    "ReestrKolRaspred",
    "ReestrSostRaspred",
    "ReestrBankRaspred",
    "Raspred_TimeToNazn",
    "ShowPrNumber",
    "Raspred_ShowDeclarant",
    "RaspredReadOnly",
    "PereraspredReadOnly",
    "Raspred_Random",
    "RepeatDelKP",
    "Raspred_UseBlockInnForJudges",
];

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
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
pub(crate) fn setting_names() -> Vec<&'static str> {
    SETTING_NAMES.to_vec()
}
fn compiled_token(request: &RequestContext) -> Result<String, String> {
    static TOKENS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<u32, String>>> =
        std::sync::OnceLock::new();
    let user = request.user.as_ref().ok_or("Требуется вход")?;
    let mut tokens = TOKENS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "Таблица сеансов недоступна")?;
    if let std::collections::hash_map::Entry::Vacant(e) = tokens.entry(user.oid) {
        e.insert(crate::settings::nonce().map_err(|e| e.to_string())?);
    }
    Ok(tokens[&user.oid].clone())
}
fn compiled_version(engine: &Db4Engine, runtime: &Runtime) -> Result<String, String> {
    let mut hash = crc32fast::Hasher::new();
    hash.update(
        &std::fs::read(&engine.volumes.first().ok_or("Нет конфигурации")?.path)
            .map_err(|e| e.to_string())?,
    );
    if let Some(r) = runtime.distribution_document()? {
        for f in r.fields.iter().filter(|f| f.id == 4) {
            hash.update(crate::document::bytes(f)?);
        }
    }
    Ok(format!("{:08x}", hash.finalize()))
}
pub(super) fn guard_compiled(
    engine: &Db4Engine,
    runtime: &Runtime,
    request: &RequestContext,
) -> Result<Option<Reply>, String> {
    if request.method == "POST" {
        if form(request, "codex_token") != Some(compiled_token(request)?.as_str()) {
            return Ok(Some(reply(403, "Откройте форму заново".into())));
        }
        if form(request, "codex_version") != Some(compiled_version(engine, runtime)?.as_str()) {
            return Ok(Some(reply(
                409,
                "Настройки изменились. Откройте форму заново.".into(),
            )));
        }
    }
    Ok(None)
}
pub(super) fn decorate_compiled(
    engine: &Db4Engine,
    runtime: &Runtime,
    request: &RequestContext,
    body: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let mut html = db::decode(&body);
    let start = html.find("<form ").ok_or("Штатная форма отсутствует")?;
    let end = start
        + html[start..]
            .find('>')
            .ok_or("Штатная форма не завершена")?
        + 1;
    html.insert_str(end,&format!("<input type=\"hidden\" name=\"codex_token\" value=\"{}\"><input type=\"hidden\" name=\"codex_version\" value=\"{}\">",compiled_token(request)?,compiled_version(engine,runtime)?));
    Ok(encoding_rs::WINDOWS_1251.encode(&html).0.into_owned())
}
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
    let entries = db::writer::settings::read(&config.path, key, &setting_names())?;
    for name in setting_names() {
        if !entries.iter().any(|e| e.name.eq_ignore_ascii_case(name)) {
            runtime.clear_directory_setting(name);
        }
    }
    for e in entries {
        runtime.update_directory_setting(&e.name, &e.value, e.kind == "REG_DWORD")?;
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
pub(super) fn guard_child(
    engine: &Db4Engine,
    request: &RequestContext,
) -> Result<Option<Reply>, String> {
    let admin = engine
        .resolve_virtual_dir("/admin/")
        .ok_or("Каталог администратора отсутствует")?;
    if !matches!(
        auth::check(engine, admin, request.authorization.as_deref())?,
        auth::Access::Authenticated(_)
    ) {
        return Ok(Some(reply(403, "Требуются права администратора".into())));
    }
    // The native child script loads libassign and checks its additional password.
    // Loading it here as well would register the same KS module twice.
    Ok(None)
}
pub(super) fn page(
    engine: &Db4Engine,
    _dir: &VirtualDir,
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
    let command = "RaspredSettingsBlockedInnForJudges";
    let ast = runtime.parse_cached(
        &r#"mp_loadLibs("libassign");
        if (!mp_CMode_isAdmin()) return;
        if (la__isPassRequired() && (CheckSession == null || !la_isSecureAccessGranted())) {
            la_passRequest("$COMMAND",inst); return;
        }
        {/CODEX_DISTRIBUTION_GRANTED/}"#
            .replace("$COMMAND", command),
    )?;
    let guard = runtime.render(&ast)?;
    if guard.trim() != "CODEX_DISTRIBUTION_GRANTED" {
        return Ok(reply(if guard.is_empty() { 403 } else { 200 }, guard));
    }
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
    blocking::page(
        path.parent().ok_or("Нет корня базы")?,
        runtime,
        request,
        &token,
    )
}
