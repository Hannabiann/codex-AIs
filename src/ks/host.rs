//! Bounded read-only JDBC bootstrap. Unsupported SQL is rejected as a whole.
use std::{cell::RefCell, rc::Rc};
#[derive(Clone, Debug)]
pub enum Handle {
    Manager,
    Connection(Rc<RefCell<Connection>>),
    Statement(Rc<RefCell<Statement>>),
    Rows(Rc<RefCell<Rows>>),
}
#[derive(Debug)]
pub struct Connection {
    closed: bool,
    scope: u32,
}
#[derive(Debug)]
pub struct Statement {
    connection: Rc<RefCell<Connection>>,
    sql: String,
    closed: bool,
    result: Option<Rc<RefCell<Rows>>>,
}
#[derive(Debug)]
pub struct Rows {
    pub area: Option<u16>,
    connection: Rc<RefCell<Connection>>,
    oids: Vec<u32>,
    projection: Projection,
    cells: Vec<Vec<ColumnValues>>,
    pub fulltext: crate::db::free_text::ResultData,
    position: i64,
    closed: bool,
}
pub enum Arg {
    Null,
    Database(u32),
    String(String),
    Number(f64),
}
pub enum ResultValue {
    Null,
    Bool(bool),
    Number(f64),
    Object(u32, u32),
    Annotation(u32, u32),
    FieldStream(u32, u32, u16, u16),
    String(String),
    IntArray(Vec<i32>),
    StringArray(Vec<String>),
    Date(f64),
    DateArray(Vec<f64>),
    Decimal([u8; 34]),
    DecimalArray(Vec<[u8; 34]>),
    Handle(Handle),
}
#[derive(Clone, Debug, PartialEq)]
pub enum Column {
    Object,
    FreeTextRank,
    Annotation,
    Oid,
    Field(String),
}
#[derive(Clone, Debug, PartialEq)]
pub struct Projection(pub Vec<Column>);
impl Projection {
    pub fn has_fields(&self) -> bool {
        self.0.iter().any(|c| matches!(c, Column::Field(_)))
    }
    fn index(&self, args: &[Arg]) -> Result<Option<usize>, String> {
        match args {
            [Arg::String(name)] => self
                .0
                .iter()
                .position(|c| match c {
                    Column::Object => name == "OBJECT",
                    Column::FreeTextRank => name == "FREETEXT_RANK",
                    Column::Annotation => name == "ANNOTATION(OBJECT)",
                    Column::Oid => name == "OID",
                    Column::Field(n) => name == n,
                })
                .map(Some)
                .ok_or_else(|| format!("unknown SQL result column {name}")),
            [Arg::Number(n)]
                if n.is_finite()
                    && n.fract() == 0.0
                    && *n >= i32::MIN as f64
                    && *n <= i32::MAX as f64 =>
            {
                Ok(if *n >= 1.0 && *n <= self.0.len() as f64 {
                    Some(*n as usize - 1)
                } else {
                    None
                })
            }
            _ => Err("SQL getter requires a column name or integer ordinal".into()),
        }
    }
}
#[derive(Debug, Default)]
pub struct QueryData {
    pub area: Option<u16>,
    pub oids: Vec<u32>,
    pub cells: Vec<Vec<ColumnValues>>,
    pub fulltext: crate::db::free_text::ResultData,
}
#[derive(Clone, Debug, PartialEq)]
pub enum ColumnValues {
    Identity,
    IdentityOid(u32),
    Strings(Vec<String>),
    Links(Vec<u32>),
    Dates(Vec<f64>),
    Decimals(Vec<[u8; 34]>),
    StreamRef { oid: u32, field: u16, kind: u16 },
}
pub type ScriptQuery = super::sql::Select;

pub fn script_query(sql: &str) -> Result<ScriptQuery, String> {
    let result = super::sql::parse(sql).or_else(|error| {
        super::sql_join::parse(sql)?
            .map(|join| join.signature())
            .ok_or(error)
    });
    if let Err(error) = &result {
        if std::env::var_os("KODEKS_TRACE").is_some_and(|v| v == "1") {
            eprintln!("KS SQL parse failed: {error}; query={sql}");
        }
    }
    result
}
impl Handle {
    pub fn call(
        &self,
        method: &str,
        args: &[Arg],
        query: &dyn Fn(u32, &str) -> Result<QueryData, String>,
    ) -> Result<ResultValue, String> {
        use ResultValue as V;
        match self {
            Self::Manager
                if method == "getConnection"
                    && (args.is_empty() || matches!(args, [Arg::Null] | [Arg::Database(_)])) =>
            {
                Ok(V::Handle(Self::Connection(Rc::new(RefCell::new(
                    Connection {
                        closed: false,
                        scope: if let [Arg::Database(id)] = args {
                            *id
                        } else {
                            0
                        },
                    },
                )))))
            }
            Self::Connection(c) => {
                if method == "close" && args.is_empty() {
                    c.borrow_mut().closed = true;
                    return Ok(V::Null);
                }
                if c.borrow().closed {
                    return Err("SQL connection closed".into());
                }
                if method == "prepareStatement" {
                    if let [Arg::String(sql)] = args {
                        script_query(sql)?;
                        return Ok(V::Handle(Self::Statement(Rc::new(RefCell::new(
                            Statement {
                                connection: c.clone(),
                                sql: sql.clone(),
                                closed: false,
                                result: None,
                            },
                        )))));
                    }
                }
                Err(format!("unsupported connection method/arguments: {method}"))
            }
            Self::Statement(s) => {
                let mut s = s.borrow_mut();
                if method == "close" && args.is_empty() {
                    s.closed = true;
                    if let Some(r) = s.result.take() {
                        r.borrow_mut().closed = true;
                    }
                    return Ok(V::Null);
                }
                if s.closed || s.connection.borrow().closed {
                    return Err("SQL statement/connection closed".into());
                }
                if !args.is_empty() {
                    return Err("prepared statement method expects no arguments".into());
                }
                match method {
                    "execute" | "executeQuery" => {
                        let QueryData {
                            area,
                            oids,
                            cells,
                            fulltext,
                        } = query(s.connection.borrow().scope, &s.sql)?;
                        if oids.len() > super::sql::MAX_SQL_ROWS {
                            return Err("SQL row limit".into());
                        }
                        if let Some(r) = s.result.take() {
                            r.borrow_mut().closed = true;
                        }
                        let r = Rc::new(RefCell::new(Rows {
                            area,
                            connection: s.connection.clone(),
                            oids,
                            cells,
                            fulltext,
                            projection: script_query(&s.sql)?.projection,
                            position: -1,
                            closed: false,
                        }));
                        s.result = Some(r.clone());
                        Ok(if method == "execute" {
                            V::Bool(true)
                        } else {
                            V::Handle(Self::Rows(r))
                        })
                    }
                    "getResultSet" => Ok(s
                        .result
                        .clone()
                        .map(|r| V::Handle(Self::Rows(r)))
                        .unwrap_or(V::Null)),
                    _ => Err(format!("unsupported statement method: {method}")),
                }
            }
            Self::Rows(r) => {
                let mut r = r.borrow_mut();
                if method == "close" && args.is_empty() {
                    r.closed = true;
                    r.oids.clear();
                    r.cells.clear();
                    return Ok(V::Null);
                }
                if r.closed || r.connection.borrow().closed {
                    return Err("SQL result/connection closed".into());
                }
                match method {
                    // JDBCResult::beforeFirst (007bfe60 -> 007cfff0).
                    // Rewind this result; do not rerun the query or close it.
                    "beforeFirst" if args.is_empty() => {
                        r.position = -1;
                        Ok(V::Null)
                    }
                    "absolute" | "next" | "prev" => {
                        let p = if method == "next" && args.is_empty() {
                            r.position + 1
                        } else if method == "prev" && args.is_empty() {
                            r.position - 1
                        } else if method == "absolute" {
                            let [Arg::Number(n)] = args else {
                                return Err("absolute requires one row index".into());
                            };
                            if !n.is_finite() || n.fract() != 0.0 || n.abs() > i32::MAX as f64 {
                                return Err("invalid row index".into());
                            }
                            *n as i64
                        } else {
                            return Err("invalid cursor arguments".into());
                        };
                        r.position = p;
                        Ok(V::Bool(p >= 0 && (p as usize) < r.oids.len()))
                    }
                    "getAsciiStream" => {
                        let Some(col) = r.projection.index(args)? else {
                            return Ok(V::Null);
                        };
                        if let Some(ColumnValues::StreamRef { oid, field, kind }) = r
                            .cells
                            .get(r.position as usize)
                            .and_then(|row| row.get(col))
                        {
                            return Ok(V::FieldStream(
                                *oid,
                                r.connection.borrow().scope,
                                *field,
                                *kind,
                            ));
                        }
                        if r.projection.0[col] != Column::Annotation {
                            return Err("getAsciiStream requires ANNOTATION(OBJECT)".into());
                        }
                        if r.position < 0 {
                            return Ok(V::Null);
                        }
                        Ok(r.oids
                            .get(r.position as usize)
                            .map(|oid| {
                                let oid = match r
                                    .cells
                                    .get(r.position as usize)
                                    .and_then(|row| row.get(col))
                                {
                                    Some(ColumnValues::IdentityOid(id)) => *id,
                                    _ => *oid,
                                };
                                V::Annotation(oid, r.connection.borrow().scope)
                            })
                            .unwrap_or(V::Null))
                    }
                    "getDate" | "_getDate" | "getTimestamp" | "getLongDouble"
                    | "_getLongDouble" => {
                        let Some(col) = r.projection.index(args)? else {
                            return Ok(V::Null);
                        };
                        if r.position < 0 || r.position as usize >= r.oids.len() {
                            return Ok(V::Null);
                        }
                        let cell = r
                            .cells
                            .get(r.position as usize)
                            .and_then(|row| row.get(col))
                            .ok_or("SQL typed column missing")?;
                        Ok(match cell {
                            ColumnValues::Dates(values)
                                if method.contains("Date") || method == "getTimestamp" =>
                            {
                                if values.is_empty() {
                                    V::Null
                                } else if method.starts_with('_') {
                                    V::DateArray(values.clone())
                                } else {
                                    V::Date(values[0])
                                }
                            }
                            ColumnValues::Decimals(values) if method.contains("LongDouble") => {
                                if values.is_empty() {
                                    V::Null
                                } else if method.starts_with('_') {
                                    V::DecimalArray(values.clone())
                                } else {
                                    V::Decimal(values[0])
                                }
                            }
                            _ => return Err(format!("SQL {method} column type mismatch")),
                        })
                    }
                    "getString" | "_getString" => {
                        // Native 007bebb0: names or one-based integer ordinals.
                        // 007cff40 throws on an unknown name; 007d2310 returns
                        // null for an invalid ordinal or a cursor outside a row.
                        let col = r.projection.index(args)?;
                        let Some(col) = col else {
                            return Ok(V::Null);
                        };
                        if !matches!(r.projection.0[col], Column::Field(_)) {
                            return Err("getString requires a string field column".into());
                        }
                        if r.position < 0 || r.position as usize >= r.oids.len() {
                            return Ok(V::Null);
                        }
                        let cell = r
                            .cells
                            .get(r.position as usize)
                            .and_then(|row| row.get(col))
                            .ok_or("SQL result column data missing")?;
                        let converted;
                        let values = match cell {
                            ColumnValues::Strings(v) => v,
                            ColumnValues::Links(v) => {
                                converted = v.iter().map(u32::to_string).collect::<Vec<_>>();
                                &converted
                            }
                            ColumnValues::Dates(v) => {
                                converted = v
                                    .iter()
                                    .map(|&millis| {
                                        let p = super::date::Date { millis }.parts();
                                        format!("{:02}.{:02}.{:04}", p[2], p[1] + 1, p[0])
                                    })
                                    .collect::<Vec<_>>();
                                &converted
                            }
                            _ => return Err("getString requires a convertible field column".into()),
                        };
                        Ok(if method == "_getString" {
                            if values.is_empty() {
                                V::Null
                            } else {
                                V::StringArray(values.clone())
                            }
                        } else {
                            // Native scalar strings expose the final stored
                            // occurrence; _getString preserves all occurrences.
                            // Verified against a two-address letter in post.db4.
                            let value = if matches!(cell, ColumnValues::Strings(_)) {
                                values.last()
                            } else {
                                values.first()
                            };
                            value.cloned().map(V::String).unwrap_or(V::Null)
                        })
                    }
                    "getObject" | "getInt" | "_getInt" => {
                        let Some(col) = r.projection.index(args)? else {
                            return Err(format!(
                                "projection has column 1..{} only",
                                r.projection.0.len()
                            ));
                        };
                        if matches!(method, "getInt" | "_getInt")
                            && r.projection.0[col] == Column::Object
                        {
                            return Err(format!("{method} requires an OID projection"));
                        }
                        // Native 007d2a30 returns no array outside a row.
                        if method == "_getInt"
                            && (r.position < 0 || r.position as usize >= r.oids.len())
                        {
                            return Ok(V::Null);
                        }
                        if r.position < 0 {
                            return Err("cursor before first row".into());
                        }
                        let mut oid = *r
                            .oids
                            .get(r.position as usize)
                            .ok_or("cursor not on a row")?;
                        if let Some(ColumnValues::IdentityOid(joined)) = r
                            .cells
                            .get(r.position as usize)
                            .and_then(|row| row.get(col))
                        {
                            oid = *joined;
                        }
                        if matches!(r.projection.0[col], Column::Field(_)) {
                            if method == "getObject" {
                                return Err(
                                    "getObject for field projections is not implemented".into()
                                );
                            }
                            let cell = r
                                .cells
                                .get(r.position as usize)
                                .and_then(|row| row.get(col))
                                .ok_or("SQL result column data missing")?;
                            let values = match cell {
                                ColumnValues::Links(values) => values.clone(),
                                ColumnValues::Strings(values) => values
                                    .iter()
                                    .map(|s| {
                                        s.trim().parse::<i32>().map(|n| n as u32).map_err(|_| {
                                            "SQL integer conversion failed".to_string()
                                        })
                                    })
                                    .collect::<Result<Vec<_>, _>>()?,
                                _ => {
                                    return Err(format!(
                                        "{method} requires an OID or convertible field"
                                    ))
                                }
                            };
                            return Ok(if method == "_getInt" {
                                if values.is_empty() {
                                    V::Null
                                } else {
                                    V::IntArray(values.iter().map(|n| *n as i32).collect())
                                }
                            } else {
                                // Scalar access uses the final stored occurrence,
                                // like getString. _getInt preserves every value in
                                // storage order. Native bankruptcy reports rely on
                                // this when an act has several "Основание" links.
                                values
                                    .last()
                                    .map(|n| V::Number(*n as f64))
                                    .unwrap_or(V::Null)
                            });
                        }
                        if method == "_getInt" {
                            // Native 007bfc60 / 007bcff9: array of signed ints.
                            // SELECT OID has one value per row. General field
                            // projections/multivalued SQL are not implemented.
                            return Ok(V::IntArray(vec![oid as i32]));
                        }
                        Ok(match &r.projection.0[col] {
                            Column::Object => V::Object(oid, r.connection.borrow().scope),
                            Column::Oid => V::Number(oid as f64),
                            Column::FreeTextRank => {
                                V::Number(r.fulltext.ranks.get(&oid).copied().unwrap_or(0) as f64)
                            }
                            Column::Field(_) | Column::Annotation => {
                                return Err(
                                    "getObject for field projections is not implemented".into()
                                )
                            }
                        })
                    }
                    "_getEstimatedSize" if args.is_empty() => Ok(V::Number(r.oids.len() as f64)),
                    _ => Err(format!("unsupported result method: {method}")),
                }
            }
            _ => Err(format!(
                "unsupported DriverManager method/arguments: {method}"
            )),
        }
    }
}
