//! B6 `cv-layout`: clusters, the district grid and placement inside clusters. Deterministic: the
//! same input always gives the same output (every ordering has a total tie-break).
//!
//! - **Clusters (DESIGN §4.10.1–2):** per lane, a window graph weighted 1.0·log₂(1+calls) +
//!   2.0·inherits + 1.5·channel links + 0.5·imports + 1.0·co-change Jaccard + cohesion between
//!   consecutive fragments of a file ([`cluster::weights`]). In-house Leiden ([`leiden`]) finds
//!   modules; modules over [`MAX_CLUSTER`] windows are split at a higher resolution, modules under
//!   [`MIN_CLUSTER`] join their strongest neighbour, and windows with no ties at all are pooled by
//!   directory. Subsystems are Leiden over the module graph (`ClusterOut::parent`).
//! - **Labels:** automatic (a dominant file, else the deepest directory holding 60% of the windows);
//!   a human or agent label carries over to the new cluster with the highest member Jaccard ≥ 0.6.
//! - **Cluster grades:** heat is a change-mass quantile *over clusters* (clusters sorted by change
//!   mass per line; Hot holds the first 50% of the lane's mass, Warm the next 25%, Mild 15%, Cool
//!   the rest, Frozen none), connectedness a rank quantile over clusters of their most-depended-on window
//!   (Core top 10%, High 15%, Medium 25%, Low the rest, Isolated none), per lane. DESIGN's literal
//!   "mass-weighted heat and maximum connectedness grade" lifts any cluster holding one hot or one
//!   central file into Hot / Core; quantiles over clusters keep D6 (quantiles, never fixed limits)
//!   at the cluster level and keep the columns meaningful.
//! - **District grid (§4.9):** lanes top to bottom, heat columns Hot → Frozen shared by every lane,
//!   connectedness rows Core → Isolated per lane, empty rows and columns collapsed (a column
//!   empty in every lane gets zero width and no gap; in a fresh layout). The
//!   review-first cell (code × Hot × Core) is the top-left cell of the code lane, where the camera
//!   starts. Within a cell, clusters of one subsystem stay together and clusters sharing files or
//!   links are packed next to each other (same-file attraction).
//! - **Inside a cluster:** longest-path layering of intra-cluster links (callers left of callees,
//!   back edges of cycles ignored); within a layer hotter files first and same-file windows
//!   adjacent, then barycenter sweeps that are kept only when they remove crossings
//!   ([`reduce_crossings`]); tall layers wrap into sub-columns.
//! - **Stability (§4.10.6):** with a previous [`Placement`] (by `WindowKey`), clusters keep their
//!   members, cells and positions; new windows join the cluster they are most linked to; a cluster
//!   changes cell only when its new heat grade is settled (connectedness grades are exact). Inside
//!   a continuing cluster surviving windows keep their offsets; a resized window moves only when
//!   it would overlap, to the nearest free spot, and new windows take free space beside their
//!   file's other fragments or linked windows ([`keep_cluster`]). Clusters that no longer fit
//!   their spot, changed cell or are new take the nearest free spot of their cell, inside the
//!   row's height when possible ([`free_spot`]); rows and columns only grow. A full re-layout
//!   happens only on request (`Input::full`). Pinned windows never move.

pub mod cluster;
pub mod folders;
pub mod leiden;
pub mod structure;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use cv_contracts::{FolderFrame, GridColumn, LaneBand, LaneRow, Rect};

pub const MIN_CLUSTER: usize = 4;
pub const MAX_CLUSTER: usize = 36;

pub const CLUSTER_PAD: f64 = 80.0;
pub const CLUSTER_TITLE: f64 = 120.0;
const LAYER_GAP: f64 = 320.0;
pub const WIN_GAP: f64 = 90.0;
const CELL_PAD: f64 = 240.0;
const CELL_GAP: f64 = 160.0;
const COL_GAP: f64 = 1600.0;
const ROW_GAP: f64 = 500.0;
const LANE_GAP: f64 = 1400.0;
const LANE_PAD: f64 = 400.0;
const HEADER_BAND: f64 = 1800.0;
/// Fresh rows get this much room to grow, so small edits do not push the rows below.
const ROW_SLACK: f64 = 0.08;
/// A cluster's heat counts as settled when this share of its change mass is in settled files.
const SETTLED_SHARE: f64 = 0.5;
/// Labels carry over to the new cluster with the highest member Jaccard at or above this.
pub const LABEL_JACCARD: f64 = 0.6;

pub struct FileIn {
    pub path: String,
    /// Index into the lanes, in display order.
    pub lane: usize,
    /// The file is its directory's entry point (its pack's `Conventions::entry_stems`): cluster
    /// labels name the directory instead of the file.
    pub entry: bool,
}

pub struct WinIn {
    pub file: usize,
    /// World size of the window frame.
    pub w: f64,
    pub h: f64,
    pub start_line: u32,
    pub heat: u8,
    pub rate: f64,
    pub conn: u8,
    pub dependents2: u32,
    /// Revision-independent `WindowKey` (`path#key`), for stability across layouts.
    pub key: String,
    /// The file's heat grade is settled (its interval lies inside one band).
    pub settled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    Call,
    Inherit,
    Implement,
    Import,
    Channel,
}

pub struct LinkIn {
    pub from: usize,
    pub to: usize,
    pub kind: EdgeKind,
}

/// Two files (indexes into `files`) that change together.
pub struct CoChangeIn {
    pub a: usize,
    pub b: usize,
    pub jaccard: f64,
}

/// A window pinned at a world position: it never moves.
#[derive(Debug, Clone, PartialEq)]
pub struct Pin {
    pub key: String,
    pub x: f64,
    pub y: f64,
}

pub struct Input<'a> {
    /// Lane ids in display order.
    pub lanes: &'a [&'a str],
    pub files: &'a [FileIn],
    pub windows: &'a [WinIn],
    pub links: &'a [LinkIn],
    /// File-rate edges between heat grades: [frozen|cool, cool|mild, mild|warm, warm|hot].
    pub heat_bands: [f64; 4],
    pub co_change: &'a [CoChangeIn],
    pub pins: &'a [Pin],
    /// The previous layout of this repo, for stability.
    pub prev: Option<&'a Placement>,
    /// Re-cluster and re-place everything (labels and pins still carry over).
    pub full: bool,
    /// D6 cut points for the cluster grades.
    pub grade_cuts: GradeCuts,
    /// How windows are grouped and placed.
    pub arrangement: Arrangement,
}

/// How a layout groups and places windows (`options.layout`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Arrangement {
    /// Dependency clusters on the district grid: lanes × heat columns × connectedness rows (D8).
    #[default]
    Districts,
    /// One cluster per directory (split when large), nested by the folder tree inside each lane
    /// ([`folders`]). Deterministic from the input alone: no previous placement, pins or labels.
    Folders,
    /// The folder tree with each directory's items ordered to keep linked ones next to each other
    /// (short links) instead of by path.
    Flow,
}

impl Arrangement {
    /// The folder tree, in either order.
    pub fn is_tree(self) -> bool {
        matches!(self, Arrangement::Folders | Arrangement::Flow)
    }
}

/// Grade cut points (D6, `.cv/grades.toml`), applied to clusters as to files and windows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GradeCuts {
    /// Cumulative change-mass shares closing Hot, Warm and Mild.
    pub mass: [f64; 3],
    /// Rank quantiles opening Medium, High and Core (ascending).
    pub rank: [f64; 3],
}

impl Default for GradeCuts {
    fn default() -> Self {
        GradeCuts {
            mass: [0.5, 0.75, 0.9],
            rank: [0.5, 0.75, 0.9],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClusterOut {
    pub lane: usize,
    pub label: String,
    /// "auto", "agent" or "human".
    pub label_source: String,
    /// Cell grades (what the position reflects).
    pub heat: u8,
    pub conn: u8,
    /// The heat grade computed now; differs from `heat` while a change is unsettled.
    pub heat_now: u8,
    pub heat_settled: bool,
    /// Heat on a continuous 0..1 scale within the cell's grade: grade `g` spans [g/5, (g+1)/5),
    /// placed by where the cluster's change mass sits inside that grade's share of the lane.
    pub heat_level: f64,
    /// Share of the cluster's windows that are review first (the L0 Hotspots lens).
    pub review: f64,
    pub members: Vec<usize>,
    pub rect: Rect,
    /// Index into `Output::subsystems`.
    pub parent: Option<usize>,
    /// Folders arrangement: the directory whose own files the cluster holds.
    pub folder: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubsystemOut {
    pub lane: usize,
    pub label: String,
    pub clusters: Vec<usize>,
}

#[derive(Debug, Clone)]
pub struct Output {
    pub rects: Vec<Rect>,
    pub cluster_of: Vec<usize>,
    pub clusters: Vec<ClusterOut>,
    pub subsystems: Vec<SubsystemOut>,
    pub lanes: Vec<LaneBand>,
    pub columns: Vec<GridColumn>,
    pub world: Rect,
    pub risk: Vec<f64>,
    pub review_first: Vec<bool>,
    /// What the next layout of this repo should start from.
    pub placement: Placement,
    /// Folders arrangement: directory frames, outermost first (empty on the district grid).
    pub folders: Vec<FolderFrame>,
}

/// A layout remembered by `WindowKey`: cluster membership, labels, cells and positions.
/// Serializable so a saved view (index.db) can hand it to the next layout after a restart.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Placement {
    /// Window key → (cluster index, world rect).
    pub windows: BTreeMap<String, (usize, Rect)>,
    pub clusters: Vec<PlacedCluster>,
    pub col_x: [f64; 5],
    pub col_w: [f64; 5],
    /// Per lane id: lane top and its rows.
    pub lanes: Vec<PlacedLane>,
}

/// A lane of a placement: id, top and rows as (conn, y, h).
pub type PlacedLane = (String, f64, Vec<(u8, f64, f64)>);

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlacedCluster {
    pub lane: String,
    pub label: String,
    pub label_source: String,
    /// Cell grades.
    pub heat: u8,
    pub conn: u8,
    pub rect: Rect,
    /// Member window keys, sorted.
    pub members: Vec<String>,
    pub subsystem: usize,
}

impl Placement {
    /// Sets a human or agent label; it carries over to later layouts (member Jaccard ≥ 0.6).
    pub fn set_label(&mut self, cluster: usize, label: &str, source: &str) -> bool {
        match self.clusters.get_mut(cluster) {
            Some(c) => {
                c.label = label.to_string();
                c.label_source = source.to_string();
                true
            }
            None => false,
        }
    }
}

/// The directory of a repo path ("" at the root).
pub fn dirname(p: &str) -> &str {
    p.rfind('/').map(|i| &p[..i]).unwrap_or("")
}

/// Layered placement inside one cluster; writes rects relative to the cluster origin and returns
/// the cluster size.
pub(crate) fn place_cluster(
    members: &[usize],
    input: &Input,
    calls: &[Vec<usize>],
    pos: &mut [Rect],
) -> (f64, f64) {
    let local: BTreeMap<usize, usize> = members.iter().enumerate().map(|(i, &w)| (w, i)).collect();
    let n = members.len();
    let out: Vec<Vec<usize>> = members
        .iter()
        .map(|&w| {
            let mut v: Vec<usize> = calls[w]
                .iter()
                .filter_map(|t| local.get(t).copied())
                .collect();
            v.sort_unstable();
            v.dedup();
            v
        })
        .collect();
    // DFS post-order on the graph minus back edges → topological order.
    let mut state = vec![0u8; n]; // 0 new, 1 on stack, 2 done
    let mut order = Vec::with_capacity(n);
    let mut keep: Vec<Vec<usize>> = vec![Vec::new(); n];
    for s in 0..n {
        if state[s] != 0 {
            continue;
        }
        let mut stack = vec![(s, 0usize)];
        state[s] = 1;
        while let Some((v, i)) = stack.pop() {
            if i < out[v].len() {
                stack.push((v, i + 1));
                let t = out[v][i];
                match state[t] {
                    0 => {
                        keep[v].push(t);
                        state[t] = 1;
                        stack.push((t, 0));
                    }
                    2 => keep[v].push(t),
                    _ => {} // back edge: ignored
                }
            } else {
                state[v] = 2;
                order.push(v);
            }
        }
    }
    let mut layer = vec![0usize; n];
    for &v in order.iter().rev() {
        for &t in &keep[v] {
            layer[t] = layer[t].max(layer[v] + 1);
        }
    }
    let n_layers = layer.iter().max().map(|m| m + 1).unwrap_or(1);
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); n_layers];
    for (i, &l) in layer.iter().enumerate() {
        layers[l].push(i);
    }
    // Hotter files first; same-file windows adjacent, in line order.
    for l in layers.iter_mut() {
        l.sort_by(|&a, &b| {
            let (wa, wb) = (&input.windows[members[a]], &input.windows[members[b]]);
            wb.rate
                .total_cmp(&wa.rate)
                .then_with(|| input.files[wa.file].path.cmp(&input.files[wb.file].path))
                .then(wa.start_line.cmp(&wb.start_line))
                .then(a.cmp(&b))
        });
    }
    reduce_crossings(&mut layers, &layer, &keep);
    let area: f64 = members
        .iter()
        .map(|&w| (input.windows[w].w + WIN_GAP) * (input.windows[w].h + WIN_GAP))
        .sum();
    let tallest = members
        .iter()
        .map(|&w| input.windows[w].h)
        .fold(0.0, f64::max);
    let target_h = tallest.max((area / 1.4).sqrt());
    let mut x = CLUSTER_PAD;
    let mut max_y: f64 = 0.0;
    for l in &layers {
        let mut y = CLUSTER_TITLE;
        let mut col_w: f64 = 0.0;
        for &i in l {
            let w = &input.windows[members[i]];
            if y > CLUSTER_TITLE && y - CLUSTER_TITLE + w.h > target_h {
                x += col_w + WIN_GAP * 2.0;
                y = CLUSTER_TITLE;
                col_w = 0.0;
            }
            pos[members[i]] = Rect {
                x,
                y,
                w: w.w,
                h: w.h,
            };
            y += w.h + WIN_GAP;
            col_w = col_w.max(w.w);
            max_y = max_y.max(y);
        }
        x += col_w + LAYER_GAP;
    }
    (x - LAYER_GAP + CLUSTER_PAD, max_y - WIN_GAP + CLUSTER_PAD)
}

/// Candidate corners come from at most this many placed rects nearest the anchor (plus the
/// bottom-left and top-right corners of every placed rect), so a search stays near-linear in a
/// crowded cell.
const FREE_NEAR: usize = 16;

/// Whether `a` comes closer than `gap` to `b`.
fn too_close(a: &Rect, b: &Rect, gap: f64) -> bool {
    a.x < b.x + b.w + gap && b.x < a.x + a.w + gap && a.y < b.y + b.h + gap && b.y < a.y + a.h + gap
}

/// The free spot for a `w`×`h` rect nearest `anchor`, keeping `gap` from every rect of `placed`,
/// with its corner at or beyond `min` and its right edge at most `max_x`. Spots inside `room`
/// (right and bottom edges, from the origin) win over spots that would grow it; among those, the
/// least growth wins, then the nearest. Candidates are corners next to placed rects (the anchor's
/// row and column included), so a rect lands flush beside, below or above a neighbour.
/// Deterministic: ties go to the smaller y, then x.
#[allow(clippy::too_many_arguments)]
pub fn free_spot(
    placed: &[Rect],
    w: f64,
    h: f64,
    anchor: (f64, f64),
    min: (f64, f64),
    max_x: f64,
    room: (f64, f64),
    gap: f64,
) -> (f64, f64) {
    let ax = anchor.0.max(min.0);
    let ay = anchor.1.max(min.1);
    let mut near: Vec<(f64, usize)> = placed
        .iter()
        .enumerate()
        .map(|(i, q)| {
            let (cx, cy) = (q.x + q.w / 2.0 - ax, q.y + q.h / 2.0 - ay);
            (cx * cx + cy * cy, i)
        })
        .collect();
    if near.len() > FREE_NEAR {
        near.select_nth_unstable_by(FREE_NEAR, |a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        near.truncate(FREE_NEAR);
    }
    let mut xs = vec![min.0, ax];
    let mut ys = vec![min.1, ay];
    for &(_, i) in &near {
        let q = &placed[i];
        xs.extend([q.x, q.x + q.w + gap, q.x - gap - w]);
        ys.extend([q.y, q.y + q.h + gap, q.y - gap - h]);
    }
    let ok_x = |x: f64| x >= min.0 - 1e-6 && x + w <= max_x + 1e-6;
    let ok_y = |y: f64| y >= min.1 - 1e-6;
    xs.retain(|&x| ok_x(x));
    ys.retain(|&y| ok_y(y));
    xs.sort_by(f64::total_cmp);
    xs.dedup();
    ys.sort_by(f64::total_cmp);
    ys.dedup();
    let mut cand: Vec<(f64, f64)> = Vec::with_capacity(xs.len() * ys.len() + 2 * placed.len());
    for &x in &xs {
        for &y in &ys {
            cand.push((x, y));
        }
    }
    for q in placed {
        for c in [(q.x, q.y + q.h + gap), (q.x + q.w + gap, q.y)] {
            if ok_x(c.0) && ok_y(c.1) {
                cand.push(c);
            }
        }
    }
    let mut keyed: Vec<(f64, f64, f64, f64)> = cand
        .into_iter()
        .map(|(x, y)| {
            let grow = (x + w - room.0).max(0.0) + (y + h - room.1).max(0.0);
            let (dx, dy) = (x - ax, y - ay);
            (grow, dx * dx + dy * dy, y, x)
        })
        .collect();
    let order = |a: &(f64, f64, f64, f64), b: &(f64, f64, f64, f64)| {
        a.0.total_cmp(&b.0)
            .then(a.1.total_cmp(&b.1))
            .then(a.2.total_cmp(&b.2))
            .then(a.3.total_cmp(&b.3))
    };
    // Overlap tests against the placed rects sorted by x (a window of x bounded by the widest).
    let mut by_x: Vec<&Rect> = placed.iter().collect();
    by_x.sort_by(|a, b| a.x.total_cmp(&b.x));
    let widest = placed.iter().map(|q| q.w).fold(0.0, f64::max);
    // Best candidates first, sorted in growing chunks: the first free one usually comes early.
    let (mut lo, mut chunk) = (0, 64);
    while lo < keyed.len() {
        let hi = (lo + chunk).min(keyed.len());
        if hi < keyed.len() {
            keyed[lo..].select_nth_unstable_by(hi - lo - 1, order);
        }
        keyed[lo..hi].sort_by(order);
        for &(_, _, y, x) in &keyed[lo..hi] {
            let r = Rect { x, y, w, h };
            let first = by_x.partition_point(|q| q.x < x - widest - gap);
            if by_x[first..]
                .iter()
                .take_while(|q| q.x < x + w + gap)
                .all(|q| !too_close(&r, q, gap))
            {
                return (x, y);
            }
        }
        lo = hi;
        chunk *= 4;
    }
    // Below everything is always free.
    let bottom = placed.iter().map(|q| q.y + q.h + gap).fold(min.1, f64::max);
    (min.0, bottom)
}

/// Incremental placement inside a continuing cluster (§4.10.6): windows that were in this
/// cluster keep their offsets from its origin. A resized window stays where it is unless that now
/// overlaps a neighbour, and then takes the nearest free spot; a window new to the cluster goes
/// into free space below its previous same-file fragment (else above the next one), else beside
/// a window it is linked to. Free spots inside the old cluster bounds win, so the cluster grows
/// only when it must. Writes rects relative to the cluster origin and returns the cluster size;
/// `None` when no member was in the cluster before (the caller lays it out afresh).
///
/// A window whose own heat grade changed keeps its offset too: the hotter-first order inside a
/// layer is a fresh-layout rule, and a settled grade change moves the whole cluster to its new
/// cell instead.
fn keep_cluster(
    members: &[usize],
    input: &Input,
    calls: &[Vec<usize>],
    prev: &Placement,
    pc: usize,
    pos: &mut [Rect],
) -> Option<(f64, f64)> {
    let pcl = &prev.clusters[pc];
    let (ox, oy) = (pcl.rect.x, pcl.rect.y);
    let mut placed: Vec<Rect> = Vec::with_capacity(members.len());
    let mut resized: Vec<(usize, Rect)> = Vec::new();
    let mut fresh: Vec<usize> = Vec::new();
    let mut done: Vec<usize> = Vec::with_capacity(members.len());
    for &w in members {
        let win = &input.windows[w];
        match prev.windows.get(&win.key) {
            Some(&(c, r)) if c == pc => {
                let at = Rect {
                    x: r.x - ox,
                    y: r.y - oy,
                    w: win.w,
                    h: win.h,
                };
                if r.w == win.w && r.h == win.h {
                    pos[w] = at;
                    placed.push(at);
                    done.push(w);
                } else {
                    resized.push((w, at));
                }
            }
            _ => fresh.push(w),
        }
    }
    if placed.is_empty() && resized.is_empty() {
        return None;
    }
    let min = (CLUSTER_PAD, CLUSTER_TITLE);
    let room = (pcl.rect.w - CLUSTER_PAD, pcl.rect.h - CLUSTER_PAD);
    // A shrunk window never needs to move; a grown one moves only when it now overlaps.
    for (w, at) in resized {
        let (x, y) = if placed.iter().any(|q| too_close(&at, q, WIN_GAP)) {
            free_spot(
                &placed,
                at.w,
                at.h,
                (at.x, at.y),
                min,
                f64::INFINITY,
                room,
                WIN_GAP,
            )
        } else {
            (at.x, at.y)
        };
        pos[w] = Rect { x, y, ..at };
        placed.push(pos[w]);
        done.push(w);
    }
    // New windows in file and line order, so a file's new fragments chain one below the other.
    fresh.sort_by(|&a, &b| {
        let (wa, wb) = (&input.windows[a], &input.windows[b]);
        input.files[wa.file]
            .path
            .cmp(&input.files[wb.file].path)
            .then(wa.start_line.cmp(&wb.start_line))
            .then(a.cmp(&b))
    });
    for w in fresh {
        let win = &input.windows[w];
        let same_file = |before: bool| {
            done.iter()
                .copied()
                .filter(|&o| {
                    let ow = &input.windows[o];
                    ow.file == win.file && (ow.start_line < win.start_line) == before
                })
                .min_by_key(|&o| (input.windows[o].start_line.abs_diff(win.start_line), o))
        };
        let anchor = if let Some(o) = same_file(true) {
            (pos[o].x, pos[o].y + pos[o].h + WIN_GAP)
        } else if let Some(o) = same_file(false) {
            (pos[o].x, pos[o].y - WIN_GAP - win.h)
        } else if let Some(&o) = done.iter().find(|&&o| calls[o].contains(&w)) {
            (pos[o].x + pos[o].w + LAYER_GAP, pos[o].y)
        } else if let Some(&o) = done.iter().find(|&&o| calls[w].contains(&o)) {
            (pos[o].x - LAYER_GAP - win.w, pos[o].y)
        } else {
            min
        };
        let (x, y) = free_spot(
            &placed,
            win.w,
            win.h,
            anchor,
            min,
            f64::INFINITY,
            room,
            WIN_GAP,
        );
        pos[w] = Rect {
            x,
            y,
            w: win.w,
            h: win.h,
        };
        placed.push(pos[w]);
        done.push(w);
    }
    let right = placed.iter().map(|r| r.x + r.w).fold(0.0, f64::max);
    let bottom = placed.iter().map(|r| r.y + r.h).fold(0.0, f64::max);
    // The old size when the extent did not change (offsets round-trip through world space).
    let snap = |new: f64, old: f64| if (new - old).abs() < 1e-6 { old } else { new };
    Some((
        snap(right + CLUSTER_PAD, pcl.rect.w),
        snap(bottom + CLUSTER_PAD, pcl.rect.h),
    ))
}

/// Above this many adjacent-layer edges a cluster skips crossing minimisation.
const MAX_SWEEP_EDGES: usize = 1500;

/// Barycenter sweeps (down, then up) over the layered order, keeping the best order seen.
///
/// The starting order is "hotter files first, same-file windows adjacent" (§4.10.4). Barycenters
/// are a stable re-sort in which a window without neighbours in the reference layer keeps its
/// current slot as its key, and ties keep the current order; a sweep's result is kept only when it
/// strictly lowers the crossing count. So the hotter-first order survives wherever links do not
/// ask for another one, and with no crossings to remove nothing moves. Edges are the layering's
/// forward edges between adjacent layers (a longer edge does not constrain the order).
/// Deterministic: every sort has a total tie-break.
fn reduce_crossings(layers: &mut [Vec<usize>], layer: &[usize], keep: &[Vec<usize>]) {
    if layers.len() < 2 {
        return;
    }
    let n = layer.len();
    // Adjacent-layer neighbours: below (next layer) and above (previous layer).
    let mut down: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut up: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (v, ts) in keep.iter().enumerate() {
        for &t in ts {
            if layer[t] == layer[v] + 1 {
                down[v].push(t);
                up[t].push(v);
            }
        }
    }
    // Crossing counts are quadratic in the edges between two layers; clusters are small
    // (≤ MAX_CLUSTER windows), and an oversized one keeps the hotter-first order.
    if down.iter().map(Vec::len).sum::<usize>() > MAX_SWEEP_EDGES {
        return;
    }
    let mut slot = vec![0usize; n];
    let set_slots = |layers: &[Vec<usize>], slot: &mut [usize]| {
        for l in layers {
            for (i, &v) in l.iter().enumerate() {
                slot[v] = i;
            }
        }
    };
    let crossings = |layers: &[Vec<usize>], slot: &[usize]| -> usize {
        let mut total = 0;
        for l in &layers[..layers.len() - 1] {
            let mut e: Vec<(usize, usize)> = l
                .iter()
                .flat_map(|&v| down[v].iter().map(move |&t| (slot[v], slot[t])))
                .collect();
            e.sort_unstable();
            for i in 0..e.len() {
                for j in i + 1..e.len() {
                    if e[i].0 < e[j].0 && e[i].1 > e[j].1 {
                        total += 1;
                    }
                }
            }
        }
        total
    };
    set_slots(layers, &mut slot);
    let mut best = crossings(layers, &slot);
    if best == 0 {
        return;
    }
    let mut cur: Vec<Vec<usize>> = layers.to_vec();
    for round in 0..8 {
        let downward = round % 2 == 0;
        let order: Vec<usize> = if downward {
            (1..cur.len()).collect()
        } else {
            (0..cur.len() - 1).rev().collect()
        };
        for li in order {
            let key = |v: usize, slot: &[usize]| -> f64 {
                let nb = if downward { &up[v] } else { &down[v] };
                if nb.is_empty() {
                    slot[v] as f64
                } else {
                    nb.iter().map(|&u| slot[u] as f64).sum::<f64>() / nb.len() as f64
                }
            };
            let mut keyed: Vec<(f64, usize, usize)> = cur[li]
                .iter()
                .map(|&v| (key(v, &slot), slot[v], v))
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
            cur[li] = keyed.into_iter().map(|k| k.2).collect();
            for (i, &v) in cur[li].iter().enumerate() {
                slot[v] = i;
            }
        }
        let c = crossings(&cur, &slot);
        if c < best {
            best = c;
            layers.clone_from_slice(&cur);
            if best == 0 {
                break;
            }
        }
    }
}

/// Percentile ranks with ties sharing their mean rank, in 0..=1.
fn pct_rank(values: &[f64]) -> Vec<f64> {
    let mut idx: Vec<usize> = (0..values.len()).collect();
    idx.sort_by(|&a, &b| values[a].total_cmp(&values[b]).then(a.cmp(&b)));
    let mut out = vec![0.0; values.len()];
    let denom = (values.len().max(2) - 1) as f64;
    let mut i = 0;
    while i < idx.len() {
        let mut j = i;
        while j + 1 < idx.len() && values[idx[j + 1]] == values[idx[i]] {
            j += 1;
        }
        let r = (i + j) as f64 / 2.0 / denom;
        for &k in &idx[i..=j] {
            out[k] = r;
        }
        i = j + 1;
    }
    out
}

/// Modules with the previous cluster each continues (if any) and their subsystem.
struct Partition {
    modules: Vec<Vec<usize>>,
    ident: Vec<Option<usize>>,
    subsystem: Vec<usize>,
}

fn fresh_partition(input: &Input, adj: &[Vec<(usize, f64)>], by_lane: &[Vec<usize>]) -> Partition {
    let mut modules = Vec::new();
    let mut subsystem = Vec::new();
    let mut next_sub = 0;
    for lane_wins in by_lane {
        if lane_wins.is_empty() {
            continue;
        }
        let ms = cluster::fresh_modules(input, lane_wins, adj);
        let subs = cluster::subsystems(&ms, adj, input.windows.len());
        let n_subs = subs.iter().max().map(|m| m + 1).unwrap_or(0);
        subsystem.extend(subs.iter().map(|s| next_sub + s));
        next_sub += n_subs;
        modules.extend(ms);
    }
    let ident = vec![None; modules.len()];
    Partition {
        modules,
        ident,
        subsystem,
    }
}

/// Keeps the previous clusters; new windows join the cluster they are most linked to (or the one
/// holding their file's other windows), and the rest are pooled by directory into new clusters.
fn incremental_partition(
    input: &Input,
    prev: &Placement,
    adj: &[Vec<(usize, f64)>],
    by_lane: &[Vec<usize>],
) -> Partition {
    let nw = input.windows.len();
    let mut group_of = vec![usize::MAX; nw];
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut fresh: Vec<usize> = Vec::new();
    for lane_wins in by_lane {
        for &w in lane_wins {
            let lane = input.lanes[input.files[input.windows[w].file].lane];
            match prev.windows.get(&input.windows[w].key) {
                Some(&(pc, _)) if prev.clusters.get(pc).is_some_and(|c| c.lane == lane) => {
                    group_of[w] = pc;
                    groups.entry(pc).or_default().push(w);
                }
                _ => fresh.push(w),
            }
        }
    }
    let mut file_group: HashMap<usize, BTreeMap<usize, usize>> = HashMap::new();
    for (&g, ws) in &groups {
        for &w in ws {
            *file_group
                .entry(input.windows[w].file)
                .or_default()
                .entry(g)
                .or_default() += 1;
        }
    }
    let mut pending = Vec::new();
    for w in fresh {
        let mut to: BTreeMap<usize, f64> = BTreeMap::new();
        for &(u, x) in &adj[w] {
            if group_of[u] != usize::MAX {
                *to.entry(group_of[u]).or_default() += x;
            }
        }
        // A new fragment of a placed file is pulled to its siblings.
        if let Some(fg) = file_group.get(&input.windows[w].file) {
            for (&g, &n) in fg {
                *to.entry(g).or_default() += cluster::COHESION * n as f64;
            }
        }
        let best = to
            .into_iter()
            .filter(|(g, _)| groups.get(g).is_some_and(|v| v.len() < MAX_CLUSTER))
            .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)));
        match best {
            Some((g, _)) => {
                group_of[w] = g;
                groups.entry(g).or_default().push(w);
            }
            None => pending.push(w),
        }
    }
    let mut modules = Vec::new();
    let mut ident = Vec::new();
    let mut subsystem = Vec::new();
    for (g, mut ws) in groups {
        ws.sort_by(|&a, &b| cluster::win_order(input, a, b));
        modules.push(ws);
        ident.push(Some(g));
        subsystem.push(prev.clusters[g].subsystem);
    }
    let first_new = prev
        .clusters
        .iter()
        .map(|c| c.subsystem + 1)
        .max()
        .unwrap_or(0);
    for (i, ws) in cluster::directory_pools(input, &pending)
        .into_iter()
        .enumerate()
    {
        modules.push(ws);
        ident.push(None);
        subsystem.push(first_new + i);
    }
    Partition {
        modules,
        ident,
        subsystem,
    }
}

/// Cluster heat: change-mass quantiles over clusters sorted by mass per line (as files are graded),
/// per lane (DESIGN §4.10.3: Hot holds the clusters with the first 50% of *the lane's* change
/// mass), so a lane with little churn still spreads over the columns. Returns (grade, settled,
/// level) per module: `level` is the continuous heat on 0..1 (see `ClusterOut::heat_level`).
pub(crate) fn cluster_heat(
    input: &Input,
    modules: &[Vec<usize>],
    file_h: &[f64],
    lane_of: &[usize],
) -> Vec<(u8, bool, f64)> {
    let mass: Vec<(f64, f64, f64)> = modules
        .iter()
        .map(|ms| {
            let (mut m, mut settled, mut h) = (0.0, 0.0, 0.0);
            for &w in ms {
                let win = &input.windows[w];
                // The file's rate, shared among its windows by height.
                let share = if file_h[win.file] > 0.0 {
                    win.h / file_h[win.file]
                } else {
                    0.0
                };
                m += win.rate * share;
                if win.settled {
                    settled += win.rate * share;
                }
                h += win.h;
            }
            (m, settled, h)
        })
        .collect();
    let mut out = vec![(0u8, true, 0.0); modules.len()];
    for lane in 0..input.lanes.len() {
        let mut idx: Vec<usize> = (0..modules.len()).filter(|&m| lane_of[m] == lane).collect();
        let total: f64 = idx.iter().map(|&m| mass[m].0).sum();
        idx.sort_by(|&a, &b| {
            let da = mass[a].0 / mass[a].2.max(1.0);
            let db = mass[b].0 / mass[b].2.max(1.0);
            db.total_cmp(&da).then(a.cmp(&b))
        });
        let mut acc = 0.0;
        let cut = input.grade_cuts.mass;
        for i in idx {
            let (m, s, _) = mass[i];
            let (g, level) = if m <= 0.0 || total <= 0.0 {
                (0, 0.0)
            } else {
                let share = acc / total + 1e-9;
                // The grade's share interval [lo, hi): the hotter end of it is the top of the grade.
                let (g, lo, hi) = if share < cut[0] {
                    (4, 0.0, cut[0])
                } else if share < cut[1] {
                    (3, cut[0], cut[1])
                } else if share < cut[2] {
                    (2, cut[1], cut[2])
                } else {
                    (1, cut[2], 1.0)
                };
                let f = if hi > lo {
                    1.0 - (share - lo) / (hi - lo)
                } else {
                    0.5
                };
                (g, (g as f64 + f.clamp(0.0, 0.999)) / 5.0)
            };
            acc += m;
            out[i] = (g, m <= 0.0 || s / m >= SETTLED_SHARE, level);
        }
    }
    out
}

/// Cluster connectedness: rank quantiles per lane of each cluster's most-depended-on window.
pub(crate) fn cluster_conn(input: &Input, modules: &[Vec<usize>], lane_of: &[usize]) -> Vec<u8> {
    let metric: Vec<u32> = modules
        .iter()
        .map(|ms| {
            ms.iter()
                .map(|&w| input.windows[w].dependents2)
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = vec![0u8; modules.len()];
    for lane in 0..input.lanes.len() {
        let mut nz: Vec<u32> = (0..modules.len())
            .filter(|&m| lane_of[m] == lane && metric[m] > 0)
            .map(|m| metric[m])
            .collect();
        if nz.is_empty() {
            continue;
        }
        nz.sort_unstable();
        let q = |p: f64| nz[((p * nz.len() as f64 + 1e-9) as usize).min(nz.len() - 1)];
        let [r0, r1, r2] = input.grade_cuts.rank;
        let (t50, t75, t90) = (q(r0), q(r1), q(r2));
        for m in 0..modules.len() {
            if lane_of[m] != lane {
                continue;
            }
            let v = metric[m];
            out[m] = if v == 0 {
                0
            } else if v >= t90 {
                4
            } else if v >= t75 {
                3
            } else if v >= t50 {
                2
            } else {
                1
            };
        }
    }
    out
}

fn jaccard(a: &[String], b: &BTreeSet<&str>) -> f64 {
    let inter = a.iter().filter(|k| b.contains(k.as_str())).count();
    let union = a.len() + b.len() - inter;
    if union == 0 {
        0.0
    } else {
        inter as f64 / union as f64
    }
}

/// Fresh packing order of a cell: subsystems together (largest first); inside, a chain that
/// always continues with the cluster most attracted to the last one (shared files, then links).
fn attraction_order(
    cell: &[usize],
    sizes: &[(f64, f64)],
    subsystem: &[usize],
    files_of: &[BTreeSet<usize>],
    link_w: &HashMap<(usize, usize), f64>,
) -> Vec<usize> {
    let mut by_sub: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for &c in cell {
        by_sub.entry(subsystem[c]).or_default().push(c);
    }
    let area = |c: usize| sizes[c].0 * sizes[c].1;
    let mut subs: Vec<(usize, Vec<usize>)> = by_sub.into_iter().collect();
    subs.sort_by(|a, b| {
        let sa: f64 = a.1.iter().map(|&c| area(c)).sum();
        let sb: f64 = b.1.iter().map(|&c| area(c)).sum();
        sb.total_cmp(&sa).then(a.0.cmp(&b.0))
    });
    let mut out = Vec::with_capacity(cell.len());
    for (_, mut rest) in subs {
        rest.sort_by(|&a, &b| area(b).total_cmp(&area(a)).then(a.cmp(&b)));
        let mut last = rest.remove(0);
        out.push(last);
        while !rest.is_empty() {
            let pull = |c: usize| {
                let shared = files_of[c].intersection(&files_of[last]).count() as f64;
                let lw = link_w
                    .get(&(c.min(last), c.max(last)))
                    .copied()
                    .unwrap_or(0.0);
                shared * 100.0 + lw
            };
            let mut best = (0, f64::MIN);
            for (i, &c) in rest.iter().enumerate() {
                let p = pull(c);
                if p > best.1 {
                    best = (i, p);
                }
            }
            last = rest.remove(best.0);
            out.push(last);
        }
    }
    out
}

/// Risk per window (review priority): geometric mean of the heat and connectedness percentiles
/// within the window's lane (lanes differ in churn and in what depends on them), times the
/// boundary factor ([`structure::boundary_factor`]): code at its module's edge ranks above code
/// whose links stay inside. A window that never changes, or that
/// nothing depends on, carries no risk: tied zeros would otherwise take a mid rank and fill a
/// quiet lane's top 5% with untouched or isolated windows. Top 5% per lane review first.
pub(crate) fn risk(input: &Input) -> (Vec<f64>, Vec<bool>) {
    let nw = input.windows.len();
    // Links inside and outside each window's module (its directory and below), as §4.11 counts
    // them: distinct window pairs, the test and docs lanes left out.
    let counted = |w: usize| {
        let lane = input.lanes[input.files[input.windows[w].file].lane];
        !structure::SKIP_LANES.contains(&lane)
    };
    let dir = |w: usize| dirname(&input.files[input.windows[w].file].path);
    let mut sides = vec![(0u32, 0u32); nw];
    let pairs: BTreeSet<(usize, usize)> = input
        .links
        .iter()
        .filter(|l| l.from != l.to && counted(l.from) && counted(l.to))
        .map(|l| (l.from, l.to))
        .collect();
    for (a, b) in pairs {
        for (w, other) in [(a, b), (b, a)] {
            if structure::below(dir(other), dir(w)) {
                sides[w].0 += 1;
            } else {
                sides[w].1 += 1;
            }
        }
    }
    let mut risk = vec![0.0; nw];
    let mut review_first = vec![false; nw];
    for li in 0..input.lanes.len() {
        let ws: Vec<usize> = (0..nw)
            .filter(|&w| input.files[input.windows[w].file].lane == li)
            .collect();
        let hp = pct_rank(
            &ws.iter()
                .map(|&w| input.windows[w].rate)
                .collect::<Vec<_>>(),
        );
        let cp = pct_rank(
            &ws.iter()
                .map(|&w| input.windows[w].dependents2 as f64)
                .collect::<Vec<_>>(),
        );
        for (i, &w) in ws.iter().enumerate() {
            let win = &input.windows[w];
            if win.rate > 0.0 && win.dependents2 > 0 {
                risk[w] =
                    (hp[i] * cp[i]).sqrt() * structure::boundary_factor(sides[w].0, sides[w].1);
            }
        }
        let mut idx: Vec<usize> = ws.iter().copied().filter(|&w| risk[w] > 0.0).collect();
        idx.sort_by(|&a, &b| risk[b].total_cmp(&risk[a]).then(a.cmp(&b)));
        for &w in idx.iter().take(ws.len().div_ceil(20)) {
            review_first[w] = true;
        }
    }
    (risk, review_first)
}

pub fn layout(input: &Input) -> Output {
    let nw = input.windows.len();
    let mut file_windows: Vec<Vec<usize>> = vec![Vec::new(); input.files.len()];
    for (i, w) in input.windows.iter().enumerate() {
        file_windows[w.file].push(i);
    }
    for fw in file_windows.iter_mut() {
        fw.sort_by_key(|&w| (input.windows[w].start_line, w));
    }
    let file_h: Vec<f64> = file_windows
        .iter()
        .map(|ws| ws.iter().map(|&w| input.windows[w].h).sum())
        .collect();
    let mut calls: Vec<Vec<usize>> = vec![Vec::new(); nw];
    for l in input.links {
        if l.from != l.to && l.kind != EdgeKind::Import {
            calls[l.from].push(l.to);
        }
    }
    if input.arrangement.is_tree() {
        return folders::layout(input, &file_h, &calls);
    }
    let weights = cluster::weights(input, &file_windows);
    let adj = cluster::adjacency(nw, &weights);
    let mut by_lane: Vec<Vec<usize>> = vec![Vec::new(); input.lanes.len()];
    {
        let mut order: Vec<usize> = (0..nw).collect();
        order.sort_by(|&a, &b| cluster::win_order(input, a, b));
        for w in order {
            by_lane[input.files[input.windows[w].file].lane].push(w);
        }
    }

    // 1. Modules (and subsystems).
    let prev = input.prev.filter(|_| !input.full);
    let part = match prev {
        Some(p) => incremental_partition(input, p, &adj, &by_lane),
        None => fresh_partition(input, &adj, &by_lane),
    };
    let nm = part.modules.len();
    let lane_of: Vec<usize> = part
        .modules
        .iter()
        .map(|ms| input.files[input.windows[ms[0]].file].lane)
        .collect();
    let mut cluster_of = vec![usize::MAX; nw];
    for (m, ms) in part.modules.iter().enumerate() {
        for &w in ms {
            cluster_of[w] = m;
        }
    }
    let keys: Vec<Vec<String>> = part
        .modules
        .iter()
        .map(|ms| {
            let mut k: Vec<String> = ms.iter().map(|&w| input.windows[w].key.clone()).collect();
            k.sort();
            k
        })
        .collect();

    // 2. Inner layout: unchanged clusters keep their windows' relative positions.
    let mut rel = vec![Rect::default(); nw];
    let mut sizes = vec![(0.0, 0.0); nm];
    let mut unchanged = vec![false; nm];
    for m in 0..nm {
        let reuse = part.ident[m].and_then(|pc| {
            let p = prev?;
            let pcl = &p.clusters[pc];
            if pcl.members != keys[m] {
                return None;
            }
            let mut out = Vec::new();
            for &w in &part.modules[m] {
                let win = &input.windows[w];
                let &(_, r) = p.windows.get(&win.key)?;
                if r.w != win.w || r.h != win.h {
                    return None;
                }
                out.push((
                    w,
                    Rect {
                        x: r.x - pcl.rect.x,
                        y: r.y - pcl.rect.y,
                        w: r.w,
                        h: r.h,
                    },
                ));
            }
            Some((out, (pcl.rect.w, pcl.rect.h)))
        });
        match reuse {
            Some((rects, size)) => {
                for (w, r) in rects {
                    rel[w] = r;
                }
                sizes[m] = size;
                unchanged[m] = true;
            }
            None => {
                // A continuing cluster keeps its windows' offsets; only a new one (or one with
                // no survivor) is laid out afresh.
                let kept = match (part.ident[m], prev) {
                    (Some(pc), Some(p)) => {
                        keep_cluster(&part.modules[m], input, &calls, p, pc, &mut rel)
                    }
                    _ => None,
                };
                sizes[m] = match kept {
                    Some(size) => size,
                    None => place_cluster(&part.modules[m], input, &calls, &mut rel),
                };
            }
        }
    }

    // 3. Grades and cells: a heat change moves a continuing cluster only once it is settled.
    let heat = cluster_heat(input, &part.modules, &file_h, &lane_of);
    let conn = cluster_conn(input, &part.modules, &lane_of);
    let cell: Vec<(u8, u8)> = (0..nm)
        .map(|m| {
            let (h, settled, _) = heat[m];
            match (part.ident[m], prev) {
                (Some(pc), Some(p)) if !settled => (p.clusters[pc].heat, conn[m]),
                _ => (h, conn[m]),
            }
        })
        .collect();

    // 4. Labels: automatic, with human / agent labels carried over by member Jaccard.
    let mut labels: Vec<(String, String)> = (0..nm)
        .map(|m| {
            (
                cluster::auto_label(input, &part.modules[m]),
                "auto".to_string(),
            )
        })
        .collect();
    if let Some(p) = input.prev {
        for pc in p.clusters.iter().filter(|c| c.label_source != "auto") {
            let pset: BTreeSet<&str> = pc.members.iter().map(String::as_str).collect();
            // Own lane first, then any lane (§4.10.6): the members may have changed lane (a
            // `.cv/lanes.toml` rule, a moved file) and the label goes with them.
            let best_in = |own: bool| {
                (0..nm)
                    .filter(|&m| (input.lanes[lane_of[m]] == pc.lane) == own)
                    .map(|m| (m, jaccard(&keys[m], &pset)))
                    .filter(|&(_, j)| j >= LABEL_JACCARD)
                    .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)))
            };
            if let Some((m, _)) = best_in(true).or_else(|| best_in(false)) {
                labels[m] = (pc.label.clone(), pc.label_source.clone());
            }
        }
    }
    // A repeated auto label in a lane is told apart by its main file, then by ordinals.
    for round in 0..2 {
        let mut dup: HashMap<(usize, String), u32> = HashMap::new();
        for m in 0..nm {
            if labels[m].1 == "auto" {
                *dup.entry((lane_of[m], labels[m].0.clone())).or_default() += 1;
            }
        }
        let mut order: Vec<usize> = (0..nm).collect();
        order.sort_by(|&a, &b| cluster::win_order(input, part.modules[a][0], part.modules[b][0]));
        let mut seen: HashMap<(usize, String), u32> = HashMap::new();
        for m in order {
            let k = (lane_of[m], labels[m].0.clone());
            if labels[m].1 != "auto" || dup.get(&k).copied().unwrap_or(0) < 2 {
                continue;
            }
            if round == 0 {
                let stem = cluster::top_file_stem(input, &part.modules[m]);
                if !labels[m].0.ends_with(&stem) {
                    labels[m].0 = format!("{} · {stem}", labels[m].0);
                }
            } else {
                let n = seen.entry(k).or_insert(0);
                *n += 1;
                labels[m].0 = format!("{} · {}", labels[m].0, n);
            }
        }
    }

    // 5. District grid.
    let col_of = |heat: u8| (4 - heat.min(4)) as usize;
    let files_of: Vec<BTreeSet<usize>> = part
        .modules
        .iter()
        .map(|ms| ms.iter().map(|&w| input.windows[w].file).collect())
        .collect();
    let mut link_w: HashMap<(usize, usize), f64> = HashMap::new();
    for (&(a, b), &x) in &weights {
        let (ca, cb) = (cluster_of[a], cluster_of[b]);
        if ca != cb {
            *link_w.entry((ca.min(cb), ca.max(cb))).or_default() += x;
        }
    }
    // Column widths: proportional to cluster area (fresh), never narrower than before. A column
    // empty in every lane collapses to zero width and no gap (D8), unless an earlier layout of
    // this repo gave it room (columns only grow, so continuing clusters keep their x).
    let mut col_area = [0f64; 5];
    let mut col_max = [0f64; 5];
    let mut col_used = [false; 5];
    for m in 0..nm {
        let c = col_of(cell[m].0);
        col_area[c] += (sizes[m].0 + CELL_GAP) * (sizes[m].1 + CELL_GAP);
        col_max[c] = col_max[c].max(sizes[m].0);
        col_used[c] = true;
    }
    let h0 = (col_area.iter().sum::<f64>() / 1.6).sqrt().max(1.0);
    let mut col_w = [0f64; 5];
    for col in 0..5 {
        let prev_w = prev.map_or(0.0, |p| p.col_w[col]);
        col_w[col] = match (col_used[col], prev) {
            (false, _) => prev_w,
            (true, Some(_)) => prev_w.max(col_max[col] + 2.0 * CELL_PAD),
            (true, None) => col_max[col].max(col_area[col] / h0 * 1.15) + 2.0 * CELL_PAD,
        };
    }
    let mut col_x = [0f64; 5];
    let mut x: f64 = 0.0;
    let mut world_w: f64 = 0.0;
    for col in 0..5 {
        col_x[col] = x;
        if col_w[col] > 0.0 {
            world_w = x + col_w[col];
            x = world_w + COL_GAP;
        }
    }

    let mut cells: BTreeMap<(usize, u8, usize), Vec<usize>> = BTreeMap::new();
    for m in 0..nm {
        cells
            .entry((lane_of[m], cell[m].1, col_of(cell[m].0)))
            .or_default()
            .push(m);
    }
    // Previous geometry: lane id → conn → (y, h).
    let prev_rows: HashMap<&str, BTreeMap<u8, (f64, f64)>> = prev
        .map(|p| {
            p.lanes
                .iter()
                .map(|(l, _, rows)| {
                    (
                        l.as_str(),
                        rows.iter().map(|&(c, y, h)| (c, (y, h))).collect(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    // Pack each cell relative to its inner origin; previous offsets are kept where they still fit.
    let mut offset = vec![(0.0, 0.0); nm];
    let mut cell_h: BTreeMap<(usize, u8, usize), f64> = BTreeMap::new();
    for (&(lane, conn_g, col), ms) in &cells {
        let inner = col_w[col] - 2.0 * CELL_PAD;
        let mut placed: Vec<Rect> = Vec::new();
        // Clusters still to place, each with the offset it would like (its old one, if any).
        let mut rest: Vec<(usize, (f64, f64))> = Vec::new();
        let prev_origin = prev.and_then(|p| {
            let row = prev_rows.get(input.lanes[lane])?.get(&conn_g)?;
            Some((p.col_x[col] + CELL_PAD, row.0 + CELL_PAD))
        });
        // Unchanged clusters claim their old spots first, then changed ones that still fit.
        let mut keepers: Vec<usize> = ms
            .iter()
            .copied()
            .filter(|&m| part.ident[m].is_some())
            .collect();
        keepers.sort_by_key(|&m| (!unchanged[m], m));
        for &m in &keepers {
            let spot = match (prev, prev_origin, part.ident[m]) {
                (Some(p), Some(o), Some(pc)) => {
                    let pcl = &p.clusters[pc];
                    ((pcl.heat, pcl.conn) == cell[m]).then(|| Rect {
                        x: pcl.rect.x - o.0,
                        y: pcl.rect.y - o.1,
                        w: sizes[m].0,
                        h: sizes[m].1,
                    })
                }
                _ => None,
            };
            match spot {
                Some(r)
                    if r.x >= 0.0
                        && r.y >= 0.0
                        && r.x + r.w <= inner + 1e-6
                        && !placed.iter().any(|q| q.intersects(&r)) =>
                {
                    offset[m] = (r.x, r.y);
                    placed.push(r);
                }
                Some(r) => rest.push((m, (r.x, r.y))),
                None => rest.push((m, (0.0, 0.0))),
            }
        }
        let newcomers: Vec<usize> = ms
            .iter()
            .copied()
            .filter(|&m| part.ident[m].is_none())
            .collect();
        if !newcomers.is_empty() {
            rest.extend(
                attraction_order(&newcomers, &sizes, &part.subsystem, &files_of, &link_w)
                    .into_iter()
                    .map(|m| (m, (0.0, 0.0))),
            );
        }
        if prev.is_some() {
            // §4.10.6: the rest goes into free space of the cell, nearest its old spot, inside
            // the row's current height where it fits: growing the row would move every row
            // below it.
            let room_h = prev_rows
                .get(input.lanes[lane])
                .and_then(|r| r.get(&conn_g))
                .map(|r| r.1 - 2.0 * CELL_PAD)
                .unwrap_or_else(|| placed.iter().map(|r| r.y + r.h).fold(0.0, f64::max));
            for (m, anchor) in rest {
                let (w, h) = sizes[m];
                let (x, y) = free_spot(
                    &placed,
                    w,
                    h,
                    anchor,
                    (0.0, 0.0),
                    inner.max(w),
                    (inner, room_h),
                    CELL_GAP,
                );
                offset[m] = (x, y);
                placed.push(Rect { x, y, w, h });
            }
            let h = placed.iter().map(|r| r.y + r.h).fold(0.0, f64::max);
            cell_h.insert((lane, conn_g, col), h);
            continue;
        }
        // A fresh layout shelf-packs the cell.
        let (mut cx, mut cy, mut shelf) = (0.0, 0.0, 0f64);
        for (m, _) in rest {
            let (w, h) = sizes[m];
            if cx > 0.0 && cx + w > inner {
                cx = 0.0;
                cy += shelf + CELL_GAP;
                shelf = 0.0;
            }
            offset[m] = (cx, cy);
            placed.push(Rect { x: cx, y: cy, w, h });
            cx += w + CELL_GAP;
            shelf = shelf.max(h);
        }
        let h = placed.iter().map(|r| r.y + r.h).fold(0.0, f64::max);
        cell_h.insert((lane, conn_g, col), h);
    }

    // Rows and lanes, top to bottom. Rows only grow; a previous row stays even when empty.
    let mut lane_bands = Vec::new();
    let mut row_y: HashMap<(usize, u8), f64> = HashMap::new();
    let mut placement_lanes = Vec::new();
    let mut y = HEADER_BAND;
    for (li, lane_id) in input.lanes.iter().enumerate() {
        let prev_lane = prev_rows.get(lane_id);
        if !(0..nm).any(|m| lane_of[m] == li) && prev_lane.is_none() {
            continue;
        }
        let lane_top = y;
        y += LANE_PAD;
        let mut rows = Vec::new();
        let mut prows = Vec::new();
        for conn_g in (0..=4u8).rev() {
            let needed = (0..5)
                .filter_map(|col| cell_h.get(&(li, conn_g, col)).copied())
                .reduce(f64::max);
            let prev_h = prev_lane.and_then(|r| r.get(&conn_g)).map(|r| r.1);
            let h = match (needed, prev_h) {
                (None, None) => continue,
                (Some(n), None) if prev.is_some() => n + 2.0 * CELL_PAD,
                (Some(n), None) => n * (1.0 + ROW_SLACK) + 2.0 * CELL_PAD,
                (None, Some(p)) => p,
                (Some(n), Some(p)) => p.max(n + 2.0 * CELL_PAD),
            };
            row_y.insert((li, conn_g), y);
            rows.push(LaneRow { conn: conn_g, y, h });
            prows.push((conn_g, y, h));
            y += h + ROW_GAP;
        }
        y += LANE_PAD - ROW_GAP;
        lane_bands.push(LaneBand {
            lane: lane_id.to_string(),
            rect: Rect {
                x: 0.0,
                y: lane_top,
                w: world_w,
                h: y - lane_top,
            },
            files: input
                .files
                .iter()
                .enumerate()
                .filter(|(i, f)| f.lane == li && !file_windows[*i].is_empty())
                .count() as u32,
            clusters: (0..nm).filter(|&m| lane_of[m] == li).count() as u32,
            rows,
        });
        placement_lanes.push((lane_id.to_string(), lane_top, prows));
        y += LANE_GAP;
    }
    let world = Rect {
        x: 0.0,
        y: 0.0,
        w: world_w.max(0.0),
        h: (y - LANE_GAP).max(HEADER_BAND),
    };

    // World positions.
    let mut clusters: Vec<ClusterOut> = Vec::with_capacity(nm);
    let mut rects = vec![Rect::default(); nw];
    for m in 0..nm {
        let (h, c) = cell[m];
        let ry = row_y.get(&(lane_of[m], c)).copied().unwrap_or(0.0);
        let rect = Rect {
            x: col_x[col_of(h)] + CELL_PAD + offset[m].0,
            y: ry + CELL_PAD + offset[m].1,
            w: sizes[m].0,
            h: sizes[m].1,
        };
        for &w in &part.modules[m] {
            rects[w] = Rect {
                x: rect.x + rel[w].x,
                y: rect.y + rel[w].y,
                ..rel[w]
            };
        }
        clusters.push(ClusterOut {
            lane: lane_of[m],
            label: labels[m].0.clone(),
            label_source: labels[m].1.clone(),
            heat: h,
            conn: c,
            heat_now: heat[m].0,
            heat_settled: heat[m].1,
            // While an unsettled cluster keeps its old cell, its level stays inside that grade.
            heat_level: heat[m].2.clamp(h as f64 / 5.0, (h as f64 + 0.999) / 5.0),
            review: 0.0,
            members: part.modules[m].clone(),
            rect,
            parent: None,
            folder: None,
        });
    }
    // The placement remembers cluster positions; pins then override the output.
    let placement = Placement {
        windows: input
            .windows
            .iter()
            .enumerate()
            .map(|(w, win)| (win.key.clone(), (cluster_of[w], rects[w])))
            .collect(),
        clusters: (0..nm)
            .map(|m| PlacedCluster {
                lane: input.lanes[lane_of[m]].to_string(),
                label: clusters[m].label.clone(),
                label_source: clusters[m].label_source.clone(),
                heat: cell[m].0,
                conn: cell[m].1,
                rect: clusters[m].rect,
                members: keys[m].clone(),
                subsystem: part.subsystem[m],
            })
            .collect(),
        col_x,
        col_w,
        lanes: placement_lanes,
    };
    let pins: HashMap<&str, &Pin> = input.pins.iter().map(|p| (p.key.as_str(), p)).collect();
    for (w, win) in input.windows.iter().enumerate() {
        if let Some(p) = pins.get(win.key.as_str()) {
            rects[w].x = p.x;
            rects[w].y = p.y;
        }
    }

    // Subsystems with more than one cluster.
    let mut subs: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for m in 0..nm {
        subs.entry(part.subsystem[m]).or_default().push(m);
    }
    let mut subsystems = Vec::new();
    for (_, ms) in subs {
        if ms.len() < 2 {
            continue;
        }
        let all: Vec<usize> = ms
            .iter()
            .flat_map(|&m| part.modules[m].iter().copied())
            .collect();
        let si = subsystems.len();
        for &m in &ms {
            clusters[m].parent = Some(si);
        }
        subsystems.push(SubsystemOut {
            lane: lane_of[ms[0]],
            label: cluster::auto_label(input, &all),
            clusters: ms,
        });
    }

    // 6. Risk.
    let (risk, review_first) = risk(input);
    for c in clusters.iter_mut() {
        let n = c.members.iter().filter(|&&w| review_first[w]).count();
        c.review = n as f64 / c.members.len().max(1) as f64;
    }

    let columns = (0..5)
        .map(|col| GridColumn {
            heat: 4 - col as u8,
            x: col_x[col],
            w: col_w[col],
            clusters: clusters.iter().filter(|c| col_of(c.heat) == col).count() as u32,
        })
        .collect();
    Output {
        rects,
        cluster_of,
        clusters,
        subsystems,
        lanes: lane_bands,
        columns,
        world,
        risk,
        review_first,
        placement,
        folders: Vec::new(),
    }
}

#[cfg(test)]
mod tests;
