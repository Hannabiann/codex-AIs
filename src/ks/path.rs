//! Script-facing spelling is distinct from filesystem canonicalization.
/// Do not expose the Windows verbatim prefix's '?' to KS wildcard detection.
/// Only drive and UNC prefixes are rewritten; other device namespaces stay intact.
pub fn script_path(text: &str) -> String {
    let text = text.replace('\\', "/");
    if let Some(rest) = text.strip_prefix("//?/UNC/") {
        return format!("//{rest}");
    }
    if let Some(rest) = text.strip_prefix("//?/") {
        let b = rest.as_bytes();
        if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'/' {
            return rest.to_owned();
        }
    }
    text
}
