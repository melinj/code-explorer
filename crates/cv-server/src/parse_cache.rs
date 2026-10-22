//! B3 parse cache (D5: index.db). A file's facts are stored per (path, blob) with the pack id,
//! pack version ([`cv_lang::LanguagePack::version`]) and codec version; re-indexing reuses them
//! when all match and parses (in parallel) only the rest. The path is part of the key because a
//! pack's analysis may depend on it.
//!
//! The cache is off until [`enable`] (the `cv` binary: index.db at its XDG location, next to the
//! repo's other derived data) or [`enable_in`] (tests). `CV_PARSE_CACHE=off` turns it off.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use cv_lang::{BlobFacts, Registry, codec, split_lines};
use cv_store::{IndexDb, StorePaths};
use rayon::prelude::*;

#[derive(Clone)]
enum Location {
    /// index.db at the repo's XDG cache location.
    Default,
    /// index.db in this directory.
    Dir(PathBuf),
}

static LOCATION: RwLock<Option<Location>> = RwLock::new(None);
static DBS: Mutex<Option<HashMap<PathBuf, Arc<IndexDb>>>> = Mutex::new(None);

/// Caches parses in each repo's index.db (`~/.cache/code-viewer/<repo>/index.db`).
pub fn enable() {
    *LOCATION.write().unwrap() = Some(Location::Default);
}

/// Caches parses in `dir/index.db` for every repo (tests).
pub fn enable_in(dir: &Path) {
    *LOCATION.write().unwrap() = Some(Location::Dir(dir.to_path_buf()));
    DBS.lock().unwrap().take();
}

pub fn disable() {
    *LOCATION.write().unwrap() = None;
}

fn db(root: &Path) -> Option<Arc<IndexDb>> {
    if std::env::var("CV_PARSE_CACHE").is_ok_and(|v| v == "off") {
        return None;
    }
    index_db(root)
}

/// The repo's index.db at the configured location (the parse cache's and the saved views'),
/// opened once per process; `None` while caching is off ([`enable`] not called).
pub fn index_db(root: &Path) -> Option<Arc<IndexDb>> {
    let path = match LOCATION.read().unwrap().clone()? {
        Location::Default => StorePaths::for_repo(root).index_db,
        Location::Dir(d) => StorePaths::in_dir(&d).index_db,
    };
    let mut dbs = DBS.lock().unwrap();
    let dbs = dbs.get_or_insert_with(HashMap::new);
    if let Some(d) = dbs.get(&path) {
        return Some(d.clone());
    }
    match IndexDb::open(&path) {
        Ok(d) => {
            let d = Arc::new(d);
            dbs.insert(path, d.clone());
            Some(d)
        }
        Err(e) => {
            eprintln!(
                "cv: parse cache unavailable ({}: {e}); parsing everything",
                path.display()
            );
            None
        }
    }
}

/// One file to parse.
pub struct Item<'a> {
    pub path: &'a str,
    pub blob: &'a str,
    pub text: &'a str,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    pub hits: usize,
    pub parsed: usize,
}

pub(crate) fn unparsed(text: &str) -> BlobFacts {
    let n = split_lines(text).len();
    BlobFacts {
        lines: n as u32,
        tokens: vec![Vec::new(); n],
        ..Default::default()
    }
}

/// Facts of every item (pack index, facts), in order: cached when possible, else parsed by the
/// pack that claims the path. `tick` is called once per item handled (progress).
pub fn parse_all(
    reg: &Registry,
    root: &Path,
    items: &[Item],
    tick: &(dyn Fn() + Sync),
) -> (Vec<(Option<usize>, BlobFacts)>, Stats) {
    let packs: Vec<Option<(usize, String)>> = items
        .iter()
        .map(|it| {
            reg.pack_for(it.path)
                .map(|(i, p)| (i, format!("{}@{}#{}", p.id(), p.version(), codec::VERSION)))
        })
        .collect();
    let db = db(root);
    let cached: Vec<Option<Vec<u8>>> = match &db {
        Some(db) => {
            let keys: Vec<(&str, &str, &str)> = items
                .iter()
                .zip(&packs)
                .map(|(it, p)| (it.path, it.blob, p.as_ref().map_or("", |p| p.1.as_str())))
                .collect();
            db.parse_cache_get(&keys).unwrap_or_else(|e| {
                eprintln!("cv: parse cache read failed: {e}");
                vec![None; items.len()]
            })
        }
        None => vec![None; items.len()],
    };
    let out: Vec<(Option<usize>, BlobFacts, bool)> = items
        .par_iter()
        .zip(&packs)
        .zip(cached)
        .map(|((it, p), bytes)| {
            tick();
            let Some((pi, _)) = p else {
                return (None, unparsed(it.text), false);
            };
            if let Some(f) = bytes.as_deref().and_then(codec::decode) {
                return (Some(*pi), f, false);
            }
            (Some(*pi), reg.pack(*pi).analyze(it.path, it.text), true)
        })
        .collect();
    let mut stats = Stats::default();
    for (p, _, fresh) in &out {
        if *fresh {
            stats.parsed += 1;
        } else if p.is_some() {
            stats.hits += 1;
        }
    }
    if let Some(db) = &db
        && stats.parsed > 0
    {
        let encoded: Vec<(usize, Vec<u8>)> = out
            .par_iter()
            .enumerate()
            .filter(|(_, o)| o.2)
            .map(|(i, o)| (i, codec::encode(&o.1)))
            .collect();
        let rows: Vec<(&str, &str, &str, &[u8])> = encoded
            .iter()
            .map(|(i, b)| {
                let p = packs[*i].as_ref().map_or("", |p| p.1.as_str());
                (items[*i].path, items[*i].blob, p, b.as_slice())
            })
            .collect();
        if let Err(e) = db.parse_cache_put(&rows) {
            eprintln!("cv: parse cache write failed: {e}");
        }
        // Bound the cache to a few snapshots' worth of files.
        if items.len() > 1 {
            let keep: Vec<(&str, &str)> = items.iter().map(|it| (it.path, it.blob)).collect();
            let _ = db.parse_cache_trim(&keep, 4 * items.len() + 1000);
        }
    }
    (out.into_iter().map(|(p, f, _)| (p, f)).collect(), stats)
}

/// [`parse_all`] for one file.
pub fn parse_one(
    reg: &Registry,
    root: &Path,
    path: &str,
    blob: &str,
    text: &str,
) -> (Option<usize>, BlobFacts) {
    let (mut v, _) = parse_all(reg, root, &[Item { path, blob, text }], &|| {});
    v.pop().unwrap_or((None, unparsed(text)))
}

/// [`parse_one`] for committed blobs (`root` given); live overlay text (`root` None) is parsed
/// directly, as it is one-off.
pub fn parse_text(
    reg: &Registry,
    root: Option<&Path>,
    path: &str,
    blob: &str,
    text: &str,
) -> (Option<usize>, BlobFacts) {
    match root {
        Some(root) => parse_one(reg, root, path, blob, text),
        None => match reg.pack_for(path) {
            Some((i, p)) => (Some(i), p.analyze(path, text)),
            None => (None, unparsed(text)),
        },
    }
}
