//! Leiden community detection (Traag, Waltman & van Eck 2019) over a petgraph undirected graph,
//! optimizing modularity with a resolution parameter. Deterministic: nodes are visited in index
//! order, ties go to the lowest community id, and the refinement phase merges greedily (the best
//! gain) instead of sampling, so identical input always gives an identical partition.

use std::collections::VecDeque;

use petgraph::graph::{NodeIndex, UnGraph};
use petgraph::visit::EdgeRef;

/// Compact adjacency (no self loops) with self-loop weights kept apart.
struct Adj {
    nbrs: Vec<Vec<(usize, f64)>>,
    self_w: Vec<f64>,
    /// Weighted degree: Σ w(i, j≠i) + 2·w(i, i).
    k: Vec<f64>,
    two_m: f64,
}

impl Adj {
    fn from_graph(g: &UnGraph<(), f64>) -> Adj {
        let n = g.node_count();
        let mut nbrs: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
        let mut self_w = vec![0.0; n];
        for e in g.edge_references() {
            let (a, b, w) = (e.source().index(), e.target().index(), *e.weight());
            if w <= 0.0 {
                continue;
            }
            if a == b {
                self_w[a] += w;
            } else {
                nbrs[a].push((b, w));
                nbrs[b].push((a, w));
            }
        }
        // Merge parallel edges; sorted neighbour lists keep every pass deterministic.
        for l in nbrs.iter_mut() {
            l.sort_by_key(|x| x.0);
            let mut merged: Vec<(usize, f64)> = Vec::with_capacity(l.len());
            for &(j, w) in l.iter() {
                match merged.last_mut() {
                    Some(last) if last.0 == j => last.1 += w,
                    _ => merged.push((j, w)),
                }
            }
            *l = merged;
        }
        let k: Vec<f64> = (0..n)
            .map(|i| nbrs[i].iter().map(|x| x.1).sum::<f64>() + 2.0 * self_w[i])
            .collect();
        let two_m = k.iter().sum();
        Adj {
            nbrs,
            self_w,
            k,
            two_m,
        }
    }

    fn len(&self) -> usize {
        self.k.len()
    }
}

/// Builds a petgraph graph from weighted edges over `n` nodes (parallel edges add up).
pub fn graph(n: usize, edges: impl IntoIterator<Item = (usize, usize, f64)>) -> UnGraph<(), f64> {
    let mut g = UnGraph::with_capacity(n, 0);
    for _ in 0..n {
        g.add_node(());
    }
    for (a, b, w) in edges {
        if w > 0.0 {
            g.add_edge(NodeIndex::new(a), NodeIndex::new(b), w);
        }
    }
    g
}

/// Community of every node, numbered 0.. in order of first appearance.
pub fn leiden(g: &UnGraph<(), f64>, resolution: f64) -> Vec<usize> {
    leiden_from(g, resolution, None)
}

/// Leiden from an initial partition (`init[i]` = community of node i), e.g. a previous layout.
pub fn leiden_from(g: &UnGraph<(), f64>, gamma: f64, init: Option<&[usize]>) -> Vec<usize> {
    let base = Adj::from_graph(g);
    let n = base.len();
    if n == 0 {
        return Vec::new();
    }
    if base.two_m <= 0.0 {
        return (0..n).collect();
    }
    // `member[i]`: the aggregate node holding original node i.
    let mut member: Vec<usize> = (0..n).collect();
    let mut adj = base;
    let mut part: Vec<usize> = match init {
        Some(p) => renumber(p),
        None => (0..n).collect(),
    };
    for _ in 0..32 {
        move_nodes(&adj, &mut part, gamma);
        let n_comms = renumber_in_place(&mut part);
        if n_comms == adj.len() {
            break;
        }
        let refined = refine(&adj, &part, gamma);
        let n_ref = refined.iter().max().map(|m| m + 1).unwrap_or(0);
        if n_ref == adj.len() {
            // Refinement kept every node alone: aggregating would change nothing.
            break;
        }
        // Aggregate on the refined partition; the aggregate starts from the unrefined one.
        let mut agg_init = vec![0usize; n_ref];
        for (v, &r) in refined.iter().enumerate() {
            agg_init[r] = part[v];
        }
        adj = aggregate(&adj, &refined, n_ref);
        for m in member.iter_mut() {
            *m = refined[*m];
        }
        part = agg_init;
    }
    let flat: Vec<usize> = member.iter().map(|&m| part[m]).collect();
    renumber(&flat)
}

fn renumber(p: &[usize]) -> Vec<usize> {
    let mut v = p.to_vec();
    renumber_in_place(&mut v);
    v
}

/// Renumbers communities 0.. by first appearance; returns their count.
fn renumber_in_place(p: &mut [usize]) -> usize {
    let mut map: Vec<usize> = vec![usize::MAX; p.iter().max().map(|m| m + 1).unwrap_or(0)];
    let mut next = 0;
    for c in p.iter_mut() {
        if map[*c] == usize::MAX {
            map[*c] = next;
            next += 1;
        }
        *c = map[*c];
    }
    next
}

/// Fast local moving: a queue of nodes; each moves to the neighbouring community with the best
/// modularity gain; the neighbours it leaves behind are queued again.
fn move_nodes(adj: &Adj, part: &mut [usize], gamma: f64) {
    let n = adj.len();
    let n_comm = part.iter().max().map(|m| m + 1).unwrap_or(0).max(n);
    let mut tot = vec![0.0; n_comm];
    for v in 0..n {
        tot[part[v]] += adj.k[v];
    }
    let mut queue: VecDeque<usize> = (0..n).collect();
    let mut queued = vec![true; n];
    let mut w_to = vec![0.0; n_comm];
    let mut touched: Vec<usize> = Vec::new();
    // Empty communities a node may move into (keeps "alone" an option after it left its own).
    let mut empty: Vec<usize> = (0..n_comm).filter(|&c| tot[c] == 0.0).rev().collect();
    while let Some(v) = queue.pop_front() {
        queued[v] = false;
        let own = part[v];
        let kv = adj.k[v];
        for &(u, w) in &adj.nbrs[v] {
            let c = part[u];
            if w_to[c] == 0.0 {
                touched.push(c);
            }
            w_to[c] += w;
        }
        tot[own] -= kv;
        let gain = |c: usize, w: f64| w - gamma * kv * tot[c] / adj.two_m;
        let mut best = own;
        let mut best_gain = gain(own, w_to[own]);
        touched.sort_unstable();
        for &c in &touched {
            if c == own {
                continue;
            }
            let g = gain(c, w_to[c]);
            if g > best_gain + 1e-12 {
                best = c;
                best_gain = g;
            }
        }
        // Moving out alone (gain 0) when staying is worse.
        if best == own && best_gain < -1e-12 && tot[own] > 0.0 {
            while let Some(e) = empty.pop() {
                if tot[e] == 0.0 && e != own {
                    best = e;
                    break;
                }
            }
        }
        tot[best] += kv;
        if best != own {
            part[v] = best;
            if tot[own] == 0.0 {
                empty.push(own);
            }
            for &(u, _) in &adj.nbrs[v] {
                if !queued[u] && part[u] != best {
                    queued[u] = true;
                    queue.push_back(u);
                }
            }
        }
        for &c in &touched {
            w_to[c] = 0.0;
        }
        touched.clear();
    }
}

/// Refinement: inside each community, nodes start alone and greedily merge into well-connected
/// sub-communities, so every final community is internally connected.
fn refine(adj: &Adj, part: &[usize], gamma: f64) -> Vec<usize> {
    let n = adj.len();
    let mut refined: Vec<usize> = (0..n).collect();
    let n_comm = part.iter().max().map(|m| m + 1).unwrap_or(0);
    let mut comm_tot = vec![0.0; n_comm];
    for v in 0..n {
        comm_tot[part[v]] += adj.k[v];
    }
    // Weight from each node to the rest of its community.
    let w_in: Vec<f64> = (0..n)
        .map(|v| {
            adj.nbrs[v]
                .iter()
                .filter(|(u, _)| part[*u] == part[v])
                .map(|x| x.1)
                .sum()
        })
        .collect();
    let mut r_tot: Vec<f64> = adj.k.clone();
    // Weight from each refined community to the rest of its community.
    let mut r_ext: Vec<f64> = w_in.clone();
    let mut singleton = vec![true; n];
    let mut w_to = vec![0.0; n];
    let mut touched: Vec<usize> = Vec::new();
    for v in 0..n {
        if !singleton[v] {
            continue;
        }
        let s = part[v];
        let kv = adj.k[v];
        // v must be well connected to its community.
        if w_in[v] < gamma * kv * (comm_tot[s] - kv) / adj.two_m {
            continue;
        }
        for &(u, w) in &adj.nbrs[v] {
            if part[u] != s {
                continue;
            }
            let c = refined[u];
            if w_to[c] == 0.0 {
                touched.push(c);
            }
            w_to[c] += w;
        }
        touched.sort_unstable();
        let mut best = v;
        let mut best_gain = 0.0;
        for &c in &touched {
            if c == v {
                continue;
            }
            // Only well-connected refined communities are merge targets.
            if r_ext[c] < gamma * r_tot[c] * (comm_tot[s] - r_tot[c]) / adj.two_m {
                continue;
            }
            let g = w_to[c] - gamma * kv * r_tot[c] / adj.two_m;
            if g > best_gain + 1e-12 {
                best = c;
                best_gain = g;
            }
        }
        if best != v {
            refined[v] = best;
            r_tot[best] += kv;
            // External weight of the merged community: both sides' minus the links between them.
            r_ext[best] += r_ext[v] - 2.0 * w_to[best];
            r_tot[v] = 0.0;
            singleton[v] = false;
            singleton[best] = false;
        }
        for &c in &touched {
            w_to[c] = 0.0;
        }
        touched.clear();
    }
    renumber(&refined)
}

fn aggregate(adj: &Adj, part: &[usize], n_comm: usize) -> Adj {
    let mut nbrs: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n_comm];
    let mut self_w = vec![0.0; n_comm];
    for v in 0..adj.len() {
        let cv = part[v];
        self_w[cv] += adj.self_w[v];
        for &(u, w) in &adj.nbrs[v] {
            let cu = part[u];
            if cu == cv {
                // Each internal edge is seen from both ends.
                self_w[cv] += w / 2.0;
            } else {
                nbrs[cv].push((cu, w));
            }
        }
    }
    for l in nbrs.iter_mut() {
        l.sort_by_key(|x| x.0);
        let mut merged: Vec<(usize, f64)> = Vec::with_capacity(l.len());
        for &(j, w) in l.iter() {
            match merged.last_mut() {
                Some(last) if last.0 == j => last.1 += w,
                _ => merged.push((j, w)),
            }
        }
        *l = merged;
    }
    let k: Vec<f64> = (0..n_comm)
        .map(|i| nbrs[i].iter().map(|x| x.1).sum::<f64>() + 2.0 * self_w[i])
        .collect();
    let two_m = k.iter().sum();
    Adj {
        nbrs,
        self_w,
        k,
        two_m,
    }
}

/// Modularity of a partition (for tests and diagnostics).
pub fn modularity(g: &UnGraph<(), f64>, part: &[usize], gamma: f64) -> f64 {
    let adj = Adj::from_graph(g);
    if adj.two_m <= 0.0 {
        return 0.0;
    }
    let n_comm = part.iter().max().map(|m| m + 1).unwrap_or(0);
    let (mut inside, mut tot) = (vec![0.0; n_comm], vec![0.0; n_comm]);
    for v in 0..adj.len() {
        tot[part[v]] += adj.k[v];
        inside[part[v]] += 2.0 * adj.self_w[v];
        for &(u, w) in &adj.nbrs[v] {
            if part[u] == part[v] {
                inside[part[v]] += w;
            }
        }
    }
    (0..n_comm)
        .map(|c| inside[c] / adj.two_m - gamma * (tot[c] / adj.two_m).powi(2))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `k` cliques of size `s` joined in a ring by single edges.
    fn ring_of_cliques(k: usize, s: usize) -> UnGraph<(), f64> {
        let mut e = Vec::new();
        for c in 0..k {
            for i in 0..s {
                for j in i + 1..s {
                    e.push((c * s + i, c * s + j, 1.0));
                }
            }
            e.push((c * s, ((c + 1) % k) * s + 1, 1.0));
        }
        graph(k * s, e)
    }

    #[test]
    fn finds_the_cliques_of_a_ring() {
        let g = ring_of_cliques(8, 5);
        let p = leiden(&g, 1.0);
        for c in 0..8 {
            let first = p[c * 5];
            assert!((0..5).all(|i| p[c * 5 + i] == first), "{p:?}");
        }
        let distinct: std::collections::HashSet<usize> = p.iter().copied().collect();
        assert_eq!(distinct.len(), 8);
        assert!(modularity(&g, &p, 1.0) > 0.7);
    }

    #[test]
    fn deterministic_and_warm_start_keeps_a_good_partition() {
        let g = ring_of_cliques(20, 6);
        let a = leiden(&g, 1.0);
        assert_eq!(a, leiden(&g, 1.0));
        let b = leiden_from(&g, 1.0, Some(&a));
        assert_eq!(a, b);
    }

    #[test]
    fn isolated_nodes_stay_alone_and_empty_graphs_work() {
        let g = graph(4, [(0, 1, 1.0)]);
        let p = leiden(&g, 1.0);
        assert_eq!(p[0], p[1]);
        assert!(p[2] != p[3] && p[2] != p[0]);
        assert!(leiden(&graph(0, []), 1.0).is_empty());
        assert_eq!(leiden(&graph(3, []), 1.0), vec![0, 1, 2]);
    }

    #[test]
    fn higher_resolution_gives_more_communities() {
        let g = ring_of_cliques(6, 6);
        let coarse = leiden(&g, 0.05);
        let fine = leiden(&g, 1.0);
        let count = |p: &[usize]| p.iter().max().map(|m| m + 1).unwrap_or(0);
        assert!(count(&coarse) < count(&fine), "{coarse:?} {fine:?}");
    }
}
