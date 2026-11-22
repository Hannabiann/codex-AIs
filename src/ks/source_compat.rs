//! Compatibility fixes and guarded corrections for verified stored KS sources.
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
    [
        184, 24, 249, 122, 216, 101, 252, 140, 168, 206, 166, 55, 77, 160, 237, 127, 250, 123, 68,
        9, 41, 207, 158, 174, 109, 218, 110, 3, 137, 137, 116, 19,
    ],
);

const USER_REGISTRATION_ID: (usize, [u8; 32]) = (
    104945,
    [
        68, 121, 250, 23, 13, 91, 230, 89, 128, 32, 7, 19, 87, 94, 74, 74, 123, 73, 230, 35, 82,
        24, 47, 38, 167, 174, 236, 21, 18, 71, 173, 110,
    ],
);

// The stored registration script checks the AIS user area, but /users can
// create a login in UserBase alone. Check both before any of the three writes.
// On edit, identify the existing account through the stored employee link;
// the submitted old-login field is not evidence of ownership.
const USERBASE_LOGIN_CHECK: &str = r#"
    if (i_errLog == 0) {
        var __codex_logins = new List("Пользователи", UserBase);
        __codex_logins.AddCondition(UserBase.Struct["Пользователи"]["UserID"].Attr, a_userData[1], 1);
        var __codex_own = null;
        if (i_action == 3 && parseInt(oid_numdoc, 10) > 0) {
            var __codex_link = mp_dosql('SELECT "Наименование" FROM "Пользователи" WHERE "Действующее лицо"=' + parseInt(oid_numdoc, 10));
            if (__codex_link != null && __codex_link.absolute(0)) {
                var __codex_previous = new List("Пользователи", UserBase);
                __codex_previous.AddCondition(UserBase.Struct["Пользователи"]["UserID"].Attr, __codex_link.getString(1), 1);
                if (__codex_previous.Size == 1) {
                    for (var __codex_i in __codex_previous) __codex_own = __codex_previous[__codex_i].NumDoc;
                }
            }
        }
        for (var __codex_i in __codex_logins) {
            if (__codex_logins[__codex_i].NumDoc != __codex_own) { i_errLog = 1; break; }
        }
    }
"#;

pub fn request_source<'a>(name: &str, source: &'a str) -> Cow<'a, str> {
    // Stored anshlag 52571 contains a semicolon before this function's body.
    // Its native bytecode executes that body; correct only this verified source.
    if name == "anshlag" && matches_source(source, (106160, [153, 94, 29, 88, 8, 129, 155, 92, 167, 92, 210, 153, 32, 250, 168, 7, 123, 138, 177, 18, 168, 178, 121, 113, 98, 45, 97, 19, 49, 60, 121, 109])) {
        return Cow::Owned(source
            .replacen("function xml_docforAnshlag();{", "function xml_docforAnshlag(){", 1)
            // This native comparison orders case links by their displayed names,
            // not by OID. The later stable time sort keeps that order for ties.
            .replacen("var aCase\t=\ta.Attr", "var aCase = '' + a.Attr", 1)
            .replacen("var bCase\t=\tb.Attr", "var bCase = '' + b.Attr", 1));
    }

    // The native KS integer quotient is the whole-page count. Floating-point
    // division otherwise generates fractional page links near the last page.
    if name == "createNewObject"
        && matches_source(
            source,
            (
                22664,
                [
                    90, 193, 165, 193, 167, 30, 81, 235, 166, 199, 164, 51, 144, 228, 157, 217,
                    225, 40, 118, 228, 2, 212, 74, 229, 136, 139, 222, 1, 28, 239, 125, 41,
                ],
            ),
        )
    {
        return Cow::Owned(
            source
                .replace(
                    "num/parseInt(nelem,10)",
                    "Math.floor(num/parseInt(nelem,10))",
                )
                .replacen(
                    "mp_dosql (req)",
                    "__codex_PAreaPage(Base, req, count, parseInt(nelem,10))",
                    1,
                ),
        );
    }
    if name == "ispravlenieForm"
        && matches_source(
            source,
            (
                74011,
                [
                    213, 105, 224, 28, 49, 130, 140, 81, 225, 225, 126, 58, 121, 174, 230, 183,
                    239, 90, 36, 121, 21, 193, 9, 220, 147, 44, 61, 80, 33, 154, 245, 124,
                ],
            ),
        )
    {
        let start = source.find("\t\t\tbfIsForm = nDoc.Area == 1002").unwrap();
        let end = start
            + source[start..].find("\t\t\tnDoc.Unlock();").unwrap()
            + "\t\t\tnDoc.Unlock();".len();
        let mut fixed = source.to_owned();
        fixed.replace_range(
            start..end,
            r###"			if (!g_bfSuperAccess && !checkDisplayArea(nDoc.Area)) {
                ErrorMess('Редактирование данного объекта ЗАПРЕЩЕНО!');
                return;
            }
            __codex_ImportKod(nDoc, writeVol);"###,
        );
        fixed = fixed.replacen(
            "var bfNewDoc\t=\t\"\"+deepnumber==\"\";",
            r#"var bfNewDoc = ""+deepnumber=="";
    if (bkod != 1 && !bfNewDoc && !__codex_PEditCheck(parseInt(deepnumber,10), codex_p_version)) {
        __codexPConflict();
        return;
    }"#, 1);
        fixed = fixed.replacen("var nDoc = mp_tryCreateDocByOid(prevdoc);",
            "var nDoc = mp_tryCreateDocByOid(prevdoc);\n            var __codex_p_version = nDoc == null ? \"\" : __codex_PEditVersion(nDoc);", 1);
        fixed = fixed.replacen("<INPUT TYPE=\"HIDDEN\" NAME=\"deepnumber\"",
            "<INPUT TYPE=\"HIDDEN\" NAME=\"codex_p_version\" VALUE=\" %__codex_p_version \">\n            <INPUT TYPE=\"HIDDEN\" NAME=\"deepnumber\"", 1);
        fixed = fixed.replacen("var value = this['a' + deffield.Attr];", r#"if (codex_p_merge == '1' && this['codex_pick_a' + deffield.Attr] != 'draft') continue;
                var value = this['a' + deffield.Attr];
                if (codex_p_merge == '1' && this['codex_absent_a' + deffield.Attr] == '1') value = null;"#, 1);
        fixed = fixed.replacen("switch(x.Code){", "if (x.codexPConflict) { __codexPConflict(); return; }\n        switch(x.Code){", 1);
        fixed.push_str(P_CONFLICT_HELPER);
        return Cow::Owned(fixed);
    }
    if name == "createnewuser" && matches_source(source, USER_REGISTRATION_ID) {
        return Cow::Owned(source.replacen(
            "\tif (i_errLog == 1) {",
            &format!("{USERBASE_LOGIN_CHECK}\tif (i_errLog == 1) {{"),
            1,
        ));
    }
    if name == "restorekp" && matches_source(source, RESTORE_KP_ID) {
        return Cow::Owned(source.replace(
            "mp_dosql ( sGetJudOids )",
            "__codex_DistributionJudges( sGetJudOids )",
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

// Serialize comparison cells with the same date/number rules as the native form.
// Strings are passed unescaped; the Rust renderer escapes every HTML boundary.
const P_CONFLICT_HELPER: &str = r#"
function __codexPConflict() {
    var doc = __codex_PEditPrepare(parseInt(deepnumber,10));
    var rows = [];
    if (doc != null) {
        var defs = Request.PathObject.Base.Struct[doc.Area];
        for (var i in defs) {
            var field = defs[i];
            if (field.Type != 1 && field.Type != 2 && field.Type != 3 && field.Type != 5 && field.Type != 12 && field.Type != 13) continue;
            var values = [];
            var names = [];
            for (var value = doc.Attr(field.Attr,false); value != null; value = doc.Next(value,false)) {
                values.push(field.Type == 2 ? value.Value : getValue(value,field.Type));
                names.push(getNiceValue(value,field.Type));
            }
            rows.push([field.Attr, field.Name, field.Type, values.join(';'), names.join(';'), values.length > 0]);
        }
    }
    __codex_PEditConflict(parseInt(deepnumber,10), rows);
}
"#;
