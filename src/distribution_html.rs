//! Browser controls for the native compiled distribution form. Assignment,
//! eligibility and coefficient calculations still run in the stored KS.
pub fn adapt(mut html: String, object: &str) -> String {
    if !object.eq_ignore_ascii_case("Raspred_Del_Auto")
        || !html.contains("function showSelect(")
        || !html.contains("p27.names")
    {
        return html;
    }
    let patch = concat!(
        "\n<script>\n",
        include_str!("distribution_browser.js"),
        "\n</script>\n"
    );
    let end = html
        .to_ascii_lowercase()
        .rfind("</body>")
        .unwrap_or(html.len());
    html.insert_str(end, patch);
    html
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_native_distribution_controls_are_adapted() {
        let form = "<body><form><input name='a123' value='456'></form><script>function showSelect(){};p27.names=[];</script></body>";
        assert_eq!(adapt(form.into(), "other"), form);
        assert_eq!(adapt("No access".into(), "Raspred_Del_Auto"), "No access");
        let actual = adapt(form.into(), "raspred_del_auto");
        assert!(actual.starts_with(form.trim_end_matches("</body>")));
        assert!(actual.ends_with("</script>\n</body>"));
        assert_eq!(actual.matches("<input name='a123' value='456'>").count(), 1);
        assert!(!include_str!("distribution_browser.js").contains("showModalDialog"));
    }
}
