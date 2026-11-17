//! Preserve native classifier/calendar forms while replacing their obsolete popup
//! unload handshake. The initial about:blank unload listener is discarded
//! during navigation in modern browsers. Results use same-origin postMessage.
pub fn adapt(html: String, object: &str) -> String {
    // Match one listener before inspecting its callback. A single regex that
    // searches for a classifier callback across arbitrary text can start at a
    // preceding calendar listener and swallow whole JavaScript functions.
    let listener =
        regex::Regex::new(r"(?s)childWindow\.addEventListener\('unload',function\(\)\{.*?\}\s*\);")
            .unwrap();
    let callback = regex::Regex::new(
        r"(callbackDialog(?:Run)?Classif)\(childWindow\.returnValue, oName, flag, childWindow\.dialogArguments\);",
    ).unwrap();
    let calendar = regex::Regex::new(
        r"callbackDialog\(childWindow\.returnValue,\s*oName,\s*flag,\s*postfix\);",
    )
    .unwrap();
    let mut html = listener
        .replace_all(&html, |c: &regex::Captures| {
            let callback_match = callback.captures(&c[0]);
            let (kind, callback_name, argument, changed) =
                if let Some(callback_match) = &callback_match {
                    (
                        "classifier",
                        &callback_match[1],
                        "vFeatures",
                        "args[2] = event.data.changed ? 1 : 0;",
                    )
                } else if calendar.is_match(&c[0]) {
                    ("calendar", "callbackDialog", "postfix", "")
                } else {
                    return c[0].to_string();
                };
            format!(
                r#"
        (function(child, name, action, args) {{
            function receive(event) {{
                if (event.origin !== location.origin || event.source !== child ||
                    !event.data || event.data.kind !== 'kodeks-{kind}-result') return;
                window.removeEventListener('message', receive);
                clearInterval(cleanup);
                {changed}
                {callback_name}(event.data.value, name, action, args);
            }}
            window.addEventListener('message', receive);
            var cleanup = setInterval(function() {{
                if (child.closed) {{ clearInterval(cleanup); setTimeout(function() {{
                    window.removeEventListener('message', receive);
                }}, 1000); }}
            }}, 1000);
        }})(childWindow, oName, flag, {argument});
    "#
            )
        })
        .into_owned();
    if object == "classifforispr" || object == "ulistforispr" {
        html = html.replace("window.close();", r#"{ if (window.opener) window.opener.postMessage({kind:'kodeks-classifier-result', value:window.returnValue || '', changed:!!(window.dialogArguments && window.dialogArguments[2])}, location.origin); window.close(); }"#);
    }
    if object == "calendar" {
        html = html.replace("window.close();", r#"{ if (window.opener) window.opener.postMessage({kind:'kodeks-calendar-result', value:window.returnValue}, location.origin); window.close(); }"#);
    }
    html
}
