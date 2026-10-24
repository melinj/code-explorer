//! Saved snapshot views (D5, B2): every fresh snapshot build is written to index.db (windows with
//! their R*Tree, links, clusters, files, the bootstrap, the layout placement and the FTS search
//! rows), and `cv serve` loads it back instead of indexing again when the spec, operation, head
//! commit and build inputs all match.
//!
//! - **What counts as the same build** ([`inputs_key`]): the `cv` executable (size and mtime: any
//!   code change, packs included), the pack versions, `--max-commits`, the window pins fed to the
//!   layout and the env switches that change indexing. Lanes, grades and channel rules come from
//!   the commit's tree, so the head commit covers them. Only builds that started from no previous
//!   placement are saved and loaded (what a fresh process computes); heat is as of the save and a
//!   saved view older than [`MAX_AGE`] is indexed again.
//! - **Loading** reads the file list, re-reads each blob's text by id, takes facts from the parse
//!   cache (tokens, silhouettes, headings and the incremental facts are re-derived from them) and
//!   takes headers, links, clusters, hubs and placement from the tables; the grid index is rebuilt.
//! - **Writing** happens on a background thread (`cv index` waits for it), so a build is not slowed
//!   down by it; a crash mid-write leaves the previous rows (one transaction).
//! - **Search** (`search_fts`): rows per view and head commit. Written with the view, and after the
//!   in-memory fallback indexed the symbols of a revision that has none (a refreshed view).
//! - **Tiles:** the in-memory grid stays the hot path (DESIGN §9 measures both); the R*Tree serves
//!   tiles of a saved view that is not loaded (a view id from before a restart), see [`disk_tile`].
//!
//! `CV_VIEW_CACHE=off` turns loading and saving off (the parse cache has its own switch).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use cv_contracts::{Rect, TILE_BASE_WU, TileResponse};
use cv_lang::Registry;
use cv_store::{IndexDb, SearchRow, StoredFile, StoredLink, StoredView};
use cv_vcs::{CommitInfo, JjRepo};
use rayon::prelude::*;

use crate::index::{self, BootstrapOut, FileRec, LinkRec, View, Win};
use crate::readside::SymbolEntry;

/// Saved views kept per repo (the newest).
pub const KEEP_VIEWS: usize = 6;
/// A saved view older than this is indexed again (heat decays with time).
pub const MAX_AGE: Duration = Duration::from_secs(24 * 3600);

static WAIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `cv index`: always index (never load a saved view) and write the result before returning.
pub fn set_wait_for_save(on: bool) {
    WAIT.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn wait_for_save() -> bool {
    WAIT.load(std::sync::atomic::Ordering::Relaxed)
}

fn enabled() -> bool {
    !std::env::var("CV_VIEW_CACHE").is_ok_and(|v| v == "off")
}

/// The repo's index.db (shared with the parse cache), when caching is on.
pub fn db(root: &Path) -> Option<Arc<IndexDb>> {
    if !enabled() {
        return None;
    }
    crate::parse_cache::index_db(root)
}

/// The `cv` executable's identity: any rebuild changes it.
fn build_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let meta = std::env::current_exe().and_then(std::fs::metadata);
        match meta {
            Ok(m) => {
                let mtime = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                format!("{}:{}:{}", env!("CARGO_PKG_VERSION"), m.len(), mtime)
            }
            Err(_) => format!("{}:unknown", env!("CARGO_PKG_VERSION")),
        }
    })
}

/// Hash of what a snapshot build reads beyond spec, op and commit.
pub fn inputs_key(reg: &Registry, opts: index::Options, pins: &[cv_layout::Pin]) -> String {
    let mut s = String::new();
    s.push_str(build_id());
    for p in reg.packs() {
        s.push_str(&format!("|{}@{}", p.id(), p.version()));
    }
    s.push_str(&format!("|codec{}", cv_lang::codec::VERSION));
    s.push_str(&format!("|max{}", opts.max_commits));
    for var in ["CV_CHANNELS", "CV_CONFIG_SCORING", "CV_LANE_SAMPLE"] {
        s.push_str(&format!(
            "|{var}={}",
            std::env::var(var).unwrap_or_default()
        ));
    }
    let mut pins: Vec<String> = pins
        .iter()
        .map(|p| format!("{}={:?},{:?}", p.key, p.x, p.y))
        .collect();
    pins.sort();
    s.push_str("|pins:");
    s.push_str(&pins.join(";"));
    format!("{:016x}", cv_store::fnv64(s.as_bytes()))
}

/// `&'static str` for kinds, langs and lanes read back from the database (a small fixed set).
fn intern(s: &str) -> &'static str {
    static SET: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut set = SET.get_or_init(Default::default).lock().unwrap();
    if let Some(&x) = set.get(s) {
        return x;
    }
    let x: &'static str = Box::leak(s.to_string().into_boxed_str());
    set.insert(x);
    x
}

fn age(created_at: &str) -> Option<Duration> {
    // "YYYY-MM-DDTHH:MM:SS.mmmZ" → ms since the epoch (UTC).
    let p = |a: usize, b: usize| created_at.get(a..b)?.parse::<i64>().ok();
    let (y, m, d) = (p(0, 4)?, p(5, 7)?, p(8, 10)?);
    let (hh, mm, ss) = (p(11, 13)?, p(14, 16)?, p(17, 19)?);
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let ms = ((days * 86_400 + hh * 3600 + mm * 60 + ss) * 1000) as u128;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis();
    Some(Duration::from_millis(now.saturating_sub(ms) as u64))
}

/// The snapshot view for `spec` at `commit`: loaded from index.db when a matching build was saved,
/// else built (and saved in the background). `wait` blocks until the save is written (`cv index`).
pub fn load_or_build(
    repo: &JjRepo,
    reg: &Registry,
    spec: serde_json::Value,
    commit: &CommitInfo,
    opts: index::Options,
    pins: &[cv_layout::Pin],
    wait: bool,
) -> Result<View, String> {
    let repo_key = repo.root().display().to_string();
    // Only a layout that starts from nothing is what a fresh process computes.
    let fresh = crate::layout_input::last_placement(&repo_key).is_none();
    let db = if fresh { db(repo.root()) } else { None };
    let inputs = inputs_key(reg, opts, pins);
    if let Some(db) = &db
        && !wait
    {
        let found = db
            .find_view(&spec.to_string(), &repo.op_id(), &commit.commit, &inputs)
            .ok()
            .flatten();
        if let Some(id) = found {
            let fresh_enough = db
                .view_created_at(&id)
                .ok()
                .flatten()
                .and_then(|t| age(&t))
                .is_some_and(|a| a < MAX_AGE);
            if fresh_enough {
                match load(db, &id, repo, reg) {
                    Ok(v) => return Ok(v),
                    Err(e) => eprintln!("cv: saved view {id} unusable ({e}); indexing"),
                }
            }
        }
    }
    let v = index::build_view_pinned(repo, reg, spec, commit, opts, pins)?;
    if let Some(db) = db {
        let t = Instant::now();
        let placement = crate::layout_input::last_placement(&repo_key)
            .and_then(|p| serde_json::to_string(&*p).ok());
        let stored = to_stored(&v, inputs, placement);
        let root = repo.root().to_path_buf();
        let save = move || {
            let t = Instant::now();
            // Its own connection (WAL): reads of the shared one go on while this writes.
            let db = db
                .path()
                .and_then(|p| IndexDb::open(p).ok())
                .map(Arc::new)
                .unwrap_or(db);
            let id = stored.bootstrap.view.id.clone();
            let head = stored.bootstrap.view.resolved.head.clone();
            match db.put_view(&stored, KEEP_VIEWS) {
                Ok(()) => {
                    let rows = search_rows_from_facts(&root, &stored);
                    if let Err(e) = db.put_search(&id, &format!("{id}|{head}"), &rows) {
                        eprintln!("cv: saving search rows: {e}");
                    }
                    if std::env::var_os("CV_DEBUG_VIEW_CACHE").is_some() {
                        eprintln!("cv: saved view {id} in {:.1?}", t.elapsed());
                    }
                }
                Err(e) => eprintln!("cv: saving view {id} to index.db: {e}"),
            }
        };
        if std::env::var_os("CV_DEBUG_VIEW_CACHE").is_some() {
            eprintln!("cv: view snapshot for index.db in {:.1?}", t.elapsed());
        }
        if wait {
            save();
        } else {
            std::thread::spawn(save);
        }
    }
    Ok(v)
}

/// What [`load`] needs back of a built view.
pub fn to_stored(v: &View, inputs: String, placement_json: Option<String>) -> StoredView {
    StoredView {
        inputs,
        bootstrap: v.bootstrap.base.clone(),
        files: v
            .files
            .iter()
            .map(|f| StoredFile {
                path: f.path.clone(),
                blob: f.blob.clone(),
                lang: f.lang.to_string(),
                lane: f.lane.to_string(),
                lines: f.line_count(),
            })
            .collect(),
        windows: v.windows.iter().map(|w| w.header.clone()).collect(),
        links: v
            .links
            .iter()
            .map(|l| StoredLink {
                id: l.id.clone(),
                kind: l.kind.to_string(),
                from: l.from,
                from_line: l.from_line,
                to: l.to,
                to_line: l.to_line,
                confidence: l.confidence,
                label: l.label.clone(),
                port: l.channel.as_ref().map(|c| c.port.clone()),
                dynamic: l
                    .channel
                    .as_ref()
                    .and_then(|c| c.dynamic.as_ref())
                    .map(|d| d.to_string()),
                provenance: l
                    .channel
                    .as_ref()
                    .map(|c| c.provenance.clone())
                    .unwrap_or_else(|| "tree-sitter".into()),
            })
            .collect(),
        renames_json: serde_json::to_string(&*v.renames).unwrap_or_else(|_| "{}".into()),
        placement_json,
    }
}

/// Loads saved view `id` (see the module docs).
pub fn load(db: &IndexDb, id: &str, repo: &JjRepo, reg: &Registry) -> Result<View, String> {
    let t_all = Instant::now();
    let t = Instant::now();
    let sv = db
        .load_view(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no saved view {id}"))?;
    let t_rows = t.elapsed();
    let t = Instant::now();
    let texts: Vec<String> = sv
        .files
        .par_iter()
        .map(|f| {
            repo.read_blob(&f.path, &f.blob)
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .map_err(|e| format!("{}: {e}", f.path))
        })
        .collect::<Result<_, String>>()?;
    let t_read = t.elapsed();
    let t = Instant::now();
    let items: Vec<crate::parse_cache::Item> = sv
        .files
        .iter()
        .zip(&texts)
        .map(|(f, text)| crate::parse_cache::Item {
            path: &f.path,
            blob: &f.blob,
            text,
        })
        .collect();
    let (parsed, stats) = crate::parse_cache::parse_all(reg, repo.root(), &items, &|| {});
    drop(items);
    let t_facts = t.elapsed();
    let t = Instant::now();
    let mut files: Vec<FileRec> = sv
        .files
        .par_iter()
        .zip(texts)
        .zip(&parsed)
        .map(|((f, text), (pack, facts))| {
            let lang = pack
                .map(|i| reg.pack(i).lang(&f.path))
                .unwrap_or_else(|| intern(&f.lang));
            crate::incremental::file_record(
                f.path.clone(),
                f.blob.clone(),
                lang,
                intern(&f.lane),
                text,
                facts,
            )
        })
        .collect();
    let by_path: HashMap<String, usize> = files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.path.clone(), i))
        .collect();
    let mut windows: Vec<Win> = Vec::with_capacity(sv.windows.len());
    for (wi, header) in sv.windows.into_iter().enumerate() {
        let file = *by_path
            .get(&header.path)
            .ok_or_else(|| format!("window {} names an unknown file", header.id))?;
        files[file].windows.push(wi);
        windows.push(Win {
            header,
            file,
            out_links: Vec::new(),
            in_links: Vec::new(),
        });
    }
    let links: Vec<LinkRec> = sv
        .links
        .into_iter()
        .map(|l| {
            let channel = l.port.map(|port| {
                Box::new(crate::channels::ChanInfo {
                    port,
                    dynamic: l
                        .dynamic
                        .as_deref()
                        .and_then(|d| serde_json::from_str(d).ok()),
                    provenance: l.provenance.clone(),
                })
            });
            LinkRec {
                id: l.id,
                kind: intern(&l.kind),
                from: l.from,
                from_line: l.from_line,
                to: l.to,
                to_line: l.to_line,
                confidence: l.confidence,
                label: l.label,
                channel,
            }
        })
        .collect();
    for (li, l) in links.iter().enumerate() {
        windows[l.from].out_links.push(li);
        if !l.is_dynamic() {
            windows[l.to].in_links.push(li);
        }
    }
    let tiles = index::grid_index(&windows, &links);
    let by_id = windows
        .iter()
        .enumerate()
        .map(|(i, w)| (w.header.id.clone(), i))
        .collect();
    let by_blob = files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.blob.clone(), i))
        .collect();
    let churn = sv
        .bootstrap
        .churn
        .clone()
        .ok_or_else(|| "saved view without churn".to_string())?;
    let renames: HashMap<String, String> =
        serde_json::from_str(&sv.renames_json).map_err(|e| e.to_string())?;
    // As a build leaves it: facts for later refreshes, the placement for the next layout.
    crate::incremental::remember_facts(
        repo.root(),
        files
            .iter()
            .map(|f| (f.path.clone(), f.blob.clone()))
            .zip(parsed),
    );
    if let Some(p) = sv
        .placement_json
        .as_deref()
        .and_then(|p| serde_json::from_str::<cv_layout::Placement>(p).ok())
    {
        crate::layout_input::remember_placement(&repo.root().display().to_string(), p);
    }
    let t_rebuild = t.elapsed();
    eprintln!(
        "  loaded saved view {id} from index.db ({} reused parses, {} parsed)",
        stats.hits, stats.parsed
    );
    Ok(View {
        bootstrap: BootstrapOut {
            base: sv.bootstrap,
            churn,
        },
        windows,
        files,
        links,
        by_id,
        by_blob,
        by_path,
        tiles,
        timings: vec![
            ("index.db rows", t_rows),
            ("read blobs", t_read),
            ("facts (parse cache)", t_facts),
            ("files + grid", t_rebuild),
            ("total", t_all.elapsed()),
        ],
        retired: HashMap::new(),
        renames: Arc::new(renames),
        expansions: HashMap::new(),
    })
}

// ─── Search rows ─────────────────────────────────────────────────────────────

/// FTS scope of a view at its current revision.
pub fn search_scope(v: &View) -> String {
    format!("{}|{}", v.id(), v.bootstrap.base.view.resolved.head)
}

fn symbol_row(window_id: &str, name: &str, qualname: &str, kind: &str, line: u32) -> SearchRow {
    SearchRow {
        kind: "symbol".into(),
        label: qualname.to_string(),
        detail: name.to_string(),
        reference: format!("{window_id}\t{line}\t{kind}"),
    }
}

fn file_row(path: &str, ord: usize) -> SearchRow {
    SearchRow {
        kind: "file".into(),
        label: path.to_string(),
        detail: String::new(),
        reference: ord.to_string(),
    }
}

/// Search rows of a saved view from the facts its build just remembered (the same symbols, in
/// the same order, as `readside::symbol_index`).
fn search_rows_from_facts(root: &Path, sv: &StoredView) -> Vec<SearchRow> {
    let keys: Vec<(String, String)> = sv
        .files
        .iter()
        .map(|f| (f.path.clone(), f.blob.clone()))
        .collect();
    let facts = crate::incremental::remembered_facts(root, &keys);
    // Windows of each path, in view order.
    let mut by_path: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, w) in sv.windows.iter().enumerate() {
        by_path.entry(w.path.as_str()).or_default().push(i);
    }
    let mut rows = Vec::new();
    for (f, facts) in sv.files.iter().zip(&facts) {
        let Some(facts) = facts else { continue };
        let wins = by_path
            .get(f.path.as_str())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        for s in &facts.symbols {
            let Some(&w) = wins.iter().find(|&&w| {
                let l = &sv.windows[w].lines;
                l.start <= s.name_line && s.name_line <= l.end
            }) else {
                continue;
            };
            rows.push(symbol_row(
                &sv.windows[w].id,
                &s.name,
                &s.qualname,
                s.kind.as_str(),
                s.name_line,
            ));
        }
    }
    rows.extend(
        sv.files
            .iter()
            .enumerate()
            .map(|(i, f)| file_row(&f.path, i)),
    );
    rows
}

/// Writes the search rows of `v` at its revision from an in-memory symbol index, in the
/// background (a view refreshed to a revision that has none yet).
pub fn index_search(root: &Path, v: &Arc<View>, symbols: &Arc<Vec<SymbolEntry>>) {
    let Some(db) = db(root) else { return };
    let (v, symbols) = (v.clone(), symbols.clone());
    std::thread::spawn(move || {
        let scope = search_scope(&v);
        if db.has_search(&scope).unwrap_or(true) {
            return;
        }
        let mut rows: Vec<SearchRow> = symbols
            .iter()
            .map(|s| {
                symbol_row(
                    &v.windows[s.window].header.id,
                    &s.name,
                    &s.qualname,
                    &s.kind,
                    s.line,
                )
            })
            .collect();
        rows.extend(
            v.files
                .iter()
                .enumerate()
                .map(|(i, f)| file_row(&f.path, i)),
        );
        if let Err(e) = db.put_search(v.id(), &scope, &rows) {
            eprintln!("cv: saving search rows: {e}");
        }
    });
}

// ─── Disk tiles ──────────────────────────────────────────────────────────────

/// A tile of a saved view that is not loaded (e.g. a view id from before a restart), straight
/// from index.db: windows from the R*Tree, edges whose port lies in the tile. Same content as
/// `routes::tile_response` of the loaded view (windows and edges in view order).
pub fn disk_tile(root: &Path, view: &str, z: u32, x: i64, y: i64) -> Option<TileResponse> {
    let db = db(root)?;
    let span = 1i64 << z;
    let rect = Rect {
        x: (x * span) as f64 * TILE_BASE_WU,
        y: (y * span) as f64 * TILE_BASE_WU,
        w: span as f64 * TILE_BASE_WU,
        h: span as f64 * TILE_BASE_WU,
    };
    let layout_version = db.view_layout_version(view).ok()??;
    let headers = db.window_headers_in_rect(view, &rect).ok()?;
    let edges = db.tile_edges(view, &rect).ok()?;
    Some(TileResponse {
        layout_version,
        windows: headers.into_iter().map(|(_, h)| h).collect(),
        edges,
    })
}

/// The bootstrap of a saved view that is not loaded (an id from before a restart), as its tiles
/// are served by [`disk_tile`]: the same id keeps working until the client re-POSTs its spec.
pub fn disk_bootstrap(root: &Path, view: &str) -> Option<cv_contracts::ViewBootstrap> {
    db(root)?.load_bootstrap(view).ok()?
}
