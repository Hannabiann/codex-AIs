//! Natural-language query terms and transparent relevance scoring.
//! Word families come from the original kodeks.spl dictionary.
//! Integer normalization follows the native engine; index scoring is in word_index/context.
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default)]
pub struct ResultData {
    pub ranks: BTreeMap<u32, u32>,
    pub statistics: Vec<(String, u64)>,
}

/// Native 00830550 / 0082fa30: integer min/max scaling, lower bound 1.
pub fn normalize(ranks: &mut BTreeMap<u32, u32>) {
    let minimum = ranks.values().copied().min().unwrap_or(0);
    let maximum = ranks.values().copied().max().unwrap_or(0);
    for rank in ranks.values_mut() {
        let mut range = maximum - minimum;
        let mut delta = *rank - minimum;
        if range == 0 {
            *rank = 100;
            continue;
        }
        while range > 0xffffe {
            range >>= 1;
            delta >>= 1;
        }
        *rank = (delta * 100 / range).max(1);
    }
}

pub fn terms(text: &str) -> Result<Vec<String>, String> {
    if text.len() > 8192 {
        return Err("FREETEXT query size limit".into());
    }
    let mut terms = BTreeMap::new();
    for word in super::text_query::words(text) {
        let (bytes, _, bad) = encoding_rs::WINDOWS_1251.encode(&word);
        if bad || bytes.len() > 32 {
            return Err("FREETEXT word encoding/length".into());
        }
        if [
            "И", "В", "ВО", "НА", "С", "СО", "К", "КО", "О", "ОБ", "ОТ", "ПО", "ДО", "ИЗ", "ЗА",
            "ДЛЯ", "НЕ", "А", "НО", "ИЛИ",
        ]
        .contains(&word.as_str())
        {
            continue;
        }
        terms.entry(word).or_insert(());
        if terms.len() > 64 {
            return Err("FREETEXT word count limit".into());
        }
    }
    if terms.is_empty() {
        return Err("FREETEXT requires searchable words".into());
    }
    Ok(terms.into_keys().collect())
}
