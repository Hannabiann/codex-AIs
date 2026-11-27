//! Browser corrections for the verified stock label-printing form.
use super::matches_source;
use std::borrow::Cow;

pub(super) fn adapt(source: &str) -> Cow<'_, str> {
    if !matches_source(
        source,
        (
            39344,
            [
                132, 107, 31, 0, 222, 234, 157, 156, 245, 194, 242, 83, 139, 149, 188, 96, 94, 233,
                50, 254, 221, 47, 253, 64, 110, 54, 122, 141, 30, 92, 28, 23,
            ],
        ),
    ) {
        return Cow::Borrowed(source);
    }
    Cow::Owned(source
        // The stock form rejects more than 50 matches. Read identities through the
        // indexes before reading/sorting the dates of every matching case.
        .replace("sQuery\t+=\t' ORDER BY", "var __stickerIds = mp_dosql('SELECT OID' + sQuery.substring(13)); if (!mp_isSqlResultEmpty(__stickerIds) && __stickerIds._getEstimatedSize() > 50) { this.Page.addHidden('error_msg', 'По вашему запросу найдено более 50 дел. Уточните запрос.'); this.Page.reloadToPagemode(PAGEMODE_QUERYFORFINDDOCS); return; } sQuery += ' ORDER BY")
        // This stock barcode routine expects one digit, not an array whose
        // length is that digit. Keep normal Array(length) semantics elsewhere.
        .replace("new Array(parseInt(\"\" + barCode.charAt(i), 10))", "[parseInt(\"\" + barCode.charAt(i), 10)]")
        .replace("if( listBlanks.Size > 0 ){", "if( listBlanks.Size == 0 ){ {/<option value=\"\">Нет доступных бланков наклейки</option>/} } if( listBlanks.Size > 0 ){")
        // The button calls checkField itself. Clicking it inside that function
        // recursively invokes the same handler and can submit more than once.
        .replacen("document.getElementById( \"ok\" ).click();", "", 1)
        .replace("onKeyUp=\"if( window.event.keyCode==13 ) checkField();\"",
            "onkeydown=\"if(event.key === 'Enter'){event.preventDefault();document.getElementById('ok').click();}\""))
}

#[cfg(test)]
mod tests {
    #[test]
    fn unknown_revision_is_unchanged() {
        let source = "document.getElementById( \"ok\" ).click();";
        assert_eq!(super::adapt(source), source);
    }
}
