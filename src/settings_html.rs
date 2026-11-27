//! Group presentation independently from the stable settings-file field order.
use super::{Settings, Store, FIELDS};
use crate::monitor::escape;

impl Store {
    pub(super) fn render(&self, saved: &Settings, error: Option<&str>) -> String {
        let mut html = String::from("<!doctype html><html lang=ru><head><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'><title>Расширенные настройки · codex-AIs</title><style>body{font:16px Arial;margin:24px auto;padding:0 18px;background:#e3ddda;color:#222;max-width:1100px}h1{font-size:26px}h2{font-size:21px;margin-top:0}h3{font-size:17px}section{background:#fff;padding:20px;margin:20px 0;border:1px solid #bbb;border-radius:6px}table{border-collapse:collapse;width:100%}th,td{padding:10px;border:1px solid #ccc;text-align:left;vertical-align:top}th{background:#f2f4f7}td:first-child{width:46%}input,select{max-width:100%;width:13em;box-sizing:border-box;font:inherit;padding:5px}input[type=hidden]{display:none}button{font:inherit;padding:10px 18px;cursor:pointer}.save{position:sticky;bottom:0;padding:14px;background:#f2f4f7;border:1px solid #bbb}.notice{padding:12px;background:#fff4c2}.error{color:#a00}code{overflow-wrap:anywhere}.help{color:#454b53;font-size:14px;line-height:1.5}.scroll{overflow-x:auto}nav a{display:inline-block;margin:0 18px 8px 0}section form{margin-top:12px}section form input{width:8em}a{color:#064c9b}</style></head><body><p><a href='/admin/' target=_top>Администрирование</a> → Установки → Расширенные настройки</p><h1>Параметры codex-AIs</h1><p>Сохранённые параметры применяются после перезапуска. Действующие и сохранённые значения показаны отдельно.</p><nav><a href='#network'>Сеть</a><a href='#timeouts'>Таймауты HTTP</a><a href='#execution'>Исполнители и очереди</a><a href='#logging'>Журналы и история</a></nav>");
        if let Some(error) = error {
            html.push_str(&format!("<p class=error>{}</p>", escape(error)));
        }
        if saved != &self.active {
            html.push_str("<p class=notice>Есть сохранённые изменения. Для их применения перезапустите codex-AIs.</p>");
        }
        // Explicit form ownership keeps native port and Rust settings separate,
        // while displaying both network controls together without nested forms.
        html.push_str(&format!("<form id=rust-settings method=post action='/admin?advanced'><input type=hidden name=csrf value='{}'></form>", self.token));
        let active_values = self.active.values();
        let saved_values = saved.values();
        for (id, title, indices, help) in [
            ("network", "Сеть и соединения", &[8,0][..], "Лимит соединений включает открытые HTTP-соединения браузера и Word, в том числе ожидающие следующий запрос. KODEKS_HOST имеет приоритет над выбранными сетевыми интерфейсами."),
            ("timeouts", "Таймауты HTTP", &[2,3,4,7][..], "Таймаут отправки начинается после расчёта ответа. Keep-alive ограничивает ожидание следующего запроса, включая Word /99. Эти параметры не ограничивают длительность отчётов и удалений; ожидающее соединение не занимает исполнителя KS/DB."),
            ("execution", "Исполнители и очереди", &[6,9,1][..], "При auto число тяжёлых исполнителей равно доступным физическим ядрам; SMT/HT отдельно не учитываются. Владелец Word/состояния, поиск и интерфейс добавляются отдельно. Общий предел — 32. Ручное число задаёт весь пул и имеет приоритет. Несколько интерфейсных мест требуют как минимум interface_workers + 3 исполнителя. Размер пула выбирается при запуске."),
            ("logging", "Журналы и история", &[10,5][..], "Срок хранения относится к ZIP-архивам прошлых дней в OLD_Log. Значение 0 отключает удаление архивов по возрасту. История монитора ограничивается числом завершённых операций; это не срок хранения файлов журналов."),
        ] {
            html.push_str(&format!("<section id={id}><h2>{title}</h2><div class=scroll><table><thead><tr><th>Параметр</th><th>Работает сейчас</th><th>Сохранённое значение</th></tr></thead><tbody>"));
            for &index in indices {
                let field = FIELDS[index];
                let active = &active_values[index];
                let value = &saved_values[index];
                if field.0 == "listen_scope" {
                    html.push_str(&format!("<tr><td><label for=listen_scope>Сетевые интерфейсы</label></td><td>{} ({})</td><td><select form=rust-settings id=listen_scope name=listen_scope>", escape(&self.host.value), escape(self.host.source)));
                    for (option, title) in [("all", "Все IPv4-интерфейсы (0.0.0.0)"), ("localhost", "Только localhost (127.0.0.1)")] {
                        html.push_str(&format!("<option value='{option}'{}>{title}</option>", if value==option {" selected"} else {""}));
                    }
                    html.push_str("</select></td></tr>");
                } else if field.0 == "workers" {
                    let current = match self.active.workers {
                        crate::worker_config::Mode::Auto => format!("auto → {}", self.worker_selection.workers),
                        crate::worker_config::Mode::Manual(n) => format!("{n} (вручную)"),
                    };
                    html.push_str(&format!("<tr><td><label for=workers>Исполнители KS/DB — всего</label></td><td>{current}</td><td><select form=rust-settings id=workers name=workers>"));
                    for option in std::iter::once("auto".to_string()).chain((1..=32).map(|n| n.to_string())) {
                        let title = if option=="auto" {"auto — при запуске".into()} else {format!("{option} — вручную")};
                        html.push_str(&format!("<option value='{option}'{}>{title}</option>", if value==&option {" selected"} else {""}));
                    }
                    html.push_str("</select></td></tr>");
                } else {
                    html.push_str(&format!("<tr><td><label for='{}'>{}</label><br><small>{}–{}</small></td><td>{active}</td><td><input form=rust-settings required type=number id='{}' name='{}' min='{}' max='{}' value='{value}'></td></tr>", field.0, field.1, field.2, field.3, field.0, field.0, field.2, field.3));
                }
            }
            html.push_str(&format!("</tbody></table></div><p class=help>{help}</p>"));
            match id {
                "network" => html.push_str("<!--native-port-form-->"),
                "execution" => html.push_str(&format!("<details><summary>Как выбран текущий пул</summary><p class=help>{}</p></details><p class=help>HTTP и очередь KS настраиваются независимо. Для владельца и выделенного интерфейса предусмотрено по одному резервному месту очереди. Тяжёлые исполнители обслуживают общую очередь отчётов и удалений и помогают свободными местами поиску и интерфейсу. Больше исполнителей может увеличить расход памяти.</p>",escape(&self.worker_selection.explanation))),
                "logging" => html.push_str(&crate::runtime_logs::description()),
                _ => {},
            }
            html.push_str("</section>");
        }
        html.push_str(&format!("<div class=save><button form=rust-settings type=submit>Сохранить параметры codex-AIs</button><p class=help>Все параметры таблиц сохраняются вместе в <code>{}</code>. Предыдущие значения — в <code>codex-AIs.settings.bak</code>. Порт сохраняется отдельно своей кнопкой в config.db4.</p></div><p><a href='/admin?status'>Состояние сервера</a> · <a href='/admin/' target=_top>Администрирование</a></p></body></html>",escape(&self.path.display().to_string())));
        html
    }
}
