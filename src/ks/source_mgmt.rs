//! Local adapters for verified maintenance fragments (stored revision 65069).
use super::matches_source;
use std::borrow::Cow;

fn fragment<'a>(source: &'a str, marker: &str, size: usize, hash: [u8; 32]) -> Option<&'a str> {
    let start = source.find(marker)?;
    let text = source.get(start..start + size)?;
    matches_source(text, (size, hash)).then_some(text)
}

pub(super) fn adapt(source: &str) -> Cow<'_, str> {
    let mut result = Cow::Borrowed(source);
    if let Some(body) = fragment(
        source,
        "var COpChangeNumBlankIL",
        208,
        [
            243, 18, 180, 125, 44, 54, 201, 68, 54, 137, 18, 245, 36, 151, 111, 161, 249, 199, 192,
            99, 28, 78, 192, 30, 53, 208, 248, 81, 250, 33, 35, 226,
        ],
    ) {
        result = Cow::Owned(result.replacen(
            body,
            &body.replacen("initialize: \"\"", "initialize: null", 1),
            1,
        ));
    }
    if let Some(body) = fragment(
        source,
        "// создание kod-файлов с полученными данными",
        3708,
        [
            210, 137, 7, 186, 177, 18, 234, 243, 94, 218, 191, 243, 123, 74, 251, 70, 184, 46, 188,
            250, 228, 43, 174, 243, 36, 195, 113, 254, 215, 10, 194, 24,
        ],
    ) {
        result = Cow::Owned(result.replacen(body, r#"
            var groups = [sqlrDoc, sqlrFile, sqlrLetter, sqlrSide, sqlrStatCard, sqlrParticipant, sqlrAdress];
            var objects = [];
            for (var gi = 0; gi < groups.length; gi++) {
                var docs = [];
                var rows = groups[gi];
                if (rows != null) while (rows.next()) docs[docs.length] = rows.getObject(1);
                objects[objects.length] = docs;
            }
            var archive = __codex_MgmtExport(dCase, objects[0], objects[1], objects[2], objects[3], objects[4], objects[5], objects[6]);
            {/Выгрузка дела выполнена успешно. Архив на сервере: <b> %archive </b><br/>Архив сохранён локально./}
"#, 1));
    }
    if let Some(body) = fragment(
        source,
        "// разархивируем папку с файлами",
        2427,
        [
            171, 180, 128, 204, 133, 163, 145, 157, 168, 206, 203, 149, 243, 149, 178, 28, 1, 37,
            153, 106, 20, 216, 164, 60, 204, 150, 143, 41, 242, 209, 157, 208,
        ],
    ) {
        result = Cow::Owned(result.replacen(
            body,
            r#"
            var result = __codex_MgmtImport(sdir, regdelo_getCurrentTempPath());
            {/ %result /}
            log.print(1, null, "Загрузка KOD-пакета завершена успешно");
"#,
            1,
        ));
    }
    // TestRead validates complete records. The native partial-record recovery
    // and Repair/reindex algorithms are not implemented: do not report a repair.
    if let Some(body) = fragment(
        source,
        "var COpCheckDBObjects",
        7575,
        [
            147, 229, 169, 209, 147, 152, 139, 212, 80, 142, 89, 208, 49, 37, 109, 31, 48, 177, 74,
            156, 115, 33, 235, 254, 83, 212, 145, 53, 75, 38, 216, 10,
        ],
    ) {
        let fixed = body.replacen("function(){", "function(){ if (trecov == '1') throw 'Восстановление повреждённых записей средствами Repair пока не поддерживается. Доступна проверка чтения.';", 1)
            .replace("id=\"try_recover\"", "id=\"try_recover\" disabled title=\"Доступна проверка чтения; Repair пока не реализован\"")
            .replace("Попытаться исправить некорректные документы ", "Попытаться исправить некорректные документы (пока не поддерживается; доступна проверка чтения) ")
            .replace("недоступен для чтения (будет удален при переиндексировании)", "недоступен для чтения; данные не изменялись");
        result = Cow::Owned(result.replacen(body, &fixed, 1));
    }
    result
}
