//! B6 layout checks on the golden `linear` fixture (scripts/fixtures.sh), not synthetic input:
//! the five-fragment file src/shapes.py stays in at most two clusters, and indexing the next
//! commit (one code file and one config file changed) with the previous placement moves < 5% of
//! the windows (DESIGN §4.10.6; zero here), including the shapes.py fragments that share a
//! cluster with the changed src/helpers.py; every other cluster keeps its rect and the changed
//! ones stay in place. Skipped when `jj` is not on PATH.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use cv_server::index::{self, Options, View};
use serde_json::json;

/// Whether `jj` is on PATH. Tests that need it skip without it, loudly; under CI (or with
/// CV_REQUIRE_JJ set, as scripts/ci.sh does) a missing jj fails them instead of skipping.
fn have_jj() -> bool {
    let ok = Command::new("jj")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        assert!(
            std::env::var_os("CI").is_none() && std::env::var_os("CV_REQUIRE_JJ").is_none(),
            "jj is not on PATH: CI must install it (these tests would skip)"
        );
        eprintln!("SKIP: jj is not on PATH");
    }
    ok
}

/// Builds the golden repos into `dir`; returns the `linear` repo.
fn linear_fixture(dir: &Path) -> PathBuf {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/fixtures.sh");
    let out = Command::new("bash")
        .arg(&script)
        .env("FIXTURES_DIR", dir)
        .output()
        .expect("run fixtures.sh");
    assert!(
        out.status.success(),
        "fixtures.sh: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    dir.join("linear")
}

fn view_at(repo: &cv_vcs::JjRepo, rev: &str) -> View {
    let reg = index::registry();
    let c = repo.resolve_one(rev).unwrap();
    let spec = json!({ "mode": "snapshot", "rev": c.commit });
    index::build_view(repo, &reg, spec, &c, Options { max_commits: 50 }).unwrap()
}

/// A window by (path, first line, last line).
type Key = (String, u32, u32);

/// Each window's rect by [`Key`].
fn places(v: &View) -> HashMap<Key, (f64, f64, f64, f64)> {
    v.windows
        .iter()
        .map(|w| {
            let h = &w.header;
            (
                (h.path.clone(), h.lines.start, h.lines.end),
                (h.rect.x, h.rect.y, h.rect.w, h.rect.h),
            )
        })
        .collect()
}

#[test]
fn golden_fixture_keeps_fragments_together_and_moves_under_5_percent() {
    if !have_jj() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let path = linear_fixture(&tmp.path().join("repos"));
    let repo = cv_vcs::JjRepo::open(&path).unwrap();
    let key = repo.root().display().to_string();
    cv_server::layout_input::forget_placement(&key);

    // D: src/shapes.py is five 28-line top-level functions.
    let d = view_at(&repo, "description(substring:\"D: tune square\")");
    let frags: Vec<usize> = d.files[d.by_path["src/shapes.py"]].windows.clone();
    assert_eq!(frags.len(), 5, "shapes.py fragments");
    let clusters: BTreeSet<&str> = frags
        .iter()
        .map(|&w| d.windows[w].header.cluster.as_str())
        .collect();
    assert!(
        clusters.len() <= 2,
        "five fragments in {} clusters: {clusters:?}",
        clusters.len()
    );

    // E (one code file gains a function, one config value changes), laid out with D's placement.
    let e = view_at(&repo, "description(substring:\"E: more retries\")");
    assert_ne!(
        d.files[d.by_path["src/helpers.py"]].blob,
        e.files[e.by_path["src/helpers.py"]].blob
    );
    let (pd, pe) = (places(&d), places(&e));
    let unchanged_files: BTreeSet<&str> = d
        .files
        .iter()
        .filter(|f| e.by_path.get(&f.path).map(|&i| &e.files[i].blob) == Some(&f.blob))
        .map(|f| f.path.as_str())
        .collect();
    let shared: Vec<&(String, u32, u32)> = pd.keys().filter(|k| pe.contains_key(*k)).collect();
    assert!(shared.len() >= 10, "{} shared windows", shared.len());
    if std::env::var_os("CV_DEBUG_GOLDEN").is_some() {
        for (n, v) in [("D", &d), ("E", &e)] {
            for w in &v.windows {
                let h = &w.header;
                eprintln!(
                    "{n} {} {:?} {} {:?}",
                    h.path,
                    (h.lines.start, h.lines.end),
                    h.cluster,
                    (h.rect.x, h.rect.y, h.rect.w, h.rect.h)
                );
            }
            for c in &v.bootstrap.base.clusters {
                eprintln!(
                    "{n} cluster {} {} {:?} heat {} conn {}",
                    c.id,
                    c.label,
                    (c.rect.x, c.rect.y, c.rect.w, c.rect.h),
                    c.heat,
                    c.conn
                );
            }
        }
    }
    // §4.10.6 acceptance check: one changed file moves < 5% of windows. Inside the changed
    // cluster the surviving windows keep their offsets (helpers.py grows where it is), so here
    // nothing moves at all.
    let changed_clusters: BTreeSet<&str> = d
        .windows
        .iter()
        .filter(|w| !unchanged_files.contains(w.header.path.as_str()))
        .map(|w| w.header.cluster.as_str())
        .collect();
    assert_eq!(changed_clusters.len(), 2, "{changed_clusters:?}"); // src · shapes and config
    let cluster_at: HashMap<(&str, u32, u32), &str> = d
        .windows
        .iter()
        .map(|w| {
            let h = &w.header;
            (
                (h.path.as_str(), h.lines.start, h.lines.end),
                h.cluster.as_str(),
            )
        })
        .collect();
    let moved: Vec<&&(String, u32, u32)> = shared.iter().filter(|k| pd[**k] != pe[**k]).collect();
    assert!(
        (moved.len() as f64) < 0.05 * shared.len() as f64,
        "{} of {} windows moved: {moved:?}",
        moved.len(),
        shared.len()
    );
    // The unchanged shapes.py fragments share the changed cluster with helpers.py and stay put.
    let helpers_cluster = d.windows[d.files[d.by_path["src/helpers.py"]].windows[0]]
        .header
        .cluster
        .as_str();
    let mut beside = 0;
    for &w in &frags {
        let h = &d.windows[w].header;
        let k = (h.path.clone(), h.lines.start, h.lines.end);
        assert_eq!(pe.get(&k), Some(&pd[&k]), "shapes.py {k:?} moved");
        beside += usize::from(h.cluster == helpers_cluster);
    }
    assert!(
        beside >= 1,
        "no shapes.py fragment shares helpers.py's cluster"
    );
    let rect_of = |v: &View, id: &str| {
        let c = v
            .bootstrap
            .base
            .clusters
            .iter()
            .find(|c| c.id == id)
            .unwrap();
        (c.rect.x, c.rect.y, c.rect.w, c.rect.h)
    };
    for c in &d.bootstrap.base.clusters {
        let (dr, er) = (rect_of(&d, &c.id), rect_of(&e, &c.id));
        if changed_clusters.contains(c.id.as_str()) {
            // In place: same origin, never smaller.
            assert!(
                dr.0 == er.0 && dr.1 == er.1 && er.2 >= dr.2 && er.3 >= dr.3,
                "{} {dr:?} → {er:?}",
                c.label
            );
        } else {
            assert_eq!(dr, er, "cluster {} moved", c.label);
        }
    }
    // The changed cluster still holds the same windows: nothing left it.
    for k in &shared {
        let de = cluster_at[&(k.0.as_str(), k.1, k.2)];
        let ee = e.windows.iter().find(|w| {
            (
                w.header.path.as_str(),
                w.header.lines.start,
                w.header.lines.end,
            ) == (k.0.as_str(), k.1, k.2)
        });
        assert_eq!(
            ee.map(|w| w.header.cluster.as_str()),
            Some(de),
            "{k:?} changed cluster"
        );
    }
    // The five fragments stay together in E too.
    let ef: BTreeSet<&str> = e.files[e.by_path["src/shapes.py"]]
        .windows
        .iter()
        .map(|&w| e.windows[w].header.cluster.as_str())
        .collect();
    assert!(ef.len() <= 2, "{ef:?}");
    cv_server::layout_input::forget_placement(&key);
}

/// No two windows of `v` overlap; returns the overlapping pairs otherwise.
fn overlaps(v: &View) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (i, a) in v.windows.iter().enumerate() {
        for b in &v.windows[i + 1..] {
            if a.header.rect.intersects(&b.header.rect) {
                out.push((a.header.id.clone(), b.header.id.clone()));
            }
        }
    }
    out
}

/// §4.10.6 for in-place refreshes: a window that grows in a refresh (no re-layout) and would
/// cover its neighbour takes the nearest free spot; nothing else moves and nothing overlaps.
#[test]
fn refresh_moves_only_a_grown_window_that_overlaps() {
    if !have_jj() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let path = linear_fixture(&tmp.path().join("repos"));
    let jj = |args: &[&str]| {
        let out = Command::new("jj")
            .arg("--repository")
            .arg(&path)
            .args(["--config", "user.name=t", "--config", "user.email=t@t"])
            .args(args)
            .output()
            .expect("run jj");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let repo = cv_vcs::JjRepo::open(&path).unwrap();
    let key = repo.root().display().to_string();
    cv_server::layout_input::forget_placement(&key);
    let d = view_at(&repo, "description(substring:\"D: tune square\")");
    assert!(overlaps(&d).is_empty());
    // shapes.py gains ten lines: a window of it grows into a neighbour.
    jj(&[
        "new",
        "description(substring:\"D: tune square\")",
        "-m",
        "grow",
    ]);
    let file = path.join("src/shapes.py");
    let text = std::fs::read_to_string(&file).unwrap();
    let mut lines: Vec<&str> = text.lines().collect();
    let pad = vec!["    _pad = 0"; 10];
    lines.splice(2..2, pad);
    std::fs::write(&file, lines.join("\n") + "\n").unwrap();
    jj(&["status"]);
    let repo = cv_vcs::JjRepo::open(&path).unwrap();
    let c = repo.resolve_one("@").unwrap();
    let reg = index::registry();
    let (r, _) = cv_server::review_api::refresh_view(&d, &reg, &repo, &c, "test").unwrap();
    let grown: Vec<usize> = (0..d.windows.len())
        .filter(|&i| r.windows[i].header.rect.h > d.windows[i].header.rect.h)
        .collect();
    assert!(!grown.is_empty(), "no window grew");
    // Grown in place, a window would cover a neighbour (what the refresh did before).
    let covers = grown.iter().any(|&g| {
        let at = cv_contracts::Rect {
            h: r.windows[g].header.rect.h,
            ..d.windows[g].header.rect
        };
        d.windows
            .iter()
            .enumerate()
            .any(|(j, w)| j != g && w.header.rect.intersects(&at))
    });
    assert!(covers, "the edit does not make a window overlap");
    let bad = overlaps(&r);
    assert!(bad.is_empty(), "overlapping windows after refresh: {bad:?}");
    // Only grown windows moved.
    for (i, w) in d.windows.iter().enumerate() {
        if !grown.contains(&i) {
            let (a, b) = (&w.header.rect, &r.windows[i].header.rect);
            assert_eq!(
                (a.x, a.y),
                (b.x, b.y),
                "{} {:?} moved",
                w.header.path,
                w.header.lines
            );
        }
    }
    cv_server::layout_input::forget_placement(&key);
}

/// §4.10.6: when a grown window has no room in its cluster, the frame grows to hold it, but not
/// into a neighbouring cluster's frame. A neighbour is put exactly where the frame grew without
/// the check; with it the window goes elsewhere, the frame keeps clear of the neighbour and
/// nothing overlaps.
#[test]
fn a_grown_frame_keeps_clear_of_neighbouring_clusters() {
    if !have_jj() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let path = linear_fixture(&tmp.path().join("repos"));
    let repo = cv_vcs::JjRepo::open(&path).unwrap();
    let key = repo.root().display().to_string();
    cv_server::layout_input::forget_placement(&key);
    let d = view_at(&repo, "description(substring:\"D: tune square\")");
    cv_server::layout_input::forget_placement(&key);
    let cluster_of = |v: &View, i: usize| {
        v.bootstrap
            .base
            .clusters
            .iter()
            .position(|c| c.id == v.windows[i].header.cluster)
            .unwrap()
    };
    // A window grows to (almost) its cluster's inner height: it overlaps a sibling, so it must
    // move, and when its cluster has no room left the frame grows to hold it. Take the first
    // window for which that happens.
    let (pad, title) = (cv_layout::CLUSTER_PAD, cv_layout::CLUSTER_TITLE);
    let found = (0..d.windows.len()).find_map(|g| {
        let ci = cluster_of(&d, g);
        let frame = d.bootstrap.base.clusters[ci].rect;
        let h = frame.h - title - pad - 1.0;
        if h <= d.windows[g].header.rect.h {
            return None;
        }
        let mut r0 = d.clone();
        r0.windows[g].header.rect.h = h;
        if cv_server::review_api::resolve_overlaps(&mut r0, &d) != 1 {
            return None;
        }
        let g0 = r0.bootstrap.base.clusters[ci].rect;
        (g0 != frame).then_some((g, ci, frame, h, g0))
    });
    let (g, ci, frame, h, g0) = found.expect("a window whose frame grows");
    let grow = |v: &mut View| {
        v.windows[g].header.rect.h = h;
    };
    let below = g0.y + g0.h > frame.y + frame.h + 1.0;
    let right = g0.x + g0.w > frame.x + frame.w + 1.0;
    assert!(below || right, "the frame did not grow: {frame:?} → {g0:?}");
    // A neighbour cluster right where it grew (outside the old frame, so it did not overlap it).
    let gap = 1.0;
    let nb = if below {
        cv_contracts::Rect {
            x: g0.x,
            y: frame.y + frame.h + gap,
            w: g0.w,
            h: g0.y + g0.h - (frame.y + frame.h),
        }
    } else {
        cv_contracts::Rect {
            x: frame.x + frame.w + gap,
            y: g0.y,
            w: g0.x + g0.w - (frame.x + frame.w),
            h: g0.h,
        }
    };
    assert!(!nb.intersects(&frame) && nb.intersects(&g0));
    let mut base = d.clone();
    let mut fake = base.bootstrap.base.clusters[ci].clone();
    fake.id = "c_neighbour".into();
    fake.rect = nb;
    base.bootstrap.base.clusters.push(fake);
    let mut r = base.clone();
    grow(&mut r);
    assert_eq!(cv_server::review_api::resolve_overlaps(&mut r, &base), 1);
    let g1 = r.bootstrap.base.clusters[ci].rect;
    assert!(
        !g1.intersects(&nb),
        "the frame grew into its neighbour: {g1:?} vs {nb:?}"
    );
    let w = r.windows[g].header.rect;
    assert!(w.x >= g1.x && w.y >= g1.y && w.x + w.w <= g1.x + g1.w && w.y + w.h <= g1.y + g1.h);
    let bad = overlaps(&r);
    assert!(bad.is_empty(), "overlapping windows: {bad:?}");
    for (i, o) in d.windows.iter().enumerate() {
        if i != g {
            assert_eq!(
                r.windows[i].header.rect, o.header.rect,
                "only the grown window moves"
            );
        }
    }
}
