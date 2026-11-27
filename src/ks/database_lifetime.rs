//! Request cleanup for temporary DataBase mount tables. The identity cache is
//! not an owner: documents, lists, JDBC handles and retained closures can own
//! a context even after its DataBase variable has gone out of scope.
use super::{Object, Runtime, Value};
use std::{
    collections::{BTreeSet, HashSet},
    rc::Rc,
};

#[derive(Default)]
struct Reachable {
    scopes: BTreeSet<u32>,
    objects: HashSet<usize>,
    pending: Vec<Value>,
}
impl Reachable {
    fn object(&mut self, object: &Object) {
        if self.objects.insert(Rc::as_ptr(object) as usize) {
            self.pending.extend(object.borrow().values().cloned());
        }
    }
    fn scope(&mut self, scope: u32, runtime: &Runtime<'_>) {
        if self.scopes.insert(scope) {
            if let Some(object) = runtime.dbobjects.borrow().get(&scope) {
                self.object(object);
            }
        }
    }
    fn drain(&mut self, runtime: &Runtime<'_>) {
        while let Some(value) = self.pending.pop() {
            match value {
                Value::Database(o) | Value::DatabaseMethod(o, _) => {
                    if let Ok(scope) = Runtime::db_scope(&o) {
                        self.scope(scope, runtime);
                    }
                    self.object(&o);
                }
                Value::Object(o)
                | Value::Array(o)
                | Value::ArrayMethod(o, _)
                | Value::ReadOnlyObject(o)
                | Value::Application(o) => self.object(&o),
                Value::Function(f) | Value::FunctionMethod(f, _) => {
                    self.object(&f.properties);
                    for o in &f.env {
                        self.object(o);
                    }
                }
                Value::Document(_, s) | Value::DocumentMethod(_, s, _) | Value::Volume(s, _) => {
                    self.scope(s, runtime)
                }
                Value::StoredField(f) | Value::StoredFieldMethod(f, _) => {
                    self.scope(f.scope, runtime)
                }
                Value::EditDocument(d) | Value::EditMethod(d, _) => {
                    self.scope(d.borrow().scope, runtime)
                }
                Value::WritableField(f) => self.scope(f.doc.borrow().scope, runtime),
                Value::List(l) | Value::ListMethod(l, _) => self.scope(l.borrow().scope, runtime),
                Value::FormTab(t) | Value::FormTabMethod(t, _) => self.scope(t.scope, runtime),
                Value::Host(h) | Value::HostMethod(h, _) => {
                    if let Some(s) = h.database_scope() {
                        self.scope(s, runtime);
                    }
                }
                Value::RichField(r) | Value::RichMethod(r, _) => {
                    for o in &r.borrow().parts {
                        self.object(o);
                    }
                }
                Value::TempDocument(d) | Value::TempMethod(d, _) => {
                    self.object(&d.borrow().properties)
                }
                // Exhaustive matching makes new scope-owning value types an
                // explicit lifetime decision instead of silently losing them.
                Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::Decimal(_)
                | Value::DecimalMethod(_, _)
                | Value::String(_)
                | Value::ScriptException(_)
                | Value::Native(_)
                | Value::Unavailable(_)
                | Value::StringMethod(_, _)
                | Value::Css(_)
                | Value::CssRules(_)
                | Value::CssMethod(_, _)
                | Value::Signal(_)
                | Value::SignalMethod(_, _)
                | Value::HttpOutput
                | Value::Xml(_)
                | Value::XmlMethod(_, _)
                | Value::LogFile(_)
                | Value::LogMethod(_, _)
                | Value::Regex(_)
                | Value::Mutex(_)
                | Value::MutexMethod(_, _)
                | Value::RegistryKey(_)
                | Value::RegistryMethod(_, _)
                | Value::DirStat(_)
                | Value::File(_)
                | Value::AncestorDirectory(_)
                | Value::FileMethod(_, _)
                | Value::FilePath(_)
                | Value::PathMethod(_, _)
                | Value::TextField(_)
                | Value::FieldMethod(_, _)
                | Value::TextStream(_)
                | Value::RichStream(_)
                | Value::StreamMethod(_, _)
                | Value::ReadStream(_)
                | Value::ReadStreamMethod(_, _)
                | Value::Date(_)
                | Value::DateMethod(_, _) => {}
            }
        }
    }
}
impl Runtime<'_> {
    pub(super) fn release_unused_databases(&mut self) {
        let Some(host) = &self.dbhost else { return };
        let mut live = Reachable::default();
        live.scope(0, self);
        if let Some(scope) = self.user_scope {
            live.scope(scope, self);
        }
        for object in self
            .scopes
            .iter()
            .chain(&self.with_objects)
            .chain(self.config.iter())
        {
            live.object(object);
        }
        live.object(&self.http_output);
        for (function, arguments) in &self.call_arguments {
            live.pending.push(Value::Function(function.clone()));
            live.object(arguments);
        }
        live.pending.extend(self.caught.iter().cloned());
        live.drain(self);
        self.dbobjects.borrow_mut().retain(|scope, object| {
            let keep = live.scopes.contains(scope);
            // Unreachable custom properties can point back to the DataBase.
            // Break those Rc cycles before dropping the identity cache entry.
            if !keep {
                object.borrow_mut().clear();
            }
            keep
        });
        host.borrow_mut().retain_scopes(&live.scopes);
    }
}
