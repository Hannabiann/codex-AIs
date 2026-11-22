//! Corrections for exact stored sources that differ from working native code.
//! These affect only the interpreter input; source volumes are never modified.
use std::borrow::Cow;
#[path = "source_fingerprint.rs"]
mod fingerprint;

fn matches_source(source: &str, expected: (usize, [u8; 32])) -> bool {
    source.len() == expected.0 && fingerprint::sha256(source.as_bytes()) == expected.1
}
const DELETE_OBJECT_ID: (usize, [u8; 32]) = (
    32432,
    [
        51, 14, 242, 93, 197, 78, 123, 137, 186, 29, 199, 152, 131, 160, 229, 160, 161, 25, 163,
        177, 239, 176, 226, 103, 190, 145, 123, 247, 91, 135, 248, 192,
    ],
);
const DELETE_LINK_SCAN: &str = "    for (var i in struct) {  \n        var areanum = struct[i];  \n        for (var j in areanum) {  \n            var field = areanum[j];  \n            if ( field.Type == 5 && (flong == 1 || field.Link == area) ) {  \n";
const SELECTED_DELETE_LINKS: &str = "    var __codex_fields = __codex_LinkFields(struct, area, flong == 1);\n    for (var i = 0; i < __codex_fields.length; i++) {\n        var areanum = __codex_fields[i][0];\n        {\n            var field = __codex_fields[i][1];\n            if (true) {\n";

fn statistics_helpers(source: &str) -> Option<&str> {
    let start = source.find("function getJudOfSost(sost){")?;
    let candidate = source.get(start..start + STATISTICS_JUDGES_ID.0)?;
    matches_source(candidate, STATISTICS_JUDGES_ID).then_some(candidate)
}

const ISSUED_WRITS_ID: (usize, [u8; 32]) = (
    1188,
    [
        254, 61, 218, 15, 188, 223, 95, 125, 103, 97, 253, 54, 43, 114, 53, 248, 157, 180, 160, 20,
        156, 191, 57, 250, 105, 132, 177, 19, 148, 80, 230, 155,
    ],
);

const REGISTRY_LIBRARY_ID: (usize, [u8; 32]) = (
    74065,
    [
        170, 124, 26, 255, 64, 134, 92, 242, 93, 22, 200, 248, 150, 164, 250, 191, 151, 43, 102,
        58, 105, 33, 248, 216, 95, 193, 9, 115, 166, 108, 168, 117,
    ],
);

const DISTRIBUTION_ID: (usize, [u8; 32]) = (
    26907,
    [
        123, 73, 96, 120, 233, 0, 195, 163, 76, 140, 24, 182, 28, 21, 75, 82, 146, 225, 21, 37, 72,
        209, 53, 39, 86, 242, 159, 85, 164, 165, 131, 104,
    ],
);

const WEB_LIBRARY_ID: (usize, [u8; 32]) = (
    210220,
    [
        87, 30, 32, 99, 243, 251, 80, 181, 2, 25, 213, 169, 105, 113, 21, 226, 28, 110, 50, 143,
        226, 38, 29, 117, 164, 61, 5, 218, 9, 122, 24, 65,
    ],
);

const STATISTICS_JUDGES_ID: (usize, [u8; 32]) = (
    1705,
    [
        97, 24, 99, 187, 5, 29, 23, 46, 253, 137, 120, 111, 114, 157, 254, 130, 73, 68, 81, 184,
        68, 187, 88, 211, 242, 163, 229, 56, 248, 72, 236, 216,
    ],
);

const HISTORICAL_STATISTICS_JUDGES: &str = r#"
function getJudOfSost(sost){
    var judges=lStat_getJudgeOfStaff(sost,"1","no_act");
    return judges && judges.length ? judges : [1];
}
function getJudOfKol(kol){
    var judges=lStat_getJudgeOfKoll(kol,"1",null,true);
    return judges && judges.length ? judges : [1];
}
"#;

const DISTRIBUTION_CATEGORIES_ID: (usize, [u8; 32]) = (
    38567,
    [
        119, 200, 221, 48, 118, 239, 159, 248, 249, 200, 87, 27, 60, 206, 246, 46, 209, 100, 165,
        27, 136, 57, 3, 198, 33, 80, 227, 253, 91, 74, 233, 95,
    ],
);

const RESTORE_KP_ID: (usize, [u8; 32]) = (
    14302,
    [184, 24, 249, 122, 216, 101, 252, 140, 168, 206, 166, 55, 77, 160, 237, 127,
     250, 123, 68, 9, 41, 207, 158, 174, 109, 218, 110, 3, 137, 137, 116, 19],
);

pub fn request_source<'a>(name: &str, source: &'a str) -> Cow<'a, str> {
    if name == "restorekp" && matches_source(source, RESTORE_KP_ID) {
        return Cow::Owned(source.replace(
            "mp_dosql ( sGetJudOids )", "__codex_DistributionJudges( sGetJudOids )",
        ));
    }
    // Match the native category editor's historical classifier membership.
    if name == "raspredkategory" && matches_source(source, DISTRIBUTION_CATEGORIES_ID) {
        let mut updated = source
            .replace(
                "mp_dosql( sReqJudes )",
                "__codex_DistributionJudges( sReqJudes )",
            )
            .replace("mp_dosql (txtReq)", "__codex_DistributionJudges(txtReq)");
        for name in ["updateSostavsKategory", "createArrayOfCheckedKategory"] {
            if let Some(start) = updated.find(&format!("function {name}(")) {
                let end = updated[start + 1..]
                    .find("\nfunction ")
                    .map_or(updated.len(), |n| start + 1 + n);
                let function = updated[start..end].replace(
                    "mp_dosql ( sQuery_2 )",
                    "__codex_DistributionJudges( sQuery_2 )",
                );
                updated.replace_range(start..end, &function);
            }
        }
        return Cow::Owned(updated);
    }
    if name.eq_ignore_ascii_case("getdelo") && source.contains("SELECT * FROM \"Дело\"") {
        return Cow::Owned(source.replace(
            "SELECT * FROM \"Дело\"",
            r#"SELECT OID,"Предмет спора","Номер аж","Номер в ФАС","Номер в ВАС РФ","Наименование (1; Ndela)","Категория спора (13; Kspk5)","Коллегия" FROM "Дело""#,
        ));
    }
    // The Word client reads these six named columns. Keep the RTF as a DB4
    // stream so its binary contents never pass through a text conversion.
    if name.eq_ignore_ascii_case("getblank") && source.contains("SELECT * FROM \"Бланки\"") {
        return Cow::Owned(source.replace(
            "SELECT * FROM \"Бланки\"",
            "SELECT OID,\"Наименование\",\"Тип\",\"Номер\",\"Содержание\",\"Данные\" FROM \"Бланки\"",
        ).replace(
            "(new Document(parseInt(result.getInt('Тип')))).NumDoc",
            "((new Document(parseInt(result.getInt('Тип')))).NumDoc || 0)",
        ));
    }
    // Only select the same immutable link descriptors once per schema/area.
    // Keep the original SQL, recursive ordering, preview and write branches.
    if name == "DeleteObject" && matches_source(source, DELETE_OBJECT_ID) {
        return Cow::Owned(source.replacen(DELETE_LINK_SCAN, SELECTED_DELETE_LINKS, 1));
    }
    // Native 1АС includes former judges in staff/college filters. The stored
    // 1.12 source instead limits these helpers to the current work section.
    // Use the supplied libStatistic helpers' explicit historical mode, whose
    // complete judge sets and report cells match the native full-data controls.
    // Match the version and both exact helper bodies; never modify the volumes.
    if name == "StatOtchet221"
        && source.contains("version = \"Версия 1.12 от 22.07.2024\";")
        && statistics_helpers(source).is_some()
    {
        return Cow::Owned(source.replacen(
            statistics_helpers(source).unwrap(),
            HISTORICAL_STATISTICS_JUDGES,
            1,
        ));
    }
    // The working compiled script has no extra ')' in its third-instance arm.
    // As above, repair only this exact vendor source, never arbitrary syntax.
    if name == "particular_reg" && matches_source(source, DISTRIBUTION_ID) {
        return Cow::Owned(source.replacen(
            "mp_addItem ( aOutData, doc.Attr(\"Номер в ФАС\") ) );",
            "mp_addItem ( aOutData, doc.Attr(\"Номер в ФАС\") );",
            1,
        ));
    }
    Cow::Borrowed(source)
}

pub fn document_source(oid: u32, attribute: u16, source: &str) -> Cow<'_, str> {
    // Native integer division rounds this page count down. The source
    // interpreter uses floating-point numbers, so retain the explicit floor
    // in this vendor paginator (otherwise three rows show a spurious Next).
    if oid == 730099420 && attribute == 3 && matches_source(source, WEB_LIBRARY_ID) {
        return Cow::Owned(source.replacen(
            "(countelement+iElement-1)/iElement",
            "Math.floor((countelement+iElement-1)/iElement)",
            1,
        ));
    }
    // The native server executes the compiled attribute 9 of this registry
    // successfully (full-data control 2026-10-01). Its saved source puts the
    // closing brace of globalPreRequestFunc inside the trailing line comment.
    // Require the complete source to match, so a changed vendor script is not
    // silently rewritten. Only restore that function boundary.
    if oid == 705000262 && attribute == 9 && matches_source(source, ISSUED_WRITS_ID) {
        return Cow::Owned(source.replacen(
            "// ИЛ/ИЛ - дубликат }\n",
            "// ИЛ/ИЛ - дубликат\n}\n",
            1,
        ));
    }
    // Direct HTML is our portable output path; an empty header has no first
    // cell whose width could be normalized. Keep all nested registry work.
    if oid == 730099386 && attribute == 3 && matches_source(source, REGISTRY_LIBRARY_ID) {
        return Cow::Owned(source.replacen(
            "if (g_mode.isDirectHTML && (this.widthabsolute",
            "if (g_mode.isDirectHTML && this.arrayCells.length > 0 && (this.widthabsolute",
            1,
        ));
    }
    Cow::Borrowed(source)
}
