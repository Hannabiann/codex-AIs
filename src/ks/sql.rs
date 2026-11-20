//! Parsed, bounded read-only SELECT subset. Never accepts an unparsed suffix.
use super::host::{Column, Projection};
use std::sync::Arc;

pub const MAX_SQL_ROWS: usize = 1_000_000;
// The stock Word template opens a document with over ninety requested
// attributes in addition to the protocol's fixed metadata columns.
pub(super) const MAX_COLUMNS: usize = 1024;
pub const MAX_INDEX_CANDIDATES: usize = 8_000_000;
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Comparison {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}
impl Comparison {
    pub fn accepts(self, c: std::cmp::Ordering) -> bool {
        match self {
            Self::Eq => c.is_eq(),
            Self::Ne => !c.is_eq(),
            Self::Lt => c.is_lt(),
            Self::Le => !c.is_gt(),
            Self::Gt => c.is_gt(),
            Self::Ge => !c.is_lt(),
        }
    }
}
pub fn numeric_cmp(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    match (a.trim().parse::<f64>(), b.trim().parse::<f64>()) {
        (Ok(a), Ok(b)) if a.is_finite() && b.is_finite() => a.partial_cmp(&b),
        _ => Some(a.cmp(b)),
    }
}
/// Monotonic key for the numeric strings used by native type 3 fields.
/// Canonicalizing signed zero keeps equality consistent with numeric predicates.
pub fn numeric_sort_key(text: &str) -> Option<u64> {
    let mut number = text.trim().parse::<f64>().ok()?;
    if !number.is_finite() {
        return None;
    }
    if number == 0.0 {
        number = 0.0;
    }
    let bits = number.to_bits();
    Some(if bits >> 63 == 0 {
        bits ^ (1 << 63)
    } else {
        !bits
    })
}
fn canonical_date(s: &str, kind: u16) -> Result<String, String> {
    let parts = s
        .split(|c: char| !c.is_ascii_digit())
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "SQL date syntax")?;
    if !(parts.len() == 3 && kind == 1 || (6..=7).contains(&parts.len()) && kind == 12) {
        return Err("SQL date parts".into());
    }
    let (y, m, d) = (parts[0], parts[1], parts[2]);
    let max = match m {
        4 | 6 | 9 | 11 => 30,
        2 if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) => 29,
        2 => 28,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => 0,
    };
    if y > 32767 || d == 0 || d > max {
        return Err("invalid SQL calendar date".into());
    }
    let date = format!("{y:04}-{m:02}-{d:02}");
    if kind == 1 {
        return Ok(date);
    }
    if parts[3] > 23 || parts[4] > 59 || parts[5] > 59 || parts.get(6).copied().unwrap_or(0) > 999 {
        return Err("invalid SQL time".into());
    }
    Ok(format!(
        "{date} {:02}:{:02}:{:02}.{:03}",
        parts[3],
        parts[4],
        parts[5],
        parts.get(6).copied().unwrap_or(0)
    ))
}
#[derive(Clone, Debug)]
pub enum Predicate {
    None,
    Contains(String),
    FreeText(String),
    Compare {
        field: String,
        op: Comparison,
        value: String,
        kind: Option<u16>,
    },
    InQuery(String, Box<Select>),
    Set {
        field: String,
        ids: Arc<Vec<u32>>,
    },
    Not(Box<Predicate>),
    All,
    IsNull {
        field: String,
        negated: bool,
    },
    Link {
        field: String,
        oid: u32,
    },
    Text {
        field: String,
        value: String,
        like: bool,
    },
    And(Box<Predicate>, Box<Predicate>),
    Between(Box<Predicate>, Box<Predicate>),
    Or(Box<Predicate>, Box<Predicate>),
}
/// Resolve SQL names once against DataBase.Struct, then use numeric IDs in
/// every volume. Older volumes may retain an earlier name for the same field.
#[derive(Clone, Debug)]
pub enum BoundPredicate {
    None,
    Between {
        attribute: u16,
        kind: u16,
        lower: String,
        upper: String,
    },
    Compare {
        attribute: u16,
        kind: u16,
        op: Comparison,
        value: String,
    },
    Not(Box<BoundPredicate>),
    Oid(u32),
    Set {
        attribute: Option<u16>,
        ids: Arc<Vec<u32>>,
    },
    All,
    IsNull {
        attribute: u16,
        kind: u16,
        negated: bool,
    },
    Link {
        attribute: u16,
        oid: u32,
    },
    Text {
        attribute: u16,
        kind: u16,
        pattern: Matcher,
        folded: String,
        like: bool,
    },
    And(Box<BoundPredicate>, Box<BoundPredicate>),
    Or(Box<BoundPredicate>, Box<BoundPredicate>),
}
impl BoundPredicate {
    /// Exact OID restrictions already resolved in this database context need
    /// only the winning mounted record, not an OID lookup in every volume.
    pub fn oid_candidates(&self) -> Option<Vec<u32>> {
        match self {
            Self::None => Some(vec![]),
            Self::Oid(id) => Some(vec![*id]),
            Self::Set {
                attribute: None,
                ids,
            } => Some(ids.as_ref().clone()),
            Self::And(a, b) => match (a.oid_candidates(), b.oid_candidates()) {
                (Some(a), Some(b)) => Some(if a.len() <= b.len() { a } else { b }),
                (a, b) => a.or(b),
            },
            Self::Or(a, b) => {
                let mut a = a.oid_candidates()?;
                a.extend(b.oid_candidates()?);
                a.sort_unstable();
                a.dedup();
                Some(a)
            }
            _ => None,
        }
    }
    /// Prefer an exact identity to an attribute scan, regardless of SQL order.
    pub fn index_cost(&self) -> u32 {
        match self {
            Self::None | Self::Oid(_) => 0,
            Self::Set {
                attribute: None, ..
            } => 0,
            Self::Set { ids, .. } => {
                if ids.len() > 256 {
                    100000
                } else if ids.len() > 32 {
                    50
                } else if ids.len() > 8 {
                    6
                } else {
                    2
                }
            }
            Self::Compare {
                kind: 1 | 12,
                op: Comparison::Eq,
                ..
            } => 1,
            Self::Link { .. } => 2,
            Self::Text {
                kind: 2 | 3,
                like: false,
                folded,
                ..
            } => {
                if folded.len() > 2 {
                    1
                } else {
                    4
                }
            }
            Self::Text { kind: 2 | 3, .. } => 3,
            Self::Compare { kind: 1 | 12, .. } => 5,
            Self::Between { kind: 1 | 12, .. } => 1,
            Self::And(a, b) => a.index_cost().min(b.index_cost()),
            Self::Or(a, b) => a.index_cost().saturating_add(b.index_cost()),
            _ => 100000,
        }
    }
    pub fn matches_record(&self, oid: u32, r: &crate::db::fields::Record) -> bool {
        use crate::db::fields::Value as F;
        match self {
            Self::None => false,
            Self::Between {
                attribute,
                kind,
                lower,
                upper,
            } => r.fields.iter().any(|f| {
                f.id == *attribute
                    && f.wire_type == *kind
                    && comparison_matches(f, *kind, Comparison::Ge, lower)
                    && comparison_matches(f, *kind, Comparison::Le, upper)
            }),
            Self::Compare {
                attribute,
                kind,
                op,
                value,
            } => r
                .fields
                .iter()
                .filter(|f| f.id == *attribute && f.wire_type == *kind)
                .any(|f| comparison_matches(f, *kind, *op, value)),
            Self::Not(p) => !p.matches_record(oid, r),
            Self::All => true,
            Self::Oid(expected) => oid == *expected,
            Self::Set {
                attribute: None,
                ids,
            } => ids.binary_search(&oid).is_ok(),
            Self::Set {
                attribute: Some(attribute),
                ids,
            } => r.fields.iter().any(|f| {
                f.id == *attribute
                    && f.wire_type == 5
                    && matches!(f.value, F::Integer(v) if ids.binary_search(&v).is_ok())
            }),
            Self::And(a, b) => a.matches_record(oid, r) && b.matches_record(oid, r),
            Self::Or(a, b) => a.matches_record(oid, r) || b.matches_record(oid, r),
            Self::IsNull {
                attribute,
                kind,
                negated,
            } => {
                r.fields
                    .iter()
                    .any(|f| f.id == *attribute && f.wire_type == *kind)
                    == *negated
            }
            Self::Link { attribute, oid } => r.fields.iter().any(|f| {
                f.id == *attribute && f.wire_type == 5 && matches!(f.value,F::Integer(v) if v==*oid)
            }),
            Self::Text {
                attribute, pattern, ..
            } => r.fields.iter().any(|f| {
                f.id == *attribute
                    && matches!(&f.value,F::String(v) if pattern.is_match(&crate::db::decode(v)))
            }),
        }
    }
}
fn comparison_matches(
    f: &crate::db::fields::Field,
    kind: u16,
    op: Comparison,
    value: &str,
) -> bool {
    use crate::db::fields::Value as F;
    if let F::Decimal(bytes) = &f.value {
        return super::runtime::decimal::Decimal::from_native(bytes)
            .and_then(|a| a.cmp(super::runtime::decimal::Decimal::parse(value)?))
            .is_ok_and(|c| op.accepts(c));
    }
    let text = match &f.value {
        F::String(v) => crate::db::decode(v),
        F::Date(v) => format!(
            "{:04}-{:02}-{:02}",
            i16::from_le_bytes([v[0], v[1]]),
            v[2],
            v[3]
        ),
        F::Timestamp(v) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
            u16::from_be_bytes([v[0], v[1]]),
            v[2],
            v[3],
            v[4],
            v[5],
            v[6],
            v[7] as u16 * 10
        ),
        _ => return false,
    };
    let order = if kind == 3 {
        numeric_cmp(&text, value)
    } else {
        Some(text.as_str().cmp(value))
    };
    order.is_some_and(|order| op.accepts(order))
}

impl Predicate {
    pub fn has_context(&self) -> bool {
        match self {
            Self::Contains(_) | Self::FreeText(_) => true,
            Self::And(a, b) | Self::Or(a, b) => a.has_context() || b.has_context(),
            Self::Not(p) => p.has_context(),
            _ => false,
        }
    }
    pub fn without_context(&self) -> Self {
        match self {
            Self::Contains(_) | Self::FreeText(_) => Self::All,
            Self::Not(p) if p.has_context() => Self::All,
            Self::And(a, b) => {
                Self::And(Box::new(a.without_context()), Box::new(b.without_context()))
            }
            Self::Or(a, b) => {
                Self::Or(Box::new(a.without_context()), Box::new(b.without_context()))
            }
            p => p.clone(),
        }
    }
    pub fn resolve_context(
        &self,
        query: &dyn Fn(&str, bool) -> Result<Vec<u32>, String>,
    ) -> Result<Self, String> {
        Ok(match self {
            Self::Contains(s) => Self::oid_set("OID", &query(s, false)?),
            Self::FreeText(s) => Self::oid_set("OID", &query(s, true)?),
            Self::And(a, b) => Self::And(
                Box::new(a.resolve_context(query)?),
                Box::new(b.resolve_context(query)?),
            ),
            Self::Or(a, b) => Self::Or(
                Box::new(a.resolve_context(query)?),
                Box::new(b.resolve_context(query)?),
            ),
            Self::Not(p) => Self::Not(Box::new(p.resolve_context(query)?)),
            p => p.clone(),
        })
    }
    pub fn oid_set(field: &str, ids: &[u32]) -> Self {
        match ids.len() {
            0 => Self::None,
            1 => Self::Link {
                field: field.into(),
                oid: ids[0],
            },
            _ => {
                let mut ids = ids.to_vec();
                ids.sort_unstable();
                ids.dedup();
                Self::Set {
                    field: field.into(),
                    ids: Arc::new(ids),
                }
            }
        }
    }
    pub fn has_subquery(&self) -> bool {
        match self {
            Self::InQuery(..) => true,
            Self::And(a, b) | Self::Or(a, b) => a.has_subquery() || b.has_subquery(),
            Self::Not(p) => p.has_subquery(),
            _ => false,
        }
    }
    /// A superset used only to obtain a bounded driving set for a semijoin.
    pub fn without_subqueries(&self) -> Self {
        match self {
            Self::InQuery(..) => Self::All,
            Self::Not(p) if p.has_subquery() => Self::All,
            Self::And(a, b) => Self::And(
                Box::new(a.without_subqueries()),
                Box::new(b.without_subqueries()),
            ),
            Self::Or(a, b) => Self::Or(
                Box::new(a.without_subqueries()),
                Box::new(b.without_subqueries()),
            ),
            p => p.clone(),
        }
    }
    pub fn resolve(
        &self,
        query: &dyn Fn(&str, &Select) -> Result<Vec<u32>, String>,
    ) -> Result<Self, String> {
        Ok(match self {
            Self::InQuery(field, nested) => {
                let mut ids = query(field, nested)?;
                ids.sort_unstable();
                ids.dedup();
                if ids.len() > MAX_SQL_ROWS {
                    return Err("SQL subquery row limit".into());
                }
                Self::oid_set(field, &ids)
            }
            Self::And(a, b) => Self::And(Box::new(a.resolve(query)?), Box::new(b.resolve(query)?)),
            Self::Or(a, b) => Self::Or(Box::new(a.resolve(query)?), Box::new(b.resolve(query)?)),
            Self::Not(p) => Self::Not(Box::new(p.resolve(query)?)),
            p => p.clone(),
        })
    }
    pub fn bind(&self, area: &crate::db::schema::Area) -> Result<BoundPredicate, String> {
        Ok(match self {
            Self::None => BoundPredicate::None,
            Self::Set { field, ids } => {
                let attribute = if field.eq_ignore_ascii_case("OID") {
                    None
                } else {
                    match (Self::Link {
                        field: field.clone(),
                        oid: 0,
                    })
                    .bind(area)?
                    {
                        BoundPredicate::Link { attribute, .. } => Some(attribute),
                        _ => return Err("invalid SQL set field".into()),
                    }
                };
                BoundPredicate::Set {
                    attribute,
                    ids: ids.clone(),
                }
            }
            Self::Contains(_) | Self::FreeText(_) => {
                return Err("context search must be resolved before binding".into())
            }
            Self::Compare {
                field,
                op,
                value,
                kind,
            } => {
                let f = area
                    .fields
                    .values()
                    .find(|f| f.name == *field)
                    .ok_or_else(|| format!("SQL field {field} absent"))?;
                if !matches!(f.kind, 1 | 2 | 3 | 12 | 13)
                    || kind.is_some_and(|k| {
                        k != f.kind && !(matches!(k, 1 | 12) && matches!(f.kind, 1 | 12))
                    })
                {
                    return Err(format!(
                        "SQL comparison type mismatch for {field}: field={}, literal={kind:?}",
                        f.kind
                    ));
                }
                let value = match (f.kind, *kind) {
                    (1, Some(12)) => value
                        .get(..10)
                        .ok_or("SQL timestamp date missing")?
                        .to_string(),
                    (12, Some(1)) => format!("{value} 00:00:00.000"),
                    _ => value.clone(),
                };
                BoundPredicate::Compare {
                    attribute: f.id,
                    kind: f.kind,
                    op: *op,
                    value: value.clone(),
                }
            }
            Self::InQuery(..) => return Err("SQL subquery must be resolved before binding".into()),
            Self::Not(p) => BoundPredicate::Not(Box::new(p.bind(area)?)),
            Self::All => BoundPredicate::All,
            Self::IsNull { field, negated } => {
                let f = area
                    .fields
                    .values()
                    .find(|f| f.name == *field)
                    .ok_or_else(|| format!("SQL field {field} absent from area {}", area.name))?;
                BoundPredicate::IsNull {
                    attribute: f.id,
                    kind: f.kind,
                    negated: *negated,
                }
            }
            Self::Link { field, oid } => {
                if field.eq_ignore_ascii_case("OID") {
                    return Ok(BoundPredicate::Oid(*oid));
                }
                let f = area
                    .fields
                    .values()
                    .find(|f| f.name == *field)
                    .ok_or_else(|| format!("SQL field {field} absent from area {}", area.name))?;
                if f.kind != 5 {
                    return Err(format!(
                        "SQL OID equality requires a link field: {field}, native type {}",
                        f.kind
                    ));
                }
                BoundPredicate::Link {
                    attribute: f.id,
                    oid: *oid,
                }
            }
            Self::And(a, b) => {
                BoundPredicate::And(Box::new(a.bind(area)?), Box::new(b.bind(area)?))
            }
            Self::Between(a, b) => {
                let BoundPredicate::Compare {
                    attribute,
                    kind,
                    value: lower,
                    ..
                } = a.bind(area)?
                else {
                    return Err("SQL BETWEEN lower comparison required".into());
                };
                let BoundPredicate::Compare {
                    attribute: other,
                    kind: other_kind,
                    value: upper,
                    ..
                } = b.bind(area)?
                else {
                    return Err("SQL BETWEEN upper comparison required".into());
                };
                if attribute != other || kind != other_kind {
                    return Err("SQL BETWEEN bound types differ".into());
                }
                BoundPredicate::Between {
                    attribute,
                    kind,
                    lower,
                    upper,
                }
            }
            Self::Or(a, b) => BoundPredicate::Or(Box::new(a.bind(area)?), Box::new(b.bind(area)?)),
            Self::Text { field, value, like } => {
                if field.eq_ignore_ascii_case("OID") && !like {
                    return Ok(BoundPredicate::Oid(
                        value.parse().map_err(|_| "SQL OID literal invalid")?,
                    ));
                }
                let f = area
                    .fields
                    .values()
                    .find(|f| f.name == *field)
                    .ok_or_else(|| format!("SQL field {field} absent from area {}", area.name))?;
                if !matches!(f.kind, 2 | 3) {
                    return Err(format!(
                        "SQL string index not implemented for field {field}, native type {}",
                        f.kind
                    ));
                }
                BoundPredicate::Text {
                    attribute: f.id,
                    kind: f.kind,
                    pattern: matcher(value, *like)?,
                    folded: value.to_lowercase(),
                    like: *like,
                }
            }
        })
    }
}
#[derive(Clone, Debug)]
pub struct Select {
    pub area: String,
    pub projection: Projection,
    pub predicate: Predicate,
    pub order: Vec<(String, bool)>,
}
impl Select {
    /// Validate even when the WHERE condition produces no rows.
    pub fn fields(
        &self,
        area: &crate::db::schema::Area,
    ) -> Result<Vec<Option<(u16, u16)>>, String> {
        self.projection
            .0
            .iter()
            .map(|column| {
                let Column::Field(name) = column else {
                    return Ok(None);
                };
                let f = area
                    .fields
                    .values()
                    .find(|f| f.name == *name)
                    .ok_or_else(|| format!("SQL field {name} absent from area {}", area.name))?;
                if !matches!(f.kind, 1 | 2 | 3 | 5 | 8 | 12 | 13 | 14) {
                    return Err(format!(
                        "SQL projection for {name}, native type {}, is not implemented",
                        f.kind
                    ));
                }
                Ok(Some((f.id, f.kind)))
            })
            .collect()
    }
}
#[derive(Clone, Debug)]
pub(super) struct Token(pub char, pub String);
struct Parser {
    tokens: Vec<Token>,
    at: usize,
    depth: usize,
}
impl Parser {
    fn kw(&mut self, s: &str) -> bool {
        if self
            .tokens
            .get(self.at)
            .is_some_and(|t| t.0 == 'w' && t.1.eq_ignore_ascii_case(s))
        {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn sym(&mut self, c: char) -> bool {
        if self.tokens.get(self.at).is_some_and(|t| t.0 == c) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn id(&mut self) -> Result<String, String> {
        let t = self.tokens.get(self.at).ok_or("SQL identifier missing")?;
        if !matches!(t.0, 'w' | '"') {
            return Err("SQL identifier expected".into());
        }
        self.at += 1;
        Ok(t.1.clone())
    }
    fn predicate(&mut self, area: &str) -> Result<Predicate, String> {
        let mut p = self.and(area)?;
        while self.kw("OR") {
            p = Predicate::Or(Box::new(p), Box::new(self.and(area)?));
        }
        Ok(p)
    }
    fn and(&mut self, area: &str) -> Result<Predicate, String> {
        let mut p = self.atom(area)?;
        while self.kw("AND") {
            p = Predicate::And(Box::new(p), Box::new(self.atom(area)?));
        }
        Ok(p)
    }
    fn atom(&mut self, area: &str) -> Result<Predicate, String> {
        self.depth += 1;
        if self.depth > 32 {
            return Err("SQL nesting limit".into());
        }
        let p = if self.kw("CONTAINS") {
            if !self.sym('(') {
                return Err("CONTAINS requires parentheses".into());
            }
            if !self.sym('*') {
                if self.id()? != area || !self.sym('.') || !self.sym('*') {
                    return Err("CONTAINS requires the current area's * fields".into());
                }
            }
            if !self.sym(',') {
                return Err("CONTAINS query missing".into());
            }
            let t = self.tokens.get(self.at).ok_or("CONTAINS text missing")?;
            if t.0 != '\'' {
                return Err("CONTAINS expects a string".into());
            }
            let text = t.1.clone();
            self.at += 1;
            if !self.sym(')') {
                return Err("CONTAINS closing parenthesis missing".into());
            }
            Predicate::Contains(text)
        } else if self.kw("FREETEXT") {
            if !self.sym('(') {
                return Err("FREETEXT requires parentheses".into());
            }
            let t = self.tokens.get(self.at).ok_or("FREETEXT text missing")?;
            if t.0 != '\'' {
                return Err("FREETEXT expects a string".into());
            }
            let text = t.1.clone();
            self.at += 1;
            if !self.sym(')') {
                return Err("FREETEXT closing parenthesis missing".into());
            }
            Predicate::FreeText(text)
        } else if self.kw("NOT") {
            Predicate::Not(Box::new(self.atom(area)?))
        } else if self.sym('(') {
            let p = self.predicate(area)?;
            if !self.sym(')') {
                return Err("SQL closing parenthesis missing".into());
            }
            p
        } else {
            let mut field = self.id()?;
            if self.sym('.') {
                if field != area {
                    return Err("SQL qualifier does not match FROM".into());
                }
                field = self.id()?;
            }
            if self.kw("IS") {
                let negated = self.kw("NOT");
                if !self.kw("NULL") {
                    return Err("SQL IS requires NULL or NOT NULL".into());
                }
                self.depth -= 1;
                return Ok(Predicate::IsNull { field, negated });
            }
            let negated = self.kw("NOT");
            if self.kw("IN") {
                if !self.sym('(') {
                    return Err("SQL IN requires a literal list".into());
                }
                if self
                    .tokens
                    .get(self.at)
                    .is_some_and(|t| t.0 == 'w' && t.1.eq_ignore_ascii_case("SELECT"))
                {
                    let query = self.select()?;
                    if !matches!(
                        query.projection.0.as_slice(),
                        [Column::Oid] | [Column::Field(_)]
                    ) {
                        return Err("SQL IN subquery requires a single OID/link column".into());
                    }
                    if !self.sym(')') {
                        return Err("SQL IN subquery closing parenthesis missing".into());
                    }
                    self.depth -= 1;
                    let p = Predicate::InQuery(field, Box::new(query));
                    return Ok(if negated {
                        Predicate::Not(Box::new(p))
                    } else {
                        p
                    });
                }
                let mut values = Vec::new();
                loop {
                    if values.len() >= 10000 {
                        return Err("SQL IN value limit".into());
                    }
                    values.push(self.literal_predicate(field.clone(), false)?);
                    if !self.sym(',') {
                        break;
                    }
                }
                if !self.sym(')') {
                    return Err("SQL IN closing parenthesis missing".into());
                }
                // Build a balanced tree so a bounded IN list cannot exhaust stack depth.
                fn combine(mut p: Vec<Predicate>) -> Predicate {
                    if p.len() == 1 {
                        return p.pop().unwrap();
                    }
                    let right = p.split_off(p.len() / 2);
                    Predicate::Or(Box::new(combine(p)), Box::new(combine(right)))
                }
                self.depth -= 1;
                let p = combine(values);
                return Ok(if negated {
                    Predicate::Not(Box::new(p))
                } else {
                    p
                });
            }
            if negated {
                return Err("SQL NOT requires IN".into());
            }
            if self.kw("BETWEEN") {
                let lower = self.comparison_literal(field.clone(), Comparison::Ge)?;
                if !self.kw("AND") {
                    return Err("SQL BETWEEN requires AND".into());
                }
                let upper = self.comparison_literal(field, Comparison::Le)?;
                self.depth -= 1;
                return Ok(Predicate::Between(Box::new(lower), Box::new(upper)));
            }
            if self.kw("LIKE") {
                self.literal_predicate(field, true)?
            } else {
                let op = if self.sym('=') {
                    Comparison::Eq
                } else if self.sym('<') {
                    if self.sym('=') {
                        Comparison::Le
                    } else if self.sym('>') {
                        Comparison::Ne
                    } else {
                        Comparison::Lt
                    }
                } else if self.sym('>') {
                    if self.sym('=') {
                        Comparison::Ge
                    } else {
                        Comparison::Gt
                    }
                } else {
                    return Err("SQL comparison expected".into());
                };
                self.comparison_literal(field, op)?
            }
        };
        self.depth -= 1;
        Ok(p)
    }
    fn literal_predicate(&mut self, field: String, like: bool) -> Result<Predicate, String> {
        let t = self.tokens.get(self.at).ok_or("SQL literal missing")?;
        self.at += 1;
        Ok(if t.0 == 'n' && !like {
            Predicate::Link {
                field,
                oid: t.1.parse().map_err(|_| "SQL OID outside 0..4294967295")?,
            }
        } else if t.0 == '\'' {
            Predicate::Text {
                field,
                value: t.1.clone(),
                like,
            }
        } else {
            return Err("SQL predicate requires a string literal or link OID".into());
        })
    }
    fn comparison_literal(&mut self, field: String, op: Comparison) -> Result<Predicate, String> {
        if self.sym('{') {
            let kind = if self.kw("d") {
                1
            } else if self.kw("ts") {
                12
            } else {
                return Err("SQL date/timestamp escape expected".into());
            };
            let t = self.tokens.get(self.at).ok_or("SQL date literal missing")?;
            if t.0 != '\'' {
                return Err("SQL date must be quoted".into());
            }
            let value = canonical_date(&t.1, kind)?;
            self.at += 1;
            if !self.sym('}') {
                return Err("SQL date escape closing brace missing".into());
            }
            return Ok(Predicate::Compare {
                field,
                op,
                value,
                kind: Some(kind),
            });
        }
        if matches!(op, Comparison::Eq | Comparison::Ne) {
            let p = self.literal_predicate(field, false)?;
            return Ok(if op == Comparison::Ne {
                Predicate::Not(Box::new(p))
            } else {
                p
            });
        }
        let t = self
            .tokens
            .get(self.at)
            .ok_or("SQL comparison literal missing")?;
        if !matches!(t.0, '\'' | 'n') {
            return Err("SQL comparison literal expected".into());
        }
        let value = t.1.clone();
        self.at += 1;
        Ok(Predicate::Compare {
            field,
            op,
            value,
            kind: None,
        })
    }
}
pub(super) fn tokenize(sql: &str) -> Result<Vec<Token>, String> {
    if sql.len() > 16384 {
        return Err("SQL statement limit".into());
    }
    let mut chars = sql.chars().peekable();
    let mut tokens = Vec::new();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == '\'' || c == '"' {
            let mut s = String::new();
            loop {
                match chars.next() {
                    None => return Err("unterminated SQL quote".into()),
                    Some(v) if v == c => {
                        if chars.peek() == Some(&c) {
                            chars.next();
                            s.push(c);
                        } else {
                            break;
                        }
                    }
                    Some(v) => s.push(v),
                }
            }
            tokens.push(Token(c, s));
        } else if c.is_alphabetic() || c == '_' {
            let mut s = c.to_string();
            while chars
                .peek()
                .is_some_and(|v| v.is_alphanumeric() || *v == '_')
            {
                s.push(chars.next().unwrap());
            }
            tokens.push(Token('w', s));
        } else if c.is_ascii_digit() {
            let mut s = c.to_string();
            while chars.peek().is_some_and(|v| v.is_ascii_digit()) {
                s.push(chars.next().unwrap());
            }
            tokens.push(Token('n', s));
        } else if "*.=;(),<>{}".contains(c) {
            tokens.push(Token(c, c.to_string()));
        } else {
            return Err(format!("unsupported SQL character {c:?}"));
        }
        if tokens.len() > 4096 {
            return Err("SQL token limit".into());
        }
    }
    Ok(tokens)
}
pub fn parse(sql: &str) -> Result<Select, String> {
    let mut p = Parser {
        tokens: tokenize(sql)?,
        at: 0,
        depth: 0,
    };
    let query = p.select()?;
    p.sym(';');
    if p.at != p.tokens.len() {
        return Err(format!(
            "unsupported SQL suffix at {:?}; nothing executed",
            p.tokens.get(p.at).map(|t| &t.1)
        ));
    }
    Ok(query)
}
impl Parser {
    fn select(&mut self) -> Result<Select, String> {
        let p = self;
        if !p.kw("SELECT") {
            return Err("SQL supports SELECT only".into());
        }
        let mut columns = Vec::new();
        let mut qualifiers = Vec::new();
        loop {
            if columns.len() >= MAX_COLUMNS {
                return Err("SQL column limit".into());
            }
            columns.push(if p.kw("ANNOTATION") {
                if !p.sym('(') {
                    return Err("SQL ANNOTATION requires parentheses".into());
                }
                if !p.kw("OBJECT") {
                    let qualifier = p.id()?;
                    if !p.sym('.') || !p.kw("OBJECT") {
                        return Err("SQL ANNOTATION requires OBJECT".into());
                    }
                    qualifiers.push(qualifier);
                }
                if !p.sym(')') {
                    return Err("SQL ANNOTATION closing parenthesis expected".into());
                }
                Column::Annotation
            } else if p.kw("OBJECT") {
                Column::Object
            } else if p.kw("OID") {
                Column::Oid
            } else if p.kw("FREETEXT_RANK") {
                Column::FreeTextRank
            } else {
                let name = p.id()?;
                if p.sym('.') {
                    qualifiers.push(name);
                    if p.kw("OID") {
                        Column::Oid
                    } else if p.kw("OBJECT") {
                        Column::Object
                    } else {
                        Column::Field(p.id()?)
                    }
                } else {
                    Column::Field(name)
                }
            });
            if !p.sym(',') {
                break;
            }
        }
        let projection = Projection(columns);
        if !p.kw("FROM") {
            return Err("SQL FROM missing".into());
        }
        let area = p.id()?;
        if qualifiers.iter().any(|q| q != &area) {
            return Err("SQL qualifier does not match FROM".into());
        }
        let predicate = if p.kw("WHERE") {
            p.predicate(&area)?
        } else {
            Predicate::All
        };
        let mut order = Vec::new();
        if p.kw("ORDER") {
            if !p.kw("BY") {
                return Err("SQL ORDER requires BY".into());
            }
            loop {
                if order.len() >= 16 {
                    return Err("SQL ordering limit".into());
                }
                let mut field = p.id()?;
                if p.sym('.') {
                    if field != area {
                        return Err("SQL qualifier does not match FROM".into());
                    }
                    field = p.id()?;
                }
                let descending = if p.kw("DESC") {
                    true
                } else if p.kw("ASC") {
                    false
                } else {
                    field.eq_ignore_ascii_case("FREETEXT_RANK")
                };
                order.push((field, descending));
                if !p.sym(',') {
                    break;
                }
            }
        }

        Ok(Select {
            area,
            projection,
            predicate,
            order,
        })
    }
}

#[derive(Clone, Debug)]
pub enum Matcher {
    Exact(String),
    Like {
        residual: regex::Regex,
        indexed: regex::Regex,
        requires_index: bool,
    },
}
impl Matcher {
    pub fn requires_index_semantics(&self) -> bool {
        matches!(
            self,
            Self::Like {
                requires_index: true,
                ..
            }
        )
    }
    pub fn is_match(&self, text: &str) -> bool {
        match self {
            // Equality folds case. Native LIKE has distinct index-mask and
            // residual comparisons (notably for lowercase ё).
            Self::Exact(value) => text.to_lowercase() == value.to_lowercase(),
            Self::Like { residual, .. } => residual.is_match(text),
        }
    }
    pub fn is_index_match(&self, text: &str) -> bool {
        match self {
            Self::Like { indexed, .. } => indexed.is_match(text),
            _ => self.is_match(text),
        }
    }
}
pub fn matcher(value: &str, like: bool) -> Result<Matcher, String> {
    if !like {
        return Ok(Matcher::Exact(value.into()));
    }
    if value.len() > 4096 {
        return Err("SQL LIKE pattern limit".into());
    }
    // The native LIKE mask folds the main Cyrillic alphabet, but leaves
    // lowercase ё unchanged while the field is uppercased. Equality uses
    // a separate comparison. Verified with ё, Ё, е and ? on the original.
    let mask = value
        .chars()
        .flat_map(|c| {
            if c == 'ё' {
                vec![c]
            } else {
                c.to_uppercase().collect()
            }
        })
        .collect::<String>();
    let pattern = format!(
        "^{}$",
        regex::escape(&mask)
            .replace("\\*", ".*")
            .replace("\\?", ".")
    );
    let indexed = regex::RegexBuilder::new(&pattern)
        .dot_matches_new_line(true)
        .size_limit(1024 * 1024)
        .build()
        .map_err(|e| e.to_string())?;
    let residual = regex::RegexBuilder::new(&format!(
        "^{}$",
        regex::escape(value)
            .replace("\\*", ".*")
            .replace("\\?", ".")
    ))
    .case_insensitive(true)
    .dot_matches_new_line(true)
    .size_limit(1024 * 1024)
    .build()
    .map_err(|e| e.to_string())?;
    Ok(Matcher::Like {
        residual,
        indexed,
        requires_index: value.contains('ё'),
    })
}
