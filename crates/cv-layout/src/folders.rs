//! Folders arrangement (`Arrangement::Folders`, the "None" lens): the map as the folder tree.
//!
//! - **Clusters:** per lane, one per directory holding the directory's own files, split into
//!   path-ordered runs of whole files of at most [`FOLDER_MAX`] windows. Inside a cluster windows
//!   are layered as on the district grid ([`crate::place_cluster`]).
//! - **Tree:** each lane holds its directories nested by path; a directory with no files of its own
//!   and a single subdirectory folds into it ("a/b"). A directory's frame holds its own clusters
//!   first, then its subdirectories, in path order, shelf-packed to a roughly 3:2 block.
//! - **Grades:** clusters keep heat and connectedness grades (quantiles over clusters per lane, as
//!   on the grid), so the colour lenses still read; position says nothing about them.
//! - **Flow** (`Arrangement::Flow`): the same tree, but a directory's items (clusters and
//!   subdirectories) are ordered by their links instead of by path: a chain that always continues
//!   with the item most linked to the last few placed, so linked code sits side by side and links
//!   stay short ([`flow_order`]). Unlinked items follow in path order.
//! - Deterministic from the input alone: no previous placement, pins or labels.

use std::collections::{BTreeMap, HashMap};

use cv_contracts::{FolderFrame, LaneBand, Rect};

use crate::{
    Arrangement, CELL_GAP, CLUSTER_PAD, ClusterOut, HEADER_BAND, Input, LANE_GAP, LANE_PAD, Output,
    PlacedCluster, Placement, cluster, cluster_conn, cluster_heat, dirname, place_cluster, risk,
};

/// A directory's own files are split into clusters of at most this many windows (files stay whole
/// unless one alone is bigger).
pub const FOLDER_MAX: usize = 48;
/// Room inside a folder frame around its content, and its title band.
pub const FOLDER_PAD: f64 = 2.0 * CLUSTER_PAD;
pub const FOLDER_TITLE: f64 = 360.0;
/// Width : height the shelf packing aims a directory's block at.
const ASPECT: f64 = 1.5;

enum Item {
    Cluster(usize),
    Node(usize),
}

struct Node {
    path: String,
    label: String,
    lane: usize,
    clusters: Vec<usize>,
    children: Vec<usize>,
    /// Item offsets inside the content box, and the content size.
    items: Vec<(Item, f64, f64)>,
    content: (f64, f64),
}

impl Node {
    /// The size the node takes in its parent: framed, except a lane's root.
    fn outer(&self, root: bool) -> (f64, f64) {
        if root {
            self.content
        } else {
            (
                self.content.0 + 2.0 * FOLDER_PAD,
                self.content.1 + 2.0 * FOLDER_PAD + FOLDER_TITLE,
            )
        }
    }
}

/// The directory runs of one lane: path-ordered windows → per directory, runs of whole files.
fn directory_runs(input: &Input, wins: &[usize]) -> Vec<(String, Vec<usize>)> {
    let mut by_dir: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for &w in wins {
        by_dir
            .entry(dirname(&input.files[input.windows[w].file].path))
            .or_default()
            .push(w);
    }
    let mut out = Vec::new();
    for (dir, mut ws) in by_dir {
        ws.sort_by(|&a, &b| cluster::win_order(input, a, b));
        let mut run: Vec<usize> = Vec::new();
        let mut i = 0;
        while i < ws.len() {
            let f = input.windows[ws[i]].file;
            let j = ws[i..]
                .iter()
                .position(|&w| input.windows[w].file != f)
                .map_or(ws.len(), |k| i + k);
            if !run.is_empty() && run.len() + (j - i) > FOLDER_MAX {
                out.push((dir.to_string(), std::mem::take(&mut run)));
            }
            run.extend_from_slice(&ws[i..j]);
            i = j;
        }
        if !run.is_empty() {
            out.push((dir.to_string(), run));
        }
    }
    out
}

/// Shelf-packs `sizes` in order into rows about ASPECT times wider than tall; returns the offsets
/// and the packed size.
fn shelf(sizes: &[(f64, f64)]) -> (Vec<(f64, f64)>, (f64, f64)) {
    let area: f64 = sizes
        .iter()
        .map(|&(w, h)| (w + CELL_GAP) * (h + CELL_GAP))
        .sum();
    let widest = sizes.iter().map(|s| s.0).fold(0.0, f64::max);
    let target = widest.max((area * ASPECT).sqrt());
    let (mut x, mut y, mut row_h, mut w_max) = (0.0, 0.0, 0f64, 0f64);
    let mut out = Vec::with_capacity(sizes.len());
    for &(w, h) in sizes {
        if x > 0.0 && x + w > target {
            x = 0.0;
            y += row_h + CELL_GAP;
            row_h = 0.0;
        }
        out.push((x, y));
        w_max = w_max.max(x + w);
        row_h = row_h.max(h);
        x += w + CELL_GAP;
    }
    (out, (w_max, y + row_h))
}

/// The folder tree of one lane over its clusters: returns the root node's index. Directories
/// without files of their own and with one subdirectory fold into it.
fn tree(nodes: &mut Vec<Node>, lane: usize, dirs: &BTreeMap<String, Vec<usize>>) -> usize {
    // Every directory on the way to a cluster's directory, with its direct subdirectories.
    let mut kids: BTreeMap<String, Vec<String>> = BTreeMap::new();
    kids.entry(String::new()).or_default();
    for d in dirs.keys() {
        let mut cur = d.clone();
        while !cur.is_empty() {
            let parent = dirname(&cur).to_string();
            kids.entry(cur.clone()).or_default();
            let siblings = kids.entry(parent.clone()).or_default();
            if siblings.contains(&cur) {
                break;
            }
            siblings.push(cur.clone());
            cur = parent;
        }
    }
    for v in kids.values_mut() {
        v.sort();
    }
    fn build(
        nodes: &mut Vec<Node>,
        lane: usize,
        path: &str,
        label: String,
        kids: &BTreeMap<String, Vec<String>>,
        dirs: &BTreeMap<String, Vec<usize>>,
    ) -> usize {
        let own = dirs.get(path).cloned().unwrap_or_default();
        let sub = kids.get(path).cloned().unwrap_or_default();
        // Fold a pass-through directory into its only subdirectory (never the lane's root).
        if !path.is_empty() && own.is_empty() && sub.len() == 1 {
            let child = &sub[0];
            let rest = &child[path.len() + 1..];
            return build(nodes, lane, child, format!("{label}/{rest}"), kids, dirs);
        }
        let children: Vec<usize> = sub
            .iter()
            .map(|c| {
                let name = c[if path.is_empty() { 0 } else { path.len() + 1 }..].to_string();
                build(nodes, lane, c, name, kids, dirs)
            })
            .collect();
        nodes.push(Node {
            path: path.to_string(),
            label,
            lane,
            clusters: own,
            children,
            items: Vec::new(),
            content: (0.0, 0.0),
        });
        nodes.len() - 1
    }
    build(nodes, lane, "", String::new(), &kids, dirs)
}

/// How many of the last placed items a candidate is pulled toward (nearer ones pull harder).
const PULL_LAST: usize = 3;

/// Flow order of `n` items with link weights `w` (keys (a, b), a < b): a chain that continues with
/// the item most linked to the last PULL_LAST placed; when nothing pulls, a new chain starts at the
/// most linked item left, and items with no links keep their order at the end.
pub(crate) fn flow_order(n: usize, w: &HashMap<(usize, usize), f64>) -> Vec<usize> {
    let weight = |a: usize, b: usize| w.get(&(a.min(b), a.max(b))).copied().unwrap_or(0.0);
    let mut total = vec![0.0; n];
    for (&(a, b), &x) in w {
        total[a] += x;
        total[b] += x;
    }
    let mut placed = vec![false; n];
    let mut out: Vec<usize> = Vec::with_capacity(n);
    while out.len() < n {
        let pull = |i: usize| -> f64 {
            out.iter()
                .rev()
                .take(PULL_LAST)
                .enumerate()
                .map(|(k, &j)| weight(i, j) / (k + 1) as f64)
                .sum()
        };
        let best = |score: &dyn Fn(usize) -> f64| {
            (0..n)
                .filter(|&i| !placed[i])
                .map(|i| (score(i), i))
                .filter(|&(k, _)| k > 0.0)
                .max_by(|a, b| a.0.total_cmp(&b.0).then(b.1.cmp(&a.1)))
                .map(|(_, i)| i)
        };
        let next = best(&pull)
            .or_else(|| best(&|i| total[i]))
            .unwrap_or_else(|| (0..n).find(|&i| !placed[i]).expect("an item is left"));
        placed[next] = true;
        out.push(next);
    }
    out
}

/// Every cluster in a node's subtree.
fn subtree_clusters(nodes: &[Node], n: usize, out: &mut Vec<usize>) {
    out.extend_from_slice(&nodes[n].clusters);
    for &c in &nodes[n].children {
        subtree_clusters(nodes, c, out);
    }
}

/// Packs a node's own clusters, then its subdirectories (children are packed first); with
/// `links` (cluster pair → weight, the flow arrangement), in [`flow_order`] instead.
fn pack(
    nodes: &mut [Node],
    n: usize,
    sizes: &[(f64, f64)],
    links: Option<&HashMap<(usize, usize), f64>>,
) {
    for c in nodes[n].children.clone() {
        pack(nodes, c, sizes, links);
    }
    let mut items: Vec<Item> = Vec::new();
    let mut dims: Vec<(f64, f64)> = Vec::new();
    for &m in &nodes[n].clusters {
        items.push(Item::Cluster(m));
        dims.push(sizes[m]);
    }
    for &c in &nodes[n].children {
        items.push(Item::Node(c));
        dims.push(nodes[c].outer(false));
    }
    if let Some(links) = links {
        let mut item_of: HashMap<usize, usize> = HashMap::new();
        for (i, it) in items.iter().enumerate() {
            match *it {
                Item::Cluster(m) => {
                    item_of.insert(m, i);
                }
                Item::Node(c) => {
                    let mut cs = Vec::new();
                    subtree_clusters(nodes, c, &mut cs);
                    item_of.extend(cs.into_iter().map(|m| (m, i)));
                }
            }
        }
        let mut w: HashMap<(usize, usize), f64> = HashMap::new();
        for (&(a, b), &x) in links {
            if let (Some(&ia), Some(&ib)) = (item_of.get(&a), item_of.get(&b))
                && ia != ib
            {
                *w.entry((ia.min(ib), ia.max(ib))).or_default() += x;
            }
        }
        let order = flow_order(items.len(), &w);
        let mut slots: Vec<Option<(Item, (f64, f64))>> =
            items.into_iter().zip(dims).map(Some).collect();
        (items, dims) = order
            .into_iter()
            .map(|i| slots[i].take().expect("each item once"))
            .unzip();
    }
    let (offsets, size) = shelf(&dims);
    nodes[n].items = items
        .into_iter()
        .zip(offsets)
        .map(|(it, (x, y))| (it, x, y))
        .collect();
    nodes[n].content = size;
}

/// World positions: clusters into `origin`, frames into `frames` (outermost first).
fn place(
    nodes: &[Node],
    n: usize,
    at: (f64, f64),
    depth: Option<u32>,
    lanes: &[&str],
    origin: &mut [(f64, f64)],
    frames: &mut Vec<FolderFrame>,
) {
    let node = &nodes[n];
    let content = match depth {
        None => at,
        Some(d) => {
            let (w, h) = node.outer(false);
            frames.push(FolderFrame {
                path: node.path.clone(),
                label: node.label.clone(),
                lane: lanes[node.lane].to_string(),
                depth: d,
                rect: Rect {
                    x: at.0,
                    y: at.1,
                    w,
                    h,
                },
            });
            (at.0 + FOLDER_PAD, at.1 + FOLDER_TITLE + FOLDER_PAD)
        }
    };
    let next = Some(depth.map_or(0, |d| d + 1));
    for (it, x, y) in &node.items {
        let p = (content.0 + x, content.1 + y);
        match *it {
            Item::Cluster(m) => origin[m] = p,
            Item::Node(c) => place(nodes, c, p, next, lanes, origin, frames),
        }
    }
}

pub(crate) fn layout(input: &Input, file_h: &[f64], calls: &[Vec<usize>]) -> Output {
    let nw = input.windows.len();
    let mut by_lane: Vec<Vec<usize>> = vec![Vec::new(); input.lanes.len()];
    for w in 0..nw {
        by_lane[input.files[input.windows[w].file].lane].push(w);
    }

    // 1. Clusters: directory runs per lane.
    let mut modules: Vec<Vec<usize>> = Vec::new();
    let mut dir_of: Vec<String> = Vec::new();
    let mut lane_of: Vec<usize> = Vec::new();
    for (lane, wins) in by_lane.iter().enumerate() {
        for (dir, run) in directory_runs(input, wins) {
            modules.push(run);
            dir_of.push(dir);
            lane_of.push(lane);
        }
    }
    let nm = modules.len();
    let mut cluster_of = vec![usize::MAX; nw];
    for (m, ms) in modules.iter().enumerate() {
        for &w in ms {
            cluster_of[w] = m;
        }
    }

    // 2. Inside each cluster, as on the grid.
    let mut rel = vec![Rect::default(); nw];
    let sizes: Vec<(f64, f64)> = modules
        .iter()
        .map(|ms| place_cluster(ms, input, calls, &mut rel))
        .collect();

    // 3. Grades (colour only here).
    let heat = cluster_heat(input, &modules, file_h, &lane_of);
    let conn = cluster_conn(input, &modules, &lane_of);

    // 4. Labels: the directory, with an ordinal when it is split.
    let mut runs_of: BTreeMap<(usize, &str), usize> = BTreeMap::new();
    for m in 0..nm {
        *runs_of.entry((lane_of[m], dir_of[m].as_str())).or_default() += 1;
    }
    let mut seen: BTreeMap<(usize, &str), usize> = BTreeMap::new();
    let labels: Vec<String> = (0..nm)
        .map(|m| {
            let d = dir_of[m].as_str();
            let base = if d.is_empty() { "(root)" } else { d };
            let k = (lane_of[m], d);
            let n = seen.entry(k).or_default();
            *n += 1;
            if runs_of[&k] > 1 {
                format!("{base} · {n}")
            } else {
                base.to_string()
            }
        })
        .collect();

    // 5. The folder tree of each lane, stacked like the grid's lanes. Flow: links between clusters
    // (one per link, whatever its kind) order each directory's items.
    let links: Option<HashMap<(usize, usize), f64>> = (input.arrangement == Arrangement::Flow)
        .then(|| {
            let mut w: HashMap<(usize, usize), f64> = HashMap::new();
            for l in input.links {
                let (a, b) = (cluster_of[l.from], cluster_of[l.to]);
                if a != b {
                    *w.entry((a.min(b), a.max(b))).or_default() += 1.0;
                }
            }
            w
        });
    let mut nodes: Vec<Node> = Vec::new();
    let mut roots: Vec<(usize, usize)> = Vec::new();
    for lane in 0..input.lanes.len() {
        let mut dirs: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for m in (0..nm).filter(|&m| lane_of[m] == lane) {
            dirs.entry(dir_of[m].clone()).or_default().push(m);
        }
        if dirs.is_empty() {
            continue;
        }
        let root = tree(&mut nodes, lane, &dirs);
        pack(&mut nodes, root, &sizes, links.as_ref());
        roots.push((lane, root));
    }
    let world_w = roots
        .iter()
        .map(|&(_, r)| nodes[r].content.0)
        .fold(0.0, f64::max)
        + 2.0 * LANE_PAD;
    let mut origin = vec![(0.0, 0.0); nm];
    let mut frames: Vec<FolderFrame> = Vec::new();
    let mut lane_bands = Vec::new();
    let mut placement_lanes = Vec::new();
    let mut y = HEADER_BAND;
    for &(lane, root) in &roots {
        let top = y;
        place(
            &nodes,
            root,
            (LANE_PAD, y + LANE_PAD),
            None,
            input.lanes,
            &mut origin,
            &mut frames,
        );
        y += nodes[root].content.1 + 2.0 * LANE_PAD;
        lane_bands.push(LaneBand {
            lane: input.lanes[lane].to_string(),
            rect: Rect {
                x: 0.0,
                y: top,
                w: world_w,
                h: y - top,
            },
            files: (0..input.files.len())
                .filter(|&f| input.files[f].lane == lane)
                .filter(|&f| input.windows.iter().any(|w| w.file == f))
                .count() as u32,
            clusters: (0..nm).filter(|&m| lane_of[m] == lane).count() as u32,
            rows: Vec::new(),
        });
        placement_lanes.push((input.lanes[lane].to_string(), top, Vec::new()));
        y += LANE_GAP;
    }
    // Outermost first, then by path: parents are drawn beneath their children.
    frames.sort_by(|a, b| a.depth.cmp(&b.depth).then(a.path.cmp(&b.path)));
    let world = Rect {
        x: 0.0,
        y: 0.0,
        w: world_w,
        h: (y - LANE_GAP).max(HEADER_BAND),
    };

    // World positions and risk.
    let (risk, review_first) = risk(input);
    let mut rects = vec![Rect::default(); nw];
    let mut clusters: Vec<ClusterOut> = Vec::with_capacity(nm);
    for m in 0..nm {
        let rect = Rect {
            x: origin[m].0,
            y: origin[m].1,
            w: sizes[m].0,
            h: sizes[m].1,
        };
        for &w in &modules[m] {
            rects[w] = Rect {
                x: rect.x + rel[w].x,
                y: rect.y + rel[w].y,
                ..rel[w]
            };
        }
        let (h, settled, level) = heat[m];
        let n_review = modules[m].iter().filter(|&&w| review_first[w]).count();
        clusters.push(ClusterOut {
            lane: lane_of[m],
            label: labels[m].clone(),
            label_source: "auto".into(),
            heat: h,
            conn: conn[m],
            heat_now: h,
            heat_settled: settled,
            heat_level: level.clamp(h as f64 / 5.0, (h as f64 + 0.999) / 5.0),
            review: n_review as f64 / modules[m].len().max(1) as f64,
            members: modules[m].clone(),
            rect,
            parent: None,
            folder: Some(dir_of[m].clone()),
        });
    }
    let placement = Placement {
        windows: input
            .windows
            .iter()
            .enumerate()
            .map(|(w, win)| (win.key.clone(), (cluster_of[w], rects[w])))
            .collect(),
        clusters: clusters
            .iter()
            .map(|c| {
                let mut members: Vec<String> = c
                    .members
                    .iter()
                    .map(|&w| input.windows[w].key.clone())
                    .collect();
                members.sort();
                PlacedCluster {
                    lane: input.lanes[c.lane].to_string(),
                    label: c.label.clone(),
                    label_source: c.label_source.clone(),
                    heat: c.heat,
                    conn: c.conn,
                    rect: c.rect,
                    members,
                    subsystem: 0,
                }
            })
            .collect(),
        col_x: [0.0; 5],
        col_w: [0.0; 5],
        lanes: placement_lanes,
    };
    Output {
        rects,
        cluster_of,
        clusters,
        subsystems: Vec::new(),
        lanes: lane_bands,
        columns: Vec::new(),
        world,
        risk,
        review_first,
        placement,
        folders: frames,
    }
}
