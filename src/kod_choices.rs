//! Mutually exclusive uploaded snapshots. The writer independently checks the
//! same identity/name constraints again before publishing a selected batch.
use super::{esc, Row, ROUTE};
use crate::db::fields::{Field, Record, Value};
use std::collections::BTreeMap;

pub(super) struct Choices {
    peers: Vec<Vec<usize>>,
}
impl Choices {
    pub(super) fn read(rows: &[Row]) -> Self {
        let keys: Vec<_> = rows
            .iter()
            .map(|r| {
                (
                    r.record.integer(1024),
                    r.record.area,
                    r.record
                        .string(1)
                        .map(crate::db::decode)
                        .unwrap_or_default()
                        .to_lowercase(),
                )
            })
            .collect();
        let mut peers = vec![Vec::new(); rows.len()];
        for i in 0..rows.len() {
            for j in i + 1..rows.len() {
                if keys[i].0 == keys[j].0
                    || (keys[i].1 == keys[j].1 && keys[i].2 == keys[j].2)
                    || deletes(&rows[i], keys[j].0)
                    || deletes(&rows[j], keys[i].0)
                {
                    peers[i].push(j);
                    peers[j].push(i);
                }
            }
        }
        Self { peers }
    }
    pub(super) fn ambiguous(&self, index: usize) -> bool {
        !self.peers[index].is_empty()
    }
    pub(super) fn invalid(&self, selected: &[bool]) -> bool {
        self.peers
            .iter()
            .enumerate()
            .any(|(i, peers)| selected[i] && peers.iter().any(|&j| selected[j]))
    }
    pub(super) fn memory(&self) -> usize {
        self.peers
            .iter()
            .map(|p| p.capacity() * std::mem::size_of::<usize>())
            .sum::<usize>()
            + self.peers.capacity() * std::mem::size_of::<Vec<usize>>()
    }
    pub(super) fn checkbox(&self, index: usize) -> String {
        // Indices are generated here, never supplied by an uploaded file.
        let names = self.peers[index]
            .iter()
            .map(|j| format!("'pick{j}'"))
            .collect::<Vec<_>>()
            .join(",");
        format!("onchange=\"if(this.checked){{var f=this.form;[{names}].forEach(function(n){{f.elements[n].checked=false;}});}}\"")
    }
    pub(super) fn row_html(&self, id: &str, index: usize, rows: &[Row]) -> String {
        if !self.ambiguous(index) {
            return String::new();
        }
        let mut out = String::from(
            "<details><summary>Совпадения среди файлов — выберите один вариант</summary><ul>",
        );
        for &j in &self.peers[index] {
            out.push_str(&format!("<li><a target=_blank rel=noopener href='{ROUTE}&amp;plan={}&amp;compare={index}&amp;with={j}'>№ {}: {}</a></li>", esc(id), j + 1, esc(&rows[j].filename)));
            if deletes(&rows[index], rows[j].record.integer(1024))
                || deletes(&rows[j], rows[index].record.integer(1024))
            {
                out.push_str("<li>Замена по имени удалит номер другого файла этой пары.</li>");
            }
        }
        out.push_str("</ul></details>");
        out
    }
    pub(super) fn compare(
        &self,
        id: &str,
        a: usize,
        b: usize,
        page: usize,
        rows: &[Row],
    ) -> Option<String> {
        if !self.peers.get(a)?.contains(&b) {
            return None;
        }
        let left = &rows[a];
        let right = &rows[b];
        let base = format!(
            "{ROUTE}&amp;plan={}&amp;compare={a}&amp;with={b}&amp;diffpage=",
            esc(id)
        );
        comparison(
            id,
            page,
            [&left.record, &right.record],
            [
                &format!("Файл № {}: {}", a + 1, left.filename),
                &format!("Файл № {}: {}", b + 1, right.filename),
            ],
            &base,
        )
    }
}
fn deletes(row: &Row, oid: Option<u32>) -> bool {
    oid.is_some()
        && oid != row.record.integer(1024)
        && row.state.names.iter().any(|r| r.integer(1024) == oid)
}
pub(super) fn database_links(id: &str, index: usize, row: &Row) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut html = String::new();
    for record in row.state.current.iter().chain(&row.state.names) {
        let Some(oid) = record.integer(1024) else {
            continue;
        };
        if seen.insert(oid) {
            html.push_str(&format!("<p><a target=_blank rel=noopener href='{ROUTE}&amp;plan={}&amp;dbrow={index}&amp;dboid={oid}'>Сравнить с записью в томе: {oid}</a></p>",esc(id)));
        }
    }
    html
}
pub(super) fn database_compare(
    id: &str,
    index: usize,
    oid: u32,
    page: usize,
    rows: &[Row],
) -> Option<String> {
    let row = rows.get(index)?;
    let stored = row
        .state
        .current
        .iter()
        .chain(&row.state.names)
        .find(|r| r.integer(1024) == Some(oid))?;
    let base = format!(
        "{ROUTE}&amp;plan={}&amp;dbrow={index}&amp;dboid={oid}&amp;diffpage=",
        esc(id)
    );
    let mut html=String::from("<p>Сравнение с записью выбранного тома на момент подготовки списка. Перед записью её неизменность проверяется повторно.</p>");
    html.push_str(&comparison(
        id,
        page,
        [&row.record, stored],
        [
            &format!("Файл № {}: {}", index + 1, row.filename),
            "Запись в выбранном томе",
        ],
        &base,
    )?);
    Some(html)
}
fn comparison(
    id: &str,
    page: usize,
    records: [&Record; 2],
    labels: [&str; 2],
    base: &str,
) -> Option<String> {
    let [left, right] = records;
    // Keep order and duplicate values within each attribute when comparing.
    fn group(r: &Record) -> BTreeMap<u16, Vec<&Field>> {
        let mut fields = BTreeMap::<u16, Vec<&Field>>::new();
        for field in r.fields.iter() {
            fields.entry(field.id).or_default().push(field);
        }
        fields
    }
    let l = group(left);
    let r = group(right);
    let keys: std::collections::BTreeSet<_> = l.keys().chain(r.keys()).copied().collect();
    let differences: Vec<_> = keys
        .into_iter()
        .filter(|key| l.get(key) != r.get(key))
        .collect();
    let pages = differences.len().div_ceil(25).max(1);
    if page >= pages {
        return None;
    }
    let mut out = format!("<p><b>{}</b> / <b>{}</b>.</p><p>Номера: {} / {}. Области: {} / {}. Отличающихся атрибутов: {}. Страница {} из {pages}.</p>",
            esc(labels[0]),esc(labels[1]),left.integer(1024).unwrap(), right.integer(1024).unwrap(), left.area, right.area, differences.len(), page+1);
    if left == right {
        out.push_str("<p>Данные объектов совпадают.</p>");
    } else {
        out.push_str(&format!("<p>Показаны только отличия. В каждом атрибуте — первые три значения, до 160 символов строки. Текст и вложения сравниваются полностью, здесь показан их размер. Даже при одинаковом размере содержимое может отличаться.</p><table><tr><th>Атрибут</th><th>{}</th><th>{}</th></tr>",esc(labels[0]),esc(labels[1])));
        for key in differences.iter().skip(page * 25).take(25) {
            out.push_str(&format!(
                "<tr><td>{key}</td><td>{}</td><td>{}</td></tr>",
                values(l.get(key)),
                values(r.get(key))
            ));
        }
        out.push_str("</table>");
    }
    out.push_str("<p>");
    if page > 0 {
        out.push_str(&format!(
            "<a href='{base}{}'>Предыдущие 25</a> · ",
            page - 1
        ));
    }
    if page + 1 < pages {
        out.push_str(&format!("<a href='{base}{}'>Следующие 25</a> · ", page + 1));
    }
    out.push_str(&format!(
        "<a href='{ROUTE}&amp;plan={}'>К выбору файлов</a></p>",
        esc(id)
    ));
    Some(out)
}
fn values(fields: Option<&Vec<&Field>>) -> String {
    let Some(fields) = fields else {
        return "Атрибут отсутствует".into();
    };
    let mut out = format!("Значений: {}<ul>", fields.len());
    for field in fields.iter().take(3) {
        let value = match &field.value {
            Value::String(bytes) => {
                let text = crate::db::decode(&bytes[..bytes.len().min(160)]);
                format!("{}{}", esc(&text), if bytes.len() > 160 { "…" } else { "" })
            }
            Value::Integer(n) => n.to_string(),
            Value::Date(d) => format!(
                "{:02}.{:02}.{}",
                d[3],
                d[2],
                u16::from_le_bytes([d[0], d[1]])
            ),
            Value::Timestamp(d) => format!(
                "{:02}.{:02}.{} {:02}:{:02}:{:02}.{:02}",
                d[3],
                d[2],
                u16::from_be_bytes([d[0], d[1]]),
                d[4],
                d[5],
                d[6],
                d[7]
            ),
            Value::Decimal(d) => crate::ks::runtime::decimal::Decimal::from_native(d)
                .map(|v| v.text())
                .unwrap_or_else(|_| "Число за пределами поддерживаемой точности".into()),
            Value::Text(data) => format!("Текст: {} байт", data.len()),
            Value::TextWithMetadata { data, metadata } => format!(
                "Текст: {} байт, метаданные: {} байт",
                data.len(),
                metadata.len()
            ),
            Value::Binary { data, file, .. } => format!(
                "Вложение: {} байт{}",
                data.len(),
                file.as_ref()
                    .map(|f| format!(", {}", esc(&crate::db::decode(&f.name))))
                    .unwrap_or_default()
            ),
        };
        out.push_str(&format!("<li>Тип {}: {value}</li>", field.wire_type));
    }
    out.push_str("</ul>");
    out
}
