//! Lane rules (DESIGN §4.3): ordered, first match wins. The default rules are data
//! (`packs/lanes/defaults.toml`, embedded): packs add their code and test globs, the rest are
//! file-role rules (names, suffixes, directories). The core itself names no file type (D13). A repo can put
//! its own lanes and rules in `.cv/lanes.toml` ([`LaneConfig::parse`]); those rules go before the
//! defaults.
//!
//! Lane ids stay `&'static str`: ids, labels and textures read from a config are interned
//! ([`intern`]), which leaks at most a few strings per distinct lane.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};

use crate::Registry;

pub const INFRA: &str = "infra";
pub const CONFIG: &str = "config";
pub const CODE: &str = "code";
pub const TEST: &str = "test";
pub const DOCS: &str = "docs";
pub const OTHER: &str = "other";

/// Lane ids in display order (top to bottom). `other` is hidden by default.
pub const ORDER: [&str; 6] = [INFRA, CONFIG, CODE, TEST, DOCS, OTHER];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaneMeta {
    pub id: &'static str,
    pub label: &'static str,
    /// model.ts LaneDef texture.
    pub texture: &'static str,
    pub hidden: bool,
    pub collapsed: bool,
}

pub const LANES: [LaneMeta; 6] = [
    LaneMeta {
        id: INFRA,
        label: "Infra",
        texture: "hatch",
        hidden: false,
        collapsed: false,
    },
    LaneMeta {
        id: CONFIG,
        label: "Config",
        texture: "dots",
        hidden: false,
        collapsed: false,
    },
    LaneMeta {
        id: CODE,
        label: "Code",
        texture: "plain",
        hidden: false,
        collapsed: false,
    },
    LaneMeta {
        id: TEST,
        label: "Tests",
        texture: "lines",
        hidden: false,
        collapsed: false,
    },
    // DESIGN §4.3: docs start collapsed (shown, but out of the way until opened).
    LaneMeta {
        id: DOCS,
        label: "Docs",
        texture: "cross",
        hidden: false,
        collapsed: true,
    },
    LaneMeta {
        id: OTHER,
        label: "Other",
        texture: "plain",
        hidden: true,
        collapsed: true,
    },
];

/// The built-in lane rules, as data: no file type is named in the core (D13). Embedded at build
/// time, so the defaults still live in the binary (DESIGN §4.3).
pub const DEFAULT_RULES_TOML: &str = include_str!("../../../packs/lanes/defaults.toml");

/// A content sniff on a matched rule: see `packs/lanes/defaults.toml`.
#[derive(Clone, Debug)]
struct Sniff {
    suffixes: Vec<String>,
    line_prefixes: Vec<String>,
    lane: &'static str,
}

/// One built-in rule: matches when any of its keys matches (see `packs/lanes/defaults.toml`).
#[derive(Clone, Debug, Default)]
struct Rule {
    lane: &'static str,
    names: Vec<String>,
    prefixes: Vec<String>,
    suffixes: Vec<String>,
    contains: Vec<String>,
    dirs: Vec<String>,
    top_dirs: Vec<String>,
    path_prefixes: Vec<String>,
    pack_code: bool,
    pack_test: bool,
    sniff: Option<Sniff>,
}

fn builtin_lane(id: &str) -> Result<&'static str, String> {
    LANES
        .iter()
        .find(|l| l.id == id)
        .map(|l| l.id)
        .ok_or_else(|| format!("unknown lane `{id}`"))
}

fn str_list(t: &dyn toml_edit::TableLike, k: &str) -> Result<Vec<String>, String> {
    let Some(v) = t.get(k) else {
        return Ok(Vec::new());
    };
    let a = v
        .as_array()
        .ok_or_else(|| format!("`{k}` must be an array"))?;
    a.iter()
        .map(|x| {
            x.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("`{k}` must hold strings"))
        })
        .collect()
}

/// Parses the built-in rule data; strict, since the data ships with the binary.
fn parse_rules(text: &str) -> Result<Vec<Rule>, String> {
    let doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| e.to_string())?;
    let tables = doc
        .get("rule")
        .and_then(|r| r.as_array_of_tables())
        .ok_or("expected [[rule]] tables")?;
    let mut rules = Vec::new();
    for (n, t) in tables.iter().enumerate() {
        let at = |e: String| format!("rule #{}: {e}", n + 1);
        for (k, _) in t.iter() {
            if !matches!(
                k,
                "lane"
                    | "names"
                    | "prefixes"
                    | "suffixes"
                    | "contains"
                    | "dirs"
                    | "top_dirs"
                    | "path_prefixes"
                    | "pack_code"
                    | "pack_test"
                    | "sniff"
            ) {
                return Err(at(format!("unknown key `{k}`")));
            }
        }
        let lane = t
            .get("lane")
            .and_then(|v| v.as_str())
            .ok_or_else(|| at("missing `lane`".into()))?;
        let flag = |k: &str| t.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
        let sniff = match t.get("sniff").and_then(|s| s.as_table_like()) {
            None => None,
            Some(s) => Some(Sniff {
                suffixes: str_list(s, "suffixes")
                    .map(|v| v.into_iter().map(|x| x.to_lowercase()).collect())
                    .map_err(at)?,
                line_prefixes: str_list(s, "line_prefixes").map_err(at)?,
                lane: builtin_lane(s.get("lane").and_then(|v| v.as_str()).unwrap_or(""))
                    .map_err(at)?,
            }),
        };
        // Paths are matched lower-cased, so the data is too.
        let lower = |v: Vec<String>| v.into_iter().map(|x| x.to_lowercase()).collect();
        let list = |k: &str| str_list(t, k).map(lower).map_err(at);
        rules.push(Rule {
            lane: builtin_lane(lane).map_err(at)?,
            names: list("names")?,
            prefixes: list("prefixes")?,
            suffixes: list("suffixes")?,
            contains: list("contains")?,
            dirs: list("dirs")?,
            top_dirs: list("top_dirs")?,
            path_prefixes: list("path_prefixes")?,
            pack_code: flag("pack_code"),
            pack_test: flag("pack_test"),
            sniff,
        });
    }
    Ok(rules)
}

fn default_rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        parse_rules(DEFAULT_RULES_TOML).expect("packs/lanes/defaults.toml is valid")
    })
}

/// Whether every prefix starts some line in the first 4,000 bytes of `head`.
fn sniff_lines(head: &[u8], prefixes: &[String]) -> bool {
    let head = String::from_utf8_lossy(&head[..head.len().min(4000)]);
    prefixes
        .iter()
        .all(|p| head.lines().any(|l| l.starts_with(p.as_str())))
}

pub struct LaneClassifier {
    code: GlobSet,
    test: GlobSet,
    /// `.cv/lanes.toml` rules, tried first: glob `i` assigns `user_lanes[i]`.
    user: GlobSet,
    user_lanes: Vec<&'static str>,
}

fn set(globs: &[&str]) -> GlobSet {
    let mut b = GlobSetBuilder::new();
    for g in globs {
        if let Ok(g) = Glob::new(g) {
            b.add(g);
        }
    }
    b.build().expect("valid glob set")
}

impl LaneClassifier {
    /// The built-in rules only.
    pub fn new(registry: &Registry) -> Self {
        Self::with_config(registry, &LaneConfig::default())
    }

    /// The config's rules first (in file order), then the built-in rules.
    pub fn with_config(registry: &Registry, config: &LaneConfig) -> Self {
        let (mut code, mut test) = (Vec::new(), Vec::new());
        for p in registry.packs() {
            let h = p.lane_hints();
            code.extend(h.code);
            test.extend(h.test);
        }
        let mut user = GlobSetBuilder::new();
        for (g, _) in &config.rules {
            user.add(g.clone());
        }
        LaneClassifier {
            code: set(&code),
            test: set(&test),
            user: user.build().unwrap_or_else(|_| GlobSet::empty()),
            user_lanes: config.rules.iter().map(|r| r.1).collect(),
        }
    }

    /// Lane of a path; `head` is the start of the file, for content sniffs (YAML k8s manifests).
    pub fn classify(&self, path: &str, head: &[u8]) -> &'static str {
        if !self.user_lanes.is_empty()
            && let Some(i) = self.user.matches(path).into_iter().min()
        {
            return self.user_lanes[i];
        }
        let p = path.to_lowercase();
        let parts: Vec<&str> = p.split('/').collect();
        let name = *parts.last().unwrap_or(&"");
        let dirs = &parts[..parts.len() - 1];
        let any = |xs: &[String], f: &dyn Fn(&str) -> bool| xs.iter().any(|x| f(x));
        for r in default_rules() {
            let hit = any(&r.names, &|x| name == x)
                || any(&r.prefixes, &|x| name.starts_with(x))
                || any(&r.suffixes, &|x| name.ends_with(x))
                || any(&r.contains, &|x| name.contains(x))
                || any(&r.dirs, &|x| dirs.contains(&x))
                || any(&r.top_dirs, &|x| dirs.first() == Some(&x))
                || any(&r.path_prefixes, &|x| p.starts_with(x))
                || r.pack_code && self.code.is_match(name)
                || r.pack_test && self.test.is_match(name);
            if !hit {
                continue;
            }
            if let Some(sn) = &r.sniff
                && any(&sn.suffixes, &|x| name.ends_with(x))
                && sniff_lines(head, &sn.line_prefixes)
            {
                return sn.lane;
            }
            return r.lane;
        }
        OTHER
    }
}

/// Where a repo keeps its lane config, relative to the repo root.
pub const CONFIG_PATH: &str = ".cv/lanes.toml";
/// At most this many lanes come from a config (bounds the interned strings).
const MAX_USER_LANES: usize = 32;
const TEXTURES: [&str; 5] = ["plain", "hatch", "dots", "lines", "cross"];

/// A `&'static str` for `s`, shared by every caller asking for the same text (leaked once).
pub fn intern(s: &str) -> &'static str {
    static POOL: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut pool = POOL.get_or_init(Default::default).lock().unwrap();
    if let Some(&x) = pool.get(s) {
        return x;
    }
    let x: &'static str = Box::leak(s.to_string().into_boxed_str());
    pool.insert(x);
    x
}

/// Lanes and their rules: the defaults, or the defaults overridden by a repo's `.cv/lanes.toml`.
///
/// Format (one `[[lane]]` table per lane; every key but `id` is optional):
///
/// ```toml
/// [[lane]]
/// id = "prompts"          # new lane, or a built-in id (infra config code test docs other)
/// label = "Prompts"       # default: the built-in label, else the id
/// texture = "dots"        # plain | hatch | dots | lines | cross; unknown → plain
/// order = 2               # 0-based position among all lanes; default: keep / before `other`
/// globs = ["src/**/prompt*.yaml"]  # repo-relative paths, `*` stays within a directory
/// hidden = false          # not indexed or drawn (like `other`)
/// collapsed = false
/// ```
///
/// Rules are every lane's globs in file order, tried before the built-in rules; the first match
/// wins, so a table with a built-in id and globs moves files into that lane. Errors never fail:
/// bad TOML keeps the defaults, a bad entry or glob is skipped, each with a warning.
#[derive(Clone)]
pub struct LaneConfig {
    /// Every lane in display order, hidden ones included.
    pub lanes: Vec<LaneMeta>,
    /// User rules in order: glob → lane id.
    pub rules: Vec<(Glob, &'static str)>,
    pub warnings: Vec<String>,
}

impl Default for LaneConfig {
    fn default() -> Self {
        LaneConfig {
            lanes: LANES.to_vec(),
            rules: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

impl LaneConfig {
    /// Lanes that are indexed and drawn, top to bottom.
    pub fn view_lanes(&self) -> Vec<&'static str> {
        self.lanes
            .iter()
            .filter(|l| !l.hidden)
            .map(|l| l.id)
            .collect()
    }

    pub fn is_hidden(&self, id: &str) -> bool {
        self.lanes.iter().any(|l| l.id == id && l.hidden)
    }

    /// Parses a `.cv/lanes.toml`; never fails (see the type docs).
    pub fn parse(text: &str) -> LaneConfig {
        let mut cfg = LaneConfig::default();
        let doc = match text.parse::<toml_edit::DocumentMut>() {
            Ok(d) => d,
            Err(e) => {
                let msg = e.to_string().lines().next().unwrap_or("").to_string();
                cfg.warnings
                    .push(format!("not valid TOML, using the default lanes: {msg}"));
                return cfg;
            }
        };
        for (k, _) in doc.iter() {
            if k != "lane" {
                cfg.warnings.push(format!("unknown key `{k}` ignored"));
            }
        }
        let Some(item) = doc.get("lane") else {
            return cfg;
        };
        let Some(tables) = item.as_array_of_tables() else {
            cfg.warnings
                .push("`lane` must be an array of tables ([[lane]]); ignored".into());
            return cfg;
        };
        let builtin: Vec<&str> = LANES.iter().map(|l| l.id).collect();
        let mut seen: Vec<&'static str> = Vec::new();
        let mut new_lanes = 0;
        // (lane id, requested order)
        let mut placed: Vec<(&'static str, usize)> = Vec::new();
        for (n, t) in tables.iter().enumerate() {
            let at = format!("lane #{}", n + 1);
            let Some(id) = t.get("id").and_then(|v| v.as_str()) else {
                cfg.warnings
                    .push(format!("{at}: missing string `id`; entry ignored"));
                continue;
            };
            let valid = !id.is_empty()
                && id.len() <= 32
                && id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
            if !valid {
                cfg.warnings.push(format!(
                    "{at}: id `{id}` must be 1-32 of a-z 0-9 - _; entry ignored"
                ));
                continue;
            }
            let is_builtin = builtin.contains(&id);
            if seen.contains(&id) {
                cfg.warnings.push(format!(
                    "lane `{id}` defined twice; the later entry is ignored"
                ));
                continue;
            }
            if !is_builtin && new_lanes >= MAX_USER_LANES {
                cfg.warnings.push(format!(
                    "more than {MAX_USER_LANES} new lanes; `{id}` ignored"
                ));
                continue;
            }
            let at = format!("lane `{id}`");
            for (k, _) in t.iter() {
                if !matches!(
                    k,
                    "id" | "label" | "texture" | "order" | "globs" | "hidden" | "collapsed"
                ) {
                    cfg.warnings
                        .push(format!("{at}: unknown key `{k}` ignored"));
                }
            }
            let id = intern(id);
            seen.push(id);
            let str_of = |k: &str, w: &mut Vec<String>| -> Option<String> {
                let v = t.get(k)?;
                match v.as_str() {
                    Some(s) => Some(s.to_string()),
                    None => {
                        w.push(format!("{at}: `{k}` must be a string; ignored"));
                        None
                    }
                }
            };
            let bool_of = |k: &str, w: &mut Vec<String>| -> Option<bool> {
                let v = t.get(k)?;
                match v.as_bool() {
                    Some(b) => Some(b),
                    None => {
                        w.push(format!("{at}: `{k}` must be true or false; ignored"));
                        None
                    }
                }
            };
            let label = str_of("label", &mut cfg.warnings);
            let texture = str_of("texture", &mut cfg.warnings).map(|tx| {
                if TEXTURES.contains(&tx.as_str()) {
                    intern(&tx)
                } else {
                    cfg.warnings
                        .push(format!("{at}: unknown texture `{tx}`, using plain"));
                    "plain"
                }
            });
            let hidden = bool_of("hidden", &mut cfg.warnings);
            let collapsed = bool_of("collapsed", &mut cfg.warnings);
            let order = t.get("order").and_then(|v| match v.as_integer() {
                Some(o) if o >= 0 => Some(o as usize),
                _ => {
                    cfg.warnings.push(format!(
                        "{at}: `order` must be a non-negative integer; ignored"
                    ));
                    None
                }
            });
            if let Some(g) = t.get("globs") {
                match g.as_array() {
                    Some(a) => {
                        for v in a.iter() {
                            let Some(src) = v.as_str() else {
                                cfg.warnings
                                    .push(format!("{at}: a glob that is not a string ignored"));
                                continue;
                            };
                            match GlobBuilder::new(src).literal_separator(true).build() {
                                Ok(glob) => cfg.rules.push((glob, id)),
                                Err(e) => cfg
                                    .warnings
                                    .push(format!("{at}: bad glob `{src}` ignored: {e}")),
                            }
                        }
                    }
                    None => cfg.warnings.push(format!(
                        "{at}: `globs` must be an array of strings; ignored"
                    )),
                }
            }
            match cfg.lanes.iter_mut().find(|l| l.id == id) {
                Some(l) => {
                    if let Some(s) = label {
                        l.label = intern(&s);
                    }
                    if let Some(tx) = texture {
                        l.texture = tx;
                    }
                    if let Some(h) = hidden {
                        l.hidden = h;
                    }
                    if let Some(c) = collapsed {
                        l.collapsed = c;
                    }
                }
                None => {
                    new_lanes += 1;
                    let meta = LaneMeta {
                        id,
                        label: intern(label.as_deref().unwrap_or(id)),
                        texture: texture.unwrap_or("plain"),
                        hidden: hidden.unwrap_or(false),
                        collapsed: collapsed.unwrap_or(false),
                    };
                    // Without an order a new lane goes last among the shown ones, before `other`.
                    let at = cfg
                        .lanes
                        .iter()
                        .position(|l| l.id == OTHER)
                        .unwrap_or(cfg.lanes.len());
                    cfg.lanes.insert(at, meta);
                }
            }
            if let Some(o) = order {
                placed.push((id, o));
            }
        }
        // Explicit orders: take those lanes out, then insert them at their positions, lowest first.
        placed.sort_by_key(|&(_, o)| o);
        let mut moved: Vec<(LaneMeta, usize)> = Vec::new();
        for &(id, o) in &placed {
            if let Some(i) = cfg.lanes.iter().position(|l| l.id == id) {
                moved.push((cfg.lanes.remove(i), o));
            }
        }
        for (l, o) in moved {
            let at = o.min(cfg.lanes.len());
            cfg.lanes.insert(at, l);
        }
        cfg
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlobFacts, ImportFact, LaneHints, LanguagePack, ProjectIndex};

    struct Stub;
    impl LanguagePack for Stub {
        fn id(&self) -> &'static str {
            "stub"
        }
        fn version(&self) -> &'static str {
            "0"
        }
        fn claims(&self, path: &str) -> bool {
            path.ends_with(".stub")
        }
        fn lang(&self, _: &str) -> &'static str {
            "other"
        }
        fn analyze(&self, _: &str, _: &str) -> BlobFacts {
            BlobFacts::default()
        }
        fn resolve_import(&self, _: &str, _: &ImportFact, _: &dyn ProjectIndex) -> Option<String> {
            None
        }
        fn lane_hints(&self) -> LaneHints {
            LaneHints {
                code: vec!["*.stub"],
                test: vec!["check_*.stub"],
            }
        }
    }

    #[test]
    fn defaults_follow_the_design_table() {
        let mut r = Registry::new();
        r.register(Box::new(Stub));
        let c = LaneClassifier::new(&r);
        assert_eq!(c.classify("src/a.stub", b""), CODE);
        assert_eq!(c.classify("src/check_a.stub", b""), TEST);
        assert_eq!(c.classify("tests/x/y.stub", b""), TEST);
        assert_eq!(c.classify("Dockerfile", b""), INFRA);
        assert_eq!(c.classify(".github/workflows/ci.yml", b""), INFRA);
        assert_eq!(
            c.classify("deploy/app.yaml", b"apiVersion: v1\nkind: Pod\n"),
            INFRA
        );
        assert_eq!(c.classify("conf/app.yaml", b"a: 1\n"), CONFIG);
        assert_eq!(c.classify(".env.example", b""), CONFIG);
        assert_eq!(c.classify("README.md", b""), DOCS);
        assert_eq!(c.classify("docs/guide.txt", b""), DOCS);
        assert_eq!(c.classify("uv.lock", b""), OTHER);
        assert_eq!(c.classify("web/node_modules/x/y.stub", b""), OTHER);
        assert_eq!(c.classify("img/logo.png", b""), OTHER);
    }

    /// D15: config files always get the config lane, under tests/ and docs/ too (a k8s manifest
    /// there is still infra); code and docs there keep their directory's lane.
    #[test]
    fn config_files_under_tests_and_docs_are_config() {
        let mut r = Registry::new();
        r.register(Box::new(Stub));
        let c = LaneClassifier::new(&r);
        for p in [
            "tests/fixtures/world.yaml",
            "tests/data/case.json",
            "src/__tests__/fixtures/golden.json",
            "e2e/playwright.config.toml",
            "tests/.env.test",
            "docs/mkdocs.yml",
            "docs/spikes/x/Cargo.toml",
            "doc/sub/y.ini",
        ] {
            assert_eq!(c.classify(p, b"a: 1\n"), CONFIG, "{p}");
        }
        assert_eq!(
            c.classify("tests/k8s/pod.yaml", b"apiVersion: v1\nkind: Pod\n"),
            INFRA
        );
        assert_eq!(c.classify("tests/x/check_y.stub", b""), TEST);
        assert_eq!(c.classify("tests/x/y.stub", b""), TEST);
        assert_eq!(c.classify("docs/guide.md", b""), DOCS);
        assert_eq!(c.classify("docs/notes.txt", b""), DOCS);
        // A user rule still beats D15 (DESIGN §4.3).
        let cfg = LaneConfig::parse("[[lane]]\nid = \"test\"\nglobs = [\"tests/fixtures/**\"]\n");
        let c = LaneClassifier::with_config(&r, &cfg);
        assert_eq!(c.classify("tests/fixtures/world.yaml", b""), TEST);
    }

    /// DESIGN §4.3 `other` (hidden): vendored and generated files, wherever they are.
    #[test]
    fn vendored_and_generated_files_are_other() {
        let mut r = Registry::new();
        r.register(Box::new(Stub));
        let c = LaneClassifier::new(&r);
        for p in [
            "vendor/github.com/x/y.stub",
            "src/third_party/lib/a.stub",
            "native/third-party/zlib/zlib.h",
            "tests/fixtures/geo/third_party/gtest/gtest.h",
            "web/src/types/generated/api.stub",
            "src/__generated__/schema.stub",
            "proto/user_pb2.py",
            "proto/user_pb2_grpc.py",
            "api/user.pb.go",
            "src/schema.generated.stub",
            "static/app.min.js",
        ] {
            assert_eq!(c.classify(p, b""), OTHER, "{p}");
        }
        // Not vendored: a name that only contains the word.
        assert_eq!(c.classify("src/vendors/a.stub", b""), CODE);
        assert_eq!(c.classify("src/generate/a.stub", b""), CODE);
        assert!(LANES.iter().any(|l| l.id == OTHER && l.hidden));
    }

    /// DESIGN §4.3: the docs lane starts collapsed (and `other` hidden); a config may open it.
    #[test]
    fn docs_start_collapsed() {
        let d = LaneConfig::default();
        let docs = d.lanes.iter().find(|l| l.id == DOCS).unwrap();
        assert!(docs.collapsed && !docs.hidden);
        assert!(
            d.lanes
                .iter()
                .filter(|l| l.collapsed)
                .all(|l| l.id == DOCS || l.id == OTHER)
        );
        let open = LaneConfig::parse("[[lane]]\nid = \"docs\"\ncollapsed = false\n");
        assert!(!open.lanes.iter().find(|l| l.id == DOCS).unwrap().collapsed);
    }

    /// The lane hints of the Python, TypeScript, Rust and C++ packs (copied: cv-lang cannot
    /// depend on its packs), as they were when `testdata/lane_sample.tsv` was captured.
    struct PackHints;
    impl LanguagePack for PackHints {
        fn id(&self) -> &'static str {
            "hints"
        }
        fn version(&self) -> &'static str {
            "0"
        }
        fn claims(&self, _: &str) -> bool {
            false
        }
        fn lang(&self, _: &str) -> &'static str {
            "other"
        }
        fn analyze(&self, _: &str, _: &str) -> BlobFacts {
            BlobFacts::default()
        }
        fn resolve_import(&self, _: &str, _: &ImportFact, _: &dyn ProjectIndex) -> Option<String> {
            None
        }
        fn lane_hints(&self) -> LaneHints {
            LaneHints {
                code: vec![
                    "*.py", "*.pyi", "*.ts", "*.tsx", "*.mts", "*.cts", "*.js", "*.jsx", "*.mjs",
                    "*.cjs", "*.rs", "*.c", "*.cc", "*.cpp", "*.cxx", "*.c++", "*.h", "*.hh",
                    "*.hpp", "*.hxx", "*.ipp", "*.inl",
                ],
                test: vec![
                    "test_*.py",
                    "*_test.py",
                    "conftest.py",
                    "*.test.[jt]s",
                    "*.test.[jt]sx",
                    "*.spec.[jt]s",
                    "*.spec.[jt]sx",
                    "*_test.rs",
                    "*_tests.rs",
                    "tests.rs",
                    "*_test.cc",
                    "*_test.cpp",
                    "*_test.cxx",
                    "*_tests.cc",
                    "*_tests.cpp",
                    "*_unittest.cc",
                    "*_unittest.cpp",
                    "test_*.c",
                    "test_*.cc",
                    "test_*.cpp",
                    "*.test.cpp",
                    "*.test.cc",
                    "*_spec.cpp",
                ],
            }
        }
    }

    /// D13: moving the rules from code to `packs/lanes/defaults.toml` changed no lane. The sample
    /// was captured from the old hard-coded classifier, with the rows that the D15 rule order and
    /// the vendored/generated rule changed on purpose updated (see its header);
    /// `CV_LANE_SAMPLE=<tsv>` checks another capture in the same format (e.g. every path of
    /// a large repo).
    #[test]
    fn data_rules_classify_like_the_old_hard_coded_rules() {
        let mut r = Registry::new();
        r.register(Box::new(PackHints));
        let c = LaneClassifier::new(&r);
        let mut text = include_str!("../testdata/lane_sample.tsv").to_string();
        if let Ok(extra) = std::env::var("CV_LANE_SAMPLE") {
            text.push_str(&std::fs::read_to_string(extra).expect("CV_LANE_SAMPLE readable"));
        }
        let (mut n, mut bad) = (0, Vec::new());
        for line in text
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
        {
            let f: Vec<&str> = line.split('\t').collect();
            let head: &[u8] = if f[1] == "1" {
                b"apiVersion: v1\nkind: Pod\n"
            } else {
                b""
            };
            n += 1;
            let got = c.classify(f[0], head);
            if got != f[2] {
                bad.push(format!("{}: {} != {}", f[0], got, f[2]));
            }
        }
        assert!(n > 1000, "sample has {n} paths");
        assert!(bad.is_empty(), "{} of {n} differ: {bad:?}", bad.len());
    }

    #[test]
    fn default_rule_data_is_valid_and_strict() {
        assert_eq!(default_rules().len(), 6);
        assert!(parse_rules("[[rule]]\nlane = \"nope\"").is_err());
        assert!(parse_rules("[[rule]]\nlane = \"code\"\nsufixes = []").is_err());
        assert!(parse_rules("[[rule]]\nlane = \"code\"\nsuffixes = [1]").is_err());
        let r = parse_rules("[[rule]]\nlane = \"docs\"\nsuffixes = [\".MD\"]").unwrap();
        assert_eq!(r[0].suffixes, [".md"], "data is lower-cased like the paths");
    }

    /// D13: no file-type literal from the rule data appears in the classifier's code.
    #[test]
    fn core_names_no_file_types() {
        let src = include_str!("lanes.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        for r in default_rules() {
            for x in r.suffixes.iter().chain(&r.names).chain(&r.prefixes) {
                assert!(
                    !code.contains(&format!("\"{x}\"")),
                    "`{x}` is hard-coded in lanes.rs"
                );
            }
        }
    }

    fn ids(c: &LaneConfig) -> Vec<&'static str> {
        c.lanes.iter().map(|l| l.id).collect()
    }

    #[test]
    fn config_adds_lanes_and_rules_first() {
        let mut r = Registry::new();
        r.register(Box::new(Stub));
        let cfg = LaneConfig::parse(
            r#"
            [[lane]]
            id = "prompts"
            label = "Prompts"
            texture = "dots"
            order = 2
            globs = ["src/**/prompt*.yaml"]

            [[lane]]
            id = "code"
            globs = ["scripts/*.yaml"]

            [[lane]]
            id = "vendor"
            hidden = true
            globs = ["third_party/**"]

            [[lane]]
            id = "infra"
            order = 9
            label = "Ops"
            "#,
        );
        assert!(cfg.warnings.is_empty(), "{:?}", cfg.warnings);
        assert_eq!(
            ids(&cfg),
            [
                "config", "code", "prompts", "test", "docs", "vendor", "other", "infra"
            ],
            "`order` is the final position; infra moved out from in front"
        );
        assert_eq!(
            cfg.view_lanes(),
            ["config", "code", "prompts", "test", "docs", "infra"]
        );
        assert_eq!(cfg.lanes.last().unwrap().label, "Ops");
        let c = LaneClassifier::with_config(&r, &cfg);
        assert_eq!(c.classify("src/agents/prompt_a.yaml", b""), "prompts");
        assert_eq!(c.classify("src/prompt_a.yaml", b""), "prompts");
        assert_eq!(c.classify("lib/prompt_a.yaml", b""), CONFIG);
        assert_eq!(c.classify("scripts/run.yaml", b""), CODE);
        assert_eq!(c.classify("third_party/x/a.stub", b""), "vendor");
        assert!(cfg.is_hidden("vendor"));
        assert_eq!(c.classify("src/a.stub", b""), CODE);
    }

    #[test]
    fn config_errors_keep_the_defaults() {
        let bad = LaneConfig::parse("[[lane]\nid = 'x'");
        assert_eq!(ids(&bad), ids(&LaneConfig::default()));
        assert_eq!(bad.warnings.len(), 1);
        let partly = LaneConfig::parse(
            r#"
            colour = "red"
            [[lane]]
            id = "Bad Id"
            [[lane]]
            id = "p"
            texture = "zigzag"
            order = -1
            globs = ["a/[x", "b/*.y"]
            [[lane]]
            id = "p"
            "#,
        );
        assert_eq!(partly.warnings.len(), 6, "{:?}", partly.warnings);
        let p = partly.lanes.iter().find(|l| l.id == "p").unwrap();
        assert_eq!((p.texture, p.label), ("plain", "p"));
        assert_eq!(partly.rules.len(), 1);
        let lane = LaneConfig::parse("lane = 3");
        assert_eq!(ids(&lane), ids(&LaneConfig::default()));
        assert_eq!(lane.warnings.len(), 1);
    }
}
