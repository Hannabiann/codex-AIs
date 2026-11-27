//! Native HTTP port editing, separate from the Rust settings-file transaction.
use crate::{
    db::{writer::registry::Transaction, Db4Engine},
    monitor::escape,
    server::Response,
};
fn open(engine: &Db4Engine) -> Result<Transaction, String> {
    let path = &engine.volumes.first().ok_or("config.db4 not loaded")?.path;
    Transaction::open(path, engine.registry.root)
}
pub(super) fn form(engine: &Db4Engine, token: &str) -> Result<String, String> {
    let tx = open(engine)?;
    let port = crate::listen_config::select(Some(&tx.registry), None)?.port;
    let active = crate::listen_config::address()
        .map(|a| a.to_string())
        .unwrap_or_else(|| "ещё не запущен".into());
    let override_note=match std::env::var("KODEKS_PORT") {
        Ok(v)=>format!("<p class=notice>KODEKS_PORT={} имеет приоритет при запуске Rust. Для использования порта из config.db4 уберите это переопределение.</p>",escape(&v)),
        Err(_)=>String::new(),
    };
    Ok(format!("<h3>Порт HTTP — общий с kodweb</h3><p>Работает сейчас: <b>{}</b>. Сохранено в config.db4: <b>{port}</b>. Новый порт применяется после перезапуска. Эта настройка используется и оригинальным kodweb.</p>{override_note}<form method=post action='/admin?advanced'><input type=hidden name=action value=set-port><input type=hidden name=csrf value='{token}'><input type=hidden name=revision value='{}'><label>Порт (1–65535) <input required type=number min=1 max=65535 name=port value='{port}'></label><br><button type=submit>Сохранить порт в config.db4</button></form>",escape(&active),tx.revision()))
}
fn reply(status: &'static str, text: &str) -> Response {
    Response{status,mime:"text/html; charset=utf-8",body:format!("<!doctype html><meta charset=utf-8><p>{}</p><p><a href='/admin?advanced'>Вернуться к настройкам</a></p>",escape(text)).into_bytes(),location:None,headers:vec![("Cache-Control".into(),"no-store".into()),("X-Frame-Options".into(),"SAMEORIGIN".into())]}
}
pub(super) fn save(
    engine: &Db4Engine,
    token: &str,
    method: &str,
    fields: &[(String, String)],
) -> Response {
    if method != "POST" {
        return reply("405 Method Not Allowed", "Для сохранения требуется POST");
    }
    let field = |key: &str| {
        fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    if fields.iter().filter(|(k, _)| k == "csrf").count() != 1 || field("csrf") != Some(token) {
        return reply(
            "403 Forbidden",
            "Форма устарела. Откройте настройки заново.",
        );
    }
    let mut names = std::collections::HashSet::new();
    if fields.len() != 4
        || fields.iter().any(|(k, _)| {
            !matches!(k.as_str(), "csrf" | "action" | "port" | "revision") || !names.insert(k)
        })
    {
        return reply(
            "400 Bad Request",
            "Указаны неверные или повторные параметры порта",
        );
    }
    let value = field("port").unwrap_or("");
    let Some(port) = value.parse::<u16>().ok().filter(|n| *n > 0) else {
        return reply(
            "400 Bad Request",
            "Порт должен быть целым числом от 1 до 65535",
        );
    };
    let result = (|| {
        let mut tx = open(engine)?;
        if field("revision") != Some(tx.revision().as_str()) {
            return Err("Настройки изменились. Откройте форму заново.".into());
        }
        let root = tx.registry.root;
        let kind = tx
            .registry
            .query(root, "Port")?
            .map(|e| e.kind.clone())
            .unwrap_or_else(|| "REG_DWORD".into());
        tx.set(root, "Port", &kind, Some(&port.to_string()))?;
        tx.commit()?;
        Ok::<_, String>(())
    })();
    match result {
        Ok(()) => Response {
            status: "303 See Other",
            mime: "text/plain; charset=utf-8",
            body: Vec::new(),
            location: Some("/admin?advanced&port_saved=1".into()),
            headers: vec![("Cache-Control".into(), "no-store".into())],
        },
        Err(e) => reply(
            if e == "Настройки изменились. Откройте форму заново."
            {
                "409 Conflict"
            } else {
                "500 Internal Server Error"
            },
            &e,
        ),
    }
}
