use super::{escape, Entry, Monitor, State};

fn executing(e: &Entry) -> bool {
    e.executor.is_some() && matches!(e.state, State::Running | State::Cancelling)
}
fn capacity(id: &str, title: &str, used: usize, total: usize, note: &str) -> String {
    let level=crate::resource_monitor::load_level((total>0).then(||100.0*used as f32/total as f32));
    format!("<section class=capacity-card id='{id}' data-used='{used}' data-total='{total}' data-load='{level}'><h3>{title}</h3><div class=capacity-value><strong>{used}</strong><span>/ {total}</span></div><meter min=0 max='{total}' value='{}' aria-label='{title}'>{used} / {total}</meter><p>{note}</p></section>",used.min(total))
}
impl Monitor {
    pub fn html(&self) -> String {
        let version = crate::product::version();
        let html = format!("<!doctype html><html lang=ru><head><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'><title>Состояние сервера</title><style>{}</style></head><body><header class=monitor-header><div class=brand><span class=brand-mark aria-hidden=true><svg viewBox='0 0 24 24' fill=none stroke=currentColor stroke-width='1.6'><path d='M3 12h4l3-7 4 14 3-7h4'/></svg></span><div><p class=eyebrow>CODEX-AIS · {version}</p><h1>Состояние сервера</h1></div></div><div class=monitor-toolbar><button id=monitor-pause type=button aria-pressed=false>Пауза</button><button id=monitor-refresh type=button>Обновить</button><span id=monitor-update role=status aria-live=polite></span><a class=settings-link href='/admin?advanced'>Настройки</a></div></header><noscript><p>Для автообновления нужен JavaScript. <a href='?status'>Обновить страницу</a>.</p></noscript>{}<script>{}</script></body></html>",include_str!("monitor.css"),self.snapshot_html(),include_str!("monitor_live.js"));
        html
    }

    fn worker_groups(&self, active: &[Entry], layout: crate::pool_layout::Layout) -> String {
        let workers = self.workers.workers;
        let role = |i| {
            if workers==1 {("all","Все запросы")}
            else if i==0 {("owner",if workers==2 {"Word / интерфейс"}else{"Word / состояние"})}
            else if layout.is_interface(i) {("interface","Интерфейс")}
            else if workers<4 {("shared","Поиск / тяжёлые")}
            else if i==layout.search {("search","Поиск")}
            else {("heavy","Отчёты и удаления")}
        };
        let mut groups: Vec<(&str, &str, Vec<usize>)> = Vec::new();
        for i in 0..workers {
            let (key,title) = role(i);
            if let Some(group) = groups.iter_mut().find(|g|g.0==key) {group.2.push(i)}
            else {groups.push((key,title,vec![i]))}
        }
        let mut out = format!("<section class=workers-section id=ks-pool data-process-id='{}' data-workers=\"{workers}\" data-interface-workers=\"{}\" data-heavy-workers=\"{}\" data-queue=\"{}\"><div class=section-title><h2>Исполнители KS/DB</h2><span class=worker-legend><span><i></i>Свободен</span><span><i class=busy-dot></i>Занят</span><span><i class=cancel-dot></i>Отмена</span></span></div><div class='worker-groups groups-{}'>",std::process::id(),layout.interfaces,workers-layout.bulk,self.settings.queue,groups.len());
        for (key,title,indices) in groups {
            let busy = indices.iter().filter(|i| active.iter().any(|e|executing(e) && e.executor==Some(**i+1))).count();
            out.push_str(&format!("<section class=worker-group id='workers-{key}' data-busy='{busy}' data-total='{}'><div class=group-title><h3>{title}</h3><span class=group-count title='Занято / всего'>{busy} / {}</span></div><ul class=worker-list>",indices.len(),indices.len()));
            for i in indices {
                let current = active.iter().find(|e|executing(e) && e.executor==Some(i+1));
                let class = current.map(|e|if e.state==State::Cancelling {" cancelling"}else{" busy"}).unwrap_or("");
                let state = current.map(|e|if e.state==State::Cancelling {"Отмена"}else{"Занят"}).unwrap_or("Свободен");
                let detail = current.map(|e|format!("{state}: {} · {}",e.group,e.url)).unwrap_or_else(||"Свободен".into());
                out.push_str(&format!("<li class='worker{class}' id='worker-{}' title='{}' aria-label='№{} · {title} · {state}'><b>№{}</b><small>{state}</small></li>",i+1,escape(&detail),i+1,i+1));
            }
            out.push_str("</ul></section>");
        }
        out.push_str("</div></section>");out
    }
    pub fn snapshot_html(&self) -> String {
        let (active, history, scheduled, connections) = {
            let m=self.inner.lock().unwrap();
            (m.active.values().filter(|e|!e.monitor_poll).cloned().collect::<Vec<_>>(),m.history.clone(),m.scheduled.clone(),m.connections)
        };
        let workers=self.workers.workers;
        let layout=crate::pool_layout::Layout::new(workers,self.settings.interface_workers).expect("validated pool layout");
        let busy=active.iter().filter(|e|executing(e)).filter_map(|e|e.executor).collect::<std::collections::BTreeSet<_>>();
        let queued=active.iter().filter(|e|!e.job && (e.state==State::Queued || e.state==State::Cancelling && e.executor.is_none())).count();
        let reserves=workers.saturating_sub(1).min(2);
        let (common_used,reserve_used)=self.admission.get().map(|q| {
            let common=self.settings.queue.saturating_sub(q[0].available_permits());
            let reserved=q.iter().skip(1).take(reserves).map(|s|1usize.saturating_sub(s.available_permits())).sum::<usize>();
            (common,reserved)
        }).unwrap_or((queued.min(self.settings.queue),queued.saturating_sub(self.settings.queue).min(reserves)));
        let mut out=String::from("<main id=monitor-data data-monitor-snapshot='1'>");
        out.push_str(&format!("<details class=monitor-help id=monitor-help><summary>Как читать показатели</summary><p>KS/DB — исполнители, занятые запросами. Зелёный — свободен, красный — занят (это не ошибка), жёлтый — отмена; при отмене место освобождается после безопасного завершения. Шкала очереди включает резерв для Word и интерфейса. HTTP включает keep-alive и запрос монитора.</p><p>ЦП: 100% — все логические процессоры. Графики: последние две минуты; сплошная линия — codex-AIs, пунктир — вся система. Цвет нагрузки: до 50% — зелёный, 50–79% — жёлтый, от 80% — красный. Для ОЗУ процент считается от всей физической памяти. ОЗУ в МиБ или ГиБ (1024 МиБ); график ОЗУ — ГиБ. ОЗУ процесса — рабочий набор (RSS); ОЗУ системы — вся память минус доступная. На ВМ показаны ресурсы гостевой ОС. Недоступные значения: «—».</p><p>{}</p></details>",escape(&self.workers.explanation)));
        out.push_str("<div class=capacity-grid id=capacity-grid>");
        out.push_str(&capacity("capacity-ks","Занято KS/DB",busy.len(),workers,&format!("Свободно: {}",workers.saturating_sub(busy.len()))));
        out.push_str(&capacity("capacity-queue","Очередь KS",common_used+reserve_used,self.settings.queue+reserves,&format!("Общая: {common_used}/{} · резерв: {reserve_used}/{reserves}",self.settings.queue)));
        out.push_str(&capacity("capacity-http","HTTP-соединения",connections,self.settings.connections,&format!("Свободно: {}",self.settings.connections.saturating_sub(connections))));
        out.push_str("</div>");
        out.push_str(&self.worker_groups(&active, layout));
        out.push_str(&self.resources.html());
        for (key,title,rows,job) in [
            ("active-http","Активные HTTP-запросы",active.clone(),false),
            ("active-at","Активные задания At",active.clone(),true),
            ("history-http","История HTTP",history.iter().cloned().collect(),false),
            ("history-at","История At",history.iter().cloned().collect(),true),
        ] {
            let total = rows.iter().filter(|e|e.job==job).count();
            out.push_str(&format!("<section class=request-section id='{key}'><div class=section-title><h3>{title}<span class=row-count>{total}</span></h3></div><div class=table-scroll id='scroll-{key}'><table><thead><tr><th>ID</th><th>Состояние</th><th>Результат</th><th>URL / действие</th><th>Источник / IP:Port</th><th>Пользователь</th><th>Начало UTC (Unix)</th><th>Мс</th><th>Соединение</th><th>KS/DB</th><th>Группа</th></tr></thead><tbody>"));
            let mut count=0;
            for e in rows.iter().filter(|e|e.job==job) {
                count+=1;
                out.push_str(&format!("<tr id='row-{key}-{}'><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td{}>{}</td><td>{}</td><td>{}</td></tr>",e.id,e.id,e.state.label(),escape(&e.result),escape(&e.url),escape(&e.source),escape(&e.user),e.start,e.elapsed.unwrap_or_else(||e.tick.elapsed().as_millis()),if e.disconnected{" class=lost"}else{""},if e.disconnected {if e.state==State::Cancelling {"отключено; ожидание остановки"}else{"отключено"}}else if e.job{"—"}else{""},e.executor.map(|v|v.to_string()).unwrap_or_default(),e.group));
            }
            if count==0 {out.push_str("<tr class=empty-row data-key=empty><td colspan=11>Нет записей</td></tr>");}
            out.push_str("</tbody></table></div></section>");
        }
        out.push_str("<details id=monitor-schedules class=monitor-help><summary>Расписание заданий At</summary><ul>");
        for row in scheduled {out.push_str(&format!("<li>{}</li>",escape(&row)));}
        out.push_str(&format!("</ul></details><p class=monitor-note>История: до {} завершений. Обновления монитора в историю не добавляются.</p></main>",self.settings.history));
        out
    }
}
