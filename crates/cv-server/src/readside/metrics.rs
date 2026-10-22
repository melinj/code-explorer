//! Hotspots (DESIGN §4.9): percentile points per cluster or file, and co-change pairs mined from
//! history: files that change together without a static link, and the clusters they make up.

use std::collections::{HashMap, HashSet};

use cv_contracts::readside::{CoChangeClusterPair, CoChangePair, MetricPoint};
use cv_contracts::{Connectedness, Heat};
use cv_vcs::JjRepo;

use crate::index::View;

/// Mid-rank percentile of each value among all values, 0..1.
pub fn percentiles(values: &[f64]) -> Vec<f64> {
    let n = values.len();
    if n == 0 {
        return vec![];
    }
    let mut sorted: Vec<f64> = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    values
        .iter()
        .map(|v| {
            let lo = sorted.partition_point(|x| x < v);
            let hi = sorted.partition_point(|x| x <= v);
            (lo as f64 + (hi - lo) as f64 / 2.0) / n as f64
        })
        .collect()
}

struct Agg {
    key: String,
    label: String,
    lane: String,
    heat: Heat,
    conn: Connectedness,
    window: Option<String>,
}

/// Points for the Hotspots scatter, highest risk first. `level` is "cluster" (default) or "file";
/// risk is the geometric mean of the heat (rate) and connectedness (dependents within 2 hops)
/// percentiles, ranked among the points returned before `limit`.
pub fn points(v: &View, level: &str, lane: Option<&str>, limit: usize) -> Vec<MetricPoint> {
    points_with(v, level, lane, limit, None)
}

/// [`points`], with file-level connectedness taken from `file_conn` (per file of `v`) instead of
/// the maximum over the file's windows: `basis=imports`, the file import graph of appendix C.
pub fn points_with(
    v: &View,
    level: &str,
    lane: Option<&str>,
    limit: usize,
    file_conn: Option<&[Connectedness]>,
) -> Vec<MetricPoint> {
    let mut aggs: Vec<Agg> = Vec::new();
    if level == "file" {
        for (fi, f) in v.files.iter().enumerate() {
            if lane.is_some_and(|l| l != f.lane) {
                continue;
            }
            let Some(best) = f.windows.iter().copied().max_by(|&a, &b| {
                v.windows[a]
                    .header
                    .risk
                    .total_cmp(&v.windows[b].header.risk)
            }) else {
                continue;
            };
            let h = &v.windows[best].header;
            let mut conn = h.conn.clone();
            if let Some(fc) = file_conn.and_then(|fc| fc.get(fi)) {
                conn = fc.clone();
            } else {
                for &w in &f.windows {
                    let c = &v.windows[w].header.conn;
                    conn.dependents2 = conn.dependents2.max(c.dependents2);
                    conn.grade = conn.grade.max(c.grade);
                }
            }
            aggs.push(Agg {
                key: h.key.clone(),
                label: f.path.clone(),
                lane: f.lane.to_string(),
                heat: h.heat.clone(),
                conn,
                window: Some(h.id.clone()),
            });
        }
    } else {
        let mut members: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, w) in v.windows.iter().enumerate() {
            members
                .entry(w.header.cluster.as_str())
                .or_default()
                .push(i);
        }
        for c in &v.bootstrap.base.clusters {
            if lane.is_some_and(|l| l != c.lane) {
                continue;
            }
            let Some(ws) = members.get(c.id.as_str()) else {
                continue;
            };
            // Heat: line-weighted mean rate over the cluster's files; connectedness: the maximum.
            let mut seen = HashSet::new();
            let (mut rate_lines, mut lines, mut touches) = (0.0, 0.0, 0u32);
            let mut settled = true;
            let mut conn = Connectedness {
                grade: c.conn,
                dependents2: 0,
                fan_in: 0,
                fan_out: 0,
                tests: 0,
                reach: 0,
                cycle: None,
            };
            for &w in ws {
                let h = &v.windows[w].header;
                let n = (h.lines.end - h.lines.start + 1) as f64;
                rate_lines += h.heat.rate * n;
                lines += n;
                settled &= h.heat.settled;
                if seen.insert(v.windows[w].file) {
                    touches += h.heat.touches;
                }
                conn.dependents2 = conn.dependents2.max(h.conn.dependents2);
                conn.fan_in = conn.fan_in.max(h.conn.fan_in);
                conn.fan_out = conn.fan_out.max(h.conn.fan_out);
                conn.tests = conn.tests.max(h.conn.tests);
                conn.reach = conn.reach.max(h.conn.reach);
                conn.cycle = conn.cycle.max(h.conn.cycle);
            }
            let rate = if lines > 0.0 { rate_lines / lines } else { 0.0 };
            aggs.push(Agg {
                key: c.id.clone(),
                label: c.label.clone(),
                lane: c.lane.clone(),
                heat: Heat {
                    grade: c.heat,
                    rate,
                    ci: [rate, rate],
                    settled,
                    touches,
                    in_range: None,
                },
                conn,
                window: None,
            });
        }
    }
    let heat_pct = percentiles(&aggs.iter().map(|a| a.heat.rate).collect::<Vec<_>>());
    let conn_pct = percentiles(
        &aggs
            .iter()
            .map(|a| a.conn.dependents2 as f64)
            .collect::<Vec<_>>(),
    );
    let mut pts: Vec<MetricPoint> = aggs
        .into_iter()
        .enumerate()
        .map(|(i, a)| MetricPoint {
            key: a.key,
            label: a.label,
            lane: a.lane,
            heat: a.heat,
            conn: a.conn,
            heat_pct: heat_pct[i],
            conn_pct: conn_pct[i],
            risk: (heat_pct[i] * conn_pct[i]).sqrt(),
            window: a.window,
        })
        .collect();
    pts.sort_by(|a, b| b.risk.total_cmp(&a.risk));
    pts.truncate(limit);
    pts
}

/// Per file of `v`: connectedness on the file import graph alone (appendix C, `axes.py`), from
/// the imports its language packs resolve.
pub fn file_import_conn(
    v: &View,
    facts: &crate::incremental::ViewFacts,
    cuts: cv_link::conn::ConnCuts,
) -> Vec<Connectedness> {
    let mut imports: Vec<(usize, usize)> = Vec::new();
    for (fi, targets) in facts.resolved.iter().enumerate() {
        imports.extend(
            targets
                .iter()
                .flatten()
                .filter(|&&t| t != fi)
                .map(|&t| (fi, t)),
        );
    }
    imports.sort_unstable();
    imports.dedup();
    let lanes: Vec<&str> = v.files.iter().map(|f| f.lane).collect();
    cv_link::conn::file_import_connectedness(&lanes, &imports, cuts)
        .into_iter()
        .map(|c| Connectedness {
            grade: c.grade,
            dependents2: c.dependents2,
            fan_in: c.fan_in,
            fan_out: c.fan_out,
            tests: c.tests,
            reach: c.reach,
            cycle: (c.cycle > 1).then_some(c.cycle),
        })
        .collect()
}

#[derive(Clone, Copy)]
pub struct CoChangeLimits {
    pub max_commits: usize,
    /// Bulk commits (renames, formatting) say little about coupling.
    pub max_files_per_commit: usize,
    pub min_support: u32,
    pub min_jaccard: f64,
    pub max_pairs: usize,
    /// Cluster pairs: a cluster changes more often than any one of its files, which dilutes its
    /// jaccard, so they are kept from a lower one; at most `max_cluster_partners` per cluster
    /// (strongest first) and `max_cluster_pairs` in all.
    pub min_cluster_jaccard: f64,
    pub max_cluster_partners: usize,
    pub max_cluster_pairs: usize,
}

impl Default for CoChangeLimits {
    fn default() -> Self {
        CoChangeLimits {
            // As many as heat reads (cv serve --max-commits default), so the two agree.
            max_commits: 2000,
            max_files_per_commit: 40,
            min_support: 3,
            min_jaccard: 0.25,
            max_pairs: 600,
            min_cluster_jaccard: 0.1,
            max_cluster_partners: 12,
            max_cluster_pairs: 4000,
        }
    }
}

/// Co-change at two levels of the map, from the same commits.
#[derive(Debug, Default)]
pub struct CoChange {
    /// Files that change together but have no static link between them.
    pub files: Vec<CoChangePair>,
    /// Clusters whose files change together, linked or not.
    pub clusters: Vec<CoChangeClusterPair>,
}

/// Pairs of files (and of clusters) from `v` that change together in the history of `head`;
/// support = commits touching both, jaccard = support / commits touching either.
pub fn cochange(repo: &JjRepo, v: &View, head: &str, lim: CoChangeLimits) -> CoChange {
    let Ok(hist) = repo.history(head) else {
        return CoChange::default();
    };
    let commits = hist.take(lim.max_commits).filter_map(Result::ok).map(|c| {
        let mut fs: Vec<usize> = c
            .files
            .iter()
            .filter_map(|p| v.by_path.get(p).copied())
            .collect();
        fs.sort_unstable();
        fs.dedup();
        fs
    });
    cochange_from(commits, v, lim)
}

/// The pair statistics over per-commit file lists (file indexes into `v.files`, sorted, unique).
pub fn cochange_from(
    commits: impl Iterator<Item = Vec<usize>>,
    v: &View,
    lim: CoChangeLimits,
) -> CoChange {
    // The cluster of each file's first window (files split over clusters count where they start),
    // as an index into `cluster_ids`.
    let mut cluster_ix: HashMap<&str, usize> = HashMap::new();
    let mut cluster_ids: Vec<&str> = Vec::new();
    let mut cluster_of: HashMap<usize, usize> = HashMap::new();
    for w in &v.windows {
        let id = w.header.cluster.as_str();
        let n = cluster_ids.len();
        let ci = *cluster_ix.entry(id).or_insert(n);
        if ci == n {
            cluster_ids.push(id);
        }
        cluster_of.entry(w.file).or_insert(ci);
    }
    let mut count: HashMap<usize, u32> = HashMap::new();
    let mut pairs: HashMap<(usize, usize), u32> = HashMap::new();
    let mut ccount: HashMap<usize, u32> = HashMap::new();
    let mut cpairs: HashMap<(usize, usize), u32> = HashMap::new();
    let mut cs: Vec<usize> = Vec::new();
    for fs in commits {
        if fs.is_empty() || fs.len() > lim.max_files_per_commit {
            continue;
        }
        for &f in &fs {
            *count.entry(f).or_default() += 1;
        }
        for i in 0..fs.len() {
            for j in i + 1..fs.len() {
                *pairs.entry((fs[i], fs[j])).or_default() += 1;
            }
        }
        cs.clear();
        cs.extend(fs.iter().filter_map(|f| cluster_of.get(f).copied()));
        cs.sort_unstable();
        cs.dedup();
        for &c in &cs {
            *ccount.entry(c).or_default() += 1;
        }
        for i in 0..cs.len() {
            for j in i + 1..cs.len() {
                *cpairs.entry((cs[i], cs[j])).or_default() += 1;
            }
        }
    }
    let mut linked: HashSet<(usize, usize)> = HashSet::new();
    let mut clinked: HashSet<(usize, usize)> = HashSet::new();
    for l in &v.links {
        let (a, b) = (v.windows[l.from].file, v.windows[l.to].file);
        linked.insert((a.min(b), a.max(b)));
        let ca = cluster_ix.get(v.windows[l.from].header.cluster.as_str());
        let cb = cluster_ix.get(v.windows[l.to].header.cluster.as_str());
        if let (Some(&ca), Some(&cb)) = (ca, cb) {
            clinked.insert((ca.min(cb), ca.max(cb)));
        }
    }
    let cluster = |f: usize| cluster_of.get(&f).map(|&c| cluster_ids[c].to_string());
    let mut out: Vec<CoChangePair> = pairs
        .into_iter()
        .filter(|(k, s)| *s >= lim.min_support && !linked.contains(k))
        .filter_map(|((a, b), s)| {
            let jaccard = s as f64 / (count[&a] + count[&b] - s) as f64;
            (jaccard >= lim.min_jaccard).then(|| CoChangePair {
                a: v.files[a].path.clone(),
                b: v.files[b].path.clone(),
                support: s,
                jaccard,
                cluster_a: cluster(a),
                cluster_b: cluster(b),
            })
        })
        .collect();
    // Strongest first by how sure the jaccard is: three commits out of three rank below thirty of
    // forty, so a cap keeps the pairs with history behind them.
    let sure = |p: &CoChangePair| wilson_lower(p.support, union_of(p.support, p.jaccard));
    out.sort_by(|x, y| {
        sure(y)
            .total_cmp(&sure(x))
            .then(y.support.cmp(&x.support))
            .then(x.a.cmp(&y.a))
    });
    out.truncate(lim.max_pairs);

    let mut cl: Vec<((usize, usize), u32, f64)> = cpairs
        .into_iter()
        .filter(|(_, s)| *s >= lim.min_support)
        .map(|((a, b), s)| ((a, b), s, s as f64 / (ccount[&a] + ccount[&b] - s) as f64))
        .filter(|&(_, _, j)| j >= lim.min_cluster_jaccard)
        .collect();
    let sure = |&(_, s, j): &((usize, usize), u32, f64)| wilson_lower(s, union_of(s, j));
    cl.sort_by(|x, y| {
        sure(y)
            .total_cmp(&sure(x))
            .then(y.1.cmp(&x.1))
            .then(x.0.cmp(&y.0))
    });
    // Strongest first; a pair stays while either cluster still has room for a partner, so a busy
    // cluster's weak pairs don't crowd out a quiet cluster's best one.
    let mut partners: HashMap<usize, usize> = HashMap::new();
    let mut clusters = Vec::new();
    for ((a, b), support, jaccard) in cl {
        if clusters.len() >= lim.max_cluster_pairs {
            break;
        }
        let (na, nb) = (
            partners.get(&a).copied().unwrap_or(0),
            partners.get(&b).copied().unwrap_or(0),
        );
        if na >= lim.max_cluster_partners && nb >= lim.max_cluster_partners {
            continue;
        }
        *partners.entry(a).or_default() += 1;
        *partners.entry(b).or_default() += 1;
        clusters.push(CoChangeClusterPair {
            a: cluster_ids[a].to_string(),
            b: cluster_ids[b].to_string(),
            support,
            jaccard: (jaccard * 1000.0).round() / 1000.0,
            linked: clinked.contains(&(a, b)),
        });
    }
    CoChange {
        files: out,
        clusters,
    }
}

/// Commits touching either side of a pair, from its support and jaccard.
fn union_of(support: u32, jaccard: f64) -> u32 {
    if jaccard > 0.0 {
        (support as f64 / jaccard).round() as u32
    } else {
        support
    }
}

/// Lower bound of the 95% Wilson interval of `k` successes in `n` trials (web cochange.ts
/// `coStrength` draws by the same bound).
pub fn wilson_lower(k: u32, n: u32) -> f64 {
    if n == 0 {
        return 0.0;
    }
    let (z, n, p) = (1.96f64, n as f64, k as f64 / n as f64);
    let z2 = z * z;
    ((p + z2 / (2.0 * n) - z * ((p * (1.0 - p) + z2 / (4.0 * n)) / n).sqrt()) / (1.0 + z2 / n))
        .max(0.0)
}

#[cfg(test)]
mod tests {
    use super::{percentiles, wilson_lower};

    #[test]
    fn wilson_ranks_history_over_tiny_samples() {
        assert!(wilson_lower(3, 3) < wilson_lower(30, 40));
        assert!(wilson_lower(0, 10) == 0.0 && wilson_lower(5, 0) == 0.0);
    }

    #[test]
    fn mid_rank_percentiles() {
        let p = percentiles(&[0.0, 0.0, 1.0, 2.0]);
        assert_eq!(p, vec![0.25, 0.25, 0.625, 0.875]);
    }
}
