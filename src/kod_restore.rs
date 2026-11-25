//! Administrator-only batch restore. Uploaded snapshots stay in bounded RAM;
//! the confirmation names one volume and one atomic DB4 publication.
use crate::{
    db::{
        fields::Record,
        writer::kod::{self, Conflict},
        Db4Engine,
    },
    ks::runtime::Runtime,
    server::RequestContext,
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
type Reply = (Vec<u8>, u16, Vec<(String, String)>);
#[path = "kod_choices.rs"]
mod choices;
#[path = "kod_links.rs"]
mod links;
const ROUTE: &str = "/20a?ispravlenieForm&codex_restore=1";
const MAX_PENDING: usize = 128 * 1024 * 1024;
struct Row {
    filename: String,
    record: Record,
    state: Conflict,
    visible: bool,
}
struct Plan {
    owner: (u32, String),
    volume: String,
    path: PathBuf,
    rows: Vec<Row>,
    links: links::Links,
    choices: choices::Choices,
    selected: Vec<bool>,
    replace: bool,
    notice: String,
    receipt: Option<String>,
}
struct Entry {
    born: Instant,
    bytes: usize,
    plan: Arc<Mutex<Plan>>,
}
fn plans() -> &'static Mutex<HashMap<String, Entry>> {
    static PLANS: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    PLANS.get_or_init(Default::default)
}
fn token() -> Result<&'static str, String> {
    static TOKEN: OnceLock<Result<String, String>> = OnceLock::new();
    TOKEN
        .get_or_init(|| crate::settings::nonce().map_err(|e| e.to_string()))
        .as_deref()
        .map_err(Clone::clone)
}
pub(crate) fn is_route(target: &str) -> bool {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    matches!(path, "/20a" | "/20a/")
        && query.split('&').next() == Some("ispravlenieForm")
        && query.split('&').any(|v| v == "codex_restore=1")
}
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn reply(code: u16, body: &str) -> Reply {
    let html = format!("<!doctype html><html lang=ru><meta charset=windows-1251><title>Восстановление KOD</title><style>body{{font:14px Arial,sans-serif;background:#f3f3f3;color:#222;margin:24px}}main{{max-width:1200px;margin:auto}}h1{{font-size:22px}}table{{border-collapse:collapse;width:100%;background:white}}td,th{{padding:8px;border:1px solid #bbb;text-align:left;overflow-wrap:anywhere}}button,select,input{{font:inherit}}button{{padding:8px 16px;margin:12px 0}}.error{{color:#a21b13}}a{{color:#075da0}}</style><main><h1>Восстановление объектов из .kod</h1>{body}<p><a href='{ROUTE}'>Выбрать файлы заново</a> · <a href='/20a?ispravlenieForm&amp;bkod=1'>Штатная форма одного файла</a> · <a href='/20a?createNewObject'>Область P</a></p></main></html>");
    (
        encoding_rs::WINDOWS_1251.encode(&html).0.into_owned(),
        code,
        vec![("Cache-Control".into(), "no-store".into())],
    )
}
fn error(code: u16, message: &str) -> Reply {
    reply(code, &format!("<p class=error>{}</p>", esc(message)))
}
fn redirect(location: String) -> Reply {
    let mut response = reply(303, "");
    response.2.push(("Location".into(), location));
    response
}
fn find_plan(id: &str) -> Result<Option<Arc<Mutex<Plan>>>, String> {
    let mut store = crate::execution::lock(plans())?;
    store.retain(|_, p| p.born.elapsed() < Duration::from_secs(1800));
    Ok(store.get(id).map(|e| e.plan.clone()))
}
pub(crate) fn decorate(body: Vec<u8>) -> Vec<u8> {
    let mut html = crate::db::decode(&body);
    let at = html
        .to_ascii_lowercase()
        .rfind("</body>")
        .unwrap_or(html.len());
    html.insert_str(at, &format!("<p style='text-align:center'><a href='{ROUTE}'>Восстановить несколько файлов .kod — предварительный список</a></p>"));
    encoding_rs::WINDOWS_1251.encode(&html).0.into_owned()
}
fn field<'a>(request: &'a RequestContext, key: &str) -> Option<&'a str> {
    request
        .form
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}
fn conflicts(row: &Row) -> bool {
    row.visible || row.state.exists()
}
fn blocked(row: &Row) -> bool {
    row.state.names.len() > 1
        || row
            .state
            .current
            .as_ref()
            .is_some_and(|r| r.area != row.record.area)
}
fn render_plan(id: &str, plan: &Plan) -> String {
    let mut s = format!("<p>Том: <b>{}</b>. Просмотр ничего не записывает. Отметьте объекты для восстановления.</p><p>{}</p><form method=post action='{ROUTE}' onchange=\"document.getElementById('selection-note').hidden=false\"><input type=hidden name=token value='{}'><input type=hidden name=plan value='{}'>", esc(&plan.path.file_name().unwrap_or_default().to_string_lossy()), esc(&plan.notice), token().unwrap_or(""), esc(id));
    s.push_str(&plan.links.summary(&plan.rows, &plan.selected));
    s.push_str(&plan.links.replacements(id, &plan.rows, &plan.selected));
    s.push_str("<p>Файлы с одинаковым номером или именем, а также замены, удаляющие номер другого файла, не выбираются автоматически. Раскройте «Совпадения среди файлов» для сравнения. Выбор варианта снимает отметки с конкурирующих файлов.</p>");
    s.push_str("<p id=selection-note hidden>Выбор изменён. Перепроверьте связи перед восстановлением.</p><button name=action value=review>Проверить связи выбранных</button> <button name=action value=related>Добавить связанные файлы</button><p>Добавляются недостающие объекты из загруженных файлов, включая их связи. Замены автоматически не выбираются. Отсутствующие ссылки сохраняют свои номера; новые объекты вместо них не создаются.</p><table><tr><th>Выбрать</th><th>Файл</th><th>Объект / область</th><th>Наименование</th><th>Состояние в базе</th><th>Связанные объекты</th></tr>");
    for (i, r) in plan.rows.iter().enumerate() {
        let identical = r.state.identical(&r.record);
        let mut notes = Vec::new();
        if r.state.names.len() > 1 {
            notes.push("Несколько совпадений по имени: восстановление запрещено".into());
        }
        if r.state
            .current
            .as_ref()
            .is_some_and(|v| v.area != r.record.area)
        {
            notes.push("Этот номер занят объектом другой области: восстановление запрещено".into());
        }
        if identical {
            notes.push("Уже восстановлен, атрибуты совпадают".into());
        } else {
            if r.state.current.is_some() {
                notes.push("Замена существующего номера".into());
            }
            let replaced: Vec<_> = r
                .state
                .names
                .iter()
                .filter_map(|v| v.integer(1024))
                .filter(|id| Some(*id) != r.record.integer(1024))
                .map(|n| n.to_string())
                .collect();
            if !replaced.is_empty() {
                notes.push(format!("Замена по имени: {}", replaced.join(", ")));
            }
            if r.visible && r.state.current.is_none() {
                notes.push("Номер виден в другом подключённом томе".into());
            }
            if r.state.deleted {
                notes.push("В выбранном томе есть отметка удаления".into());
            }
            if notes.is_empty() {
                notes.push("Будет восстановлен".into());
            }
        }
        s.push_str(&format!("<tr><td><input type=checkbox name=pick{i} value=1 {} {} {} aria-label='Выбрать файл {}'></td><td>№ {}: {}{}</td><td>{} / {}</td><td>{}</td><td>{}{}</td><td>{}</td></tr>", if blocked(r) || identical { "disabled" } else { "" }, if plan.selected[i] { "checked" } else { "" }, plan.choices.checkbox(i), i+1, i+1, esc(&r.filename), plan.choices.row_html(id,i,&plan.rows), r.record.integer(1024).unwrap(), r.record.area, esc(&r.record.string(1).map(crate::db::decode).unwrap_or_default()), esc(&notes.join(". ")), choices::database_links(id,i,r), plan.links.row_html(id, i, &plan.rows, &plan.selected)));
    }
    s.push_str(&format!("</table><p><label><input type=checkbox name=replace value=1 {}> Разрешить замену выбранных существующих объектов по номеру или имени</label></p><p>Выбранные объекты записываются вместе в один том. Совпадающие записи пропускаются. После изменения базы потребуется повторный просмотр. Список хранится 30 минут, до перезапуска сервера.</p><button type=submit name=action value=commit>Восстановить выбранные объекты</button></form>", if plan.replace { "checked" } else { "" }));
    s.push_str(&format!("<form method=post action='{ROUTE}'><input type=hidden name=action value=cancel><input type=hidden name=token value='{}'><input type=hidden name=plan value='{}'><button type=submit>Отменить список</button></form>", token().unwrap_or(""), esc(id)));
    s
}
pub(crate) fn page(
    engine: &Db4Engine,
    runtime: &mut Runtime,
    request: &RequestContext,
) -> Result<Reply, String> {
    let admin = engine
        .resolve_virtual_dir("/admin/")
        .ok_or("Каталог администратора отсутствует")?;
    let crate::auth::Access::Authenticated(user) =
        crate::auth::check(engine, admin, request.authorization.as_deref())?
    else {
        return Ok(error(
            403,
            "Пакетное восстановление доступно администратору",
        ));
    };
    let owner = (user.oid, user.login.to_lowercase());
    if matches!(request.method.as_str(), "GET" | "HEAD") {
        let mut ids = request
            .target
            .split('&')
            .filter_map(|part| part.strip_prefix("plan="));
        if let Some(id) = ids.next() {
            if ids.next().is_some() {
                return Ok(error(400, "Укажите один предварительный список"));
            }
            let Some(arc) = find_plan(id)? else {
                return Ok(error(
                    409,
                    "Список истёк или сервер перезапущен. Выберите файлы заново.",
                ));
            };
            let plan = crate::execution::lock(&arc)?;
            if plan.owner != owner {
                return Ok(error(403, "Список принадлежит другому пользователю"));
            }
            let query_number = |key: &str| -> Result<Option<usize>, String> {
                let prefix = format!("{key}=");
                let mut parts = request
                    .target
                    .split('&')
                    .filter_map(|p| p.strip_prefix(&prefix));
                let value = parts
                    .next()
                    .map(str::parse::<usize>)
                    .transpose()
                    .map_err(|_| "Неверный номер объекта или страницы".to_owned())?;
                if parts.next().is_some() {
                    return Err("Повтор параметра просмотра".into());
                }
                Ok(value)
            };
            let dbrow = match query_number("dbrow") {
                Ok(v) => v,
                Err(e) => return Ok(error(400, &e)),
            };
            let replaced = match query_number("replaced") {
                Ok(v) => v,
                Err(e) => return Ok(error(400, &e)),
            };
            if plan.receipt.is_none() {
                if let Some(index) = dbrow {
                    let oid = match query_number("dboid") {
                        Ok(Some(n)) => u32::try_from(n).ok(),
                        _ => None,
                    };
                    let Some(oid) = oid else {
                        return Ok(error(400, "Укажите номер записи для сравнения"));
                    };
                    let page = match query_number("diffpage") {
                        Ok(n) => n.unwrap_or(0),
                        Err(e) => return Ok(error(400, &e)),
                    };
                    return Ok(match choices::database_compare(id,index,oid,page,&plan.rows) {
                        Some(html)=>reply(200,&html),None=>error(400,"Запись отсутствует в предварительном списке или страница не найдена"),
                    });
                }
                if let Some(oid) = replaced {
                    let Ok(oid) = u32::try_from(oid) else {
                        return Ok(error(400, "Неверный номер заменяемого объекта"));
                    };
                    let page = match query_number("impactpage") {
                        Ok(n) => n.unwrap_or(0),
                        Err(e) => return Ok(error(400, &e)),
                    };
                    return Ok(
                        match plan
                            .links
                            .replacement_page(id, oid, page, &plan.rows, &plan.selected)
                        {
                            Some(html) => reply(200, &html),
                            None => error(400, "Замена не выбрана или страница отсутствует"),
                        },
                    );
                }
            }
            let row = match query_number("linkrow") {
                Ok(v) => v,
                Err(e) => return Ok(error(400, &e)),
            };
            let comparison = match query_number("compare") {
                Ok(v) => v,
                Err(e) => return Ok(error(400, &e)),
            };
            if let Some(a) = comparison.filter(|_| plan.receipt.is_none()) {
                let b = match query_number("with") {
                    Ok(Some(v)) => v,
                    _ => return Ok(error(400, "Укажите второй файл для сравнения")),
                };
                let page = match query_number("diffpage") {
                    Ok(v) => v.unwrap_or(0),
                    Err(e) => return Ok(error(400, &e)),
                };
                return Ok(match plan.choices.compare(id, a, b, page, &plan.rows) {
                    Some(html) => reply(200, &html),
                    None => error(
                        400,
                        "Файлы не конкурируют друг с другом или страница отсутствует",
                    ),
                });
            }
            if let Some(row) = row {
                let page = match query_number("linkpage") {
                    Ok(v) => v.unwrap_or(0),
                    Err(e) => return Ok(error(400, &e)),
                };
                if plan.receipt.is_none() {
                    return Ok(
                        match plan
                            .links
                            .detail_page(id, row, page, &plan.rows, &plan.selected)
                        {
                            Some(html) => reply(200, &html),
                            None => error(400, "Страница связей отсутствует"),
                        },
                    );
                }
            }
            return Ok(reply(
                200,
                &plan
                    .receipt
                    .clone()
                    .unwrap_or_else(|| render_plan(id, &plan)),
            ));
        }
        let mut opts = String::from("<option value=''>Выберите том для записи</option>");
        for volume in runtime.kod_volumes()? {
            opts.push_str(&format!(
                "<option value='{}'>{} — {}</option>",
                esc(&volume.name),
                esc(&volume.title),
                esc(&volume.name)
            ));
        }
        return Ok(reply(200, &format!("<p>Выберите сохранённые при удалении файлы .kod и том для восстановления. Сначала будет показан список объектов и конфликтов.</p><form method=post enctype='multipart/form-data' action='{ROUTE}'><input type=hidden name=action value=preview><input type=hidden name=token value='{}'><p><label>Файлы .kod <input type=file name=filekod accept='.kod' multiple required></label></p><p><label>Том <select name=volume required>{opts}</select></label></p><button type=submit>Проверить файлы</button></form><p>До 256 файлов, суммарный размер запроса — до 64 МиБ. Исполняемые KS, служебные области и области участников спора не импортируются этим режимом. Файлы внешнего хранилища, не включённые в KOD, восстанавливаются отдельно.</p>", token()?)));
    }
    if request.method != "POST" {
        return Ok(error(405, "Для восстановления требуется POST"));
    }
    if field(request, "token") != Some(token()?) {
        return Ok(error(403, "Откройте форму восстановления заново"));
    }
    match field(request, "action") {
        Some("preview") => {
            if request.uploads.is_empty() || request.uploads.len() > 256 {
                return Ok(error(400, "Выберите от 1 до 256 файлов KOD"));
            }
            let Some(volume) = field(request, "volume").filter(|s| !s.is_empty()) else {
                return Ok(error(400, "Выберите том для записи"));
            };
            let mut records = Vec::new();
            for file in &request.uploads {
                crate::execution::check()?;
                let record = match kod::decode_file(&file.data) {
                    Ok(r) => r,
                    Err(e) => {
                        return Ok(error(
                            400,
                            &format!("{}: {e}. Ничего не записано.", file.filename),
                        ))
                    }
                };
                records.push(record);
            }
            let path = match runtime.kod_target(volume, &records) {
                Ok(p) => p,
                Err(e) => return Ok(error(400, &e)),
            };
            let states = kod::preview(&path, &records)?;
            let mut rows = Vec::new();
            let mut bytes = 0usize;
            for ((record, state), file) in records.into_iter().zip(states).zip(&request.uploads) {
                bytes += crate::db::writer::encode_record(&record)?.len();
                for r in state.current.iter().chain(&state.names) {
                    bytes += crate::db::writer::encode_record(r)?.len();
                }
                let visible = runtime
                    .read_loaded_document(record.integer(1024).unwrap())?
                    .is_some();
                rows.push(Row {
                    filename: file.filename.clone(),
                    record,
                    state,
                    visible,
                });
            }
            let links = links::Links::read(&rows, |oid| runtime.kod_link_exists(oid))?;
            let choices = choices::Choices::read(&rows);
            bytes = bytes
                .saturating_add(links.memory())
                .saturating_add(choices.memory());
            let selected = rows
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    !choices.ambiguous(i)
                        && !blocked(r)
                        && !conflicts(r)
                        && !r.state.identical(&r.record)
                })
                .collect();
            let plan = Plan {
                owner,
                volume: volume.into(),
                path,
                rows,
                links,
                choices,
                selected,
                replace: false,
                notice: String::new(),
                receipt: None,
            };
            let id = crate::settings::nonce().map_err(|e| e.to_string())?;
            let location = format!("{ROUTE}&plan={id}");
            let mut store = crate::execution::lock(plans())?;
            store.retain(|_, p| p.born.elapsed() < Duration::from_secs(1800));
            if store.len() >= 16
                || store
                    .values()
                    .map(|p| p.bytes)
                    .sum::<usize>()
                    .saturating_add(bytes)
                    > MAX_PENDING
            {
                return Ok(error(503, "Память предварительных списков занята. Отмените ненужные списки или повторите позже."));
            }
            store.insert(
                id,
                Entry {
                    born: Instant::now(),
                    bytes,
                    plan: Arc::new(Mutex::new(plan)),
                },
            );
            Ok(redirect(location))
        }
        Some("commit" | "cancel" | "review" | "related") => {
            let id = field(request, "plan").unwrap_or("");
            let arc = find_plan(id)?;
            let Some(arc) = arc else {
                return Ok(error(
                    409,
                    "Список истёк или сервер перезапущен. Выберите файлы заново.",
                ));
            };
            let mut plan = crate::execution::lock(&arc)?;
            if plan.owner != owner {
                return Ok(error(403, "Список принадлежит другому пользователю"));
            }
            if field(request, "action") == Some("cancel") {
                drop(plan);
                crate::execution::lock(plans())?.remove(id);
                return Ok(redirect(ROUTE.into()));
            }
            if plan.receipt.is_some() {
                return Ok(redirect(format!("{ROUTE}&plan={id}")));
            }
            let old_issues = plan.links.issues(&plan.rows, &plan.selected);
            if plan
                .rows
                .iter()
                .enumerate()
                .any(|(i, r)| blocked(r) && field(request, &format!("pick{i}")) == Some("1"))
            {
                return Ok(error(
                    409,
                    "Выбран объект с неоднозначным совпадением или чужой областью",
                ));
            }
            plan.selected = plan
                .rows
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    field(request, &format!("pick{i}")) == Some("1")
                        && !blocked(r)
                        && !r.state.identical(&r.record)
                })
                .collect();
            plan.replace = field(request, "replace") == Some("1");
            if plan.choices.invalid(&plan.selected) {
                plan.notice = "Одновременно выбраны несовместимые файлы: одинаковый номер, имя или замена, удаляющая номер другого файла. Оставьте один вариант каждой конфликтующей пары. Отметки сохранены; ничего не записано.".into();
                return Ok(redirect(format!("{ROUTE}&plan={id}")));
            }
            plan.links = links::Links::read(&plan.rows, |oid| runtime.kod_link_exists(oid))?;
            if field(request, "action") == Some("related") {
                let mut selected = plan.selected.clone();
                let added = plan
                    .links
                    .add_related(&plan.rows, &mut selected, &plan.choices);
                plan.selected = selected;
                plan.notice =
                    format!("Добавлено связанных файлов: {added}. Изменения ещё не записаны.");
                return Ok(redirect(format!("{ROUTE}&plan={id}")));
            }
            if field(request, "action") == Some("review") {
                plan.notice =
                    "Связи выбранных объектов перепроверены. Изменения ещё не записаны.".into();
                return Ok(redirect(format!("{ROUTE}&plan={id}")));
            }
            let issues = plan.links.issues(&plan.rows, &plan.selected);
            if !issues.is_subset(&old_issues) {
                plan.notice = "Обнаружены новые отсутствующие или исключённые связанные объекты. Проверьте список. Можно добавить связанные файлы либо повторно подтвердить восстановление с оставшимися ссылками. Ничего не записано.".into();
                return Ok(redirect(format!("{ROUTE}&plan={id}")));
            }
            let mut records = Vec::new();
            let mut expected = Vec::new();
            let replace = field(request, "replace") == Some("1");
            for (i, r) in plan.rows.iter().enumerate() {
                if !plan.selected[i] {
                    continue;
                }
                if blocked(r) {
                    return Ok(error(
                        409,
                        "Выбран объект с неоднозначным совпадением или чужой областью",
                    ));
                }
                if r.state.identical(&r.record) {
                    continue;
                }
                if conflicts(r) && !replace {
                    return Ok(error(
                        409,
                        "Разрешите замену выбранных существующих объектов или снимите их отметки",
                    ));
                }
                records.push(r.record.clone());
                expected.push(r.state.clone());
            }
            if records.is_empty() {
                return Ok(error(400, "Не выбраны объекты для восстановления"));
            }
            let path = match runtime.kod_target(&plan.volume, &records) {
                Ok(p) => p,
                Err(e) => return Ok(error(409, &e)),
            };
            if path != plan.path {
                return Ok(error(
                    409,
                    "Изменился выбранный том. Сформируйте список заново.",
                ));
            }
            if let Err(e) = kod::import_batch(&path, &records, &expected, replace) {
                return Ok(error(409, &e));
            }
            let mut receipt = format!(
                "<p>Восстановлено объектов: <b>{}</b>. Изменения записаны в том.</p><ul>",
                records.len()
            );
            for record in &records {
                let oid = record.integer(1024).unwrap();
                receipt.push_str(&format!(
                    "<li><a href='/20a?ispravlenieForm&amp;prevdoc={oid}'>{oid}: {}</a></li>",
                    esc(&record.string(1).map(crate::db::decode).unwrap_or_default())
                ));
            }
            receipt.push_str(
                "</ul><p>Повторная отправка этого подтверждения не выполняет запись снова.</p>",
            );
            receipt.push_str(&plan.links.summary(&plan.rows, &plan.selected));
            if !issues.is_empty() {
                receipt.push_str("<p>Не все связанные объекты были доступны при проверке. Их внутренние номера сохранены без изменения. Недостающие объекты можно восстановить отдельно.</p>");
            }
            plan.receipt = Some(receipt.clone());
            plan.rows.clear();
            plan.links = Default::default();
            plan.choices = choices::Choices::read(&[]);
            plan.selected.clear();
            drop(plan);
            if let Some(entry) = crate::execution::lock(plans())?.get_mut(id) {
                entry.bytes = receipt.len();
            }
            Ok(redirect(format!("{ROUTE}&plan={id}")))
        }
        _ => Ok(error(400, "Неизвестное действие восстановления")),
    }
}
