use super::{message, RequestContext, Response};
use crate::db::{writer::counters, Db4Engine};
use std::sync::OnceLock;

fn token() -> Result<&'static str, String> {
    static TOKEN: OnceLock<Result<String, String>> = OnceLock::new();
    TOKEN
        .get_or_init(|| crate::settings::nonce().map_err(|e| e.to_string()))
        .as_deref()
        .map_err(Clone::clone)
}

pub(crate) fn controls(html: String) -> Result<String, String> {
    let field = |name| {
        let pattern = format!("name=\"{name}\" value=\"");
        let (_, tail) = html.split_once(&pattern)?;
        let (value, _) = tail.split_once('"')?;
        value.parse::<u32>().ok()
    };
    let (Some(case), Some(incoming)) = (field("regdoc_numberdelo"), field("regdoc_numbervhod"))
    else {
        return Ok(html);
    };
    let start = html.find("name=\"regdoc_numbervhod\"").unwrap();
    let end = start
        + html[start..]
            .find('>')
            .ok_or("incomplete registration field")?
        + 1;
    let form_end = end
        + html[end..]
            .find("</form>")
            .ok_or("registration form missing its end")?
        + "</form>".len();
    // The all-settings page includes hundreds of unrelated and repeated
    // controls. Submit just the two edited numbers and their original values.
    let form = format!(
        "<form id='codex_numbers' method='post' action='/20a?saveRegistrationNumbers'><input type=hidden name=regdoc_numberdelo><input type=hidden name=regdoc_numbervhod><input type=hidden name=codex_previous_case value='{case}'><input type=hidden name=codex_previous_incoming value='{incoming}'><input type=hidden name=codex_counter_token value='{}'></form>",
        token()?
    );
    let button = r#"<button type="button" onclick="var f=document.getElementById('codex_numbers');f.regdoc_numberdelo.value=this.form.regdoc_numberdelo.value;f.regdoc_numbervhod.value=this.form.regdoc_numbervhod.value;f.submit()">Сохранить только номера</button>"#;
    let mut html = html;
    html.insert_str(form_end, &form);
    html.insert_str(end, button);
    Ok(html)
}

pub(super) fn save(
    engine: &Db4Engine,
    request: &RequestContext,
    authorization: Option<&str>,
) -> Response {
    if request.method != "POST" {
        return message("405 Method Not Allowed", "Требуется POST");
    }
    let Some(admin) = engine.resolve_virtual_dir("/admin/") else {
        return message("403 Forbidden", "Отсутствует каталог администратора");
    };
    if !matches!(
        crate::auth::check(engine, admin, authorization),
        Ok(crate::auth::Access::Authenticated(_))
    ) {
        return message("403 Forbidden", "Требуются права администратора");
    }
    let get = |name| {
        let mut found = request.form.iter().filter(|(k, _)| k == name);
        let (_, v) = found.next()?;
        found.next().is_none().then_some(v.as_str())
    };
    if !token()
        .ok()
        .is_some_and(|t| Some(t) == get("codex_counter_token"))
    {
        return message("403 Forbidden", "Откройте форму настроек заново");
    }
    let number = |name| {
        get(name)
            .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|v| v.parse::<u32>().ok())
    };
    let (Some(case), Some(incoming), Some(old_case), Some(old_incoming)) = (
        number("regdoc_numberdelo"),
        number("regdoc_numbervhod"),
        number("codex_previous_case"),
        number("codex_previous_incoming"),
    ) else {
        return message("400 Bad Request", "Оба номера должны быть целыми числами");
    };
    if [case, incoming]
        .iter()
        .any(|v| !(1..=i32::MAX as u32).contains(v))
    {
        return message("400 Bad Request", "Номер должен быть от 1 до 2147483647");
    }
    let result = (|| {
        let registry = &engine.registry;
        let oid = |name| {
            registry
                .query(registry.root, name)?
                .map(|entry| entry.oid)
                .ok_or_else(|| format!("Отсутствует счётчик {name}"))
        };
        let ids = [oid(counters::NAMES[0])?, oid(counters::NAMES[1])?];
        let path = &engine.volumes.first().ok_or("config.db4 отсутствует")?.path;
        counters::save(
            path,
            registry.root,
            ids,
            [old_case, old_incoming],
            [case, incoming],
        )
    })();
    match result {
        Ok(()) => Response {
            status: "303 See Other",
            mime: "text/plain",
            body: Vec::new(),
            location: Some("/20a?adminsettings&mode=1&id=4".into()),
            headers: Vec::new(),
        },
        Err(e) => message("409 Conflict", e),
    }
}
