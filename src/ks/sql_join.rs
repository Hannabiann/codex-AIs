//! Bounded multi-area equijoins used by native registry definitions.
//! Local predicates still use the ordinary SQL parser and native indexes.
use super::{
    database::DatabaseHost,
    host::{Column, ColumnValues, Projection, QueryData},
    sql::{self, Token},
};
use crate::db::{
    fields::{Record, Value},
    schema::Area,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
struct Ref {
    area: usize,
    field: String,
}
#[derive(Clone, Debug)]
pub struct Join {
    areas: Vec<String>,
    columns: Vec<(usize, Column)>,
    local: Vec<String>,
    edges: Vec<(Ref, Ref)>,
    order: Vec<(Ref, bool)>,
}
fn kw(t: &Token, word: &str) -> bool {
    t.0 == 'w' && t.1.eq_ignore_ascii_case(word)
}
fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}
fn text(tokens: &[Token]) -> String {
    tokens
        .iter()
        .map(|t| match t.0 {
            '"' => quoted(&t.1),
            '\'' => format!("'{}'", t.1.replace('\'', "''")),
            _ => t.1.clone(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}
fn strip(mut t: &[Token]) -> &[Token] {
    while t.first().is_some_and(|t| t.0 == '(') && t.last().is_some_and(|t| t.0 == ')') {
        let mut depth = 0;
        let mut encloses = true;
        for (i, x) in t.iter().enumerate() {
            if x.0 == '(' {
                depth += 1;
            } else if x.0 == ')' {
                depth -= 1;
            }
            if depth == 0 && i + 1 < t.len() {
                encloses = false;
                break;
            }
        }
        if !encloses {
            break;
        }
        t = &t[1..t.len() - 1];
    }
    t
}
fn split(t: &[Token], comma: bool) -> Result<Vec<&[Token]>, String> {
    if !comma {
        let mut depth = 0;
        for x in t {
            if x.0 == '(' {
                depth += 1;
            } else if x.0 == ')' {
                depth -= 1;
            }
            // OR has lower precedence than AND: keep its entire local tree.
            if depth == 0 && kw(x, "OR") {
                return Ok(vec![t]);
            }
        }
    }
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let mut between = false;
    for (i, x) in t.iter().enumerate() {
        if x.0 == '(' {
            depth += 1;
        } else if x.0 == ')' {
            depth -= 1;
        }
        if depth < 0 {
            return Err("unbalanced join expression".into());
        }
        if depth == 0 {
            if kw(x, "BETWEEN") {
                between = true;
            } else if kw(x, "AND") && between {
                between = false;
            } else if (comma && x.0 == ',') || (!comma && kw(x, "AND")) {
                if start == i {
                    return Err("empty join expression".into());
                }
                out.push(&t[start..i]);
                start = i + 1;
            }
        }
    }
    if depth != 0 || start == t.len() {
        return Err("incomplete join expression".into());
    }
    out.push(&t[start..]);
    Ok(out)
}
fn reference(t: &[Token], areas: &[(String, usize)]) -> Result<Ref, String> {
    let (area, field) = match t {
        [a, dot, f] if dot.0 == '.' && matches!(a.0, 'w' | '"') && matches!(f.0, 'w' | '"') => (
            areas
                .iter()
                .find(|(n, _)| n == &a.1)
                .map(|(_, i)| *i)
                .ok_or("unknown join qualifier")?,
            f.1.clone(),
        ),
        [f] if matches!(f.0, 'w' | '"') => (0, f.1.clone()),
        _ => return Err("join field reference expected".into()),
    };
    Ok(Ref { area, field })
}
fn local_areas(t: &[Token], areas: &[(String, usize)]) -> Result<BTreeSet<usize>, String> {
    let mut result = BTreeSet::new();
    let mut depth = 0i32;
    let mut nested = None;
    for (i, x) in t.iter().enumerate() {
        if x.0 == '(' {
            depth += 1;
            if t.get(i + 1).is_some_and(|v| kw(v, "SELECT")) {
                nested = Some(depth);
            }
        }
        if x.0 == ')' {
            if nested == Some(depth) {
                nested = None;
            }
            depth -= 1;
        }
        if nested.is_none() && t.get(i + 1).is_some_and(|v| v.0 == '.') {
            result.insert(
                areas
                    .iter()
                    .find(|(a, _)| a == &x.1)
                    .map(|(_, i)| *i)
                    .ok_or("unknown join qualifier")?,
            );
        }
    }
    Ok(result)
}
fn conditions(
    t: &[Token],
    areas: &[String],
    aliases: &[(String, usize)],
    local: &mut [Vec<String>],
    edges: &mut Vec<(Ref, Ref)>,
) -> Result<(), String> {
    let t = strip(t);
    let parts = split(t, false)?;
    if parts.len() > 1 {
        for p in parts {
            conditions(p, areas, aliases, local, edges)?;
        }
        return Ok(());
    }
    if t.len() == 7 && t[3].0 == '=' {
        if let (Ok(a), Ok(b)) = (reference(&t[..3], aliases), reference(&t[4..], aliases)) {
            if a.area != b.area {
                edges.push((a, b));
                return Ok(());
            }
        }
    }
    let names = local_areas(t, aliases)?;
    if names.len() > 1 {
        return Err("join predicates across areas require an equality joined with AND".into());
    }
    let area = names.first().copied().unwrap_or(0);
    let mut local_tokens = Vec::new();
    let mut at = 0;
    while at < t.len() {
        if t.get(at + 1).is_some_and(|v| v.0 == '.') && aliases.iter().any(|(n, _)| n == &t[at].1) {
            at += 2;
        } else {
            local_tokens.push(t[at].clone());
            at += 1;
        }
    }
    let condition = text(&local_tokens);
    sql::parse(&format!(
        "SELECT OID FROM {} WHERE {condition}",
        quoted(&areas[area])
    ))?;
    local[area].push(format!("({condition})"));
    Ok(())
}
pub fn parse(sql_text: &str) -> Result<Option<Join>, String> {
    let tokens = sql::tokenize(sql_text)?;
    let t = if tokens.last().is_some_and(|t| t.0 == ';') {
        &tokens[..tokens.len() - 1]
    } else {
        &tokens
    };
    if t.iter().any(|t| t.0 == ';') {
        return Err("multiple SQL statements are not supported".into());
    }
    let mut depth = 0;
    let mut from = None;
    let mut where_at = None;
    let mut order = None;
    for (i, x) in t.iter().enumerate() {
        if x.0 == '(' {
            depth += 1;
        } else if x.0 == ')' {
            depth -= 1;
        }
        if depth == 0 {
            if kw(x, "FROM") {
                from = Some(i);
            } else if kw(x, "WHERE") {
                where_at = Some(i);
            } else if kw(x, "ORDER") {
                order = Some(i);
            }
        }
    }
    let Some(from) = from else { return Ok(None) };
    if where_at.is_some_and(|at| at <= from)
        || order.is_some_and(|at| at <= from || where_at.is_some_and(|w| at <= w))
    {
        return Err("invalid SQL clause order".into());
    }
    let end_from = where_at.or(order).unwrap_or(t.len());
    if !t[from + 1..end_from].iter().any(|t| t.0 == ',') {
        return Ok(None);
    }
    if !t.first().is_some_and(|t| kw(t, "SELECT")) {
        return Err("join requires SELECT".into());
    }
    let table_tokens = split(&t[from + 1..end_from], true)?;
    if !(2..=4).contains(&table_tokens.len()) {
        return Err("joins support two to four areas".into());
    }
    let mut areas = Vec::new();
    let mut aliases = Vec::new();
    for part in table_tokens {
        let (area, alias) = match part {
            [a] if matches!(a.0, 'w' | '"') => (&a.1, &a.1),
            [a, as_word, b]
                if matches!(a.0, 'w' | '"') && kw(as_word, "AS") && matches!(b.0, 'w' | '"') =>
            {
                (&a.1, &b.1)
            }
            _ => return Err("named area with optional AS alias expected".into()),
        };
        if aliases.iter().any(|(a, _)| a == alias) {
            return Err("duplicate join alias".into());
        }
        aliases.push((alias.clone(), areas.len()));
        areas.push(area.clone());
    }
    // Native report scripts can retain the unaliased table name for the first
    // instance of a self join. Explicit aliases still take precedence.
    for (i, name) in areas.iter().enumerate() {
        if !aliases.iter().any(|(a, _)| a == name) {
            aliases.push((name.clone(), i));
        }
    }
    let mut columns = Vec::new();
    for part in split(&t[1..from], true)? {
        let annotation = part.first().is_some_and(|t| kw(t, "ANNOTATION"))
            && part.get(1).is_some_and(|t| t.0 == '(')
            && part.last().is_some_and(|t| t.0 == ')');
        let r = reference(
            if annotation {
                &part[2..part.len() - 1]
            } else {
                part
            },
            &aliases,
        )?;
        if annotation && !r.field.eq_ignore_ascii_case("OBJECT") {
            return Err("ANNOTATION requires OBJECT".into());
        }
        let q = sql::parse(&format!(
            "SELECT {} FROM {}",
            quoted(&r.field),
            quoted(&areas[r.area])
        ))?;
        // Quoted OBJECT/OID are still identifiers, so identify these explicitly.
        let column = if annotation {
            Column::Annotation
        } else if r.field.eq_ignore_ascii_case("OBJECT") {
            Column::Object
        } else if r.field.eq_ignore_ascii_case("OID") {
            Column::Oid
        } else {
            q.projection.0[0].clone()
        };
        columns.push((r.area, column));
    }
    if columns.len() > 64 {
        return Err("SQL column limit".into());
    }
    let mut local = vec![Vec::new(); areas.len()];
    let mut edges = Vec::new();
    let start = where_at.ok_or("join requires WHERE and a link equality")?;
    conditions(
        &t[start + 1..order.unwrap_or(t.len())],
        &areas,
        &aliases,
        &mut local,
        &mut edges,
    )?;
    let mut ordering = Vec::new();
    if let Some(at) = order {
        if !t.get(at + 1).is_some_and(|t| kw(t, "BY")) {
            return Err("SQL ORDER requires BY".into());
        }
        for mut part in split(&t[at + 2..], true)? {
            let desc = part.last().is_some_and(|t| kw(t, "DESC"));
            if part.last().is_some_and(|t| kw(t, "ASC") || kw(t, "DESC")) {
                part = &part[..part.len() - 1];
            }
            ordering.push((reference(part, &aliases)?, desc));
        }
    }
    Ok(Some(Join {
        areas,
        columns,
        local: local.into_iter().map(|p| p.join(" AND ")).collect(),
        edges,
        order: ordering,
    }))
}
impl Join {
    pub fn signature(&self) -> sql::Select {
        sql::Select {
            area: self.areas[0].clone(),
            projection: Projection(self.columns.iter().map(|(_, c)| c.clone()).collect()),
            predicate: sql::Predicate::All,
            order: Vec::new(),
        }
    }
    fn query(&self, area: usize, extra: Option<String>, columns: &str) -> String {
        let mut conditions = Vec::new();
        if !self.local[area].is_empty() {
            conditions.push(self.local[area].clone());
        }
        if let Some(extra) = extra {
            conditions.push(extra);
        }
        format!(
            "SELECT {columns} FROM {}{}",
            quoted(&self.areas[area]),
            if conditions.is_empty() {
                String::new()
            } else {
                format!(" WHERE {}", conditions.join(" AND "))
            }
        )
    }
    pub fn execute<H: DatabaseHost + ?Sized>(
        &self,
        host: &H,
        scope: u32,
    ) -> Result<QueryData, String> {
        let schema = host.shared_schema(scope)?;
        let areas = self
            .areas
            .iter()
            .map(|name| schema.area(name).ok_or("join area absent"))
            .collect::<Result<Vec<_>, _>>()?;
        for (a, b) in &self.edges {
            validate_edge(a, b, &areas)?;
        }
        let cost = |i| -> Result<u32, String> {
            let q = sql::parse(&self.query(i, None, "OID"))?;
            let p = q.predicate.without_subqueries().bind(areas[i])?;
            // A bounded date interval is a better driver than a common category.
            let bounds = self.local[i].matches(" >= ").count()
                + self.local[i].matches(" <= ").count()
                + self.local[i].matches(" < ").count()
                + self.local[i].matches(" > ").count();
            Ok(
                if p.index_cost() > 0 && (bounds >= 2 || self.local[i].contains(" BETWEEN ")) {
                    1
                } else {
                    p.index_cost()
                },
            )
        };
        let costs = (0..areas.len()).map(cost).collect::<Result<Vec<_>, _>>()?;
        let driver = (0..areas.len()).min_by_key(|i| costs[*i]).unwrap();
        let ids = host.query(scope, &self.query(driver, None, "OID"))?;
        if ids.len() > sql::MAX_SQL_ROWS {
            return Err("SQL join driver limit".into());
        }
        let mut records = vec![BTreeMap::new(); areas.len()];
        let mut pairs = BTreeSet::new();
        for id in ids {
            let record = host.read(scope, id)?.ok_or("join driver record missing")?;
            records[driver].insert(id, self.compact(record, driver, areas[driver]));
            let mut pair = vec![0; areas.len()];
            pair[driver] = id;
            pairs.insert(pair);
        }
        let mut visited = BTreeSet::from([driver]);
        let mut link_count = 0usize;
        while visited.len() < areas.len() {
            let edge = self
                .edges
                .iter()
                .filter_map(|(a, b)| {
                    if visited.contains(&a.area) && !visited.contains(&b.area) {
                        Some((a, b))
                    } else if visited.contains(&b.area) && !visited.contains(&a.area) {
                        Some((b, a))
                    } else {
                        None
                    }
                })
                .min_by_key(|(_, other)| costs[other.area]);
            let Some((dr, ot)) = edge else {
                let other = (0..areas.len())
                    .filter(|i| !visited.contains(i))
                    .min_by_key(|i| costs[*i])
                    .unwrap();
                let ids = if pairs.is_empty() {
                    vec![]
                } else {
                    host.query(scope, &self.query(other, None, "OID"))?
                };
                if pairs
                    .len()
                    .checked_mul(ids.len())
                    .map_or(true, |n| n > sql::MAX_SQL_ROWS)
                {
                    return Err("SQL cross product row limit".into());
                }
                for id in &ids {
                    let r = host.read(scope, *id)?.ok_or("join record missing")?;
                    records[other].insert(*id, self.compact(r, other, areas[other]));
                }
                pairs = pairs
                    .into_iter()
                    .flat_map(|pair| {
                        ids.iter().map(move |id| {
                            let mut p = pair.clone();
                            p[other] = *id;
                            p
                        })
                    })
                    .collect();
                visited.insert(other);
                continue;
            };
            let other = ot.area;
            let rows = pairs.into_iter().collect::<Vec<_>>();
            let mut by_key = BTreeMap::<u32, Vec<usize>>::new();
            for (index, pair) in rows.iter().enumerate() {
                let id = pair[dr.area];
                for key in links(id, &records[dr.area][&id], dr, areas[dr.area])? {
                    link_count += 1;
                    if link_count > sql::MAX_INDEX_CANDIDATES {
                        return Err("SQL join link limit".into());
                    }
                    by_key.entry(key).or_default().push(index);
                }
            }
            let keys = by_key.keys().copied().collect::<Vec<_>>();
            let mut others = BTreeSet::new();
            for chunk in keys.chunks(128) {
                let extra = format!(
                    "{} IN ({})",
                    quoted(&ot.field),
                    chunk
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                );
                others.extend(host.query(scope, &self.query(other, Some(extra), "OID"))?);
                if others.len() > sql::MAX_SQL_ROWS {
                    return Err("SQL join candidate limit".into());
                }
            }
            pairs = BTreeSet::new();
            for id in others {
                let record = host.read(scope, id)?.ok_or("join record missing")?;
                for key in links(id, &record, ot, areas[other])? {
                    if let Some(drivers) = by_key.get(&key) {
                        for index in drivers {
                            let mut pair = rows[*index].clone();
                            pair[other] = id;
                            pairs.insert(pair);
                            if pairs.len() > sql::MAX_SQL_ROWS {
                                return Err("SQL join row limit".into());
                            }
                        }
                    }
                }
                records[other].insert(id, self.compact(record, other, areas[other]));
            }
            visited.insert(other);
        }
        let mut pairs = pairs
            .into_iter()
            .filter(|pair| {
                self.edges.iter().all(|(a, b)| {
                    let av = links(
                        pair[a.area],
                        &records[a.area][&pair[a.area]],
                        a,
                        areas[a.area],
                    )
                    .unwrap_or_default();
                    let bv = links(
                        pair[b.area],
                        &records[b.area][&pair[b.area]],
                        b,
                        areas[b.area],
                    )
                    .unwrap_or_default();
                    av.iter().any(|id| bv.contains(id))
                })
            })
            .collect::<Vec<_>>();
        for (r, _) in &self.order {
            if !r.field.eq_ignore_ascii_case("OID")
                && !areas[r.area]
                    .fields
                    .values()
                    .any(|f| f.name == r.field && matches!(f.kind, 1 | 2 | 3 | 5 | 12))
            {
                return Err("join sort field absent or unsupported".into());
            }
        }
        pairs.sort_by(|a, b| {
            for (r, desc) in &self.order {
                let c = sort_key(a[r.area], &records[r.area][&a[r.area]], r, areas[r.area]).cmp(
                    &sort_key(b[r.area], &records[r.area][&b[r.area]], r, areas[r.area]),
                );
                if !c.is_eq() {
                    return if *desc { c.reverse() } else { c };
                }
            }
            // Native joins retain the driving area's record order within an
            // equal ORDER BY group, including when it is not the first table.
            a[driver].cmp(&b[driver]).then_with(|| a.cmp(b))
        });
        let mut projected = vec![BTreeMap::new(); areas.len()];
        for area in 0..areas.len() {
            let needed = self
                .columns
                .iter()
                .filter(|(a, _)| *a == area)
                .map(|(_, c)| c.clone())
                .collect::<Vec<_>>();
            if !needed.iter().any(|c| matches!(c, Column::Field(_))) {
                continue;
            }
            let columns = needed
                .iter()
                .map(|c| match c {
                    Column::Object => "OBJECT".into(),
                    Column::Annotation => "ANNOTATION(OBJECT)".into(),
                    Column::Oid => "OID".into(),
                    Column::Field(n) => quoted(n),
                    _ => unreachable!(),
                })
                .collect::<Vec<_>>()
                .join(",");
            let ids = pairs
                .iter()
                .map(|p| p[area])
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            for chunk in ids.chunks(128) {
                let q = format!(
                    "SELECT {columns} FROM {} WHERE OID IN ({})",
                    quoted(&self.areas[area]),
                    chunk
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                );
                let data = host.select(scope, &q)?;
                for (id, row) in data.oids.into_iter().zip(data.cells) {
                    projected[area].insert(id, row);
                }
            }
        }
        let mut cells = Vec::new();
        let mut bytes = 0usize;
        for pair in &pairs {
            let mut positions = vec![0usize; areas.len()];
            let mut row = Vec::new();
            for (area, c) in &self.columns {
                let value = match c {
                    Column::Object | Column::Oid | Column::Annotation => {
                        ColumnValues::IdentityOid(pair[*area])
                    }
                    Column::Field(_) => projected[*area]
                        .get(&pair[*area])
                        .and_then(|r| r.get(positions[*area]))
                        .ok_or("join projection missing")?
                        .clone(),
                    _ => unreachable!(),
                };
                positions[*area] += 1;
                bytes += cell_bytes(&value);
                if bytes > 4 * 1024 * 1024 {
                    return Err("SQL join result byte limit".into());
                }
                row.push(value);
            }
            cells.push(row);
        }
        Ok(QueryData {
            area: Some(areas[0].id),
            oids: pairs.iter().map(|p| p[0]).collect(),
            cells,
            fulltext: Default::default(),
        })
    }
    fn compact(&self, record: Record, area: usize, schema: &Area) -> Record {
        let names = self
            .edges
            .iter()
            .flat_map(|(a, b)| [a, b])
            .chain(self.order.iter().map(|(r, _)| r))
            .filter(|r| r.area == area)
            .map(|r| r.field.as_str())
            .collect::<BTreeSet<_>>();
        let ids = schema
            .fields
            .values()
            .filter(|f| names.contains(f.name.as_str()))
            .map(|f| f.id)
            .collect::<BTreeSet<_>>();
        Record {
            area: record.area,
            flags: record.flags,
            fields: record
                .fields
                .iter()
                .filter(|f| ids.contains(&f.id))
                .cloned()
                .collect::<Vec<_>>()
                .into(),
        }
    }
}
fn validate_edge(a: &Ref, b: &Ref, areas: &[&Area]) -> Result<(), String> {
    let aid = a.field.eq_ignore_ascii_case("OID");
    let bid = b.field.eq_ignore_ascii_case("OID");
    if aid == bid {
        return Err("join equality requires one OID and one link field".into());
    }
    let r = if aid { b } else { a };
    if !areas[r.area]
        .fields
        .values()
        .any(|f| f.name == r.field && f.kind == 5)
    {
        return Err("join field is not a link".into());
    }
    Ok(())
}
fn links(id: u32, record: &Record, r: &Ref, area: &Area) -> Result<Vec<u32>, String> {
    if r.field.eq_ignore_ascii_case("OID") {
        return Ok(vec![id]);
    }
    let field = area
        .fields
        .values()
        .find(|f| f.name == r.field && f.kind == 5)
        .ok_or("join link absent")?;
    Ok(record
        .fields
        .iter()
        .filter(|f| f.id == field.id && f.wire_type == 5)
        .filter_map(|f| {
            if let Value::Integer(id) = f.value {
                Some(id)
            } else {
                None
            }
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Missing,
    Number(u32),
    Numeric(u64),
    Text(Vec<u8>),
    Date(i16, u8, u8),
    Stamp([u8; 8]),
}
fn sort_key(id: u32, record: &Record, r: &Ref, area: &Area) -> Key {
    if r.field.eq_ignore_ascii_case("OID") {
        return Key::Number(id);
    }
    let Some(f) = area.fields.values().find(|f| f.name == r.field) else {
        return Key::Missing;
    };
    match record
        .fields
        .iter()
        .find(|v| v.id == f.id)
        .map(|f| &f.value)
    {
        Some(Value::Integer(n)) => Key::Number(*n),
        Some(Value::String(b)) => {
            let text = crate::db::decode(b).to_lowercase();
            if f.kind == 3 {
                if let Some(key) = sql::numeric_sort_key(&text) {
                    return Key::Numeric(key);
                }
            }
            Key::Text(encoding_rs::WINDOWS_1251.encode(&text).0.into_owned())
        }
        Some(Value::Date(d)) => Key::Date(i16::from_le_bytes([d[0], d[1]]), d[2], d[3]),
        Some(Value::Timestamp(t)) => Key::Stamp(*t),
        _ => Key::Missing,
    }
}
fn cell_bytes(c: &ColumnValues) -> usize {
    match c {
        ColumnValues::Identity | ColumnValues::IdentityOid(_) => 4,
        ColumnValues::Strings(v) => v.iter().map(String::len).sum(),
        ColumnValues::Links(v) => v.len() * 4,
        ColumnValues::Dates(v) => v.len() * 8,
        ColumnValues::Decimals(v) => v.len() * 34,
        ColumnValues::StreamRef { .. } => 8,
    }
}
