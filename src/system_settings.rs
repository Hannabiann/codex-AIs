//! Native adminsettings forms, authorization and optimistic config revision.
use crate::{db::Db4Engine, ks::runtime::Runtime, server::RequestContext};
use std::sync::OnceLock;
fn token() -> Result<&'static str, String> {
    static TOKEN: OnceLock<Result<String, String>> = OnceLock::new();
    TOKEN
        .get_or_init(|| crate::settings::nonce().map_err(|e| e.to_string()))
        .as_deref()
        .map_err(Clone::clone)
}
type Reply = (Vec<u8>, u16, Vec<(String, String)>);
fn reply(code: u16, s: &str) -> Reply {
    (
        encoding_rs::WINDOWS_1251.encode(s).0.into_owned(),
        code,
        vec![("Cache-Control".into(), "no-store".into())],
    )
}
pub fn save_error(error: &str) -> Reply {
    reply(
        if error == "Настройки изменились. Откройте форму заново."
        {
            409
        } else {
            500
        },
        error,
    )
}
pub fn guard(
    engine: &Db4Engine,
    request: &RequestContext,
    runtime: &mut Runtime,
) -> Result<Option<Reply>, String> {
    let field = |name: &str| {
        request
            .form
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    if request.method != "POST" {
        for (name, value) in request.target.split('&').filter_map(|p| p.split_once('=')) {
            if crate::server::percent_decode(name)? == "mode"
                && crate::server::percent_decode(value)? == "2"
            {
                return Ok(Some(reply(405, "Для сохранения требуется POST")));
            }
        }
        return Ok(None);
    }
    if field("mode") != Some("2") {
        return Ok(None);
    }
    let admin = engine
        .resolve_virtual_dir("/admin/")
        .ok_or("Каталог администратора отсутствует")?;
    if !matches!(
        crate::auth::check(engine, admin, request.authorization.as_deref())?,
        crate::auth::Access::Authenticated(_)
    ) {
        return Ok(Some(reply(403, "Требуются права администратора")));
    }
    if field("codex_settings_token") != Some(token()?) {
        return Ok(Some(reply(403, "Откройте форму настроек заново")));
    }
    for name in ["regdoc_numberdelo", "regdoc_numbervhod"] {
        if field(name).is_some_and(|v| {
            v.is_empty()
                || !v.bytes().all(|b| b.is_ascii_digit())
                || !v.parse::<i32>().is_ok_and(|n| n > 0)
        }) {
            return Ok(Some(reply(
                400,
                "Регистрационный номер должен быть от 1 до 2147483647",
            )));
        }
    }
    if let Err(e) = runtime.prepare_system_settings(field("codex_settings_revision").unwrap_or(""))
    {
        return Ok(Some(reply(409, &e)));
    }
    Ok(None)
}
pub fn decorate(revision: &str, body: Vec<u8>) -> Result<Vec<u8>, String> {
    let mut html = crate::db::decode(&body);
    let Some(start) = html.to_ascii_lowercase().find("<form ") else {
        return Ok(body);
    };
    let end = start
        + html[start..]
            .find('>')
            .ok_or("Незавершённая форма настроек")?
        + 1;
    html.insert_str(end,&format!("<input type=\"hidden\" name=\"codex_settings_token\" value=\"{}\"><input type=\"hidden\" name=\"codex_settings_revision\" value=\"{revision}\">",token()?));
    Ok(encoding_rs::WINDOWS_1251.encode(&html).0.into_owned())
}
