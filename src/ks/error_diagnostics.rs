//! Snapshots are formatted only while unwinding a KS failure. No DB getters run.
use super::*;
use std::fmt::Write;

const REPORT_LIMIT: usize = 8 * 1024 * 1024;
const VALUE_LIMIT: usize = 4096;
#[derive(Default)]
pub(super) struct State {
    pub location: Option<parser::SourceSpan>,
    pub frames: Vec<Frame>,
    pub fault: Option<Rc<String>>,
    pub caught: Vec<Option<Rc<String>>>,
    // The nearest source-level catch decides whether the original exception
    // can be rethrown. A plain fallback needs no formatted failure snapshot.
    pub discard_script_fault: Vec<bool>,
    profile: HashMap<String, (u64, u128, u64)>,
    #[cfg(test)]
    captures: usize,
}
pub(super) struct Frame {
    label: String,
    function: Rc<Function>,
    locals: Object,
    callsite: Option<parser::SourceSpan>,
}
fn secret(name: &str) -> bool {
    let n = name.to_lowercase();
    ["password", "passwd", "парол", "authorization", "cookie", "secret", "token", "privatekey", "private_key"]
        .iter().any(|s| n.contains(s)) || matches!(n.as_str(), "pwd" | "auth" | "accesskey" | "access_key")
}
fn short(s: &str, max: usize) -> String {
    let mut value = s.chars().take(max).collect::<String>();
    if value.len() < s.len() { value.push_str(" … [truncated]"); }
    value
}
fn atom(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(VALUE_LIMIT));
    let mut chars = s.chars();
    for c in chars.by_ref().take(VALUE_LIMIT) {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => { let _ = write!(out, "\\u{{{:x}}}", c as u32); }
            c => out.push(c),
        }
    }
    if chars.next().is_some() { out.push_str(" … [truncated]"); }
    out
}

// Only prove very simple handlers. Calls, conditional logic and bare rethrow
// retain the complete original snapshot, including calls into another script
// that may execute a bare throw. Host failures are fatal even inside a catch.
pub(super) fn discards_exception(handler: &Stmt) -> bool {
    fn scalar(e: &Expr) -> bool {
        matches!(e, Expr::Name(_) | Expr::String(_) | Expr::Number(_))
    }
    match handler.unlocated() {
        Stmt::Empty | Stmt::Continue | Stmt::Break | Stmt::Return(None) => true,
        Stmt::Return(Some(e)) => scalar(e),
        Stmt::Block(statements) => statements.iter().all(discards_exception),
        Stmt::Var(bindings) => bindings.iter().all(|(_, v)| v.as_ref().is_none_or(scalar)),
        Stmt::Expression(Expr::Binary(op, target, v)) =>
            op == "=" && matches!(target.as_ref(), Expr::Name(_)) && scalar(v),
        _ => false,
    }
}
fn value(v: &Value, name: &str, depth: usize, seen: &mut Vec<usize>) -> (&'static str, String) {
    if secret(name) { return ("redacted", "[hidden]".into()); }
    match v {
        Value::Null => ("NULL", "NULL".into()),
        Value::Bool(v) => ("boolean", v.to_string()),
        Value::Number(v) => ("number", native_number_text(*v)),
        Value::Decimal(v) => ("LongDouble", v.text()),
        Value::String(s) | Value::ScriptException(s) => ("String", if s.starts_with("Basic ") || s.starts_with("Bearer ") {
            "[hidden]".into()
        } else { atom(s) }),
        Value::Document(oid, scope) => ("Document", format!("OID={oid}, context={scope}")),
        Value::RegistryKey(id) => ("RegistryKey", id.to_string()),
        Value::Function(f) | Value::FunctionMethod(f, _) => ("Function", function_name(f)),
        Value::Native(n) => ("Native function", atom(n)),
        Value::Object(o) | Value::Array(o) | Value::ReadOnlyObject(o) | Value::Database(o) | Value::Application(o) => {
            let kind = if matches!(v, Value::Array(_)) { "Array" } else { "Object" };
            let id = Rc::as_ptr(o) as usize;
            if seen.contains(&id) { return (kind, "[cycle]".into()); }
            let Ok(map) = o.try_borrow() else { return (kind, "[borrowed]".into()) };
            if depth >= 2 { return (kind, format!("{} members", map.len())); }
            seen.push(id);
            let mut keys = map.keys().filter(|n| !n.starts_with('\0')).collect::<Vec<_>>();
            keys.sort();
            let mut text = String::new();
            for (index, key) in keys.iter().take(128).enumerate() {
                if index != 0 { text.push_str(", "); }
                let (_, item) = value(&map[*key], key, depth + 1, seen);
                let _ = write!(text, "{}={item}", atom(key));
                if text.len() >= VALUE_LIMIT { break; }
            }
            if keys.len() > 128 { text.push_str(" … [members truncated]"); }
            seen.pop();
            (kind, short(&text, VALUE_LIMIT))
        }
        Value::Date(date) => ("Date", date.try_borrow().map(|d| format!("{:?}", d.parts())).unwrap_or_else(|_| "[borrowed]".into())),
        Value::StoredField(f) => ("Field", atom(&f.display)),
        Value::EditDocument(d) => ("EditDocument", d.try_borrow().map(|d| format!("OID={}", d.oid)).unwrap_or_else(|_| "[borrowed]".into())),
        Value::File(f) => ("File", atom(&f.path.to_string_lossy())),
        Value::LogFile(f) => ("LogFile", atom(&f.name)),
        Value::ReadStream(_) | Value::TextStream(_) | Value::RichStream(_) | Value::HttpOutput => ("Stream", "[contents omitted]".into()),
        Value::List(_) => ("RecordList", "[lazy DB contents not read]".into()),
        Value::Host(_) | Value::HostMethod(_, _) => ("Host object", "[host state not evaluated]".into()),
        Value::Unavailable(s) => ("Unavailable", atom(s)),
        _ => ("KS object", "[opaque handle]".into()),
    }
}
fn function_name(f: &Function) -> String {
    if let Some(name) = &f.name { return atom(name); }
    f.properties.try_borrow().ok().and_then(|p| match p.get("\0name") {
        Some(Value::String(s)) => Some(atom(s)), _ => None,
    }).unwrap_or_else(|| "anonymous".into())
}
fn table(out: &mut String, name: &str, scope: &Object) {
    let _ = writeln!(out, "\n{name}\nName\tType\tValue");
    let Ok(map) = scope.try_borrow() else { out.push_str("[scope borrowed]\n"); return };
    let mut keys = map.keys().filter(|n| !n.starts_with('\0')).collect::<Vec<_>>();
    keys.sort();
    for key in keys {
        if out.len() >= REPORT_LIMIT { out.push_str("[diagnostic truncated at 8 MiB]\n"); break; }
        let (kind, text) = if key == "arguments" {
            ("Array", "[see named Arguments table]".into())
        } else { value(&map[key], key, 0, &mut vec![Rc::as_ptr(scope) as usize]) };
        let _ = writeln!(out, "{}\t{kind}\t{text}", atom(key));
    }
}
fn location(span: &parser::SourceSpan) -> String {
    let start = span.start.min(span.source.text.len());
    let line = span.source.text[..start].bytes().filter(|b| *b == b'\n').count() + 1;
    format!("{}:{line} (UTF-8 byte {start})", atom(&span.source.name))
}
fn source(out: &mut String, span: &parser::SourceSpan) {
    let _ = writeln!(out, "\nSource: {}\nScript through the failing statement (>>>):", location(span));
    let mut offset = 0;
    let mut omitted = false;
    for (i, line) in span.source.text.split_inclusive('\n').enumerate() {
        if offset > span.end { break; }
        let failing = offset <= span.start && span.start < offset + line.len();
        if offset < 256 * 1024 || offset.saturating_add(16384) >= span.start {
            let text = if secret(line) || line.contains("Basic ") || line.contains("Bearer ") { "[line with sensitive data hidden]".into() }
                else if failing {
                    let from = span.start - offset;
                    let to = span.focus_end.min(offset + line.len()) - offset;
                    short(&format!("{}<< {} >>{}", &line[..from], &line[from..to],
                        line[to..].trim_end_matches(['\r', '\n'])), 8192)
                } else { short(line.trim_end_matches(['\r', '\n']), 8192) };
            let _ = writeln!(out, "{} {:6} | {text}", if failing { ">>>" } else { "   " }, i+1);
        } else if !omitted {
            out.push_str("    … [long source prefix truncated; failing location retained]\n"); omitted = true;
        }
        offset += line.len();
        if offset >= span.end && failing { break; }
    }
    if span.start >= span.source.text.len() { out.push_str(">>> << end of source >>\n"); }
}
impl Runtime<'_> {
    pub(super) fn diagnostic_needed(&self, fault: &Fault) -> bool {
        !matches!(fault, Fault::Script(_))
            || !self.diagnostics.discard_script_fault.last().copied().unwrap_or(false)
    }
    pub(super) fn diagnostic_enter(&mut self, label: &str, function: &Rc<Function>) {
        self.diagnostics.frames.push(Frame { label: label.into(), function: function.clone(),
            locals: self.scopes.last().unwrap().clone(), callsite: self.diagnostics.location.clone() });
    }
    pub(super) fn diagnostic_capture(&mut self) {
        if self.diagnostics.fault.is_some() { return; }
        let started = self.profile_functions.then(std::time::Instant::now);
        #[cfg(test)]
        { self.diagnostics.captures += 1; }
        let mut out = String::new();
        if let Some(span) = &self.diagnostics.location { source(&mut out, span); }
        else { out.push_str("\nSource: no source position available (native/compiled operation)\n"); }
        out.push_str("\nStack call function (entry → failure):\n");
        for (i, frame) in self.diagnostics.frames.iter().enumerate() {
            if out.len() >= REPORT_LIMIT { break; }
            let _ = writeln!(out, "\n#{} Function: {} / {}({})", i+1, atom(&frame.label),
                function_name(&frame.function), frame.function.params.iter().map(|s| atom(s)).collect::<Vec<_>>().join(", "));
            if let Some(callsite) = &frame.callsite { let _ = writeln!(out, "Called at: {}", location(callsite)); }
            if let Some((_, arguments)) = self.call_arguments.get(i) {
                out.push_str("Arguments (scalar values at call; objects show their current state):\nName\tType\tValue\n");
                if let Ok(args) = arguments.try_borrow() {
                    let len = match args.get("length") { Some(Value::Number(n)) => *n as usize, _ => 0 };
                    for n in 0..len.min(4096) {
                        let index = n.to_string();
                        let name = frame.function.params.get(n).map(String::as_str).unwrap_or(&index);
                        if let Some(v) = args.get(&index) {
                            let (kind, text) = value(v, name, 0, &mut vec![]);
                            let _ = writeln!(out, "{n}: {}\t{kind}\t{text}", atom(name));
                        }
                    }
                }
            }
            table(&mut out, "Local variables and constants", &frame.locals);
        }
        let mut seen = self.diagnostics.frames.iter().map(|f| Rc::as_ptr(&f.locals) as usize).collect::<std::collections::HashSet<_>>();
        if let Some(global) = self.scopes.first() {
            table(&mut out, "Global variables and constants", global);
            seen.insert(Rc::as_ptr(global) as usize);
        }
        for (i, scope) in self.scopes.iter().enumerate().skip(1).chain(self.with_objects.iter().enumerate()) {
            if seen.insert(Rc::as_ptr(scope) as usize) { table(&mut out, &format!("Captured/context scope {i}"), scope); }
        }
        for frame in &self.diagnostics.frames {
            for (i, scope) in frame.function.env.iter().enumerate() {
                if out.len() >= REPORT_LIMIT { break; }
                if seen.insert(Rc::as_ptr(scope) as usize) {
                    table(&mut out, &format!("Captured scope {i} of {}", atom(&frame.label)), scope);
                }
            }
        }
        if let Some(started) = started {
            let elapsed = started.elapsed().as_micros();
            let function = self.diagnostics.frames.last().map(|f| f.label.as_str()).unwrap_or("<global>");
            let site = self.diagnostics.location.as_ref().map_or_else(|| function.to_owned(),
                |span| format!("{}:{} / {function}", span.source.name, span.start));
            let key = if self.diagnostics.profile.len() < 64 || self.diagnostics.profile.contains_key(&site) {
                site
            } else { "<other sites>".into() };
            let row = self.diagnostics.profile.entry(key).or_default();
            row.0 += 1; row.1 += elapsed; row.2 += out.len() as u64;
        }
        self.diagnostics.fault = Some(Rc::new(out));
    }
    pub(super) fn finish_diagnostic_profile(&mut self) {
        let mut rows = std::mem::take(&mut self.diagnostics.profile).into_iter().collect::<Vec<_>>();
        rows.sort_by_key(|(_, (_, us, _))| std::cmp::Reverse(*us));
        let (count, us, bytes) = rows.iter().fold((0u64, 0u128, 0u64), |sum, (_, row)|
            (sum.0 + row.0, sum.1 + row.1, sum.2 + row.2));
        let thread = std::thread::current().id();
        eprintln!("KS diagnostic profile: thread={thread:?}, snapshots={count}, total_us={us}, bytes={bytes}");
        for (site, (count, us, bytes)) in rows.into_iter().take(12) {
            eprintln!("KS diagnostic site: thread={thread:?}, source={}, snapshots={count}, total_us={us}, bytes={bytes}", atom(&site));
        }
    }
    pub(super) fn diagnostic_native(&mut self, label: &str, values: &[Value], fault: &Fault) {
        if !self.diagnostic_needed(fault) { return; }
        if self.diagnostics.fault.is_some() { return; }
        self.diagnostic_capture();
        if let Some(report) = &mut self.diagnostics.fault {
            let out = Rc::make_mut(report);
            let _ = writeln!(out, "\nFailing host function: {}\nArguments:", atom(label));
            let sensitive = values.iter().any(|v| matches!(v, Value::String(s) if secret(s)));
            for (i, v) in values.iter().enumerate().take(128) {
                let (kind, text) = value(v, if sensitive { "secret" } else { label }, 0, &mut vec![]);
                let _ = writeln!(out, "{i}\t{kind}\t{text}");
            }
        }
    }
    pub(super) fn diagnostic_fault(&mut self, fault: &Fault) {
        if !self.diagnostic_needed(fault) { return; }
        self.diagnostic_capture();
        if let Some(report) = &mut self.diagnostics.fault {
            let out = Rc::make_mut(report);
            if out.contains("\nException:\n") { return; }
            out.push_str("\nException:\n");
            match fault {
                Fault::Script(v) => {
                    let (kind, text) = value(v, "exception", 0, &mut vec![]);
                    let _ = writeln!(out, "KS throw\t{kind}\t{text}");
                }
                Fault::Host(message) => { let _ = writeln!(out, "Host error\t{}", atom(message)); }
            }
        }
    }
    pub(super) fn diagnostic_syntax(&mut self, text: &str, name: &str, offset: usize) {
        self.diagnostics.location = Some(parser::SourceSpan { source: Rc::new(parser::Source {
            name: name.into(), text: text.into() }), start: offset,
            focus_end: offset + text.get(offset..).and_then(|s| s.chars().next()).map_or(0, char::len_utf8), end: offset });
        self.diagnostic_capture();
        self.diagnostics.location = None;
    }
    pub(super) fn diagnostic_bytecode(&mut self, program: &super::super::compiled::Program, pc: usize, entry: usize) {
        if self.diagnostics.fault.is_none() {
            let previous = self.diagnostics.location.take();
            self.diagnostic_capture();
            self.diagnostics.location = previous;
        }
        if let Some(report) = &mut self.diagnostics.fault {
            let out = Rc::make_mut(report);
            if out.contains("\nBytecode:") { return; }
            out.push_str("\nBytecode: source text unavailable; decoded instructions near failure\n");
            for (index, instruction) in program.instructions.iter().enumerate().take(pc+1).skip(entry.max(pc.saturating_sub(32))) {
                let _ = writeln!(out, "{} {:#x} opcode={:#x} number={} text={}", if index == pc { ">>>" } else { "   " },
                    instruction.at, instruction.op, instruction.number, if secret(&instruction.text) { "[hidden]".into() } else { atom(&instruction.text) });
            }
        }
    }
    pub fn take_error_diagnostic(&mut self) -> String {
        self.diagnostic_capture();
        self.diagnostics.fault.take().map(|r| (*r).clone()).unwrap_or_default()
    }
    pub fn clear_error_diagnostic(&mut self) {
        self.diagnostics.fault = None;
        self.diagnostics.caught.clear();
    }
}

#[cfg(test)]
#[path = "error_diagnostics_tests.rs"]
mod tests;
