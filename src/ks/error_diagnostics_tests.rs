use super::*;

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
