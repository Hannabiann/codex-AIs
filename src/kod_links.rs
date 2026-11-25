//! Outgoing reference checks for a KOD preview. No record or link is rewritten.
use super::{blocked, conflicts, esc, Row};
use crate::db::fields::Value;
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Link {
    field: u16,
    oid: u32,
}
#[derive(Default)]
pub(super) struct Links {
    rows: Vec<Vec<Link>>,
    present: HashMap<u32, bool>,
    uploaded: HashMap<u32, Vec<usize>>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Selected(usize),
    Existing,
    Unselected(usize),
    Ambiguous,
    Missing,
    Removed,
}
pub(super) type Issue = (u32, u16, u32, u8);
impl Links {
    pub(super) fn read(
        rows: &[Row],
        mut exists: impl FnMut(u32) -> Result<bool, String>,
    ) -> Result<Self, String> {
        let mut result = Self::default();
        let mut link_bytes = 0usize;
        for (i, row) in rows.iter().enumerate() {
            result
                .uploaded
                .entry(row.record.integer(1024).unwrap())
                .or_default()
                .push(i);
            let mut links = Vec::new();
            for f in row.record.fields.iter() {
                if f.id == 1024 || f.wire_type != 5 {
                    continue;
                }
                if let Value::Integer(oid) = f.value {
                    if oid != 0 {
                        links.push(Link { field: f.id, oid });
                    }
                }
            }
            links.sort_unstable();
            links.dedup();
            link_bytes += links.capacity() * std::mem::size_of::<Link>();
            for link in &links {
                crate::execution::check()?;
                if link_bytes + result.present.capacity() * 24 > super::MAX_PENDING {
                    return Err(
                        "Превышен бюджет памяти проверки связей KOD. Разделите набор файлов."
                            .into(),
                    );
                }
                if let std::collections::hash_map::Entry::Vacant(e) = result.present.entry(link.oid)
                {
                    e.insert(exists(link.oid)?);
                }
            }
            result.rows.push(links);
        }
        Ok(result)
    }
    pub(super) fn memory(&self) -> usize {
        self.rows
            .iter()
            .map(|v| v.capacity() * std::mem::size_of::<Link>())
            .sum::<usize>()
            + self.present.capacity() * 24
            + self.uploaded.capacity() * 48
            + self
                .uploaded
                .values()
                .map(|v| v.capacity() * std::mem::size_of::<usize>())
                .sum::<usize>()
    }
    fn removed(rows: &[Row], selected: &[bool]) -> HashSet<u32> {
        rows.iter()
            .zip(selected)
            .filter(|(_, yes)| **yes)
            .flat_map(|(r, _)| {
                r.state
                    .names
                    .iter()
                    .filter_map(|n| n.integer(1024))
                    .filter(|id| Some(*id) != r.record.integer(1024))
            })
            .collect()
    }
    fn state(&self, oid: u32, selected: &[bool], removed: &HashSet<u32>) -> State {
        if removed.contains(&oid) {
            return State::Removed;
        }
        if let Some(indices) = self.uploaded.get(&oid) {
            let mut chosen = indices.iter().copied().filter(|&i| selected[i]);
            if let Some(i) = chosen.next() {
                if chosen.next().is_some() {
                    return State::Ambiguous;
                }
                return State::Selected(i);
            }
        }
        if self.present.get(&oid) == Some(&true) {
            return State::Existing;
        }
        match self.uploaded.get(&oid) {
            Some(indices) if indices.len() == 1 => State::Unselected(indices[0]),
            Some(_) => State::Ambiguous,
            None => State::Missing,
        }
    }
    pub(super) fn add_related(
        &self,
        rows: &[Row],
        selected: &mut [bool],
        choices: &super::choices::Choices,
    ) -> usize {
        let mut pending: Vec<_> = selected
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.then_some(i))
            .collect();
        let mut visited = HashSet::new();
        let mut added = 0;
        while let Some(i) = pending.pop() {
            if !visited.insert(i) {
                continue;
            }
            for link in &self.rows[i] {
                let Some(indices) = self.uploaded.get(&link.oid) else {
                    continue;
                };
                if let Some(&other) = indices.iter().find(|&&j| selected[j]) {
                    pending.push(other);
                    continue;
                }
                if indices.len() != 1 {
                    continue;
                }
                let other = indices[0];
                let row = &rows[other];
                if self.present.get(&link.oid) != Some(&true)
                    && !choices.ambiguous(other)
                    && !blocked(row)
                    && !conflicts(row)
                    && !row.state.identical(&row.record)
                {
                    selected[other] = true;
                    added += 1;
                    pending.push(other);
                }
            }
        }
        added
    }
    pub(super) fn issues(&self, rows: &[Row], selected: &[bool]) -> BTreeSet<Issue> {
        let removed = Self::removed(rows, selected);
        let mut issues = BTreeSet::new();
        for (i, links) in self.rows.iter().enumerate().filter(|(i, _)| selected[*i]) {
            for link in links {
                let reason = match self.state(link.oid, selected, &removed) {
                    State::Unselected(_) => 1,
                    State::Missing => 2,
                    State::Removed => 3,
                    State::Ambiguous => 4,
                    _ => continue,
                };
                issues.insert((
                    rows[i].record.integer(1024).unwrap(),
                    link.field,
                    link.oid,
                    reason,
                ));
            }
        }
        issues
    }
    pub(super) fn summary(&self, rows: &[Row], selected: &[bool]) -> String {
        let issues = self.issues(rows, selected);
        let pending = issues.iter().filter(|i| i.3 == 1).count();
        let missing = issues.iter().filter(|i| i.3 == 2).count();
        let removed = issues.iter().filter(|i| i.3 == 3).count();
        let ambiguous = issues.iter().filter(|i| i.3 == 4).count();
        format!("<p id=link-summary>Проверка выбранных объектов: ссылки на невыбранные файлы — <b>{pending}</b>; требуется выбор редакции — <b>{ambiguous}</b>; ссылки на отсутствующие объекты — <b>{missing}</b>; ссылки на номера, удаляемые заменой по имени, — <b>{removed}</b>.</p>")
    }
    pub(super) fn replacements(&self, id: &str, rows: &[Row], selected: &[bool]) -> String {
        let removed = Self::removed(rows, selected);
        if removed.is_empty() {
            return String::new();
        }
        let mut counts = HashMap::<u32, usize>::new();
        for (_, links) in self.rows.iter().enumerate().filter(|(i, _)| selected[*i]) {
            for link in links {
                if removed.contains(&link.oid) {
                    *counts.entry(link.oid).or_default() += 1;
                }
            }
        }
        let mut html=String::from("<details open><summary>Последствия замены по имени</summary><p>В выбранном томе будут удалены прежние номера. Ссылки на них не переназначаются. Ниже учитываются только выбранные файлы; ссылки из остальных объектов базы здесь не проверяются.</p><table><tr><th>Прежний номер</th><th>Новый номер / файл</th><th>Ссылки из выбранных файлов</th></tr>");
        for (i, row) in rows.iter().enumerate().filter(|(i, _)| selected[*i]) {
            for old in &row.state.names {
                let Some(oid) = old
                    .integer(1024)
                    .filter(|n| Some(*n) != row.record.integer(1024))
                else {
                    continue;
                };
                let count = counts.get(&oid).copied().unwrap_or(0);
                html.push_str(&format!("<tr><td>{oid}</td><td>{} / № {}: {}</td><td><a target=_blank rel=noopener href='{}&amp;plan={}&amp;replaced={oid}'>{count}</a></td></tr>",row.record.integer(1024).unwrap(),i+1,esc(&row.filename),super::ROUTE,esc(id)));
            }
        }
        html.push_str("</table></details>");
        html
    }
    pub(super) fn replacement_page(
        &self,
        id: &str,
        oid: u32,
        page: usize,
        rows: &[Row],
        selected: &[bool],
    ) -> Option<String> {
        if !Self::removed(rows, selected).contains(&oid) {
            return None;
        }
        let sources = || {
            self.rows
                .iter()
                .enumerate()
                .filter(|(i, _)| selected[*i])
                .flat_map(|(i, links)| {
                    links
                        .iter()
                        .filter(move |l| l.oid == oid)
                        .map(move |l| (i, l))
                })
        };
        let count = sources().count();
        let pages = count.div_ceil(100).max(1);
        if page >= pages {
            return None;
        }
        let mut html=format!("<p>Ссылки на удаляемый при замене номер <b>{oid}</b>: {count}. Страница {} из {pages}. Показаны только выбранные файлы.</p><table><tr><th>Файл</th><th>Объект</th><th>Атрибут</th></tr>",page+1);
        for (i, link) in sources().skip(page * 100).take(100) {
            html.push_str(&format!(
                "<tr><td>№ {}: {}</td><td>{}</td><td>{}</td></tr>",
                i + 1,
                esc(&rows[i].filename),
                rows[i].record.integer(1024).unwrap(),
                link.field
            ));
        }
        html.push_str("</table><p>");
        let base = format!(
            "{}&amp;plan={}&amp;replaced={oid}&amp;impactpage=",
            super::ROUTE,
            esc(id)
        );
        if page > 0 {
            html.push_str(&format!(
                "<a href='{base}{}'>Предыдущие 100</a> · ",
                page - 1
            ));
        }
        if page + 1 < pages {
            html.push_str(&format!(
                "<a href='{base}{}'>Следующие 100</a> · ",
                page + 1
            ));
        }
        html.push_str(&format!(
            "<a href='{}&amp;plan={}'>К выбору файлов</a></p>",
            super::ROUTE,
            esc(id)
        ));
        Some(html)
    }
    pub(super) fn row_html(
        &self,
        id: &str,
        index: usize,
        rows: &[Row],
        selected: &[bool],
    ) -> String {
        let links = &self.rows[index];
        if links.is_empty() {
            return "Нет ссылок".into();
        }
        let removed = Self::removed(rows, selected);
        let mut out = format!("<details><summary>Ссылки: {}</summary><ul>", links.len());
        for link in links.iter().take(10) {
            out.push_str(&self.link_html(link, rows, selected, &removed));
        }
        out.push_str("</ul>");
        if links.len() > 10 {
            out.push_str(&format!("<p>Показаны первые 10. <a href='{}&amp;plan={}&amp;linkrow={index}'>Все ссылки: {}</a></p>", super::ROUTE, esc(id), links.len()));
        }
        out.push_str("</details>");
        out
    }
    pub(super) fn detail_page(
        &self,
        id: &str,
        index: usize,
        page: usize,
        rows: &[Row],
        selected: &[bool],
    ) -> Option<String> {
        let links = self.rows.get(index)?;
        let pages = links.len().div_ceil(100).max(1);
        if page >= pages {
            return None;
        }
        let mut out = format!(
            "<p>Файл: <b>{}</b>. Объект: {}. Ссылок: {}. Страница {} из {pages}.</p><ul>",
            esc(&rows[index].filename),
            rows[index].record.integer(1024).unwrap(),
            links.len(),
            page + 1
        );
        let removed = Self::removed(rows, selected);
        for link in links.iter().skip(page * 100).take(100) {
            out.push_str(&self.link_html(link, rows, selected, &removed));
        }
        out.push_str("</ul><p>");
        let base = format!(
            "{}&amp;plan={}&amp;linkrow={index}&amp;linkpage=",
            super::ROUTE,
            esc(id)
        );
        if page > 0 {
            out.push_str(&format!(
                "<a href='{base}{}'>Предыдущие 100</a> · ",
                page - 1
            ));
        }
        if page + 1 < pages {
            out.push_str(&format!(
                "<a href='{base}{}'>Следующие 100</a> · ",
                page + 1
            ));
        }
        out.push_str(&format!(
            "<a href='{}&amp;plan={}'>К выбору файлов</a></p>",
            super::ROUTE,
            esc(id)
        ));
        Some(out)
    }
    fn link_html(
        &self,
        link: &Link,
        rows: &[Row],
        selected: &[bool],
        removed: &HashSet<u32>,
    ) -> String {
        let state = self.state(link.oid, selected, &removed);
        let status = match state {
            State::Selected(i) => format!("файл {} выбран", esc(&rows[i].filename)),
            State::Existing => "есть в базе".into(),
            State::Unselected(i) => format!(
                "нет в базе; файл {} не выбран{}",
                esc(&rows[i].filename),
                if blocked(&rows[i]) {
                    " (конфликт запрещает восстановление)"
                } else {
                    ""
                }
            ),
            State::Missing => "нет в базе и среди файлов".into(),
            State::Ambiguous => "несколько файлов этого номера; требуется выбор редакции".into(),
            State::Removed => "удаляется из выбранного тома при замене по имени".into(),
        };
        let oid = link.oid;
        let label = if state == State::Existing {
            format!("<a href='/20a?ispravlenieForm&amp;prevdoc={oid}' target=_blank rel=noopener>{oid}</a>")
        } else {
            oid.to_string()
        };
        format!("<li>Атрибут {} → {label}: {status}</li>", link.field)
    }
}
