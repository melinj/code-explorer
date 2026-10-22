//! P5 config-fields channel pack (DESIGN §4.6, D15): config keys (YAML, TOML, JSON) → one hub per
//! (file family, key) → code that reads them, scored heuristically. `docs/analysis/config_fields.py`
//! is the reference implementation; this pack reproduces it:
//!
//! - read sites are scanned once per code file and indexed by token (`get("k")`/`["k"]` access 0.55,
//!   bare string 0.35, model field 0.35, attribute 0.20);
//! - score = base + up to 0.30 for rarity (IDF over code files) + 0.30 same directory / 0.15
//!   sibling + 0.30 when the code mentions the config's path / 0.25 its file name; generic keys
//!   score half; links ≥ 0.65 are kept (≥ 0.80 solid), at most 5 readers per key.
//!
//! That is [`Scoring::Reference`] (`CV_CONFIG_SCORING=reference`, used by the parity harness). The
//! default, [`Scoring::Context`], deliberately diverges from the reference to raise plausibility
//! (64% → 82% on hand-labelled samples, WORKSTREAMS P5): it adds node context and key genericity
//! from [`context`]. A parent key of the config key (`yields` in `yields.rules.*.qty`) named
//! within a few lines of the read site adds 0.15; a key defined in ≥ 10% of config families
//! (`from`, `qty`, `words`) scores ×0.75; and a link needs context: a parent key near the site, or
//! (for keys that are not generic) a same or sibling directory, or a loader mention of a top-level
//! key.

use std::collections::{HashMap, HashSet};

use aho_corasick::{AhoCorasick, MatchKind};
use cv_channel::{
    ChannelKey, ChannelLink, ChannelPack, EndpointFact, HubOut, KeyExpr, LinkContext, Mention,
    Output, Project, Role, RuleSet, Target,
};
use rayon::prelude::*;
use regex::Regex;

pub mod context;

pub const MIN_SCORE: f64 = 0.65;
pub const SOLID_SCORE: f64 = 0.80;
pub const MAX_READERS: usize = 5;

const LOCK: [&str; 5] = [
    "package-lock.json",
    "uv.lock",
    "pnpm-lock.yaml",
    "yarn.lock",
    "poetry.lock",
];
const SKIP_DIRS: [&str; 6] = ["node_modules", "docs", "tests", "test", "__tests__", "e2e"];

/// Generic keys score half.
pub const STOP: [&str; 32] = [
    "name",
    "type",
    "id",
    "description",
    "version",
    "value",
    "data",
    "path",
    "key",
    "enabled",
    "default",
    "items",
    "title",
    "label",
    "kind",
    "mode",
    "model",
    "source",
    "target",
    "url",
    "port",
    "host",
    "text",
    "content",
    "config",
    "params",
    "inputs",
    "outputs",
    "optional",
    "required",
    "properties",
    "format",
];

fn name_of(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

fn dirname(p: &str) -> &str {
    cv_lang::dirname(p)
}

/// Config files the pack reads keys from (YAML, TOML, JSON outside tests, docs and lockfiles).
pub fn is_config(path: &str) -> bool {
    let lower = path.to_lowercase();
    let parts: Vec<&str> = lower.split('/').collect();
    let name = parts[parts.len() - 1];
    if LOCK.contains(&name)
        || parts[..parts.len() - 1]
            .iter()
            .any(|p| SKIP_DIRS.contains(p))
        || parts.contains(&".github")
    {
        return false;
    }
    [".yaml", ".yml", ".toml", ".json"]
        .iter()
        .any(|e| name.ends_with(e))
        && !name.starts_with("tsconfig")
        && !name.starts_with("jsconfig")
}

/// Code files scanned for read sites (tests excluded).
pub fn is_code(path: &str) -> bool {
    let name = name_of(path);
    let parts: Vec<&str> = path.split('/').collect();
    if parts[..parts.len() - 1]
        .iter()
        .any(|p| SKIP_DIRS.contains(p))
        || name.starts_with("test_")
        || name.ends_with("_test.py")
        || name == "conftest.py"
    {
        return false;
    }
    [".py", ".ts", ".tsx", ".js", ".jsx"]
        .iter()
        .any(|e| name.ends_with(e))
        && !name.contains(".test.")
        && !name.contains(".spec.")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ReadKind {
    Access,
    String,
    Attr,
    Field,
}

impl ReadKind {
    pub fn base(self) -> f64 {
        match self {
            ReadKind::Access => 0.55,
            ReadKind::String | ReadKind::Field => 0.35,
            ReadKind::Attr => 0.2,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            ReadKind::Access => "access",
            ReadKind::String => "string",
            ReadKind::Attr => "attr",
            ReadKind::Field => "field",
        }
    }
}

struct Scanners {
    res: [(ReadKind, Regex); 4],
}

impl Scanners {
    fn new() -> Self {
        let r = |s: &str| Regex::new(s).expect("read-site regex");
        Scanners {
            res: [
                (
                    ReadKind::Access,
                    r(
                        r#"(?:\[\s*|\bget\w*\(\s*|\w*[cC]onfig\w*\(\s*|\bparams?\w*\(\s*)["']([A-Za-z_][\w-]{2,})["']"#,
                    ),
                ),
                (ReadKind::String, r(r#"["']([A-Za-z_][\w-]{2,})["']"#)),
                (ReadKind::Attr, r(r"\.([A-Za-z_]\w{2,})\b")),
                (
                    ReadKind::Field,
                    r(r#"(?m)^\s+([a-z_]\w{2,})\s*:\s*[A-Za-z"']"#),
                ),
            ],
        }
    }

    /// (token, line, kind) in the reference's order: by regex, then by position.
    fn scan(&self, text: &str) -> Vec<(String, u32, ReadKind)> {
        let starts: Vec<usize> = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        let line = |at: usize| starts.partition_point(|&s| s <= at) as u32;
        let mut out = Vec::new();
        for (kind, re) in &self.res {
            for c in re.captures_iter(text) {
                let (Some(m), Some(t)) = (c.get(0), c.get(1)) else {
                    continue;
                };
                out.push((t.as_str().to_string(), line(m.start()), *kind));
            }
        }
        out
    }
}

/// The code side: read sites by token, document frequencies, and which config files each code
/// file mentions.
pub struct CodeIndex {
    /// Project file index of each code file, sorted by path.
    pub code: Vec<usize>,
    sites: HashMap<String, Vec<(u32, u32, ReadKind)>>,
    df: HashMap<String, u32>,
    /// Per code file: mentioned config file paths / names (ids into the maps below).
    ment_path: Vec<HashSet<u32>>,
    ment_name: Vec<HashSet<u32>>,
    cfg_path_id: HashMap<String, u32>,
    cfg_name_id: HashMap<String, u32>,
    code_pos: HashMap<usize, u32>,
}

impl CodeIndex {
    pub fn build(project: &Project, configs: &[usize]) -> Self {
        let mut code: Vec<usize> = (0..project.files.len())
            .filter(|&i| is_code(project.files[i].path))
            .collect();
        code.sort_by_key(|&i| project.files[i].path);
        let sc = Scanners::new();
        let scanned: Vec<Vec<(String, u32, ReadKind)>> = code
            .par_iter()
            .map(|&i| sc.scan(project.files[i].text))
            .collect();
        let mut sites: HashMap<String, Vec<(u32, u32, ReadKind)>> = HashMap::new();
        let mut df: HashMap<String, u32> = HashMap::new();
        for (ci, s) in scanned.into_iter().enumerate() {
            let mut seen: HashSet<&str> = HashSet::new();
            for (tok, line, kind) in &s {
                if seen.insert(tok.as_str()) {
                    *df.entry(tok.clone()).or_default() += 1;
                }
                sites
                    .entry(tok.clone())
                    .or_default()
                    .push((ci as u32, *line, *kind));
            }
        }
        let mut cfg_path_id = HashMap::new();
        let mut cfg_name_id = HashMap::new();
        let mut pats: Vec<String> = Vec::new();
        for &c in configs {
            let p = project.files[c].path;
            if !cfg_path_id.contains_key(p) {
                cfg_path_id.insert(p.to_string(), pats.len() as u32);
                pats.push(p.to_string());
            }
        }
        let n_paths = pats.len();
        for &c in configs {
            let n = name_of(project.files[c].path);
            if !cfg_name_id.contains_key(n) {
                cfg_name_id.insert(n.to_string(), (pats.len() - n_paths) as u32);
                pats.push(n.to_string());
            }
        }
        let ac = AhoCorasick::builder()
            .match_kind(MatchKind::Standard)
            .build(&pats)
            .expect("mention automaton");
        let (ment_path, ment_name): (Vec<HashSet<u32>>, Vec<HashSet<u32>>) = code
            .par_iter()
            .map(|&i| {
                let (mut a, mut b) = (HashSet::new(), HashSet::new());
                for m in ac.find_overlapping_iter(project.files[i].text) {
                    let id = m.pattern().as_usize();
                    if id < n_paths {
                        a.insert(id as u32);
                    } else {
                        b.insert((id - n_paths) as u32);
                    }
                }
                (a, b)
            })
            .unzip();
        let code_pos = code
            .iter()
            .enumerate()
            .map(|(i, &f)| (f, i as u32))
            .collect();
        CodeIndex {
            code,
            sites,
            df,
            ment_path,
            ment_name,
            cfg_path_id,
            cfg_name_id,
            code_pos,
        }
    }

    pub fn n(&self) -> usize {
        self.code.len()
    }

    fn idf_term(&self, token: &str) -> f64 {
        let n = self.n() as f64;
        let idf = ((n + 1.0) / (*self.df.get(token).unwrap_or(&0) as f64 + 1.0)).ln();
        0.3 * (idf / (n + 1.0).ln()).min(1.0)
    }

    fn mention(&self, code_ix: u32, cfg_path: &str) -> Mention {
        let ci = code_ix as usize;
        if self
            .cfg_path_id
            .get(cfg_path)
            .is_some_and(|id| self.ment_path[ci].contains(id))
        {
            Mention::Path
        } else if self
            .cfg_name_id
            .get(name_of(cfg_path))
            .is_some_and(|id| self.ment_name[ci].contains(id))
        {
            Mention::FileName
        } else {
            Mention::None
        }
    }
}

fn dir_bonus(code: &str, cfg: &str) -> f64 {
    let (cdir, kdir) = (dirname(code), dirname(cfg));
    if cdir == kdir {
        0.3
    } else if dirname(cdir) == dirname(kdir) {
        0.15
    } else {
        0.0
    }
}

fn mention_bonus(m: Mention) -> f64 {
    match m {
        Mention::Path => 0.3,
        Mention::FileName => 0.25,
        Mention::None => 0.0,
    }
}

fn is_stop(key: &str) -> bool {
    let k = key.to_lowercase();
    STOP.contains(&k.as_str())
}

/// The reference score of one read site for one config key.
pub fn score(kind: ReadKind, key: &str, idf_term: f64, dir: f64, mention: f64) -> f64 {
    let mut s = kind.base();
    s += idf_term;
    s += dir;
    s += mention;
    if is_stop(key) {
        s *= 0.5;
    }
    s.min(1.0)
}

/// One config key → read site link, as the reference reports it.
#[derive(Debug, Clone)]
pub struct DirectLink {
    pub score: f64,
    pub cfg: usize,
    pub path: String,
    pub cfg_line: u32,
    pub code: usize,
    pub code_line: u32,
    pub kind: ReadKind,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    pub config_files: usize,
    pub config_keys: usize,
    pub code_files: usize,
    pub links: usize,
    pub keys_with_a_reader: usize,
    pub files_with_a_linked_key: usize,
    /// By [`ReadKind`] order: access, string, attr, field.
    pub link_kinds: [usize; 4],
    pub family_hubs: usize,
    pub edges_via_hubs: usize,
}

impl std::fmt::Display for Stats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "config: {} config files, {} key paths, {} code files; {} links; {} keys with a reader ({:.1}%); {} files with a linked key; access {} · string {} · field {} · attribute {}; {} family hubs, {} edges via hubs",
            self.config_files,
            self.config_keys,
            self.code_files,
            self.links,
            self.keys_with_a_reader,
            100.0 * self.keys_with_a_reader as f64 / self.config_keys.max(1) as f64,
            self.files_with_a_linked_key,
            self.link_kinds[0],
            self.link_kinds[1],
            self.link_kinds[3],
            self.link_kinds[2],
            self.family_hubs,
            self.edges_via_hubs
        )
    }
}

/// How links are scored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scoring {
    /// Exactly `docs/analysis/config_fields.py` (the parity check in
    /// `scripts/acceptance/config_fields.py`; `CV_CONFIG_SCORING=reference`).
    Reference,
    /// The default: the reference score plus node context and key genericity
    /// ([`context::Refine`]): a parent key named within a few lines of the read site adds
    /// [`context::PARENT_BONUS`]; keys defined in many config families score ×0.75; and a link needs
    /// context (a parent key near the site, a same or sibling directory, or a loader mention of a
    /// top-level key; generic keys only the first).
    Context,
}

impl Scoring {
    /// `CV_CONFIG_SCORING=reference` selects [`Scoring::Reference`].
    pub fn from_env() -> Self {
        match std::env::var("CV_CONFIG_SCORING").as_deref() {
            Ok("reference") => Scoring::Reference,
            _ => Scoring::Context,
        }
    }
}

/// [`analyze_with`] under the default [`Scoring::Context`].
pub fn analyze(project: &Project, min: f64) -> (Vec<DirectLink>, Stats) {
    analyze_with(project, min, Scoring::Context)
}

/// Every config key's readers (≤ 5, score ≥ `min`); with [`Scoring::Reference`] exactly as
/// `config_fields.py` computes them.
pub fn analyze_with(project: &Project, min: f64, scoring: Scoring) -> (Vec<DirectLink>, Stats) {
    let configs: Vec<usize> = (0..project.files.len())
        .filter(|&i| {
            let f = &project.files[i];
            is_config(f.path)
                && !f.text.is_empty()
                && (f.path.ends_with(".yaml")
                    || f.path.ends_with(".yml")
                    || f.config_keys.iter().any(|k| !k.item))
        })
        .collect();
    let idx = CodeIndex::build(project, &configs);
    let refine = (scoring == Scoring::Context).then(|| {
        let texts: Vec<&str> = idx.code.iter().map(|&i| project.files[i].text).collect();
        context::Refine::new(
            &texts,
            configs.iter().flat_map(|&c| {
                let path = project.files[c].path;
                project.files[c]
                    .config_keys
                    .iter()
                    .filter(|k| !k.item)
                    .map(move |k| (path, k.key.as_str()))
            }),
        )
    });
    let links: Vec<DirectLink> = configs
        .par_iter()
        .flat_map_iter(|&cfg| {
            let cfg_path = project.files[cfg].path;
            let mut out = Vec::new();
            for fact in project.files[cfg].config_keys.iter().filter(|k| !k.item) {
                let key = fact.key.as_str();
                let Some(list) = idx.sites.get(key) else {
                    continue;
                };
                let idf_t = idx.idf_term(key);
                let stop = is_stop(key);
                let (parents, top) = context::parents(&fact.path, key);
                let generic = refine.as_ref().is_some_and(|r| r.generic(key));
                let factor = if stop {
                    0.5
                } else if generic {
                    context::GENERIC_FACTOR
                } else {
                    1.0
                };
                let bonus = if refine.is_some() {
                    context::PARENT_BONUS
                } else {
                    0.0
                };
                if (0.55 + idf_t + 0.6 + bonus) * factor < min - 1e-9 {
                    continue;
                }
                let mut best: Vec<((u32, u32), f64, ReadKind)> = Vec::new();
                let mut at: HashMap<(u32, u32), usize> = HashMap::new();
                let mut i = 0;
                while i < list.len() {
                    let ci = list[i].0;
                    let mut j = i;
                    while j < list.len() && list[j].0 == ci {
                        j += 1;
                    }
                    let code_path = project.files[idx.code[ci as usize]].path;
                    let dir = dir_bonus(code_path, cfg_path);
                    let ment = mention_bonus(idx.mention(ci, cfg_path));
                    if (0.55 + idf_t + dir + ment + bonus) * factor >= min - 1e-9 {
                        for &(_, line, kind) in &list[i..j] {
                            let sc = match &refine {
                                None => score(kind, key, idf_t, dir, ment),
                                Some(r) => {
                                    let near = r.near(ci as usize, &parents, line);
                                    let context =
                                        near || (!generic && (dir > 0.0 || (top && ment > 0.0)));
                                    if !context {
                                        continue;
                                    }
                                    let b = if near { bonus } else { 0.0 };
                                    ((kind.base() + idf_t + dir + ment + b) * factor).min(1.0)
                                }
                            };
                            if sc < min {
                                continue;
                            }
                            match at.get(&(ci, line)) {
                                Some(&b) => {
                                    if sc > best[b].1 {
                                        best[b].1 = sc;
                                        best[b].2 = kind;
                                    }
                                }
                                None => {
                                    at.insert((ci, line), best.len());
                                    best.push(((ci, line), sc, kind));
                                }
                            }
                        }
                    }
                    i = j;
                }
                best.sort_by(|a, b| b.1.total_cmp(&a.1));
                for ((ci, line), sc, kind) in best.into_iter().take(MAX_READERS) {
                    out.push(DirectLink {
                        score: sc,
                        cfg,
                        path: fact.path.clone(),
                        cfg_line: fact.line,
                        code: idx.code[ci as usize],
                        code_line: line,
                        kind,
                    });
                }
            }
            out
        })
        .collect();
    let mut st = Stats {
        config_files: configs.len(),
        config_keys: configs
            .iter()
            .map(|&c| {
                project.files[c]
                    .config_keys
                    .iter()
                    .filter(|k| !k.item)
                    .count()
            })
            .sum(),
        code_files: idx.n(),
        links: links.len(),
        ..Default::default()
    };
    let mut keys: HashSet<(usize, &str)> = HashSet::new();
    let mut files: HashSet<usize> = HashSet::new();
    type Hub = (HashSet<usize>, HashSet<(usize, u32)>);
    let mut hubs: HashMap<(&str, &str), Hub> = HashMap::new();
    for l in &links {
        keys.insert((l.cfg, l.path.as_str()));
        files.insert(l.cfg);
        st.link_kinds[l.kind as usize] += 1;
        let h = hubs
            .entry(family_key(project.files[l.cfg].path, &l.path))
            .or_default();
        h.0.insert(l.cfg);
        h.1.insert((l.code, l.code_line));
    }
    st.keys_with_a_reader = keys.len();
    st.files_with_a_linked_key = files.len();
    st.family_hubs = hubs.len();
    st.edges_via_hubs = hubs.values().map(|(d, r)| d.len() + r.len()).sum();
    (links, st)
}

/// Hub key: (file family, leaf key), e.g. ("node.yaml", "capacity").
fn family_key<'a>(cfg: &'a str, path: &'a str) -> (&'a str, &'a str) {
    (name_of(cfg), path.rsplit('.').next().unwrap_or(path))
}

pub struct ConfigFieldsPack {
    rules: RuleSet,
    pub min_score: f64,
    pub scoring: Scoring,
}

impl ConfigFieldsPack {
    pub fn new() -> Self {
        ConfigFieldsPack {
            rules: RuleSet::default(),
            min_score: MIN_SCORE,
            scoring: Scoring::from_env(),
        }
    }
}

impl Default for ConfigFieldsPack {
    fn default() -> Self {
        Self::new()
    }
}

impl ChannelPack for ConfigFieldsPack {
    fn protocol(&self) -> &'static str {
        "config"
    }
    fn rules(&self) -> &RuleSet {
        &self.rules
    }
    fn rules_mut(&mut self) -> &mut RuleSet {
        &mut self.rules
    }
    fn sides(&self) -> (&'static [Role], &'static [Role]) {
        (&[Role::Define], &[Role::Read])
    }
    fn normalize(&self, key: &KeyExpr, _role: Role) -> ChannelKey {
        match key.as_lit() {
            Some(k) => ChannelKey::Known(k.rsplit('.').next().unwrap_or(k).to_string()),
            None => ChannelKey::Dynamic { hint: key.render() },
        }
    }
    fn matches(&self, producer: &ChannelKey, consumer: &ChannelKey) -> bool {
        producer == consumer
    }
    fn hub_label(&self, key: &ChannelKey) -> String {
        key.text().to_string()
    }
    fn link_kind(&self, _producer: &EndpointFact, _consumer: &EndpointFact) -> &'static str {
        "read"
    }
    fn stub_kind(&self, _role: Role) -> &'static str {
        "read"
    }
    /// Producer: a config key (`rule` = the config file's path); consumer: a read site (`rule` =
    /// the read kind, `handler` = the code file's path).
    fn affinity(
        &self,
        producer: &EndpointFact,
        consumer: &EndpointFact,
        ctx: &dyn LinkContext,
    ) -> Option<f32> {
        let key = producer.key.as_lit()?;
        let leaf = key.rsplit('.').next().unwrap_or(key);
        if consumer.key.as_lit()? != leaf {
            return None;
        }
        let kind = match consumer.rule.as_str() {
            "access" => ReadKind::Access,
            "string" => ReadKind::String,
            "field" => ReadKind::Field,
            _ => ReadKind::Attr,
        };
        let (cfg, code) = (producer.rule.as_str(), consumer.handler.as_deref()?);
        let dir = match ctx.dir_distance(code, cfg) {
            0 => 0.3,
            1 => 0.15,
            _ => 0.0,
        };
        let s = score(
            kind,
            leaf,
            0.3 * f64::from(ctx.idf(leaf)).min(1.0),
            dir,
            mention_bonus(ctx.mentions(code, cfg)),
        );
        (s >= self.min_score).then_some(s as f32)
    }
    fn link_all(&self, project: &Project, _endpoints: &[EndpointFact]) -> Option<Output> {
        let (links, st) = analyze_with(project, self.min_score, self.scoring);
        let mut out = Output::default();
        let mut hub_ix: HashMap<(&str, &str), usize> = HashMap::new();
        type Sites = (HashMap<usize, u32>, HashSet<(usize, u32)>);
        let mut sites: Vec<Sites> = Vec::new();
        for l in &links {
            let (fam, leaf) = family_key(project.files[l.cfg].path, &l.path);
            let hi = *hub_ix.entry((fam, leaf)).or_insert_with(|| {
                out.hubs.push(HubOut {
                    id: format!("config:{fam}#{leaf}"),
                    protocol: "config",
                    key: leaf.to_string(),
                    family: Some(fam.to_string()),
                    display: format!("{fam}#{leaf}"),
                    producers: vec![],
                    consumers: vec![],
                });
                sites.push(Default::default());
                out.hubs.len() - 1
            });
            let e = sites[hi].0.entry(l.cfg).or_insert(l.cfg_line);
            *e = (*e).min(l.cfg_line);
            sites[hi].1.insert((l.code, l.code_line));
            out.links.push(ChannelLink {
                protocol: "config",
                kind: "read",
                from_file: l.cfg,
                from_line: l.cfg_line,
                to: Target::Site {
                    file: l.code,
                    line: l.code_line,
                },
                confidence: l.score as f32,
                label: out.hubs[hi].display.clone(),
                hub: Some(hi),
                rule: l.kind.as_str().to_string(),
            });
        }
        for (h, (p, c)) in out.hubs.iter_mut().zip(sites) {
            let mut p: Vec<(usize, u32)> = p.into_iter().collect();
            p.sort_unstable();
            let mut c: Vec<(usize, u32)> = c.into_iter().collect();
            c.sort_unstable();
            h.producers = p;
            h.consumers = c;
        }
        out.notes.push(st.to_string());
        Some(out)
    }
}

/// [`LinkContext`] over a built [`CodeIndex`] (for callers scoring single pairs).
pub struct IndexContext<'a> {
    pub index: &'a CodeIndex,
    pub project: &'a Project<'a>,
}

impl LinkContext for IndexContext<'_> {
    fn idf(&self, token: &str) -> f32 {
        (self.index.idf_term(token) / 0.3) as f32
    }
    fn dir_distance(&self, a: &str, b: &str) -> u32 {
        match dir_bonus(a, b) {
            x if x >= 0.3 => 0,
            x if x > 0.0 => 1,
            _ => 2,
        }
    }
    fn mentions(&self, code: &str, config: &str) -> Mention {
        self.project
            .file(code)
            .and_then(|f| self.index.code_pos.get(&f))
            .map(|&ci| self.index.mention(ci, config))
            .unwrap_or(Mention::None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cv_channel::{ConfigKeyFact, FileIn};

    /// DESIGN §4.6: links ≥ SOLID_SCORE are solid, MIN_SCORE..SOLID_SCORE faded with "?". The
    /// canvas draws that from its own copy of the threshold (web/src/canvas/edges.ts); keep them
    /// equal.
    #[test]
    fn the_canvas_fades_config_links_below_solid_score() {
        let ts = include_str!("../../../web/src/canvas/edges.ts");
        let line = ts
            .lines()
            .find(|l| l.starts_with("export const CONFIG_SOLID_SCORE = "))
            .expect("edges.ts exports CONFIG_SOLID_SCORE");
        let v: f64 = line
            .trim_start_matches("export const CONFIG_SOLID_SCORE = ")
            .trim_end_matches(';')
            .parse()
            .unwrap();
        assert_eq!(v, SOLID_SCORE);
        const { assert!(MIN_SCORE < SOLID_SCORE) };
    }

    fn key(path: &str, line: u32) -> ConfigKeyFact {
        ConfigKeyFact {
            path: path.into(),
            key: path.rsplit('.').next().unwrap().into(),
            line,
            value: None,
            item: false,
        }
    }

    fn file<'a>(path: &'a str, text: &'a str, keys: &'a [ConfigKeyFact]) -> FileIn<'a> {
        FileIn {
            path,
            lane: "code",
            lang: None,
            conv: Default::default(),
            text,
            config_keys: keys,
            imports: vec![],
            symbols: vec![],
        }
    }

    #[test]
    fn scores_follow_the_reference() {
        let node_keys = [key("config", 1), key("config.capacity", 2), key("name", 3)];
        let surf_keys = [key("tone", 1), key("tone.temperature", 2)];
        let files = vec![
            file(
                "nodes/a/node.yaml",
                "config:\n  capacity: 3\nname: a\n",
                &node_keys,
            ),
            file(
                "nodes/a/node.py",
                "cfg = load('node.yaml')\ncap = item.get(\"capacity\")\nn = d[\"name\"]\n",
                &[],
            ),
            file("ui/surfaces.yaml", "tone:\n  temperature: 1\n", &surf_keys),
            file("ui/surfaces.py", "t = tone.get(\"temperature\")\n", &[]),
            file("other/x.py", "y.capacity\n", &[]),
            file("tests/test_a.py", "z[\"capacity\"]\n", &[]),
        ];
        let project = Project::new(&files);
        let (links, st) = analyze_with(&project, MIN_SCORE, Scoring::Reference);
        let got: Vec<(&str, &str, u32, &str)> = links
            .iter()
            .map(|l| {
                (
                    project.files[l.cfg].path,
                    l.path.as_str(),
                    l.code_line,
                    l.kind.as_str(),
                )
            })
            .collect();
        assert!(got.contains(&("nodes/a/node.yaml", "config.capacity", 2, "access")));
        assert!(got.contains(&("ui/surfaces.yaml", "tone.temperature", 1, "access")));
        // `name` is generic: halved below the threshold.
        assert!(!got.iter().any(|g| g.1 == "name"));
        assert_eq!(st.code_files, 3);
        assert_eq!(st.config_files, 2);
        assert!((score(ReadKind::Access, "capacity", 0.3, 0.3, 0.25) - 1.0).abs() < 1e-9);
        assert!((score(ReadKind::Access, "name", 0.3, 0.3, 0.0) - 0.575).abs() < 1e-9);
        let mut pack = ConfigFieldsPack::new();
        pack.scoring = Scoring::Reference;
        let out = pack.link_all(&project, &[]).unwrap();
        assert!(out.hubs.iter().any(|h| h.display == "node.yaml#capacity"));
        assert!(
            out.links
                .iter()
                .all(|l| l.kind == "read" && l.confidence >= 0.65)
        );
    }

    /// Context scoring: a read site with the key's parent named nearby links, the same key read
    /// in unrelated code without it does not, and a generic key needs the parent even in a sibling
    /// directory.
    #[test]
    fn context_requires_node_context() {
        let rec_keys = [
            key("recipes", 1),
            key("recipes.bread", 2),
            key("recipes.bread.ingredients", 3),
        ];
        let q = |p: &str, l| {
            let mut k = key(p, l);
            k.key = "qty".into();
            k
        };
        let gen_keys: Vec<Vec<ConfigKeyFact>> = (0..10)
            .map(|i| vec![q(&format!("section{i}.qty"), 1)])
            .collect();
        let gen_paths: Vec<String> = (0..10).map(|i| format!("d{i}/f{i}.yaml")).collect();
        let mut files = vec![
            file(
                "world/recipes.yaml",
                "recipes:\n  bread:\n    ingredients: [flour]\n",
                &rec_keys,
            ),
            // Reads `ingredients` right under a `recipes` lookup: node context.
            file(
                "studio/compile.py",
                "doc = load('recipes.yaml')\nfor r in doc.get(\"recipes\").values():\n    x = r.get(\"ingredients\")\n",
                &[],
            ),
            // Loads the file but reads a same-named key far from any recipe context.
            file(
                "far/away/other.py",
                "doc = load('recipes.yaml')\n\n\n\n\n\n\n\n\n\n\n\n\n\n\ny = shopping.get(\"ingredients\")\n",
                &[],
            ),
            file("d0/reader.py", "v = line.get(\"qty\")\n", &[]),
        ];
        for (p, k) in gen_paths.iter().zip(&gen_keys) {
            files.push(file(p, "x: 1\n", k));
        }
        let project = Project::new(&files);
        let (links, _) = analyze_with(&project, MIN_SCORE, Scoring::Context);
        let got: Vec<(&str, &str)> = links
            .iter()
            .map(|l| (l.path.as_str(), project.files[l.code].path))
            .collect();
        assert!(got.contains(&("recipes.bread.ingredients", "studio/compile.py")));
        assert!(!got.contains(&("recipes.bread.ingredients", "far/away/other.py")));
        // `qty` is defined in 10 of 11 families: generic, so the same directory alone is not enough.
        assert!(!got.iter().any(|g| g.0.ends_with("qty")));
        let (ref_links, _) = analyze_with(&project, MIN_SCORE, Scoring::Reference);
        assert!(ref_links.iter().any(|l| l.path.ends_with("qty")));
        assert!(
            ref_links
                .iter()
                .any(|l| project.files[l.code].path == "far/away/other.py")
        );
    }

    #[test]
    fn words_and_parents() {
        let w = context::Words::of("a = 1\nitems_static = doc[\"items\"]\nentryList = 2\n");
        assert_eq!(w.distance("items", 1), Some(1));
        assert_eq!(w.distance("item", 2), Some(0));
        assert_eq!(w.distance("entries", 3), Some(0));
        assert_eq!(w.distance("static", 3), Some(1));
        assert_eq!(w.distance("missing", 1), None);
        assert_eq!(
            context::parents("loot_tables.*.entries.weight", "weight"),
            (vec!["entries", "loot_tables"], false)
        );
        assert_eq!(context::parents("llm", "llm"), (vec![], true));
        assert_eq!(
            context::spread_family("a/workflows/x_v2.yaml", false),
            "workflows/*.yaml"
        );
        assert_eq!(context::spread_family("a/b/node.yaml", true), "node.yaml");
    }
}
