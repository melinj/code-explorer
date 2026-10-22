//! B4 `cv-link`: code links and connectedness.
//!
//! **Links** (DESIGN §4.5) come in tiers:
//!
//! 1. *Lexical, import-aware* ([`links`]). A call or inheritance ref resolves precisely when the
//!    name is pinned down without guessing:
//!    - an unqualified name defined at the top level of the same file (0.9);
//!    - a name bound by an import whose target file defines it at top level (0.85);
//!    - `self.x` / `this.x` / `cls.x` → a method of the enclosing class (0.9);
//!    - `m.x` where `m` is an imported module, or a class (local or imported) → its member (0.85);
//!    - names reachable only through star imports (0.6).
//!
//!    A ref that stays unresolved and is not bound to anything in its file is *ambiguous*: when
//!    1–[`MAX_AMBIGUOUS_DEFS`] same-language definitions of that name exist in the project (top-level
//!    ones for a bare name, members for `x.name` / `self.name`), the nearest
//!    [`MAX_CANDIDATES`] by directory become candidates at confidence ≤ 0.5 ([`candidate_confidence`]).
//!    The UI draws links below 0.6 faded and dashed; connectedness and layout ignore them.
//! 2. *SCIP* ([`scip`]): an `index.scip` at the indexed revision adds precise links (1.0,
//!    provenance "scip") that replace the lexical guesses on the lines it covers.
//! 3. Human and agent feedback lives in the server (review.db).
//!
//! One link per (caller window, callee window, kind), at the first callsite line, keeping the most
//! confident.
//!
//! **Connectedness** (DESIGN §4.8, D7) is computed over windows and typed links: see [`conn`].

pub mod conn;
pub mod scip;

use std::collections::{BTreeMap, HashMap, HashSet};

use cv_lang::{BlobFacts, Conventions, RefKind, SymbolKind};
use rayon::prelude::*;

pub struct FileIn<'a> {
    pub path: &'a str,
    pub lane: &'a str,
    /// Language pack id; lexical candidates stay within one language.
    pub lang: Option<&'a str>,
    /// The language pack's naming conventions (self receivers, entry modules, reserved names).
    pub conv: Conventions,
    pub facts: Option<&'a BlobFacts>,
    /// Resolved target file (index into the input slice) per import, parallel to `facts.imports`.
    pub imports: Vec<Option<usize>>,
    /// This file's windows as (start, end, window id), sorted by start.
    pub windows: Vec<(u32, u32, usize)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LinkOut {
    pub kind: RefKind,
    pub from_window: usize,
    pub from_line: u32,
    pub to_window: usize,
    pub to_line: u32,
    pub confidence: f64,
    /// Qualified name of the target symbol.
    pub label: String,
    /// "tree-sitter" (lexical) or "scip".
    pub provenance: &'static str,
}

/// At most this many candidates for an ambiguous ref.
pub const MAX_CANDIDATES: usize = 3;
/// Names defined more often than this are too ambiguous to guess.
pub const MAX_AMBIGUOUS_DEFS: usize = 6;

/// Confidence of each candidate of an ambiguous ref with `defs` definitions: 0.5 for a unique
/// definition, 0.05 less per extra definition, never below 0.25.
pub fn candidate_confidence(defs: usize) -> f64 {
    (0.5 - 0.05 * defs.saturating_sub(1) as f64).max(0.25)
}

pub const CODE_LANE: &str = "code";
pub const TEST_LANE: &str = "test";

struct FileSyms {
    top: HashMap<String, usize>,
    members: HashMap<(usize, String), usize>,
}

fn file_syms(f: Option<&BlobFacts>) -> FileSyms {
    let mut top = HashMap::new();
    let mut members = HashMap::new();
    if let Some(f) = f {
        for (i, s) in f.symbols.iter().enumerate() {
            match s.parent {
                None => {
                    top.entry(s.name.clone()).or_insert(i);
                }
                Some(p) => {
                    members.entry((p, s.name.clone())).or_insert(i);
                }
            }
        }
    }
    FileSyms { top, members }
}

/// What an import binds locally.
#[derive(Clone, Copy)]
enum Binding<'a> {
    Module(usize),
    Member(usize, &'a str),
}

pub(crate) fn window_at(windows: &[(u32, u32, usize)], line: u32) -> Option<usize> {
    let i = windows.partition_point(|w| w.0 <= line);
    (i > 0 && windows[i - 1].1 >= line).then(|| windows[i - 1].2)
}

fn is_callable(k: SymbolKind) -> bool {
    !matches!(k, SymbolKind::ConfigBlock | SymbolKind::Module)
}

/// Directory segments two paths share.
fn shared_dirs(a: &str, b: &str) -> usize {
    let (da, db) = (cv_lang::dirname(a), cv_lang::dirname(b));
    if da.is_empty() || db.is_empty() {
        return 0;
    }
    da.split('/')
        .zip(db.split('/'))
        .take_while(|(x, y)| x == y)
        .count()
}

/// Project-wide definitions by name, for lexical candidates.
#[derive(Default)]
struct Defs<'a> {
    top: HashMap<&'a str, Vec<(usize, usize)>>,
    members: HashMap<&'a str, Vec<(usize, usize)>>,
}

impl<'a> Defs<'a> {
    fn new(files: &[FileIn<'a>]) -> Self {
        let mut d = Defs::default();
        for (fi, f) in files.iter().enumerate() {
            let Some(facts) = f.facts else { continue };
            if f.windows.is_empty() {
                continue;
            }
            for (si, s) in facts.symbols.iter().enumerate() {
                if !is_callable(s.kind) {
                    continue;
                }
                let m = if s.parent.is_none() {
                    &mut d.top
                } else {
                    &mut d.members
                };
                m.entry(s.name.as_str()).or_default().push((fi, si));
            }
        }
        d
    }
}

/// Whether a name is worth guessing at all: reserved names (the pack's reserved prefixes) and
/// one- or two-letter names are not.
fn guessable(name: &str, conv: &Conventions) -> bool {
    name.chars().count() >= 3 && !conv.is_reserved(name)
}

pub fn links(files: &[FileIn]) -> Vec<LinkOut> {
    let syms: Vec<FileSyms> = files.iter().map(|f| file_syms(f.facts)).collect();
    let defs = Defs::new(files);
    // Files in parallel: a link's key starts with its caller window, which belongs to one file,
    // so the per-file maps are disjoint and their union is what one pass would build.
    let per_file: Vec<BTreeMap<(usize, usize, u8), LinkOut>> = files
        .par_iter()
        .enumerate()
        .map(|(fi, f)| file_links(files, &syms, &defs, fi, f))
        .collect();
    let mut out: BTreeMap<(usize, usize, u8), LinkOut> = BTreeMap::new();
    for m in per_file {
        out.extend(m);
    }
    out.into_values().collect()
}

/// The links out of file `fi` (see [`links`]), by (caller window, callee window, kind).
fn file_links(
    files: &[FileIn],
    syms: &[FileSyms],
    defs: &Defs,
    fi: usize,
    f: &FileIn,
) -> BTreeMap<(usize, usize, u8), LinkOut> {
    let mut out: BTreeMap<(usize, usize, u8), LinkOut> = BTreeMap::new();
    {
        let Some(facts) = f.facts else { return out };
        if f.windows.is_empty() {
            return out;
        }
        let mut bindings: HashMap<&str, Binding> = HashMap::new();
        let mut stars: Vec<usize> = Vec::new();
        for (imp, target) in facts.imports.iter().zip(&f.imports) {
            let Some(t) = *target else { continue };
            match imp.imported.as_deref() {
                Some("*") => stars.push(t),
                Some(name) if files[t].conv.module_name(files[t].path) != name => {
                    if !imp.local.is_empty() {
                        bindings.insert(imp.local.as_str(), Binding::Member(t, name));
                    }
                }
                _ => {
                    if !imp.local.is_empty() {
                        bindings.insert(imp.local.as_str(), Binding::Module(t));
                    }
                }
            }
        }
        // Every name an import binds, resolved or not: such names are never guessed.
        let bound: HashSet<&str> = facts
            .imports
            .iter()
            .map(|i| i.local.as_str())
            .filter(|l| !l.is_empty())
            .collect();
        let class_of = |mut s: Option<usize>| {
            while let Some(i) = s {
                if facts.symbols[i].kind == SymbolKind::Class {
                    return Some(i);
                }
                s = facts.symbols[i].parent;
            }
            None
        };
        for r in &facts.refs {
            let target: Option<(usize, usize, f64)> = match r.qualifier.as_deref() {
                None => {
                    if let Some(&s) = syms[fi].top.get(&r.name) {
                        Some((fi, s, 0.9))
                    } else if let Some(b) = bindings.get(r.name.as_str()) {
                        match *b {
                            Binding::Member(t, name) => {
                                syms[t].top.get(name).map(|&s| (t, s, 0.85))
                            }
                            Binding::Module(_) => None,
                        }
                    } else {
                        stars
                            .iter()
                            .find_map(|&t| syms[t].top.get(&r.name).map(|&s| (t, s, 0.6)))
                    }
                }
                Some(q) if f.conv.is_self_receiver(q) => class_of(r.enclosing)
                    .and_then(|c| syms[fi].members.get(&(c, r.name.clone())))
                    .map(|&s| (fi, s, 0.9)),
                Some(q) => {
                    let class_member = |t: usize, class: usize| {
                        syms[t]
                            .members
                            .get(&(class, r.name.clone()))
                            .map(|&s| (t, s, 0.85))
                    };
                    if let Some(b) = bindings.get(q) {
                        match *b {
                            Binding::Module(t) => syms[t].top.get(&r.name).map(|&s| (t, s, 0.85)),
                            Binding::Member(t, name) => {
                                syms[t].top.get(name).and_then(|&c| class_member(t, c))
                            }
                        }
                    } else if let Some(&c) = syms[fi].top.get(q) {
                        class_member(fi, c)
                    } else {
                        None
                    }
                }
            };
            let targets: Vec<(usize, usize, f64)> = match target {
                Some(t) => vec![t],
                None => candidates(files, defs, &syms[fi], &bound, fi, r),
            };
            for (tf, ts, confidence) in targets {
                let Some(tfacts) = files[tf].facts else {
                    continue;
                };
                let sym = &tfacts.symbols[ts];
                if !is_callable(sym.kind) {
                    continue;
                }
                let (Some(from), Some(to)) = (
                    window_at(&f.windows, r.line),
                    window_at(&files[tf].windows, sym.name_line),
                ) else {
                    continue;
                };
                if from == to {
                    continue;
                }
                let link = LinkOut {
                    kind: r.kind,
                    from_window: from,
                    from_line: r.line,
                    to_window: to,
                    to_line: sym.name_line,
                    confidence,
                    label: sym.qualname.clone(),
                    provenance: "tree-sitter",
                };
                match out.entry((from, to, r.kind as u8)) {
                    std::collections::btree_map::Entry::Vacant(e) => {
                        e.insert(link);
                    }
                    std::collections::btree_map::Entry::Occupied(mut e) => {
                        if e.get().confidence < confidence {
                            e.insert(link);
                        }
                    }
                }
            }
        }
    }
    out
}

/// Lexical candidates for a ref that did not resolve (tier 1, ambiguous): same-language
/// definitions of the name elsewhere, nearest directory first, at most [`MAX_CANDIDATES`].
fn candidates(
    files: &[FileIn],
    defs: &Defs,
    own: &FileSyms,
    bound: &HashSet<&str>,
    fi: usize,
    r: &cv_lang::RefFact,
) -> Vec<(usize, usize, f64)> {
    if !guessable(&r.name, &files[fi].conv)
        || files[fi]
            .conv
            .is_builtin_call(r.qualifier.as_deref(), &r.name)
    {
        return vec![];
    }
    let pool = match r.qualifier.as_deref() {
        // A bare name: not defined here, not imported → a top-level definition elsewhere.
        None => {
            if bound.contains(r.name.as_str()) || own.top.contains_key(&r.name) {
                return vec![];
            }
            defs.top.get(r.name.as_str())
        }
        // `self.x` that is not a method of the enclosing class (inherited, mixed in), or `v.x` on a
        // receiver that is neither imported nor defined here → a member of some class.
        Some(q) => {
            if bound.contains(q) || own.top.contains_key(q) {
                return vec![];
            }
            defs.members.get(r.name.as_str())
        }
    };
    let lang = files[fi].lang;
    let mut c: Vec<(usize, usize)> = pool
        .map(|v| {
            v.iter()
                .copied()
                .filter(|&(t, _)| (r.qualifier.is_some() || t != fi) && files[t].lang == lang)
                .collect()
        })
        .unwrap_or_default();
    if c.is_empty() || c.len() > MAX_AMBIGUOUS_DEFS {
        return vec![];
    }
    let conf = candidate_confidence(c.len());
    let here = files[fi].path;
    c.sort_by(|&(a, sa), &(b, sb)| {
        shared_dirs(here, files[b].path)
            .cmp(&shared_dirs(here, files[a].path))
            .then_with(|| (b == fi).cmp(&(a == fi)))
            .then_with(|| files[a].path.cmp(files[b].path))
            .then(sa.cmp(&sb))
    });
    c.truncate(MAX_CANDIDATES);
    c.into_iter().map(|(t, s)| (t, s, conf)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cv_lang::{ImportFact, RefFact, SymbolFact};

    /// Conventions of a Python-like test language.
    const TEST_CONV: Conventions = Conventions {
        self_receivers: &["self"],
        module_entry_stems: &["__init__"],
        entry_stems: &[],
        reserved_prefixes: &["__"],
        builtin_methods: &["append"],
        builtin_receivers: &["os"],
        builtin_functions: &["len"],
    };

    fn sym(
        name: &str,
        parent: Option<usize>,
        kind: SymbolKind,
        start: u32,
        end: u32,
    ) -> SymbolFact {
        SymbolFact {
            kind,
            name: name.into(),
            qualname: name.into(),
            parent,
            start,
            end,
            name_line: start,
        }
    }
    fn call(name: &str, q: Option<&str>, line: u32, enclosing: Option<usize>) -> RefFact {
        RefFact {
            kind: RefKind::Call,
            name: name.into(),
            qualifier: q.map(String::from),
            line,
            enclosing,
        }
    }
    fn imp(local: &str, imported: Option<&str>) -> ImportFact {
        ImportFact {
            local: local.into(),
            module: String::new(),
            imported: imported.map(String::from),
            line: 1,
        }
    }

    #[test]
    fn resolves_local_imported_module_and_self_calls() {
        // lib.py: def helper (1–20), class Bus (21–60) with method emit (30–60).
        let lib = BlobFacts {
            symbols: vec![
                sym("helper", None, SymbolKind::Function, 1, 20),
                sym("Bus", None, SymbolKind::Class, 21, 60),
                sym("emit", Some(1), SymbolKind::Method, 30, 60),
            ],
            ..Default::default()
        };
        // app.py: from lib import helper; import lib as m; class App (1–50) with run (10–30), stop (31–50).
        let app = BlobFacts {
            symbols: vec![
                sym("App", None, SymbolKind::Class, 1, 50),
                sym("run", Some(0), SymbolKind::Method, 10, 30),
                sym("stop", Some(0), SymbolKind::Method, 31, 50),
            ],
            imports: vec![
                imp("helper", Some("helper")),
                imp("m", None),
                imp("Bus", Some("Bus")),
            ],
            refs: vec![
                call("helper", None, 12, Some(1)),
                call("stop", Some("self"), 13, Some(1)),
                call("helper", Some("m"), 14, Some(1)),
                call("emit", Some("Bus"), 15, Some(1)),
                call("unknown", None, 16, Some(1)),
                call("print", Some("x"), 17, Some(1)),
            ],
            ..Default::default()
        };
        let files = vec![
            FileIn {
                path: "lib.py",
                lane: "code",
                lang: Some("py"),
                conv: TEST_CONV,
                facts: Some(&lib),
                imports: vec![],
                windows: vec![(1, 20, 0), (21, 60, 1)],
            },
            FileIn {
                path: "app.py",
                lane: "code",
                lang: Some("py"),
                conv: TEST_CONV,
                facts: Some(&app),
                imports: vec![Some(0), Some(0), Some(0)],
                windows: vec![(1, 30, 2), (31, 50, 3)],
            },
        ];
        let ls = links(&files);
        let got: Vec<(usize, u32, usize, &str)> = ls
            .iter()
            .map(|l| (l.from_window, l.from_line, l.to_window, l.label.as_str()))
            .collect();
        // helper is reached twice from window 2 (plain and via module): one link at the first callsite.
        assert_eq!(
            got,
            vec![(2, 12, 0, "helper"), (2, 15, 1, "emit"), (2, 13, 3, "stop")]
        );
        assert!(ls.iter().all(|l| l.confidence >= 0.85));
        // Typed dependencies: the three links plus the import windows (helper, module, Bus).
        let deps = conn::typed_deps(&files, &ls);
        let w = &deps.windows;
        assert!(w.contains(&(2, 0)) && w.contains(&(2, 1)) && w.contains(&(2, 3)));
        assert_eq!(deps.imports, vec![(1, 0)]);
    }

    fn file<'a>(path: &'a str, lang: &'a str, facts: &'a BlobFacts, w0: usize) -> FileIn<'a> {
        FileIn {
            path,
            lane: "code",
            lang: Some(lang),
            conv: TEST_CONV,
            facts: Some(facts),
            imports: vec![],
            windows: vec![(1, 100, w0)],
        }
    }

    #[test]
    fn builtin_calls_are_never_guessed() {
        let methods = BlobFacts {
            symbols: vec![
                sym("Log", None, SymbolKind::Class, 1, 50),
                sym("append", Some(0), SymbolKind::Method, 2, 20),
                sym("length", None, SymbolKind::Function, 30, 40),
            ],
            ..Default::default()
        };
        let caller = BlobFacts {
            symbols: vec![sym("main", None, SymbolKind::Function, 1, 100)],
            refs: vec![
                // `items.append(x)`: list.append, not Log.append.
                call("append", Some("items"), 5, Some(0)),
                // `os.length()` through a builtin module; `len(x)` a builtin function.
                call("length", Some("os.path"), 6, Some(0)),
                call("len", None, 7, Some(0)),
                // An unresolved `self.append()` may be inherited: still a candidate.
                call("append", Some("self"), 8, Some(0)),
            ],
            ..Default::default()
        };
        let files = vec![
            file("app/main.py", "py", &caller, 0),
            file("app/log.py", "py", &methods, 1),
        ];
        let lines: Vec<u32> = links(&files).iter().map(|l| l.from_line).collect();
        assert_eq!(lines, vec![8]);
    }

    #[test]
    fn ambiguous_refs_get_up_to_three_nearest_candidates_at_low_confidence() {
        let def = |name: &str| BlobFacts {
            symbols: vec![sym(name, None, SymbolKind::Function, 1, 10)],
            ..Default::default()
        };
        let method = |class: &str, name: &str| BlobFacts {
            symbols: vec![
                sym(class, None, SymbolKind::Class, 1, 50),
                sym(name, Some(0), SymbolKind::Method, 2, 20),
            ],
            ..Default::default()
        };
        // `render` is defined in 4 files; `publish` once; `flush` as a method of two classes;
        // `common` in 7 files (too ambiguous); `x` is too short.
        let caller = BlobFacts {
            symbols: vec![sym("main", None, SymbolKind::Function, 1, 100)],
            imports: vec![imp("bound", Some("bound"))],
            refs: vec![
                call("render", None, 5, Some(0)),
                call("publish", None, 6, Some(0)),
                call("flush", Some("out"), 7, Some(0)),
                call("common", None, 8, Some(0)),
                call("bound", None, 9, Some(0)),
                call("x", None, 10, Some(0)),
            ],
            ..Default::default()
        };
        let (r, p, fl, cm, b) = (
            def("render"),
            def("publish"),
            method("Out", "flush"),
            def("common"),
            def("bound"),
        );
        let mut files = vec![
            file("app/ui/main.py", "py", &caller, 0),
            file("app/ui/render.py", "py", &r, 1),
            file("app/other/render.py", "py", &r, 2),
            file("lib/render.py", "py", &r, 3),
            file("zzz/render.py", "py", &r, 4),
            file("web/render.ts", "ts", &r, 5), // another language: never a candidate
            file("app/bus.py", "py", &p, 6),
            file("app/io/out.py", "py", &fl, 7),
            file("lib/out2.py", "py", &fl, 8),
            file("bound.py", "py", &b, 9),
        ];
        for i in 0..7 {
            files.push(file(
                [
                    "c0.py", "c1.py", "c2.py", "c3.py", "c4.py", "c5.py", "c6.py",
                ][i],
                "py",
                &cm,
                10 + i,
            ));
        }
        files[0].imports = vec![None]; // `bound` is imported but unresolved: never guessed
        let ls = links(&files);
        let to = |line: u32| -> Vec<(usize, f64)> {
            ls.iter()
                .filter(|l| l.from_line == line)
                .map(|l| (l.to_window, l.confidence))
                .collect()
        };
        // 4 py definitions: the nearest 3 by shared directory, then path; 0.5 − 3 × 0.05.
        let mut got = to(5);
        got.sort_by_key(|a| a.0);
        assert_eq!(got, vec![(1, 0.35), (2, 0.35), (3, 0.35)]);
        assert_eq!(to(6), vec![(6, 0.5)]);
        let mut got = to(7);
        got.sort_by_key(|a| a.0);
        assert_eq!(got, vec![(7, 0.45), (8, 0.45)]);
        assert!(to(8).is_empty() && to(9).is_empty() && to(10).is_empty());
        assert!(
            ls.iter()
                .all(|l| l.confidence <= 0.5 && l.provenance == "tree-sitter")
        );
        // Candidates are not dependencies.
        assert!(conn::typed_deps(&files, &ls).windows.is_empty());
    }

    #[test]
    fn a_precise_link_beats_a_candidate_for_the_same_pair() {
        let lib = BlobFacts {
            symbols: vec![
                sym("helper", None, SymbolKind::Function, 1, 10),
                sym("Box", None, SymbolKind::Class, 11, 30),
                sym("helper2", Some(1), SymbolKind::Method, 12, 20),
            ],
            ..Default::default()
        };
        // Line 3 guesses Box.helper2 (window 1), line 4 calls helper via the import (window 1 too
        // since both sit in one window here).
        let app = BlobFacts {
            symbols: vec![sym("main", None, SymbolKind::Function, 1, 20)],
            imports: vec![imp("helper", Some("helper"))],
            refs: vec![
                call("helper2", Some("thing"), 3, Some(0)),
                call("helper", None, 4, Some(0)),
            ],
            ..Default::default()
        };
        let mut a = file("app.py", "py", &app, 0);
        a.imports = vec![Some(1)];
        let files = vec![a, file("lib.py", "py", &lib, 1)];
        let ls = links(&files);
        assert_eq!(ls.len(), 1);
        assert_eq!((ls[0].from_line, ls[0].confidence), (4, 0.85));
    }
}
