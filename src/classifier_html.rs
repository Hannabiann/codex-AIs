//! Preserve native classifier/calendar forms while replacing their obsolete popup
//! unload handshake. The initial about:blank unload listener is discarded
//! during navigation in modern browsers. Results use same-origin postMessage.
pub fn adapt(html: String, object: &str) -> String {
    let html = if matches!(object, "makeVacation" | "newJudAndHoliday") {
        // The native helper replaces an existing hidden input by default.
        // Each removed vacation row must append its OID instead.
        regex::Regex::new(r#"js_addFormHidden\(\s*"nm_deldoc"\s*,\s*numdoc\s*\);"#)
            .unwrap()
            .replace_all(&html, "js_addFormHidden(\"nm_deldoc\",numdoc,false);")
            .into_owned()
    } else {
        html
    };
    let html = if object == "regassessors" {
        // Native row numbers are also submitted field suffixes. Reusing one
        // after deleting a preceding row loses dates and produces duplicates.
        // Keep the counter monotonic and remove a saved row only after the
        // server confirms deletion, so an AJAX error cannot look successful.
        regex::Regex::new(r#"(?s)\$\('\.delvac'\)\.live\('click',\s*function\(\)\{.*?\n\s*\}\);"#)
            .unwrap().replace(&html, |_: &regex::Captures| r#"
            $('.delvac').live('click', function(){
                var button=$(this), oid=button.attr('oid');
                var row=$('#id__regularrow_' + button.attr('id').replace('id__delvac_', ''));
                function removed(){
                    row.remove(); js_redrawZebra(sRegularTrClassName);
                    if (!$('.' + sRegularTrClassName).length) {
                        $('.' + sHaederTrClassName).remove(); bRowHaeder=false;
                    }
                }
                if (!oid) { removed(); return; }
                if (button.attr('disabled')) return;
                button.attr('disabled', 'disabled'); $('#id__save').attr('disabled','disabled');
                $.ajax({url:sURL, type:'GET', data:{mode:5,oid:oid},
                    success:removed,
                    error:function(){alert('Не удалось удалить период. Обновите страницу и повторите попытку.');},
                    complete:function(){
                        button.removeAttr('disabled');
                        if (!$('.delvac[disabled]').length) $('#id__save').removeAttr('disabled');
                    }
                });
            });
"#.to_owned()).into_owned()
    } else {
        html
    };
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
            if object == "ispravlenieForm" {
                let editor_calendar = c[0].contains("callbackDialog(childWindow.returnValue, oName, inputInnerId, oSelect, oType)");
                let editor_classifier = c[0].contains("callbackDialogClassif(childWindow.returnValue, oName, oInnerId, oSelect, childWindow.dialogArguments)");
                if editor_calendar || editor_classifier {
                    let (kind, call, extra) = if editor_calendar {
                        ("calendar", "callbackDialog(event.data.value, name, input, select, args);", "inputInnerId, oSelect, oType")
                    } else {
                        ("classifier", "args[2] = event.data.changed ? 1 : 0; callbackDialogClassif(event.data.value, name, input, select, args);", "oInnerId, oSelect, vFeatures")
                    };
                    return format!(r#"
                    (function(child, name, input, select, args) {{
                        function receive(event) {{
                            if(event.origin !== location.origin || event.source !== child ||
                                !event.data || event.data.kind !== 'kodeks-{kind}-result') return;
                            window.removeEventListener('message', receive); clearInterval(cleanup);
                            {call}
                        }}
                        window.addEventListener('message', receive);
                        var cleanup=setInterval(function() {{ if(child.closed) {{
                            clearInterval(cleanup); setTimeout(function() {{
                                window.removeEventListener('message', receive);
                            }}, 1000);
                        }} }}, 1000);
                    }})(childWindow, oName, {extra});
                    "#);
                }
            }
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
    crate::distribution_html::adapt(html, object)
}
