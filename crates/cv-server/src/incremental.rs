//! Incremental view updates for added and deleted files (DESIGN §4.1): after a jj operation an
//! open snapshot view gains windows for new files and loses the windows of deleted ones, without
//! waiting for a full index.
//!
//! - **Deleted files:** their windows and every link touching them leave the view; hub sites in
//!   them are dropped; the old text stays readable by blob id (`View::retired`).
//! - **New files** (in a lane the view shows): parsed (through the parse cache), chunked into
//!   windows and placed next to their directory: in the cluster of the nearest same-lane file in
//!   the same or a parent directory, in a free spot inside or just outside its frame that keeps
//!   the frame clear of other clusters; else in a new cluster to the right of the world in their
//!   lane. Their outgoing code links resolve through their imports (lexical tier). The same
//!   placement draws never-snapshotted files in live overlays (crate::workspaces).
//! - **Everything derived from the link graph** (links into new files, re-resolved refs,
//!   connectedness, then channel links and heat in the background): see "Complete incremental
//!   updates" below. Only the positions a full layout would choose wait for the next full index.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use cv_contracts::*;
use cv_lang::chunk;
use cv_lang::{BlobFacts, PathIndex, Registry, hash, silhouette, split_lines};
use cv_vcs::CommitInfo;
use rayon::prelude::*;

use crate::index::{FileRec, LinkRec, View, Win};

/// Space kept between a placed window and its neighbours (wu).
const GAP: f64 = 120.0;

/// A file of the new tree that the view does not have yet.
pub struct NewFile {
    pub path: String,
    pub blob: String,
    pub lane: &'static str,
    pub text: String,
}

/// Builds the file record (text, tokens, silhouette, headings) for parsed text.
pub fn file_record(
    path: String,
    blob: String,
    lang: &'static str,
    lane: &'static str,
    text: String,
    facts: &BlobFacts,
) -> FileRec {
    let starts = crate::index::line_starts(&text);
    let lines = split_lines(&text);
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
        .filter(|s| s.name_line >= 1 && (s.name_line as usize) <= lines.len())
        .map(|s| {
            let label: String = lines[(s.name_line - 1) as usize]
                .trim()
                .chars()
                .take(120)
                .collect();
            (s.name_line, depth(s.parent), label)
        })
        .collect();
    drop(lines);
    FileRec {
        path,
        blob,
        lang,
        lane,
        text,
        line_starts: starts,
        tokens,
        silhouette: sil,
        headings,
        windows: Vec::new(),
    }
}

/// Removes the files at `gone` (file indices) with their windows and links. Returns the ids of
/// the removed windows.
pub fn remove_files(v: &mut View, gone: &HashSet<usize>) -> Vec<String> {
    if gone.is_empty() {
        return vec![];
    }
    let removed: HashSet<usize> = gone
        .iter()
        .flat_map(|&f| v.files[f].windows.iter().copied())
        .collect();
    let removed_ids: Vec<String> = removed
        .iter()
        .map(|&w| v.windows[w].header.id.clone())
        .collect();
    // Window and file index maps.
    let mut wmap = vec![usize::MAX; v.windows.len()];
    let mut next = 0;
    for (i, m) in wmap.iter_mut().enumerate() {
        if !removed.contains(&i) {
            *m = next;
            next += 1;
        }
    }
    let mut fmap = vec![usize::MAX; v.files.len()];
    let mut next = 0;
    for (i, m) in fmap.iter_mut().enumerate() {
        if !gone.contains(&i) {
            *m = next;
            next += 1;
        }
    }
    let old_windows = std::mem::take(&mut v.windows);
    v.windows = old_windows
        .into_iter()
        .enumerate()
        .filter(|(i, _)| wmap[*i] != usize::MAX)
        .map(|(_, mut w)| {
            w.file = fmap[w.file];
            w
        })
        .collect();
    v.links
        .retain(|l| wmap[l.from] != usize::MAX && wmap[l.to] != usize::MAX);
    for l in v.links.iter_mut() {
        l.from = wmap[l.from];
        l.to = wmap[l.to];
    }
    let old_files = std::mem::take(&mut v.files);
    for (i, mut f) in old_files.into_iter().enumerate() {
        if fmap[i] == usize::MAX {
            v.retired.insert(f.blob.clone(), std::sync::Arc::new(f));
            continue;
        }
        f.windows = f.windows.iter().map(|&w| wmap[w]).collect();
        v.files.push(f);
    }
    let gone_ids: HashSet<&str> = removed_ids.iter().map(String::as_str).collect();
    for h in v.bootstrap.base.hubs.iter_mut() {
        drop_sites(h, &gone_ids);
    }
    relink(v);
    removed_ids
}

/// Drops hub sites in removed windows (and their anchors, which run parallel to the sites).
fn drop_sites(h: &mut Hub, gone: &HashSet<&str>) {
    let keep = |s: &SiteRef| !s.window.as_deref().is_some_and(|w| gone.contains(w));
    let flags: Vec<bool> = h.producers.iter().chain(&h.consumers).map(keep).collect();
    if flags.iter().all(|k| *k) {
        return;
    }
    if let Some(a) = h.anchors.as_mut()
        && a.len() == flags.len()
    {
        let mut it = flags.iter();
        a.retain(|_| *it.next().unwrap_or(&true));
    }
    h.producers.retain(keep);
    h.consumers.retain(keep);
}

/// Recomputes everything derived from windows and links: link lists, port counts, cluster window
/// counts, bundles, id/path/blob maps, the tile index and the stats.
pub fn relink(v: &mut View) {
    relink_without_tiles(v);
    v.tiles = crate::index::grid_index(&v.windows, &v.links);
}

/// [`relink`] but the tile index, for callers that rebuild it once at the end
/// ([`crate::review_api::finish_refresh`]).
pub fn relink_without_tiles(v: &mut View) {
    for w in v.windows.iter_mut() {
        w.out_links.clear();
        w.in_links.clear();
    }
    for (li, l) in v.links.iter().enumerate() {
        v.windows[l.from].out_links.push(li);
        if !l.is_dynamic() {
            v.windows[l.to].in_links.push(li);
        }
    }
    let links = &v.links;
    for w in v.windows.iter_mut() {
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
    let mut per_cluster: HashMap<&str, u32> = HashMap::new();
    for w in &v.windows {
        *per_cluster.entry(w.header.cluster.as_str()).or_default() += 1;
    }
    for c in v.bootstrap.base.clusters.iter_mut() {
        c.windows = per_cluster.get(c.id.as_str()).copied().unwrap_or(0);
    }
    let mut bundles: BTreeMap<(&str, &str), EdgeBundle> = BTreeMap::new();
    for l in &v.links {
        let (a, b) = (
            v.windows[l.from].header.cluster.as_str(),
            v.windows[l.to].header.cluster.as_str(),
        );
        if a == b {
            continue;
        }
        let e = bundles.entry((a, b)).or_insert_with(|| EdgeBundle {
            from: a.to_string(),
            to: b.to_string(),
            kinds: BTreeMap::new(),
            weight: 0.0,
        });
        *e.kinds.entry(l.kind.to_string()).or_insert(0) += 1;
        e.weight += 1.0;
    }
    v.bootstrap.base.bundles = bundles.into_values().collect();
    v.by_id = v
        .windows
        .iter()
        .enumerate()
        .map(|(i, w)| (w.header.id.clone(), i))
        .collect();
    v.by_path = v
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.path.clone(), i))
        .collect();
    v.by_blob = v
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.blob.clone(), i))
        .collect();
    let stats = &mut v.bootstrap.base.view.stats;
    stats.windows = v.windows.len() as u32;
    stats.files = v.files.len() as u32;
    stats.clusters = v.bootstrap.base.clusters.len() as u32;
}

/// Whether `r` (grown by [`GAP`]) is clear of every window of `v`.
fn is_free(v: &View, r: &Rect) -> bool {
    let grown = Rect {
        x: r.x - GAP,
        y: r.y - GAP,
        w: r.w + 2.0 * GAP,
        h: r.h + 2.0 * GAP,
    };
    let cell = |x: f64| (x / TILE_BASE_WU).floor() as i64;
    for tx in cell(grown.x)..=cell(grown.x + grown.w) {
        for ty in cell(grown.y)..=cell(grown.y + grown.h) {
            if let Some((ws, _)) = v.tiles.get(&(tx, ty))
                && ws
                    .iter()
                    .any(|&w| v.windows[w].header.rect.intersects(&grown))
            {
                return false;
            }
        }
    }
    true
}

/// The cluster of the nearest same-lane file sharing the deepest directory with `path`.
fn sibling_cluster(v: &View, path: &str, lane: &str) -> Option<usize> {
    let dir = cv_lang::dirname(path);
    let depth = |p: &str| {
        let d = cv_lang::dirname(p);
        if d.is_empty() || dir.is_empty() {
            return 0;
        }
        d.split('/')
            .zip(dir.split('/'))
            .take_while(|(a, b)| a == b)
            .count()
    };
    let best = v
        .files
        .iter()
        .filter(|f| f.lane == lane && !f.windows.is_empty())
        .max_by_key(|f| (depth(&f.path), std::cmp::Reverse(f.path.clone())))?;
    if depth(&best.path) == 0 && !dir.is_empty() {
        return None;
    }
    let id = &v.windows[best.windows[0]].header.cluster;
    v.bootstrap.base.clusters.iter().position(|c| &c.id == id)
}

fn union(a: &Rect, b: &Rect) -> Rect {
    let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
    let (x1, y1) = ((a.x + a.w).max(b.x + b.w), (a.y + a.h).max(b.y + b.h));
    Rect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    }
}

/// Adds new files to `v`. Returns the ids of the windows created.
pub fn add_files(
    v: &mut View,
    reg: &Registry,
    root: Option<&Path>,
    commit: &CommitInfo,
    new: Vec<NewFile>,
) -> Vec<String> {
    if new.is_empty() {
        return vec![];
    }
    let vid = v.id().to_string();
    let first_new_file = v.files.len();
    let mut created = Vec::new();
    let mut new_facts: Vec<BlobFacts> = Vec::new();
    // New clusters (one per lane) for files without a placed sibling.
    let mut lane_cluster: HashMap<&'static str, NewCluster> = HashMap::new();
    for nf in new {
        let (pack, facts) = crate::parse_cache::parse_text(reg, root, &nf.path, &nf.blob, &nf.text);
        let lang = pack.map(|i| reg.pack(i).lang(&nf.path)).unwrap_or("other");
        let mut rec = file_record(nf.path, nf.blob, lang, nf.lane, nf.text, &facts);
        let mut specs = chunk::chunk(rec.line_count(), &facts.symbols);
        chunk::assign_keys(&mut specs, &facts.symbols);
        let fi = v.files.len();
        let sibling = sibling_cluster(v, &rec.path, rec.lane);
        for w in &specs {
            let key = format!("{}#{}", rec.path, w.key);
            let mut id = format!("w{:012x}", hash(&format!("{vid}|{key}")) & 0xffff_ffff_ffff);
            while v.by_id.contains_key(&id) {
                id.push('_');
            }
            let width = crate::index::window_width(&rec, w.start, w.end);
            let height = HEADER_WU + (w.end - w.start + 1) as f64 * LINE_WU;
            let (ci, rect) = place(v, sibling, rec.lane, &mut lane_cluster, width, height);
            let wi = v.windows.len();
            let symbol = w.symbol.map(|s| {
                let s = &facts.symbols[s];
                SymbolRef {
                    name: s.name.clone(),
                    qualname: s.qualname.clone(),
                    kind: s.kind.as_str().into(),
                }
            });
            v.windows.push(Win {
                header: WindowHeader {
                    id: id.clone(),
                    key,
                    kind: w.kind.into(),
                    cluster: v.bootstrap.base.clusters[ci].id.clone(),
                    lane: rec.lane.into(),
                    path: rec.path.clone(),
                    lang: rec.lang.into(),
                    rev: commit.commit.clone(),
                    change: commit.change.clone(),
                    blob: rec.blob.clone(),
                    symbol,
                    lines: LineSpan {
                        start: w.start,
                        end: w.end,
                        file_lines: rec.line_count(),
                    },
                    rect,
                    heat: Heat {
                        grade: 0,
                        rate: 0.0,
                        ci: [0.0, 0.0],
                        settled: false,
                        touches: 0,
                        in_range: None,
                    },
                    conn: Connectedness {
                        grade: 0,
                        dependents2: 0,
                        fan_in: 0,
                        fan_out: 0,
                        tests: 0,
                        reach: 0,
                        cycle: None,
                    },
                    risk: 0.0,
                    review_first: false,
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
                    expanded_from: None,
                },
                file: fi,
                out_links: vec![],
                in_links: vec![],
            });
            // Keep the free-space check current for the next window.
            v.by_id.insert(id.clone(), wi);
            let cell = |x: f64| (x / TILE_BASE_WU).floor() as i64;
            for tx in cell(rect.x)..=cell(rect.x + rect.w) {
                for ty in cell(rect.y)..=cell(rect.y + rect.h) {
                    v.tiles.entry((tx, ty)).or_default().0.push(wi);
                }
            }
            rec.windows.push(wi);
            created.push(id);
        }
        if rec.windows.len() > 1 {
            let siblings: Vec<String> = rec
                .windows
                .iter()
                .map(|&w| v.windows[w].header.id.clone())
                .collect();
            for (i, &w) in rec.windows.iter().enumerate() {
                v.windows[w].header.fragment = Some(FileFragment {
                    index: i as u32 + 1,
                    of: rec.windows.len() as u32,
                    chip: chunk::chip_slot(&rec.path),
                    siblings: siblings.clone(),
                });
            }
        }
        v.by_path.insert(rec.path.clone(), fi);
        v.files.push(rec);
        new_facts.push(facts);
    }
    add_outgoing_links(v, reg, root, first_new_file, &new_facts);
    // Callers finish with `finish_refresh`, which rebuilds the tile index.
    relink_without_tiles(v);
    created
}

/// Where the next window of a lane's "new files" cluster goes: its column's x, the next y, the
/// column's width so far.
struct NewCluster {
    ci: usize,
    x: f64,
    y: f64,
    col_w: f64,
}

/// A spot for a `w`×`h` window in the sibling cluster `ci`: the nearest free spot inside its frame
/// (from its bottom-left), else just outside it, where the grown frame stays clear of every other
/// cluster's frame. `None` when no nearby spot allows that: walking further away would stretch the
/// frame over its neighbours (a tall box whose windows sit among other clusters' windows).
fn beside(v: &View, ci: usize, w: f64, h: f64) -> Option<Rect> {
    const TRIES: usize = 48;
    let (pad, title) = (cv_layout::CLUSTER_PAD, cv_layout::CLUSTER_TITLE);
    let frame = v.bootstrap.base.clusters[ci].rect;
    let reach = w.max(h) + 8.0 * GAP;
    let area = Rect {
        x: frame.x - reach,
        y: frame.y - reach,
        w: frame.w + 2.0 * reach,
        h: frame.h + 2.0 * reach,
    };
    // Neighbouring frames (that do not already overlap this one) are walls, grown by the padding.
    let frames: Vec<Rect> = v
        .bootstrap
        .base
        .clusters
        .iter()
        .enumerate()
        .filter(|&(c, k)| {
            c != ci && k.rect.w > 0.0 && k.rect.intersects(&area) && !k.rect.intersects(&frame)
        })
        .map(|(_, k)| k.rect)
        .collect();
    let cell = |x: f64| (x / TILE_BASE_WU).floor() as i64;
    let mut near: Vec<usize> = Vec::new();
    for tx in cell(area.x)..=cell(area.x + area.w) {
        for ty in cell(area.y)..=cell(area.y + area.h) {
            if let Some((ws, _)) = v.tiles.get(&(tx, ty)) {
                near.extend(ws.iter().copied());
            }
        }
    }
    near.sort_unstable();
    near.dedup();
    let mut placed: Vec<Rect> = near
        .into_iter()
        .map(|i| v.windows[i].header.rect)
        .filter(|r| r.intersects(&area))
        .chain(frames.iter().map(|f| Rect {
            x: f.x - pad,
            y: f.y - pad,
            w: f.w + 2.0 * pad,
            h: f.h + 2.0 * pad,
        }))
        .collect();
    let min = (frame.x + pad, frame.y + title);
    let room = (frame.x + frame.w - pad, frame.y + frame.h - pad);
    for _ in 0..TRIES {
        let (x, y) = cv_layout::free_spot(
            &placed,
            w,
            h,
            (min.0, room.1),
            min,
            f64::INFINITY,
            room,
            GAP,
        );
        let r = Rect { x, y, w, h };
        let grown = crate::review_api::grown_frame(&frame, &r, pad);
        let within = r.x + r.w <= area.x + area.w && r.y + r.h <= area.y + area.h;
        if within && is_free(v, &r) && !frames.iter().any(|f| f.intersects(&grown)) {
            return Some(r);
        }
        if !within {
            return None;
        }
        placed.push(r);
    }
    None
}

/// A spot for a `w`×`h` window: with the sibling cluster when it has room ([`beside`]), else in
/// this lane's new cluster at the lane's right edge (right of the lane's own clusters, inside the
/// lane band: a column that would run out of the band continues in a new column to its right).
/// Grows the cluster, lane band and world to include it.
fn place(
    v: &mut View,
    sibling: Option<usize>,
    lane: &'static str,
    lane_cluster: &mut HashMap<&'static str, NewCluster>,
    w: f64,
    h: f64,
) -> (usize, Rect) {
    let spot = sibling.and_then(|ci| beside(v, ci, w, h).map(|r| (ci, r)));
    let world = v.bootstrap.base.world;
    let band = v
        .bootstrap
        .base
        .lanes
        .iter()
        .find(|b| b.lane == lane)
        .map(|b| b.rect)
        .unwrap_or(world);
    let (ci, rect) = spot.unwrap_or_else(|| {
        let nc = lane_cluster.entry(lane).or_insert_with(|| {
            // The lane's right edge: right of its rightmost cluster (bands span the whole world).
            let right = v
                .bootstrap
                .base
                .clusters
                .iter()
                .filter(|c| c.lane == lane && c.rect.w > 0.0)
                .map(|c| c.rect.x + c.rect.w)
                .fold(f64::NEG_INFINITY, f64::max);
            let x = if right.is_finite() {
                right + 8.0 * GAP
            } else {
                band.x + 4.0 * GAP
            };
            let y = band.y + 4.0 * GAP;
            let clusters = &mut v.bootstrap.base.clusters;
            clusters.push(Cluster {
                id: format!("c{}", clusters.len()),
                parent: None,
                lane: lane.into(),
                label: "new files".into(),
                label_source: "auto".into(),
                summary: None,
                rect: Rect {
                    x,
                    y,
                    w: 0.0,
                    h: 0.0,
                },
                heat: 0,
                conn: 0,
                windows: 0,
                open_threads: 0,
                conflicts: 0,
                pinned: false,
                heat_level: None,
                review: None,
                folder: None,
            });
            NewCluster {
                ci: clusters.len() - 1,
                x,
                y,
                col_w: 0.0,
            }
        });
        let top = band.y + 4.0 * GAP;
        let bottom = band.y + band.h - 4.0 * GAP;
        let (mut x, mut y, mut col_w) = (nc.x, nc.y, nc.col_w);
        let mut r = Rect { x, y, w, h };
        for _ in 0..2000 {
            r = Rect { x, y, w, h };
            let first_in_col = y <= top;
            if (first_in_col || y + h <= bottom) && is_free(v, &r) {
                break;
            }
            if y + h > bottom && !first_in_col {
                // The column is full: the next one, right of it.
                x += col_w.max(w) + GAP;
                y = top;
                col_w = 0.0;
            } else {
                y += GAP;
            }
        }
        nc.x = x;
        nc.y = r.y + h + GAP;
        nc.col_w = col_w.max(w);
        (nc.ci, r)
    });
    let b = &mut v.bootstrap.base;
    let c = &mut b.clusters[ci].rect;
    *c = if c.w == 0.0 && c.h == 0.0 {
        rect
    } else if spot.is_some() {
        crate::review_api::grown_frame(c, &rect, cv_layout::CLUSTER_PAD)
    } else {
        union(c, &rect)
    };
    if let Some(band) = b.lanes.iter_mut().find(|l| l.lane == lane) {
        band.rect = union(&band.rect, &rect);
    }
    b.world = union(&b.world, &rect);
    (ci, rect)
}

/// Outgoing code links of the new files (`v.files[first..]`), resolved through their imports.
fn add_outgoing_links(
    v: &mut View,
    reg: &Registry,
    root: Option<&Path>,
    first: usize,
    new_facts: &[BlobFacts],
) {
    let project = PathIndex::new(v.files.iter().map(|f| f.path.as_str()), |p| {
        v.by_path.get(p).map(|&i| v.files[i].text.clone())
    });
    // Import targets of the new files, with their facts (cached parses).
    let mut members: Vec<usize> = (first..v.files.len()).collect();
    let mut resolved: Vec<Vec<Option<usize>>> = Vec::new();
    for (k, facts) in new_facts.iter().enumerate() {
        let fi = first + k;
        let path = v.files[fi].path.clone();
        let pack = reg.pack_for(&path).map(|(i, _)| i);
        let r: Vec<Option<usize>> = facts
            .imports
            .iter()
            .map(|imp| {
                pack.and_then(|pi| reg.pack(pi).resolve_import(&path, imp, &project))
                    .and_then(|p| v.by_path.get(&p).copied())
            })
            .collect();
        members.extend(r.iter().flatten().copied());
        resolved.push(r);
    }
    drop(project);
    members.sort_unstable();
    members.dedup();
    let pos: HashMap<usize, usize> = members.iter().enumerate().map(|(i, &f)| (f, i)).collect();
    let target_facts: HashMap<usize, (Option<usize>, BlobFacts)> = members
        .iter()
        .filter(|&&f| f < first)
        .map(|&f| {
            let rec = &v.files[f];
            (
                f,
                crate::parse_cache::parse_text(reg, root, &rec.path, &rec.blob, &rec.text),
            )
        })
        .collect();
    let inputs: Vec<cv_link::FileIn> = members
        .iter()
        .map(|&f| {
            let rec = &v.files[f];
            let (facts, lang, imports) = if f >= first {
                let k = f - first;
                (
                    &new_facts[k],
                    reg.pack_for(&rec.path).map(|(_, p)| p.id()),
                    resolved[k]
                        .iter()
                        .map(|t| t.and_then(|t| pos.get(&t).copied()))
                        .collect(),
                )
            } else {
                let (p, facts) = &target_facts[&f];
                (facts, p.map(|i| reg.pack(i).id()), vec![])
            };
            cv_link::FileIn {
                path: &rec.path,
                lane: rec.lane,
                lang,
                conv: reg.conventions(lang),
                facts: Some(facts),
                imports,
                windows: rec
                    .windows
                    .iter()
                    .map(|&w| {
                        let l = &v.windows[w].header.lines;
                        (l.start, l.end, w)
                    })
                    .collect(),
            }
        })
        .collect();
    let new_windows: HashSet<usize> = (first..v.files.len())
        .flat_map(|f| v.files[f].windows.iter().copied())
        .collect();
    let out: Vec<cv_link::LinkOut> = cv_link::links(&inputs)
        .into_iter()
        .filter(|l| new_windows.contains(&l.from_window))
        .collect();
    drop(inputs);
    let first_id = v
        .links
        .iter()
        .filter_map(|l| {
            l.id.trim_start_matches("dyn-")
                .trim_start_matches('l')
                .parse::<usize>()
                .ok()
        })
        .max()
        .map_or(0, |m| m + 1);
    for (i, l) in out.into_iter().enumerate() {
        v.links.push(LinkRec {
            id: format!("l{}", first_id + i),
            kind: l.kind.as_str(),
            from: l.from_window,
            from_line: l.from_line,
            to: l.to_window,
            to_line: l.to_line,
            confidence: l.confidence,
            label: l.label,
            channel: None,
        });
    }
}

// ─── Complete incremental updates (DESIGN §4.1) ─────────────────────────────
//
// After the structural refresh above, everything derived from the link graph is brought up to
// date without a full index:
//
// - **Code links** ([`relink_code`]): every ref is re-resolved over the whole view (lexical tier,
//   plus SCIP when the revision carries an index), so refs in changed files, in files importing a
//   new file and lexical candidates all see the new files. Links that did not change keep their
//   ids; the windows at both ends of a link that came, went or moved are reported.
// - **Connectedness** ([`reconnect`]): recomputed from the updated graph (code links, imports and
//   the confident channel links the view has), ~0.3 s on a 5.6k-file repo.
// - **Channel links and heat** ([`settle`], run in the background after the fast refresh): the
//   channel packs run over the whole view (they match producers and consumers project-wide, ~2 s
//   on a 5.6k-file repo) and the churn walk runs again. A window's heat grade follows the settled-grades
//   rule (§4.7, §4.10 "moves happen only on settled grade changes"): it changes only when the new
//   grade is settled (its interval lies inside one band), or when the window had no change in
//   history before (a new file, a frozen file now touched). Positions never move here: cluster
//   cells follow at the next full index or re-layout.
//
// Facts come from an in-memory cache of slim facts (no tokens) per (path, blob), filled by the
// full index and by misses (through the parse cache), so a refresh does not decode every file.

type Slim = Arc<(Option<usize>, BlobFacts)>;
/// Slim facts of one repo by (path, blob).
type SlimMap = HashMap<(String, String), Slim>;

static SLIM: Mutex<Option<HashMap<PathBuf, SlimMap>>> = Mutex::new(None);

/// Keeps the facts of a just-indexed snapshot for later refreshes (tokens dropped), as
/// ((path, blob), (pack, facts)). Facts of other snapshots of the repo stay while the cache holds
/// at most three snapshots' worth.
pub fn remember_facts(
    root: &Path,
    parsed: impl IntoIterator<Item = ((String, String), (Option<usize>, BlobFacts))>,
) {
    let entries = slim(parsed);
    let mut g = SLIM.lock().unwrap();
    let cur = g
        .get_or_insert_with(HashMap::new)
        .entry(root.to_path_buf())
        .or_default();
    if cur.len() + entries.len() > 3 * entries.len().max(1) {
        cur.retain(|k, _| entries.contains_key(k));
    }
    cur.extend(entries);
}

/// The remembered facts (tokens dropped) of each (path, blob), when this process has them.
pub fn remembered_facts(root: &Path, keys: &[(String, String)]) -> Vec<Option<Slim1>> {
    let g = SLIM.lock().unwrap();
    let m = g.as_ref().and_then(|m| m.get(root));
    keys.iter()
        .map(|k| m.and_then(|m| m.get(k)).map(|s| Slim1(s.clone())))
        .collect()
}

/// One file's remembered facts.
pub struct Slim1(Slim);

impl std::ops::Deref for Slim1 {
    type Target = BlobFacts;
    fn deref(&self) -> &BlobFacts {
        &self.0.1
    }
}

fn slim(
    parsed: impl IntoIterator<Item = ((String, String), (Option<usize>, BlobFacts))>,
) -> SlimMap {
    parsed
        .into_iter()
        .map(|(k, (p, mut facts))| {
            facts.tokens = Vec::new();
            (k, Arc::new((p, facts)))
        })
        .collect()
}

/// Facts and resolved imports of every file of a view.
pub struct ViewFacts {
    pub parsed: Vec<Slim>,
    /// Per file, the target file of each import (parallel to `facts.imports`).
    pub resolved: Vec<Vec<Option<usize>>>,
}

/// Facts of every file of `v` (slim cache, else the parse cache, else parsed) and its imports
/// resolved against the view's files.
pub fn view_facts(v: &View, reg: &Registry, root: &Path) -> ViewFacts {
    let mut parsed: Vec<Option<Slim>> = {
        let g = SLIM.lock().unwrap();
        let m = g.as_ref().and_then(|m| m.get(root));
        v.files
            .iter()
            .map(|f| m.and_then(|m| m.get(&(f.path.clone(), f.blob.clone())).cloned()))
            .collect()
    };
    let misses: Vec<usize> = (0..parsed.len()).filter(|&i| parsed[i].is_none()).collect();
    if !misses.is_empty() {
        let items: Vec<crate::parse_cache::Item> = misses
            .iter()
            .map(|&i| crate::parse_cache::Item {
                path: &v.files[i].path,
                blob: &v.files[i].blob,
                text: &v.files[i].text,
            })
            .collect();
        let (got, _) = crate::parse_cache::parse_all(reg, root, &items, &|| {});
        let fresh = slim(
            misses
                .iter()
                .map(|&i| (v.files[i].path.clone(), v.files[i].blob.clone()))
                .zip(got),
        );
        for &i in &misses {
            parsed[i] = fresh
                .get(&(v.files[i].path.clone(), v.files[i].blob.clone()))
                .cloned();
        }
        let mut g = SLIM.lock().unwrap();
        g.get_or_insert_with(HashMap::new)
            .entry(root.to_path_buf())
            .or_default()
            .extend(fresh);
    }
    let parsed: Vec<Slim> = parsed
        .into_iter()
        .enumerate()
        .map(|(i, p)| {
            p.unwrap_or_else(|| Arc::new((None, crate::parse_cache::unparsed(&v.files[i].text))))
        })
        .collect();
    let project = PathIndex::new(v.files.iter().map(|f| f.path.as_str()), |p| {
        v.by_path.get(p).map(|&i| v.files[i].text.clone())
    });
    let resolved: Vec<Vec<Option<usize>>> = v
        .files
        .par_iter()
        .zip(&parsed)
        .map(|(f, pf)| match pf.0 {
            Some(pi) => {
                pf.1.imports
                    .iter()
                    .map(|imp| {
                        reg.pack(pi)
                            .resolve_import(&f.path, imp, &project)
                            .and_then(|p| v.by_path.get(&p).copied())
                    })
                    .collect()
            }
            None => Vec::new(),
        })
        .collect();
    ViewFacts { parsed, resolved }
}

/// cv-link inputs for every file of `v`.
fn link_inputs<'a>(v: &'a View, reg: &Registry, f: &'a ViewFacts) -> Vec<cv_link::FileIn<'a>> {
    v.files
        .iter()
        .enumerate()
        .map(|(fi, file)| {
            let mut windows: Vec<(u32, u32, usize)> = file
                .windows
                .iter()
                .map(|&w| {
                    let l = &v.windows[w].header.lines;
                    (l.start, l.end, w)
                })
                .collect();
            windows.sort_unstable();
            cv_link::FileIn {
                path: &file.path,
                lane: file.lane,
                lang: f.parsed[fi].0.map(|i| reg.pack(i).id()),
                conv: f.parsed[fi]
                    .0
                    .map(|i| reg.pack(i).conventions())
                    .unwrap_or_default(),
                facts: Some(&f.parsed[fi].1),
                imports: f.resolved[fi].clone(),
                windows,
            }
        })
        .collect()
}

/// Replaces the links `replace` selects with `fresh`, keeping the id of each old link with the
/// same key. Returns the windows at the ends of links that came, went or changed.
fn swap_links(
    v: &mut View,
    replace: impl Fn(&LinkRec) -> bool,
    mut fresh: Vec<LinkRec>,
    key: impl Fn(&LinkRec) -> (usize, usize, &'static str, u32),
) -> HashSet<usize> {
    let (old, keep): (Vec<LinkRec>, Vec<LinkRec>) =
        std::mem::take(&mut v.links).into_iter().partition(&replace);
    let mut by_key: HashMap<(usize, usize, &'static str, u32), LinkRec> =
        old.into_iter().map(|l| (key(&l), l)).collect();
    let mut changed = HashSet::new();
    let mut next = keep
        .iter()
        .chain(by_key.values())
        .filter_map(|l| {
            l.id.trim_start_matches("dyn-")
                .trim_start_matches('l')
                .parse::<usize>()
                .ok()
        })
        .max()
        .map_or(0, |m| m + 1);
    for l in fresh.iter_mut() {
        match by_key.remove(&key(l)) {
            Some(o) => {
                if o.from_line != l.from_line
                    || o.to_line != l.to_line
                    || o.confidence != l.confidence
                    || o.label != l.label
                {
                    changed.extend([l.from, l.to]);
                }
                l.id = o.id;
            }
            None => {
                changed.extend([l.from, l.to]);
                l.id = if l.is_dynamic() {
                    format!("dyn-{next}")
                } else {
                    format!("l{next}")
                };
                next += 1;
            }
        }
    }
    for o in by_key.values() {
        changed.extend([o.from, o.to]);
    }
    v.links = fresh;
    v.links.extend(keep);
    relink_without_tiles(v);
    changed
}

/// Re-resolves every code link of `v` (see the section comment). Returns the windows whose links
/// changed.
pub fn relink_code(
    v: &mut View,
    reg: &Registry,
    f: &ViewFacts,
    scip: Option<&cv_link::scip::Index>,
) -> HashSet<usize> {
    let t = std::time::Instant::now();
    let out = {
        let inputs = link_inputs(v, reg, f);
        let mut out = cv_link::links(&inputs);
        if let Some(ix) = scip {
            let precise = cv_link::scip::links(ix, &inputs);
            let covered = cv_link::scip::covered_lines(ix, &inputs);
            out = cv_link::scip::merge(out, precise, &covered, &inputs);
        }
        out
    };
    if std::env::var_os("CV_DEBUG_REFRESH").is_some() {
        eprintln!("  refresh: cv-link {:.1?}", t.elapsed());
    }
    let fresh: Vec<LinkRec> = out
        .into_iter()
        .map(|l| LinkRec {
            id: String::new(),
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
    swap_links(
        v,
        |l| !crate::channels::is_channel(l),
        fresh,
        |l| (l.from, l.to, l.kind, 0),
    )
}

fn ref_kind(kind: &str) -> cv_lang::RefKind {
    match kind {
        "inherit" => cv_lang::RefKind::Inherit,
        "implement" => cv_lang::RefKind::Implement,
        _ => cv_lang::RefKind::Call,
    }
}

/// Recomputes every window's connectedness (§4.8, D7) from the view's links and imports. Returns
/// the windows whose connectedness changed.
pub fn reconnect(
    v: &mut View,
    reg: &Registry,
    f: &ViewFacts,
    cuts: cv_link::conn::ConnCuts,
) -> HashSet<usize> {
    let conn = {
        let inputs = link_inputs(v, reg, f);
        let code: Vec<cv_link::LinkOut> = v
            .links
            .iter()
            .filter(|l| !crate::channels::is_channel(l))
            .map(|l| cv_link::LinkOut {
                kind: ref_kind(l.kind),
                from_window: l.from,
                from_line: l.from_line,
                to_window: l.to,
                to_line: l.to_line,
                confidence: l.confidence,
                label: String::new(),
                provenance: "tree-sitter",
            })
            .collect();
        let mut deps = cv_link::conn::typed_deps(&inputs, &code);
        deps.windows
            .extend(v.links.iter().filter_map(crate::channels::link_dependency));
        let lanes: Vec<&str> = v.windows.iter().map(|w| v.files[w.file].lane).collect();
        let file_of: Vec<usize> = v.windows.iter().map(|w| w.file).collect();
        cv_link::conn::window_connectedness(&lanes, &file_of, &deps, cuts)
    };
    let mut changed = HashSet::new();
    for (wi, c) in conn.into_iter().enumerate() {
        let new = Connectedness {
            grade: c.grade,
            dependents2: c.dependents2,
            fan_in: c.fan_in,
            fan_out: c.fan_out,
            tests: c.tests,
            reach: c.reach,
            cycle: (c.cycle > 1).then_some(c.cycle),
        };
        let h = &mut v.windows[wi].header;
        let old = &h.conn;
        if (
            old.grade,
            old.dependents2,
            old.fan_in,
            old.fan_out,
            old.tests,
            old.reach,
            old.cycle,
        ) != (
            new.grade,
            new.dependents2,
            new.fan_in,
            new.fan_out,
            new.tests,
            new.reach,
            new.cycle,
        ) {
            changed.insert(wi);
            h.conn = new;
        }
    }
    changed
}

/// Heat from a fresh churn walk under the settled-grades rule (see the section comment). Returns
/// the windows whose heat changed.
pub fn apply_heat(v: &mut View, churn: &cv_churn::ChurnResult) -> HashSet<usize> {
    let mut changed = HashSet::new();
    for fi in 0..v.files.len() {
        let h = churn
            .files
            .get(&v.files[fi].path)
            .cloned()
            .unwrap_or_default();
        for wi in v.files[fi].windows.clone() {
            let old = &v.windows[wi].header.heat;
            let take = h.grade == old.grade || h.settled || old.touches == 0;
            let new = Heat {
                grade: if take { h.grade } else { old.grade },
                rate: h.rate,
                ci: h.ci,
                settled: take && h.settled,
                touches: h.touches,
                in_range: old.in_range.clone(),
            };
            if (old.grade, old.rate, old.ci, old.settled, old.touches)
                != (new.grade, new.rate, new.ci, new.settled, new.touches)
            {
                changed.insert(wi);
                v.windows[wi].header.heat = new;
            }
        }
    }
    changed
}

/// What [`settle`] produced.
pub struct Settled {
    pub view: View,
    /// Windows whose headers or links changed.
    pub windows: Vec<String>,
    /// The bootstrap's hubs changed (clients reload everything).
    pub hubs_changed: bool,
}

/// The slower half of an incremental update, run in the background after the fast refresh:
/// channel links and hubs over the whole view, connectedness again with them, and heat from a
/// fresh churn walk (see the section comment). `old` must be a snapshot view.
pub fn settle(
    old: &View,
    reg: &Registry,
    repo: &cv_vcs::JjRepo,
    max_commits: usize,
) -> Result<Settled, String> {
    let mut v = old.clone();
    let head = v.bootstrap.base.view.resolved.head.clone();
    let grade_cfg = crate::grades::grade_config(repo, &head);
    let f = view_facts(&v, reg, repo.root());
    let (chan, churn) = std::thread::scope(|s| {
        let churn = s.spawn(|| crate::index::churn(repo, &head, max_commits, grade_cfg.mass_cuts));
        let parsed: Vec<(Option<usize>, BlobFacts)> =
            f.parsed.iter().map(|p| (p.0, p.1.clone())).collect();
        let specs: Vec<(usize, chunk::WindowSpec)> = v
            .windows
            .iter()
            .map(|w| {
                (
                    w.file,
                    chunk::WindowSpec {
                        kind: "chunk",
                        start: w.header.lines.start,
                        end: w.header.lines.end,
                        symbol: None,
                        key: String::new(),
                    },
                )
            })
            .collect();
        let chan = crate::channels::build(reg, &v.files, &parsed, &f.resolved, &specs);
        let churn = churn
            .join()
            .map_err(|_| "churn thread panicked".to_string());
        (chan, churn)
    });
    let (churn, renames, _) = churn??;
    let fresh: Vec<LinkRec> = chan.link_recs(0).collect();
    let mut touched = swap_links(&mut v, crate::channels::is_channel, fresh, |l| {
        let dynamic = l.is_dynamic();
        (
            l.from,
            if dynamic { l.from } else { l.to },
            l.kind,
            if dynamic { l.from_line } else { 0 },
        )
    });
    let lanes = v.bootstrap.base.lanes.clone();
    let lane_index = |lane: &str| lanes.iter().position(|b| b.lane == lane).unwrap_or(0);
    let hubs = crate::channels::hubs(&chan, &v.files, &v.windows, &lanes, lane_index);
    let hubs_changed =
        serde_json::to_value(&hubs).ok() != serde_json::to_value(&v.bootstrap.base.hubs).ok();
    v.bootstrap.base.hubs = hubs;
    touched.extend(reconnect(&mut v, reg, &f, grade_cfg.conn));
    touched.extend(apply_heat(&mut v, &churn));
    let c = &mut v.bootstrap.churn;
    c.head = head;
    c.scanned_commits = churn.scanned as u32;
    c.settled_mass = churn.settled_mass;
    c.stop = churn.stop.as_str().to_string();
    c.oldest = churn
        .oldest_ms
        .map(crate::index::iso_time)
        .unwrap_or_default();
    c.bands = churn.bands;
    v.renames = Arc::new(renames);
    v.tiles = crate::index::grid_index(&v.windows, &v.links);
    let mut windows: Vec<String> = touched
        .into_iter()
        .filter(|&w| w < v.windows.len())
        .map(|w| v.windows[w].header.id.clone())
        .collect();
    windows.sort();
    Ok(Settled {
        view: v,
        windows,
        hubs_changed,
    })
}
