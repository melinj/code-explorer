//! What indexing hands to cv-layout beyond windows: the clustering edges (links by kind, import
//! edges, co-change) and the in-memory placement of the last layout per repo, so a new view of the
//! same repo keeps clusters, labels and positions (DESIGN §4.10.6).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use cv_lang::{BlobFacts, RefKind};
use cv_layout::{CoChangeIn, EdgeKind, LinkIn, Placement};

/// Last placement per repo (keyed by repo root), in memory for now.
fn memory() -> &'static Mutex<HashMap<String, Arc<Placement>>> {
    static M: OnceLock<Mutex<HashMap<String, Arc<Placement>>>> = OnceLock::new();
    M.get_or_init(Default::default)
}

pub fn last_placement(repo: &str) -> Option<Arc<Placement>> {
    memory().lock().ok()?.get(repo).cloned()
}

pub fn remember_placement(repo: &str, p: Placement) {
    if let Ok(mut m) = memory().lock() {
        m.insert(repo.to_string(), Arc::new(p));
    }
}

/// Edits the repo's last placement in place (layout PATCH: moved positions carry into the next
/// re-index of this process).
pub fn update_placement(repo: &str, f: impl FnOnce(&mut Placement)) {
    let Ok(mut m) = memory().lock() else {
        return;
    };
    if let Some(p) = m.get_mut(repo) {
        f(Arc::make_mut(p));
    }
}

/// Forgets the repo's last placement (tests: what a restart does).
pub fn forget_placement(repo: &str) {
    if let Ok(mut m) = memory().lock() {
        m.remove(repo);
    }
}

/// Sets a human or agent label on a cluster of the repo's last layout; it carries over to later
/// layouts by member Jaccard ≥ 0.6. `cluster` is the index in `c{index}`.
pub fn set_cluster_label(repo: &str, cluster: usize, label: &str, source: &str) -> bool {
    let Ok(mut m) = memory().lock() else {
        return false;
    };
    let Some(p) = m.get_mut(repo) else {
        return false;
    };
    Arc::make_mut(p).set_label(cluster, label, source)
}

/// Code links by kind, plus one import edge per resolved import: from the window holding the
/// import line to the first window of the imported file.
pub fn links(
    link_out: &[cv_link::LinkOut],
    facts: &[&BlobFacts],
    resolved: &[Vec<Option<usize>>],
    file_windows: &[Vec<(u32, u32, usize)>],
) -> Vec<LinkIn> {
    let mut out: Vec<LinkIn> = link_out
        .iter()
        .map(|l| LinkIn {
            from: l.from_window,
            to: l.to_window,
            kind: match l.kind {
                RefKind::Call => EdgeKind::Call,
                RefKind::Inherit => EdgeKind::Inherit,
                RefKind::Implement => EdgeKind::Implement,
            },
        })
        .collect();
    for (fi, f) in facts.iter().enumerate() {
        for (imp, target) in f.imports.iter().zip(&resolved[fi]) {
            let Some(t) = *target else { continue };
            if t == fi {
                continue;
            }
            let ws = &file_windows[fi];
            let from = ws
                .iter()
                .find(|w| w.0 <= imp.line && imp.line <= w.1)
                .or(ws.first());
            if let (Some(from), Some(to)) = (from, file_windows[t].first()) {
                out.push(LinkIn {
                    from: from.2,
                    to: to.2,
                    kind: EdgeKind::Import,
                });
            }
        }
    }
    out
}

/// Co-change pairs from churn, as file indexes.
pub fn co_change(pairs: &[cv_churn::CoChange], by_path: &HashMap<&str, usize>) -> Vec<CoChangeIn> {
    pairs
        .iter()
        .filter_map(|c| {
            Some(CoChangeIn {
                a: *by_path.get(c.a.as_str())?,
                b: *by_path.get(c.b.as_str())?,
                jaccard: c.jaccard,
            })
        })
        .collect()
}
