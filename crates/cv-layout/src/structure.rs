//! How the folder tree fits the code (DESIGN §4.11): every directory seen as a module, with the
//! links that stay inside it and those that cross its boundary (data flow), and how its change
//! is spread over its files (heat). Feeds `GET /views/:id/structure`, MCP `get_structure` and the
//! Flow and Tension lenses.
//!
//! - **Links** are distinct window pairs between counted files; an import counts only when the two
//!   files share no other link (it is the same dependency, seen at file level). Links from or to
//!   the test and docs lanes are left out: tests reaching into a module say little about its shape.
//! - **Flow pressure** of a directory: boundary links / all links touching it. A good module has a
//!   narrow interface (few windows linked from outside) and long chains inside; a poor one is a
//!   spiderweb of links to many partners.
//! - **Gateway spread** of a directory: how its boundary links are spread over its files,
//!   (gateways − 1) / (files − 1). One file serving as the way in and out is cold (0); every
//!   file reaching outside is hot (1).
//! - **Heat tension**: share of the change mass in warm or hot files × share of the lines in cool
//!   or frozen files. High when a few busy files sit among code that rarely changes: candidates
//!   for a module of their own.
//! - **Divergence** from the district (change) layout: 1 − normalised mutual information between
//!   the directories and the dependency clusters, over windows.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use cv_contracts::readside::{
    FileStructure, FolderPull, FolderStructure, StructureResponse, StructureSummary,
    WindowStructure,
};

use crate::dirname;

/// Lanes whose links and files are left out.
pub const SKIP_LANES: [&str; 2] = ["test", "docs"];
/// Heat grades: warm and hot (≥), cool and frozen (≤).
const HOT: u8 = 3;
const COLD: u8 = 1;
/// A file's pull toward another directory needs at least this many links.
const MIN_PULL: u32 = 2;
const HOT_FILES: usize = 5;

pub struct FileIn<'a> {
    pub path: &'a str,
    pub lane: &'a str,
    /// File heat grade (0 frozen .. 4 hot) and change rate.
    pub heat: u8,
    pub rate: f64,
    pub lines: u32,
}

pub struct WinIn<'a> {
    pub key: &'a str,
    pub file: usize,
    /// The window's dependency cluster in the district layout, when known.
    pub module: Option<usize>,
}

pub struct LinkIn {
    pub from: usize,
    pub to: usize,
    pub import: bool,
}

/// Whether directory `d` is `of` or lies below it: a window's module is its directory with
/// everything below it, so a link into a subfolder stays inside and one to a sibling or a parent
/// leaves.
pub fn below(d: &str, of: &str) -> bool {
    d == of || of.is_empty() || d.starts_with(&format!("{of}/"))
}

/// Review priority's boundary factor (DESIGN §4.9 risk): 0.5 for code whose links all stay in its
/// module, rising to 1 for code whose links all leave it (changes there ripple further). Code
/// without counted links takes 0.5.
pub fn boundary_factor(inside: u32, outside: u32) -> f64 {
    let n = inside + outside;
    if n == 0 {
        0.5
    } else {
        0.5 + 0.5 * outside as f64 / n as f64
    }
}

fn round(x: f64) -> f64 {
    // `+ 0.0` turns -0.0 into 0.0.
    (x * 1000.0).round() / 1000.0 + 0.0
}

/// `dir` and its ancestors up to the root (""), innermost first.
fn ancestors(dir: &str) -> impl Iterator<Item = &str> {
    let mut cur = Some(dir);
    std::iter::from_fn(move || {
        let d = cur?;
        cur = (!d.is_empty()).then(|| dirname(d));
        Some(d)
    })
}

/// The deepest directory holding both.
fn common<'a>(a: &'a str, b: &str) -> &'a str {
    ancestors(a)
        .find(|d| d.is_empty() || b == *d || b.starts_with(&format!("{d}/")))
        .unwrap_or("")
}

/// Heat tension of a set of files: (share of change mass in hot files) × (share of lines in cold
/// files); 0 for fewer than two files or no change.
fn tension(files: &[&FileIn]) -> f64 {
    let mass: f64 = files.iter().map(|f| f.rate).sum();
    if files.len() < 2 || mass <= 0.0 {
        return 0.0;
    }
    let hot: f64 = files.iter().filter(|f| f.heat >= HOT).map(|f| f.rate).sum();
    let lines: f64 = files.iter().map(|f| f.lines as f64).sum();
    let cold: f64 = files
        .iter()
        .filter(|f| f.heat <= COLD)
        .map(|f| f.lines as f64)
        .sum();
    if lines <= 0.0 {
        return 0.0;
    }
    hot / mass * cold / lines
}

/// Longest chain of links among `members` (in windows), cutting cycles at their back edges.
fn longest_chain(members: &[usize], out: &HashMap<usize, Vec<usize>>) -> u32 {
    let local: HashMap<usize, usize> = members.iter().enumerate().map(|(i, &w)| (w, i)).collect();
    let n = members.len();
    let adj: Vec<Vec<usize>> = members
        .iter()
        .map(|w| {
            out.get(w)
                .into_iter()
                .flatten()
                .filter_map(|t| local.get(t).copied())
                .collect()
        })
        .collect();
    // Iterative DFS: post-order over the graph minus back edges; depth = 1 + deepest callee.
    let mut state = vec![0u8; n];
    let mut depth = vec![1u32; n];
    let mut best = 0;
    for s in 0..n {
        if state[s] != 0 {
            continue;
        }
        let mut stack = vec![(s, 0usize)];
        state[s] = 1;
        while let Some((v, i)) = stack.pop() {
            if i < adj[v].len() {
                stack.push((v, i + 1));
                let t = adj[v][i];
                if state[t] == 0 {
                    state[t] = 1;
                    stack.push((t, 0));
                }
            } else {
                state[v] = 2;
                let d = adj[v]
                    .iter()
                    .filter(|&&t| state[t] == 2 && t != v)
                    .map(|&t| depth[t] + 1)
                    .max()
                    .unwrap_or(1);
                depth[v] = d;
                best = best.max(d);
            }
        }
    }
    if n > 0 { best } else { 0 }
}

/// Normalised mutual information of two labelings of the same items.
fn nmi(pairs: &[(usize, usize)]) -> f64 {
    let n = pairs.len() as f64;
    if pairs.is_empty() {
        return 1.0;
    }
    let mut a: HashMap<usize, f64> = HashMap::new();
    let mut b: HashMap<usize, f64> = HashMap::new();
    let mut ab: HashMap<(usize, usize), f64> = HashMap::new();
    for &(x, y) in pairs {
        *a.entry(x).or_default() += 1.0;
        *b.entry(y).or_default() += 1.0;
        *ab.entry((x, y)).or_default() += 1.0;
    }
    let h = |m: &HashMap<usize, f64>| -> f64 {
        m.values()
            .map(|&c| {
                let p = c / n;
                -p * p.ln()
            })
            .sum()
    };
    let (ha, hb) = (h(&a), h(&b));
    if ha + hb <= 0.0 {
        return 1.0;
    }
    let i: f64 = ab
        .iter()
        .map(|(&(x, y), &c)| {
            let p = c / n;
            p * (p / (a[&x] / n * b[&y] / n)).ln()
        })
        .sum();
    (2.0 * i / (ha + hb)).clamp(0.0, 1.0)
}

#[derive(Default)]
struct Dir {
    files: Vec<usize>,
    windows: Vec<usize>,
    internal: u32,
    outgoing: u32,
    incoming: u32,
    interface: HashSet<usize>,
    partners: HashSet<usize>,
    gateways: HashSet<usize>,
}

pub fn analyse(files: &[FileIn], windows: &[WinIn], links: &[LinkIn]) -> StructureResponse {
    let counted = |f: usize| !SKIP_LANES.contains(&files[f].lane);
    let folder = |f: usize| dirname(files[f].path);

    // Distinct window links between counted files; imports only where nothing else links the pair.
    let file_pair = |a: usize, b: usize| (a.min(b), a.max(b));
    let mut linked: HashSet<(usize, usize)> = HashSet::new();
    for l in links.iter().filter(|l| !l.import) {
        linked.insert(file_pair(windows[l.from].file, windows[l.to].file));
    }
    let mut edges: BTreeSet<(usize, usize)> = BTreeSet::new();
    for l in links {
        let (fa, fb) = (windows[l.from].file, windows[l.to].file);
        if l.from == l.to || !counted(fa) || !counted(fb) {
            continue;
        }
        if l.import && (fa == fb || linked.contains(&file_pair(fa, fb))) {
            continue;
        }
        edges.insert((l.from, l.to));
    }

    // Directories: every ancestor of a counted file's directory.
    let mut dirs: BTreeMap<&str, Dir> = BTreeMap::new();
    let mut file_windows: Vec<Vec<usize>> = vec![Vec::new(); files.len()];
    for (w, win) in windows.iter().enumerate() {
        file_windows[win.file].push(w);
    }
    for f in (0..files.len()).filter(|&f| counted(f)) {
        for d in ancestors(folder(f)) {
            let e = dirs.entry(d).or_default();
            e.files.push(f);
            e.windows.extend_from_slice(&file_windows[f]);
        }
    }
    // Leaf folders get small ids (partners are counted as leaf folders).
    let leaf_ids: BTreeMap<&str, usize> = {
        let mut leaves: Vec<&str> = (0..files.len())
            .filter(|&f| counted(f))
            .map(folder)
            .collect();
        leaves.sort();
        leaves.dedup();
        leaves
            .into_iter()
            .enumerate()
            .map(|(i, d)| (d, i))
            .collect()
    };

    let mut out_adj: HashMap<usize, Vec<usize>> = HashMap::new();
    // Per file: internal links, external links per other folder, exported windows.
    let mut f_internal = vec![0u32; files.len()];
    let mut f_external: Vec<BTreeMap<&str, u32>> = vec![BTreeMap::new(); files.len()];
    let mut f_exports: Vec<HashSet<usize>> = vec![HashSet::new(); files.len()];
    // Per leaf folder (own files only): links inside and across.
    let mut own_inside: HashMap<&str, u32> = HashMap::new();
    let mut own_across: HashMap<&str, u32> = HashMap::new();
    let mut cross = 0u32;
    // Per window: links within its directory, across it, and of those coming in.
    let mut w_internal = vec![0u32; windows.len()];
    let mut w_external = vec![0u32; windows.len()];
    let mut w_incoming = vec![0u32; windows.len()];
    for &(a, b) in &edges {
        out_adj.entry(a).or_default().push(b);
        let (fa, fb) = (windows[a].file, windows[b].file);
        let (da, db) = (folder(fa), folder(fb));
        let lca = common(da, db);
        for d in ancestors(lca) {
            dirs.get_mut(d).expect("ancestor").internal += 1;
        }
        // Up from each end to (not including) the common directory: boundary crossings.
        for d in ancestors(da).take_while(|d| *d != lca) {
            let e = dirs.get_mut(d).expect("ancestor");
            e.outgoing += 1;
            e.partners.insert(leaf_ids[db]);
            e.gateways.insert(fa);
        }
        for d in ancestors(db).take_while(|d| *d != lca) {
            let e = dirs.get_mut(d).expect("ancestor");
            e.incoming += 1;
            e.interface.insert(b);
            e.partners.insert(leaf_ids[da]);
            e.gateways.insert(fb);
        }
        // A window's module is its directory with everything below it: a link into a subfolder
        // stays inside, one to a sibling or a parent leaves.
        if below(db, da) {
            w_internal[a] += 1;
        } else {
            w_external[a] += 1;
        }
        if below(da, db) {
            w_internal[b] += 1;
        } else {
            w_external[b] += 1;
            w_incoming[b] += 1;
        }
        if da == db {
            *own_inside.entry(da).or_default() += 1;
            if fa != fb {
                f_internal[fa] += 1;
                f_internal[fb] += 1;
            }
        } else {
            cross += 1;
            *own_across.entry(da).or_default() += 1;
            *own_across.entry(db).or_default() += 1;
            *f_external[fa].entry(db).or_default() += 1;
            *f_external[fb].entry(da).or_default() += 1;
            f_exports[fb].insert(b);
        }
    }

    // Dependency clusters (district layout) per window, for divergence and spread.
    let module_pairs: Vec<(usize, usize)> = windows
        .iter()
        .filter(|w| counted(w.file))
        .filter_map(|w| Some((leaf_ids[folder(w.file)], w.module?)))
        .collect();
    let known_modules = !module_pairs.is_empty();
    let misgrouped = known_modules.then(|| {
        let mut by_module: HashMap<usize, HashMap<usize, u32>> = HashMap::new();
        for &(d, m) in &module_pairs {
            *by_module.entry(m).or_default().entry(d).or_default() += 1;
        }
        let off: u32 = by_module
            .values()
            .map(|ds| ds.values().sum::<u32>() - ds.values().max().copied().unwrap_or(0))
            .sum();
        round(off as f64 / module_pairs.len() as f64)
    });

    let mut folders_out = Vec::new();
    for (path, d) in &dirs {
        if path.is_empty() {
            continue;
        }
        let fs: Vec<&FileIn> = d.files.iter().map(|&f| &files[f]).collect();
        let t = tension(&fs);
        let boundary = d.outgoing + d.incoming;
        let touching = d.internal + boundary;
        let mut hot: Vec<&FileIn> = if t > 0.0 {
            fs.iter().copied().filter(|f| f.heat >= HOT).collect()
        } else {
            Vec::new()
        };
        hot.sort_by(|a, b| b.rate.total_cmp(&a.rate).then(a.path.cmp(b.path)));
        let modules: BTreeSet<usize> = d
            .windows
            .iter()
            .filter_map(|&w| windows[w].module)
            .collect();
        folders_out.push(FolderStructure {
            path: path.to_string(),
            depth: path.matches('/').count() as u32 + 1,
            files: d.files.len() as u32,
            windows: d.windows.len() as u32,
            internal: d.internal,
            outgoing: d.outgoing,
            incoming: d.incoming,
            pressure: if touching > 0 {
                round(boundary as f64 / touching as f64)
            } else {
                0.0
            },
            inside: if touching > 0 {
                round(d.internal as f64 / touching as f64)
            } else {
                0.0
            },
            interface: d.interface.len() as u32,
            interface_share: round(d.interface.len() as f64 / d.windows.len().max(1) as f64),
            partners: d.partners.len() as u32,
            gateways: d.gateways.len() as u32,
            spread: if d.gateways.len() > 1 && d.files.len() > 1 {
                round((d.gateways.len() - 1) as f64 / (d.files.len() - 1) as f64)
            } else {
                0.0
            },
            chain: longest_chain(&d.windows, &out_adj),
            modules: known_modules.then_some(modules.len() as u32),
            mass: round(fs.iter().map(|f| f.rate).sum()),
            tension: round(t),
            hot_files: hot
                .into_iter()
                .take(HOT_FILES)
                .map(|f| f.path.to_string())
                .collect(),
        });
    }

    // Files: tension against their siblings, and their pull toward another directory.
    let mut by_folder: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for f in (0..files.len()).filter(|&f| counted(f)) {
        by_folder.entry(folder(f)).or_default().push(f);
    }
    let mut files_out = Vec::new();
    let mut heat_num = 0.0;
    let mut heat_den = 0.0;
    let mut press_num = 0.0;
    let mut press_den = 0.0;
    for (dir, fs) in &by_folder {
        let own: Vec<&FileIn> = fs.iter().map(|&f| &files[f]).collect();
        let mass: f64 = own.iter().map(|f| f.rate).sum();
        heat_num += tension(&own) * mass;
        heat_den += mass;
        let (inside, across) = (
            own_inside.get(dir).copied().unwrap_or(0),
            own_across.get(dir).copied().unwrap_or(0),
        );
        let n_win: usize = fs.iter().map(|&f| file_windows[f].len()).sum();
        if inside + across > 0 {
            press_num += across as f64 / (inside + across) as f64 * n_win as f64;
        }
        press_den += n_win as f64;
        for &f in fs {
            let file = &files[f];
            let external: u32 = f_external[f].values().sum();
            let siblings: Vec<&FileIn> =
                fs.iter().filter(|&&g| g != f).map(|&g| &files[g]).collect();
            let t = if file.heat >= HOT && !siblings.is_empty() {
                let lines: f64 = siblings.iter().map(|s| s.lines as f64).sum();
                let cold: f64 = siblings
                    .iter()
                    .filter(|s| s.heat <= COLD)
                    .map(|s| s.lines as f64)
                    .sum();
                let weight = if file.heat >= 4 { 1.0 } else { 0.6 };
                if lines > 0.0 {
                    cold / lines * weight
                } else {
                    0.0
                }
            } else {
                0.0
            };
            if f_internal[f] + external == 0 && file.rate <= 0.0 {
                continue;
            }
            // A file alone in its directory has nothing to be pulled away from.
            let pull = f_external[f]
                .iter()
                .filter(|_| !siblings.is_empty())
                .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
                .filter(|(_, n)| **n >= MIN_PULL && **n > f_internal[f])
                .map(|(d, n)| FolderPull {
                    folder: d.to_string(),
                    links: *n,
                });
            files_out.push(FileStructure {
                path: file.path.to_string(),
                folder: dir.to_string(),
                lane: file.lane.to_string(),
                heat: file.heat,
                rate: file.rate,
                internal: f_internal[f],
                external,
                exports: f_exports[f].len() as u32,
                tension: round(t),
                pull,
            });
        }
    }
    files_out.sort_by(|a, b| a.path.cmp(&b.path));
    let file_tension: HashMap<&str, f64> = files_out
        .iter()
        .map(|f| (f.path.as_str(), f.tension))
        .collect();
    let windows_out: Vec<WindowStructure> = windows
        .iter()
        .enumerate()
        .filter(|(_, w)| counted(w.file))
        .filter_map(|(i, w)| {
            let path = files[w.file].path;
            let tension = file_tension.get(path).copied().unwrap_or(0.0);
            (w_internal[i] + w_external[i] > 0 || tension > 0.0).then(|| WindowStructure {
                key: w.key.to_string(),
                path: path.to_string(),
                internal: w_internal[i],
                external: w_external[i],
                incoming: w_incoming[i],
                tension,
            })
        })
        .collect();

    let n_links = edges.len() as u32;
    StructureResponse {
        summary: StructureSummary {
            folders: by_folder.len() as u32,
            files: by_folder.values().map(Vec::len).sum::<usize>() as u32,
            links: n_links,
            cross_folder_links: cross,
            link_locality: if n_links > 0 {
                round(1.0 - cross as f64 / n_links as f64)
            } else {
                1.0
            },
            divergence: known_modules.then(|| round(1.0 - nmi(&module_pairs))),
            misgrouped,
            heat_tension: if heat_den > 0.0 {
                round(heat_num / heat_den)
            } else {
                0.0
            },
            pressure: if press_den > 0.0 {
                round(press_num / press_den)
            } else {
                0.0
            },
        },
        folders: folders_out,
        files: files_out,
        windows: windows_out,
    }
}
