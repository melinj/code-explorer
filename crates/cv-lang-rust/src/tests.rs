use super::*;
use cv_lang::PathIndex;
use cv_lang::token;

const SAMPLE: &str = r#"//! Module docs.
use std::collections::HashMap;
use crate::store::{self, Store as Db, events::*};
use super::util::helper;
pub use crate::model::Item;

mod parse;

/// A bus.
#[derive(Debug)]
pub struct Bus {
    subs: HashMap<String, u32>,
}

impl Bus {
    pub fn new() -> Self {
        Self::with_capacity(4)
    }

    fn with_capacity(n: usize) -> Self {
        let b = Bus { subs: HashMap::new() };
        b.emit("é", n);
        helper(n);
        Db::open("x");
        store::flush();
        crate::model::load(1);
        parse::run();
        self::top_level();
        b
    }

    fn emit(&self, topic: &str, n: usize) {
        self.publish(format!("bus/{topic}/{}", n));
    }

    fn publish(&self, _t: String) {}
}

impl fmt::Display for Bus {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.render())
    }
}

pub trait Sink: Send + io::Write {
    fn put(&self);
    fn flush(&self) {
        self.put();
    }
}

pub fn top_level() -> u32 {
    let x = Bus::new();
    x.emit("a", 1);
    7
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_a_bus() {
        assert_eq!(top_level(), compute(2));
    }
}
"#;

fn syms(f: &BlobFacts) -> Vec<(&str, &str, u32, u32)> {
    let mut v: Vec<(&str, &str, u32, u32)> = f
        .symbols
        .iter()
        .map(|s| (s.qualname.as_str(), s.kind.as_str(), s.start, s.end))
        .collect();
    v.sort_by_key(|x| (x.2, x.0));
    v
}

fn has_ref(f: &BlobFacts, kind: RefKind, name: &str, q: Option<&str>, line: u32) -> bool {
    f.refs
        .iter()
        .any(|r| r.kind == kind && r.name == name && r.qualifier.as_deref() == q && r.line == line)
}

#[test]
fn symbols_with_attributes_impls_traits_and_tests() {
    let f = analyze(SAMPLE);
    assert_eq!(f.syntax_errors, 0);
    assert_eq!(
        syms(&f),
        vec![
            ("Bus", "struct", 9, 13),
            ("impl Bus", "class", 15, 37),
            ("Bus::new", "method", 16, 18),
            ("Bus::with_capacity", "method", 20, 30),
            ("Bus::emit", "method", 32, 34),
            ("Bus::publish", "method", 36, 36),
            ("impl Display for Bus", "class", 39, 43),
            ("<Bus as Display>::fmt", "method", 40, 42),
            ("Sink", "interface", 45, 50),
            ("Sink::put", "method", 46, 46),
            ("Sink::flush", "method", 47, 49),
            ("top_level", "function", 52, 56),
            ("tests", "module", 58, 66),
            ("tests::makes_a_bus", "test", 62, 65),
        ]
    );
    // Inherent impls come first, so `Bus` binds to the impl holding `new`.
    assert_eq!(f.symbols[0].qualname, "impl Bus");
    let new = f.symbols.iter().find(|s| s.qualname == "Bus::new").unwrap();
    assert_eq!(f.symbols[new.parent.unwrap()].qualname, "impl Bus");
}

#[test]
fn use_trees_mod_decls_and_path_imports() {
    let f = analyze(SAMPLE);
    let imps: Vec<(&str, &str, Option<&str>)> = f
        .imports
        .iter()
        .map(|i| (i.local.as_str(), i.module.as_str(), i.imported.as_deref()))
        .collect();
    for want in [
        ("HashMap", "std::collections", Some("HashMap")),
        ("store", "crate", Some("store")),
        ("Db", "crate::store", Some("Store")),
        ("", "crate::store::events", Some("*")),
        ("helper", "super::util", Some("helper")),
        ("Item", "crate::model", Some("Item")),
        ("parse", "self::parse", None),
        // `use super::*` inside `mod tests` is the file's own module.
        ("", "self", Some("*")),
        // Implicit: the module path of `crate::model::load(1)`.
        ("crate::model", "crate::model", None),
    ] {
        assert!(imps.contains(&want), "missing {want:?} in {imps:?}");
    }
    // `parse::run()` goes through the bound `mod parse;` (no implicit import needed).
    assert!(!imps.iter().any(|i| i.0 == "parse" && i.1 != "self::parse"));
}

#[test]
fn call_implement_and_inherit_refs() {
    let f = analyze(SAMPLE);
    assert!(has_ref(&f, RefKind::Call, "with_capacity", Some("Bus"), 17)); // Self::
    assert!(has_ref(&f, RefKind::Call, "emit", Some("b"), 22));
    assert!(has_ref(&f, RefKind::Call, "helper", None, 23));
    assert!(has_ref(&f, RefKind::Call, "open", Some("Db"), 24));
    assert!(has_ref(&f, RefKind::Call, "flush", Some("store"), 25));
    assert!(has_ref(&f, RefKind::Call, "load", Some("crate::model"), 26));
    assert!(has_ref(&f, RefKind::Call, "run", Some("parse"), 27));
    assert!(has_ref(&f, RefKind::Call, "top_level", None, 28)); // self::top_level
    assert!(has_ref(&f, RefKind::Call, "publish", Some("self"), 33));
    // Inside macros: `self.render()` in write!, `compute(2)` and `top_level()` in assert_eq!.
    assert!(has_ref(&f, RefKind::Call, "render", Some("self"), 41));
    assert!(has_ref(&f, RefKind::Call, "compute", None, 64));
    assert!(has_ref(&f, RefKind::Call, "top_level", None, 64));
    assert!(has_ref(&f, RefKind::Implement, "Display", Some("fmt"), 39));
    assert!(has_ref(&f, RefKind::Inherit, "Send", None, 45));
    assert!(has_ref(&f, RefKind::Inherit, "Write", Some("io"), 45));
    assert!(has_ref(&f, RefKind::Call, "put", Some("self"), 48));
    // std macros are not calls; `format!` neither.
    assert!(
        !f.refs
            .iter()
            .any(|r| r.name == "format" || r.name == "write")
    );
    let emit = f
        .refs
        .iter()
        .find(|r| r.name == "emit" && r.line == 54)
        .unwrap();
    assert_eq!(f.symbols[emit.enclosing.unwrap()].qualname, "top_level");
}

#[test]
fn tokens_use_utf16_columns() {
    let f = analyze("fn f() { let s = \"é😀\"; g(1); }\n");
    // `"é😀"` starts at column 17 and is 5 UTF-16 units long.
    let t = &f.tokens[0];
    assert!(t.chunks(3).any(|c| c == [17, 5, token::STRING]), "{t:?}");
    // `g(` is a call after the string: column 24 (not 25 as bytes would say... é=2 bytes, 😀=4).
    assert!(t.chunks(3).any(|c| c == [24, 1, token::FUNCTION]), "{t:?}");
    assert!(t.chunks(3).any(|c| c == [0, 2, token::KEYWORD]), "{t:?}");
    assert!(t.chunks(3).any(|c| c == [3, 1, token::FUNCTION]), "{t:?}");
}

#[test]
fn folds_format_concat_constants_and_literals() {
    let src = r##"const PREFIX: &str = "world";
const TOPIC: &str = concat!("map.", "district", ".build");
fn f(id: u32, name: &str) {
    bus.emit(format!("{PREFIX}.entity.{}", id));
    bus.emit(format!("{}/{{x}}/{n}", PREFIX, n = name));
    bus.emit(TOPIC);
    bus.emit("a\tb\u{e9}");
    bus.emit(r#"raw"#);
    bus.emit(String::from("s") + "/" + &name.to_string());
    bus.emit(name);
    bus.emit("é😀".to_string());
}
"##;
    let at = |line: u32, needle: &str| {
        let l = src.lines().nth(line as usize - 1).unwrap();
        let byte = l.find(needle).unwrap();
        let col = l[..byte].encode_utf16().count() as u32;
        fold_key(src, line, col).unwrap()
    };
    use KeyPart::*;
    assert_eq!(at(4, "format!"), vec![Lit("world.entity.".into()), Segment]);
    assert_eq!(at(5, "format!"), vec![Lit("world/{x}/".into()), Segment]);
    assert_eq!(at(6, "TOPIC"), vec![Lit("map.district.build".into())]);
    assert_eq!(at(7, "\"a"), vec![Lit("a\tbé".into())]);
    assert_eq!(at(8, "r#"), vec![Lit("raw".into())]);
    assert_eq!(at(9, "String"), vec![Lit("s/".into()), Segment]);
    assert!(matches!(at(10, "name").as_slice(), [Unknown(_)]));
    assert_eq!(at(11, "\"é"), vec![Lit("é😀".into())]);
}

#[test]
fn resolves_mod_tree_crates_and_reexports() {
    let files: Vec<(&str, &str)> = vec![
        ("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n"),
        ("crates/core/Cargo.toml", "[package]\nname = \"my-core\"\n"),
        (
            "crates/core/src/lib.rs",
            "pub mod store;\nmod model;\npub use model::Item;\npub use store::*;\n",
        ),
        ("crates/core/src/model.rs", "pub struct Item;\n"),
        (
            "crates/core/src/store/mod.rs",
            "mod disk;\npub use disk::Disk;\npub fn flush() {}\n",
        ),
        ("crates/core/src/store/disk.rs", "pub struct Disk;\n"),
        ("crates/core/src/store/events.rs", "pub enum Event {}\n"),
        (
            "crates/app/Cargo.toml",
            "[package]\nname = \"app\"\n[lib]\npath = \"src/app.rs\"\n",
        ),
        ("crates/app/src/app.rs", "mod util;\n"),
        ("crates/app/src/util.rs", "pub fn helper() {}\n"),
        ("crates/app/src/main.rs", "fn main() {}\n"),
        ("crates/app/src/bin/tool.rs", "fn main() {}\n"),
        ("crates/app/tests/it.rs", "mod common;\n"),
        ("crates/app/tests/common/mod.rs", "pub fn setup() {}\n"),
    ];
    let paths: Vec<&str> = files.iter().map(|f| f.0).collect();
    let text: HashMap<String, String> = files
        .iter()
        .map(|(p, t)| (p.to_string(), t.to_string()))
        .collect();
    let idx = PathIndex::new(paths.iter().copied(), |p| text.get(p).cloned());
    let pack = RustPack::default();
    let r = |from: &str, module: &str, imported: Option<&str>| {
        let imp = ImportFact {
            local: String::new(),
            module: module.into(),
            imported: imported.map(String::from),
            line: 1,
        };
        pack.resolve_import(from, &imp, &idx)
    };
    let lib = "crates/core/src/lib.rs";
    let store = "crates/core/src/store/mod.rs";
    // mod tree
    assert_eq!(r(lib, "self::store", None).as_deref(), Some(store));
    assert_eq!(
        r(store, "self::disk", None).as_deref(),
        Some("crates/core/src/store/disk.rs")
    );
    // crate / super / self
    assert_eq!(
        r(store, "crate::model", Some("Item")).as_deref(),
        Some("crates/core/src/model.rs")
    );
    assert_eq!(
        r("crates/core/src/store/disk.rs", "super", Some("flush")).as_deref(),
        Some(store)
    );
    assert_eq!(
        r("crates/core/src/store/disk.rs", "super::super::model", None).as_deref(),
        Some("crates/core/src/model.rs")
    );
    // other workspace crates by package name (dashes → underscores), with re-exports followed
    assert_eq!(
        r("crates/app/src/app.rs", "my_core", Some("Item")).as_deref(),
        Some("crates/core/src/model.rs")
    );
    assert_eq!(
        r("crates/app/src/app.rs", "my_core", Some("Disk")).as_deref(),
        Some("crates/core/src/store/disk.rs")
    );
    assert_eq!(
        r("crates/app/src/app.rs", "my_core::store::events", Some("*")).as_deref(),
        Some("crates/core/src/store/events.rs")
    );
    // [lib] path, main.rs and bins are their own roots; tests/ targets too
    assert_eq!(
        r("crates/app/src/util.rs", "crate", Some("util")).as_deref(),
        Some("crates/app/src/util.rs")
    );
    assert_eq!(
        r("crates/app/src/main.rs", "app", None).as_deref(),
        Some("crates/app/src/app.rs")
    );
    assert_eq!(
        r("crates/app/tests/it.rs", "self::common", None).as_deref(),
        Some("crates/app/tests/common/mod.rs")
    );
    assert_eq!(r(lib, "std::collections", Some("HashMap")), None);
    assert_eq!(r(lib, "serde", None), None);
}

/// Hand-checked facts on this repo's own crates (≥ 20 files): symbols, refs and resolved imports.
#[test]
fn this_repos_crates() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut paths = Vec::new();
    let mut stack = vec![root.join("crates"), root.clone()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(&root).unwrap().to_string_lossy().to_string();
            if p.is_dir() {
                if d != root && !rel.contains("target") && !rel.contains("fixtures") {
                    stack.push(p);
                }
            } else if rel.ends_with(".rs") || rel.ends_with("Cargo.toml") {
                paths.push(rel);
            }
        }
    }
    paths.sort();
    paths.dedup();
    let text: HashMap<String, String> = paths
        .iter()
        .filter_map(|p| Some((p.clone(), std::fs::read_to_string(root.join(p)).ok()?)))
        .collect();
    let idx = PathIndex::new(paths.iter().map(|s| s.as_str()), |p| text.get(p).cloned());
    let pack = RustPack::default();
    let facts = |p: &str| analyze(&text[p]);
    let resolved = |p: &str| -> Vec<(String, Option<String>, Option<String>)> {
        facts(p)
            .imports
            .iter()
            .map(|i| {
                (
                    i.local.clone(),
                    i.imported.clone(),
                    pack.resolve_import(p, i, &idx),
                )
            })
            .collect()
    };
    let has_sym = |p: &str, q: &str, kind: &str| {
        let f = facts(p);
        assert!(
            f.symbols
                .iter()
                .any(|s| s.qualname == q && s.kind.as_str() == kind),
            "{p}: no {kind} {q}"
        );
        assert_eq!(f.syntax_errors, 0, "{p}");
    };
    let imports_to = |p: &str, local: &str, target: &str| {
        let r = resolved(p);
        assert!(
            r.iter()
                .any(|(l, _, t)| l == local && t.as_deref() == Some(target)),
            "{p}: {local} ↛ {target}: {r:?}"
        );
    };
    // Every Rust file parses.
    let rs: Vec<&String> = paths.iter().filter(|p| p.ends_with(".rs")).collect();
    assert!(rs.len() >= 20, "{}", rs.len());
    for p in &rs {
        let f = facts(p);
        assert_eq!(f.tokens.len() as u32, f.lines, "{p}");
    }

    // 1–4 cv-lang
    has_sym("crates/cv-lang/src/lib.rs", "split_lines", "function");
    has_sym("crates/cv-lang/src/lib.rs", "LanguagePack", "interface");
    has_sym("crates/cv-lang/src/lib.rs", "Registry::pack_for", "method");
    has_sym(
        "crates/cv-lang/src/lib.rs",
        "tests::lines_and_paths",
        "test",
    );
    imports_to(
        "crates/cv-lang/src/lib.rs",
        "chunk",
        "crates/cv-lang/src/chunk.rs",
    );
    imports_to(
        "crates/cv-lang/src/lib.rs",
        "lanes",
        "crates/cv-lang/src/lanes.rs",
    );
    has_sym("crates/cv-lang/src/chunk.rs", "chunk", "function");
    imports_to(
        "crates/cv-lang/src/chunk.rs",
        "SymbolFact",
        "crates/cv-lang/src/lib.rs",
    );
    has_sym(
        "crates/cv-lang/src/lanes.rs",
        "LaneClassifier::classify",
        "method",
    );
    imports_to(
        "crates/cv-lang/src/lanes.rs",
        "Registry",
        "crates/cv-lang/src/lib.rs",
    );
    has_sym("crates/cv-lang/src/silhouette.rs", "records", "function");
    // 5 cv-lang-sitter
    has_sym("crates/cv-lang-sitter/src/lib.rs", "Text::col16", "method");
    imports_to(
        "crates/cv-lang-sitter/src/lib.rs",
        "token",
        "crates/cv-lang/src/lib.rs",
    );
    // 6 cv-lang-python
    has_sym(
        "crates/cv-lang-python/src/lib.rs",
        "<PythonPack as LanguagePack>::analyze",
        "method",
    );
    imports_to(
        "crates/cv-lang-python/src/lib.rs",
        "Text",
        "crates/cv-lang-sitter/src/lib.rs",
    );
    imports_to(
        "crates/cv-lang-python/src/lib.rs",
        "dirname",
        "crates/cv-lang/src/lib.rs",
    );
    // 7 cv-lang-typescript, 8 cv-lang-config
    has_sym(
        "crates/cv-lang-typescript/src/lib.rs",
        "TypeScriptPack",
        "struct",
    );
    has_sym("crates/cv-lang-config/src/lib.rs", "ConfigPack", "struct");
    // 9 cv-link
    has_sym("crates/cv-link/src/lib.rs", "links", "function");
    has_sym(
        "crates/cv-link/src/conn.rs",
        "window_connectedness",
        "function",
    );
    imports_to(
        "crates/cv-link/src/lib.rs",
        "BlobFacts",
        "crates/cv-lang/src/lib.rs",
    );
    // 10 cv-churn, 11 cv-layout
    has_sym("crates/cv-churn/src/lib.rs", "wilson", "function");
    has_sym("crates/cv-layout/src/lib.rs", "layout", "function");
    // 12 cv-contracts
    has_sym("crates/cv-contracts/src/lib.rs", "Rect", "struct");
    imports_to(
        "crates/cv-contracts/src/lib.rs",
        "readside",
        "crates/cv-contracts/src/readside.rs",
    );
    // 13–17 cv-server
    imports_to(
        "crates/cv-server/src/lib.rs",
        "index",
        "crates/cv-server/src/index.rs",
    );
    imports_to(
        "crates/cv-server/src/lib.rs",
        "routes",
        "crates/cv-server/src/routes.rs",
    );
    imports_to(
        "crates/cv-server/src/index.rs",
        "chunk",
        "crates/cv-lang/src/chunk.rs",
    );
    imports_to(
        "crates/cv-server/src/index.rs",
        "LaneClassifier",
        "crates/cv-lang/src/lanes.rs",
    );
    has_sym("crates/cv-server/src/index.rs", "build_view", "function");
    has_sym("crates/cv-server/src/index.rs", "FileRec::line", "method");
    imports_to(
        "crates/cv-server/src/main.rs",
        "routes",
        "crates/cv-server/src/routes.rs",
    );
    has_sym("crates/cv-server/src/main.rs", "main", "function");
    has_sym("crates/cv-server/src/routes.rs", "router", "function");
    has_sym("crates/cv-server/src/readside/mod.rs", "router", "function");
    imports_to(
        "crates/cv-server/src/readside/mod.rs",
        "range",
        "crates/cv-server/src/readside/range.rs",
    );
    // 18–19 cv-vcs
    imports_to(
        "crates/cv-vcs/src/lib.rs",
        "read",
        "crates/cv-vcs/src/read.rs",
    );
    has_sym("crates/cv-vcs/src/lib.rs", "JjRepo::history", "method");
    has_sym("crates/cv-vcs/src/read.rs", "line_runs", "function");
    // 20–21 cv-mcp
    imports_to(
        "crates/cv-mcp/src/lib.rs",
        "server",
        "crates/cv-mcp/src/server.rs",
    );
    has_sym("crates/cv-mcp/src/main.rs", "main", "function");
    // 22 cv-mock
    has_sym("crates/cv-mock/src/main.rs", "main", "function");
    // 23 this pack
    has_sym("crates/cv-lang-rust/src/lib.rs", "fold_key", "function");
    has_sym(
        "crates/cv-lang-rust/src/lib.rs",
        "Resolver::follow",
        "method",
    );

    // Refs: Self:: and self. calls inside impls, macro-argument calls, implement refs.
    let f = facts("crates/cv-lang/src/lib.rs");
    assert!(f.refs.iter().any(|r| {
        r.kind == RefKind::Implement
            && r.name == "ProjectIndex"
            && f.symbols[r.enclosing.unwrap()]
                .qualname
                .starts_with("impl ProjectIndex for PathIndex")
    }));
    let f = facts("crates/cv-lang/src/chunk.rs");
    assert!(
        f.refs
            .iter()
            .any(|r| r.name == "chunk_range" && r.qualifier.is_none())
    );
    assert!(
        f.refs
            .iter()
            .any(|r| r.name == "merge_tiny" && r.qualifier.is_none())
    );
}

/// CPU time of this thread in ms (wall time on a loaded machine overstates parse cost).
fn cpu_ms() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/thread-self/schedstat").ok()?;
    Some(s.split_whitespace().next()?.parse::<f64>().ok()? / 1e6)
}

#[test]
fn parse_speed() {
    // A ~2k-line source made of this crate's own code.
    let src = include_str!("lib.rs");
    let lines = src.lines().count() as f64;
    let mut times: Vec<f64> = (0..7)
        .map(|_| {
            let t = std::time::Instant::now();
            let c = cpu_ms();
            let f = analyze(src);
            std::hint::black_box(&f);
            let ms = match (c, cpu_ms()) {
                (Some(a), Some(b)) => b - a,
                _ => t.elapsed().as_secs_f64() * 1000.0,
            };
            ms / (lines / 1000.0)
        })
        .collect();
    times.sort_by(f64::total_cmp);
    let median = times[times.len() / 2];
    eprintln!("rust pack: median {median:.2} ms per 1k lines ({lines} lines)");
    // A regression guard for the whole analysis, not the budget: the WORKSTREAMS budget (< 5 ms per
    // 1k lines) is on the grammar parse, checked with `examples/parse_speed` in cv-server.
    let limit = if cfg!(debug_assertions) { 80.0 } else { 25.0 };
    assert!(median < limit, "{median} ms per 1k lines");
}

/// The generated anonymous-token block of `highlights.scm` matches the grammar (regenerate:
/// `cargo test -p cv-lang-rust anonymous -- --ignored --nocapture`).
#[test]
fn highlights_cover_every_anonymous_token() {
    let src = include_str!("../queries/highlights.scm");
    let want = anonymous_block();
    assert_eq!(
        cv_lang_sitter::query::anonymous_block_in(src),
        Some(want.as_str())
    );
}

fn anonymous_block() -> String {
    cv_lang_sitter::query::anonymous_block(
        &tree_sitter_rust::LANGUAGE.into(),
        &[],
        cv_lang_sitter::anonymous_class,
    )
}

#[test]
#[ignore]
fn print_anonymous_block() {
    println!("{}", anonymous_block());
}
