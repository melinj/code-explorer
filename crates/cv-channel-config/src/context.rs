//! Context evidence for config-key links (P5 refinement): where a key's parent keys and its file
//! family's name appear in the reading code, and how generic a key is across config families.

use std::collections::{HashMap, HashSet};

use rayon::prelude::*;

/// Lowercase identifier parts of a code file with the lines they occur on: whole identifiers and
/// their `snake_case` / `camelCase` parts.
pub struct Words {
    by_word: HashMap<String, Vec<u32>>,
}

fn push_parts(ident: &str, line: u32, out: &mut HashMap<String, Vec<u32>>) {
    let mut add = |w: String| {
        if w.len() >= 2 {
            let v = out.entry(w).or_default();
            if v.last() != Some(&line) {
                v.push(line);
            }
        }
    };
    add(ident.to_lowercase());
    let mut cur = String::new();
    let mut prev_lower = false;
    for ch in ident.chars() {
        if ch == '_' || ch == '-' {
            if !cur.is_empty() {
                add(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if ch.is_uppercase() && prev_lower && !cur.is_empty() {
            add(std::mem::take(&mut cur));
        }
        prev_lower = ch.is_lowercase() || ch.is_ascii_digit();
        cur.extend(ch.to_lowercase());
    }
    if !cur.is_empty() && cur.len() < ident.len() {
        add(cur);
    }
}

impl Words {
    /// Every word of the file (for document frequencies).
    pub fn words(&self) -> impl Iterator<Item = &str> {
        self.by_word.keys().map(String::as_str)
    }

    pub fn of(text: &str) -> Self {
        let mut by_word: HashMap<String, Vec<u32>> = HashMap::new();
        for (i, l) in text.lines().enumerate() {
            let line = i as u32 + 1;
            let mut start = None;
            for (j, ch) in l.char_indices() {
                let w = ch.is_alphanumeric() || ch == '_' || ch == '-';
                match (w, start) {
                    (true, None) => start = Some(j),
                    (false, Some(s)) => {
                        push_parts(&l[s..j], line, &mut by_word);
                        start = None;
                    }
                    _ => {}
                }
            }
            if let Some(s) = start {
                push_parts(&l[s..], line, &mut by_word);
            }
        }
        Words { by_word }
    }

    /// Distance in lines from `line` to the nearest occurrence of `word` (or its singular /
    /// plural form); `None` when the file never uses it.
    pub fn distance(&self, word: &str, line: u32) -> Option<u32> {
        let w = word.to_lowercase();
        let mut best: Option<u32> = None;
        for form in forms(&w) {
            if let Some(lines) = self.by_word.get(&form) {
                let i = lines.partition_point(|&l| l < line);
                for &l in lines[i.saturating_sub(1)..(i + 1).min(lines.len())].iter() {
                    let d = l.abs_diff(line);
                    best = Some(best.map_or(d, |b| b.min(d)));
                }
            }
        }
        best
    }
}

/// `word`, plus its singular or plural (`items` ↔ `item`, `entries` ↔ `entry`).
fn forms(w: &str) -> Vec<String> {
    let mut v = vec![w.to_string()];
    if let Some(s) = w.strip_suffix("ies") {
        v.push(format!("{s}y"));
    } else if let Some(s) = w.strip_suffix('s') {
        if s.len() >= 2 {
            v.push(s.to_string());
        }
    } else if let Some(s) = w.strip_suffix('y') {
        v.push(format!("{s}ies"));
        v.push(format!("{w}s"));
    } else {
        v.push(format!("{w}s"));
    }
    v
}

/// The word index of every code file (same order as `CodeIndex::code`).
pub fn words_of(texts: &[&str]) -> Vec<Words> {
    texts.par_iter().map(|t| Words::of(t)).collect()
}

/// Key spread: for each leaf key, the number of config families (file names) defining it.
pub fn family_spread<'a>(
    keys: impl Iterator<Item = (&'a str, &'a str)>,
) -> (HashMap<String, u32>, u32) {
    let mut fams: HashMap<&str, HashSet<&str>> = HashMap::new();
    let mut all: HashSet<&str> = HashSet::new();
    for (fam, leaf) in keys {
        fams.entry(leaf).or_default().insert(fam);
        all.insert(fam);
    }
    (
        fams.into_iter()
            .map(|(k, v)| (k.to_string(), v.len() as u32))
            .collect(),
        all.len() as u32,
    )
}

/// Bonus for a parent key named near the read site (node context).
pub const PARENT_BONUS: f64 = 0.15;
/// A parent key counts as context within this many lines of the read site…
pub const NEAR_LINES: u32 = 10;
/// …or within this many when the parent word is common (in more than [`COMMON_WORD`] of code files).
pub const NEAR_LINES_COMMON: u32 = 2;
pub const COMMON_WORD: f64 = 0.3;
/// A key defined in at least this share of config families is generic: it scores ×0.75 and links
/// only with node context.
pub const GENERIC_SPREAD: f64 = 0.1;
/// …and in at least this many (a small repo's few families make nothing generic).
pub const GENERIC_MIN_FAMILIES: f64 = 3.0;
pub const GENERIC_FACTOR: f64 = 0.75;

/// The family a config file counts in for key spread: its file name when several config files share
/// it (`node.yaml`, `props.yaml`), else its directory (`workflows/*.yaml`), so one-off names such as
/// `billing_workflow_v2.yaml` count as one family with their siblings.
pub fn spread_family(path: &str, shared_name: bool) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    if shared_name {
        return name.to_string();
    }
    let dir = cv_lang::dirname(path);
    let dir = dir.rsplit('/').next().unwrap_or(dir);
    let ext = name.rfind('.').map(|i| &name[i..]).unwrap_or("");
    format!("{dir}/*{ext}")
}

/// Context evidence over the code files of a [`crate::CodeIndex`].
pub struct Refine {
    words: Vec<Words>,
    /// Code files using each word.
    word_df: HashMap<String, u32>,
    common_df: f64,
    /// Leaf key → config families defining it.
    spread: HashMap<String, u32>,
    generic_at: f64,
}

impl Refine {
    /// `code_texts` in code-index order; `keys` = (config path, leaf key) of every key path.
    pub fn new<'a>(code_texts: &[&str], keys: impl Iterator<Item = (&'a str, &'a str)>) -> Self {
        let words = words_of(code_texts);
        let mut word_df: HashMap<String, u32> = HashMap::new();
        for w in &words {
            for x in w.words() {
                *word_df.entry(x.to_string()).or_default() += 1;
            }
        }
        let keys: Vec<(&str, &str)> = keys.collect();
        let mut names: HashMap<&str, HashSet<&str>> = HashMap::new();
        for &(path, _) in &keys {
            names
                .entry(path.rsplit('/').next().unwrap_or(path))
                .or_default()
                .insert(path);
        }
        let mut fam_of: HashMap<&str, String> = HashMap::new();
        for &(path, _) in &keys {
            fam_of.entry(path).or_insert_with(|| {
                let name = path.rsplit('/').next().unwrap_or(path);
                spread_family(path, names[name].len() >= 2)
            });
        }
        let (spread, nfam) = family_spread(keys.iter().map(|&(p, k)| (fam_of[p].as_str(), k)));
        Refine {
            words,
            word_df,
            common_df: COMMON_WORD * code_texts.len() as f64,
            spread,
            generic_at: (GENERIC_SPREAD * f64::from(nfam)).max(GENERIC_MIN_FAMILIES),
        }
    }

    /// Defined in many config families (`from`, `qty`, `words`): structure, not a setting.
    pub fn generic(&self, leaf: &str) -> bool {
        f64::from(self.spread.get(leaf).copied().unwrap_or(0)) >= self.generic_at
    }

    /// Is one of `parents` (nearest first) named near `line` of code file `ci`?
    pub fn near(&self, ci: usize, parents: &[&str], line: u32) -> bool {
        let w = &self.words[ci];
        parents.iter().any(|p| {
            let common = self
                .word_df
                .get(p.to_lowercase().as_str())
                .is_some_and(|&n| f64::from(n) > self.common_df);
            let reach = if common {
                NEAR_LINES_COMMON
            } else {
                NEAR_LINES
            };
            w.distance(p, line).is_some_and(|d| d <= reach)
        })
    }
}

/// The parent keys of a key path that can name a node in code: nearest first, at most 4, without
/// `*` and segments shorter than 3 characters. The bool says whether the key is top-level.
pub fn parents<'a>(path: &'a str, leaf: &str) -> (Vec<&'a str>, bool) {
    let all: Vec<&str> = path
        .strip_suffix(leaf)
        .unwrap_or("")
        .trim_end_matches('.')
        .rsplit('.')
        .filter(|s| !s.is_empty() && *s != "*")
        .collect();
    let top = all.is_empty();
    (
        all.into_iter().filter(|s| s.len() >= 3).take(4).collect(),
        top,
    )
}
