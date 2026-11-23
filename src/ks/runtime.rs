//! Bounded source interpreter. Host/database APIs are explicit missing dependencies.

use super::parser::{self, Expr, Part, Stmt};
#[path = "array_search.rs"]
mod array_search;
#[path = "base64.rs"]
mod base64;
#[path = "compiled_runtime.rs"]
mod compiled_runtime;
#[path = "path_settings.rs"]
mod path_settings;

#[path = "attachment_files.rs"]
mod attachment_files;
#[path = "context_mark.rs"]
mod context_mark;
#[path = "database_lifetime.rs"]
mod database_lifetime;
#[path = "decimal.rs"]
pub(crate) mod decimal;
#[path = "document_fields.rs"]
mod document_fields;
#[path = "p_edit.rs"]
mod p_edit;
#[path = "document_kod.rs"]
mod document_kod;
#[path = "document_write.rs"]
mod document_write;
#[path = "file_access.rs"]
mod file_access;
#[path = "kjson.rs"]
mod kjson;

#[path = "form_doc.rs"]
mod form_doc;
#[path = "record_list.rs"]
mod record_list;
#[path = "report_cells.rs"]
mod report_cells;
#[path = "report_document.rs"]
mod report_document;
#[path = "report_rtf.rs"]
mod report_rtf;
#[path = "report_text.rs"]
mod report_text;

#[path = "web_request.rs"]
mod web_request;
#[path = "xls_report.rs"]
mod xls_report;
#[path = "xml_report.rs"]
mod xml_report;
#[path = "xml_sax.rs"]
pub(crate) mod xml_sax;
use std::{cell::RefCell, collections::HashMap, rc::Rc};
type Object = Rc<RefCell<HashMap<String, Value>>>;
type Loader<'a> = Box<dyn Fn(u32, u16) -> Result<String, String> + 'a>;
type Documents<'a> = Box<dyn Fn(u32) -> Result<Option<crate::db::fields::Record>, String> + 'a>;
type Query<'a> = Box<dyn Fn(&str) -> Result<Vec<u32>, String> + 'a>;
#[derive(Debug)]
struct Function {
    params: Vec<String>,
    body: Rc<Vec<Stmt>>,
    env: Vec<Object>,
    name: Option<String>,
    properties: Object,
}
#[derive(Debug)]
struct TempDocument {
    properties: Object,
    fields: HashMap<String, Rc<RefCell<String>>>,
}
#[derive(Debug)]
struct InputStream {
    data: Vec<u8>,
    // Stored rich text reports its encoded byte length, including font markers.
    encoded_extra: usize,
    position: usize,
    charset: String,
    closed: bool,
}
impl InputStream {
    fn insert_text(&mut self, bytes: &[u8]) -> Result<(), Fault> {
        if self.data.len().saturating_add(bytes.len()) > 16 * 1024 * 1024 {
            return Err("stream size limit".into());
        }
        if self.position == self.data.len() {
            self.data.extend_from_slice(bytes);
        } else {
            self.data
                .splice(self.position..self.position, bytes.iter().copied());
        }
        self.position += bytes.len();
        Ok(())
    }

    fn value(data: &[u8]) -> Value {
        Value::ReadStream(Rc::new(RefCell::new(Self {
            data: data.to_vec(),
            encoded_extra: 0,
            position: 0,
            charset: "windows-1251".into(),
            closed: false,
        })))
    }
}
#[derive(Clone, Debug)]
enum Value {
    EditDocument(document_write::Edit),
    EditMethod(document_write::Edit, String),
    WritableField(document_write::WritableField),
    FormTab(Rc<form_doc::Tab>),
    FormTabMethod(Rc<form_doc::Tab>, String),
    RichField(Rc<RefCell<form_doc::RichText>>),
    RichMethod(Rc<RefCell<form_doc::RichText>>, String),
    List(Rc<RefCell<record_list::RecordList>>),
    ListMethod(Rc<RefCell<record_list::RecordList>>, String),
    Css(Rc<RefCell<Vec<String>>>),
    CssRules(Rc<RefCell<Vec<String>>>),
    CssMethod(Rc<RefCell<Vec<String>>>, String),
    Signal(std::sync::Arc<super::signal::Signal>),
    SignalMethod(std::sync::Arc<super::signal::Signal>, String),
    HttpOutput,
    LogFile(Rc<file_access::WriteFile>),
    LogMethod(Rc<RefCell<std::fs::File>>, String),
    Regex(regex::Regex),
    Mutex(Rc<RefCell<super::mutex::NamedMutex>>),
    MutexMethod(Rc<RefCell<super::mutex::NamedMutex>>, String),
    RegistryKey(u32),
    RegistryMethod(u32, String),
    DirStat(super::file_list::Entry),
    File(Rc<file_access::ReadFile>),
    // Original KS createPath walks existing parents above the configured root.
    // This is only a directory probe, never a handle for listing or reading them.
    AncestorDirectory(std::path::PathBuf),
    FileMethod(Rc<file_access::ReadFile>, String),
    FilePath(Rc<RefCell<String>>),
    PathMethod(Rc<RefCell<String>>, String),
    TempDocument(Rc<RefCell<TempDocument>>),
    TempMethod(Rc<RefCell<TempDocument>>, String),
    TextField(Rc<RefCell<String>>),
    FieldMethod(Rc<RefCell<String>>, String),
    TextStream(Rc<RefCell<String>>),
    RichStream(Rc<RefCell<String>>),
    StreamMethod(Rc<RefCell<String>>, String),
    ReadStream(Rc<RefCell<InputStream>>),
    ReadStreamMethod(Rc<RefCell<InputStream>>, String),
    Date(Rc<RefCell<super::date::Date>>),
    DateMethod(Rc<RefCell<super::date::Date>>, String),
    Database(Object),
    DatabaseMethod(Object, String),
    Volume(u32, super::database::Volume),
    Application(Object),
    Host(super::host::Handle),
    HostMethod(super::host::Handle, String),
    Null,
    Bool(bool),
    Number(f64),
    Decimal(decimal::Decimal),
    DecimalMethod(decimal::Decimal, String),
    String(String),
    /// A caught native KS string exception (Code 668), distinct from a string.
    ScriptException(String),
    Function(Rc<Function>),
    FunctionMethod(Rc<Function>, String),
    Native(String),
    Object(Object),
    Array(Object),
    ArrayMethod(Object, String),
    ReadOnlyObject(Object),
    Unavailable(String),
    StoredField(document_fields::Field),
    StoredFieldMethod(document_fields::Field, String),
    Document(u32, u32),
    DocumentMethod(u32, u32, String),
    StringMethod(String, String),
}
// Native 006e2a00 requests %.20g; its legacy CRT emits 17 significant digits.
fn native_number_text(n: f64) -> String {
    if !n.is_finite() || n == 0.0 {
        return n.to_string();
    }
    let scientific = format!("{n:.16e}");
    let (mantissa, exp) = scientific.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    if !(-4..20).contains(&exp) {
        return format!(
            "{}e{}{abs:02}",
            mantissa.trim_end_matches('0').trim_end_matches('.'),
            if exp < 0 { "-" } else { "+" },
            abs = exp.unsigned_abs()
        );
    }
    let text = format!("{n:.places$}", places = (16 - exp).max(0) as usize);
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').into()
    } else {
        text
    }
}

impl Value {
    fn text(&self) -> String {
        match self {
            Self::EditDocument(d) => d.borrow().oid.to_string(),
            Self::WritableField(f) => f.snapshot().map(|f| f.display).unwrap_or_default(),
            Self::Document(oid, _) => oid.to_string(),
            Self::StoredField(field) => field.display.clone(),
            Self::Volume(_, v) => v.name.clone(),
            Self::DirStat(e) => e.name.clone(),
            Self::File(p) => p.path.to_string_lossy().into_owned(),
            Self::LogFile(p) => p.name.clone(),
            Self::AncestorDirectory(p) => p.to_string_lossy().into_owned(),
            Self::FilePath(s) => s.borrow().clone(),
            Self::Null => "".into(),
            Self::Bool(b) => b.to_string(),
            Self::Number(n) => native_number_text(*n),
            Self::Decimal(n) => n.text(),
            Self::String(s) | Self::ScriptException(s) => s.clone(),
            Self::Date(d) => {
                let p = d.borrow().parts();
                format!("{:02}.{:02}.{:04}", p[2], p[1] + 1, p[0])
            }
            Self::ReadOnlyObject(o) => o
                .borrow()
                .get("\0text")
                .map(Value::text)
                .unwrap_or_else(|| "[object Object]".into()),
            _ => "[function]".into(),
        }
    }
    fn truth(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(b) => *b,
            Self::Number(n) => *n != 0.0 && !n.is_nan(),
            // Native LongDouble is an object: a stored zero still exists.
            // Treating it as numeric false blanks money inputs on round-trip.
            Self::Decimal(_) => true,
            Self::String(s) => !s.is_empty(),
            _ => true,
        }
    }
    fn number(&self) -> Result<f64, String> {
        match self {
            Self::EditDocument(d) => Ok(d.borrow().oid as f64),
            Self::WritableField(f) => f.value().map_err(|e| e.to_string())?.number(),
            Self::Document(oid, _) => Ok(*oid as f64),
            Self::StoredField(field) => field.value().map_err(|e| e.to_string())?.number(),
            Self::Date(d) => Ok(d.borrow().millis),
            Self::Number(n) => Ok(*n),
            Self::Decimal(n) => n.text().parse().map_err(|_| "invalid decimal".into()),
            Self::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
            Self::Null => Ok(0.0),
            Self::String(s) => Ok(if s.trim().is_empty() {
                0.0
            } else {
                s.trim().parse().unwrap_or(f64::NAN)
            }),
            _ => Err("expected number".into()),
        }
    }
}
#[derive(Debug, Clone)]
enum Fault {
    Script(Value),
    Host(String),
}
impl From<String> for Fault {
    fn from(s: String) -> Self {
        Self::Host(s)
    }
}
impl From<&str> for Fault {
    fn from(s: &str) -> Self {
        Self::Host(s.into())
    }
}
impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Script(v) => write!(f, "KS throw: {}", v.text()),
            Self::Host(s) => f.write_str(s),
        }
    }
}
enum Flow {
    Next,
    Return(Value),
    Break,
    Continue,
}
#[derive(Debug)]
pub enum OpenError {
    Schema(String),
    Io(String),
    Unsupported(String),
}
type Opener<'a> = Box<dyn Fn(&str, &[bool]) -> Result<crate::db::schema::Schema, OpenError> + 'a>;
pub struct Runtime<'a> {
    parsed_programs: HashMap<String, Rc<Vec<Stmt>>>,
    parsed_source_bytes: usize,
    profile_functions: bool,
    function_times: HashMap<String, (u64, u128, u128)>,
    function_children: Vec<u128>,
    numeric_arrays: array_search::NumericSearchCache,
    schema_links: HashMap<(usize, u16, bool), (Object, Value)>,
    document_vars: RefCell<HashMap<(u32, u32), Object>>,
    document_edits: RefCell<HashMap<(u32, u32), document_write::Edit>>,
    document_write_allowed: bool,
    schedule_export_allowed: bool,
    kod_snapshot_loaded: bool,
    p_edit_guard_required: bool,
    p_edit_submitted: std::collections::BTreeMap<String, String>,
    p_edit_checked: Option<(u32, u32)>,
    p_edit_snapshot: Option<(u32, u32, crate::db::fields::Record)>,
    scheduled_request: bool,
    registration_write_allowed: bool,
    attachment_write_allowed: bool,
    request_document_scopes: Vec<Object>,
    form_tabs: RefCell<HashMap<(u32, u32, u16), Rc<form_doc::Tab>>>,
    form_fields: RefCell<HashMap<(u32, u32, u16), Value>>,
    context_marks: HashMap<(u32, u32, usize), context_mark::Pattern>,
    rich_context_marks: HashMap<usize, (Rc<RefCell<form_doc::RichText>>, context_mark::Pattern)>,
    form_annotation_depth: usize,
    dbhost: Option<RefCell<Box<dyn super::database::DatabaseHost + 'a>>>,
    dbobjects: RefCell<HashMap<u32, Object>>,
    registry: Option<crate::db::registry::Registry>,
    documents: Option<Documents<'a>>,
    data_root: Option<String>,
    log_root: Option<std::path::PathBuf>,
    opener: Option<Opener<'a>>,
    closure_cells: RefCell<Vec<std::rc::Weak<RefCell<HashMap<String, Value>>>>>,
    attribute: Option<Box<dyn Fn(u32, &str) -> Result<u16, String> + 'a>>,
    with_objects: Vec<Object>,
    call_arguments: Vec<(Rc<Function>, Object)>,
    query: Option<Query<'a>>,
    pub scheduler: super::scheduler::Scheduler,
    http_replica: bool,
    replica_startup: bool,
    replica_signals: HashMap<String, std::sync::Arc<super::signal::Signal>>,
    mutex_handles: Vec<std::rc::Weak<RefCell<super::mutex::NamedMutex>>>,
    config: Option<Object>,
    caught: Vec<Value>,
    loader: Option<Loader<'a>>,
    include_depth: usize,
    scopes: Vec<Object>,
    output: String,
    binary_output: Option<Vec<u8>>,
    rtf_styles: RefCell<Option<Value>>,
    steps: usize,
    pub(crate) distribution_generation: u64,
    instruction_limit: Option<usize>,
    eval_depth: usize,
    pub path: String,
    http_output: Object,
    http_input: Value,
    request_parameters: Vec<String>,
    user_scope: Option<u32>,
    profile: Option<(std::time::Instant, usize)>,
    output_stream: Option<Rc<RefCell<InputStream>>>,
    request_baseline: Option<(HashMap<String, Value>, Object, HashMap<String, Value>)>,
}
impl<'a> Runtime<'a> {
    pub fn new(path: &str) -> Self {
        let mut global = HashMap::new();
        global.insert(
            "this".into(),
            Value::Object(Rc::new(RefCell::new(HashMap::from([(
                "\0typename".into(),
                Value::String("BaseVirtualDir".into()),
            )])))),
        );
        Self {
            parsed_programs: HashMap::new(),
            parsed_source_bytes: 0,
            profile_functions: std::env::var_os("KODEKS_PROFILE").is_some_and(|v| v == "1"),
            function_times: HashMap::new(),
            function_children: Vec::new(),
            numeric_arrays: Default::default(),
            schema_links: Default::default(),
            document_vars: Default::default(),
            document_edits: Default::default(),
            document_write_allowed: false,
            schedule_export_allowed: false,
            kod_snapshot_loaded: false,
            p_edit_guard_required: false,
            p_edit_submitted: Default::default(),
            p_edit_checked: None,
            p_edit_snapshot: None,
            scheduled_request: false,
            registration_write_allowed: false,
            attachment_write_allowed: false,
            request_document_scopes: Vec::new(),
            form_tabs: Default::default(),
            form_fields: Default::default(),
            context_marks: Default::default(),
            rich_context_marks: Default::default(),
            form_annotation_depth: 0,
            dbhost: None,
            dbobjects: RefCell::new(HashMap::new()),
            registry: None,
            documents: None,
            data_root: None,
            log_root: crate::runtime_logs::configured_root(),
            opener: None,
            closure_cells: RefCell::new(Vec::new()),
            attribute: None,
            with_objects: Vec::new(),
            call_arguments: Vec::new(),
            query: None,
            scheduler: Default::default(),
            http_replica: false,
            replica_startup: false,
            replica_signals: HashMap::new(),
            mutex_handles: Vec::new(),
            config: None,
            caught: Vec::new(),
            loader: None,
            include_depth: 0,
            scopes: vec![Rc::new(RefCell::new(global))],
            output: String::new(),
            binary_output: None,
            rtf_styles: RefCell::new(None),
            steps: 0,
            distribution_generation: 0,
            instruction_limit: None,
            eval_depth: 0,
            path: path.into(),
            http_output: Rc::new(RefCell::new(HashMap::new())),
            http_input: InputStream::value(&[]),
            request_parameters: Vec::new(),
            user_scope: None,
            profile: None,
            output_stream: None,
            request_baseline: None,
        }
    }
    pub fn with_application_config(
        mut self,
        config: &std::collections::BTreeMap<String, crate::db::ConfigValue>,
    ) -> Self {
        let fields = config
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    match v {
                        crate::db::ConfigValue::Text(s) => Value::String(s.clone()),
                        crate::db::ConfigValue::Integer(n) => Value::Number(*n as f64),
                        crate::db::ConfigValue::Invalid(s) => Value::Unavailable(s.clone()),
                    },
                )
            })
            .collect();
        self.scopes[0].borrow_mut().insert(
            "Application".into(),
            Value::Application(Rc::new(RefCell::new(HashMap::new()))),
        );
        self.config = Some(Rc::new(RefCell::new(fields)));
        self
    }
    pub fn with_data_root(mut self, path: &str) -> Self {
        self.data_root = Some(super::path::script_path(path));
        self
    }
    pub fn with_opener(
        mut self,
        f: impl Fn(&str, &[bool]) -> Result<crate::db::schema::Schema, OpenError> + 'a,
    ) -> Self {
        self.opener = Some(Box::new(f));
        self
    }
    fn database(schema: &crate::db::schema::Schema, scope: u32) -> Value {
        let mut areas = HashMap::new();
        areas.insert("\0enumerate_numeric".into(), Value::Bool(true));
        for a in schema.areas.values() {
            let mut fields = HashMap::new();
            fields.insert("\0enumerate_numeric".into(), Value::Bool(true));
            fields.insert("Name".into(), Value::String(a.name.clone()));
            fields.insert("Area".into(), Value::Number(a.id as f64));
            for f in a.fields.values() {
                let obj = Value::ReadOnlyObject(Rc::new(RefCell::new(HashMap::from([
                    ("Name".into(), Value::String(f.name.clone())),
                    ("Attr".into(), Value::Number(f.id as f64)),
                    ("Type".into(), Value::Number(f.kind as f64)),
                    ("Link".into(), Value::Number(f.reference as f64)),
                ]))));
                fields.insert(f.id.to_string(), obj.clone());
                fields.insert(f.name.clone(), obj);
            }
            let obj = Value::ReadOnlyObject(Rc::new(RefCell::new(fields)));
            areas.insert(a.id.to_string(), obj.clone());
            areas.insert(a.name.clone(), obj);
        }
        let db = Value::Database(Rc::new(RefCell::new(HashMap::from([
            ("\0scope".into(), Value::Number(scope as f64)),
            (
                "Struct".into(),
                Value::ReadOnlyObject(Rc::new(RefCell::new(areas))),
            ),
        ]))));
        db
    }
    fn selected_schema_links(&mut self, args: &[Value]) -> Result<Value, Fault> {
        let [Value::ReadOnlyObject(structure), Value::Number(area), Value::Bool(all)] = args else {
            return Err("__codex_LinkFields expects schema, area, all-links flag".into());
        };
        if !area.is_finite() || area.fract() != 0.0 || !(0.0..=65535.0).contains(area) {
            return Err("invalid link target area".into());
        }
        let key = (
            Rc::as_ptr(structure) as usize,
            if *all { 0 } else { *area as u16 },
            *all,
        );
        if self
            .schema_links
            .values()
            .next()
            .is_some_and(|(owner, _)| !Rc::ptr_eq(owner, structure))
        {
            self.schema_links.clear();
        }
        if let Some((_, value)) = self.schema_links.get(&key) {
            return Ok(value.clone());
        }
        // Struct is immutable and gets a new identity on schema revision.
        // Retaining that identity prevents pointer reuse within this request.
        fn entries(map: &Object) -> Result<Vec<(u16, Value)>, Fault> {
            let map = map.borrow();
            if !matches!(map.get("\0enumerate_numeric"), Some(Value::Bool(true))) {
                return Err("expected native schema metadata".into());
            }
            let mut items = map
                .iter()
                .filter_map(|(k, v)| k.parse::<u16>().ok().map(|i| (i, v.clone())))
                .collect::<Vec<_>>();
            items.sort_by_key(|(i, _)| *i);
            Ok(items)
        }
        let mut result = HashMap::new();
        let mut count = 0;
        for (_, owner) in entries(structure)? {
            let Value::ReadOnlyObject(fields) = &owner else {
                return Err("invalid schema area".into());
            };
            for (_, field) in entries(fields)? {
                let Value::ReadOnlyObject(properties) = &field else {
                    return Err("invalid schema field".into());
                };
                let properties = properties.borrow();
                if matches!(properties.get("Type"),Some(Value::Number(n)) if *n==5.0)
                    && (*all || matches!(properties.get("Link"),Some(Value::Number(n)) if n==area))
                {
                    result.insert(
                        count.to_string(),
                        Value::ReadOnlyObject(Rc::new(RefCell::new(HashMap::from([
                            ("0".into(), owner.clone()),
                            ("1".into(), field.clone()),
                        ])))),
                    );
                    count += 1;
                }
            }
        }
        result.insert("length".into(), Value::Number(count as f64));
        let value = Value::ReadOnlyObject(Rc::new(RefCell::new(result)));
        if self.schema_links.len() >= 128 {
            self.schema_links.clear();
        }
        self.schema_links
            .insert(key, (structure.clone(), value.clone()));
        Ok(value)
    }
    pub fn with_schema(self, schema: &crate::db::schema::Schema) -> Self {
        if let Value::Object(o) = &self.scopes[0].borrow()["this"] {
            let db = Self::database(schema, 0);
            if let Value::Database(o) = &db {
                self.dbobjects.borrow_mut().insert(0, o.clone());
            }
            o.borrow_mut().insert("Base".into(), db);
        }
        self
    }
    pub fn with_directory_properties(
        self,
        properties: &std::collections::BTreeMap<String, crate::db::ConfigValue>,
    ) -> Self {
        if let Value::Object(o) = &self.scopes[0].borrow()["this"] {
            for (k, v) in properties {
                if k == "Base" {
                    continue;
                }
                let v = match v {
                    crate::db::ConfigValue::Text(s) => Value::String(s.clone()),
                    crate::db::ConfigValue::Integer(n) => Value::Number(*n as f64),
                    crate::db::ConfigValue::Invalid(s) => Value::Unavailable(s.clone()),
                };
                o.borrow_mut().insert(k.clone(), v);
            }
        }
        self
    }
    pub(crate) fn update_directory_setting(
        &mut self,
        name: &str,
        value: &str,
        integer: bool,
    ) -> Result<(), String> {
        let value = if integer {
            Value::Number(value.parse::<i32>().map_err(|_| "Некорректный REG_DWORD")? as f64)
        } else {
            Value::String(value.into())
        };
        if let Some(Value::Object(this)) = self.scopes[0].borrow().get("this") {
            this.borrow_mut().insert(name.into(), value);
        }
        Ok(())
    }
    pub(crate) fn clear_directory_setting(&mut self, name: &str) {
        if let Some(Value::Object(this)) = self.scopes[0].borrow().get("this") {
            this.borrow_mut().insert(name.into(), Value::Null);
        }
    }
    pub fn with_database_host(mut self, host: impl super::database::DatabaseHost + 'a) -> Self {
        self.dbhost = Some(RefCell::new(Box::new(host)));
        self
    }
    fn db_scope(o: &Object) -> Result<u32, Fault> {
        match o.borrow().get("\0scope") {
            Some(Value::Number(n)) => Ok(*n as u32),
            _ => Err("DataBase identity missing".into()),
        }
    }
    fn database_for(&self, scope: u32) -> Result<Value, Fault> {
        if let Some(o) = self.dbobjects.borrow().get(&scope) {
            return Ok(Value::Database(o.clone()));
        }
        let schema = self
            .dbhost
            .as_ref()
            .ok_or("DataBase host not configured")?
            .borrow()
            .schema(scope)?;
        let db = Self::database(&schema, scope);
        if let Value::Database(o) = &db {
            self.dbobjects.borrow_mut().insert(scope, o.clone());
        }
        Ok(db)
    }
    pub fn with_attribute_resolver(
        mut self,
        f: impl Fn(u32, &str) -> Result<u16, String> + 'a,
    ) -> Self {
        self.attribute = Some(Box::new(f));
        self
    }
    pub fn with_registry(mut self, registry: &crate::db::registry::Registry) -> Self {
        self.registry = Some(registry.clone());
        self.scopes[0]
            .borrow_mut()
            .insert("RegistryRoot".into(), Value::RegistryKey(registry.root));
        self
    }
    pub fn with_documents(
        mut self,
        reader: impl Fn(u32) -> Result<Option<crate::db::fields::Record>, String> + 'a,
    ) -> Self {
        self.documents = Some(Box::new(reader));
        self
    }
    pub fn with_loader(mut self, loader: impl Fn(u32, u16) -> Result<String, String> + 'a) -> Self {
        self.loader = Some(Box::new(loader));
        self
    }
    pub fn with_query(mut self, query: impl Fn(&str) -> Result<Vec<u32>, String> + 'a) -> Self {
        self.query = Some(Box::new(query));
        self
    }
    pub fn load(&mut self, source: &str) -> Result<(), String> {
        self.load_inner(source).map_err(|e| e.to_string())
    }
    pub fn parse_cached(&mut self, source: &str) -> Result<Rc<Vec<Stmt>>, String> {
        // Cache syntax only. Functions and their captured request/document
        // environments are still created on every execution.
        if let Some(ast) = self.parsed_programs.get(source) {
            return Ok(ast.clone());
        }
        let ast = Rc::new(parser::parse(source).map_err(|e| format!("KS syntax: {e}"))?);
        if source.len() <= 1024 * 1024 {
            if self.parsed_programs.len() >= 256
                || self.parsed_source_bytes + source.len() > 4 * 1024 * 1024
            {
                self.parsed_programs.clear();
                self.parsed_source_bytes = 0;
            }
            self.parsed_source_bytes += source.len();
            self.parsed_programs.insert(source.into(), ast.clone());
        }
        Ok(ast)
    }
    fn load_inner(&mut self, source: &str) -> Result<(), Fault> {
        let ast = self.parse_cached(source)?;
        match self.list(&ast)? {
            Flow::Next | Flow::Return(_) => Ok(()),
            _ => Err("break/continue outside loop".into()),
        }
    }
    fn assign(&mut self, target: &Expr, v: Value) -> Result<(), Fault> {
        match target {
            Expr::Name(n) => {
                self.set(n, v)?;
                Ok(())
            }
            Expr::Member(b, k) => {
                let obj = self.eval(b)?;
                let key = self.eval(k)?.text();
                self.assign_member(obj, key, v)
            }
            _ => Err("invalid assignment target".into()),
        }
    }
    fn assign_member(&mut self, obj: Value, key: String, v: Value) -> Result<(), Fault> {
        // K5 accepts property assignment on boxed scalar values without
        // changing the scalar. The participant form submits case as a number,
        // while its KS normalizes case.oid before accepting either form.
        if matches!(obj, Value::Number(_) | Value::String(_) | Value::Bool(_)) {
            return Ok(());
        }
        if let Value::Function(f) = &obj {
            if key != "prototype" && !key.starts_with('\0') {
                if let Some(Value::Object(prototype)) = f.properties.borrow().get("prototype") {
                    if prototype.borrow().len() >= 10000 {
                        return Err("function property limit".into());
                    }
                    prototype.borrow_mut().insert(key, v);
                    return Ok(());
                }
            }
        }
        if let Value::File(file) = &obj {
            if key != "CharSet" {
                return Err("file stream property is read-only".into());
            }
            let charset = v.text().to_lowercase();
            if !matches!(charset.as_str(), "windows-1251" | "utf-8") {
                return Err("unsupported input stream charset".into());
            }
            *file.charset.borrow_mut() = charset;
            return Ok(());
        }
        match obj{Value::EditDocument(d)=>self.edit_set(d,&key,v),Value::WritableField(f)=>self.writable_set(f,&key,v),Value::List(l)=>self.list_set(&mut l.borrow_mut(), &key, v),Value::Array(o)=>{
 self.numeric_arrays.invalidate(&o);
 let mut map=o.borrow_mut();
 if key=="length" {let n=v.number()?;if !n.is_finite()||n<0.0||n.fract()!=0.0||n>u32::MAX as f64{return Err("invalid or excessive array length".into());}map.retain(|k,_|k.parse::<usize>().map(|i|i<(n as usize)).unwrap_or(true));map.insert(key,Value::Number(n));}
 else {if let Ok(index)=key.parse::<usize>(){if index.to_string()==key{if index>=u32::MAX as usize{return Err("KS array index limit".into());}let old=map.get("length").unwrap().number()? as usize;if index>=old{map.insert("length".into(),Value::Number((index+1) as f64));}}}map.insert(key,v);}Ok(())},Value::Document(oid,scope)=>{if key=="Area" && self.document_write_allowed {return self.assign_stored_area(oid,scope,&v);}let o=self.doc_object(oid,scope);if o.borrow().len()>10000{return Err("document member limit".into());}o.borrow_mut().insert(key,v);Ok(())},Value::TempDocument(d)=>{if !matches!(key.as_str(),"Area"|"NumDoc"){return Err("unsupported temporary document property".into());}d.borrow().properties.borrow_mut().insert(key,v);Ok(())},Value::Function(f)=>{if f.properties.borrow().len()>=10000{return Err("function property limit".into());}f.properties.borrow_mut().insert(key,v);Ok(())},Value::ReadStream(s)=>{if key!="CharSet"{return Err("input stream property is read-only".into());}let charset=v.text().to_lowercase();if !matches!(charset.as_str(),"windows-1251"|"utf-8"){return Err("unsupported input stream charset".into());}s.borrow_mut().charset=charset;Ok(())},Value::HttpOutput=>{if self.http_output.borrow().len()>=64{return Err("HTTP header limit".into());}self.http_output.borrow_mut().insert(key,v);Ok(())},Value::StoredField(_)=>Err("Document fields are read-only; persistent field writes are not implemented".into()),Value::ReadOnlyObject(_)=>Err("Application.Config writes require the native registry writer; source volumes remain read-only".into()),Value::Object(o)|Value::Application(o)=>{if o.borrow().len()>10000{return Err("KS object member limit".into());}o.borrow_mut().insert(key,v);Ok(())},_=>Err("property assignment requires an object".into())}
    }
    fn read_document(&self, oid: u32, scope: u32) -> Result<crate::db::fields::Record, Fault> {
        if let Some(d) = self.document_edits.borrow().get(&(scope, oid)) {
            return Ok(d.borrow().record());
        }
        if let Some((owner, id, record)) = &self.p_edit_snapshot {
            if (*owner, *id) == (scope, oid) { return Ok(record.clone()); }
        }
        (if let Some(host) = &self.dbhost {
            host.borrow().read(scope, oid)?
        } else {
            if scope != 0 {
                return Err("independent DataBase host not configured".into());
            }
            self.documents
                .as_ref()
                .ok_or("Document reader is not configured")?(oid)?
        })
        .ok_or_else(|| {
            Fault::Script(Value::String(format!(
                "Document({oid}) absent from mounted volumes"
            )))
        })
    }
    fn document_metadata(
        &self,
        oid: u32,
        scope: u32,
    ) -> Result<crate::db::fields::RecordMetadata, Fault> {
        // Pending edits take precedence over the mounted snapshot, just as for
        // a full read. The metadata type cannot be saved as a full document.
        if self.document_edits.borrow().contains_key(&(scope, oid)) || self.dbhost.is_none()
            || self.p_edit_snapshot.as_ref().is_some_and(|(owner, id, _)| (*owner, *id) == (scope, oid)) {
            return Ok((&self.read_document(oid, scope)?).into());
        }
        self.dbhost
            .as_ref()
            .unwrap()
            .borrow()
            .metadata(scope, oid)?
            .ok_or_else(|| {
                Fault::Script(Value::String(format!(
                    "Document({oid}) absent from mounted volumes"
                )))
            })
    }
    pub fn read_loaded_document(
        &self,
        oid: u32,
    ) -> Result<Option<crate::db::fields::Record>, String> {
        self.dbhost
            .as_ref()
            .ok_or("database host missing")?
            .borrow()
            .read(0, oid)
    }
    pub fn read_named_document(
        &self,
        name: &str,
    ) -> Result<Option<crate::db::fields::Record>, String> {
        self.dbhost
            .as_ref()
            .ok_or("database host missing")?
            .borrow()
            .named(0, name)
    }
    pub(crate) fn distribution_document(
        &self,
    ) -> Result<Option<crate::db::fields::Record>, String> {
        let host = self
            .dbhost
            .as_ref()
            .ok_or("database host missing")?
            .borrow();
        let schema = host.shared_schema(0)?;
        let area = schema
            .areas
            .get(&0)
            .ok_or("Служебная область отсутствует")?;
        let name = area.fields.get(&1).ok_or("Поле названия отсутствует")?;
        let quote = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
        let sql = format!(
            "SELECT OID FROM {} WHERE {}='Настройка распределения'",
            quote(&area.name),
            quote(&name.name)
        );
        let ids = host.query(0, &sql)?;
        match ids.as_slice() {
            [] => Ok(None),
            [oid] => host.read(0, *oid),
            _ => Err("В базе несколько документов «Настройка распределения»".into()),
        }
    }
    pub(crate) fn allow_distribution_writes(&mut self, allowed: bool) {
        self.document_write_allowed = allowed;
    }
    pub(crate) fn distribution_choices_where(
        &self,
        id: u16,
        condition: &str,
    ) -> Result<String, String> {
        let host = self
            .dbhost
            .as_ref()
            .ok_or("database host missing")?
            .borrow();
        let schema = host.shared_schema(0)?;
        let area = schema.areas.get(&id).ok_or("Справочник отсутствует")?;
        let area_name = area.name.replace('"', "\"\"");
        let ids = host.query(0, &format!("SELECT OID FROM \"{area_name}\" {condition}"))?;
        let mut rows = Vec::new();
        for oid in ids {
            crate::execution::check()?;
            if let Some(meta) = host.metadata(0, oid)? {
                if meta.area == id {
                    if let Some(name) = meta.name {
                        let s = crate::db::decode(&name).replace(['\t', '\r', '\n'], " ");
                        rows.push((s, oid));
                    }
                }
            }
        }
        rows.sort_unstable();
        Ok(rows
            .into_iter()
            .map(|(s, oid)| format!("{oid}\t{s}\n"))
            .collect())
    }
    fn execute_document(
        &mut self,
        oid: u32,
        scope: u32,
        attr: u16,
        context: Option<Value>,
    ) -> Result<Value, Fault> {
        if self.include_depth >= 32 {
            return Err("Document.Execute recursion limit".into());
        }
        let mut calendar_job = false;
        let source = if self.dbhost.is_some() {
            let rec = self.read_document(oid, scope)?;
            calendar_job = self.scheduled_request
                && attr == 3
                && rec.string(1).map(crate::db::decode).as_deref() == Some("newJudAndHoliday")
                && self.calendar_job_principal();
            if std::env::var_os("KODEKS_TRACE").is_some_and(|s| s == "1") {
                eprintln!(
                    "KS DataBase({scope}) Document({oid}).Execute({attr}): {}",
                    rec.string(1).map(crate::db::decode).unwrap_or_default()
                );
            }
            let mut fields = rec
                .fields
                .iter()
                .filter(|f| f.id == attr && f.wire_type == 4);
            let Some(field) = fields.next() else {
                // Classifier entries may have no optional SignScript. Native
                // Execute returns null for an absent attribute (e.g. letters).
                if !rec.fields.iter().any(|f| f.id == attr) {
                    return Ok(Value::Null);
                }
                return Err(
                    "Document.Execute attribute has no source text; compiled KS execution required"
                        .into(),
                );
            };
            if fields.next().is_some() {
                return Err("Document.Execute source field is ambiguous".into());
            }
            crate::db::decode(crate::document::bytes(field)?)
        } else {
            self.loader
                .as_ref()
                .ok_or("Document.Execute requires database context")?(oid, attr)?
        };
        let source = super::source_compat::document_source(oid, attr, &source);
        let previous_write = self.document_write_allowed;
        self.document_write_allowed |= calendar_job;
        let result = self.execute_code(&source, context);
        self.document_write_allowed = previous_write;
        result.map_err(|e| match e {
            Fault::Host(s) => Fault::Host(format!("Document({oid}).Execute({attr}): {s}")),
            e => e,
        })
    }
    fn execute_code(&mut self, source: &str, context: Option<Value>) -> Result<Value, Fault> {
        if self.include_depth >= 32 {
            return Err("Document.Execute recursion limit".into());
        }
        self.include_depth += 1;
        // Native Execute without an explicit context imports into the virtual directory.
        // A caller method's receiver is not the import target.
        let target = context.unwrap_or(
            self.scopes[0]
                .borrow()
                .get("this")
                .cloned()
                .ok_or("global context missing")?,
        );
        let object = match &target {
            Value::Object(o) => o.clone(),
            Value::Document(oid, scope) => self.doc_object(*oid, *scope),
            _ => {
                self.include_depth -= 1;
                return Err("Document.Execute context must be an object or document".into());
            }
        };
        let is_global =
            matches!(&self.scopes[0].borrow()["this"],Value::Object(o) if Rc::ptr_eq(o,&object));
        let scope = if is_global {
            self.scopes[0].clone()
        } else {
            let mut m = if matches!(target, Value::Document(..)) {
                HashMap::new()
            } else {
                self.scopes[0].borrow().clone()
            };
            m.extend(object.borrow().clone());
            m.insert("this".into(), target.clone());
            Rc::new(RefCell::new(m))
        };
        let context_scopes = if matches!(target, Value::Document(..)) {
            if self.request_baseline.is_some() {
                self.request_document_scopes.push(scope.clone());
            }
            vec![self.scopes[0].clone(), scope.clone()]
        } else {
            vec![scope.clone()]
        };
        let old = std::mem::replace(&mut self.scopes, context_scopes);
        let result = self
            .parse_cached(source)
            .map_err(Fault::Host)
            .and_then(|ast| self.list(&ast));
        self.scopes = old;
        if !is_global {
            let mut exported = scope.borrow().clone();
            exported.remove("this");
            object.borrow_mut().extend(exported);
        }
        self.include_depth -= 1;
        match result? {
            Flow::Return(v) => Ok(v),
            Flow::Next => Ok(Value::Null),
            _ => Err("invalid control flow in document".into()),
        }
    }
    fn tick(&mut self) -> Result<(), Fault> {
        self.steps = self.steps.saturating_add(1);
        if self.steps & 255 == 0 {
            crate::execution::check()?;
        }
        if self.profile_functions && self.steps % (64 * 1024 * 1024) == 0 {
            eprintln!(
                "KS progress: executed={}, active_calls={}, numeric_index_keys={}, output_bytes={}",
                self.steps,
                self.function_children.len(),
                self.numeric_arrays.indexed_values(),
                self.output.len()
            );
        }
        if let Some(limit) = self.instruction_limit.filter(|&limit| self.steps > limit) {
            Err(format!("KS instruction budget exceeded ({})", limit).into())
        } else {
            Ok(())
        }
    }
    pub fn set_instruction_limit(&mut self, limit: usize) {
        self.instruction_limit = Some(limit);
    }
    pub fn finish_function_profile(&mut self) {
        if !self.profile_functions {
            return;
        }
        eprintln!(
            "KS instruction profile: executed={}, limit={}",
            self.steps,
            self.instruction_limit
                .map_or_else(|| "unlimited".into(), |v| v.to_string())
        );
        let mut items = std::mem::take(&mut self.function_times)
            .into_iter()
            .collect::<Vec<_>>();
        items.sort_by_key(|(_, (_, _, exclusive))| std::cmp::Reverse(*exclusive));
        for (name, (calls, total, exclusive)) in items.into_iter().take(18) {
            eprintln!(
                "KS function profile: {name}, calls={calls}, total_us={total}, self_us={exclusive}"
            );
        }
    }
    pub fn instruction_count(&self) -> usize {
        self.steps
    }
    fn write(&mut self, s: &str) -> Result<(), Fault> {
        if let Some(stream) = &self.output_stream {
            let mut stream = stream.borrow_mut();
            if stream.closed {
                return Err("output stream closed".into());
            }
            let (bytes, _, bad) = encoding_rs::WINDOWS_1251.encode(s);
            if bad {
                return Err("output stream encoding".into());
            }
            stream.insert_text(&bytes)?;
            return Ok(());
        }
        if let Some(bytes) = &mut self.binary_output {
            let (encoded, _, bad) = encoding_rs::WINDOWS_1251.encode(s);
            if bad {
                return Err("output cannot be encoded as Windows-1251".into());
            }
            if bytes.len() + encoded.len() > 16 * 1024 * 1024 {
                return Err("binary output size limit".into());
            }
            bytes.extend_from_slice(&encoded);
            return Ok(());
        }
        self.output
            .try_reserve(s.len())
            .map_err(|e| format!("KS output allocation: {e}"))?;
        self.output.push_str(s);
        Ok(())
    }
    fn write_filtered(&mut self, text: &str) -> Result<(), Fault> {
        let filter = self.http_output.borrow().get("Escape").cloned();
        let chars = self
            .http_output
            .borrow()
            .get("escapeStr")
            .map(Value::text)
            .unwrap_or_default();
        let Some(Value::Function(filter)) = filter else {
            return self.write(text);
        };
        let mut start = 0;
        for (at, ch) in text.char_indices() {
            if chars.contains(ch) {
                self.write(&text[start..at])?;
                self.invoke(
                    filter.clone(),
                    vec![Value::String(ch.to_string())],
                    Some(Value::HttpOutput),
                    "Output.Escape".into(),
                )?;
                start = at + ch.len_utf8();
            }
        }
        self.write(&text[start..])
    }
    fn get(&self, n: &str) -> Result<Value, Fault> {
        for o in self.with_objects.iter().rev() {
            if let Some(v) = Self::property(o, n)? {
                return Ok(v);
            }
        }
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.borrow().get(n) {
                return Ok(v.clone());
            }
        }
        // Execute(source, this) may import members during a constructor.
        if let Some(Value::Object(receiver)) = self
            .scopes
            .last()
            .and_then(|s| s.borrow().get("this").cloned())
        {
            if let Some(value) = Self::property(&receiver, n)? {
                return Ok(value);
            }
        }
        // The directory's dynamic properties (e.g. report add-clause values)
        // are visible as globals while lexical locals still take precedence.
        if let Some(Value::Object(directory)) = self.scopes[0].borrow().get("this") {
            if let Some(value) = directory.borrow().get(n) {
                return Ok(value.clone());
            }
        }
        // Native request arguments are case-insensitive (toolbar prevDoc is
        // read as prevdoc by ispravlenieForm). Ordinary KS locals remain exact.
        if let Some(key) = self
            .request_parameters
            .iter()
            .find(|k| k.eq_ignore_ascii_case(n))
        {
            if let Some(v) = self.scopes[0].borrow().get(key) {
                return Ok(v.clone());
            }
        }
        match n {
            "Shell" => Ok(Value::Object(Rc::new(RefCell::new(HashMap::from([(
                "Execute".into(),
                Value::Native("attachment.shell".into()),
            )]))))),
            "RegistryRoot" => Err(format!("{n}: host binding not configured").into()),
            "DriverManager" => Ok(Value::Host(super::host::Handle::Manager)),
            "null" | "undefined" => Ok(Value::Null),
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            "StartProfile"
            | "__codex_LinkFields"
            | "__codex_TextFieldSize"
            | "__codex_PEditVersion"
            | "__codex_PEditCheck"
            | "__codex_PEditPrepare"
            | "__codex_PEditConflict"
            | "__codex_ImportKod"
            | "__codex_DistributionJudges"
            | "__codex_PAreaPage"
            | "StopProfile"
            | "escape"
            | "unescape"
            | "eval"
            | "parseInt"
            | "parseFloat"
            | "isNaN"
            | "FromBase64"
            | "ToBase64"
            | "pPath"
            | "Output"
            | "SetOutput"
            | "Document"
            | "KodeksFormatRTFWriter"
            | "Array"
            | "Object"
            | "Math"
            | "KJSON"
            | "char"
            | "String"
            | "Number"
            | "Boolean"
            | "InetConnection"
            | "toLog"
            | "MkDir"
            | "RmDir"
            | "Unlink"
            | "FindOrCreateHashObject" => Ok(Value::Native(n.into())),
            _ => Ok(Value::Null),
        }
    }
    fn set(&mut self, n: &str, v: Value) -> Result<(), Fault> {
        for o in self.with_objects.iter().rev() {
            if Self::property(o, n)?.is_some() {
                o.borrow_mut().insert(n.into(), v);
                return Ok(());
            }
        }
        for scope in self.scopes.iter_mut().rev() {
            if scope.borrow().contains_key(n) {
                scope.borrow_mut().insert(n.into(), v);
                return Ok(());
            }
        }
        self.scopes[0].borrow_mut().insert(n.into(), v);
        Ok(())
    }
    fn hoist(&mut self, stmts: &[Stmt]) {
        for s in stmts {
            let synchronized = matches!(s, Stmt::Synchronized(_));
            let s = if let Stmt::Synchronized(inner) = s {
                inner.as_ref()
            } else {
                s
            };
            if let Stmt::Block(body) = s {
                self.hoist(body);
            }
            if let Stmt::Function(n, args, b) = s {
                let body = if synchronized {
                    Rc::new(vec![Stmt::Synchronized(Box::new(Stmt::Block(
                        b.as_ref().clone(),
                    )))])
                } else {
                    b.clone()
                };
                let f = self.function(None, args.clone(), body);
                if let Value::Function(function) = &f {
                    function
                        .properties
                        .borrow_mut()
                        .insert("\0name".into(), Value::String(n.clone()));
                }
                self.scopes
                    .last()
                    .unwrap()
                    .borrow_mut()
                    .insert(n.clone(), f);
            }
        }
    }
    fn list(&mut self, stmts: &[Stmt]) -> Result<Flow, Fault> {
        self.hoist(stmts);
        for s in stmts {
            let f = self.stmt(s)?;
            if !matches!(f, Flow::Next) {
                return Ok(f);
            }
        }
        Ok(Flow::Next)
    }
    fn stmt(&mut self, s: &Stmt) -> Result<Flow, Fault> {
        self.tick()?;
        match s {
            Stmt::Compiled(program, entry) => return self.compiled(program, *entry),
            Stmt::With(e, body) => {
                let value = self.eval(e)?;
                let Value::Object(o) = value else {
                    return Err("with requires an object".into());
                };
                self.with_objects.push(o);
                let r = self.stmt(body);
                self.with_objects.pop();
                return r;
            }
            Stmt::Synchronized(inner) => {
                if matches!(inner.as_ref(), Stmt::Function(..)) {
                    return Ok(Flow::Next);
                }
                // Several KS executors share the registry. In particular, the
                // native incoming-number allocator protects a read/set pair
                // with synchronized. A per-runtime lock would lose increments.
                let name = format!(
                    "codex-KS-synchronized:{}",
                    self.data_root.as_deref().unwrap_or("")
                );
                let mut lock = super::mutex::NamedMutex::new(&name)?;
                lock.acquire(None)?;
                return self.stmt(inner);
            }
            Stmt::Empty | Stmt::Function(..) => {}
            Stmt::Expression(e) => {
                self.eval(e)?;
            }
            Stmt::Block(b) => return self.list(b),
            Stmt::Var(v) => {
                for (n, e) in v {
                    let val = if let Some(e) = e {
                        self.eval(e)?
                    } else {
                        Value::Null
                    };
                    self.scopes
                        .last()
                        .unwrap()
                        .borrow_mut()
                        .insert(n.clone(), val);
                }
            }
            Stmt::Template(parts) => {
                for p in parts {
                    match p {
                        Part::Text(t) => self.write(t)?,
                        Part::Value(e) => {
                            let v = self.eval(e)?;
                            self.write_filtered(&v.text())?;
                        }
                    }
                }
            }
            Stmt::If(c, a, b) => {
                if self.eval(c)?.truth() {
                    return self.stmt(a);
                } else if let Some(b) = b {
                    return self.stmt(b);
                }
            }
            Stmt::Return(e) => {
                return Ok(Flow::Return(if let Some(e) = e {
                    self.eval(e)?
                } else {
                    Value::Null
                }))
            }
            Stmt::Break => return Ok(Flow::Break),
            Stmt::Continue => return Ok(Flow::Continue),
            Stmt::While(c, b) => {
                while self.eval(c)?.truth() {
                    match self.stmt(b)? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        _ => {}
                    }
                }
            }
            Stmt::For(init, c, step, b) => {
                if let Some(i) = init {
                    self.stmt(i)?;
                }
                loop {
                    self.tick()?;
                    if let Some(c) = c {
                        if !self.eval(c)?.truth() {
                            break;
                        }
                    }
                    match self.stmt(b)? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        _ => {}
                    }
                    if let Some(e) = step {
                        self.eval(e)?;
                    }
                }
            }
            Stmt::Throw(e) => return Err(Fault::Script(self.eval(e)?)),
            Stmt::Rethrow => {
                return Err(self
                    .caught
                    .last()
                    .cloned()
                    .map(Fault::Script)
                    .unwrap_or_else(|| Fault::Host("rethrow outside catch".into())))
            }
            Stmt::Try(body, catch, finally) => {
                let mut result = self.stmt(body);
                if let Err(Fault::Script(value)) = &result {
                    if let Some(parser::Catch {
                        name,
                        body: handler,
                        declare,
                    }) = catch
                    {
                        let value = match value {
                            Value::String(text) => Value::ScriptException(text.clone()),
                            value => value.clone(),
                        };
                        // Restore the previous catch binding without creating a function scope.
                        let previous = self
                            .scopes
                            .last()
                            .unwrap()
                            .borrow_mut()
                            .insert(name.clone(), value.clone());
                        self.caught.push(value);
                        result = self.stmt(handler);
                        self.caught.pop();
                        if !declare {
                            let mut scope = self.scopes.last().unwrap().borrow_mut();
                            if let Some(old) = previous {
                                scope.insert(name.clone(), old);
                            } else {
                                scope.remove(name);
                            }
                        }
                    }
                }
                // Unsupported host behavior and exhausted budgets are fatal, never swallowed by finally.
                if matches!(result, Err(Fault::Host(_))) {
                    return result;
                }
                if let Some(cleanup) = finally {
                    match self.stmt(cleanup) {
                        Ok(Flow::Next) => {}
                        other => return other,
                    }
                }
                return result;
            }
            Stmt::ForIn(binding, source, body) => {
                let value = self.eval(source)?;
                let numeric_keys = matches!(
                    &value,
                    Value::Array(_) | Value::List(_) | Value::StoredField(_) | Value::Database(_)
                );
                let mut keys = match value {
                    Value::RichField(f) => (0..f.borrow().parts.len()).map(|i|i.to_string()).collect(),
                    Value::StoredField(f) => (0..f.text_objects()?.len())
                        .map(|i| i.to_string())
                        .collect(),
                    Value::List(list) => (0..self.list_len(&list.borrow())?)
                        .map(|i| i.to_string())
                        .collect(),
                    Value::Database(o) => {
                        let scope = Self::db_scope(&o)?;
                        let n = self
                            .dbhost
                            .as_ref()
                            .ok_or("DataBase enumeration host unavailable")?
                            .borrow()
                            .volumes(scope)?
                            .len();
                        (0..n).map(|i| i.to_string()).collect()
                    }
                    Value::Object(o) => {
                        let mut keys = Self::keys(&o)?;
                        if let Some((_, _, baseline)) = &self.request_baseline {
                            if matches!(self.scopes[0].borrow().get("this"), Some(Value::Object(connection)) if Rc::ptr_eq(connection, &o))
                            {
                                keys.retain(|k| {
                                    self.request_parameters.contains(k)
                                        || (!baseline.contains_key(k)
                                            && !matches!(k.as_str(), "Request" | "Globals"))
                                });
                            }
                        }
                        keys
                    }
                    Value::ReadOnlyObject(o) => {
                        let o = o.borrow();
                        o.keys()
                            .filter(|k| {
                                !k.starts_with('\0')
                                    && (!o.contains_key("\0enumerate_numeric")
                                        || k.parse::<u16>().is_ok())
                            })
                            .cloned()
                            .collect()
                    }
                    Value::Function(f) => {
                        let properties = f.properties.borrow();
                        let mut keys = properties
                            .keys()
                            .filter(|k| k.as_str() != "prototype" && !k.starts_with('\0'))
                            .cloned()
                            .collect::<Vec<_>>();
                        if let Some(Value::Object(prototype)) = properties.get("prototype") {
                            keys.extend(Self::keys(prototype)?);
                        }
                        keys.sort();
                        keys.dedup();
                        keys
                    }
                    Value::Array(o) => {
                        let m=o.borrow();
                        m.keys().filter(|k| k.as_str()!="length" && !k.starts_with('\0') &&
                            (!m.contains_key("\0enumerate_numeric") || k.parse::<usize>().is_ok())).cloned().collect()
                    }
                    Value::Document(oid, scope) => self
                        .doc_object(oid, scope)
                        .borrow()
                        .iter()
                        .filter(|(k, v)| !k.starts_with('\0') && !matches!(v, Value::Function(_)))
                        .map(|(k, _)| k.clone())
                        .collect(),
                    _ => {
                        return Err(format!("for-in requires an object or array: {source:?}").into())
                    }
                };
                keys.sort_by(|a, b| match (a.parse::<usize>(), b.parse::<usize>()) {
                    (Ok(a), Ok(b)) => a.cmp(&b),
                    (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                    (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                    _ => a.cmp(b),
                });
                let target = match binding.as_ref() {
                    Stmt::Var(v) if v.len() == 1 && v[0].1.is_none() => {
                        self.scopes
                            .last()
                            .unwrap()
                            .borrow_mut()
                            .insert(v[0].0.clone(), Value::Null);
                        Expr::Name(v[0].0.clone())
                    }
                    Stmt::Expression(e) => e.clone(),
                    _ => return Err("unsupported for-in binding".into()),
                };
                for key in keys {
                    self.tick()?;
                    let index = if numeric_keys {
                        key.parse::<usize>()
                            .ok()
                            .map(|i| Value::Number(i as f64))
                            .unwrap_or(Value::String(key))
                    } else {
                        Value::String(key)
                    };
                    self.assign(&target, index)?;
                    match self.stmt(body)? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        _ => {}
                    }
                }
            }
            Stmt::Do(body, condition) => loop {
                match self.stmt(body)? {
                    Flow::Break => break,
                    Flow::Return(v) => return Ok(Flow::Return(v)),
                    _ => {}
                }
                if !self.eval(condition)?.truth() {
                    break;
                }
            },
            Stmt::Switch(expr, cases) => {
                let value = self.eval(expr)?;
                let mut first = None;
                let mut default = None;
                for (i, (key, _)) in cases.iter().enumerate() {
                    if let Some(key) = key {
                        if field_equal(&value, &self.eval(key)?) {
                            first = Some(i);
                            break;
                        }
                    } else {
                        default = Some(i);
                    }
                }
                if let Some(start) = first.or(default) {
                    for (_, body) in &cases[start..] {
                        match self.list(body)? {
                            Flow::Next => {}
                            Flow::Break => break,
                            other => return Ok(other),
                        }
                    }
                }
            }
        }
        Ok(Flow::Next)
    }
    fn function(&self, name: Option<String>, params: Vec<String>, body: Rc<Vec<Stmt>>) -> Value {
        let prototype = Value::Object(Rc::new(RefCell::new(HashMap::new())));
        let properties = Rc::new(RefCell::new(HashMap::from([(
            "prototype".into(),
            prototype,
        )])));
        let mut env = self.scopes.clone();
        env.extend(self.with_objects.iter().cloned());
        {
            let mut cells = self.closure_cells.borrow_mut();
            cells.extend(env.iter().map(Rc::downgrade));
            cells.push(Rc::downgrade(&properties));
            if let Some(Value::Object(p)) = properties.borrow().get("prototype") {
                cells.push(Rc::downgrade(p));
            }
        }
        Value::Function(Rc::new(Function {
            params,
            body,
            env,
            name,
            properties,
        }))
    }
    fn property(o: &Object, key: &str) -> Result<Option<Value>, Fault> {
        let mut current = Some(o.clone());
        let mut seen = std::collections::HashSet::new();
        while let Some(o) = current {
            if seen.len() >= 64 || !seen.insert(Rc::as_ptr(&o) as usize) {
                return Err("prototype chain cycle/depth limit".into());
            }
            let m = o.borrow();
            if let Some(v) = m.get(key) {
                return Ok(Some(v.clone()));
            }
            if key == "constructor" {
                if let Some(v) = m.get("\0constructor") {
                    return Ok(Some(v.clone()));
                }
            }
            current = match m.get("\0prototype") {
                Some(Value::Object(p)) => Some(p.clone()),
                _ => None,
            };
        }
        Ok(None)
    }
    fn keys(o: &Object) -> Result<Vec<String>, Fault> {
        let mut keys = std::collections::HashSet::new();
        let mut seen = std::collections::HashSet::new();
        let mut current = Some(o.clone());
        while let Some(o) = current {
            if seen.len() >= 64 || !seen.insert(Rc::as_ptr(&o) as usize) {
                return Err("prototype chain cycle/depth limit".into());
            }
            let m = o.borrow();
            keys.extend(m.keys().filter(|k| !k.starts_with('\0') &&
                (!m.contains_key("\0enumerate_numeric") || k.parse::<usize>().is_ok())).cloned());
            current = match m.get("\0prototype") {
                Some(Value::Object(p)) => Some(p.clone()),
                _ => None,
            };
        }
        Ok(keys.into_iter().collect())
    }
    fn member(&self, base: Value, key: String) -> Result<Value, Fault> {
        if let Value::Object(o) = &base {
            if let Some(Value::String(kind))=o.borrow().get("\0report_collection") {
                if (kind=="rows" && matches!(key.as_str(),"Add"|"Remove")) ||
                    (kind=="parts" && matches!(key.as_str(),"InsertText"|"InsertTable"|"Insert"|"TextLength")) {
                    return Ok(Value::Native(format!("report.collection.{key}")));
                }
            }
        }
        // HTTP argument names are case-insensitive on Connection objects,
        // including bracket access used by compiled KS (mp_g_this["name"]).
        if let Value::Object(object) = &base {
            let map = object.borrow();
            if !map.contains_key(&key)
                && matches!(map.get("\0typename"),Some(Value::String(n)) if n=="Connection")
            {
                if let Some(name) = self
                    .request_parameters
                    .iter()
                    .find(|n| n.eq_ignore_ascii_case(&key))
                {
                    if let Some(value) = map.get(name) {
                        return Ok(value.clone());
                    }
                }
            }
        }
        match (base, key.as_str()) {
            (Value::Native(n), "styles") if n=="KodeksFormatRTFWriter" => {
                Ok(self.rtf_styles.borrow_mut().get_or_insert_with(report_rtf::styles).clone())
            }
            (Value::Native(n), "ReadFromStream") if n == "Document" => {
                Ok(Value::Native("Document.ReadFromStream".into()))
            }
            (Value::EditDocument(d), _) => self.edit_property(d, key),
            (Value::WritableField(f), _) => self.writable_property(f, key),
            (
                Value::Document(oid, scope),
                "Lock" | "Unlock" | "Write" | "NewField" | "DeleteField" | "Delete" | "SaveTo",
            ) => Ok(Value::DocumentMethod(oid, scope, key)),
            (Value::ScriptException(text), "errText") => Ok(Value::String(text)),
            (Value::ScriptException(_), "Code") => Ok(Value::Number(668.0)),
            (Value::ScriptException(_), _) => Ok(Value::Null),
            (Value::Native(n), "stringify" | "parse") if n == "KJSON" => {
                Ok(Value::Native(format!("KJSON.{key}")))
            }
            (Value::Native(n), _) if n == "Math" => match key.as_str() {
                "random" | "floor" | "ceil" | "round" | "abs" | "sqrt" | "min" | "max" | "pow" => {
                    Ok(Value::Native(format!("Math.{key}")))
                }
                "PI" => Ok(Value::Number(std::f64::consts::PI)),
                _ => Err(format!("Math.{key} not implemented").into()),
            },
            (Value::StoredField(f), _) if key.parse::<usize>().is_ok() => Ok(f
                .text_objects()?
                .get(key.parse::<usize>().unwrap())
                .cloned()
                .unwrap_or(Value::Null)),
            (Value::RichField(text), "InsertText" | "InsertTable" | "Insert" | "TextLength" | "InsertList" | "MarkContext") => {
                Ok(Value::RichMethod(text, key))
            }
            (Value::RichField(text), "length") => Ok(Value::Number(text.borrow().parts.len() as f64)),
            (Value::RichField(text), _) if key.parse::<usize>().is_ok() => Ok(text.borrow().parts.get(key.parse::<usize>().unwrap()).cloned().map(Value::Object).unwrap_or(Value::Null)),
            (Value::FormTab(tab), "isEmpty") => Ok(Value::FormTabMethod(tab, key)),
            (Value::FormTab(tab), _) => Ok(match key.as_str() {
                "name" => Value::String(tab.name.clone()),
                "attr" => Value::Number(tab.attr as f64),
                "type" => Value::Number(if tab.kind == "text" { 1.0 } else { 16.0 }),
                "fullText" => Value::Bool(false),
                _ => Value::Null,
            }),
            (
                Value::List(list),
                "AddCondition" | "Add" | "Find" | "AddEmptyCondition" | "Delete" | "IsInArray",
            ) => Ok(Value::ListMethod(list, key)),
            (Value::List(list), "Size" | "length") => {
                Ok(Value::Number(self.list_len(&list.borrow())? as f64))
            }
            (Value::List(list), _) => {
                let l = list.borrow();
                let index = key
                    .parse::<usize>()
                    .map_err(|_| format!("List.{key} not implemented"))?;
                Ok(self
                    .list_item(&l, index)?
                    .map(|id| Value::Document(id, l.scope))
                    .unwrap_or(Value::Null))
            }
            (Value::Css(css), "cssRules") => Ok(Value::CssRules(css)),
            (Value::Css(css), "insertRule" | "deleteRule") => Ok(Value::CssMethod(css, key)),
            (Value::CssRules(css), "length") => Ok(Value::Number(css.borrow().len() as f64)),
            (Value::CssRules(css), "item") => Ok(Value::CssMethod(css, key)),
            (Value::Native(n), "MAX_value") if n == "Number" => Ok(Value::Number(1.7e308)),
            (Value::Native(n), "MIN_value") if n == "Number" => Ok(Value::Number(1.7e-308)),
            (Value::Signal(s), "SwitchOn" | "SwitchOff" | "Wait" | "InfiniteWait") => {
                Ok(Value::SignalMethod(s, key))
            }
            (Value::Mutex(m), "Lock" | "TryLock" | "UnLock") => Ok(Value::MutexMethod(m, key)),
            (Value::RegistryKey(id), "Name") => Ok(Value::String(
                self.registry
                    .as_ref()
                    .ok_or("RegistryRoot not configured")?
                    .key(id)?
                    .name
                    .clone(),
            )),
            (Value::RegistryKey(id), "Key") => Ok(Value::Number(id as f64)),
            (Value::RegistryKey(id), _) => Ok(Value::RegistryMethod(id, key)),
            (Value::Null, _) => Err(Fault::Script(Value::String(format!(
                "cannot read {key} of null/undefined"
            )))),
            (Value::StoredField(f), "Attr") => Ok(Value::Number(f.data().id as f64)),
            (Value::StoredField(f), "Type") => Ok(Value::Number(f.data().wire_type as f64)),
            (Value::StoredField(f), "Value") => f.value(),
            (Value::StoredField(f), "NumDoc") if f.data().wire_type == 5 => f.value(),
            (Value::StoredField(f), "TextLength" | "ToStream" | "MarkContext")
                if f.data().wire_type == 4 =>
            {
                Ok(Value::StoredFieldMethod(f, key))
            }
            (Value::StoredField(f), "Stream") if matches!(f.data().wire_type, 8 | 14) => f.value(),
            (Value::StoredField(f), "FileName") if matches!(f.data().wire_type, 8 | 14) => {
                let crate::db::fields::Value::Binary { file, .. } = &f.data().value else {
                    return Err("invalid binary field".into());
                };
                Ok(file
                    .as_ref()
                    .map(|m| Value::String(crate::db::decode(&m.name)))
                    .unwrap_or(Value::Null))
            }
            (Value::StoredField(f), "Link") if f.data().wire_type == 5 => f.value(),
            (Value::ReadStream(stream), _) => {
                let s = stream.borrow();
                if s.closed {
                    return Err("stream closed".into());
                }
                match key.as_str() {
                    "length" => Ok(Value::Number((s.data.len() + s.encoded_extra) as f64)),
                    "CharSet" => Ok(Value::String(s.charset.clone())),
                    "read" | "readByte" | "readLine" | "reset" | "close" | "copyTo" | "Print"
                    | "PrintS" | "write" | "skip" => {
                        Ok(Value::ReadStreamMethod(stream.clone(), key))
                    }
                    _ => Err(format!("unsupported input stream member {key}").into()),
                }
            }
            (
                Value::HttpOutput,
                "read" | "readByte" | "readLine" | "reset" | "copyTo" | "length",
            ) => self.member(self.http_input.clone(), key),
            (
                Value::HttpOutput,
                "Print" | "PrintS" | "write" | "StopBuffMode" | "StartBuffMode",
            ) => Ok(Value::Native(format!("stream.{key}"))),
            (Value::HttpOutput, _) => Ok(self
                .http_output
                .borrow()
                .get(&key)
                .cloned()
                .unwrap_or(Value::Null)),
            (Value::LogFile(file), _) => Ok(Value::LogMethod(file.file.clone(), key)),
            (Value::Document(_, scope), "Base") => self.database_for(scope),
            (Value::Document(oid, scope), "Volume") => Ok(Value::Volume(
                scope,
                self.dbhost
                    .as_ref()
                    .ok_or("Document.Volume requires database context")?
                    .borrow()
                    .document_volume(scope, oid)?,
            )),
            (Value::Document(oid, _scope), "NumDoc") => Ok(Value::Number(oid as f64)),
            (Value::Document(oid, scope), "Ver") => {
                if let Some(host) = &self.dbhost {
                    if let Some(version) = host.borrow().document_revision(scope, oid)? {
                        return Ok(Value::Number(version as f64));
                    }
                }
                // Opaque content revision for this read-only runtime's cache keys.
                use std::hash::{Hash, Hasher};
                let record = self.read_document(oid, scope)?;
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                format!("{record:?}").hash(&mut hash);
                Ok(Value::String(format!("{:016x}", hash.finish())))
            }
            (Value::Document(oid, scope), "Area") => Ok(Value::Number(
                self.document_metadata(oid, scope)?.area as f64,
            )),
            (Value::Document(oid, scope), "Locked") => Ok(Value::Bool(
                self.document_edits
                    .borrow()
                    .get(&(scope, oid))
                    .is_some_and(|d| d.borrow().locked),
            )),
            (Value::Document(oid, scope), "AreaName") => {
                let area = self.document_metadata(oid, scope)?.area;
                let structure = self.member(self.database_for(scope)?, "Struct".into())?;
                let definition = self.member(structure, area.to_string())?;
                self.member(definition, "Name".into())
            }
            (Value::Document(oid, scope), "Name") => Ok(self
                .document_metadata(oid, scope)?
                .name
                .map(|b| Value::String(crate::db::decode(&b)))
                .unwrap_or(Value::Null)),
            (Value::DirStat(e), "Name") => Ok(Value::String(e.name)),
            (Value::DirStat(e), "lastModified") => Ok(Value::Number(e.modified)),
            (Value::DirStat(e), "length") => Ok(Value::Number(e.size as f64)),
            (Value::File(p), "length") => Ok(Value::Number(
                std::fs::metadata(&p.path)
                    .map_err(|e| Fault::Script(Value::String(e.to_string())))?
                    .len() as f64,
            )),
            (Value::File(p), "CharSet") => Ok(Value::String(p.charset.borrow().clone())),
            (Value::File(p), _) => Ok(Value::FileMethod(p, key)),
            (Value::AncestorDirectory(p), "isDirectory") => {
                Ok(Value::FileMethod(file_access::ReadFile::new(p), key))
            }
            (Value::AncestorDirectory(_), _) => Err(format!(
                "File.{key}: ancestor directory supports only existence and isDirectory probes"
            )
            .into()),
            (Value::FilePath(p), _) => Ok(Value::PathMethod(p, key)),
            (Value::Function(f), "call" | "apply") => Ok(Value::FunctionMethod(f, key)),
            (Value::Function(f), "arguments") => Ok(self
                .call_arguments
                .iter()
                .rev()
                .find(|(g, _)| Rc::ptr_eq(g, &f))
                .map(|(_, a)| Value::Array(a.clone()))
                .unwrap_or(Value::Null)),
            (Value::Function(f), "length") => Ok(Value::Number(f.params.len() as f64)),
            (Value::Function(f), _) => {
                let properties = f.properties.borrow();
                if let Some(value) = properties.get(&key) {
                    return Ok(value.clone());
                }
                // K5 exposes class methods/metadata through both Ctor and
                // Ctor.prototype. The participant wrappers use both spellings.
                if let Some(Value::Object(prototype)) = properties.get("prototype") {
                    return Ok(Self::property(prototype, &key)?.unwrap_or(Value::Null));
                }
                Ok(Value::Null)
            }
            (Value::TempDocument(d), "NewField" | "Compile" | "Execute" | "Attr" | "Lock" | "Unlock" | "Write") => {
                Ok(Value::TempMethod(d, key))
            }
            (Value::TempDocument(d), _) => Ok(d
                .borrow()
                .properties
                .borrow()
                .get(&key)
                .cloned()
                .unwrap_or(Value::Null)),
            (Value::TextField(t), "InsertText") => Ok(Value::FieldMethod(t, key)),
            (Value::TextStream(t), "Print" | "PrintS") => Ok(Value::StreamMethod(t, key)),
            (Value::RichStream(t), "Print" | "PrintS") => {
                Ok(Value::StreamMethod(t, format!("rich.{key}")))
            }
            (Value::Date(d), _) => Ok(Value::DateMethod(d, key)),
            (Value::Database(o), "Open" | "Remove" | "OpenExclusive" | "Create") => {
                Ok(Value::DatabaseMethod(o, key))
            }
            (Value::Database(o), _) => {
                let scope = Self::db_scope(&o)?;
                if let Some(host) = &self.dbhost {
                    if key == "NumberOfVolums" {
                        return Ok(Value::Number(host.borrow().volumes(scope)?.len() as f64));
                    }
                    if key == "Struct" {
                        let revision = host.borrow().schema_revision();
                        if matches!(o.borrow().get("\0schema_revision"),Some(Value::Number(n)) if *n==revision as f64)
                        {
                            return Ok(o.borrow()["Struct"].clone());
                        }
                        let Value::Database(db) =
                            Self::database(&host.borrow().schema(scope)?, scope)
                        else {
                            unreachable!()
                        };
                        let v = db.borrow()["Struct"].clone();
                        o.borrow_mut().insert("Struct".into(), v.clone());
                        o.borrow_mut()
                            .insert("\0schema_revision".into(), Value::Number(revision as f64));
                        return Ok(v);
                    }
                    if let Ok(i) = key.parse::<usize>() {
                        return Ok(host
                            .borrow()
                            .volumes(scope)?
                            .get(i)
                            .map(|v| Value::Volume(scope, v.clone()))
                            .unwrap_or(Value::Null));
                    }
                }
                Ok(o.borrow().get(&key).cloned().unwrap_or(Value::Null))
            }
            (Value::Volume(scope, handle), _) => {
                let host = self.dbhost.as_ref().ok_or("Volume host not configured")?;
                let v = host
                    .borrow()
                    .volumes(scope)?
                    .into_iter()
                    .find(|v| v.id == handle.id)
                    .ok_or_else(|| Fault::Script(Value::String("Volume is detached".into())))?;
                match key.as_str() {
                    "Struct" => {
                        let Value::Database(db) =
                            Self::database(&host.borrow().volume_schema(scope, v.id)?, scope)
                        else {
                            unreachable!()
                        };
                        let structure = db.borrow()["Struct"].clone();
                        Ok(structure)
                    }
                    "FileName" => Ok(Value::String(v.name)),
                    "toString" => Ok(Value::StringMethod(v.name, key)),
                    "name" => Ok(Value::String(v.title)),
                    "updatenum" => Ok(Value::Number(v.update as f64)),
                    "DataBase" => self.database_for(scope),
                    "readOnly" | "noWriteAccess" => {
                        Ok(Value::Bool(!host.borrow().volume_writable(scope, v.id)?))
                    }
                    "OpenExclusive" => Ok(Value::Bool(false)),
                    _ => Err(format!("Volume.{key} is not implemented").into()),
                }
            }
            // AppObj case 0x7fff000f (005a9da9 -> 005ab500) wraps
            // the execution context's database (+0x4c), not config.db4.
            // Scope 0 is this catalog runtime's default database.
            (Value::Application(_), "DataBase") => self.database_for(0),
            (Value::Application(_), "OSversion") => Ok(Value::String(format!(
                "{} · {} ({} bit)",
                std::env::consts::OS,
                std::env::consts::ARCH,
                usize::BITS
            ))),
            (Value::Application(_), "RuntimeVersion") => Ok(Value::String(format!(
                "codex-AIs {} · {} ({} bit)",
                crate::product::version(),
                std::env::consts::ARCH,
                usize::BITS
            ))),
            (Value::Application(_), "GetFindObjects") => Ok(Value::Native("GetFindObjects".into())),
            (Value::Application(_), "FindObjects") => Ok(Value::Native("FindObjects".into())),
            (Value::Native(n), "AddObject") if n == "FindObjects" => {
                Ok(Value::Native("FindObjects.AddObject".into()))
            }
            (Value::Application(_), "Servers") => {
                let server = Value::ReadOnlyObject(Rc::new(RefCell::new(HashMap::from([
                    (
                        "hostname".into(),
                        Value::String(
                            std::env::var("KODEKS_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
                        ),
                    ),
                    (
                        "port".into(),
                        Value::Number(
                            std::env::var("KODEKS_PORT")
                                .ok()
                                .and_then(|s| s.parse::<u16>().ok())
                                .unwrap_or(8070) as f64,
                        ),
                    ),
                    ("prefix".into(), Value::String(String::new())),
                ]))));
                Ok(Value::Array(Rc::new(RefCell::new(HashMap::from([
                    ("0".into(), server),
                    ("length".into(), Value::Number(1.0)),
                ])))))
            }
            (Value::Application(_), "MIME") => {
                let reg = self.registry.as_ref().ok_or("MIME registry unavailable")?;
                let key = reg
                    .open("SOFTWARE\\Kodeks\\Web\\MIME")?
                    .ok_or("MIME registry key absent")?;
                let entries = reg
                    .key(key)?
                    .values
                    .iter()
                    .map(|entry| {
                        Value::ReadOnlyObject(Rc::new(RefCell::new(HashMap::from([
                            ("Pattern".into(), Value::String(entry.name.clone())),
                            ("Type".into(), Value::String(entry.value.clone())),
                        ]))))
                    })
                    .collect::<Vec<_>>();
                let mut array = entries
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| (i.to_string(), v))
                    .collect::<HashMap<_, _>>();
                array.insert("length".into(), Value::Number(array.len() as f64));
                Ok(Value::Array(Rc::new(RefCell::new(array))))
            }
            (Value::Application(_), "RuntimeListenAddress") => Ok(Value::String(format!(
                "{}:{}",
                std::env::var("KODEKS_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
                std::env::var("KODEKS_PORT").unwrap_or_else(|_| "8070".into())
            ))),
            (Value::Native(n), "UseWinInet") if n == "InetConnection" => Ok(Value::Bool(false)),
            // Compatibility identity used by mp__ctxGetKsContext, recovered from
            // AppObj (005ab204, 005a9340). Toolbar/UserAgent are absent in kodweb.
            (Value::Application(_), "Name") => Ok(Value::String("KODWEB".into())),
            (Value::Application(app), "Version") => {
                // Startup rewrites RegInfo from these same saved fields. Keep
                // its native compatibility version (x86 or x64), so changing
                // the original executable does not require writing config.db4.
                // User-facing server information uses RuntimeVersion instead.
                if self
                    .config
                    .as_ref()
                    .is_some_and(|config| config.borrow().contains_key("RegInfo"))
                {
                    let metadata = self.member(Value::Application(app), "RegInfo".into())?;
                    if let Value::ReadOnlyObject(fields) = metadata {
                        if let Some(Value::String(version)) = fields.borrow().get("Version") {
                            return Ok(Value::String(version.clone()));
                        }
                    }
                }
                Ok(Value::String("5.3.2.120 (32 bit)".into()))
            }
            (Value::Application(_), "Toolbar" | "UserAgent" | "Protocol") => Ok(Value::Null),
            (Value::Application(_), "RegInfo") => {
                // Compatibility metadata previously saved by the installed original.
                // This does not fabricate a registration key or validate a new license.
                let config = self
                    .config
                    .as_ref()
                    .ok_or("Application configuration unavailable")?;
                let fields = config.borrow();
                let Some(Value::String(json)) = fields.get("RegInfo") else {
                    return Err(
                        "Application.RegInfo: original registration metadata missing".into(),
                    );
                };
                if json.len() > 16384 {
                    return Err("registration metadata size limit".into());
                }
                let ast = parser::parse(&format!("({json});"))
                    .map_err(|e| format!("registration metadata syntax: {e}"))?;
                let [Stmt::Expression(Expr::Object(items))] = ast.as_slice() else {
                    return Err("registration metadata must be an object".into());
                };
                let mut object = HashMap::new();
                for (name, expr) in items {
                    let value = match expr {
                        Expr::String(s) => Value::String(s.clone()),
                        Expr::Number(n) => Value::Number(*n),
                        Expr::Name(s) if s == "null" => Value::Null,
                        Expr::Name(s) if s == "true" || s == "false" => Value::Bool(s == "true"),
                        _ => {
                            return Err(
                                "registration metadata contains a non-scalar expression".into()
                            )
                        }
                    };
                    object.insert(name.clone(), value);
                }
                for key in [
                    "Dealer",
                    "Master",
                    "Network",
                    "Protect",
                    "RegNumber",
                    "UserName",
                ] {
                    if !object.contains_key(key) {
                        return Err(format!("registration metadata field {key} missing").into());
                    }
                }
                object.insert(
                    "GetAttribute".into(),
                    Value::Native("RegInfo.GetAttribute".into()),
                );
                Ok(Value::ReadOnlyObject(Rc::new(RefCell::new(object))))
            }
            (Value::Application(_), "Config") => Ok(Value::ReadOnlyObject(
                self.config
                    .clone()
                    .ok_or("Application configuration unavailable")?,
            )),
            (Value::Application(o), _) => o.borrow().get(&key).cloned().ok_or_else(|| {
                Fault::Host(format!("Application.{key}: host binding not implemented"))
            }),
            (Value::Host(super::host::Handle::Rows(rows)), "_Area") => Ok(rows
                .borrow()
                .area
                .map(|n| Value::Number(n as f64))
                .unwrap_or(Value::Null)),
            (Value::Host(super::host::Handle::Rows(rows)), "FreeTextStatistic") => {
                let rows = rows.borrow();
                if rows.fulltext.statistics.is_empty() {
                    return Ok(Value::Null);
                }
                let mut values = HashMap::new();
                for (i, (word, count)) in rows.fulltext.statistics.iter().enumerate() {
                    values.insert(
                        i.to_string(),
                        Value::ReadOnlyObject(Rc::new(RefCell::new(HashMap::from([
                            ("Word".into(), Value::String(word.clone())),
                            ("Number".into(), Value::Number(*count as f64)),
                        ])))),
                    );
                }
                values.insert(
                    "length".into(),
                    Value::Number(rows.fulltext.statistics.len() as f64),
                );
                Ok(Value::Array(Rc::new(RefCell::new(values))))
            }
            (Value::Host(h), _) => Ok(Value::HostMethod(h, key)),
            (Value::Object(o), "At") if !o.borrow().contains_key("At") => {
                Ok(Value::Native("At".into()))
            }
            (
                Value::Array(o),
                "push" | "pop" | "join" | "sort" | "search" | "concat" | "Remove" | "Delete",
            ) => Ok(Value::ArrayMethod(o, key)),
            (Value::Array(o), _) => Ok(o.borrow().get(&key).cloned().unwrap_or(Value::Null)),
            (Value::Native(n), "Config") if n == "Application" => Ok(Value::ReadOnlyObject(
                self.config
                    .clone()
                    .ok_or("Application configuration unavailable")?,
            )),
            (Value::Native(n), _) if n == "Application" => {
                Err(format!("Application.{key}: host binding not implemented").into())
            }
            (Value::ReadOnlyObject(o), _) => {
                match o.borrow().get(&key).cloned().unwrap_or(Value::Null) {
                    Value::ReadStream(stream)
                        if key == "Text" && o.borrow().contains_key("\0fonts") =>
                    {
                        // Native TextObject.Text opens a new reader. A probe such
                        // as checkEmptyRow must not consume a later header render.
                        let source = stream.borrow();
                        let value = InputStream::value(&source.data);
                        if let Value::ReadStream(copy) = &value {
                            copy.borrow_mut().encoded_extra = source.encoded_extra;
                        }
                        Ok(value)
                    }
                    Value::Unavailable(e) => Err(e.into()),
                    v => Ok(v),
                }
            }
            (Value::Object(o), _) => {
                if let Some(value) = Self::property(&o, &key)? {
                    return Ok(value);
                }
                if o.borrow().contains_key("\0json") {
                    return Ok(Value::Null);
                }
                if o.borrow().len() >= 10000 {
                    return Err("KS object member limit".into());
                }
                // KS reads create an undefined member. Original menu JSON retains
                // the previously probed Номера_Групп:null entry; `in` does not create.
                o.borrow_mut().insert(key, Value::Null);
                Ok(Value::Null)
            }
            (Value::Document(oid, scope), "Execute" | "Attr" | "Next" | "TextLength") => {
                Ok(Value::DocumentMethod(oid, scope, key))
            }
            (Value::Document(oid, scope), _) => Ok(self
                .doc_object(oid, scope)
                .borrow()
                .get(&key)
                .cloned()
                .unwrap_or(Value::Null)),
            (Value::Native(n), "Print" | "PrintS") if n == "stream" => {
                Ok(Value::Native(format!("stream.{key}")))
            }
            (Value::Decimal(d), "toString" | "toFormattedString") => {
                Ok(Value::DecimalMethod(d, key))
            }
            (Value::String(s), "length") => Ok(Value::Number(s.chars().count() as f64)),
            (
                Value::String(s),
                "match" | "replace" | "split" | "charAt" | "substring" | "indexOf" | "lastIndexOf"
                | "trim" | "toLowerCase" | "toUpperCase" | "toString" | "localeCompare" | "isDigit",
            ) => Ok(Value::StringMethod(s, key)),
            (v @ (Value::Number(_) | Value::Bool(_)), "toString") => {
                Ok(Value::StringMethod(v.text(), key))
            }
            (Value::String(_) | Value::Number(_) | Value::Bool(_), _) => Ok(Value::Null),
            (value, _) => Err(format!(
                "unsupported KS member {key} on {:?}",
                std::mem::discriminant(&value)
            )
            .into()),
        }
    }
    fn eval(&mut self, e: &Expr) -> Result<Value, Fault> {
        self.tick()?;
        match e {
            Expr::Regex(pattern, flags) => {
                if flags.chars().any(|c| !"ims".contains(c)) {
                    return Err(format!("unsupported KS regex flags {flags}").into());
                }
                let re = regex::RegexBuilder::new(pattern)
                    .case_insensitive(flags.contains('i'))
                    .multi_line(flags.contains('m'))
                    .dot_matches_new_line(flags.contains('s'))
                    .size_limit(1024 * 1024)
                    .dfa_size_limit(1024 * 1024)
                    .build()
                    .map_err(|e| format!("unsupported regex: {e}"))?;
                Ok(Value::Regex(re))
            }
            Expr::String(s) => Ok(Value::String(s.clone())),
            Expr::Number(n) => Ok(Value::Number(*n)),
            Expr::Name(n) => self.get(n),
            Expr::Function(name, a, b) => Ok(self.function(name.clone(), a.clone(), b.clone())),
            Expr::Object(fields) => {
                if fields.len() > 10000 {
                    return Err("KS object member limit".into());
                }
                let mut map = HashMap::new();
                for (k, e) in fields {
                    map.insert(k.clone(), self.eval(e)?);
                }
                Ok(Value::Object(Rc::new(RefCell::new(map))))
            }
            Expr::Array(values) => {
                if values.len() > u32::MAX as usize {
                    return Err("KS array limit".into());
                }
                let mut map = HashMap::new();
                for (i, e) in values.iter().enumerate() {
                    map.insert(i.to_string(), self.eval(e)?);
                }
                map.insert("length".into(), Value::Number(values.len() as f64));
                Ok(Value::Array(Rc::new(RefCell::new(map))))
            }
            Expr::Conditional(c, a, b) => {
                if self.eval(c)?.truth() {
                    self.eval(a)
                } else {
                    self.eval(b)
                }
            }
            Expr::Unary(op, e) => {
                if op == "new" {
                    let Expr::Call(f, args) = e.as_ref() else {
                        return Err("unsupported constructor form".into());
                    };
                    if let Expr::Name(n) = f.as_ref() {
                        match n.as_str() {
                            "File" => {
                                if args.is_empty() || args.len() > 2 {
                                    return Err("File expects path[, create]".into());
                                }
                                let value = self.eval(&args[0])?;
                                if !matches!(value, Value::String(_) | Value::FilePath(_)) {
                                    return Err("File requires a path".into());
                                }
                                let create = args.len() == 2 && self.eval(&args[1])?.truth();
                                return self.open_file(&value.text(), create);
                            }
                            "FilePath" => {
                                if args.len() > 1 {
                                    return Err("FilePath expects a path".into());
                                }
                                let value = if args.is_empty() {
                                    Value::String(String::new())
                                } else {
                                    self.eval(&args[0])?
                                };
                                let value = if matches!(value, Value::Null) {
                                    Value::String(String::new())
                                } else {
                                    value
                                };
                                if !matches!(value, Value::String(_) | Value::FilePath(_)) {
                                    return Err("FilePath requires a string or FilePath".into());
                                }
                                let text = super::path::script_path(&value.text());
                                if text.contains('\0') {
                                    return Err("FilePath contains NUL".into());
                                }
                                return Ok(Value::FilePath(Rc::new(RefCell::new(text))));
                            }
                            "Date" => {
                                let mut values = Vec::new();
                                for a in args {
                                    values.push(self.eval(a)?);
                                }
                                let d = match values.as_slice() {
                                    [] => super::date::Date::now()?,
                                    [Value::Date(d)] => d.borrow().clone(),
                                    [Value::String(s)] => super::date::Date::from_text(s)
                                        .map_err(|e| Fault::Script(Value::String(e)))?,
                                    [v] => super::date::Date::from_number(v.number()?)?,
                                    v if (3..=7).contains(&v.len()) => {
                                        let mut p = [0, 0, 1, 0, 0, 0, 0];
                                        for (i, x) in v.iter().enumerate() {
                                            let n = x.number()?;
                                            if !n.is_finite() || n.abs() > 10_000_000.0 {
                                                return Err(Fault::Script(Value::String(
                                                    "Date component range".into(),
                                                )));
                                            }
                                            p[i] = n as i64;
                                        }
                                        let date = super::date::Date::from_parts(p)
                                            .map_err(|e| Fault::Script(Value::String(e)))?;
                                        date.check_calendar()
                                            .map_err(|e| Fault::Script(Value::String(e)))?;
                                        date
                                    }
                                    _ => {
                                        return Err("unsupported Date constructor arguments".into())
                                    }
                                };
                                return Ok(Value::Date(Rc::new(RefCell::new(d))));
                            }
                            "String" => {
                                if args.len() > 1 {
                                    return Err("String accepts at most one argument".into());
                                }
                                return Ok(Value::String(if let Some(a) = args.first() {
                                    self.eval(a)?.text()
                                } else {
                                    String::new()
                                }));
                            }
                            "Signal" => {
                                if args.len() != 1 {
                                    return Err("Signal expects one name".into());
                                }
                                let name = self.eval(&args[0])?.text();
                                if self.http_replica {
                                    if name.is_empty() || name.len() > 1024 || name.contains('\0') {
                                        return Err("invalid Signal name".into());
                                    }
                                    if !self.replica_signals.contains_key(&name)
                                        && self.replica_signals.len() >= 2048
                                    {
                                        return Err("named Signal limit".into());
                                    }
                                    return Ok(Value::Signal(
                                        self.replica_signals.entry(name).or_default().clone(),
                                    ));
                                }
                                return Ok(Value::Signal(super::signal::Signal::named(&name)?));
                            }
                            "RTFStylesForKodeks" | "RTFFieldsForKodeks" => {
                                if !args.is_empty() { return Err("RTF mapping constructor expects no arguments".into()); }
                                return Ok(Value::Object(Rc::new(RefCell::new(HashMap::from([
                                    ("addEntry".into(), Value::Native("rtf.mapping".into())),
                                    ("addForHyperlinks".into(), Value::Native("rtf.mapping".into())),
                                    ("addForTOCentries".into(), Value::Native("rtf.mapping".into())),
                                ])))));
                            }
                            "CSSStyleSheet" => {
                                if !args.is_empty() {
                                    return Err("CSSStyleSheet expects no arguments".into());
                                }
                                return Ok(Value::Css(Rc::new(RefCell::new(Vec::new()))));
                            }
                            "KodeksFormatParser"
                            | "KodeksDOMParser"
                            | "KodeksFormatHTMLWriter"
                            | "KodeksFormatRTFWriter"
                            | "KodeksTextSections" => {
                                let values = args
                                    .iter()
                                    .map(|a| self.eval(a))
                                    .collect::<Result<Vec<_>, _>>()?;
                                return self.format_object(n, values);
                            }
                            "FormDoc" | "HashFormDoc" => {
                                let values = args
                                    .iter()
                                    .map(|a| self.eval(a))
                                    .collect::<Result<Vec<_>, _>>()?;
                                return self.make_form(values);
                            }
                            "List" => {
                                if args.len() > 2 {
                                    return Err("List expects [area[, database]]".into());
                                }
                                let area_value = if args.is_empty() {
                                    Value::Number(0.0)
                                } else {
                                    self.eval(&args[0])?
                                };
                                let scope = if let Some(arg) = args.get(1) {
                                    let value = self.eval(arg)?;
                                    match value {
                                        Value::Database(o) => Self::db_scope(&o)?,
                                        _ => {
                                            return Err(
                                                "List second argument must be DataBase".into()
                                            )
                                        }
                                    }
                                } else {
                                    0
                                };
                                let area = self.list_area(&area_value, scope)?;
                                return Ok(Value::List(Rc::new(RefCell::new(
                                    record_list::RecordList {
                                        scope,
                                        area,
                                        order: None,
                                        conditions: Vec::new(),
                                        query_all: area != 0,
                                        ids: record_list::ListIds::new(None),
                                    },
                                ))));
                            }
                            "Mutex" => {
                                if args.len() > 1 {
                                    return Err("Mutex expects an optional name".into());
                                }
                                let mutex = if let Some(arg) = args.first() {
                                    super::mutex::NamedMutex::new(&self.eval(arg)?.text())?
                                } else {
                                    super::mutex::NamedMutex::unnamed()
                                };
                                let handle = Rc::new(RefCell::new(mutex));
                                self.mutex_handles.push(Rc::downgrade(&handle));
                                return Ok(Value::Mutex(handle));
                            }
                            "DataBase" => {
                                if !args.is_empty() {
                                    return Err("DataBase expects no arguments".into());
                                }
                                let scope = self
                                    .dbhost
                                    .as_ref()
                                    .ok_or("DataBase host not configured")?
                                    .borrow_mut()
                                    .create()?;
                                return self.database_for(scope);
                            }
                            "LongDouble" => {
                                if args.len() > 1 {
                                    return Err("LongDouble expects one value".into());
                                }
                                let value = if let Some(arg) = args.first() {
                                    self.eval(arg)?.text()
                                } else {
                                    "0".into()
                                };
                                return Ok(Value::Decimal(decimal::Decimal::parse(&value)?));
                            }
                            "VirtualText" => {
                                if !args.is_empty() {
                                    return Err("VirtualText expects no arguments".into());
                                }
                                return Ok(InputStream::value(&[]));
                            }
                            "XMLBaseTagWriter" | "SAXTagFilter" => {
                                let values = args
                                    .iter()
                                    .map(|a| self.eval(a))
                                    .collect::<Result<Vec<_>, _>>()?;
                                return self.report_xml_writer(n, values);
                            }
                            "XMLSAXparser" => {
                                if args.len() != 1 {
                                    return Err("XMLSAXparser expects a stream".into());
                                }
                                let stream = self.eval(&args[0])?;
                                if !matches!(stream, Value::ReadStream(_)) {
                                    return Err("XMLSAXparser requires Stream".into());
                                }
                                return Ok(Value::Object(Rc::new(RefCell::new(HashMap::from([
                                    ("\0stream".into(), stream),
                                    ("Parse".into(), Value::Native("xml.Parse".into())),
                                    (
                                        "SetEncoding".into(),
                                        Value::Native("xml.SetEncoding".into()),
                                    ),
                                ])))));
                            }
                            "Document" => {
                                if args.is_empty() {
                                    return self.new_edit_document();
                                }
                                if !(1..=2).contains(&args.len()) {
                                    return Err(
                                        "Document expects OID[, database] or a database".into()
                                    );
                                }
                                let value = self.eval(&args[0])?;
                                if args.len() == 1 && self.document_write_allowed {
                                    if let Value::Database(db) = &value {
                                        return self.new_edit_document_in(Self::db_scope(db)?);
                                    }
                                }
                                if args.len() == 1
                                    && matches!(
                                        value,
                                        Value::Database(_) | Value::Null | Value::Number(0.0)
                                    )
                                {
                                    return Ok(Value::TempDocument(Rc::new(RefCell::new(
                                        TempDocument {
                                            properties: Rc::new(RefCell::new(HashMap::from([(
                                                "Base".into(),
                                                value,
                                            )]))),
                                            fields: HashMap::new(),
                                        },
                                    ))));
                                }
                                let scope = if args.len() == 2 {
                                    match self.eval(&args[1])? {
                                        Value::Null => 0,
                                        Value::Database(o) => Self::db_scope(&o)?,
                                        _ => {
                                            return Err(
                                                "Document requires a DataBase argument".into()
                                            )
                                        }
                                    }
                                } else {
                                    0
                                };
                                let oid = value.number()?;
                                // Native /users creates an unsaved UserBase
                                // record with Document(0, UserBase).
                                if oid == 0.0 && args.len() == 2 {
                                    return self.new_edit_document_in(scope);
                                }
                                if !oid.is_finite()
                                    || oid <= 0.0
                                    || oid > u32::MAX as f64
                                    || oid.fract() != 0.0
                                {
                                    return Err(Fault::Script(Value::String(
                                        "invalid Document OID".into(),
                                    )));
                                }
                                if self.documents.is_some() || self.dbhost.is_some() {
                                    self.document_metadata(oid as u32, scope)?;
                                }
                                return Ok(Value::Document(oid as u32, scope));
                            }
                            "Array" => {
                                let values = args
                                    .iter()
                                    .map(|a| self.eval(a))
                                    .collect::<Result<Vec<_>, _>>()?;
                                let mut fields = HashMap::new();
                                let length = if let [Value::Number(n)] = values.as_slice() {
                                    if !n.is_finite()
                                        || *n < 0.0
                                        || *n > u32::MAX as f64
                                        || n.fract() != 0.0
                                    {
                                        return Err("Array length limit".into());
                                    }
                                    *n as usize
                                } else {
                                    if values.len() > u32::MAX as usize {
                                        return Err("Array length limit".into());
                                    }
                                    let length = values.len();
                                    fields.extend(
                                        values
                                            .into_iter()
                                            .enumerate()
                                            .map(|(i, v)| (i.to_string(), v)),
                                    );
                                    length
                                };
                                fields.insert("length".into(), Value::Number(length as f64));
                                return Ok(Value::Array(Rc::new(RefCell::new(fields))));
                            }
                            "FontAttribute" => {
                                if !args.is_empty() {
                                    return Err("FontAttribute expects no arguments".into());
                                }
                                let mut o = HashMap::new();
                                for name in ["bold", "italic", "underline", "strikeout"] {
                                    o.insert(name.into(), Value::Bool(false));
                                }
                                o.insert(
                                    "color".into(),
                                    Value::Object(Rc::new(RefCell::new(HashMap::from([
                                        ("red".into(), Value::Number(0.0)),
                                        ("green".into(), Value::Number(0.0)),
                                        ("blue".into(), Value::Number(0.0)),
                                    ])))),
                                );
                                return Ok(Value::Object(Rc::new(RefCell::new(o))));
                            }
                            "Object" => {
                                if !args.is_empty() {
                                    return Err("constructor arguments not yet implemented".into());
                                }
                                return Ok(Value::Object(Rc::new(RefCell::new(HashMap::new()))));
                            }
                            _ => {}
                        }
                    }
                    if let Value::Function(f) = self.eval(f)? {
                        let mut values = Vec::new();
                        for a in args {
                            values.push(self.eval(a)?);
                        }
                        let mut fields = HashMap::new();
                        if let Some(Value::Object(proto)) = f.properties.borrow().get("prototype") {
                            fields.insert("\0prototype".into(), Value::Object(proto.clone()));
                        }
                        fields.insert("\0constructor".into(), Value::Function(f.clone()));
                        if let Some(name) = f
                            .name
                            .clone()
                            .or_else(|| f.properties.borrow().get("\0name").map(Value::text))
                        {
                            fields.insert("\0typename".into(), Value::String(name));
                        }
                        let object = Value::Object(Rc::new(RefCell::new(fields)));
                        let result =
                            self.invoke(f, values, Some(object.clone()), "constructor".into())?;
                        return Ok(
                            if matches!(
                                result,
                                Value::Object(_) | Value::Array(_) | Value::Function(_)
                            ) {
                                result
                            } else {
                                object
                            },
                        );
                    }
                    return Err(format!("constructor not implemented: {f:?}").into());
                }

                if op == "delete" {
                    let Expr::Member(base, key) = e.as_ref() else {
                        return Err("delete requires a property".into());
                    };
                    let base = self.eval(base)?;
                    let key = self.eval(key)?.text();
                    match base {
                        Value::Object(o) | Value::Array(o) => {
                            self.numeric_arrays.invalidate(&o);
                            o.borrow_mut().remove(&key);
                        }
                        Value::Function(f) => {
                            if key != "prototype" {
                                if let Some(Value::Object(prototype)) =
                                    f.properties.borrow().get("prototype")
                                {
                                    prototype.borrow_mut().remove(&key);
                                }
                            }
                            f.properties.borrow_mut().remove(&key);
                        }
                        _ => return Err("delete requires a writable object".into()),
                    }
                    return Ok(Value::Bool(true));
                }
                if op == "void" {
                    self.eval(e)?;
                    return Ok(Value::Null);
                }
                if matches!(op.as_str(), "++" | "--" | "post++" | "post--") {
                    let location = match e.as_ref() {
                        Expr::Name(_) => None,
                        Expr::Member(base, key) => Some((self.eval(base)?, self.eval(key)?.text())),
                        _ => return Err("increment requires a variable or property".into()),
                    };
                    let old = if let Some((obj, key)) = &location {
                        self.member(obj.clone(), key.clone())?.number()?
                    } else {
                        self.eval(e)?.number()?
                    };
                    let new = old + if op.ends_with("++") { 1.0 } else { -1.0 };
                    if let Some((obj, key)) = location {
                        self.assign_member(obj, key, Value::Number(new))?;
                    } else {
                        self.assign(e, Value::Number(new))?;
                    }
                    return Ok(Value::Number(if op.starts_with("post") {
                        old
                    } else {
                        new
                    }));
                }
                let v = self.eval(e)?;
                if op == "typeof" {
                    let field = match &v { Value::StoredField(f)=>Some(f.clone()), Value::WritableField(f)=>Some(f.snapshot()?), _=>None };
                    if field.is_some_and(|f|f.data().wire_type==5) {return Ok(Value::String("LinkField".into()));}
                    if matches!(v, Value::Host(super::host::Handle::Rows(_))) {
                        return Ok(Value::String("JDBCResult".into()));
                    }
                    if let Value::Object(object) = &v {
                        if let Some(Value::String(name)) = object.borrow().get("\0typename") {
                            return Ok(Value::String(name.clone()));
                        }
                    }
                }
                match op.as_str(){"~"=>Ok(Value::Number((!to_i32(v.number()?)) as f64)),"!"=>Ok(Value::Bool(!v.truth())),"-"=>Ok(Value::Number(-v.number()?)),"+"=>Ok(Value::Number(v.number()?)),"typeof"=>Ok(Value::String(match v{Value::EditDocument(_)|Value::TempDocument(_)=>"Document",Value::ReadStream(_)|Value::TextStream(_)|Value::RichStream(_)|Value::LogFile(_)|Value::File(_)|Value::HttpOutput=>"Stream",Value::Date(_)=>"Date",Value::Database(_)=>"DataBase",Value::Host(super::host::Handle::Rows(_))=>"JDBCResult",Value::Null=>"undefined",Value::ScriptException(_)=>"Error",Value::String(_)=>"String",Value::Decimal(_)=>"LongDouble",Value::Number(_)=>"number",Value::Bool(_)=>"boolean",Value::Array(_)=>"Array",Value::Document(_,_)=>"Document",Value::Volume(_,_)=>"Volume",Value::Object(ref o) if matches!(&self.scopes[0].borrow()["this"],Value::Object(g) if Rc::ptr_eq(g,o))=>"BaseVirtualDir",Value::Object(_)=>"Object",Value::Function(..)=>"Function",_=>"Object"}.into())),_=>Err(format!("unsupported KS unary operator {op}").into())}
            }
            Expr::Binary(op, a, b) => {
                if op == "=" || matches!(op.as_str(), "+=" | "-=" | "*=" | "/=") {
                    let v = if op == "=" {
                        self.eval(b)?
                    } else {
                        self.eval(&Expr::Binary(op[..1].into(), a.clone(), b.clone()))?
                    };
                    self.assign(a, v.clone())?;
                    return Ok(v);
                }
                let x = self.eval(a)?;
                if op == "&&" && !x.truth() || op == "||" && x.truth() {
                    return Ok(x);
                }
                let y = self.eval(b)?;
                if matches!(x, Value::Decimal(_)) || matches!(y, Value::Decimal(_)) {
                    if matches!(op.as_str(), "==" | "!=")
                        && (matches!(x, Value::Null) || matches!(y, Value::Null))
                    {
                        return Ok(Value::Bool(op == "!="));
                    }
                    if ["+", "-", "*", "/", "%", "<", ">", "<=", ">=", "==", "!="]
                        .contains(&op.as_str())
                        && !(op == "+"
                            && (matches!(x, Value::String(_)) || matches!(y, Value::String(_))))
                    {
                        let a = decimal::Decimal::parse(&x.text())?;
                        let b = decimal::Decimal::parse(&y.text())?;
                        if ["<", ">", "<=", ">=", "==", "!="].contains(&op.as_str()) {
                            let c = a.cmp(b)?;
                            return Ok(Value::Bool(match op.as_str() {
                                "<" => c.is_lt(),
                                ">" => c.is_gt(),
                                "<=" => !c.is_gt(),
                                ">=" => !c.is_lt(),
                                "==" => c.is_eq(),
                                _ => !c.is_eq(),
                            }));
                        }
                        return Ok(Value::Decimal(a.op(op, b)?));
                    }
                }
                match op.as_str() {
                    "instanceof" => {
                        let Value::Function(constructor) = y else {
                            return Err("instanceof requires a constructor".into());
                        };
                        let Some(Value::Object(target)) =
                            constructor.properties.borrow().get("prototype").cloned()
                        else {
                            return Err("instanceof constructor prototype is not an object".into());
                        };
                        let mut current = match x {
                            Value::Object(o) => Some(o),
                            _ => None,
                        };
                        let mut seen = std::collections::HashSet::new();
                        let mut found = false;
                        while let Some(object) = current {
                            if !seen.insert(Rc::as_ptr(&object) as usize) || seen.len() > 64 {
                                return Err("prototype chain cycle/depth limit".into());
                            }
                            current = match object.borrow().get("\0prototype") {
                                Some(Value::Object(p)) => Some(p.clone()),
                                _ => None,
                            };
                            if current.as_ref().is_some_and(|p| Rc::ptr_eq(p, &target)) {
                                found = true;
                                break;
                            }
                        }
                        Ok(Value::Bool(found))
                    }
                    "&" => Ok(Value::Number(
                        (to_i32(x.number()?) & to_i32(y.number()?)) as f64,
                    )),
                    "|" => Ok(Value::Number(
                        (to_i32(x.number()?) | to_i32(y.number()?)) as f64,
                    )),
                    "^" => Ok(Value::Number(
                        (to_i32(x.number()?) ^ to_i32(y.number()?)) as f64,
                    )),
                    "<<" => Ok(Value::Number(
                        to_i32(x.number()?).wrapping_shl((to_i32(y.number()?) as u32) & 31) as f64,
                    )),
                    ">>" => Ok(Value::Number(
                        (to_i32(x.number()?) >> ((to_i32(y.number()?) as u32) & 31)) as f64,
                    )),
                    ">>>" => Ok(Value::Number(
                        ((to_i32(x.number()?) as u32) >> ((to_i32(y.number()?) as u32) & 31))
                            as f64,
                    )),
                    "<" | ">" | "<=" | ">="
                        if matches!((&x, &y), (Value::String(_), Value::String(_))) =>
                    {
                        let (a, b) = (x.text(), y.text());
                        Ok(Value::Bool(match op.as_str() {
                            "<" => a < b,
                            ">" => a > b,
                            "<=" => a <= b,
                            _ => a >= b,
                        }))
                    }
                    "in" => match y {
                        Value::Object(o) => {
                            Ok(Value::Bool(Self::property(&o, &x.text())?.is_some()))
                        }
                        Value::Function(f) => {
                            let properties = f.properties.borrow();
                            let key = x.text();
                            let inherited = if let Some(Value::Object(prototype)) =
                                properties.get("prototype")
                            {
                                Self::property(prototype, &key)?.is_some()
                            } else {
                                false
                            };
                            Ok(Value::Bool(properties.contains_key(&key) || inherited))
                        }
                        Value::Array(o)
                        | Value::ReadOnlyObject(o)
                        | Value::Database(o)
                        | Value::Application(o) => {
                            Ok(Value::Bool(o.borrow().contains_key(&x.text())))
                        }
                        _ => Err("right operand of in must be an object".into()),
                    },
                    "," | "&&" | "||" => Ok(y),
                    "+" if matches!(x, Value::String(_)) || matches!(y, Value::String(_)) => {
                        let text = x.text() + &y.text();
                        if text.len() > 4 * 1024 * 1024 {
                            return Err("KS string limit".into());
                        }
                        Ok(Value::String(text))
                    }
                    "+" => Ok(Value::Number(x.number()? + y.number()?)),
                    "-" => Ok(Value::Number(x.number()? - y.number()?)),
                    "*" => Ok(Value::Number(x.number()? * y.number()?)),
                    "/" => Ok(Value::Number(x.number()? / y.number()?)),
                    "%" => Ok(Value::Number(x.number()? % y.number()?)),
                    "<" => Ok(Value::Bool(x.number()? < y.number()?)),
                    ">" => Ok(Value::Bool(x.number()? > y.number()?)),
                    "<=" => Ok(Value::Bool(x.number()? <= y.number()?)),
                    ">=" => Ok(Value::Bool(x.number()? >= y.number()?)),
                    "==" => Ok(Value::Bool(field_equal(&x, &y))),
                    "!=" => Ok(Value::Bool(!field_equal(&x, &y))),
                    "===" => Ok(Value::Bool(equal(&x, &y))),
                    "!==" => Ok(Value::Bool(!equal(&x, &y))),
                    _ => Err(format!("unsupported KS binary operator {op}").into()),
                }
            }
            Expr::Member(base, key) => {
                let key = self.eval(key)?.text();

                let base = self.eval(base)?;
                self.member(base, key)
            }
            Expr::Call(f, args) => {
                let (f, receiver, label) = if let Expr::Member(base, key) = f.as_ref() {
                    let base = self.eval(base)?;
                    let key = self.eval(key)?.text();
                    let value = self.member(base.clone(), key.clone())?;
                    // Native KS dispatch tables (Classifier.modeFormList) keep
                    // the calling object's this when invoking an array item.
                    let receiver =
                        if matches!((&base, &value), (Value::Array(_), Value::Function(_))) {
                            None
                        } else {
                            Some(base)
                        };
                    (value, receiver, key)
                } else {
                    let label = if let Expr::Name(n) = f.as_ref() {
                        n.clone()
                    } else {
                        "expression".into()
                    };
                    let mut receiver = None;
                    if let Expr::Name(name) = f.as_ref() {
                        for o in self.with_objects.iter().rev() {
                            if Self::property(o, name)?.is_some() {
                                receiver = Some(Value::Object(o.clone()));
                                break;
                            }
                        }
                    }
                    (self.eval(f)?, receiver, label)
                };
                let mut values = Vec::new();
                for a in args {
                    values.push(self.eval(a)?);
                }
                match f {
                    Value::Function(f) => self.invoke(f, values, receiver, label),
                    f => self.call_host(f, values, receiver, label),
                }
            }
        }
    }
    fn call_host(
        &mut self,
        f: Value,
        values: Vec<Value>,
        receiver: Option<Value>,
        label: String,
    ) -> Result<Value, Fault> {
        match f {
            Value::EditMethod(doc, method) => self.edit_call(doc, &method, &values),
            Value::StoredFieldMethod(field, method) => {
                if method == "MarkContext" {
                    if values.is_empty() || values.len() > 3 {
                        return Err("MarkContext expects text[, positions, advanced]".into());
                    }
                    let key = (field.scope, field.oid, field.index);
                    self.context_marks.remove(&key);
                    let Some(pattern) =
                        context_mark::pattern_at(&values[0].text(), self.data_root.as_deref())?
                    else {
                        return Ok(Value::Bool(false));
                    };
                    if values
                        .get(1)
                        .is_some_and(|v| !matches!(v, Value::Null) && !v.text().trim().is_empty())
                    {
                        return Err("context position highlighting is not implemented".into());
                    }
                    let html = self.rich_html(&Value::StoredField(field))?;
                    let (_, count) = context_mark::highlight(&html, &pattern)?;
                    if count > 0 {
                        self.context_marks.insert(key, pattern);
                    }
                    return Ok(Value::Bool(count > 0));
                }
                if method == "TextLength" {
                    if !values.is_empty() {
                        return Err("TextLength expects no arguments".into());
                    }
                    return Ok(Value::Number(
                        crate::document::bytes(field.data())?.len() as f64
                    ));
                }
                if values.is_empty() || values.len() > 2 {
                    return Err("ToStream expects stream[, format]".into());
                }
                if values
                    .get(1)
                    .is_some_and(|v| !matches!(v, Value::Null) && v.text() != "text")
                {
                    return Err("ToStream format not implemented".into());
                }
                // A text stream contains characters, not the KText font-run
                // delimiters. In particular XML attribute names can follow a
                // formatting delimiter in stored registry definitions.
                let plain: Vec<u8> = crate::document::bytes(field.data())?
                    .iter()
                    .copied()
                    .filter(|b| !matches!(b, 0 | 1))
                    .collect();
                let bytes = plain.as_slice();
                match &values[0] {
                    Value::ReadStream(s) => {
                        let mut s = s.borrow_mut();
                        if s.closed {
                            return Err("stream closed".into());
                        }
                        if s.data.len() + bytes.len() > 16 * 1024 * 1024 {
                            return Err("stream size limit".into());
                        }
                        // ToStream fills a readable virtual stream; writing does not consume its read cursor.
                        s.data.extend_from_slice(bytes);
                    }
                    Value::HttpOutput => self.write(&crate::db::decode(bytes))?,
                    _ => return Err("ToStream target must be a Stream".into()),
                }
                Ok(Value::Null)
            }
            Value::SignalMethod(s, method) => {
                if method == "Wait" {
                    let [n] = values.as_slice() else {
                        return Err("Signal.Wait expects seconds".into());
                    };
                    let n = n.number()?;
                    if !n.is_finite() || n < 0.0 || n > 30.0 || n.fract() != 0.0 {
                        return Err("Signal.Wait limit: 0..30 seconds".into());
                    }
                    Ok(Value::Bool(s.wait(n as u32)?))
                } else if method == "InfiniteWait" {
                    Err("Signal.InfiniteWait requires concurrent request dispatch and is not implemented".into())
                } else {
                    if !values.is_empty() {
                        return Err("Signal switch takes no arguments".into());
                    }
                    s.switch(method == "SwitchOn")?;
                    Ok(Value::Null)
                }
            }
            Value::LogMethod(file, method) => {
                use std::io::{Seek, Write};
                let mut file = file.borrow_mut();
                match method.as_str() {
                    "writeByte" => {
                        let [value] = values.as_slice() else {
                            return Err("writeByte expects one byte".into());
                        };
                        let n = value.number()?;
                        if !n.is_finite() || n.fract() != 0.0 || !(-128.0..=255.0).contains(&n) {
                            return Err("writeByte range".into());
                        }
                        file.write_all(&[n as i16 as u8])
                            .map_err(|e| e.to_string())?;
                        Ok(Value::Null)
                    }
                    "length" => {
                        if !values.is_empty() {
                            return Err("length takes no arguments".into());
                        }
                        Ok(Value::Number(
                            file.metadata().map_err(|e| e.to_string())?.len() as f64,
                        ))
                    }
                    "skip" => {
                        let [n] = values.as_slice() else {
                            return Err("skip expects a byte count".into());
                        };
                        let n = n.number()?;
                        if !n.is_finite()
                            || n < 0.0
                            || n.fract() != 0.0
                            || n > 16.0 * 1024.0 * 1024.0
                        {
                            return Err("log seek limit".into());
                        }
                        file.seek(std::io::SeekFrom::Current(n as i64))
                            .map_err(|e| e.to_string())?;
                        Ok(Value::Null)
                    }
                    "Print" | "PrintS" => {
                        for value in values {
                            let text = value.text();
                            let (bytes, _, bad) = encoding_rs::WINDOWS_1251.encode(&text);
                            if bad {
                                return Err("log encoding error".into());
                            }
                            if file.stream_position().map_err(|e| e.to_string())?
                                + bytes.len() as u64
                                > 16 * 1024 * 1024
                            {
                                return Err("log file size limit".into());
                            }
                            file.write_all(&bytes).map_err(|e| e.to_string())?;
                        }
                        file.flush().map_err(|e| e.to_string())?;
                        Ok(Value::Null)
                    }
                    _ => Err(format!("log File.{method} is not implemented").into()),
                }
            }
            Value::MutexMethod(m, method) => {
                if method == "TryLock" {
                    if values.len() > 1 {
                        return Err("TryLock expects optional milliseconds".into());
                    }
                    let ms = values
                        .first()
                        .map(Value::number)
                        .transpose()?
                        .unwrap_or(0.0);
                    if !ms.is_finite() || ms < 0.0 || ms > 30000.0 {
                        return Err("Mutex timeout must be 0..30000 ms".into());
                    }
                    return Ok(Value::Bool(
                        m.borrow_mut()
                            .acquire(Some(std::time::Duration::from_millis(ms as u64)))?,
                    ));
                }
                if !values.is_empty() {
                    return Err("Mutex method expects no arguments".into());
                }
                if method == "Lock" {
                    m.borrow_mut().acquire(None)?;
                } else {
                    m.borrow_mut().release()?;
                }
                Ok(Value::Null)
            }
            Value::RegistryMethod(id, method) => {
                let reg = self
                    .registry
                    .as_ref()
                    .ok_or("RegistryRoot is not configured")?;
                let entry_value = |e: &crate::db::registry::Entry| -> Result<Value, Fault> {
                    let current = e.current_value(self.data_root.as_deref(), id == reg.root)?;
                    Ok(Value::ReadOnlyObject(Rc::new(RefCell::new(HashMap::from(
                        [
                            ("NameVal".into(), Value::String(e.name.clone())),
                            (
                                "NameType".into(),
                                match e.kind.as_str() {
                                    "REG_SZ" => Value::Number(1.0),
                                    "REG_DWORD" => Value::Number(4.0),
                                    _ => Value::Unavailable(format!(
                                        "unsupported registry type {}",
                                        e.kind
                                    )),
                                },
                            ),
                            ("Value".into(), Value::String(current)),
                        ],
                    )))))
                };
                match method.as_str() {
                    "OpenKey" | "CreateKey" => {
                        let [Value::String(path)] = values.as_slice() else {
                            return Err("registry key method expects one absolute path".into());
                        };
                        let key = reg.open(path)?;
                        if method == "CreateKey" {
                            if key.is_none() {
                                return Err(format!("RegistryRoot.CreateKey would write {path}; persistent registry writes are not implemented").into());
                            }
                            return Ok(Value::Null);
                        }
                        Ok(key.map(Value::RegistryKey).unwrap_or(Value::Null))
                    }
                    "EnumKey" | "EnumValue" => {
                        let [Value::Number(n)] = values.as_slice() else {
                            return Err("registry enumeration expects one index".into());
                        };
                        if !n.is_finite() || *n < 0.0 || n.fract() != 0.0 || *n > u32::MAX as f64 {
                            return Err("invalid registry index".into());
                        }
                        let key = reg.key(id)?;
                        Ok(if method == "EnumKey" {
                            key.children
                                .get(*n as usize)
                                .map(|id| Value::RegistryKey(*id))
                                .unwrap_or(Value::Null)
                        } else {
                            key.values
                                .get(*n as usize)
                                .map(entry_value)
                                .transpose()?
                                .unwrap_or(Value::Null)
                        })
                    }
                    "QueryValue" => {
                        let [Value::String(name)] = values.as_slice() else {
                            return Err("QueryValue expects one name".into());
                        };
                        Ok(reg
                            .query(id, name)?
                            .map(entry_value)
                            .transpose()?
                            .unwrap_or(Value::Null))
                    }
                    "SetValue" => {
                        let [Value::String(name), Value::Number(kind), value] = values.as_slice()
                        else {
                            return Err("SetValue expects name, type, value".into());
                        };
                        let kind = match *kind as i32 {
                            1 if *kind == 1.0 => "REG_SZ",
                            4 if *kind == 4.0 => "REG_DWORD",
                            _ => return Err("unsupported registry value type".into()),
                        };
                        if let Some(old) = reg.query(id, name)? {
                            if old.kind == kind
                                && old.current_value(self.data_root.as_deref(), id == reg.root)?
                                    == value.text()
                            {
                                return Ok(Value::Null);
                            }
                            if self.registration_write_allowed
                                && id == reg.root
                                && crate::db::writer::counters::NAMES
                                    .iter()
                                    .any(|n| n.eq_ignore_ascii_case(name))
                                && old.kind == kind
                            {
                                let next = value.text();
                                if !next
                                    .parse::<u32>()
                                    .ok()
                                    .is_some_and(|n| (1..=i32::MAX as u32).contains(&n))
                                {
                                    return Err("Invalid registration counter value".into());
                                }
                                let root =
                                    self.data_root.as_deref().ok_or("Data directory missing")?;
                                let path = std::path::Path::new(root)
                                    .join("config.db4")
                                    .canonicalize()
                                    .map_err(|e| e.to_string())?;
                                let current = old.current_value(Some(root), true)?;
                                crate::db::writer::counters::save_values(
                                    &path,
                                    id,
                                    &[(old.oid, old.name.as_str(), current, next)],
                                )?;
                                return Ok(Value::Null);
                            }
                        }
                        Err(format!("RegistryRoot.SetValue({name}): value would change; persistent registry writes are not implemented").into())
                    }
                    "DeleteValue" | "DeleteKey" => Err(format!(
                        "RegistryRoot.{method}: persistent registry writes are not implemented"
                    )
                    .into()),
                    _ => Err(format!("RegistryRoot.{method} is not implemented").into()),
                }
            }
            Value::FileMethod(file, method) => {
                let path = &file.path;
                let meta = std::fs::metadata(path)
                    .map_err(|e| Fault::Script(Value::String(e.to_string())))?;
                match method.as_str() {
                    "copyTo" | "read" | "readByte" | "readLine" | "reset" | "skip" => {
                        if !meta.is_file() || meta.len() > crate::http_form::MAX_UPLOAD_BODY as u64
                        {
                            return Err("file stream size/type limit".into());
                        }
                        if file.stream.borrow().is_none() {
                            let Value::ReadStream(stream) = InputStream::value(
                                &std::fs::read(path)
                                    .map_err(|e| Fault::Script(Value::String(e.to_string())))?,
                            ) else {
                                unreachable!()
                            };
                            *file.stream.borrow_mut() = Some(stream);
                        }
                        let stream = file.stream.borrow().as_ref().unwrap().clone();
                        stream.borrow_mut().charset = file.charset.borrow().clone();
                        let input = Value::ReadStream(stream);
                        let f = self.member(input.clone(), method.clone())?;
                        self.call_host(f, values, Some(input), format!("File.{method}"))
                    }
                    "close" if values.is_empty() => {
                        if let Some(stream) = file.stream.borrow().as_ref() {
                            stream.borrow_mut().closed = true;
                        }
                        Ok(Value::Null)
                    }
                    "list" => {
                        if values.len() > 3 {
                            return Err("File.list expects [mask[, mode[, flag]]]".into());
                        }
                        let mask = match values.first() {
                            None | Some(Value::Null) => "*.*".into(),
                            Some(Value::String(s)) => s.clone(),
                            _ => return Err("File.list mask must be a string".into()),
                        };
                        let mode = values.get(1).map(Value::number).transpose()?.unwrap_or(3.0);
                        if !mode.is_finite() || mode < 0.0 || mode.fract() != 0.0 || mode > 3.0 {
                            return Err("File.list mode must be 0..3".into());
                        }
                        if values.get(2).is_some_and(|v| !matches!(v, Value::Bool(_))) {
                            return Err("File.list third argument must be Boolean".into());
                        }
                        if !meta.is_dir() {
                            return Ok(Value::Null);
                        }
                        let entries = super::file_list::list(&path, &mask, mode as u32).map_err(
                            |e| match e {
                                super::file_list::Error::Io(s) => Fault::Script(Value::String(s)),
                                super::file_list::Error::Limit(s) => Fault::Host(s),
                            },
                        )?;
                        let mut items = HashMap::new();
                        items.insert("length".into(), Value::Number(entries.len() as f64));
                        for (i, e) in entries.into_iter().enumerate() {
                            items.insert(i.to_string(), Value::DirStat(e));
                        }
                        Ok(Value::Array(Rc::new(RefCell::new(items))))
                    }
                    "isDirectory" => {
                        if !values.is_empty() {
                            return Err("isDirectory takes no arguments".into());
                        }
                        Ok(Value::Bool(meta.is_dir()))
                    }
                    "length" => {
                        if !values.is_empty() {
                            return Err("length takes no arguments".into());
                        }
                        Ok(Value::Number(meta.len() as f64))
                    }
                    _ => Err(
                        format!("File.{method} is not implemented for {}", path.display()).into(),
                    ),
                }
            }
            Value::PathMethod(path, method) => {
                let mut path = path.borrow_mut();
                match method.as_str() {
                    "GetTempDirectory" if values.is_empty() => {
                        *path = super::path::script_path(
                            &self.attachment_temp_base()?.to_string_lossy(),
                        );
                        if !path.ends_with('/') {
                            path.push('/');
                        }
                        Ok(Value::Null)
                    }
                    "MakeFullKodeksPath" => {
                        if values.len() > 1
                            || values.first().is_some_and(|v| !matches!(v, Value::Bool(_)))
                        {
                            return Err("MakeFullKodeksPath expects optional Boolean".into());
                        }
                        let empty = path.is_empty();
                        let p = std::path::Path::new(&*path);
                        if !p.is_absolute() {
                            let root = self
                                .data_root
                                .as_ref()
                                .ok_or("FilePath requires data root")?;
                            *path = super::path::script_path(
                                &std::path::Path::new(root).join(p).to_string_lossy(),
                            );
                        }
                        if empty && !path.ends_with('/') {
                            path.push('/');
                        }
                        Ok(Value::Null)
                    }
                    "IsWildcard" => {
                        if !values.is_empty() {
                            return Err("IsWildcard takes no arguments".into());
                        }
                        Ok(Value::Bool(path.contains('*') || path.contains('?')))
                    }
                    "GetPath" | "GetName" => {
                        if !values.is_empty() {
                            return Err("FilePath getter takes no arguments".into());
                        }
                        let split = path.rfind('/');
                        let value = if method == "GetPath" {
                            split.map(|n| path[..=n].to_string()).unwrap_or_default()
                        } else {
                            split
                                .map(|n| path[n + 1..].to_string())
                                .unwrap_or_else(|| path.clone())
                        };
                        Ok(Value::String(value))
                    }
                    "AddLastSlash" => {
                        if !values.is_empty() {
                            return Err("AddLastSlash takes no arguments".into());
                        }
                        if !path.ends_with('/') {
                            path.push('/');
                        }
                        Ok(Value::Null)
                    }
                    "ChangeName" => {
                        if values.len() != 1 {
                            return Err("ChangeName takes one name".into());
                        }
                        let name = values[0].text();
                        if name.contains('/') || name.contains('\\') || name.contains('\0') {
                            return Err("ChangeName requires a filename".into());
                        }
                        let prefix = path.rfind('/').map(|n| n + 1).unwrap_or(0);
                        path.truncate(prefix);
                        path.push_str(&name);
                        Ok(Value::Null)
                    }
                    _ => Err(format!("FilePath.{method} is not implemented").into()),
                }
            }
            Value::DatabaseMethod(db, method) => {
                let scope = Self::db_scope(&db)?;
                if method == "Remove" {
                    let [Value::Volume(owner, id)] = values.as_slice() else {
                        return Err(Fault::Script(Value::String(
                            "DataBase.Remove expects a Volume".into(),
                        )));
                    };
                    if *owner != scope {
                        return Err(Fault::Script(Value::String(
                            "Volume belongs to another DataBase".into(),
                        )));
                    }
                    self.dbhost
                        .as_ref()
                        .ok_or("Remove requires DataBase host")?
                        .borrow_mut()
                        .remove(scope, id.id)
                        .map_err(|s| Fault::Script(Value::String(s)))?;
                    return Ok(Value::Null);
                }
                if method != "Open" {
                    return Err(format!("DataBase.{method} is not implemented").into());
                }
                if values.is_empty() || values.len() > 5 {
                    return Err("Base.Open expects a path and up to four Boolean flags".into());
                }
                if !matches!(values[0], Value::String(_) | Value::FilePath(_)) {
                    return Err("Base.Open requires string or FilePath".into());
                }
                let path = values[0].text();
                let flags = values[1..]
                    .iter()
                    .map(|v| match v {
                        Value::Bool(b) => Ok(*b),
                        _ => Err("Base.Open flags must be Boolean"),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let map_err = |e| match e {
                    OpenError::Io(s) | OpenError::Schema(s) => Fault::Script(Value::String(s)),
                    OpenError::Unsupported(s) => Fault::Host(s),
                };
                if let Some(host) = &self.dbhost {
                    let v = host
                        .borrow_mut()
                        .open(scope, &path, &flags)
                        .map_err(map_err)?;
                    return Ok(Value::Volume(scope, v.clone()));
                }
                if scope != 0 {
                    return Err("independent DataBase host not configured".into());
                }
                let schema = self
                    .opener
                    .as_ref()
                    .ok_or("Base.Open requires volume context")?(
                    &path, &flags
                )
                .map_err(map_err)?;
                Ok(Self::database(&schema, scope))
            }
            Value::ArrayMethod(o, name) if name == "Remove" || name == "Delete" => {
                self.numeric_arrays.invalidate(&o);
                if values.len() != 1 {
                    return Err(format!("Array.{name} expects index").into());
                }
                let n = values[0].number()?;
                let mut a = o.borrow_mut();
                let len = a.get("length").ok_or("array length missing")?.number()? as usize;
                if !n.is_finite() || n < 0.0 || n.fract() != 0.0 || n as usize >= len {
                    return Err(format!("Array.{name} index outside array").into());
                }
                let removed = a.remove(&(n as usize).to_string()).unwrap_or(Value::Null);
                // A registry may use OIDs as sparse array indices. Shift only
                // present elements rather than iterating hundreds of millions
                // of holes, preserving non-index properties.
                let shifted = a
                    .iter()
                    .filter_map(|(key, value)| {
                        let i = array_index(key, len)?;
                        (i > n as usize).then(|| (key.clone(), i - 1, value.clone()))
                    })
                    .collect::<Vec<_>>();
                for (key, _, _) in &shifted {
                    a.remove(key);
                }
                for (_, i, value) in shifted {
                    a.insert(i.to_string(), value);
                }
                a.remove(&(len - 1).to_string());
                a.insert("length".into(), Value::Number((len - 1) as f64));
                // KS Delete is also used as pop by mp_CMtArray. Keep the
                // existing Remove result while returning Delete's removed item.
                Ok(if name == "Delete" {
                    removed
                } else {
                    Value::Null
                })
            }
            Value::ArrayMethod(o, name) if name == "search" => self.array_search(o, values),
            Value::ArrayMethod(o, name) if name == "concat" => {
                let mut items = Vec::new();
                for value in std::iter::once(Value::Array(o)).chain(values) {
                    if let Value::Array(array) = value {
                        let a = array.borrow();
                        let len = a["length"].number()? as usize;
                        if len > u32::MAX as usize - items.len() {
                            return Err("KS concat array limit".into());
                        }
                        for i in 0..len {
                            items.push(a.get(&i.to_string()).cloned().unwrap_or(Value::Null));
                        }
                    } else {
                        if items.len() >= u32::MAX as usize {
                            return Err("KS concat array limit".into());
                        }
                        items.push(value);
                    }
                }
                let len = items.len();
                let mut map: HashMap<_, _> = items
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| (i.to_string(), v))
                    .collect();
                map.insert("length".into(), Value::Number(len as f64));
                Ok(Value::Array(Rc::new(RefCell::new(map))))
            }
            Value::ArrayMethod(o, name) if name == "sort" => {
                if values.len() > 1 {
                    return Err("sort expects at most one comparator".into());
                }
                let comparator = match values.first() {
                    None | Some(Value::Null) => None,
                    Some(Value::Function(f)) => Some(f.clone()),
                    _ => return Err("sort comparator must be a function".into()),
                };
                let len = o
                    .borrow()
                    .get("length")
                    .ok_or("array length missing")?
                    .number()? as usize;
                let mut items = (0..len)
                    .map(|i| {
                        o.borrow()
                            .get(&i.to_string())
                            .cloned()
                            .unwrap_or(Value::Null)
                    })
                    .collect::<Vec<_>>();
                // Fallible stable merge sort: no RefCell borrow spans a KS callback.
                let mut width = 1;
                while width < len {
                    let old = items.clone();
                    for start in (0..len).step_by(width * 2) {
                        let mid = (start + width).min(len);
                        let end = (start + 2 * width).min(len);
                        let (mut a, mut b) = (start, mid);
                        for target in start..end {
                            self.tick()?;
                            let left = if b == end {
                                true
                            } else if a == mid {
                                false
                            } else if let Some(f) = &comparator {
                                let n = self
                                    .invoke(
                                        f.clone(),
                                        vec![old[a].clone(), old[b].clone()],
                                        None,
                                        "Array.sort comparator".into(),
                                    )?
                                    .number()?;
                                n.is_nan() || n <= 0.0
                            } else {
                                old[a].text() <= old[b].text()
                            };
                            if left {
                                items[target] = old[a].clone();
                                a += 1;
                            } else {
                                items[target] = old[b].clone();
                                b += 1;
                            }
                        }
                    }
                    width *= 2;
                }
                {
                    self.numeric_arrays.invalidate(&o);
                    let mut map = o.borrow_mut();
                    for (i, v) in items.into_iter().enumerate() {
                        map.insert(i.to_string(), v);
                    }
                }
                Ok(Value::Array(o))
            }
            Value::ArrayMethod(o, name) => {
                let mut map = o.borrow_mut();
                let len = map.get("length").unwrap().number()? as usize;
                match name.as_str() {
                    "push" => {
                        let next = len
                            .checked_add(values.len())
                            .filter(|n| *n <= u32::MAX as usize)
                            .ok_or("KS array index overflow")?;
                        map.try_reserve(values.len())
                            .map_err(|e| format!("Array.push allocation: {e}"))?;
                        self.numeric_arrays.append(&o, len, &values);
                        for (i, v) in values.into_iter().enumerate() {
                            map.insert((len + i).to_string(), v);
                        }
                        map.insert("length".into(), Value::Number(next as f64));
                        Ok(Value::Number(next as f64))
                    }
                    "pop" => {
                        self.numeric_arrays.invalidate(&o);
                        if !values.is_empty() {
                            return Err("pop takes no arguments".into());
                        }
                        if len == 0 {
                            return Ok(Value::Null);
                        }
                        let v = map.remove(&(len - 1).to_string()).unwrap_or(Value::Null);
                        map.insert("length".into(), Value::Number((len - 1) as f64));
                        Ok(v)
                    }
                    "join" => {
                        if values.len() > 1 {
                            return Err("join takes at most one separator".into());
                        }
                        let sep = values.first().map(Value::text).unwrap_or(",".into());
                        let mut out = String::new();
                        let mut present = map
                            .iter()
                            .filter_map(|(key, value)| array_index(key, len).map(|i| (i, value)))
                            .collect::<Vec<_>>();
                        present.sort_unstable_by_key(|(i, _)| *i);
                        let mut separators = 0;
                        for (i, value) in present {
                            self.tick()?;
                            let item = value.text();
                            let gap = i - separators;
                            let extra = gap
                                .checked_mul(sep.len())
                                .and_then(|n| n.checked_add(item.len()))
                                .ok_or("KS string length overflow")?;
                            if out
                                .len()
                                .checked_add(extra)
                                .filter(|n| *n <= 4 * 1024 * 1024)
                                .is_none()
                            {
                                return Err("KS string limit".into());
                            }
                            if !sep.is_empty() {
                                out.push_str(&sep.repeat(gap));
                            }
                            out.push_str(&item);
                            separators = i;
                        }
                        let tail = len.saturating_sub(1).saturating_sub(separators);
                        let extra = tail
                            .checked_mul(sep.len())
                            .ok_or("KS string length overflow")?;
                        if out
                            .len()
                            .checked_add(extra)
                            .filter(|n| *n <= 4 * 1024 * 1024)
                            .is_none()
                        {
                            return Err("KS string limit".into());
                        }
                        if !sep.is_empty() {
                            out.push_str(&sep.repeat(tail));
                        }
                        Ok(Value::String(out))
                    }
                    _ => unreachable!(),
                }
            }
            Value::StringMethod(s, name) => {
                let chars: Vec<_> = s.chars().collect();
                let num = |i: usize, default: usize| -> Result<usize, String> {
                    match values.get(i) {
                        None => Ok(default),
                        Some(v) => Ok(v.number()?.max(0.0) as usize),
                    }
                };
                match name.as_str() {
                    "toString" => {
                        if !values.is_empty() {
                            return Err("toString arguments not implemented".into());
                        }
                        Ok(Value::String(s))
                    }
                    "replace" => {
                        if !(1..=2).contains(&values.len()) {
                            return Err("String.replace expects pattern[, replacement]".into());
                        }
                        let Value::String(needle) = &values[0] else {
                            return Err("String.replace requires a literal pattern".into());
                        };
                        let replacement = values.get(1).map(Value::text).unwrap_or_default();
                        let (needle, replacement) = (needle.as_str(), replacement.as_str());
                        if needle.is_empty() {
                            return Err("String.replace empty pattern is not implemented".into());
                        }
                        let count = s.matches(needle).count();
                        let size = s
                            .len()
                            .checked_sub(count * needle.len())
                            .and_then(|n| {
                                replacement
                                    .len()
                                    .checked_mul(count)
                                    .and_then(|extra| n.checked_add(extra))
                            })
                            .filter(|n| *n <= 4 * 1024 * 1024)
                            .ok_or("KS string limit")?;
                        let mut out = String::with_capacity(size);
                        let mut end = 0;
                        for (i, _) in s.match_indices(needle) {
                            self.tick()?;
                            out.push_str(&s[end..i]);
                            out.push_str(replacement);
                            end = i + needle.len();
                        }
                        out.push_str(&s[end..]);
                        Ok(Value::String(out))
                    }
                    "match" => {
                        if values.len() != 1 {
                            return Err("String.match requires one pattern".into());
                        }
                        let regex = if let Value::Regex(re) = &values[0] {
                            re.clone()
                        } else {
                            let pattern = values[0].text();
                            if pattern.len() > 16384 {
                                return Err("regex pattern limit".into());
                            }
                            regex::RegexBuilder::new(&pattern)
                                .size_limit(1024 * 1024)
                                .dfa_size_limit(1024 * 1024)
                                .build()
                                .map_err(|e| format!("unsupported regex: {e}"))?
                        };
                        let Some(captures) = regex.captures(&s) else {
                            return Ok(Value::Null);
                        };
                        // KS indexes captured groups from zero; it does not prepend the full match.
                        let mut map = HashMap::new();
                        let count = captures.len().saturating_sub(1);
                        for i in 0..count {
                            map.insert(
                                i.to_string(),
                                captures
                                    .get(i + 1)
                                    .map(|m| Value::String(m.as_str().into()))
                                    // Native optional captures are empty strings.
                                    // mp_CFsPath splits an absent path component.
                                    .unwrap_or_else(|| Value::String(String::new())),
                            );
                        }
                        map.insert("length".into(), Value::Number(count as f64));
                        Ok(Value::Array(Rc::new(RefCell::new(map))))
                    }
                    "split" => {
                        // Native KS yields no items for an empty list. Calendar
                        // forms split an absent oidsofdays parameter before saving.
                        if s.is_empty() {
                            return Ok(Value::Array(Rc::new(RefCell::new(HashMap::from([(
                                "length".into(),
                                Value::Number(0.0),
                            )])))));
                        }
                        // The /99 SQL formatter supplies an empty string as
                        // its optional second argument and reads the second part.
                        if values.len() > 1
                            && !matches!(values.as_slice(), [_, Value::String(s)] if s.is_empty())
                        {
                            return Err("unsupported KS split arguments".into());
                        }
                        let mut map = HashMap::new();
                        let mut count = 0;
                        let mut add = |part: String| -> Result<(), Fault> {
                            if count >= u32::MAX as usize {
                                return Err("KS split array limit".into());
                            }
                            map.insert(count.to_string(), Value::String(part));
                            count += 1;
                            Ok(())
                        };
                        if let Some(Value::Regex(re)) = values.first() {
                            for part in re.split(&s) {
                                add(part.into())?;
                            }
                        } else if let Some(separator) = values.first() {
                            let sep = separator.text();
                            if let Some(pattern) = sep.strip_prefix('$').filter(|s| !s.is_empty()) {
                                if pattern.len() > 16384 {
                                    return Err("regex pattern limit".into());
                                }
                                let re = regex::RegexBuilder::new(pattern)
                                    .size_limit(1024 * 1024)
                                    .build()
                                    .map_err(|e| format!("unsupported split regex: {e}"))?;
                                for part in re.split(&s) {
                                    add(part.into())?;
                                }
                            } else if sep.is_empty() {
                                for ch in s.chars() {
                                    add(ch.to_string())?;
                                }
                            } else {
                                for part in s.split(&sep) {
                                    add(part.into())?;
                                }
                            }
                        } else {
                            add(s)?;
                        }
                        map.insert("length".into(), Value::Number(count as f64));
                        Ok(Value::Array(Rc::new(RefCell::new(map))))
                    }
                    "charAt" => Ok(Value::String(
                        chars
                            .get(num(0, 0)?)
                            .map(char::to_string)
                            .unwrap_or_default(),
                    )),
                    "substring" => {
                        let a = num(0, 0)?.min(chars.len());
                        let b = num(1, chars.len())?.min(chars.len());
                        Ok(Value::String(chars[a.min(b)..a.max(b)].iter().collect()))
                    }
                    "isDigit" => {
                        if !values.is_empty() {
                            return Err("String.isDigit expects no arguments".into());
                        }
                        Ok(Value::Bool(
                            !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()),
                        ))
                    }
                    "trim" => Ok(Value::String(s.trim().into())),
                    "toLowerCase" => Ok(Value::String(s.to_lowercase())),
                    "toUpperCase" => Ok(Value::String(s.to_uppercase())),
                    "localeCompare" => {
                        if values.len() != 1 {
                            return Err("localeCompare requires one string".into());
                        }
                        let a = s.to_lowercase();
                        let b = values[0].text().to_lowercase();
                        let a = encoding_rs::WINDOWS_1251.encode(&a).0.into_owned();
                        let b = encoding_rs::WINDOWS_1251.encode(&b).0.into_owned();
                        Ok(Value::Number(match a.cmp(&b) {
                            std::cmp::Ordering::Less => -1.0,
                            std::cmp::Ordering::Equal => 0.0,
                            std::cmp::Ordering::Greater => 1.0,
                        }))
                    }
                    "indexOf" | "lastIndexOf" => {
                        if values.len() != 1 {
                            return Err("string search requires one argument".into());
                        }
                        let needle = values[0].text();
                        let at = if name == "indexOf" {
                            s.find(&needle)
                        } else {
                            s.rfind(&needle)
                        };
                        Ok(Value::Number(
                            at.map(|i| s[..i].chars().count() as f64).unwrap_or(-1.0),
                        ))
                    }
                    _ => Err(format!("unsupported string method {name}").into()),
                }
            }
            Value::DecimalMethod(d, method) => {
                let text = if method == "toString" && values.is_empty() {
                    d.text()
                } else if method == "toFormattedString" && values.len() <= 4 {
                    // Native 006d3b10 -> 006d52c0: precision, explicit plus,
                    // buffer size, and decimal point even for precision zero.
                    let places = values
                        .first()
                        .map(Value::number)
                        .transpose()?
                        .unwrap_or(2.0);
                    if !(0.0..=28.0).contains(&places) || places.fract() != 0.0 {
                        return Err("invalid LongDouble decimal places".into());
                    }
                    let mut text = d.formatted(places as u32)?;
                    if values.get(1).is_some_and(Value::truth) && !text.starts_with('-') {
                        text.insert(0, '+');
                    }
                    if places == 0.0 && values.get(3).map_or(true, Value::truth) {
                        text.push('.');
                    }
                    let size = values
                        .get(2)
                        .filter(|v| !matches!(v, Value::Null))
                        .map(Value::number)
                        .transpose()?
                        .unwrap_or(62.0);
                    if !size.is_finite()
                        || size < 0.0
                        || size.fract() != 0.0
                        || text.len() > size.min(256.0) as usize
                    {
                        return Err("LongDouble output buffer too small".into());
                    }
                    text
                } else {
                    return Err(format!(
                        "unsupported LongDouble.{method}({})",
                        values
                            .iter()
                            .map(Value::text)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                    .into());
                };
                Ok(Value::String(text))
            }
            Value::ReadStreamMethod(stream, method) => {
                if matches!(method.as_str(), "Print" | "PrintS" | "write") {
                    let mut s = stream.borrow_mut();
                    if s.closed {
                        return Err("stream closed".into());
                    }
                    for value in values {
                        let text = value.text();
                        let (bytes, _, bad) = encoding_rs::WINDOWS_1251.encode(&text);
                        if bad {
                            return Err("stream encoding".into());
                        }
                        s.insert_text(&bytes)?;
                    }
                    return Ok(Value::Null);
                }
                if method == "copyTo" {
                    if let [Value::LogFile(file)] = values.as_slice() {
                        use std::io::Write;
                        let mut source = stream.borrow_mut();
                        if source.closed {
                            return Err("stream closed".into());
                        }
                        let mut file = file.file.borrow_mut();
                        file.write_all(&source.data[source.position..])
                            .and_then(|_| file.sync_all())
                            .map_err(|e| e.to_string())?;
                        source.position = source.data.len();
                        return Ok(Value::Null);
                    }
                    if let [Value::ReadStream(target)] = values.as_slice() {
                        if Rc::ptr_eq(&stream, target) {
                            return Err("cannot copy a stream into itself".into());
                        }
                        let mut source = stream.borrow_mut();
                        let mut target = target.borrow_mut();
                        if source.closed || target.closed {
                            return Err("stream closed".into());
                        }
                        if target
                            .data
                            .len()
                            .saturating_add(source.data.len() - source.position)
                            > 16 * 1024 * 1024
                        {
                            return Err("stream size limit".into());
                        }
                        target
                            .data
                            .extend_from_slice(&source.data[source.position..]);
                        source.position = source.data.len();
                        return Ok(Value::Null);
                    }
                    if !matches!(values.as_slice(), [Value::HttpOutput]) {
                        return Err("copyTo expects a stream or Output".into());
                    }
                    let mut s = stream.borrow_mut();
                    if s.closed {
                        return Err("stream closed".into());
                    }
                    let (prefix, _, bad) = encoding_rs::WINDOWS_1251.encode(&self.output);
                    if bad {
                        return Err("output cannot be encoded as Windows-1251".into());
                    }
                    let size = self.binary_output.as_ref().map_or(0, Vec::len)
                        + prefix.len()
                        + s.data.len()
                        - s.position;
                    if size > crate::http_form::MAX_UPLOAD_BODY {
                        return Err("binary output size limit".into());
                    }
                    let bytes = self.binary_output.get_or_insert_with(Vec::new);
                    bytes.extend_from_slice(&prefix);
                    bytes.extend_from_slice(&s.data[s.position..]);
                    self.output.clear();
                    s.position = s.data.len();
                    return Ok(Value::Null);
                }
                if method == "readByte" {
                    if !values.is_empty() {
                        return Err("readByte expects no arguments".into());
                    }
                    let mut s = stream.borrow_mut();
                    if s.closed {
                        return Err("stream closed".into());
                    }
                    let b = s.data.get(s.position).copied();
                    if b.is_some() {
                        s.position += 1;
                    }
                    return Ok(Value::Number(b.map(|b| b as f64).unwrap_or(-1.0)));
                }
                let mut s = stream.borrow_mut();
                if s.closed {
                    return Err("stream closed".into());
                }
                match method.as_str() {
                    "skip" => {
                        let [count] = values.as_slice() else {
                            return Err("skip expects a byte count".into());
                        };
                        let n = count.number()?;
                        if !n.is_finite()
                            || n < 0.0
                            || n.fract() != 0.0
                            || n > 16.0 * 1024.0 * 1024.0
                        {
                            return Err("stream skip limit".into());
                        }
                        // Stored text length includes encoding markers. Seeking
                        // that length advances to the end of the decoded stream.
                        s.position = s.position.saturating_add(n as usize).min(s.data.len());
                        Ok(Value::Null)
                    }
                    "readLine" => {
                        if !values.is_empty() {
                            return Err("readLine expects no arguments".into());
                        }
                        if s.position >= s.data.len() {
                            return Ok(Value::Null);
                        }
                        let end = s.data[s.position..]
                            .iter()
                            .position(|b| *b == 10)
                            .map(|n| s.position + n)
                            .unwrap_or(s.data.len());
                        let mut line_end = end;
                        if line_end > s.position && s.data[line_end - 1] == 13 {
                            line_end -= 1;
                        }
                        let bytes = &s.data[s.position..line_end];
                        let text = if s.charset == "utf-8" {
                            std::str::from_utf8(bytes)
                                .map_err(|_| "invalid UTF-8 stream")?
                                .to_owned()
                        } else {
                            crate::db::decode(bytes)
                        };
                        s.position = (end + 1).min(s.data.len());
                        Ok(Value::String(text))
                    }
                    "read" => {
                        let [count] = values.as_slice() else {
                            return Err("read requires a byte count".into());
                        };
                        let n = count.number()?;
                        if !n.is_finite()
                            || n < 0.0
                            || n.fract() != 0.0
                            || n > 16.0 * 1024.0 * 1024.0
                        {
                            return Err("invalid stream read size".into());
                        }
                        let end = (s.position + n as usize).min(s.data.len());
                        let bytes = &s.data[s.position..end];
                        let text = if s.charset == "utf-8" {
                            std::str::from_utf8(bytes)
                                .map_err(|_| "invalid UTF-8 stream")?
                                .to_string()
                        } else {
                            crate::db::decode(bytes)
                        };
                        s.position = end;
                        Ok(Value::String(text))
                    }
                    "reset" if values.is_empty() => {
                        s.position = 0;
                        Ok(Value::Null)
                    }
                    "close" if values.is_empty() => {
                        s.closed = true;
                        s.data.clear();
                        s.encoded_extra = 0;
                        Ok(Value::Null)
                    }
                    _ => Err("unsupported input stream method/arguments".into()),
                }
            }
            Value::StreamMethod(t, method) => {
                let mut t = t.borrow_mut();
                if method.starts_with("rich.") {
                    let mut text = values.iter().map(Value::text).collect::<String>();
                    if method == "rich.Print" && !text.ends_with('\n') {
                        text.push('\n');
                    }
                    if t.len() + text.len() > 2 * 1024 * 1024 {
                        return Err("rich text size limit".into());
                    }
                    t.insert_str(0, &text);
                    return Ok(Value::Null);
                }
                for v in values {
                    let s = v.text();
                    if t.len() + s.len() + 1 > 2 * 1024 * 1024 {
                        return Err("temporary script size limit".into());
                    }
                    t.push_str(&s);
                }
                if method == "Print" {
                    t.push('\n');
                }
                Ok(Value::Null)
            }
            Value::FieldMethod(t, _) => {
                if !values.is_empty() {
                    return Err("InsertText requires no arguments".into());
                }
                Ok(Value::Object(Rc::new(RefCell::new(HashMap::from([(
                    "Text".into(),
                    Value::TextStream(t),
                )])))))
            }
            Value::TempMethod(d, method) => {
                if d.borrow().properties.borrow().contains_key("\0report") {
                    return self.report_document_call(d, &method, &values);
                }
                if values.is_empty() || values.len() > if method == "Execute" { 2 } else { 1 } {
                    return Err("temporary document method arguments".into());
                }
                let key = values[0].text();
                if method == "NewField" {
                    let props = d.borrow().properties.clone();
                    let (base, area) = {
                        let p = props.borrow();
                        (
                            p.get("Base").cloned().unwrap_or(Value::Null),
                            p.get("Area").cloned().unwrap_or(Value::Null),
                        )
                    };
                    let structure = self.member(base, "Struct".into())?;
                    let area = self.member(structure, area.text())?;
                    let field = self.member(area, key.clone())?;
                    if !matches!(self.member(field,"Type".into())?,Value::Number(n) if n==4.0) {
                        return Err("temporary document requires a declared text field".into());
                    }
                    let mut doc = d.borrow_mut();
                    if doc.fields.len() >= 1000 {
                        return Err("temporary document field limit".into());
                    }
                    let text = Rc::new(RefCell::new(String::new()));
                    doc.fields.insert(key, text.clone());
                    return Ok(Value::TextField(text));
                }
                let text = d
                    .borrow()
                    .fields
                    .get(&key)
                    .cloned()
                    .ok_or("temporary field missing")?;
                let source = text.borrow().clone();
                if method == "Compile" {
                    parser::parse(&source).map_err(|e| format!("temporary KS syntax: {e}"))?;
                    Ok(Value::Null)
                } else {
                    self.execute_code(&source, values.get(1).cloned())
                }
            }
            Value::DateMethod(d, method) => {
                if method == "parse" {
                    let value = values.first().ok_or("Date.parse requires a date string")?;
                    let date = super::date::Date::from_text(&value.text())
                        .map_err(|e| Fault::Script(Value::String(e)))?;
                    return Ok(Value::Number(date.millis));
                }
                if method == "toString" && values.is_empty() {
                    let date = d.borrow();
                    date.get("getFullYear")?;
                    let [year, month, day, ..] = date.parts();
                    Ok(Value::String(format!(
                        "{day:02}.{:02}.{year:04}",
                        month + 1
                    )))
                } else if method == "toGMTString" || method == "toUTCString" {
                    if !values.is_empty() {
                        return Err("Date formatter expects no arguments".into());
                    }
                    Ok(Value::String(d.borrow().gmt_string()?))
                } else if method.starts_with("set") {
                    let component = method.strip_prefix("setUTC")
                        .or_else(|| method.strip_prefix("set"));
                    if !matches!(component, Some("Time" | "FullYear" | "Month" | "Date"
                        | "Hours" | "Minutes" | "Seconds" | "Milliseconds")) {
                        return Err(format!("Date.{method} not implemented").into());
                    }
                    // Stored KS catches invalid optional Word dates in DocAttr.
                    // Validation errors belong to that script's try/catch;
                    // missing host bindings must still fail explicitly.
                    let nums = values
                        .iter()
                        .map(Value::number)
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|e| Fault::Script(Value::String(e)))?;
                    d.borrow_mut().set(&method, &nums)
                        .map_err(|e| Fault::Script(Value::String(e)))?;
                    Ok(Value::Date(d))
                } else {
                    if !values.is_empty() {
                        return Err("Date getter expects no arguments".into());
                    }
                    let n = d.borrow().get(&method)?;
                    Ok(Value::Number(n))
                }
            }
            Value::DocumentMethod(oid, scope, method) => {
                if method == "SaveTo" {
                    let [Value::LogFile(file)] = values.as_slice() else {
                        return Err("Document.SaveTo expects an archive File".into());
                    };
                    use std::io::Write;
                    let record = self.read_document(oid, scope)?;
                    let mut bytes = oid.to_le_bytes().to_vec();
                    bytes.extend(crate::db::writer::encode_record(&record)?);
                    let mut file = file.file.borrow_mut();
                    file.write_all(&bytes)
                        .and_then(|_| file.sync_all())
                        .map_err(|e| e.to_string())?;
                    return Ok(Value::Null);
                }
                if method == "Delete" {
                    if self.kod_snapshot_loaded {
                        return Err("KOD replacement requires the atomic import adapter; this stored form revision is unsupported".into());
                    }
                    if !self.document_write_allowed {
                        return Err(
                            "Document deletion requires an authenticated editing request".into(),
                        );
                    }
                    if values.len() > 2 {
                        return Err(
                            "Document.Delete expects optional marker flag and volume array".into(),
                        );
                    }
                    if self.document_edits.borrow().contains_key(&(scope, oid)) {
                        return Err("Cannot delete a locked document".into());
                    }
                    if let Some(volumes) = values.get(1) {
                        let Value::Array(volumes) = volumes else {
                            return Err("Document.Delete requires a volume array".into());
                        };
                        let volumes = volumes.borrow();
                        let count = volumes
                            .get("length")
                            .ok_or("Volume array length missing")?
                            .number()? as usize;
                        if count == 0 {
                            return Err("Document.Delete volume array is empty".into());
                        }
                        let ids = (0..count)
                            .map(|i| match volumes.get(&i.to_string()) {
                                Some(Value::Volume(owner, v)) if *owner == scope => Ok(v.id),
                                _ => Err("Volume belongs to another DataBase or is invalid"),
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        for id in ids {
                            use super::database::DeleteError;
                            self.dbhost
                                .as_ref()
                                .ok_or("Document writer missing")?
                                .borrow_mut()
                                .delete_document_from_volume(scope, oid, values[0].truth(), id)
                                .map_err(|e| match e {
                                    DeleteError::Failed(message) => Fault::Host(message),
                                    e => Fault::Script(Value::Object(Rc::new(RefCell::new(
                                        HashMap::from([
                                            (
                                                "Code".into(),
                                                Value::Number(
                                                    if matches!(e, DeleteError::Missing) {
                                                        695.0
                                                    } else {
                                                        785.0
                                                    },
                                                ),
                                            ),
                                            (
                                                "errText".into(),
                                                Value::String(
                                                    if matches!(e, DeleteError::Missing) {
                                                        "Object absent from selected volume"
                                                    } else {
                                                        "Selected volume is read-only"
                                                    }
                                                    .into(),
                                                ),
                                            ),
                                        ]),
                                    )))),
                                })?;
                        }
                        return Ok(Value::Bool(true));
                    }
                    let record = self.read_document(oid, scope)?;
                    self.dbhost
                        .as_ref()
                        .ok_or("Document writer missing")?
                        .borrow_mut()
                        .delete_document(
                            scope,
                            oid,
                            values.first().is_some_and(Value::truth),
                            &record,
                        )?;
                    return Ok(Value::Bool(true));
                }
                if matches!(
                    method.as_str(),
                    "Lock" | "Unlock" | "Write" | "NewField" | "DeleteField"
                ) {
                    return self.edit_existing(oid, scope, &method, &values);
                }
                if method == "\0abstract" {
                    if !values.is_empty() {
                        return Err("FormDoc.abstract expects no arguments".into());
                    }
                    let html = self.abstract_html(oid, scope)?;
                    return Ok(Value::RichField(Rc::new(RefCell::new(
                        form_doc::RichText {
                            report: false,
                            parts: vec![Rc::new(RefCell::new(HashMap::from([(
                                "\0html".into(),
                                Value::String(html),
                            )])))],
                        },
                    ))));
                }
                if method == "TextLength" {
                    let record = self.read_document(oid, scope)?;
                    return Ok(Value::Number(
                        record
                            .fields
                            .iter()
                            .filter(|f| f.wire_type == 4)
                            .map(|f| crate::document::bytes(f).map(|b| b.len()).unwrap_or(0))
                            .sum::<usize>() as f64,
                    ));
                }
                if method == "Attr" {
                    return self.document_attr(oid, scope, &values);
                }
                if method == "Next" {
                    return self.document_next(oid, scope, &values);
                }
                if method != "Execute" || !(1..=2).contains(&values.len()) {
                    return Err("expected Document.Execute(attribute[, context])".into());
                }
                let attr = match &values[0] {
                    Value::String(name) => {
                        if let Some(host) = &self.dbhost {
                            host.borrow().attribute(scope, oid, name)? as f64
                        } else {
                            self.attribute
                                .as_ref()
                                .ok_or("named Execute requires schema resolver")?(
                                oid, name
                            )? as f64
                        }
                    }
                    v => v.number()?,
                };
                if !attr.is_finite() || attr < 0.0 || attr > 65535.0 || attr.fract() != 0.0 {
                    return Err("invalid Execute attribute".into());
                }
                self.execute_document(oid, scope, attr as u16, values.get(1).cloned())
            }
            Value::HostMethod(host, method) => {
                let args = values
                    .iter()
                    .map(|v| match v {
                        Value::Null => Ok(super::host::Arg::Null),
                        Value::Database(o) => {
                            let scope = Self::db_scope(o)?;
                            if scope != 0 && self.dbhost.is_none() {
                                return Err("independent DataBase host not configured".into());
                            }
                            Ok(super::host::Arg::Database(scope))
                        }
                        Value::String(s) => Ok(super::host::Arg::String(s.clone())),
                        Value::Number(n) => Ok(super::host::Arg::Number(*n)),
                        _ => Err(Fault::Host("unsupported SQL argument type".into())),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let query = |scope, sql: &str| {
                    if let Some(host) = &self.dbhost {
                        host.borrow().select(scope, sql)
                    } else {
                        if scope != 0 {
                            return Err("independent DataBase host not configured".into());
                        }
                        if super::sql::parse(sql)?.projection.has_fields() {
                            return Err("SQL field projection requires database context".into());
                        }
                        Ok(super::host::QueryData {
                            area: None,
                            oids: self.query.as_ref().ok_or("SQL requires database context")?(sql)?,
                            cells: vec![],
                            fulltext: Default::default(),
                        })
                    }
                };
                let result = host.call(&method, &args, &query)?;
                use super::host::ResultValue as H;
                Ok(match result {
                    H::Null => Value::Null,
                    H::Bool(v) => Value::Bool(v),
                    H::Number(v) => Value::Number(v),
                    H::Date(millis) => {
                        Value::Date(Rc::new(RefCell::new(super::date::Date { millis })))
                    }
                    H::DateArray(items) => {
                        let mut map = HashMap::new();
                        map.insert("length".into(), Value::Number(items.len() as f64));
                        for (i, millis) in items.into_iter().enumerate() {
                            map.insert(
                                i.to_string(),
                                Value::Date(Rc::new(RefCell::new(super::date::Date { millis }))),
                            );
                        }
                        Value::Array(Rc::new(RefCell::new(map)))
                    }
                    H::Decimal(bytes) => Value::Decimal(decimal::Decimal::from_native(&bytes)?),
                    H::DecimalArray(items) => {
                        let mut map = HashMap::new();
                        map.insert("length".into(), Value::Number(items.len() as f64));
                        for (i, b) in items.iter().enumerate() {
                            map.insert(
                                i.to_string(),
                                Value::Decimal(decimal::Decimal::from_native(b)?),
                            );
                        }
                        Value::Array(Rc::new(RefCell::new(map)))
                    }
                    H::String(v) => Value::String(v),
                    H::Object(oid, scope) => Value::Document(oid, scope),
                    H::FieldStream(oid, scope, field, kind) => {
                        let value = self.document_attr(
                            oid,
                            scope,
                            &[
                                Value::Number(field as f64),
                                Value::Bool(false),
                                Value::Number(kind as f64),
                            ],
                        )?;
                        if matches!(value, Value::Null) {
                            InputStream::value(&[])
                        } else if kind == 4 {
                            // JDBC getAsciiStream exposes the text characters,
                            // not the stored KText formatting delimiters.
                            let field = match value {
                                Value::StoredField(field) => field,
                                Value::WritableField(field) => field.snapshot()?,
                                _ => return Err("SQL text stream requires a text field".into()),
                            };
                            let plain: Vec<u8> = crate::document::bytes(field.data())?
                                .iter().copied().filter(|b| !matches!(b, 0 | 1)).collect();
                            InputStream::value(&plain)
                        } else {
                            self.member(value, "Stream".into())?
                        }
                    }
                    H::Annotation(oid, scope) => {
                        let html = self.sql_annotation_html(oid, scope)?;
                        // fragment emits <BR> followed by a source newline.
                        // That newline is HTML whitespace, not a second line
                        // in the JDBC annotation stream.
                        static BR: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
                        static TAGS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
                        let lines = BR
                            .get_or_init(|| regex::Regex::new("(?i)<br\\s*/?>\\r?\\n?").unwrap())
                            .replace_all(&html, "\n");
                        let text = TAGS
                            .get_or_init(|| regex::Regex::new("(?s)<[^>]*>").unwrap())
                            .replace_all(&lines, "")
                            .replace("&nbsp;", " ")
                            .replace("&quot;", "\"")
                            .replace("&#39;", "'")
                            .replace("&lt;", "<")
                            .replace("&gt;", ">")
                            .replace("&amp;", "&");
                        let (bytes, _, bad) = encoding_rs::WINDOWS_1251.encode(&text);
                        if bad {
                            return Err("annotation encoding".into());
                        }
                        InputStream::value(&bytes)
                    }
                    H::Handle(h) => Value::Host(h),
                    H::StringArray(items) => {
                        let len = items.len();
                        let mut map = items
                            .into_iter()
                            .enumerate()
                            .map(|(i, s)| (i.to_string(), Value::String(s)))
                            .collect::<HashMap<_, _>>();
                        map.insert("length".into(), Value::Number(len as f64));
                        Value::Array(Rc::new(RefCell::new(map)))
                    }
                    H::IntArray(items) => {
                        let len = items.len();
                        let mut map = items
                            .into_iter()
                            .enumerate()
                            .map(|(i, n)| (i.to_string(), Value::Number(n as f64)))
                            .collect::<HashMap<_, _>>();
                        map.insert("length".into(), Value::Number(len as f64));
                        Value::Array(Rc::new(RefCell::new(map)))
                    }
                })
            }
            Value::FormTabMethod(tab, _) => {
                let field = self.tab_field(&tab)?;
                let html = self.rich_html(&field)?;
                Ok(Value::Bool(form_doc::is_empty_html(&html)))
            }
            Value::RichMethod(field, method) => {
                if matches!(method.as_str(),"InsertText"|"InsertTable"|"Insert"|"TextLength") {
                    return self.report_collection_call(&method,Value::RichField(field),&values);
                }
                if method == "MarkContext" {
                    if values.is_empty() || values.len() > 3 {
                        return Err("MarkContext expects text[, positions, advanced]".into());
                    }
                    let key = Rc::as_ptr(&field) as usize;
                    self.rich_context_marks.remove(&key);
                    let Some(pattern) =
                        context_mark::pattern_at(&values[0].text(), self.data_root.as_deref())?
                    else {
                        return Ok(Value::Bool(false));
                    };
                    if values
                        .get(1)
                        .is_some_and(|v| !matches!(v, Value::Null) && !v.text().trim().is_empty())
                    {
                        return Err("context position highlighting is not implemented".into());
                    }
                    let html = self.rich_html(&Value::RichField(field.clone()))?;
                    let (_, count) = context_mark::highlight(&html, &pattern)?;
                    if count > 0 {
                        self.rich_context_marks.insert(key, (field, pattern));
                    }
                    return Ok(Value::Bool(count > 0));
                }
                if !values.is_empty() {
                    return Err("rich field insertion expects no arguments".into());
                }
                let mut m = HashMap::new();
                if method == "InsertText" {
                    m.insert(
                        "SetFontAttributes".into(),
                        Value::Native("report.text.set_fonts".into()),
                    );
                    m.insert(
                        "Text".into(),
                        Value::RichStream(Rc::new(RefCell::new(String::new()))),
                    );
                }
                let object = Rc::new(RefCell::new(m));
                field.borrow_mut().parts.push(object.clone());
                Ok(Value::Object(object))
            }
            Value::ListMethod(list, method) => {
                if method == "IsInArray" {
                    let [value] = values.as_slice() else {
                        return Err("List.IsInArray expects a document id".into());
                    };
                    let id = value.number()?;
                    if !(0.0..=u32::MAX as f64).contains(&id) || id.fract() != 0.0 {
                        return Err("List.IsInArray invalid OID".into());
                    }
                    let list = list.borrow();
                    let id = id as u32;
                    if list.ids.borrow().is_some() {
                        return Ok(Value::Bool(list.ids.contains(id)));
                    }
                    return Ok(Value::Bool(self.list_ids(&list)?.contains(&id)));
                }
                if method == "Delete" {
                    let [Value::List(other)] = values.as_slice() else {
                        return Err(
                            "List.Delete expects a List; persistent deletion is unsupported".into(),
                        );
                    };
                    let removed = self
                        .list_ids(&other.borrow())?
                        .into_iter()
                        .collect::<std::collections::HashSet<_>>();
                    let mut ids = self.list_ids(&list.borrow())?;
                    ids.retain(|id| !removed.contains(id));
                    *list.borrow().ids.borrow_mut() = Some(ids);
                    return Ok(Value::Null);
                }
                if method == "Add" {
                    if values.len() != 1 {
                        return Err("List.Add expects a document id".into());
                    }
                    let incoming = if let Value::List(other) = &values[0] {
                        self.list_ids(&other.borrow())?
                    } else {
                        let n = values[0].number()?;
                        if !n.is_finite() || n < 0.0 || n > u32::MAX as f64 || n.fract() != 0.0 {
                            return Err("List.Add invalid OID".into());
                        }
                        vec![n as u32]
                    };
                    let list = list.borrow();
                    if !list.conditions.is_empty() && list.ids.borrow().is_none() {
                        self.list_ids(&list)?;
                    }
                    list.ids.append(&incoming)?;
                    return Ok(Value::Null);
                }
                if method == "Find" {
                    if let [Value::List(other)] = values.as_slice() {
                        let incoming = self.list_ids(&other.borrow())?;
                        let target = list.borrow();
                        let mut ids =
                            if target.ids.borrow().is_none() && target.conditions.is_empty() {
                                incoming.clone()
                            } else {
                                self.list_ids(&target)?
                            };
                        let allowed = incoming
                            .into_iter()
                            .collect::<std::collections::HashSet<_>>();
                        ids.retain(|id| allowed.contains(id));
                        if target.area != 0 {
                            let mut valid = Vec::with_capacity(ids.len());
                            for id in ids {
                                if self.document_metadata(id, target.scope)?.area == target.area {
                                    valid.push(id);
                                }
                            }
                            ids = valid;
                        }
                        *target.ids.borrow_mut() = Some(ids);
                    } else if values.is_empty() {
                        list.borrow_mut().query_all = true;
                        self.list_ids(&list.borrow())?;
                    } else {
                        return Err("List.Find expects an optional List".into());
                    }
                    return Ok(Value::Null);
                }
                if method == "AddEmptyCondition" {
                    return self.list_empty_condition(&mut list.borrow_mut(), &values);
                }
                self.list_condition(&mut list.borrow_mut(), &values)
            }
            Value::CssMethod(css, method) => {
                if method == "insertRule" {
                    if values.is_empty() || values.len() > 2 {
                        return Err("insertRule expects rule[, index]".into());
                    }
                    let rule = values[0].text();
                    if rule.len() > 16384 || !rule.contains('{') || !rule.trim_end().ends_with('}')
                    {
                        return Err(Fault::Script(Value::String("invalid CSS rule".into())));
                    }
                    let mut rules = css.borrow_mut();
                    if rules.len() >= 1024 {
                        return Err("CSS rule limit".into());
                    }
                    let index = values
                        .get(1)
                        .map(Value::number)
                        .transpose()?
                        .unwrap_or(rules.len() as f64);
                    if index < 0.0 || index.fract() != 0.0 || index > rules.len() as f64 {
                        return Err("CSS rule index".into());
                    }
                    rules.insert(index as usize, rule);
                    Ok(Value::Number(index))
                } else {
                    if values.len() != 1 {
                        return Err("CSS rule method expects index".into());
                    }
                    let index = values[0].number()?;
                    if index < 0.0 || index.fract() != 0.0 || index >= css.borrow().len() as f64 {
                        return Ok(Value::Null);
                    }
                    if method == "deleteRule" {
                        css.borrow_mut().remove(index as usize);
                        Ok(Value::Null)
                    } else {
                        Ok(Value::ReadOnlyObject(Rc::new(RefCell::new(HashMap::from(
                            [(
                                "cssText".into(),
                                Value::String(css.borrow()[index as usize].clone()),
                            )],
                        )))))
                    }
                }
            }
            Value::Native(n) => match n.as_str() {
                "Document.ReadFromStream" => self.read_kod_document(&values),
                "__codex_TextFieldSize" => {
                    let [Value::StoredField(field)] = values.as_slice() else {
                        return Err("Text field size expects one stored text attribute".into());
                    };
                    if field.data().wire_type != 4 {
                        return Err("Text field size requires a text attribute".into());
                    }
                    Ok(Value::Number(crate::rich_text::attribute_text_size(field.data())? as f64))
                }
                "__codex_PEditVersion" => self.p_edit_version(&values),
                "__codex_PEditCheck" => self.p_edit_check(&values),
                "__codex_PEditPrepare" => self.p_edit_prepare(&values),
                "__codex_PEditConflict" => self.p_edit_conflict(&values),
                "__codex_ImportKod" => self.import_kod_document(&values),
                "attachment.shell" => self.attachment_shell(&values),
                n if n.starts_with("report.collection.") => self.report_collection_call(n.trim_start_matches("report.collection."),receiver.ok_or("Report receiver missing")?,&values),
                "report.text.set_fonts" => self.report_set_font_attributes(receiver, &values),
                "MkDir" | "Unlink" | "RmDir" => self.log_path_operation(&n, &values),
                "SetOutput" => {
                    match values.as_slice() {
                        [Value::HttpOutput] => self.output_stream = None,
                        [Value::ReadStream(s)] => self.output_stream = Some(s.clone()),
                        _ => return Err("SetOutput requires a stream".into()),
                    };
                    Ok(Value::Null)
                }
                "report.text.fonts" => {
                    let Some(Value::ReadOnlyObject(o)) = receiver else {
                        return Err("text font receiver missing".into());
                    };
                    if values.len() != 2 || values[0].number()? != 0.0 {
                        return Err("font runs currently require the full text range".into());
                    }
                    let fonts = o.borrow().get("\0fonts").cloned().unwrap_or(Value::Null);
                    Ok(fonts)
                }
                "path.SaveState" => self.save_path_settings(receiver, &values, false),
                "path.SaveCalendarAccess" => self.save_path_settings(receiver, &values, true),
                "field.InsertText" => self.writable_insert_text(receiver, &values),
                "rtf.mapping" => {
                    let Some(Value::Object(map)) = receiver else { return Err("RTF mapping receiver missing".into()); };
                    let key=if values.len()==2 {values[0].text()}else{"converter".into()};
                    let value=values.last().cloned().ok_or("RTF mapping value missing")?;
                    map.borrow_mut().insert(key,value);
                    Ok(Value::Null)
                }
                "field.FromStream" => self.writable_from_stream(receiver, &values),
                "xml.Parse" => self.xml_parse(receiver, &values),
                "xml.SetEncoding" => {
                    let [value] = values.as_slice() else {
                        return Err("XML.SetEncoding expects charset".into());
                    };
                    let charset = value.text().to_ascii_lowercase();
                    if !matches!(charset.as_str(), "windows-1251" | "utf-8") {
                        return Err("unsupported XML charset".into());
                    }
                    let Some(Value::Object(parser)) = receiver else {
                        return Err("XML parser receiver missing".into());
                    };
                    let stream = parser.borrow().get("\0stream").cloned();
                    let Some(Value::ReadStream(stream)) = stream else {
                        return Err("XML stream missing".into());
                    };
                    stream.borrow_mut().charset = charset;
                    Ok(Value::Null)
                }
                name if name.starts_with("xml.report.") => {
                    self.report_xml_call(&name[11..], receiver, values)
                }
                "Math.random" => {
                    if !values.is_empty() {
                        return Err("Math.random expects no arguments".into());
                    }
                    let bits = std::hash::BuildHasher::hash_one(
                        &std::collections::hash_map::RandomState::new(),
                        std::time::SystemTime::now(),
                    );
                    Ok(Value::Number((bits >> 11) as f64 / 9007199254740992.0))
                }
                name if name.starts_with("Math.") => {
                    let v = values
                        .iter()
                        .map(Value::number)
                        .collect::<Result<Vec<_>, _>>()?;
                    let result = match (name, v.as_slice()) {
                        ("Math.floor", [x]) => x.floor(),
                        ("Math.ceil", [x]) => x.ceil(),
                        ("Math.round", [x]) => (x + 0.5).floor(),
                        ("Math.abs", [x]) => x.abs(),
                        ("Math.sqrt", [x]) => x.sqrt(),
                        ("Math.pow", [x, y]) => x.powf(*y),
                        ("Math.min", _) => {
                            if v.iter().any(|n| n.is_nan()) {
                                f64::NAN
                            } else {
                                v.into_iter().fold(f64::INFINITY, f64::min)
                            }
                        }
                        ("Math.max", _) => {
                            if v.iter().any(|n| n.is_nan()) {
                                f64::NAN
                            } else {
                                v.into_iter().fold(f64::NEG_INFINITY, f64::max)
                            }
                        }
                        _ => return Err(format!("{name} argument count").into()),
                    };
                    Ok(Value::Number(result))
                }
                "GetFindObjects" => self.get_find_objects(&values),
                "FindObjects.AddObject" => self.add_find_object(values),
                "toLog" => {
                    if values.len() != 2 {
                        return Err("toLog expects document and text length".into());
                    }
                    let Value::Document(oid, scope) = values[0] else {
                        return Err("toLog expects Document".into());
                    };
                    let length = values[1].number()?;
                    if !length.is_finite() || length < 0.0 {
                        return Err("toLog invalid text length".into());
                    }
                    eprintln!("document access: scope={scope} oid={oid} text_bytes={length}");
                    Ok(Value::Null)
                }
                name if name.starts_with("format.") => self.format_call(name, receiver, values),
                "RegInfo.GetAttribute" => {
                    if values.len() != 1 {
                        return Err("RegInfo.GetAttribute expects one id".into());
                    }
                    let key = values[0].number()?.to_string();
                    let Some(Value::ReadOnlyObject(info)) = receiver else {
                        return Err("registration metadata receiver missing".into());
                    };
                    let value = info.borrow().get(&key).cloned().unwrap_or(Value::Null);
                    Ok(value)
                }
                "__codex_PAreaPage" => {
                    if values.len() != 4 {
                        return Err(
                            "P-area query expects database, SQL, offset and page size".into()
                        );
                    }
                    let Value::Database(base) = &values[0] else {
                        return Err("P-area database missing".into());
                    };
                    let scope = Self::db_scope(base)?;
                    let start = values[2].number()?;
                    let size = values[3].number()?;
                    if !start.is_finite()
                        || start < 0.0
                        || start > i32::MAX as f64
                        || start.fract() != 0.0
                        || !size.is_finite()
                        || size < 1.0
                        || size > 500.0
                        || size.fract() != 0.0
                    {
                        return Err("Invalid P-area page range".into());
                    }
                    let sql = values[1].text();
                    let host = self
                        .dbhost
                        .as_ref()
                        .ok_or("Database host missing")?
                        .borrow();
                    let page = host.object_page(scope, &sql, start as usize, size as usize)?;
                    // Native empty SQL results are null. Keep the empty-list
                    // branch, but retain a cursor for a page past a nonempty list.
                    if page.estimated == 0 && page.ids.is_empty() {
                        return Ok(Value::Null);
                    }
                    Ok(Value::Host(super::host::Handle::object_page(
                        scope, page, &sql,
                    )?))
                }
                "__codex_DistributionJudges" => {
                    if values.len() != 1 {
                        return Err("Distribution query expects SQL".into());
                    }
                    let host = self
                        .dbhost
                        .as_ref()
                        .ok_or("Database host missing")?
                        .borrow();
                    let ids = host.distribution_judges(0, &values[0].text())?;
                    Ok(Value::Host(super::host::Handle::identities(
                        0,
                        7345,
                        ids,
                        &values[0].text(),
                    )?))
                }
                "FindOrCreateHashObject" => {
                    if values.len() < 2 || values.len() > 4 {
                        return Err(
                            "FindOrCreateHashObject expects key, factory[, argument, version]"
                                .into(),
                        );
                    }
                    let Value::Function(factory) = &values[1] else {
                        return Err("hash factory must be a function".into());
                    };
                    // Execute in the request context; never cache user-dependent forms across users.
                    let result = self.invoke(
                        factory.clone(),
                        vec![values.get(2).cloned().unwrap_or(Value::Null)],
                        receiver,
                        "FindOrCreateHashObject".into(),
                    )?;
                    // The native object hash does not return a Boolean factory result.
                    // GenerateHashFormDocParam uses true to signal a URL was already rendered.
                    Ok(if matches!(result, Value::Bool(_)) {
                        Value::Null
                    } else {
                        result
                    })
                }
                "StartProfile" | "StopProfile" => {
                    if !values.is_empty() {
                        return Err("profile control expects no arguments".into());
                    }
                    if n == "StartProfile" {
                        self.profile = Some((std::time::Instant::now(), self.steps));
                    } else if let Some((start, steps)) = self.profile.take() {
                        eprintln!(
                            "KS profile: instructions={}, elapsed_us={}",
                            self.steps.saturating_sub(steps),
                            start.elapsed().as_micros()
                        );
                    }
                    Ok(Value::Null)
                }
                "escape" | "unescape" => {
                    let [value] = values.as_slice() else {
                        return Err("URL conversion expects one argument".into());
                    };
                    let s = value.text();
                    Ok(Value::String(if n == "unescape" {
                        // The Word transfer component encodes spaces as '+'.
                        // A literal plus arrives as %2B and is decoded once.
                        crate::server::percent_decode(&s.replace('+', " "))?
                    } else {
                        let (bytes, _, bad) = encoding_rs::WINDOWS_1251.encode(&s);
                        if bad {
                            return Err("URL value cannot be encoded as CP1251".into());
                        }
                        let mut out = String::new();
                        for b in bytes.iter() {
                            // KS escape is used for application/x-www-form-urlencoded
                            // values. Native KS escapes '+' (unlike JavaScript escape),
                            // otherwise Base64 classifier SQL becomes a space on input.
                            if b.is_ascii_alphanumeric() || b"@*_-./".contains(b) {
                                out.push(*b as char);
                            } else {
                                out.push_str(&format!("%{b:02X}"));
                            }
                        }
                        out
                    }))
                }
                "stream.StopBuffMode" | "stream.StartBuffMode" => {
                    if !values.is_empty() {
                        return Err("buffer mode takes no arguments".into());
                    }
                    self.http_output.borrow_mut().insert(
                        "\0buffering".into(),
                        Value::Bool(n == "stream.StartBuffMode"),
                    );
                    Ok(Value::Null)
                }
                "__codex_LinkFields" => self.selected_schema_links(&values),
                "eval" => {
                    let [source] = values.as_slice() else {
                        return Err("eval expects one expression".into());
                    };
                    let Value::String(source) = source else {
                        return Ok(source.clone());
                    };
                    if source.len() > 2 * 1024 * 1024 || self.eval_depth >= 64 {
                        return Err("eval size/depth limit".into());
                    }
                    let ast = parser::parse(&format!("return {source};"))
                        .map_err(|e| format!("eval expression syntax: {e}"))?;
                    self.eval_depth += 1;
                    let result = self.list(&ast).map_err(|e| match e {
                        Fault::Host(s) if s.contains("is not callable") => {
                            Fault::Script(Value::String(s))
                        }
                        other => other,
                    });
                    self.eval_depth -= 1;
                    match result? {
                        Flow::Return(v) => Ok(v),
                        _ => Err("eval expression did not return a value".into()),
                    }
                }
                "parseFloat" => {
                    if values.len() != 1 {
                        return Err("parseFloat expects one argument".into());
                    }
                    let input = values[0].text();
                    let text = input.trim_start();
                    let b = text.as_bytes();
                    let mut end = 0;
                    if b.first().is_some_and(|v| matches!(v, b'+' | b'-')) {
                        end += 1;
                    }
                    if text[end..].starts_with("Infinity") {
                        return Ok(Value::Number(if b.first() == Some(&b'-') {
                            f64::NEG_INFINITY
                        } else {
                            f64::INFINITY
                        }));
                    }
                    let mut digits = 0;
                    while end < b.len() && b[end].is_ascii_digit() {
                        digits += 1;
                        end += 1;
                    }
                    if b.get(end) == Some(&b'.') {
                        end += 1;
                        while end < b.len() && b[end].is_ascii_digit() {
                            digits += 1;
                            end += 1;
                        }
                    }
                    if digits == 0 {
                        return Ok(Value::Number(f64::NAN));
                    }
                    if b.get(end).is_some_and(|v| matches!(v, b'e' | b'E')) {
                        let exponent = end;
                        end += 1;
                        if b.get(end).is_some_and(|v| matches!(v, b'+' | b'-')) {
                            end += 1;
                        }
                        let start = end;
                        while end < b.len() && b[end].is_ascii_digit() {
                            end += 1;
                        }
                        if end == start {
                            end = exponent;
                        }
                    }
                    // Native 0076ddb0 accumulates decimal places one at a time.
                    let token = &text[..end];
                    let negative = token.starts_with('-');
                    let token = token.trim_start_matches(['-', '+']);
                    let (mantissa, exponent) = token.split_once(['e', 'E']).unwrap_or((token, "0"));
                    let mut n = 0.0;
                    let mut divisor = 0.0;
                    for c in mantissa.bytes() {
                        if c == b'.' {
                            divisor = 10.0;
                        } else if divisor == 0.0 {
                            n = n * 10.0 + (c - b'0') as f64;
                        } else {
                            n += (c - b'0') as f64 / divisor;
                            divisor *= 10.0;
                        }
                    }
                    n *= 10f64.powf(exponent.parse().unwrap_or(0.0));
                    Ok(Value::Number(if negative { -n } else { n }))
                }
                "FromBase64" | "ToBase64" => {
                    if values.len() != 1 {
                        return Err(format!("{n} expects one argument").into());
                    }
                    Ok(Value::String(if n == "FromBase64" {
                        base64::decode(&values[0].text())?
                    } else {
                        base64::encode(&values[0].text())?
                    }))
                }
                "isNaN" => {
                    if values.len() != 1 {
                        return Err("isNaN expects one argument".into());
                    }
                    Ok(Value::Bool(values[0].number().unwrap_or(f64::NAN).is_nan()))
                }
                "parseInt" => {
                    if values.is_empty() || values.len() > 2 {
                        return Err("parseInt expects text[, radix]".into());
                    }
                    let text = values[0].text();
                    let mut text = text.trim_start();
                    // Native KS maps an empty/null mode to array index zero;
                    // classifforispr calls modeFormList[parseInt(nmode)]().
                    if text.is_empty() {
                        return Ok(Value::Number(0.0));
                    }
                    let sign = if text.starts_with('-') {
                        text = &text[1..];
                        -1.0
                    } else {
                        if text.starts_with('+') {
                            text = &text[1..];
                        }
                        1.0
                    };
                    let mut radix = values
                        .get(1)
                        .map(Value::number)
                        .transpose()?
                        .map(to_i32)
                        .unwrap_or(0);
                    if radix != 0 && !(2..=36).contains(&radix) {
                        return Err("parseInt radix outside 2..36".into());
                    }
                    if (radix == 0 || radix == 16)
                        && (text.starts_with("0x") || text.starts_with("0X"))
                    {
                        text = &text[2..];
                        radix = 16;
                    }
                    if radix == 0 {
                        radix = 10;
                    }
                    let mut n = 0.0;
                    let mut count = 0;
                    for c in text.chars() {
                        let Some(d) = c.to_digit(radix as u32) else {
                            break;
                        };
                        n = n * (radix as f64) + d as f64;
                        count += 1;
                    }
                    // Native 0068ee60 -> 0076d8d0 returns integer zero when
                    // the text has no leading digits (including registry names).
                    Ok(Value::Number(if count == 0 { 0.0 } else { sign * n }))
                }
                "char" => {
                    if values.len() != 1 {
                        return Err("char requires one byte code".into());
                    }
                    let n = values[0].number()?;
                    if !n.is_finite() || n < 0.0 || n > 255.0 || n.fract() != 0.0 {
                        return Err("char byte code outside 0..255".into());
                    }
                    Ok(Value::String(crate::db::decode(&[n as u8])))
                }
                "KJSON.parse" => {
                    let [Value::String(source)] = values.as_slice() else {
                        return Err("KJSON.parse expects one string".into());
                    };
                    kjson::parse(source).map_err(|e| Fault::Script(Value::String(e)))
                }
                "KJSON.stringify" => {
                    let [value] = values.as_slice() else {
                        return Err("KJSON.stringify expects one value".into());
                    };
                    kjson::stringify(value)
                        .map(Value::String)
                        .map_err(Fault::from)
                }
                "String" => Ok(Value::String(
                    values.first().map(Value::text).unwrap_or_default(),
                )),
                "Number" => Ok(Value::Number(
                    values
                        .first()
                        .map(Value::number)
                        .transpose()?
                        .unwrap_or(0.0),
                )),
                "Boolean" => Ok(Value::Bool(values.first().is_some_and(Value::truth))),
                "At" => {
                    if self.replica_startup {
                        return Ok(Value::Bool(true));
                    }
                    if self.http_replica {
                        return Err("This KS action requires the catalog owner executor".into());
                    }
                    if values.is_empty() || values.len() > 2 {
                        return Err("At requires script[, seconds|HH:MM|deletetask]".into());
                    }
                    let name = values[0].text();
                    let when = match values.get(1) {
                        None => super::scheduler::When::Delay(0),
                        Some(Value::Number(n))
                            if n.is_finite()
                                && *n >= 0.0
                                && *n <= i32::MAX as f64
                                && n.fract() == 0.0 =>
                        {
                            super::scheduler::When::Delay(*n as u32)
                        }
                        Some(Value::String(s)) => {
                            let Some(w) = super::scheduler::parse_time(s) else {
                                return Ok(Value::Bool(false));
                            };
                            w
                        }
                        _ => return Err("invalid At time".into()),
                    };
                    let (now, day, minute) = super::scheduler::clock()?;
                    self.scheduler.schedule(
                        format!("{}/{}", self.path.trim_end_matches('/'), name),
                        when,
                        now,
                        day,
                        minute,
                    )?;
                    Ok(Value::Bool(true))
                }
                "pPath" => {
                    if !values.is_empty() {
                        return Err("pPath arguments not implemented".into());
                    }
                    Ok(Value::String(format!(
                        "{}?",
                        self.path.trim_end_matches('/')
                    )))
                }
                "Output" => Ok(self
                    .output_stream
                    .as_ref()
                    .map(|s| Value::ReadStream(s.clone()))
                    .unwrap_or(Value::HttpOutput)),
                "stream.Print" | "stream.PrintS" | "stream.write" => {
                    for v in values {
                        if n == "stream.Print" {
                            self.write_filtered(&v.text())?;
                        } else {
                            self.write(&v.text())?;
                        }
                    }
                    Ok(Value::Null)
                }
                _ => Err(format!("unknown native {n}").into()),
            },
            Value::ReadOnlyObject(o)
                if label == "Struct"
                    && values.is_empty()
                    && matches!(receiver, Some(Value::Database(_))) =>
            {
                Ok(Value::ReadOnlyObject(o))
            }
            Value::Function(f) => self.invoke(f, values, receiver, label),
            Value::FunctionMethod(f, method) => {
                let receiver = values
                    .first()
                    .cloned()
                    .filter(|v| !matches!(v, Value::Null));
                let args = if method == "call" {
                    values.into_iter().skip(1).collect()
                } else {
                    if values.len() > 2 {
                        return Err("apply expects receiver and array".into());
                    }
                    match values.get(1) {
                        None | Some(Value::Null) => Vec::new(),
                        Some(Value::Array(a)) => {
                            let a = a.borrow();
                            let n = a["length"].number()? as usize;
                            (0..n)
                                .map(|i| a.get(&i.to_string()).cloned().unwrap_or(Value::Null))
                                .collect()
                        }
                        _ => return Err("apply expects an array".into()),
                    }
                };
                self.invoke(f, args, receiver, label)
            }
            Value::Number(n)
                if label == "length"
                    && values.is_empty()
                    && matches!(
                        receiver,
                        Some(Value::File(_) | Value::ReadStream(_) | Value::HttpOutput)
                    ) =>
            {
                Ok(Value::Number(n))
            }
            _ => Err(format!(
                "KS member or function '{label}' is not callable; host binding may be required"
            )
            .into()),
        }
    }
    fn invoke(
        &mut self,
        f: Rc<Function>,
        mut values: Vec<Value>,
        receiver: Option<Value>,
        _label: String,
    ) -> Result<Value, Fault> {
        if _label == "waitForJobEnd"
            && matches!(f.properties.borrow().get("\0name"), Some(Value::String(name)) if name == "mp_at_CJobsQueue_waitForJobEnd")
        {
            if let Some(value) = self.run_waited_calendar_job(&values, receiver.as_ref())? {
                return Ok(value);
            }
        }
        if _label == "formRegistry" || f.name.as_deref() == Some("CRegGenerator_formRegistry") {
            let owner = receiver.clone().unwrap_or(self.get("this")?);
            if self.registry_is_filter_only(&owner)? {
                return Ok(Value::Null);
            }
        }
        let registry_cell = if _label == "fullCell" {
            self.registry_cell_target(&mut values)?
        } else {
            None
        };
        if _label == "CRegGenerator_getArrayCells" && self.get("bword")?.text() != "1" {
            let owner = receiver.clone().unwrap_or(self.get("this")?);
            self.assign_member(owner, "isDirectHTML".into(), Value::Bool(true))?;
        }
        if _label == "jc_expandXPathExcelCells"
            || f.name.as_deref() == Some("jc_expandXPathExcelCells")
        {
            return self.report_excel(&values);
        }
        if _label == "mp_xml_createDocument" || f.name.as_deref() == Some("mp_xml_createDocument") {
            return self.report_xml_document(&values);
        }
        let mut scope = HashMap::new();
        scope.insert("this".into(), receiver.unwrap_or(self.get("this")?));
        for (i, p) in f.params.iter().enumerate() {
            scope.insert(p.clone(), values.get(i).cloned().unwrap_or(Value::Null));
        }
        let mut args = HashMap::new();
        for (i, v) in values.iter().enumerate() {
            args.insert(i.to_string(), v.clone());
        }
        args.insert("length".into(), Value::Number(values.len() as f64));
        let args = Rc::new(RefCell::new(args));
        scope.insert("arguments".into(), Value::Array(args.clone()));
        self.call_arguments.push((f.clone(), args));
        // Dynamic with bindings belong to the calling statement, not the callee.
        let outer_with = std::mem::take(&mut self.with_objects);
        if let Some(name) = &f.name {
            scope.insert(name.clone(), Value::Function(f.clone()));
        } else if let Some(Value::String(name)) = f.properties.borrow().get("\0name") {
            // A queued job can retain a generated function after the HTTP
            // request's globals have been restored. Its own name must still
            // resolve for the native Function.arguments idiom. Preserve an
            // existing (possibly reassigned) declaration binding.
            if !f.env.iter().any(|env| env.borrow().contains_key(name)) {
                scope.insert(name.clone(), Value::Function(f.clone()));
            }
        }
        let outer_scopes = std::mem::replace(&mut self.scopes, f.env.clone());
        self.scopes.push(Rc::new(RefCell::new(scope)));
        let profile_start = if self.profile_functions {
            self.function_children.push(0);
            Some(std::time::Instant::now())
        } else {
            None
        };
        // Large linked cases legitimately exceed 64 nested KS calls. Grow the
        // native stack at this boundary instead of truncating the cascade or
        // overflowing the worker thread's stack. Execution stays on the same
        // thread, including its Rc/RefCell state and database locks.
        let r = stacker::maybe_grow(256 * 1024, 2 * 1024 * 1024, || self.list(&f.body));
        if let Some(start) = profile_start {
            let elapsed = start.elapsed().as_micros();
            let child = self.function_children.pop().unwrap_or(0);
            if let Some(parent) = self.function_children.last_mut() {
                *parent += elapsed;
            }
            let entry = self.function_times.entry(_label.clone()).or_default();
            entry.0 += 1;
            entry.1 += elapsed;
            entry.2 += elapsed.saturating_sub(child);
        }
        self.scopes = outer_scopes;
        self.call_arguments.pop();
        self.with_objects = outer_with;
        let r = r.map_err(|e| match e {
            Fault::Host(s) => Fault::Host(format!("{_label}: {s}")),
            Fault::Script(v) => {
                if std::env::var_os("KODEKS_TRACE").is_some_and(|s| s == "1") {
                    eprintln!("KS exception through {_label}: {}", v.text());
                }
                Fault::Script(v)
            }
        })?;
        match r {
            Flow::Return(v) => {
                if let Some(field) = registry_cell {
                    let html = self.rich_html(&field)?;
                    if !html.is_empty() {
                        return Ok(Value::String(html + &v.text()));
                    }
                }
                Ok(v)
            }
            Flow::Next => {
                if let Some(field) = registry_cell {
                    let html = self.rich_html(&field)?;
                    if !html.is_empty() {
                        return Ok(Value::String(html));
                    }
                }
                Ok(Value::Null)
            }
            _ => Err("break/continue escaped function".into()),
        }
    }
    pub fn take_binary_output(&mut self) -> Option<Vec<u8>> {
        self.binary_output.take()
    }
    pub fn render(&mut self, stmts: &[Stmt]) -> Result<String, String> {
        self.steps = 0;
        self.output.clear();
        self.binary_output = None;
        match self.list(stmts).map_err(|e| e.to_string())? {
            Flow::Next | Flow::Return(_) => Ok(std::mem::take(&mut self.output)),
            _ => Err("break/continue outside loop".into()),
        }
    }
    pub fn run(mut self, stmts: &[Stmt]) -> Result<String, String> {
        self.render(stmts)
    }
}
// Break closure environment cycles when a catalog runtime is discarded.
// This is not a collector for arbitrary user-created object cycles.
impl Drop for Runtime<'_> {
    fn drop(&mut self) {
        let cells = self
            .closure_cells
            .get_mut()
            .iter()
            .filter_map(std::rc::Weak::upgrade)
            .collect::<Vec<_>>();
        let mut seen = std::collections::HashSet::new();
        for c in cells {
            if seen.insert(Rc::as_ptr(&c) as usize) {
                c.borrow_mut().clear();
            }
        }
    }
}
fn array_index(key: &str, len: usize) -> Option<usize> {
    if key.is_empty()
        || (key.len() > 1 && key.starts_with('0'))
        || !key.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    key.parse::<usize>().ok().filter(|&i| i < len)
}
fn to_i32(n: f64) -> i32 {
    if !n.is_finite() || n == 0.0 {
        0
    } else {
        n.trunc().rem_euclid(4294967296.0) as u32 as i32
    }
}
fn field_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::WritableField(f), Value::String(_) | Value::Number(_)) => f
            .snapshot()
            .is_ok_and(|f| field_equal(&Value::StoredField(f), b)),
        (Value::String(_) | Value::Number(_), Value::WritableField(f)) => f
            .snapshot()
            .is_ok_and(|f| field_equal(a, &Value::StoredField(f))),
        (Value::EditDocument(d), Value::Number(_) | Value::String(_)) => {
            let d = d.borrow();
            field_equal(&Value::Document(d.oid, d.scope), b)
        }
        (Value::Number(_) | Value::String(_), Value::EditDocument(d)) => {
            let d = d.borrow();
            field_equal(a, &Value::Document(d.oid, d.scope))
        }
        (Value::Date(a), Value::Date(b)) => a.borrow().millis == b.borrow().millis,
        (Value::Date(date), Value::Number(n)) | (Value::Number(n), Value::Date(date)) => {
            date.borrow().millis as f64 == *n
        }
        (Value::Number(n), Value::String(s)) | (Value::String(s), Value::Number(n)) => {
            s.trim().parse::<f64>().ok() == Some(*n)
        }
        (Value::Document(oid, _), Value::Number(n))
        | (Value::Number(n), Value::Document(oid, _)) => *oid as f64 == *n,
        (Value::Document(oid, _), Value::String(s))
        | (Value::String(s), Value::Document(oid, _)) => {
            s.trim().parse::<f64>().ok() == Some(*oid as f64)
        }
        (Value::StoredField(f), Value::String(s)) | (Value::String(s), Value::StoredField(f)) => {
            f.display == *s
        }
        (Value::StoredField(f), Value::Number(n)) | (Value::Number(n), Value::StoredField(f)) => {
            f.value().ok().and_then(|v| v.number().ok()) == Some(*n)
        }
        _ => equal(a, b),
    }
}
fn equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::WritableField(a), Value::WritableField(b)) => {
            Rc::ptr_eq(&a.doc, &b.doc) && a.index == b.index
        }
        (Value::EditDocument(a), Value::EditDocument(b)) => Rc::ptr_eq(a, b),
        (Value::EditDocument(a), Value::Document(oid, scope))
        | (Value::Document(oid, scope), Value::EditDocument(a)) => {
            let a = a.borrow();
            a.oid == *oid && a.scope == *scope
        }
        (Value::Volume(a, b), Value::Volume(c, d)) => a == c && b.id == d.id,
        (Value::Document(a, b), Value::Document(c, d)) => a == c && b == d,
        (Value::StoredField(a), Value::StoredField(b)) => {
            a.oid == b.oid && a.scope == b.scope && a.index == b.index
        }
        (Value::Database(a), Value::Database(b)) => Rc::ptr_eq(a, b),
        (Value::Date(a), Value::Date(b)) => Rc::ptr_eq(a, b),
        (Value::Function(a), Value::Function(b)) => Rc::ptr_eq(a, b),
        (Value::Null, Value::Null) => true,
        (Value::Number(a), Value::Number(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Object(a), Value::Object(b))
        | (Value::Array(a), Value::Array(b))
        | (Value::ReadOnlyObject(a), Value::ReadOnlyObject(b)) => Rc::ptr_eq(a, b),
        _ => false,
    }
}
