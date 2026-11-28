use super::*;

#[test]
fn profiling_counts_snapshots_once_and_resets_between_requests() {
    let mut r = Runtime::new("/20a");
    r.profile_functions = true;
    assert!(render(&mut r, "function fail(x) { MissingHost(x); } fail(7);").is_err());
    assert_eq!(r.diagnostics.profile.values().map(|v| v.0).sum::<u64>(), 1);
    assert!(r.diagnostics.profile.values().map(|v| v.2).sum::<u64>() > 0);
    let report = r.take_error_diagnostic();
    assert!(report.contains("x\tnumber\t7"));
    assert_eq!(r.diagnostics.profile.values().map(|v| v.0).sum::<u64>(), 1);
    r.finish_function_profile();
    assert!(r.diagnostics.profile.is_empty());
    render(&mut r, "try { throw 'optional'; } catch(e) { return null; }").unwrap();
    assert!(r.diagnostics.profile.is_empty());
}

#[test]
fn diagnostic_profiling_is_opt_in_and_site_storage_is_bounded() {
    let mut r = Runtime::new("/20a");
    r.profile_functions = false;
    assert!(render(&mut r, "MissingHost();").is_err());
    assert!(r.diagnostics.profile.is_empty());
    r.profile_functions = true;
    for i in 0..100 {
        let ast = r.parse_cached_named("MissingHost();", &format!("site-{i}.ks")).unwrap();
        assert!(r.render(&ast).is_err());
    }
    assert_eq!(r.diagnostics.profile.len(), 65);
    assert_eq!(r.diagnostics.profile.values().map(|v| v.0).sum::<u64>(), 100);
    let ast = r.parse_cached_named("MissingHost();", "site-0.ks").unwrap();
    assert!(r.render(&ast).is_err());
    let first = r.diagnostics.profile.iter().find(|(key, _)| key.starts_with("site-0.ks:")).unwrap().1;
    assert_eq!(first.0, 2, "an existing site must stay distinct after the bound");
}

#[test]
fn routine_fallbacks_do_not_format_discarded_exceptions() {
    let mut r = Runtime::new("/20a");
    let source = "function absent() { throw 'not present'; } function fallback() { try { absent(); } catch(e) { return null; } } var total=0; for(var i=0;i<200;i++) { fallback(); try { absent(); } catch(e) { total=7; continue; } } if(total!=7) throw 'wrong result';";
    render(&mut r, source).unwrap();
    assert_eq!(r.diagnostics.captures, 0);
    assert!(r.diagnostics.discard_script_fault.is_empty());
    assert!(r.diagnostics.fault.is_none());
}

#[test]
fn fallback_does_not_suppress_fatal_host_diagnostics() {
    let mut r = Runtime::new("/20a");
    assert!(render(&mut r, "function fatal(n) { MissingHost(n); } try { fatal(19); } catch(e) { return null; }").is_err());
    let report = r.take_error_diagnostic();
    assert!(report.contains("Function: fatal"));
    assert!(report.contains("n\tnumber\t19"));
    assert!(r.diagnostics.discard_script_fault.is_empty());
}

#[test]
fn nested_rethrow_and_finally_keep_the_original_snapshot() {
    let mut r = Runtime::new("/20a");
    let source = "var shared={before:42}; function origin(n) { throw 'original'; } function relay() { throw; } try { try { origin(17); } catch(e) { relay(); } finally { shared.before=99; } } catch(e) { throw; }";
    assert!(render(&mut r, source).is_err());
    let report = r.take_error_diagnostic();
    assert!(report.contains("Function: origin"));
    assert!(report.contains("n\tnumber\t17"));
    assert!(report.contains("before=42"));
    assert!(!report.split("Stack call function").nth(1).unwrap().contains("before=99"));
    assert!(r.diagnostics.discard_script_fault.is_empty());
}

#[test]
fn outer_fallback_does_not_hide_an_inner_fatal_handler() {
    let mut r = Runtime::new("/20a");
    assert!(render(&mut r, "try { try { throw 'handled'; } catch(e) { MissingHost(23); } } catch(e) { return null; }").is_err());
    let report = r.take_error_diagnostic();
    assert!(report.contains("Failing host function: MissingHost"));
    assert!(report.contains("0\tnumber\t23"));
    assert!(!report.contains("KS throw\tString\thandled"));
}

#[test]
fn diagnostic_atoms_preserve_unicode_escaping_and_limits() {
    assert_eq!(atom("Строка\n\r\t\u{7}🙂"), "Строка\\n\\r\\t\\u{7}🙂");
    let exact = "я".repeat(VALUE_LIMIT);
    assert_eq!(atom(&exact), exact);
    assert_eq!(atom(&(exact.clone()+"🙂")), exact+" … [truncated]");
}

#[test]
fn source_fault_preserves_locals_arguments_and_origin() {
    let mut r = Runtime::new("/20a");
    let source = "const rate = 7;\nfunction inner(n) { var before = 42; MissingHost(n, before); }\nfunction outer(x) { var caller = 'saved'; inner(x + rate); }\nouter(3);";
    let ast = r.parse_cached_named(source, "spb.dbs/test-error").unwrap();
    assert!(r.render(&ast).is_err());
    let report = r.take_error_diagnostic();
    for expected in ["spb.dbs/test-error:2", ">>>", "<< MissingHost >>", "before\tnumber\t42", "caller\tString\tsaved", "n\tnumber\t10", "rate\tnumber\t7", "Function: outer", "Function: inner", "MissingHost"] {
        assert!(report.contains(expected), "missing {expected}: {report}");
    }
}

fn render(r: &mut Runtime<'_>, source: &str) -> Result<String, String> {
    let ast = r.parse_cached_named(source, "fixture.ks").unwrap();
    r.render(&ast)
}
#[test]
fn queued_execute_sees_live_directory_globals_and_keeps_its_arguments() {
    let mut r = Runtime::new("/20a");
    render(&mut r, "var jobContext=null; function enter(context) { jobContext=context; }").unwrap();
    r.scheduled_request = true;
    let args = Rc::new(RefCell::new(HashMap::from([
        ("bfExport".into(), Value::Bool(true)),
        ("job".into(), Value::String("1".into())),
    ])));
    let result = r.execute_code_named(
        "function exportJob() { return jobContext.bfExport && imported(); } this.imported=function() {return true;}; enter(this); return exportJob();",
        Some(Value::Object(args.clone())), "queued fixture").unwrap();
    assert!(matches!(result, Value::Bool(true)));
    assert_eq!(args.borrow().get("job").unwrap().text(), "1");
    assert!(!args.borrow().contains_key("jobContext"), "directory bindings must not be copied into job arguments");
}
#[test]
fn path_mount_descriptors_preserve_configured_write_access() {
    use crate::db::{ConfigValue as C, VirtualDir};
    let dir = VirtualDir { path:"/bras".into(), properties:std::collections::BTreeMap::from([
        ("Path".into(),C::Text("/bras".into())),
        ("Base1".into(),C::Text("archive.db4".into())),
        ("Base2".into(),C::Text("active.db4".into())),
        ("Redact2".into(),C::Integer(1)),
        ("Base3".into(),C::Text("other.db4".into())),
        ("Redact3".into(),C::Text("1".into())),
        ("Base4".into(),C::Text("locked.db4".into())),
        ("Redact4".into(),C::Integer(0)),
    ]), ..Default::default() };
    let mut r=Runtime::new("/20a").with_application_config(&Default::default()).with_http_paths(&[dir]);
    render(&mut r, "var mounts=Application.Paths[0].vol; if(mounts.length!=5 || !mounts[1].vol.readOnly || mounts[2].vol.readOnly || mounts[3].vol.readOnly || !mounts[4].vol.readOnly) throw 'incorrect volume flags';").unwrap();
    assert!(!r.document_write_allowed);
}
#[test]
fn base64_stream_preserves_binary_bytes_and_returns_a_readable_stream() {
    let mut r=Runtime::new("/test");
    let source=InputStream::value(&[0,1,254,255]);
    let encoded=r.call_host(Value::Native("ToBase64".into()),vec![source.clone()],None,"ToBase64".into()).unwrap();
    let Value::ReadStream(ref stream)=encoded else { panic!("stream expected") };
    assert_eq!(stream.borrow().data,b"AAH+/w==");
    let decoded=r.call_host(Value::Native("FromBase64".into()),vec![encoded.clone()],None,"FromBase64".into()).unwrap();
    let Value::ReadStream(stream)=decoded else { panic!("stream expected") };
    assert_eq!(stream.borrow().data,vec![0,1,254,255]);
    let Value::ReadStream(stream)=source else {unreachable!()};
    assert_eq!(stream.borrow().position,4);
    assert_eq!(base64::decode(&base64::encode("Строка").unwrap()).unwrap(),"Строка");
}
#[test]
fn handled_errors_do_not_leak_into_later_failures_or_requests() {
    let mut r = Runtime::new("/20a");
    assert!(render(&mut r, "function oldFail() { throw 'handled'; } try { oldFail(); } catch(e) {} var fine=3;").is_ok());
    assert!(r.diagnostics.fault.is_none());
    assert!(render(&mut r, "function newFail(v) { NewMissing(v); } newFail(9);").is_err());
    let report = r.take_error_diagnostic();
    assert!(!report.contains("Function: oldFail"));
    assert!(report.contains("Function: newFail"));
    assert!(report.contains("v\tnumber\t9"));
    assert!(render(&mut r, "var done=1;").is_ok());
    assert!(r.diagnostics.fault.is_none());
}
#[test]
fn rethrow_preserves_origin_but_finally_replacement_has_new_origin() {
    let mut r = Runtime::new("/20a");
    assert!(render(&mut r, "function origin(n) { var before=17; throw 'original'; } try { origin(2); } catch(e) { throw; }").is_err());
    let report = r.take_error_diagnostic();
    assert!(report.contains("Function: origin"));
    assert!(report.contains("<< throw >>"));
    assert!(report.contains("before\tnumber\t17"));
    assert!(render(&mut r, "function old() { throw 'old'; } function replacement() { throw 'new'; } try { old(); } finally { replacement(); }").is_err());
    let report = r.take_error_diagnostic();
    assert!(report.contains("Function: replacement"));
    assert!(!report.contains("Function: old /"));
}
#[test]
fn diagnostic_redacts_secrets_and_handles_cycles_without_host_evaluation() {
    let mut r = Runtime::new("/20a");
    let source = "var password='SECRET_VALUE_1';\nvar obj={ok:1}; obj.self=obj;\nfunction fail(password) { MissingHost(); }\nfail('SECRET_VALUE_2');";
    assert!(render(&mut r, source).is_err());
    let report = r.take_error_diagnostic();
    // The literal on the caller's source line is not identifiable by its name;
    // the variable/argument tables must still hide sensitive bindings.
    let tables = report.split("Stack call function").nth(1).unwrap();
    assert!(!tables.contains("SECRET_VALUE_1"));
    assert!(!tables.contains("SECRET_VALUE_2"));
    assert!(tables.contains("[hidden]"));
    assert!(tables.contains("[cycle]"));
    assert!(report.len() < REPORT_LIMIT);
}
#[test]
fn source_wrappers_keep_hoisting_with_synchronized_loops_and_closures() {
    let mut r = Runtime::new("/20a");
    let source = "var x=first(3); synchronized function first(n) { var total=0; for(var i=0;i<n;i++) total+=i; return total; } var f=function(n) { return function() { return n+1; }; }; var got=f(x)(); if(got!=4) throw 'wrong'; var a={a:1,b:2}; for(var k in a) { x+=a[k]; } if(x!=6) throw 'loop';";
    assert!(render(&mut r, source).is_ok());
}
#[test]
fn bytecode_failure_reports_offset_and_arguments() {
    let mut bytes = Vec::new();
    fn op(bytes: &mut Vec<u8>, code: u16) { bytes.extend_from_slice(&[0;6]); bytes.extend_from_slice(&code.to_le_bytes()); }
    op(&mut bytes, 2); bytes.push(7); bytes.extend_from_slice(b"Missing");
    op(&mut bytes, 3); bytes.extend_from_slice(&42i32.to_le_bytes());
    op(&mut bytes, 0x2a); bytes.push(1);
    op(&mut bytes, 0);
    let ast = crate::ks::compiled::parse(&bytes).unwrap();
    let mut r = Runtime::new("/20a");
    assert!(r.render(&ast).is_err());
    let report = r.take_error_diagnostic();
    assert!(report.contains("Bytecode:"));
    assert!(report.contains("opcode=0x2a"));
    assert!(report.contains("0\tnumber\t42"));
}
#[test]
fn syntax_error_keeps_source_and_offset() {
    let mut r = Runtime::new("/20a");
    assert!(r.parse_cached_named("var x=1;\nvar y = ;", "broken.ks").is_err());
    let report = r.take_error_diagnostic();
    assert!(report.contains("broken.ks:2"));
    assert!(report.contains(">>>"));
}

#[test]
fn thrown_exception_object_keeps_description_and_code() {
    let mut r = Runtime::new("/20a");
    let source = "function mp_xCreate(code, text) { return {Code:code, Description:text}; }\nfunction getAttribute(attrId) { throw mp_xCreate(695, 'Invalid attribute: '+attrId); }\ngetAttribute(17);";
    assert!(render(&mut r, source).is_err());
    let report = r.take_error_diagnostic();
    assert!(report.contains("<< throw >> mp_xCreate"));
    assert!(report.contains("Code=695"));
    assert!(report.contains("Description=Invalid attribute: 17"));
    assert!(report.contains("attrId\tnumber\t17"));
}

#[test]
fn captures_closed_over_variables_of_callers() {
    let mut r = Runtime::new("/20a");
    assert!(render(&mut r, "function fail() { Missing(); } function make(captured) { return function() { fail(); }; } var f=make(71); f();").is_err());
    let report = r.take_error_diagnostic();
    assert!(report.contains("captured\tnumber\t71"));
}

#[test]
fn template_failure_marks_the_interpolation_line() {
    let mut r = Runtime::new("/20a");
    assert!(render(&mut r, "{/\n<html>\n<div> %Missing(17) </div>\n</html>\n/}").is_err());
    let report = r.take_error_diagnostic();
    assert!(report.contains("fixture.ks:3"));
    assert!(report.contains("%<< Missing >>(17)"));
    assert!(report.contains("0\tnumber\t17"));
}
