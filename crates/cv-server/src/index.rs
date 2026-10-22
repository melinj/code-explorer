//! Snapshot indexing: tree → lanes → parse → imports → windows → links and connectedness → churn →
//! layout → the in-memory view the routes serve.
//!
//! Storage for this pass is in memory (no `index.db` yet): one [`View`] per resolved snapshot,
//! holding headers, links, file text with tokens, base silhouettes and a grid index over base tiles.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use cv_contracts::*;
use cv_lang::chunk::{self, WindowSpec};
use cv_lang::lanes::{self, LaneClassifier, LaneConfig, LaneMeta};
use cv_lang::{BlobFacts, PathIndex, Registry, hash, silhouette, split_lines};
use cv_vcs::{CommitInfo, JjRepo};
use rayon::prelude::*;
use serde::Serialize;

/// Files larger than this are shown as "other" (not parsed, not drawn).
pub(crate) const MAX_FILE_BYTES: usize = 1 << 20;
/// The lane config of a revision: its `.cv/lanes.toml` (read from the commit's tree, not the
/// working directory) over the defaults. Problems are logged and fall back (see [`LaneConfig`]).
pub fn lane_config(repo: &JjRepo, commit: &str) -> LaneConfig {
    let text = match repo.blob_at(commit, lanes::CONFIG_PATH) {
        Ok(Some(blob)) => match repo.read_blob(lanes::CONFIG_PATH, &blob) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) => {
                eprintln!(
                    "warning: {}: {e}; using the default lanes",
                    lanes::CONFIG_PATH
                );
                return LaneConfig::default();
            }
        },
        _ => return LaneConfig::default(),
    };
    let cfg = LaneConfig::parse(&text);
    for w in &cfg.warnings {
        eprintln!(
            "warning: {} at {}: {w}",
            lanes::CONFIG_PATH,
            &commit[..commit.len().min(12)]
        );
    }
    cfg
}

/// LaneDefs of some lanes, `order` = position in the slice.
pub fn lane_defs(lanes: &[LaneMeta]) -> Vec<LaneDef> {
    lanes
        .iter()
        .enumerate()
        .map(|(i, l)| LaneDef {
            id: l.id.into(),
            label: l.label.into(),
            order: i as u32,
            texture: match l.texture {
                "hatch" => Texture::Hatch,
                "dots" => Texture::Dots,
                "lines" => Texture::Lines,
                "cross" => Texture::Cross,
                _ => Texture::Plain,
            },
            collapsed: l.collapsed,
            hidden: l.hidden,
        })
        .collect()
}
const HEAT_LABELS: [&str; 5] = ["Frozen", "Cool", "Mild", "Warm", "Hot"];

type ProgressFn = dyn Fn(&'static str, u32, u32) + Send + Sync;
static PROGRESS: std::sync::RwLock<Option<std::sync::Arc<ProgressFn>>> =
    std::sync::RwLock::new(None);

/// Report index phases (job, done, total) while views build: `cv serve` turns them into
/// `index.progress` events. One server per process, so the hook is process-wide.
pub fn on_progress(f: impl Fn(&'static str, u32, u32) + Send + Sync + 'static) {
    *PROGRESS.write().unwrap() = Some(std::sync::Arc::new(f));
}

fn progress(job: &'static str, done: usize, total: usize) {
    let f = PROGRESS.read().unwrap().clone();
    if let Some(f) = f {
        f(job, done as u32, total as u32);
    }
}

pub fn registry() -> Registry {
    let mut r = Registry::new();
    r.register(Box::new(cv_lang_python::PythonPack));
    r.register(Box::new(cv_lang_typescript::TypeScriptPack));
    r.register(Box::new(cv_lang_config::ConfigPack));
    r.register(Box::new(cv_lang_rust::RustPack::default()));
    r.register(Box::new(cv_lang_cpp::CppPack::default()));
    r
}

#[derive(Clone)]
pub struct FileRec {
    pub path: String,
    pub blob: String,
    pub lang: &'static str,
    pub lane: &'static str,
    pub text: String,
    /// Byte offset of each line start; `line_starts.len()` = line count.
    pub line_starts: Vec<usize>,
    pub tokens: Vec<Vec<u32>>,
    /// OutlineEncoding base records, 3 bytes per line.
    pub silhouette: Vec<u8>,
    /// (line, depth, label) of each symbol's name line.
    pub headings: Vec<(u32, u32, String)>,
    pub windows: Vec<usize>,
}

impl FileRec {
    pub fn line_count(&self) -> u32 {
        self.line_starts.len() as u32
    }

    /// Line `n` (1-based) without its newline.
    pub fn line(&self, n: u32) -> &str {
        let i = (n - 1) as usize;
        let s = self.line_starts[i];
        let e = self
            .line_starts
            .get(i + 1)
            .map(|&e| e - 1)
            .unwrap_or(self.text.len());
        let l = &self.text[s..e.max(s)];
        let l = l.strip_suffix('\n').unwrap_or(l);
        l.strip_suffix('\r').unwrap_or(l)
    }
}

#[derive(Clone)]
pub struct Win {
    pub header: WindowHeader,
    pub file: usize,
    pub out_links: Vec<usize>,
    pub in_links: Vec<usize>,
}

#[derive(Clone)]
pub struct LinkRec {
    pub id: String,
    pub kind: &'static str,
    pub from: usize,
    pub from_line: u32,
    pub to: usize,
    pub to_line: u32,
    pub confidence: f64,
    /// Qualified name of the target.
    pub label: String,
    /// Channel links (bus, http, env, deploy, config): port text and dynamic target.
    pub channel: Option<Box<crate::channels::ChanInfo>>,
}

/// Whether the map draws a link as uncertain (faded, with "?"): below 0.6, or for config-field
/// links below cv-channel-config's SOLID_SCORE (web edges.ts `isUncertain`). Highways leave these
/// out, so a bundle counts only links the map draws solid.
pub fn is_uncertain(kind: &str, confidence: f64) -> bool {
    let config = matches!(kind, "config" | "define" | "read" | "deploy");
    confidence
        < if config {
            cv_channel_config::SOLID_SCORE
        } else {
            0.6
        }
}

impl LinkRec {
    /// A runtime-only target (D11): drawn as an open stub, never as an edge.
    pub fn is_dynamic(&self) -> bool {
        self.channel.as_ref().is_some_and(|c| c.dynamic.is_some())
    }
}

/// The served ViewBootstrap (`base`, whose `churn` is always set) and the churn summary as a
/// typed field for the server's own use (not serialized twice).
#[derive(Serialize, Clone)]
pub struct BootstrapOut {
    #[serde(flatten)]
    pub base: ViewBootstrap,
    #[serde(skip)]
    pub churn: ChurnSummary,
}

#[derive(Clone)]
pub struct View {
    pub bootstrap: BootstrapOut,
    pub windows: Vec<Win>,
    pub files: Vec<FileRec>,
    pub links: Vec<LinkRec>,
    pub by_id: HashMap<String, usize>,
    pub by_blob: HashMap<String, usize>,
    pub by_path: HashMap<String, usize>,
    /// Base tiles (TILE_BASE_WU) → windows and links whose ports fall in them.
    pub tiles: HashMap<(i64, i64), (Vec<usize>, Vec<usize>)>,
    pub timings: Vec<(&'static str, Duration)>,
    /// Text of blobs an edit replaced (blob → file record), still served by id: blobs are immutable.
    pub retired: HashMap<String, std::sync::Arc<FileRec>>,
    /// Renames the churn walk saw: old path → path at this view's head (carries window pins).
    pub renames: std::sync::Arc<HashMap<String, String>>,
}

impl View {
    pub fn id(&self) -> &str {
        &self.bootstrap.base.view.id
    }

    pub fn link_points(&self, l: &LinkRec) -> ([f64; 2], [f64; 2]) {
        link_points(&self.windows, l)
    }
}

/// World positions of a link's out port (right edge at the callsite) and in port (left edge at the definition).
pub fn link_points(windows: &[Win], l: &LinkRec) -> ([f64; 2], [f64; 2]) {
    let f = &windows[l.from].header;
    let t = &windows[l.to].header;
    let fy = f.rect.y + HEADER_WU + (l.from_line - f.lines.start) as f64 * LINE_WU + LINE_WU / 2.0;
    let ty = t.rect.y + HEADER_WU + (l.to_line - t.lines.start) as f64 * LINE_WU + LINE_WU / 2.0;
    ([f.rect.x + f.rect.w, fy], [t.rect.x, ty])
}

pub fn view_id(spec: &serde_json::Value, commit: &str, op: &str) -> String {
    format!(
        "v_{:012x}",
        hash(&format!("{spec}|{commit}|{op}")) & 0xffff_ffff_ffff
    )
}

#[derive(Clone, Copy)]
pub struct Options {
    pub max_commits: usize,
}

struct Loaded {
    path: String,
    blob: String,
    lane: &'static str,
    text: String,
}

pub(crate) fn iso_time(ms: i64) -> String {
    // Civil-from-days (Howard Hinnant), UTC.
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

pub(crate) fn line_starts(text: &str) -> Vec<usize> {
    let mut v = vec![0];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' && i + 1 < text.len() {
            v.push(i + 1);
        }
    }
    v
}

/// Window width in world units from the longest line it shows: 60–110 columns, in steps of 10.
pub(crate) fn window_width(file: &FileRec, start: u32, end: u32) -> f64 {
    let mut longest = 0usize;
    for n in start..=end {
        longest = longest.max(file.line(n).chars().count());
    }
    let cols = (longest + 4).div_ceil(10) * 10;
    cols.clamp(60, 110) as f64 * COL_WU
}

/// The first SCIP index found at `commit` ([`cv_link::scip::PATHS`]), decoded. A file that does
/// not decode is skipped with a warning.
pub fn scip_index(repo: &JjRepo, commit: &str) -> Option<(&'static str, cv_link::scip::Index)> {
    for path in cv_link::scip::PATHS {
        let Ok(Some(blob)) = repo.blob_at(commit, path) else {
            continue;
        };
        let bytes = match repo.read_blob(path, &blob) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("warning: {path}: {e}");
                continue;
            }
        };
        match cv_link::scip::decode(&bytes) {
            Ok(ix) => return Some((path, ix)),
            Err(e) => eprintln!("warning: {path}: not a SCIP index ({e}); skipped"),
        }
    }
    None
}

pub fn build_view(
    repo: &JjRepo,
    reg: &Registry,
    spec: serde_json::Value,
    commit: &CommitInfo,
    opts: Options,
) -> Result<View, String> {
    build_view_pinned(repo, reg, spec, commit, opts, &[])
}

/// Base tiles (TILE_BASE_WU) → windows intersecting them and links with a port in them.
pub fn grid_index(
    windows: &[Win],
    links: &[LinkRec],
) -> HashMap<(i64, i64), (Vec<usize>, Vec<usize>)> {
    let mut tiles: HashMap<(i64, i64), (Vec<usize>, Vec<usize>)> = HashMap::new();
    let cell = |v: f64| (v / TILE_BASE_WU).floor() as i64;
    for (i, w) in windows.iter().enumerate() {
        let r = &w.header.rect;
        for tx in cell(r.x)..=cell(r.x + r.w) {
            for ty in cell(r.y)..=cell(r.y + r.h) {
                tiles.entry((tx, ty)).or_default().0.push(i);
            }
        }
    }
    for (li, l) in links.iter().enumerate() {
        if l.is_dynamic() {
            continue;
        }
        let (a, b) = link_points(windows, l);
        let (ca, cb) = ((cell(a[0]), cell(a[1])), (cell(b[0]), cell(b[1])));
        tiles.entry(ca).or_default().1.push(li);
        if ca != cb {
            tiles.entry(cb).or_default().1.push(li);
        }
    }
    tiles
}

/// [`build_view`] with pinned windows (by `WindowKey`) held at their world positions.
/// The history walk (DESIGN §4.7): change-mass grades per file at `head`, the renames it saw
/// (old path → path at `head`) and how long it took.
pub fn churn(
    repo: &JjRepo,
    head: &str,
    max_commits: usize,
    mass_cuts: [f64; 3],
) -> Result<(cv_churn::ChurnResult, HashMap<String, String>, Duration), String> {
    let t = Instant::now();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let hist = repo.history(head).map_err(|e| e.to_string())?;
    let limits = cv_churn::Limits {
        max_commits,
        mass_cuts,
        ..Default::default()
    };
    // Lazy: the grader stops pulling (and diffing) once its grades settle or a limit hits.
    let mut renames = cv_churn::Renames::default();
    let touches = hist.map_while(|c| c.ok()).map(|c| {
        renames.record(&c.renames);
        cv_churn::CommitTouch {
            time_ms: c.committer_time_ms,
            files: c.files,
            renames: c.renames,
        }
    });
    let res = cv_churn::grade(touches, now, limits);
    Ok((res, renames.into_map(), t.elapsed()))
}

pub fn build_view_pinned(
    repo: &JjRepo,
    reg: &Registry,
    spec: serde_json::Value,
    commit: &CommitInfo,
    opts: Options,
    pins: &[cv_layout::Pin],
) -> Result<View, String> {
    let t_all = Instant::now();
    let mut timings: Vec<(&'static str, Duration)> = Vec::new();
    let op = repo.op_id();
    let vid = view_id(&spec, &commit.commit, &op);
    let lane_cfg = lane_config(repo, &commit.commit);
    let classifier = LaneClassifier::with_config(reg, &lane_cfg);
    let view_lanes = lane_cfg.view_lanes();
    let grade_cfg = crate::grades::grade_config(repo, &commit.commit);

    // Churn runs on its own thread while the tree is read and parsed.
    let (churn, loaded, t_read) = std::thread::scope(|s| {
        let churn = s.spawn(|| churn(repo, &commit.commit, opts.max_commits, grade_cfg.mass_cuts));
        let t = Instant::now();
        let mut entries = repo.list_tree(&commit.commit).map_err(|e| e.to_string())?;
        // ViewSpec.fileset: only the files it matches are indexed (model.ts).
        let fileset = crate::routes::spec_fileset(&spec);
        entries.retain(|e| fileset.matches(&e.path));
        progress("sync", 0, entries.len());
        let loaded: Vec<Loaded> = entries
            .par_iter()
            .filter_map(|e| {
                if lane_cfg.is_hidden(classifier.classify(&e.path, b"")) {
                    return None;
                }
                let bytes = repo.read_blob(&e.path, &e.blob).ok()?;
                if bytes.len() > MAX_FILE_BYTES || bytes[..bytes.len().min(8000)].contains(&0) {
                    return None;
                }
                let lane = classifier.classify(&e.path, &bytes);
                if !view_lanes.contains(&lane) {
                    return None;
                }
                Some(Loaded {
                    path: e.path.clone(),
                    blob: e.blob.clone(),
                    lane,
                    text: String::from_utf8_lossy(&bytes).into_owned(),
                })
            })
            .collect();
        let t_read = t.elapsed();
        let churn = churn
            .join()
            .map_err(|_| "churn thread panicked".to_string())??;
        Ok::<_, String>((churn, loaded, t_read))
    })?;
    let (churn, renames, t_churn) = churn;
    timings.push(("read tree + blobs", t_read));
    progress("churn", 1, 1);

    // Parse, reporting every 500 files.
    let parsed_n = std::sync::atomic::AtomicUsize::new(0);
    progress("parse", 0, loaded.len());
    // Parse (B3: facts cached per blob × pack version in index.db, see crate::parse_cache).
    let t = Instant::now();
    let items: Vec<crate::parse_cache::Item> = loaded
        .iter()
        .map(|f| crate::parse_cache::Item {
            path: &f.path,
            blob: &f.blob,
            text: &f.text,
        })
        .collect();
    let tick = || {
        let n = parsed_n.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        if n.is_multiple_of(500) {
            progress("parse", n, loaded.len());
        }
    };
    let (parsed, parse_stats) = crate::parse_cache::parse_all(reg, repo.root(), &items, &tick);
    drop(items);
    if parse_stats.hits > 0 {
        eprintln!(
            "  parse cache: {} reused, {} parsed",
            parse_stats.hits, parse_stats.parsed
        );
    }
    timings.push(("parse", t.elapsed()));
    progress("parse", loaded.len(), loaded.len());
    progress("link", 0, 1);

    // Imports.
    let t = Instant::now();
    let by_path: HashMap<String, usize> = loaded
        .iter()
        .enumerate()
        .map(|(i, f)| (f.path.clone(), i))
        .collect();
    let project = PathIndex::new(loaded.iter().map(|f| f.path.as_str()), |p| {
        by_path.get(p).map(|&i| loaded[i].text.clone())
    });
    let resolved: Vec<Vec<Option<usize>>> = loaded
        .par_iter()
        .zip(&parsed)
        .map(|(f, (pack, facts))| match pack {
            Some(pi) => facts
                .imports
                .iter()
                .map(|imp| {
                    reg.pack(*pi)
                        .resolve_import(&f.path, imp, &project)
                        .and_then(|p| by_path.get(&p).copied())
                })
                .collect(),
            None => Vec::new(),
        })
        .collect();
    timings.push(("resolve imports", t.elapsed()));

    // Files and windows.
    let t = Instant::now();
    let mut files: Vec<FileRec> = Vec::with_capacity(loaded.len());
    let mut specs: Vec<Vec<WindowSpec>> = Vec::with_capacity(loaded.len());
    for (f, (pack, facts)) in loaded.into_iter().zip(&parsed) {
        let starts = line_starts(&f.text);
        let lines: Vec<&str> = split_lines(&f.text);
        let mut tokens = facts.tokens.clone();
        tokens.resize(starts.len(), Vec::new());
        let sil = silhouette::records(&lines, &tokens);
        let depth = |mut p: Option<usize>| {
            let mut d = 0;
            while let Some(i) = p {
                d += 1;
                p = facts.symbols[i].parent;
            }
            d
        };
        let headings = facts
            .symbols
            .iter()
            .filter(|s| (s.name_line as usize) <= lines.len())
            .map(|s| {
                let label: String = lines[(s.name_line - 1) as usize]
                    .trim()
                    .chars()
                    .take(120)
                    .collect();
                (s.name_line, depth(s.parent), label)
            })
            .collect();
        let mut ws = chunk::chunk(starts.len() as u32, &facts.symbols);
        chunk::assign_keys(&mut ws, &facts.symbols);
        specs.push(ws);
        let lang = pack.map(|i| reg.pack(i).lang(&f.path)).unwrap_or("other");
        files.push(FileRec {
            path: f.path,
            blob: f.blob,
            lang,
            lane: f.lane,
            text: f.text,
            line_starts: starts,
            tokens,
            silhouette: sil,
            headings,
            windows: Vec::new(),
        });
    }
    let mut win_specs: Vec<(usize, WindowSpec)> = Vec::new();
    for (fi, ws) in specs.into_iter().enumerate() {
        for w in ws {
            files[fi].windows.push(win_specs.len());
            win_specs.push((fi, w));
        }
    }
    timings.push(("windows", t.elapsed()));

    // Links and connectedness.
    let t = Instant::now();
    let link_in: Vec<cv_link::FileIn> = files
        .iter()
        .enumerate()
        .map(|(fi, f)| cv_link::FileIn {
            path: &f.path,
            lane: f.lane,
            lang: parsed[fi].0.map(|i| reg.pack(i).id()),
            conv: parsed[fi]
                .0
                .map(|i| reg.pack(i).conventions())
                .unwrap_or_default(),
            facts: Some(&parsed[fi].1),
            imports: resolved[fi].clone(),
            windows: f
                .windows
                .iter()
                .map(|&w| (win_specs[w].1.start, win_specs[w].1.end, w))
                .collect(),
        })
        .collect();
    let mut link_out = cv_link::links(&link_in);
    // Tier 2: a SCIP index at this revision adds precise links (DESIGN §4.5).
    if let Some((path, ix)) = scip_index(repo, &commit.commit) {
        let scip = cv_link::scip::links(&ix, &link_in);
        eprintln!(
            "  scip: {path}: {} documents, {} links",
            ix.documents.len(),
            scip.len()
        );
        let covered = cv_link::scip::covered_lines(&ix, &link_in);
        link_out = cv_link::scip::merge(link_out, scip, &covered, &link_in);
    }
    let t_chan = Instant::now();
    let chan = crate::channels::build(reg, &files, &parsed, &resolved, &win_specs);
    let t_chan_total = t_chan.elapsed();
    timings.push(("channels", t_chan_total));
    for n in &chan.notes {
        eprintln!("  {n}");
    }
    // Connectedness over typed links (§4.8, D7): code links and imports, plus confident channel
    // links (consumer depends on producer).
    let mut deps = cv_link::conn::typed_deps(&link_in, &link_out);
    deps.windows.extend(chan.window_dependencies());
    drop(link_in);
    let win_lanes: Vec<&str> = win_specs.iter().map(|(fi, _)| files[*fi].lane).collect();
    let win_file: Vec<usize> = win_specs.iter().map(|(fi, _)| *fi).collect();
    let conn = cv_link::conn::window_connectedness(&win_lanes, &win_file, &deps, grade_cfg.conn);
    drop(deps);
    timings.push(("links + connectedness", t.elapsed() - t_chan_total));
    progress("link", 1, 1);
    progress("layout", 0, 1);
    timings.push(("churn (parallel)", t_churn));

    // Layout.
    let t = Instant::now();
    let lane_index = |lane: &str| view_lanes.iter().position(|l| *l == lane).unwrap_or(0);
    let layout_files: Vec<cv_layout::FileIn> = files
        .iter()
        .enumerate()
        .map(|(fi, f)| cv_layout::FileIn {
            path: f.path.clone(),
            lane: lane_index(f.lane),
            entry: parsed[fi].0.is_some_and(|i| {
                reg.pack(i)
                    .conventions()
                    .is_entry(cv_lang::file_stem(&f.path))
            }),
        })
        .collect();
    let heat_of = |path: &str| churn.files.get(path).cloned().unwrap_or_default();
    let layout_windows: Vec<cv_layout::WinIn> = win_specs
        .iter()
        .enumerate()
        .map(|(wi, (fi, w))| {
            let f = &files[*fi];
            let h = heat_of(&f.path);
            cv_layout::WinIn {
                file: *fi,
                w: window_width(f, w.start, w.end),
                h: HEADER_WU + (w.end - w.start + 1) as f64 * LINE_WU,
                start_line: w.start,
                heat: h.grade,
                rate: h.rate,
                conn: conn[wi].grade,
                dependents2: conn[wi].dependents2,
                key: format!("{}#{}", f.path, w.key),
                settled: h.settled,
            }
        })
        .collect();
    let file_wins: Vec<Vec<(u32, u32, usize)>> = files
        .iter()
        .map(|f| {
            f.windows
                .iter()
                .map(|&w| (win_specs[w].1.start, win_specs[w].1.end, w))
                .collect()
        })
        .collect();
    let all_facts: Vec<&BlobFacts> = parsed.iter().map(|p| &p.1).collect();
    // Lexical candidates (≤ 0.5) are drawn but do not pull windows together.
    let confident: Vec<cv_link::LinkOut> = link_out
        .iter()
        .filter(|l| l.confidence >= cv_link::conn::MIN_DEP_CONFIDENCE)
        .cloned()
        .collect();
    let mut layout_links =
        crate::layout_input::links(&confident, &all_facts, &resolved, &file_wins);
    drop(confident);
    // Channel links (§4.10 weight 1.5): confident, resolved ones only; stubs have no far end.
    layout_links.extend(
        chan.links
            .iter()
            .filter(|d| d.info.dynamic.is_none() && d.confidence >= 0.8 && d.from != d.to)
            .map(|d| cv_layout::LinkIn {
                from: d.from,
                to: d.to,
                kind: cv_layout::EdgeKind::Channel,
            }),
    );
    let path_ix: HashMap<&str, usize> = files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.path.as_str(), i))
        .collect();
    let co_change = crate::layout_input::co_change(&churn.co_change, &path_ix);
    let repo_key = repo.root().display().to_string();
    let prev = crate::layout_input::last_placement(&repo_key);
    let lo = cv_layout::layout(&cv_layout::Input {
        lanes: &view_lanes,
        files: &layout_files,
        windows: &layout_windows,
        links: &layout_links,
        heat_bands: churn.bands,
        co_change: &co_change,
        pins,
        prev: prev.as_deref(),
        full: false,
        grade_cuts: cv_layout::GradeCuts {
            mass: grade_cfg.mass_cuts,
            rank: grade_cfg.conn.rank_cuts(),
        },
    });
    crate::layout_input::remember_placement(&repo_key, lo.placement.clone());
    timings.push(("layout", t.elapsed()));
    progress("layout", 1, 1);

    // Headers.
    let t = Instant::now();
    let mut seen_ids: HashMap<String, usize> = HashMap::new();
    let mut windows: Vec<Win> = Vec::with_capacity(win_specs.len());
    for (wi, (fi, w)) in win_specs.iter().enumerate() {
        let f = &files[*fi];
        let key = format!("{}#{}", f.path, w.key);
        let mut id = format!("w{:012x}", hash(&format!("{vid}|{key}")) & 0xffff_ffff_ffff);
        while seen_ids.contains_key(&id) {
            id.push('_');
        }
        seen_ids.insert(id.clone(), wi);
        let h = heat_of(&f.path);
        let c = &conn[wi];
        let symbol = w.symbol.map(|s| {
            let s = &parsed[*fi].1.symbols[s];
            SymbolRef {
                name: s.name.clone(),
                qualname: s.qualname.clone(),
                kind: s.kind.as_str().into(),
            }
        });
        windows.push(Win {
            header: WindowHeader {
                id,
                key,
                kind: w.kind.into(),
                cluster: format!("c{}", lo.cluster_of[wi]),
                lane: f.lane.into(),
                path: f.path.clone(),
                lang: f.lang.into(),
                rev: commit.commit.clone(),
                change: commit.change.clone(),
                blob: f.blob.clone(),
                symbol,
                lines: LineSpan {
                    start: w.start,
                    end: w.end,
                    file_lines: f.line_count(),
                },
                rect: lo.rects[wi],
                heat: Heat {
                    grade: h.grade,
                    rate: h.rate,
                    ci: h.ci,
                    settled: h.settled,
                    touches: h.touches,
                    in_range: None,
                },
                conn: Connectedness {
                    grade: c.grade,
                    dependents2: c.dependents2,
                    fan_in: c.fan_in,
                    fan_out: c.fan_out,
                    tests: c.tests,
                    reach: c.reach,
                    cycle: (c.cycle > 1).then_some(c.cycle),
                },
                risk: lo.risk[wi],
                review_first: lo.review_first[wi],
                fragment: None,
                counts: WindowCounts {
                    open_threads: 0,
                    ports: 0,
                    conflicts: 0,
                },
                flags: WindowFlags {
                    pinned: false,
                    truncated: false,
                },
                diff_stat: None,
            },
            file: *fi,
            out_links: Vec::new(),
            in_links: Vec::new(),
        });
    }
    for f in &files {
        if f.windows.len() > 1 {
            let siblings: Vec<String> = f
                .windows
                .iter()
                .map(|&w| windows[w].header.id.clone())
                .collect();
            for (i, &w) in f.windows.iter().enumerate() {
                windows[w].header.fragment = Some(FileFragment {
                    index: i as u32 + 1,
                    of: f.windows.len() as u32,
                    chip: chunk::chip_slot(&f.path),
                    siblings: siblings.clone(),
                });
            }
        }
    }
    let mut links: Vec<LinkRec> = link_out
        .into_iter()
        .enumerate()
        .map(|(i, l)| LinkRec {
            id: format!("l{i}"),
            kind: l.kind.as_str(),
            from: l.from_window,
            from_line: l.from_line,
            to: l.to_window,
            to_line: l.to_line,
            confidence: l.confidence,
            channel: (l.provenance != "tree-sitter").then(|| {
                Box::new(crate::channels::ChanInfo {
                    port: format!("→ {}", l.label),
                    dynamic: None,
                    provenance: l.provenance.to_string(),
                })
            }),
            label: l.label,
        })
        .collect();
    let n_code = links.len();
    links.extend(chan.link_recs(n_code));
    for (li, l) in links.iter().enumerate() {
        windows[l.from].out_links.push(li);
        if !l.is_dynamic() {
            windows[l.to].in_links.push(li);
        }
    }
    for w in windows.iter_mut() {
        let mut lines: Vec<(bool, u32)> = w
            .out_links
            .iter()
            .map(|&l| (true, links[l].from_line))
            .chain(w.in_links.iter().map(|&l| (false, links[l].to_line)))
            .collect();
        lines.sort_unstable();
        lines.dedup();
        w.header.counts.ports = lines.len() as u32;
    }

    // Bootstrap.
    let clusters: Vec<Cluster> = lo
        .clusters
        .iter()
        .enumerate()
        .map(|(ci, c)| Cluster {
            id: format!("c{ci}"),
            parent: c.parent.map(|s| format!("s{s}")),
            lane: view_lanes[c.lane].into(),
            label: c.label.clone(),
            label_source: c.label_source.clone(),
            summary: None,
            rect: c.rect,
            heat: c.heat,
            conn: c.conn,
            windows: c.members.len() as u32,
            open_threads: 0,
            conflicts: 0,
            pinned: false,
            heat_level: Some((c.heat_level * 1000.0).round() / 1000.0),
            review: Some((c.review * 1000.0).round() / 1000.0),
        })
        .collect();
    let mut bundle_map: BTreeMap<(usize, usize), EdgeBundle> = BTreeMap::new();
    for l in &links {
        let (a, b) = (lo.cluster_of[l.from], lo.cluster_of[l.to]);
        if a == b || is_uncertain(l.kind, l.confidence) {
            continue;
        }
        let e = bundle_map.entry((a, b)).or_insert_with(|| EdgeBundle {
            from: format!("c{a}"),
            to: format!("c{b}"),
            kinds: BTreeMap::new(),
            weight: 0.0,
        });
        *e.kinds.entry(l.kind.to_string()).or_insert(0) += 1;
        e.weight += 1.0;
    }
    let shown: Vec<LaneMeta> = lane_cfg
        .lanes
        .iter()
        .filter(|l| !l.hidden)
        .copied()
        .collect();
    let lane_defs = lane_defs(&shown);
    let n_clusters = clusters.len() as u32;
    let hubs = crate::channels::hubs(&chan, &files, &windows, &lo.lanes, lane_index);
    let churn = ChurnSummary {
        head: commit.commit.clone(),
        scanned_commits: churn.scanned as u32,
        oldest: churn.oldest_ms.map(iso_time).unwrap_or_default(),
        settled_mass: churn.settled_mass,
        stop: churn.stop.as_str().into(),
        half_life_days: cv_churn::HALF_LIFE_DAYS,
        bands: churn.bands,
        top_rate: churn.files.values().map(|f| f.rate).reduce(f64::max),
    };
    let bootstrap = BootstrapOut {
        base: ViewBootstrap {
            view: ViewRef {
                id: vid.clone(),
                spec,
                resolved: Resolved {
                    op,
                    head: commit.commit.clone(),
                    commits: vec![commit.commit.clone()],
                },
                status: "ready".into(),
                layout_version: 1,
                stats: ViewStats {
                    windows: windows.len() as u32,
                    files: files.len() as u32,
                    clusters: n_clusters,
                },
                warnings: vec![],
                shared_placement: None,
            },
            world: lo.world,
            lane_defs,
            lanes: lo.lanes,
            columns: lo.columns,
            clusters,
            hubs,
            bundles: bundle_map.into_values().collect(),
            churn: Some(churn.clone()),
            heat_labels: HEAT_LABELS.map(String::from),
        },
        churn,
    };

    // Grid index over base tiles.
    let tiles = grid_index(&windows, &links);
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
    let by_path = files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.path.clone(), i))
        .collect();
    // Later incremental refreshes re-derive links from these facts (crate::incremental).
    crate::incremental::remember_facts(
        repo.root(),
        files
            .iter()
            .map(|f| (f.path.clone(), f.blob.clone()))
            .zip(parsed),
    );
    timings.push(("headers + tiles", t.elapsed()));
    timings.push(("total", t_all.elapsed()));
    Ok(View {
        bootstrap,
        windows,
        files,
        links,
        by_id,
        by_blob,
        by_path,
        tiles,
        timings,
        retired: HashMap::new(),
        renames: std::sync::Arc::new(renames),
    })
}
