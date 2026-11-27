//! Archive revision 73090: retain native forms, fields and movement history.
use super::matches_source;
use std::borrow::Cow;

pub(super) fn adapt(source: &str) -> Cow<'_, str> {
    if !matches_source(
        source,
        (
            343724,
            [
                145, 215, 101, 123, 254, 159, 14, 107, 237, 92, 238, 48, 58, 23, 173, 141, 147, 94,
                105, 195, 69, 85, 159, 132, 110, 153, 128, 104, 69, 241, 75, 199,
            ],
        ),
    ) {
        return Cow::Borrowed(source);
    }
    let mut result = source.replacen(
        "mp_executeMode(CArchiveMode);",
        "if (__codex_ArchiveLegacyEntry()) this.mode = 'arxgiveout';\nmp_executeMode(CArchiveMode);",
        1,
    );
    // Enable DB4 writes only on entry into these verified handlers. Form/menu
    // requests and unrelated archive operations keep their read-only host gate.
    for marker in [
        "function CArchiveMode_pagemodeWkGiveout () {",
        "function CArchiveMode_pagemodeWkReturn() {",
        "function CArchiveMode_pagemodeWkSaveDate () {",
    ] {
        result = result.replacen(marker, &format!(
            "{marker}\nif (!this.bfRegistrar) {{ this._noAccess('Выдача и возврат из архива'); return; }}\n__codex_ArchiveBeginWrite();"
        ), 1);
    }
    Cow::Owned(result)
}
