//! Adapt only the verified information-system script, leaving other List
//! condition modes and vendor revisions unchanged.
use super::matches_source;
use std::borrow::Cow;

pub(super) fn adapt(source: &str) -> Cow<'_, str> {
    if !matches_source(source, (56892, [81,56,214,223,63,37,156,246,132,38,16,173,240,12,105,130,52,62,53,46,2,187,53,56,48,139,225,81,216,217,166,172])) {
        return Cow::Borrowed(source);
    }
    Cow::Owned(source
        // The native string-mode 2 selects matching setting names here; the
        // generic List adapter implements mode 2 as inequality. Express these
        // known selections with supported prefix and exact-name conditions.
        .replace("AddCondition(1, \"Настройка справочной системы\", 2)", "AddCondition(1, \"Настройка справочной системы \", 4)")
        .replace("commentList.AddCondition(1, soderg, 2)", "commentList.AddCondition(1, \"Настройка справочной системы \" + soderg, 1)")
        // Removing membership compacts the list. Walk backwards while clearing
        // obsolete settings so no entry is skipped or read past the end.
        .replace("for(var j in commentList){", "for(var j = commentList.Size - 1; j >= 0; j--){")
        // Element zero is the category OID, not a configurable column.
        .replacen("for(var i in arr){", "for(var i in arr){ if (i == 0) continue;", 1)
        // Some configs contain a numeric zero for the default output format.
        // Native split() then fails while printing a nonempty search result.
        .replacen("if (\"\" + sISSetts != '')", "if (typeof(sISSetts) == 'string' && sISSetts != '')", 1)
        // A category without a parenthetical suffix keeps its entire name in
        // native KS; the generic JavaScript substring clamps a negative end.
        .replace("typed.substring(0, nLeftBrPos)", "(nLeftBrPos < 0 ? \"\" + typed : typed.substring(0, nLeftBrPos))"))
}

pub(super) fn movement(source: &str) -> Cow<'_, str> {
    if !matches_source(source, (50587, [105,128,122,96,221,166,159,218,211,213,193,21,161,16,13,42,156,156,220,136,73,49,1,109,72,184,134,240,101,247,23,73])) {
        return Cow::Borrowed(source);
    }
    // Native KS permits initialization before the first case label. Preserve
    // its execution order using the ordinary block grammar.
    Cow::Owned(source.replacen(
        "switch(action){ \n\t\tvar strVolumes = \"\"; \n\t\tvar attrVolumes = \"\"; \n\t\tvar mytempArr = new Array();",
        "var strVolumes = \"\"; var attrVolumes = \"\"; var mytempArr = new Array(); switch(action){",
        1,
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn changed_vendor_script_is_not_rewritten() {
        let text = "commentList.AddCondition(1, soderg, 2);";
        assert_eq!(super::adapt(text), text);
        assert_eq!(super::movement(text), text);
    }
}
