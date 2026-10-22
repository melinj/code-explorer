//! Rename detection for history walks (B5: renames carry history). jj (like git) records no
//! renames, so a commit's deletions and additions are paired by content:
//! 1. identical blob ids (a pure move);
//! 2. then the most similar remaining pair by line-set Jaccard ≥ [`MIN_SIMILARITY`], among pairs
//!    sharing a file name, or sharing an extension when the commit deletes and adds only a few files.

use std::collections::{HashMap, HashSet};

pub const MIN_SIMILARITY: f64 = 0.5;
/// Above this many deletions × additions only same-name pairs are compared.
const MAX_ANY_PAIRS: usize = 64;
/// Same-name candidates compared per deleted file (closest paths first).
const PER_FILE: usize = 3;
/// Content comparisons per commit; a mass move beyond this is left to exact-id matching.
const MAX_COMPARISONS: usize = 200;
/// Blobs larger than this are compared by id only.
const MAX_COMPARE_BYTES: usize = 512 * 1024;

fn name(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

fn ext(p: &str) -> &str {
    let n = name(p);
    n.rfind('.').map(|i| &n[i..]).unwrap_or("")
}

fn line_set(b: &[u8]) -> HashSet<&[u8]> {
    b.split(|&c| c == b'\n')
        .map(|l| l.trim_ascii())
        .filter(|l| !l.is_empty())
        .collect()
}

pub fn similarity(a: &[u8], b: &[u8]) -> f64 {
    let (sa, sb) = (line_set(a), line_set(b));
    if sa.is_empty() && sb.is_empty() {
        return 1.0;
    }
    let inter = sa.intersection(&sb).count();
    inter as f64 / (sa.len() + sb.len() - inter) as f64
}

/// Pairs `deleted` (path, blob id) with `added` (path, blob id) into (old path, new path) renames.
/// `read(path, blob)` loads a blob for the similarity pass.
pub fn detect(
    deleted: &[(String, String)],
    added: &[(String, String)],
    read: impl Fn(&str, &str) -> Option<Vec<u8>>,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut used_d = vec![false; deleted.len()];
    let mut used_a = vec![false; added.len()];
    // 1. Exact moves (first unused match by id, in path order for determinism).
    let mut by_id: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, (_, id)) in added.iter().enumerate() {
        by_id.entry(id.as_str()).or_default().push(i);
    }
    for (di, (dp, id)) in deleted.iter().enumerate() {
        if let Some(ai) = by_id
            .get(id.as_str())
            .and_then(|v| v.iter().copied().find(|&a| !used_a[a]))
        {
            used_a[ai] = true;
            used_d[di] = true;
            out.push((dp.clone(), added[ai].0.clone()));
        }
    }
    // 2. Similar content.
    let rest_d: Vec<usize> = (0..deleted.len()).filter(|&i| !used_d[i]).collect();
    let rest_a: Vec<usize> = (0..added.len()).filter(|&i| !used_a[i]).collect();
    if rest_d.is_empty() || rest_a.is_empty() {
        return out;
    }
    let any = rest_d.len() * rest_a.len() <= MAX_ANY_PAIRS;
    let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
    for &a in &rest_a {
        by_name.entry(name(&added[a].0)).or_default().push(a);
    }
    // Trailing path components two paths share (`x/node.yaml` in `a/x/node.yaml`, `b/x/node.yaml`).
    let common_tail = |p: &str, q: &str| {
        p.rsplit('/')
            .zip(q.rsplit('/'))
            .take_while(|(a, b)| a == b)
            .count()
    };
    let mut cands: Vec<(usize, usize)> = Vec::new();
    for &d in &rest_d {
        let dp = &deleted[d].0;
        if any {
            cands.extend(
                rest_a
                    .iter()
                    .filter(|&&a| ext(dp) == ext(&added[a].0))
                    .map(|&a| (d, a)),
            );
        } else if let Some(same) = by_name.get(name(dp)) {
            let mut same = same.clone();
            same.sort_by_key(|&a| (std::cmp::Reverse(common_tail(dp, &added[a].0)), a));
            cands.extend(same.into_iter().take(PER_FILE).map(|a| (d, a)));
        }
        if cands.len() >= MAX_COMPARISONS {
            cands.truncate(MAX_COMPARISONS);
            break;
        }
    }
    if cands.is_empty() {
        return out;
    }
    let mut cache: HashMap<(bool, usize), Option<Vec<u8>>> = HashMap::new();
    let mut load = |side: bool, i: usize| -> Option<Vec<u8>> {
        cache
            .entry((side, i))
            .or_insert_with(|| {
                let (p, id) = if side { &added[i] } else { &deleted[i] };
                read(p, id).filter(|b| b.len() <= MAX_COMPARE_BYTES)
            })
            .clone()
    };
    let mut scored: Vec<(f64, usize, usize)> = Vec::new();
    for (d, a) in cands {
        let (Some(db), Some(ab)) = (load(false, d), load(true, a)) else {
            continue;
        };
        let s = similarity(&db, &ab);
        if s >= MIN_SIMILARITY {
            scored.push((s, d, a));
        }
    }
    // Best pairs first; ties by path order.
    scored.sort_by(|x, y| y.0.total_cmp(&x.0).then(x.1.cmp(&y.1)).then(x.2.cmp(&y.2)));
    for (_, d, a) in scored {
        if !used_d[d] && !used_a[a] {
            used_d[d] = true;
            used_a[a] = true;
            out.push((deleted[d].0.clone(), added[a].0.clone()));
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_moves_then_similar_content() {
        let blobs: HashMap<&str, &str> = [
            ("b1", "a\nb\nc\nd\n"),
            ("b2", "a\nb\nc\nX\n"),
            ("b3", "totally\ndifferent\n"),
            ("b4", "one\ntwo\n"),
        ]
        .into_iter()
        .collect();
        let s = |v: &[(&str, &str)]| -> Vec<(String, String)> {
            v.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        let deleted = s(&[("old/x.py", "b1"), ("old/y.py", "b4"), ("gone.py", "b3")]);
        let added = s(&[("new/x.py", "b2"), ("new/y2.py", "b4"), ("other.rs", "b3")]);
        let r = detect(&deleted, &added, |_, id| {
            blobs.get(id).map(|t| t.as_bytes().to_vec())
        });
        assert_eq!(
            r,
            vec![
                // b3 moved under a new name and extension: an exact move still counts.
                ("gone.py".to_string(), "other.rs".to_string()),
                ("old/x.py".to_string(), "new/x.py".to_string()),
                ("old/y.py".to_string(), "new/y2.py".to_string()),
            ]
        );
        assert!(similarity(b"a\nb\nc\nd", b"a\nb\nc\nX") >= 0.5);
        assert!(similarity(b"a\nb", b"c\nd") < 0.5);
    }
}
