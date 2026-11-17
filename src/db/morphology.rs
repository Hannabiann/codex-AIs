//! Read-only Max System Fast Dictionary (kodeks.spl 666.1).
//! Layout: native 007ad2c0/007ad390; codec: 005dea40 and 005dd600.
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};
#[derive(Default)]
pub struct Dictionary {
    groups: Vec<String>,
    words: HashMap<String, Vec<usize>>,
}
impl Dictionary {
    pub fn parse(data: &[u8]) -> Result<Self, String> {
        if data.len() < 256 || !data.starts_with(b"666.1 Max System Fast Dictionary.") {
            return Err("unsupported kodeks.spl header".into());
        }
        let u16at = |p| u16::from_le_bytes(data[p..p + 2].try_into().unwrap()) as usize;
        let n = u16at(250);
        let at = u32::from_le_bytes(data[252..256].try_into().unwrap()) as usize;
        if n == 0 || at < 256 || at.checked_add(n * 10) != Some(data.len()) {
            return Err("invalid kodeks.spl directory".into());
        }
        let mut out = Self::default();
        let mut budget = 0;
        for p in (at..data.len()).step_by(10) {
            let packed = u16at(p);
            let size = u16at(p + 2);
            let off = u32::from_le_bytes(data[p + 4..p + 8].try_into().unwrap()) as usize;
            if u16at(p + 8) == 0 {
                continue;
            }
            if packed == 0
                || size == 0
                || packed > size
                || off < 256
                || off.checked_add(packed).map_or(true, |e| e > at)
            {
                return Err("invalid kodeks.spl block".into());
            }
            budget += size;
            if budget > 64 * 1024 * 1024 {
                return Err("kodeks.spl decoded size limit".into());
            }
            let bytes = &data[off..off + packed];
            let plain = if packed == size {
                bytes.to_vec()
            } else {
                inflate(bytes, size)?
            };
            if plain.last() != Some(&0) {
                return Err("unterminated kodeks.spl word family".into());
            }
            for g in super::decode(&plain).split('\0').filter(|s| !s.is_empty()) {
                let group = g.replace('Ё', "Е");
                let id = out.groups.len();
                for word in group.split_whitespace() {
                    if word.len() > 128 {
                        return Err("kodeks.spl word length limit".into());
                    }
                    out.words.entry(word.to_owned()).or_default().push(id);
                }
                out.groups.push(group);
            }
        }
        Ok(out)
    }
    pub fn forms(&self, word: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::from([word.to_owned()]);
        if let Some(ids) = self.words.get(word) {
            for id in ids {
                out.extend(self.groups[*id].split_whitespace().map(str::to_owned));
            }
        } else {
            fallback(word, &mut out);
        }
        out
    }
}
/// One immutable dictionary per data root, shared by request workers.
pub fn load(root: &Path) -> Result<Arc<Dictionary>, String> {
    type Cache = HashMap<PathBuf, Arc<Dictionary>>;
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let path = root.join("kodeks.spl");
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "morphology cache poisoned")?;
    if let Some(d) = cache.get(&path) {
        return Ok(d.clone());
    }
    let d = match std::fs::metadata(&path) {
        Ok(m) => {
            if m.len() > 16 * 1024 * 1024 {
                return Err("kodeks.spl size limit".into());
            }
            Dictionary::parse(&std::fs::read(&path).map_err(|e| e.to_string())?)?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Dictionary::default(),
        Err(e) => return Err(format!("kodeks.spl: {e}")),
    };
    let d = Arc::new(d);
    if cache.len() >= 8 {
        cache.clear();
    }
    cache.insert(path, d.clone());
    Ok(d)
}
/// Prefix partitions keep dictionary families with alternating stems indexed.
pub fn prefixes(forms: &BTreeSet<String>) -> Vec<String> {
    let mut groups: std::collections::BTreeMap<char, Vec<&str>> = Default::default();
    for s in forms {
        if let Some(c) = s.chars().next() {
            groups.entry(c).or_default().push(s);
        }
    }
    groups
        .values()
        .map(|g| {
            let mut p = g[0].to_owned();
            for s in &g[1..] {
                while !s.starts_with(&p) {
                    p.pop();
                }
            }
            p
        })
        .collect()
}
fn fallback(word: &str, out: &mut BTreeSet<String>) {
    if !word.chars().all(|c| ('А'..='Я').contains(&c)) {
        return;
    }
    for (bare, ends) in PARADIGMS {
        let longest = ends
            .iter()
            .filter(|s| word.ends_with(**s))
            .max_by_key(|s| s.len());
        let base = match longest {
            Some(s) => &word[..word.len() - s.len()],
            None if *bare => word,
            None => continue,
        };
        if base.chars().count() < 2 {
            continue;
        }
        if *bare {
            out.insert(base.to_owned());
        }
        out.extend(ends.iter().map(|s| format!("{base}{s}")));
    }
}
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}
impl Bits<'_> {
    fn read(&mut self, n: usize) -> Result<usize, String> {
        if self.pos + n > self.data.len() * 8 {
            return Err("truncated SPL compressed bits".into());
        }
        let mut v = 0;
        for i in 0..n {
            v |= (((self.data[self.pos / 8] >> (self.pos % 8)) & 1) as usize) << i;
            self.pos += 1;
        }
        Ok(v)
    }
}
struct Model {
    weights: [usize; 257],
    symbols: [usize; 257],
    sum: usize,
    seen: usize,
    limit: usize,
}
impl Model {
    fn new(limit: usize) -> Self {
        Self {
            weights: [1; 257],
            symbols: std::array::from_fn(|i| i),
            sum: 257,
            seen: 0,
            limit,
        }
    }
    fn decode(&mut self, bits: &mut Bits<'_>) -> Result<usize, String> {
        let mut lo = 0;
        let mut sum = self.sum;
        for _ in 0..32 {
            let mut left = 0;
            let mut split = lo;
            while left < sum / 2 {
                left += *self.weights.get(split).ok_or("invalid SPL code tree")?;
                split += 1;
            }
            if bits.read(1)? != 0 {
                lo = split;
                sum -= left;
            } else {
                sum = left;
            }
            if *self.weights.get(lo).ok_or("invalid SPL symbol")? >= sum {
                let value = self.symbols[lo];
                if self.seen < self.limit {
                    self.sum += 32;
                    if self.sum >= 65502 {
                        self.weights.iter_mut().for_each(|w| *w = (*w + 1) / 2);
                        self.sum = self.weights.iter().sum::<usize>() + 32;
                    }
                    let weight = self.weights[lo] + 32;
                    let mut j = lo;
                    while j > 0 && self.weights[j - 1] < weight {
                        self.weights[j] = self.weights[j - 1];
                        self.symbols[j] = self.symbols[j - 1];
                        j -= 1;
                    }
                    self.weights[j] = weight;
                    self.symbols[j] = value;
                }
                self.seen += 1;
                return Ok(value);
            }
        }
        Err("SPL code depth limit".into())
    }
}
fn inflate(data: &[u8], size: usize) -> Result<Vec<u8>, String> {
    if data.first().map(|b| b & 7) == Some(4) {
        return super::fields::decompress_mode4(data, size).map_err(|e| e.to_string());
    }
    let mode = data
        .first()
        .map(|b| b & 7)
        .ok_or("empty SPL compressed block")?;
    if !matches!(mode, 6 | 7) {
        return Err(format!("unsupported SPL compression mode {mode}"));
    }
    let mut bits = Bits { data, pos: 3 };
    let mut length = Model::new(3000);
    let mut distance = Model::new(5000);
    let mut literal = Model::new(3000);
    let mut out = Vec::with_capacity(size);
    while out.len() < size {
        let n = length.decode(&mut bits)? + 2;
        if n >= 258 {
            break;
        }
        if n == 257 {
            let c = if mode == 7 {
                literal.decode(&mut bits)?
            } else {
                bits.read(8)?
            };
            if c > 255 {
                return Err("invalid SPL literal".into());
            }
            out.push(c as u8);
            continue;
        }
        let high = if n > 2 {
            distance.decode(&mut bits)?
        } else {
            0
        };
        let d = (high << 8) + bits.read(8)?;
        if d == 0 || d > out.len() {
            return Err("invalid SPL back-reference".into());
        }
        for _ in 0..n.min(size - out.len()) {
            out.push(out[out.len() - d]);
        }
    }
    if out.len() != size {
        return Err("SPL unpacked length mismatch".into());
    }
    Ok(out)
}
// Native unknown-word fallback paradigms: 007daa70/007dab30, table 00b33ef8.
const PARADIGMS: &[(bool, &[&str])] = &[
    (
        true,
        &[
            "А", "У", "ОЙ", "Е", "Ы", "АМИ", "АХ", "АМ", "НЫЙ", "НЫМ", "НОМ", "НОГО", "НАЯ", "НУЮ",
            "НОЙ", "НЫЕ", "НЫХ", "НЫМИ",
        ],
    ),
    (false, &["А", "И", "Е", "У", "ЕЮ", "АМ", "ЕЙ", "АМИ", "АХ"]),
    (false, &["А", "Ы", "Е", "У", "ЕЮ", "АМ", "ЕЙ", "АМИ", "АХ"]),
    (false, &["А", "И", "Е", "У", "ОЮ", "АМ", "ОЙ", "АМИ", "АХ"]),
    (false, &["Я", "И", "Е", "Ю", "ЕЙ", "ЕЮ", "ЯМ", "ЯМИ", "ЯХ"]),
    (
        false,
        &[
            "ИЯ",
            "ИИ",
            "ИЮ",
            "ИЕЙ",
            "ИЙ",
            "ИЯМ",
            "ИЯМИ",
            "ИЯХ",
            "ИЙНАЯ",
            "ИЙНОГО",
            "ИЙНОЕ",
            "ИЙНОЙ",
            "ИЙНОМ",
            "ИЙНОМУ",
            "ИЙНУЮ",
            "ИЙНЫЕ",
            "ИЙНЫЙ",
            "ИЙНО",
            "ИЙНЫМ",
            "ИЙНЫИ",
            "ИЙНЫХ",
            "ИЙНЫМИ",
        ],
    ),
    (
        false,
        &["ИЙ", "ИЯ", "ИЮ", "ИЕМ", "ИИ", "ИЕВ", "ИЯМ", "ИЯМИ", "ИЯХ"],
    ),
    (
        false,
        &["Ь", "Я", "Ю", "ЕМ", "Е", "И", "ЯМ", "ЕЙ", "ЯМИ", "ЯХ"],
    ),
    (
        true,
        &[
            "А",
            "ОМ",
            "Е",
            "У",
            "Ы",
            "ОВ",
            "АМ",
            "АМИ",
            "АХ",
            "НЫЙ",
            "НОГО",
            "НЫМ",
            "НОМ",
            "НОГО",
            "НОМУ",
            "НАЯ",
            "НУЮ",
            "НОЙ",
            "НЫЕ",
            "НЫХ",
            "НЫМИ",
            "НЫМ",
            "НОЕ",
            "ОВАЯ",
            "ОВОЙ",
            "ОВЫЙ",
            "ОВЫМ",
            "ОВОМ",
            "ОВОЕ",
            "ОВЫЕ",
            "ИРОВАЛО",
            "ИРОВАНИЕ",
            "ИРОВАН",
            "ИРОВАНИЕМ",
            "ИРОВАНИИ",
            "ИРОВАНИЯ",
            "ИРОВАННОГО",
            "ИРОВАННЫЕ",
            "ИРОВАТЬ",
            "ИРУЮТСЯ",
            "ИРУЮТ",
            "ИРУЕТ",
            "ИРУЮТСЯ",
            "ИРУЕТСЯ",
        ],
    ),
    (
        true,
        &[
            "А",
            "ЕМ",
            "Е",
            "У",
            "Ы",
            "ЕВ",
            "АМ",
            "АМИ",
            "АХ",
            "НЫЙ",
            "НОГО",
            "НЫМ",
            "НОМ",
            "НОГО",
            "НОМУ",
            "НАЯ",
            "НУЮ",
            "НОЙ",
            "НЫЕ",
            "НЫХ",
            "НЫМИ",
            "НЫМ",
            "НОЕ",
            "ЕВАЯ",
            "ЕВОЙ",
            "ЕВЫЙ",
            "ЕВЫМ",
            "ЕВОМ",
            "ЕВОЕ",
            "ЕВЫЕ",
            "ИРОВАЛО",
            "ИРОВАНИЕ",
            "ИРОВАН",
            "ИРОВАНИЕМ",
            "ИРОВАНИИ",
            "ИРОВАНИЯ",
            "ИРОВАННОГО",
            "ИРОВАННЫЕ",
            "ИРОВАТЬ",
            "ИРУЮТСЯ",
            "ИРУЮТ",
            "ИРУЕТ",
            "ИРУЮТСЯ",
            "ИРУЕТСЯ",
        ],
    ),
    (
        true,
        &[
            "А",
            "ОМ",
            "Е",
            "У",
            "И",
            "ОВ",
            "АМ",
            "АМИ",
            "АХ",
            "ОВАЯ",
            "ОВОЙ",
            "ОВЫЙ",
            "ОВЫМ",
            "ОВОМ",
            "ОВОЕ",
            "ОВЫЕ",
            "ОВОГО",
            "ОВОМУ",
            "ОВУЮ",
            "ОВЫМИ",
            "ОВЫХ",
        ],
    ),
    (
        false,
        &[
            "К",
            "КОМ",
            "КЕ",
            "КУ",
            "КА",
            "КИ",
            "КАХ",
            "КАМ",
            "КОВ",
            "ЧНЫЙ",
            "ЧНОГО",
            "ЧНЫМ",
            "ЧНОМ",
            "ЧНОГО",
            "ЧНАЯ",
            "ЧНУЮ",
            "ЧНОЙ",
            "ЧНОЕ",
            "ЧНЫЕ",
            "ЧНЫМИ",
            "ЧНЫХ",
        ],
    ),
    (
        false,
        &["О", "А", "У", "ОМ", "Е", "АМ", "АМИ", "АХ", "И", "ОВ"],
    ),
    (
        false,
        &[
            "О",
            "А",
            "У",
            "ОМ",
            "Е",
            "ЬЯ",
            "ЬЯМ",
            "ЬЯМИ",
            "ЯХ",
            "ЕВ",
            "ЬЕВОЙ",
            "ЬЕВОМУ",
            "ЬЕВОГО",
            "ЬЕВЫМ",
            "ЬЕВОМ",
            "ЬЕВАЯ",
            "ЬЕВОЙ",
            "ЬЕВЫЕ",
            "ЬЕВЫМ",
            "ЬЕВЫХ",
            "ЬЕВЫМИ",
            "ЬЕВ",
            "ЬЯХ",
        ],
    ),
    (false, &["Е", "Я", "Ю", "Е", "ЕЙ", "ЯМ", "ЯМИ", "ЯХ", "Й"]),
    (true, &["Е", "А", "ЕМ", "АМ", "АМИ", "АХ"]),
    (
        true,
        &[
            "ИЕ", "ИЯ", "ИЮ", "ИИ", "ИЕМ", "ИЯМ", "ИЯХ", "ИЯМИ", "НЫЕ", "НЫХ", "НЫМ", "НЫХ",
            "НЫМИ", "НАЯ", "НУЮ", "НОЙ", "НЫЙ", "НОГО", "НОМУ", "НОМ", "НЫМ", "НЫ", "А", "О", "Ы",
        ],
    ),
    (false, &["Ь", "И", "ЬЮ", "ЕЙ", "ЯМ", "ЬМИ", "ЯХ"]),
    (false, &["Ь", "И", "ЬЮ", "ЕЙ", "ЯМ", "ЯМИ", "ЯХ"]),
    (false, &["Ь", "И", "ЬЮ", "ЕЙ", "АМ", "АМИ", "АХ"]),
    (false, &["ОЙ", "ОГО", "ОМУ", "ЫМ", "ОМ"]),
    (false, &["ЫЙ", "ОГО", "ОМУ", "ЫМ", "ОМ", "ЫЕ", "ЫХ", "ЫМИ"]),
    (false, &["ЕГО", "ЕМУ", "ИЙ", "ИМ", "ЕМ", "ИЕ", "ИХ", "ИМИ"]),
    (false, &["ОЕ", "ОГО", "ОМУ", "ЫМ", "ОМ", "ЫЕ", "ЫХ", "ЫМИ"]),
    (false, &["ЕЕ", "ЕГО", "ЕМУ", "ИМ", "ЕМ", "ИЕ", "ИХ", "ИМИ"]),
    (
        false,
        &[
            "АЯ", "ОЙ", "УЮ", "ИЙ", "ОГО", "ОМУ", "ИМ", "ОМ", "ИЕ", "ИХ", "ИЕ", "ИМИ", "ОЕ",
        ],
    ),
    (
        false,
        &[
            "АЯ", "ОЙ", "УЮ", "ЫЙ", "ОГО", "ОМУ", "ЫМ", "ОМ", "ЫЕ", "ЫХ", "ЫМИ", "ОЕ",
        ],
    ),
    (false, &["ЯЯ", "ЕЙ", "ЮЮ", "ОЙ", "ИЕ", "ИХ", "ИМИ"]),
    (
        false,
        &[
            "ОВ",
            "ОВА",
            "ОВУ",
            "ОВЫМ",
            "ОВОМ",
            "ОВЫ",
            "ОВЫХ",
            "ОВЫМ",
            "ОВЫМИ",
            "ОВЫХ",
        ],
    ),
    (
        false,
        &[
            "ОВО",
            "ОВА",
            "ОВУ",
            "ОВЫМ",
            "ОВОМ",
            "ОВЫ",
            "ОВЫХ",
            "ОВЫМ",
            "ОВЫМИ",
        ],
    ),
    (
        false,
        &[
            "ОВАЯ",
            "ОВОЙ",
            "ОВУЮ",
            "ОВЫМ",
            "ОВОМ",
            "ОВЫЕ",
            "ОВЫХ",
            "ОВЫМ",
            "ОВЫМИ",
            "ОВЫХ",
        ],
    ),
    (
        false,
        &[
            "ИН",
            "ИНОГО",
            "ИНОМУ",
            "ИНЫМ",
            "ИНОМ",
            "ИНЫ",
            "ИНЫХ",
            "ИНЫМ",
            "ИНЫМИ",
        ],
    ),
    (
        false,
        &[
            "ИНО",
            "ИНОГО",
            "ИНОМУ",
            "ИНЫМ",
            "ИНОМ",
            "ИНЫ",
            "ИНЫХ",
            "ИНЫМ",
            "ИНЫМИ",
        ],
    ),
    (
        false,
        &["ИНА", "ИНОЙ", "ИНУ", "ИНЫ", "ИНЫХ", "ИНЫМ", "ИНЫМИ"],
    ),
    (
        false,
        &[
            "ИТЬ", "Ю", "ИШЬ", "ИТ", "ИМ", "ИТЕ", "ЯТ", "ИЛ", "ИЛА", "ИЛО",
        ],
    ),
    (
        false,
        &[
            "ИТЬ",
            "ИТ",
            "Ю",
            "ЯТ",
            "ИМ",
            "ИТЕ",
            "ИЛ",
            "ИЛА",
            "ИЛИ",
            "ИЛО",
            "ИТСЯ",
            "ИТЬСЯ",
            "ИЛСЯ",
            "ИЛАСЬ",
            "ИЛОСЬ",
        ],
    ),
    (
        false,
        &[
            "ТЬ", "Ю", "ЕШЬ", "ЕТ", "ЕМ", "ЕТЕ", "ЮТ", "Л", "ЛА", "ЛО", "ЛИ", "ЛИСЬ", "ЛОСЬ",
            "ЛСЯ", "ЕТСЯ", "ТЬСЯ", "ЮТСЯ", "АЛ", "АЛА", "АЛО", "АЛИ", "Я",
        ],
    ),
    (false, &["ЕТ", "ТЬ"]),
    (
        false,
        &[
            "НЯ",
            "НЮ",
            "НЕ",
            "НЕЙ",
            "НИ",
            "ЕН",
            "НЯХ",
            "НЯМ",
            "НЯМИ",
            "ЕННЫЙ",
            "ЕННОГО",
            "ЕННОМ",
            "ЕННОЙ",
            "ЕННОЕ",
            "ЕННЫМ",
            "ЕННЫМИ",
        ],
    ),
];
