//! Thread anchoring: fingerprints of the anchored lines and re-anchoring after a rewrite.
//!
//! A thread anchors to lines `start..=end` of a file in a change. When the change gets a new
//! commit, the old and new file texts are diffed line by line:
//!
//! - every anchored line survived unchanged and stays contiguous → the anchor moves with it;
//! - otherwise the anchored text (its fingerprint) is searched for in the new file, nearest to
//!   where the diff says the lines went → moved there;
//! - otherwise the thread is outdated (lines changed), keeping its last good commit.

use cv_contracts::review::Fingerprint;

/// FNV-1a 64 of the lines joined with "\n", as 16 hex digits.
pub fn hash_lines(lines: &[&str]) -> String {
    let joined = lines.join("\n");
    format!("{:016x}", cv_store::fnv64(joined.as_bytes()))
}

/// Lines of a text, without terminators.
pub fn split(text: &str) -> Vec<&str> {
    let mut v: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        v.pop();
    }
    v.into_iter()
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect()
}

/// Fingerprint of lines `start..=end` (1-based) and the two lines either side.
pub fn fingerprint(lines: &[&str], start: u32, end: u32) -> Fingerprint {
    let n = lines.len();
    let s = (start.max(1) as usize - 1).min(n);
    let e = (end as usize).clamp(s, n);
    Fingerprint {
        text: hash_lines(&lines[s..e]),
        before: hash_lines(&lines[s.saturating_sub(2)..s]),
        after: hash_lines(&lines[e..(e + 2).min(n)]),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reanchor {
    Unchanged,
    Moved { start: u32, end: u32 },
    Outdated,
}

/// For each old line (0-based), the new line it survived as, if it is unchanged.
pub fn line_map(old: &[&str], new: &[&str]) -> Vec<Option<usize>> {
    let mut map = vec![None; old.len()];
    let mut pre = 0;
    while pre < old.len() && pre < new.len() && old[pre] == new[pre] {
        map[pre] = Some(pre);
        pre += 1;
    }
    let mut suf = 0;
    while suf < old.len() - pre
        && suf < new.len() - pre
        && old[old.len() - 1 - suf] == new[new.len() - 1 - suf]
    {
        map[old.len() - 1 - suf] = Some(new.len() - 1 - suf);
        suf += 1;
    }
    let (a, b) = (&old[pre..old.len() - suf], &new[pre..new.len() - suf]);
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 || n.saturating_mul(m) > 16_000_000 {
        return map;
    }
    // LCS table over the changed middle, filled from the end.
    let w = m + 1;
    let mut t = vec![0u32; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[i * w + j] = if a[i] == b[j] {
                t[(i + 1) * w + j + 1] + 1
            } else {
                t[(i + 1) * w + j].max(t[i * w + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            map[pre + i] = Some(pre + j);
            i += 1;
            j += 1;
        } else if t[(i + 1) * w + j] >= t[i * w + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    map
}

/// Where lines `start..=end` of `old` went in `new`.
pub fn reanchor(old: &[&str], new: &[&str], start: u32, end: u32, fp: &Fingerprint) -> Reanchor {
    if old.is_empty() || start == 0 || start as usize > old.len() {
        return Reanchor::Outdated;
    }
    let end = end.clamp(start, old.len() as u32);
    let map = line_map(old, new);
    let (s, e) = (start as usize - 1, end as usize - 1);
    let mapped: Vec<Option<usize>> = map[s..=e].to_vec();
    if let (Some(Some(ns)), true) = (mapped.first(), mapped.iter().all(Option::is_some)) {
        let contiguous = mapped.iter().enumerate().all(|(k, m)| *m == Some(ns + k));
        if contiguous {
            let (ns, ne) = (*ns as u32 + 1, (*ns + (e - s)) as u32 + 1);
            return if ns == start && ne == end {
                Reanchor::Unchanged
            } else {
                Reanchor::Moved { start: ns, end: ne }
            };
        }
    }
    // Expected position: where the nearest surviving line above the anchor went.
    let expect = (0..s)
        .rev()
        .find_map(|i| map[i].map(|j| j + (s - i)))
        .unwrap_or(s);
    let len = e - s + 1;
    if new.len() < len {
        return Reanchor::Outdated;
    }
    let best = (0..=new.len() - len)
        .filter(|&j| hash_lines(&new[j..j + len]) == fp.text)
        .min_by_key(|&j| j.abs_diff(expect));
    match best {
        Some(j) => Reanchor::Moved {
            start: j as u32 + 1,
            end: (j + len) as u32,
        },
        None => Reanchor::Outdated,
    }
}

/// Lines a patch writes (as [`apply_patches`] counts them).
fn patch_len(p: &cv_contracts::review::LinePatch) -> i64 {
    if p.text.is_empty() {
        if p.end >= p.start.max(1) { 0 } else { 1 }
    } else {
        split(&p.text).len().max(1) as i64
    }
}

/// Where lines `start..=end` of a file go when `patches` (all against that file, as
/// [`apply_patches`] takes them) are applied; `new_len` is the patched file's line count.
///
/// A patch that overlaps the range, or inserts inside it or right next to it, joins the range: the
/// result covers the new text that answers the anchored lines. Patches elsewhere only shift it.
/// A range whose lines were all deleted collapses to the line where they were.
pub fn map_through_patches(
    start: u32,
    end: u32,
    patches: &[cv_contracts::review::LinePatch],
    new_len: usize,
) -> (u32, u32) {
    let (mut s, mut e) = (start.max(1) as i64, end.max(start.max(1)) as i64);
    let span = |p: &cv_contracts::review::LinePatch| (p.start.max(1) as i64, p.end as i64);
    let touches = |p: &cv_contracts::review::LinePatch, s: i64, e: i64| {
        let (ps, pe) = span(p);
        if pe >= ps {
            ps <= e && pe >= s
        } else {
            // An insertion before line ps: inside the range or right next to it.
            ps >= s && ps <= e + 1
        }
    };
    let mut joined = vec![false; patches.len()];
    loop {
        let mut grew = false;
        for (i, p) in patches.iter().enumerate() {
            if !joined[i] && touches(p, s, e) {
                joined[i] = true;
                let (ps, pe) = span(p);
                if pe >= ps {
                    s = s.min(ps);
                    e = e.max(pe);
                }
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    let delta = |p: &cv_contracts::review::LinePatch| {
        let (ps, pe) = span(p);
        patch_len(p) - (pe - ps + 1).max(0)
    };
    let before: i64 = patches
        .iter()
        .enumerate()
        .filter(|(i, p)| !joined[*i] && span(p).0 < s)
        .map(|(_, p)| delta(p))
        .sum();
    let inside: i64 = patches
        .iter()
        .enumerate()
        .filter(|(i, _)| joined[*i])
        .map(|(_, p)| delta(p))
        .sum();
    let max = new_len.max(1) as i64;
    let ns = (s + before).clamp(1, max);
    let len = (e - s + 1 + inside).max(1);
    let ne = (ns + len - 1).clamp(ns, max);
    (ns as u32, ne as u32)
}

/// Where lines `start..=end` of `old` go in `new` when the whole file was rewritten: as
/// [`reanchor`] when the lines survived; otherwise the new lines between the nearest surviving
/// lines above and below them.
pub fn map_through_texts(old: &[&str], new: &[&str], start: u32, end: u32) -> (u32, u32) {
    match reanchor(old, new, start, end, &fingerprint(old, start, end)) {
        Reanchor::Moved { start, end } => return (start, end),
        Reanchor::Unchanged => return (start, end),
        Reanchor::Outdated => {}
    }
    let map = line_map(old, new);
    let s0 = (start.max(1) as usize - 1).min(old.len());
    let e0 = (end as usize).clamp(s0, old.len());
    // 0-based: first new line after the nearest surviving line above; last before the one below.
    let lo = (0..s0).rev().find_map(|i| map[i]).map_or(0, |j| j + 1);
    let hi = (e0..old.len()).find_map(|i| map[i]).unwrap_or(new.len());
    let max = new.len().max(1);
    let ns = (lo + 1).min(max);
    (ns as u32, hi.clamp(ns, max) as u32)
}

/// Applies line patches (1-based inclusive; `end = start - 1` inserts) to `text`, bottom-up.
pub fn apply_patches(
    text: &str,
    patches: &[cv_contracts::review::LinePatch],
) -> Result<String, String> {
    let trailing_newline = text.is_empty() || text.ends_with('\n');
    let mut lines: Vec<String> = split(text).into_iter().map(String::from).collect();
    let mut ps: Vec<&cv_contracts::review::LinePatch> = patches.iter().collect();
    ps.sort_by_key(|p| std::cmp::Reverse(p.start));
    for p in ps {
        let start = p.start.max(1) as usize;
        let end = p.end as usize;
        if end + 1 < start || start > lines.len() + 1 || end > lines.len() {
            return Err(format!(
                "patch {}–{} is outside the file ({} lines)",
                p.start,
                p.end,
                lines.len()
            ));
        }
        let new: Vec<String> = if p.text.is_empty() && end >= start {
            // An empty replacement deletes the lines.
            vec![]
        } else {
            split(&p.text).into_iter().map(String::from).collect()
        };
        let new = if new.is_empty() && !p.text.is_empty() {
            vec![String::new()]
        } else {
            new
        };
        lines.splice(start - 1..end, new);
    }
    let mut out = lines.join("\n");
    if trailing_newline && !lines.is_empty() {
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cv_contracts::review::LinePatch;

    fn fp(lines: &[&str], s: u32, e: u32) -> Fingerprint {
        fingerprint(lines, s, e)
    }

    #[test]
    fn moved_lines_follow() {
        let old = ["a", "b", "target1", "target2", "c"];
        let new = ["new", "new2", "a", "b", "target1", "target2", "c"];
        assert_eq!(
            reanchor(&old, &new, 3, 4, &fp(&old, 3, 4)),
            Reanchor::Moved { start: 5, end: 6 }
        );
        assert_eq!(
            reanchor(&old, &old, 3, 4, &fp(&old, 3, 4)),
            Reanchor::Unchanged
        );
    }

    #[test]
    fn changed_lines_are_outdated() {
        let old = ["a", "b", "target1", "target2", "c"];
        let new = ["a", "b", "target1 changed", "target2", "c"];
        assert_eq!(
            reanchor(&old, &new, 3, 4, &fp(&old, 3, 4)),
            Reanchor::Outdated
        );
    }

    #[test]
    fn block_moved_elsewhere_is_found_by_fingerprint() {
        let old = ["x", "fn a", "body", "y", "z"];
        // The block moved below other code and the lines around it changed.
        let new = ["x", "y", "z2", "w", "fn a", "body"];
        assert_eq!(
            reanchor(&old, &new, 2, 3, &fp(&old, 2, 3)),
            Reanchor::Moved { start: 5, end: 6 }
        );
    }

    #[test]
    fn patches_replace_insert_and_delete() {
        let t = "1\n2\n3\n";
        let p = |s, e, text: &str| LinePatch {
            start: s,
            end: e,
            text: text.into(),
        };
        assert_eq!(apply_patches(t, &[p(2, 2, "two")]).unwrap(), "1\ntwo\n3\n");
        assert_eq!(
            apply_patches(t, &[p(1, 0, "# top")]).unwrap(),
            "# top\n1\n2\n3\n"
        );
        assert_eq!(apply_patches(t, &[p(2, 3, "")]).unwrap(), "1\n");
        assert_eq!(
            apply_patches(t, &[p(1, 1, "a\nb"), p(3, 3, "c")]).unwrap(),
            "a\nb\n2\nc\n"
        );
        assert!(apply_patches(t, &[p(5, 5, "x")]).is_err());
    }

    #[test]
    fn anchors_map_through_several_patches() {
        let t: String = (1..=20).map(|i| format!("l{i}\n")).collect();
        let p = |s, e, text: &str| LinePatch {
            start: s,
            end: e,
            text: text.into(),
        };
        let check = |ps: &[LinePatch], s, e, want: &[&str]| {
            let out = apply_patches(&t, ps).unwrap();
            let lines = split(&out);
            let (ns, ne) = map_through_patches(s, e, ps, lines.len());
            assert_eq!(&lines[ns as usize - 1..ne as usize], want, "{ps:?} {s}-{e}");
        };
        // A patch above grows the file; a patch inside rewrites the anchored lines; one below.
        let ps = [p(2, 2, "a\nb\nc"), p(10, 11, "X"), p(18, 18, "z")];
        check(&ps, 10, 11, &["X"]);
        check(&ps, 9, 12, &["l9", "X", "l12"]);
        // Untouched lines just shift.
        check(&ps, 5, 6, &["l5", "l6"]);
        check(&ps, 15, 15, &["l15"]);
        // An insertion right after the anchor and an edit inside it both join.
        let ps = [p(10, 10, "ten"), p(12, 11, "new1\nnew2"), p(1, 1, "")];
        check(&ps, 10, 11, &["ten", "l11", "new1", "new2"]);
        // Every anchored line deleted: collapses onto the line that took their place.
        let ps = [p(10, 11, ""), p(3, 2, "x")];
        check(&ps, 10, 11, &["l12"]);
        // Overlapping the anchor's edges pulls the whole patched block in.
        let ps = [p(8, 10, "A\nB"), p(11, 13, "C")];
        check(&ps, 10, 11, &["A", "B", "C"]);
    }

    #[test]
    fn anchors_map_through_rewritten_texts() {
        let old = ["a", "b", "c", "d", "e"];
        let new = ["a", "b", "C1", "C2", "d", "e"];
        assert_eq!(map_through_texts(&old, &new, 3, 3), (3, 4));
        assert_eq!(map_through_texts(&old, &new, 4, 5), (5, 6));
        let new = ["a", "e"];
        assert_eq!(map_through_texts(&old, &new, 2, 4), (2, 2));
    }
}
