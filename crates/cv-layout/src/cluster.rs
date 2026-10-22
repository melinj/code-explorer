//! Window clustering (DESIGN §4.10.1–2): the weighted window graph of a lane, Leiden modules,
//! size bounds, subsystems (Leiden over the module graph), and labels.

use std::collections::{BTreeMap, HashMap};

use crate::leiden;
use crate::{EdgeKind, Input, MAX_CLUSTER, MIN_CLUSTER, dirname};

/// Weight of a link between consecutive fragments of one file.
pub const COHESION: f64 = 2.0;
/// Leiden resolutions: modules, then subsystems over the module graph (modules are split to at
/// most MAX_CLUSTER windows, so Leiden on their graph at the same resolution regroups them).
pub const MODULE_RESOLUTION: f64 = 1.0;
pub const SUBSYSTEM_RESOLUTION: f64 = 1.0;

/// Weighted, undirected window graph: (a, b) with a < b → weight.
pub type Weights = BTreeMap<(usize, usize), f64>;

/// Edge weights per DESIGN §4.10: 1.0·log₂(1+calls) + 2.0·inherits + 1.5·channel links +
/// 0.5·imports + 1.0·co-change Jaccard + cohesion. Only pairs inside one lane count.
pub fn weights(input: &Input, file_windows: &[Vec<usize>]) -> Weights {
    let lane_of = |w: usize| input.files[input.windows[w].file].lane;
    let key = |a: usize, b: usize| (a.min(b), a.max(b));
    let mut calls: BTreeMap<(usize, usize), f64> = BTreeMap::new();
    let mut other: Weights = BTreeMap::new();
    for l in input.links {
        if l.from == l.to || lane_of(l.from) != lane_of(l.to) {
            continue;
        }
        let k = key(l.from, l.to);
        match l.kind {
            EdgeKind::Call => *calls.entry(k).or_default() += 1.0,
            EdgeKind::Inherit | EdgeKind::Implement => *other.entry(k).or_default() += 2.0,
            EdgeKind::Channel => *other.entry(k).or_default() += 1.5,
            EdgeKind::Import => *other.entry(k).or_default() += 0.5,
        }
    }
    for (k, n) in calls {
        *other.entry(k).or_default() += (1.0 + n).log2();
    }
    // Co-change links the files' first windows (their cohesion chains carry the rest).
    for c in input.co_change {
        let (Some(&a), Some(&b)) = (
            file_windows.get(c.a).and_then(|v| v.first()),
            file_windows.get(c.b).and_then(|v| v.first()),
        ) else {
            continue;
        };
        if a != b && lane_of(a) == lane_of(b) && c.jaccard > 0.0 {
            *other.entry(key(a, b)).or_default() += c.jaccard;
        }
    }
    for fw in file_windows {
        for p in fw.windows(2) {
            *other.entry(key(p[0], p[1])).or_default() += COHESION;
        }
    }
    other
}

/// Lane-local adjacency: for each window, (neighbour, weight).
pub fn adjacency(n: usize, w: &Weights) -> Vec<Vec<(usize, f64)>> {
    let mut adj = vec![Vec::new(); n];
    for (&(a, b), &x) in w {
        adj[a].push((b, x));
        adj[b].push((a, x));
    }
    adj
}

/// Fresh modules of one lane: Leiden, then oversized communities split and small ones merged into
/// their strongest neighbour or pooled by directory. Returns groups of window indices.
pub fn fresh_modules(
    input: &Input,
    lane_wins: &[usize],
    adj: &[Vec<(usize, f64)>],
) -> Vec<Vec<usize>> {
    let local: HashMap<usize, usize> = lane_wins.iter().enumerate().map(|(i, &w)| (w, i)).collect();
    let mut edges = Vec::new();
    for (i, &w) in lane_wins.iter().enumerate() {
        for &(u, x) in &adj[w] {
            if let Some(&j) = local.get(&u)
                && i < j
            {
                edges.push((i, j, x));
            }
        }
    }
    let g = leiden::graph(lane_wins.len(), edges);
    let part = leiden::leiden(&g, MODULE_RESOLUTION);
    let mut comms: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (i, &c) in part.iter().enumerate() {
        comms.entry(c).or_default().push(lane_wins[i]);
    }
    let mut big: Vec<Vec<usize>> = Vec::new();
    for (_, members) in comms {
        split(input, members, adj, MODULE_RESOLUTION * 2.0, &mut big);
    }
    merge_small(input, big, adj)
}

/// Splits a group over [`MAX_CLUSTER`] windows with Leiden at a higher resolution, or into runs.
fn split(
    input: &Input,
    members: Vec<usize>,
    adj: &[Vec<(usize, f64)>],
    gamma: f64,
    out: &mut Vec<Vec<usize>>,
) {
    if members.len() <= MAX_CLUSTER {
        out.push(members);
        return;
    }
    if gamma <= 64.0 {
        let local: HashMap<usize, usize> =
            members.iter().enumerate().map(|(i, &w)| (w, i)).collect();
        let mut edges = Vec::new();
        for (i, &w) in members.iter().enumerate() {
            for &(u, x) in &adj[w] {
                if let Some(&j) = local.get(&u)
                    && i < j
                {
                    edges.push((i, j, x));
                }
            }
        }
        let part = leiden::leiden(&leiden::graph(members.len(), edges), gamma);
        let n = part.iter().max().map(|m| m + 1).unwrap_or(1);
        if n > 1 {
            let mut parts: Vec<Vec<usize>> = vec![Vec::new(); n];
            for (i, &c) in part.iter().enumerate() {
                parts[c].push(members[i]);
            }
            for p in parts {
                split(input, p, adj, gamma * 2.0, out);
            }
            return;
        }
    }
    // Inseparable: path-ordered runs, keeping files whole where they fit.
    let mut sorted = members;
    sorted.sort_by(|&a, &b| win_order(input, a, b));
    out.extend(runs(input, &sorted));
}

pub(crate) fn win_order(input: &Input, a: usize, b: usize) -> std::cmp::Ordering {
    let (wa, wb) = (&input.windows[a], &input.windows[b]);
    input.files[wa.file]
        .path
        .cmp(&input.files[wb.file].path)
        .then(wa.start_line.cmp(&wb.start_line))
        .then(a.cmp(&b))
}

/// Path-ordered windows → runs of ≤ MAX_CLUSTER, breaking between files when possible.
fn runs(input: &Input, sorted: &[usize]) -> Vec<Vec<usize>> {
    let mut out: Vec<Vec<usize>> = vec![Vec::new()];
    let mut i = 0;
    while i < sorted.len() {
        let f = input.windows[sorted[i]].file;
        let mut j = i;
        while j < sorted.len() && input.windows[sorted[j]].file == f {
            j += 1;
        }
        let file_ws = &sorted[i..j];
        let cur = out.last().map(|v| v.len()).unwrap_or(0);
        if cur > 0 && cur + file_ws.len() > MAX_CLUSTER {
            out.push(Vec::new());
        }
        for chunk in file_ws.chunks(MAX_CLUSTER) {
            if out
                .last()
                .is_some_and(|v| v.len() + chunk.len() > MAX_CLUSTER)
            {
                out.push(Vec::new());
            }
            if let Some(last) = out.last_mut() {
                last.extend_from_slice(chunk);
            }
        }
        i = j;
    }
    out.retain(|v| !v.is_empty());
    out
}

/// Groups under [`MIN_CLUSTER`] windows, and groups of a single file (held together only by
/// cohesion), join the neighbouring group they are most linked to (if it stays within
/// [`MAX_CLUSTER`]); the rest are pooled by directory like the stand-in clusters were.
fn merge_small(
    input: &Input,
    groups: Vec<Vec<usize>>,
    adj: &[Vec<(usize, f64)>],
) -> Vec<Vec<usize>> {
    let mut group_of: HashMap<usize, usize> = HashMap::new();
    for (g, ms) in groups.iter().enumerate() {
        for &w in ms {
            group_of.insert(w, g);
        }
    }
    let mut groups: Vec<Option<Vec<usize>>> = groups.into_iter().map(Some).collect();
    let mut leftovers: Vec<usize> = Vec::new();
    // Smallest first, so singletons find homes before the groups they join fill up.
    let mut order: Vec<usize> = (0..groups.len()).collect();
    order.sort_by_key(|&g| (groups[g].as_ref().map(|v| v.len()).unwrap_or(0), g));
    for g in order {
        let Some(ms) = groups[g].clone() else {
            continue;
        };
        let one_file = ms
            .iter()
            .all(|&w| input.windows[w].file == input.windows[ms[0]].file);
        if ms.len() >= MIN_CLUSTER && !one_file {
            continue;
        }
        let mut to: BTreeMap<usize, f64> = BTreeMap::new();
        for &w in &ms {
            for &(u, x) in &adj[w] {
                if let Some(&h) = group_of.get(&u)
                    && h != g
                {
                    *to.entry(h).or_default() += x;
                }
            }
        }
        let best = to
            .into_iter()
            .filter(|(h, _)| {
                groups[*h]
                    .as_ref()
                    .is_some_and(|v| v.len() + ms.len() <= MAX_CLUSTER)
            })
            .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)));
        groups[g] = None;
        match best {
            Some((h, _)) => {
                for &w in &ms {
                    group_of.insert(w, h);
                }
                if let Some(v) = groups[h].as_mut() {
                    v.extend(ms);
                }
            }
            None => {
                for &w in &ms {
                    group_of.remove(&w);
                }
                leftovers.extend(ms);
            }
        }
    }
    let mut out: Vec<Vec<usize>> = groups.into_iter().flatten().collect();
    out.extend(directory_pools(input, &leftovers));
    for g in out.iter_mut() {
        g.sort_by(|&a, &b| win_order(input, a, b));
    }
    out.sort_by(|a, b| win_order(input, a[0], b[0]));
    out
}

/// Windows without dependency ties, grouped by directory: a directory with fewer than
/// MIN_CLUSTER windows folds into its parent; a big one splits into path-ordered runs.
pub fn directory_pools(input: &Input, wins: &[usize]) -> Vec<Vec<usize>> {
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for &w in wins {
        let f = &input.files[input.windows[w].file];
        groups
            .entry(dirname(&f.path).to_string())
            .or_default()
            .push(w);
    }
    let depth = |d: &str| {
        if d.is_empty() {
            0
        } else {
            d.matches('/').count() + 1
        }
    };
    let max_depth = groups.keys().map(|d| depth(d)).max().unwrap_or(0);
    for level in (1..=max_depth).rev() {
        let small: Vec<String> = groups
            .iter()
            .filter(|(d, ws)| depth(d) == level && ws.len() < MIN_CLUSTER)
            .map(|(d, _)| d.clone())
            .collect();
        for d in small {
            let ws = groups.remove(&d).unwrap_or_default();
            groups
                .entry(dirname(&d).to_string())
                .or_default()
                .extend(ws);
        }
    }
    let mut out = Vec::new();
    for (_, mut ws) in groups {
        ws.sort_by(|&a, &b| win_order(input, a, b));
        out.extend(runs(input, &ws));
    }
    out
}

/// Subsystems: Leiden over the module graph at a lower resolution. Returns a subsystem per module.
pub fn subsystems(
    modules: &[Vec<usize>],
    adj: &[Vec<(usize, f64)>],
    n_windows: usize,
) -> Vec<usize> {
    let mut module_of = vec![usize::MAX; n_windows];
    for (m, ws) in modules.iter().enumerate() {
        for &w in ws {
            module_of[w] = m;
        }
    }
    let mut w: BTreeMap<(usize, usize), f64> = BTreeMap::new();
    for (m, ws) in modules.iter().enumerate() {
        for &a in ws {
            for &(b, x) in &adj[a] {
                let n = module_of[b];
                if n != usize::MAX && n > m {
                    *w.entry((m, n)).or_default() += x;
                }
            }
        }
    }
    let g = leiden::graph(modules.len(), w.into_iter().map(|((a, b), x)| (a, b, x)));
    leiden::leiden(&g, SUBSYSTEM_RESOLUTION)
}

/// Auto label: "dir · stem" when one file holds ≥ 60% of the windows, else the last two
/// components of the deepest directory holding ≥ 60% of them.
pub fn auto_label(input: &Input, members: &[usize]) -> String {
    let n = members.len().max(1) as f64;
    let mut by_file: BTreeMap<usize, usize> = BTreeMap::new();
    for &w in members {
        *by_file.entry(input.windows[w].file).or_default() += 1;
    }
    let two = |d: &str| {
        let parts: Vec<&str> = d.split('/').filter(|s| !s.is_empty()).collect();
        match parts.len() {
            0 => "(root)".to_string(),
            1 => parts[0].to_string(),
            k => format!("{} · {}", parts[k - 2], parts[k - 1]),
        }
    };
    if let Some((&f, &c)) = by_file
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
        && c as f64 >= 0.6 * n
    {
        let (dir, stem) = file_label(&input.files[f]);
        let d = dir.rsplit('/').next().unwrap_or(dir);
        return if d.is_empty() || d == stem {
            stem.to_string()
        } else {
            format!("{d} · {stem}")
        };
    }
    // Deepest directory prefix holding ≥ 60% of the windows.
    let mut count: BTreeMap<String, usize> = BTreeMap::new();
    for &w in members {
        let d = dirname(&input.files[input.windows[w].file].path);
        let mut acc = String::new();
        *count.entry(String::new()).or_default() += 1;
        for part in d.split('/').filter(|s| !s.is_empty()) {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(part);
            *count.entry(acc.clone()).or_default() += 1;
        }
    }
    let depth = |d: &str| {
        if d.is_empty() {
            0
        } else {
            d.matches('/').count() + 1
        }
    };
    let best = count
        .iter()
        .filter(|(_, c)| **c as f64 >= 0.6 * n)
        .max_by(|a, b| depth(a.0).cmp(&depth(b.0)).then(b.0.cmp(a.0)))
        .map(|(d, _)| d.clone())
        .unwrap_or_default();
    two(&best)
}

/// The file holding most of a cluster's windows, as "stem" (ties by path).
pub fn top_file_stem(input: &Input, members: &[usize]) -> String {
    let mut by_file: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for &w in members {
        let f = input.windows[w].file;
        by_file
            .entry(input.files[f].path.as_str())
            .or_insert((0, f))
            .0 += 1;
    }
    by_file
        .iter()
        .max_by(|a, b| a.1.0.cmp(&b.1.0).then(b.0.cmp(a.0)))
        .map(|(_, &(_, f))| file_label(&input.files[f]).1.to_string())
        .unwrap_or_default()
}

/// (directory, name) a file is known by: its stem, or for a directory's entry file (the language
/// pack's `Conventions::entry_stems`, e.g. a package's module or crate root) the directory it
/// stands for, skipping `src/`.
fn file_label(file: &crate::FileIn) -> (&str, &str) {
    let path = file.path.as_str();
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.split('.').next().unwrap_or(name);
    let mut dir = dirname(path);
    if !file.entry {
        return (dir, stem);
    }
    let mut d = dir.rsplit('/').next().unwrap_or(dir);
    if d == "src" {
        dir = dirname(dir);
        d = dir.rsplit('/').next().unwrap_or(dir);
    }
    if d.is_empty() {
        return (dirname(path), stem);
    }
    (dirname(dir), d)
}
