//! Request-scoped access for the verified archive issue/return handlers.
use super::{Fault, Runtime, Value};

#[derive(Default)]
pub(super) struct Access {
    can_write: bool,
    pub legacy_entry: bool,
}

impl Access {
    pub fn request(
        path: &str,
        command: &str,
        query: &str,
        method: &str,
        authenticated: bool,
        has_body: bool,
        scheduled: bool,
    ) -> Self {
        let allowed =
            !scheduled && authenticated && matches!(path, "/20a" | "/20a/") && command == "arx";
        Self {
            can_write: allowed && method == "POST",
            // An old menu entry, not a submitted worker operation. Numeric IDs
            // changed in later KS revisions; never remap a POST or its objects.
            legacy_entry: allowed
                && method == "GET"
                && !has_body
                && matches!(query, "arx&mode=7&force" | "arx&mode=7&force="),
        }
    }
}

impl Runtime<'_> {
    pub(super) fn archive_begin_write(&mut self) -> Result<Value, Fault> {
        if !self.archive_access.can_write {
            return Err(
                "Выдача и возврат из архива требуют авторизованной отправки формы POST".into(),
            );
        }
        self.document_write_allowed = true;
        Ok(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_redirect_renders_path_object_without_losing_directory_settings() {
        for path in ["/20a", "/20a/", "/another-court"] {
            let mut runtime = Runtime::new(path);
            runtime.execute_code("this.WorkJud = 123;", None).unwrap();
            runtime.begin_request(&crate::server::RequestContext {
                target: format!("{path}?arxgiveout&"),
                method: "GET".into(),
                ..Default::default()
            }).unwrap();
            let source = r#"
                if (Request.PathObject.WorkJud != 123) throw 'directory settings lost';
                if (typeof Request.PathObject != 'BaseVirtualDir') throw 'directory type lost';
                {/<script>document.location=" %(Request.PathObject) ?arx&mode=7&force=";</script>/}
            "#;
            let ast = super::super::parser::parse(source).unwrap();
            let expected = "<script>document.location=\"?arx&mode=7&force=\";</script>";
            assert_eq!(runtime.render(&ast).unwrap().trim(), expected);
            runtime.end_request();
            assert_eq!(runtime.get("this").unwrap().text(), "");
            assert!(!runtime.document_write_allowed);
        }
    }

    #[test]
    fn legacy_entry_never_grants_write_or_remaps_submitted_objects() {
        let get = Access::request("/20a", "arx", "arx&mode=7&force", "GET", true, false, false);
        assert!(get.legacy_entry);
        assert!(!get.can_write);
        for query in [
            "arx&mode=7&force&nd=123",
            "arx&mode=7&force=0",
            "arx&mode=arxgiveout&force",
        ] {
            assert!(!Access::request("/20a", "arx", query, "GET", true, false, false).legacy_entry);
        }
        let mut runtime = Runtime::new("/20a");
        runtime.archive_access = get;
        assert!(runtime.archive_begin_write().is_err());
        runtime.archive_access =
            Access::request("/20a", "arx", "arx&mode=22", "POST", true, true, false);
        assert!(!runtime.document_write_allowed);
        runtime.archive_begin_write().unwrap();
        assert!(runtime.document_write_allowed);
        runtime.end_request();
        assert!(!runtime.document_write_allowed);
        assert!(runtime.archive_begin_write().is_err());
    }

    #[test]
    fn archive_write_gate_excludes_other_routes_anonymous_and_jobs() {
        for (path, command, method, auth, scheduled) in [
            ("/99", "arx", "POST", true, false),
            ("/20a", "mgmt", "POST", true, false),
            ("/20a", "arx", "POST", false, false),
            ("/20a", "arx", "POST", true, true),
            ("/20a", "arx", "GET", true, false),
        ] {
            assert!(
                !Access::request(path, command, "arx", method, auth, true, scheduled).can_write
            );
        }
    }

    #[test]
    fn archive_selection_decodes_repeated_objects_in_native_forms() {
        let expected = vec![
            ("mode".into(), "arxgiveout".into()),
            ("nm_cases".into(), "123;456".into()),
        ];
        assert_eq!(
            crate::http_form::decode_for(
                "/20a?arx",
                "application/x-www-form-urlencoded",
                b"mode=arxgiveout&nm_cases=123&nm_cases=456",
            )
            .unwrap(),
            expected
        );
        let mut body = String::new();
        for (name, value) in [
            ("mode", "arxgiveout"),
            ("nm_cases", "123"),
            ("nm_cases", "456"),
        ] {
            body.push_str(&format!(
                "--test\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            ));
        }
        body.push_str("--test--\r\n");
        assert_eq!(
            crate::http_form::decode_for(
                "/20a?arx",
                "multipart/form-data; boundary=test",
                body.as_bytes(),
            )
            .unwrap(),
            expected
        );
        for body in [
            b"mode=22&mode=21".as_slice(),
            b"nm_cases=123&nm_cases=bad",
            b"nm_cases=123&nm_cases=0",
        ] {
            assert!(crate::http_form::decode_for(
                "/20a?arx",
                "application/x-www-form-urlencoded",
                body,
            )
            .is_err());
        }
        assert!(crate::http_form::decode_for(
            "/20a?other",
            "application/x-www-form-urlencoded",
            b"nm_cases=123&nm_cases=456",
        )
        .is_err());
    }
}
