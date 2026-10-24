//! DESIGN §4.1 incremental updates: after an operation that deletes one file and adds two, the
//! refreshed snapshot view drops the deleted file's windows and links and gains windows for the
//! new files (placed clear of every other window, near their directory), with their outgoing
//! links, without a full index. Skipped when `jj` is not on PATH.

use std::path::Path;
use std::process::Command;

use cv_server::index::{self, Options};
use serde_json::json;

const USER: &str = "[user]\nname = \"Test\"\nemail = \"test@example.com\"\n";

fn jj(dir: &Path, args: &[&str]) {
    let out = Command::new("jj")
        .args(args)
        .current_dir(dir)
        .env("JJ_CONFIG", dir.join("../jjconfig.toml"))
        .output()
        .expect("run jj");
    assert!(
        out.status.success(),
        "jj {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

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

fn write(repo: &Path, path: &str, text: &str) {
    let full = repo.join(path);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, text).unwrap();
}

#[test]
fn new_and_deleted_files_update_the_view() {
    if !have_jj() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), USER).unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    jj(&repo, &["git", "init"]);
    write(&repo, "src/a.py", "def f0(x):\n    return x\n");
    write(
        &repo,
        "src/b.py",
        "from a import f0\n\n\ndef main():\n    return f0(1)\n",
    );
    write(
        &repo,
        "src/gone.py",
        "from a import f0\n\n\ndef g():\n    return f0(2)\n",
    );
    jj(&repo, &["commit", "-m", "base"]);

    let reg = index::registry();
    let r0 = cv_vcs::JjRepo::open(&repo).unwrap();
    let c0 = r0.resolve_one("@-").unwrap();
    let spec = json!({ "mode": "snapshot", "rev": "@-" });
    let mut v0 = index::build_view(&r0, &reg, spec, &c0, Options { max_commits: 50 }).unwrap();
    // Another lane reaches far to the right of the code lane (as a hotter column used only by the
    // tests would): a new code cluster still goes to the code lane's right edge, not the world's.
    let world0 = v0.bootstrap.base.world;
    v0.bootstrap.base.world.w += 50_000.0;
    let gone_ids: Vec<String> = v0.files[v0.by_path["src/gone.py"]]
        .windows
        .iter()
        .map(|&w| v0.windows[w].header.id.clone())
        .collect();
    assert!(
        v0.links
            .iter()
            .any(|l| v0.windows[l.from].header.path == "src/gone.py")
    );

    std::fs::remove_file(repo.join("src/gone.py")).unwrap();
    write(
        &repo,
        "src/c.py",
        "from a import f0\n\n\ndef h():\n    return f0(3)\n",
    );
    write(&repo, "lib/new/d.py", "def d():\n    return 4\n");
    // Files in directories of their own: they join the lane's "new files" cluster too.
    let body: String = (0..30)
        .map(|i| format!("def f{i}():\n    return {i}\n\n\n"))
        .collect();
    for i in 0..8 {
        write(&repo, &format!("n{i}/m.py"), &body);
    }
    jj(&repo, &["commit", "-m", "add and delete"]);
    let r1 = cv_vcs::JjRepo::open(&repo).unwrap();
    let c1 = r1.resolve_one("@-").unwrap();
    let (v1, changed) =
        cv_server::review_api::refresh_view(&v0, &reg, &r1, &c1, &r1.op_id()).unwrap();

    // The deleted file is gone, with its windows and links; its text stays readable by blob.
    assert!(!v1.by_path.contains_key("src/gone.py"));
    for id in &gone_ids {
        assert!(changed.contains(id) && !v1.by_id.contains_key(id));
    }
    let gone_blob = &v0.files[v0.by_path["src/gone.py"]].blob;
    assert!(v1.retired.contains_key(gone_blob));
    assert!(
        v1.links
            .iter()
            .all(|l| l.from < v1.windows.len() && l.to < v1.windows.len())
    );
    // New files have windows, listed as changed, clear of every other window.
    for path in ["src/c.py", "lib/new/d.py"] {
        let fi = v1.by_path[path];
        assert_eq!(v1.files[fi].windows.len(), 1, "{path}");
        let w = v1.files[fi].windows[0];
        let h = &v1.windows[w].header;
        assert!(changed.contains(&h.id), "{path}");
        assert_eq!(
            (h.path.as_str(), h.lane.as_str(), h.rev.as_str()),
            (path, "code", c1.commit.as_str())
        );
        for (i, o) in v1.windows.iter().enumerate() {
            assert!(
                i == w || !o.header.rect.intersects(&h.rect),
                "{path} overlaps {}",
                o.header.path
            );
        }
        assert!(v1.bootstrap.base.clusters.iter().any(|c| c.id == h.cluster));
        assert_eq!(v1.by_id[&h.id], w);
        // The tile index serves it.
        let cell = |x: f64| (x / cv_contracts::TILE_BASE_WU).floor() as i64;
        assert!(v1.tiles[&(cell(h.rect.x), cell(h.rect.y))].0.contains(&w));
    }
    // src/c.py sits with its directory's cluster (src/), lib/new/d.py in a new one.
    let cluster_of = |p: &str| {
        v1.windows[v1.files[v1.by_path[p]].windows[0]]
            .header
            .cluster
            .clone()
    };
    assert_eq!(cluster_of("src/c.py"), cluster_of("src/a.py"));
    let rect_of = |p: &str| v1.windows[v1.files[v1.by_path[p]].windows[0]].header.rect;
    let cluster0 = |id: &str| {
        v0.bootstrap
            .base
            .clusters
            .iter()
            .find(|c| c.id == id)
            .unwrap()
    };
    let cluster1 = |id: &str| {
        v1.bootstrap
            .base
            .clusters
            .iter()
            .find(|c| c.id == id)
            .unwrap()
    };
    // Where: c.py in free space inside its cluster's old frame (it has a hole that fits)...
    let (c, src0) = (rect_of("src/c.py"), cluster0(&cluster_of("src/a.py")).rect);
    assert!(
        c.x >= src0.x + cv_layout::CLUSTER_PAD
            && c.y >= src0.y + cv_layout::CLUSTER_TITLE
            && c.x + c.w <= src0.x + src0.w - cv_layout::CLUSTER_PAD
            && c.y + c.h <= src0.y + src0.h - cv_layout::CLUSTER_PAD,
        "{c:?} in {src0:?}"
    );
    // ...d.py in a "new files" cluster at the code lane's right edge (right of every code cluster
    // of v0, by the same margin), level with the top of the code band, nowhere near the world's.
    let d = rect_of("lib/new/d.py");
    let new_c = cluster1(&cluster_of("lib/new/d.py"));
    assert_eq!(
        (new_c.label.as_str(), new_c.lane.as_str()),
        ("new files", "code")
    );
    assert!(v0.bootstrap.base.clusters.iter().all(|c| c.id != new_c.id));
    let code_right = v0
        .bootstrap
        .base
        .clusters
        .iter()
        .filter(|c| c.lane == "code")
        .map(|c| c.rect.x + c.rect.w)
        .fold(f64::NEG_INFINITY, f64::max);
    assert_eq!(d.x, code_right + 8.0 * 120.0, "{d:?}");
    assert!(d.x + d.w < world0.x + world0.w + 50_000.0 - 1_000.0);
    let band0 = v0
        .bootstrap
        .base
        .lanes
        .iter()
        .find(|b| b.lane == "code")
        .unwrap()
        .rect;
    assert_eq!(d.y, band0.y + 4.0 * 120.0);
    // Every surviving window kept its rect exactly.
    for w in &v0.windows {
        if w.header.path == "src/gone.py" {
            continue;
        }
        assert_eq!(
            v1.windows[v1.by_id[&w.header.id]].header.rect, w.header.rect,
            "{} moved",
            w.header.path
        );
    }
    // The clusters, the lane band and the world grew to hold the new windows.
    let band1 = v1
        .bootstrap
        .base
        .lanes
        .iter()
        .find(|b| b.lane == "code")
        .unwrap()
        .rect;
    let world1 = v1.bootstrap.base.world;
    let inside = |r: &cv_contracts::Rect, o: &cv_contracts::Rect| {
        r.x >= o.x && r.y >= o.y && r.x + r.w <= o.x + o.w && r.y + r.h <= o.y + o.h
    };
    for (p, r) in [("src/c.py", c), ("lib/new/d.py", d)] {
        assert!(inside(&r, &cluster1(&cluster_of(p)).rect), "{p}: {r:?}");
        assert!(inside(&r, &band1), "{p}: {r:?} outside {band1:?}");
        assert!(inside(&r, &world1), "{p}");
    }
    // The "new files" cluster stays inside the code band of v0: a full column continues in a new
    // column to its right (here the band holds fewer than all of them), clear of every window.
    let (top, bottom) = (band0.y + 4.0 * 120.0, band0.y + band0.h - 4.0 * 120.0);
    let mut xs = std::collections::BTreeSet::new();
    for i in 0..8 {
        let p = format!("n{i}/m.py");
        let fi = v1.by_path[&p];
        for &w in &v1.files[fi].windows {
            let r = v1.windows[w].header.rect;
            assert_eq!(v1.windows[w].header.cluster, new_c.id, "{p}");
            assert!(
                r.y == top || r.y + r.h <= bottom,
                "{p}: {r:?} runs out of {band0:?}"
            );
            assert!(r.x >= d.x, "{p}: {r:?}");
            for (j, o) in v1.windows.iter().enumerate() {
                assert!(
                    j == w || !o.header.rect.intersects(&r),
                    "{p} overlaps {}",
                    o.header.path
                );
            }
            xs.insert(r.x as i64);
        }
    }
    assert!(
        xs.len() >= 2,
        "one column of {} px in a band of {}: {xs:?}",
        8 * 500,
        band0.h
    );
    // Outgoing links of the new file resolve through its import.
    let c_win = v1.files[v1.by_path["src/c.py"]].windows[0];
    let a_win = v1.files[v1.by_path["src/a.py"]].windows[0];
    let l = v1
        .links
        .iter()
        .find(|l| l.from == c_win)
        .expect("a link out of src/c.py");
    assert_eq!((l.to, l.kind, l.from_line), (a_win, "call", 5));
    assert!(
        v1.windows[c_win]
            .out_links
            .contains(&v1.links.iter().position(|x| std::ptr::eq(x, l)).unwrap())
    );
    assert_eq!(
        v1.bootstrap.base.view.stats.windows as usize,
        v1.windows.len()
    );
    assert_eq!(v1.bootstrap.base.view.stats.files as usize, v1.files.len());
}

/// A new file whose directory's cluster is boxed in by neighbouring clusters (below and to the
/// right) does not walk past them to the first free hole: that stretched the cluster's frame into
/// a tall box over its neighbours, its windows among theirs. It goes to the lane's "new files"
/// cluster instead, and the sibling's frame stays clear of its neighbours.
#[test]
fn new_file_does_not_stretch_a_boxed_in_cluster() {
    if !have_jj() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), USER).unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    jj(&repo, &["git", "init"]);
    write(&repo, "src/a.py", "def f0(x):\n    return x\n");
    write(
        &repo,
        "src/b.py",
        "from a import f0\n\n\ndef main():\n    return f0(1)\n",
    );
    jj(&repo, &["commit", "-m", "base"]);
    let reg = index::registry();
    let r0 = cv_vcs::JjRepo::open(&repo).unwrap();
    let c0 = r0.resolve_one("@-").unwrap();
    let spec = json!({ "mode": "snapshot", "rev": "@-" });
    let mut v0 = index::build_view(&r0, &reg, spec, &c0, Options { max_commits: 50 }).unwrap();
    let src_id = v0.windows[v0.files[v0.by_path["src/a.py"]].windows[0]]
        .header
        .cluster
        .clone();
    let src = v0
        .bootstrap
        .base
        .clusters
        .iter()
        .find(|c| c.id == src_id)
        .unwrap()
        .clone();
    // Neighbours one cell gap below and to the right, each far bigger than the new file.
    let below = cv_contracts::Rect {
        x: src.rect.x - 5_000.0,
        y: src.rect.y + src.rect.h + 160.0,
        w: src.rect.w + 20_000.0,
        h: 20_000.0,
    };
    let right = cv_contracts::Rect {
        x: src.rect.x + src.rect.w + 160.0,
        y: src.rect.y - 5_000.0,
        w: 20_000.0,
        h: src.rect.h + 5_000.0,
    };
    for (i, rect) in [below, right].into_iter().enumerate() {
        let mut c = src.clone();
        c.id = format!("c_wall{i}");
        c.rect = rect;
        v0.bootstrap.base.clusters.push(c);
    }

    // Functions too tall for the free space left inside the sibling's frame.
    let body: String = (0..6)
        .map(|i| {
            let lines: String = (0..15).map(|j| format!("    x = x + {j}\n")).collect();
            format!("def g{i}(x):\n{lines}    return x + {i}\n\n\n")
        })
        .collect();
    write(&repo, "src/big.py", &body);
    jj(&repo, &["commit", "-m", "add"]);
    let r1 = cv_vcs::JjRepo::open(&repo).unwrap();
    let c1 = r1.resolve_one("@-").unwrap();
    let (v1, _) = cv_server::review_api::refresh_view(&v0, &reg, &r1, &c1, &r1.op_id()).unwrap();

    let frame = |id: &str| {
        v1.bootstrap
            .base
            .clusters
            .iter()
            .find(|c| c.id == id)
            .unwrap()
            .rect
    };
    let src1 = frame(&src_id);
    for wall in [below, right] {
        assert!(!src1.intersects(&wall), "{src1:?} runs into {wall:?}");
    }
    let fi = v1.by_path["src/big.py"];
    for &w in &v1.files[fi].windows {
        let h = &v1.windows[w].header;
        for wall in [below, right] {
            assert!(!h.rect.intersects(&wall), "{:?} inside {wall:?}", h.rect);
        }
        let label = &v1
            .bootstrap
            .base
            .clusters
            .iter()
            .find(|c| c.id == h.cluster)
            .unwrap()
            .label;
        assert_eq!(label, "new files", "{:?}", h.rect);
    }
}

/// §4.1 complete incremental updates: an op adds a file that existing code already imports and
/// calls, plus a new bus subscriber. Without a full index, the refreshed view links the callers
/// into the new file (re-resolved refs), the new file's window has dependents (connectedness),
/// and after the background settle the subscriber's channel link exists and the new files have
/// heat from the history walk.
#[test]
fn links_into_new_files_connectedness_channels_and_heat() {
    if !have_jj() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), USER).unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    jj(&repo, &["git", "init"]);
    // src/a.py imports and calls helpers.util before src/helpers.py exists.
    write(
        &repo,
        "src/a.py",
        "from helpers import util\n\n\ndef f():\n    return util(1)\n",
    );
    write(&repo, "src/events.py", "MOVED = \"world.entity.moved\"\n");
    write(
        &repo,
        "src/presence.py",
        "from events import MOVED\n\n\nclass Presence:\n    def moved(self):\n        self._bus.emit(EventEnvelope(topic=MOVED, payload=1))\n",
    );
    jj(&repo, &["commit", "-m", "base"]);

    let reg = index::registry();
    let r0 = cv_vcs::JjRepo::open(&repo).unwrap();
    let c0 = r0.resolve_one("@-").unwrap();
    let spec = json!({ "mode": "snapshot", "rev": "@-" });
    let v0 = index::build_view(&r0, &reg, spec, &c0, Options { max_commits: 50 }).unwrap();
    let a_win = v0.files[v0.by_path["src/a.py"]].windows[0];
    let a_id = v0.windows[a_win].header.id.clone();
    assert!(v0.windows[a_win].out_links.is_empty(), "util is unresolved");

    write(&repo, "src/helpers.py", "def util(x):\n    return x\n");
    write(
        &repo,
        "src/listener.py",
        "def start(bus):\n    bus.on(\"world.entity.moved\", on_moved)\n\n\ndef on_moved(envelope):\n    return envelope\n",
    );
    jj(&repo, &["commit", "-m", "add helpers and a listener"]);
    let r1 = cv_vcs::JjRepo::open(&repo).unwrap();
    let c1 = r1.resolve_one("@-").unwrap();
    let (v1, changed) =
        cv_server::review_api::refresh_view(&v0, &reg, &r1, &c1, &r1.op_id()).unwrap();

    // The caller in the unchanged file src/a.py now links into the new file, and is reported.
    let h_win = v1.files[v1.by_path["src/helpers.py"]].windows[0];
    let a_win = v1.by_id[&a_id];
    let l = v1
        .links
        .iter()
        .find(|l| l.from == a_win && l.to == h_win)
        .expect("a link from src/a.py into the new src/helpers.py");
    assert_eq!((l.kind, l.from_line), ("call", 5));
    assert!(
        v1.windows[h_win]
            .in_links
            .iter()
            .any(|&i| v1.links[i].from == a_win)
    );
    assert!(changed.contains(&a_id), "the caller's window is refetched");
    // Connectedness follows the new graph: helpers.util has a dependent.
    let conn = &v1.windows[h_win].header.conn;
    assert!(conn.dependents2 >= 1 && conn.fan_in >= 1, "{conn:?}");
    assert!(v1.windows[h_win].header.conn.grade > 0);
    // Link ids stay unique.
    let ids: std::collections::HashSet<&str> = v1.links.iter().map(|l| l.id.as_str()).collect();
    assert_eq!(ids.len(), v1.links.len());

    // The background half: channel links over the whole view, connectedness with them, heat.
    let s = cv_server::incremental::settle(&v1, &reg, &r1, 50).unwrap();
    let v2 = s.view;
    let lw: Vec<usize> = v2.files[v2.by_path["src/listener.py"]].windows.clone();
    let pw: Vec<usize> = v2.files[v2.by_path["src/presence.py"]].windows.clone();
    let bus = v2
        .links
        .iter()
        .find(|l| {
            l.channel.is_some()
                && (lw.contains(&l.from) || lw.contains(&l.to))
                && (pw.contains(&l.from) || pw.contains(&l.to))
        })
        .expect("a bus link between the new listener and the publisher");
    assert!(
        s.windows.contains(&v2.windows[bus.from].header.id),
        "settle reports the windows it linked"
    );
    // Heat: the new files were touched by one commit of the history the walk saw.
    for path in ["src/helpers.py", "src/listener.py"] {
        let h = &v2.windows[v2.files[v2.by_path[path]].windows[0]]
            .header
            .heat;
        assert!(h.touches >= 1 && h.rate > 0.0, "{path}: {h:?}");
    }
    assert!(v2.bootstrap.churn.scanned_commits >= 2);
    // Code links survived the channel pass with their ids.
    assert!(v2.links.iter().any(|x| x.id == l.id && x.from == a_win));
}

/// (from window id, to window id, kind, from line) → link id, over a view's links.
fn link_ids(
    v: &cv_server::index::View,
) -> std::collections::HashMap<(String, String, String, u32), String> {
    v.links
        .iter()
        .map(|l| {
            (
                (
                    v.windows[l.from].header.id.clone(),
                    v.windows[l.to].header.id.clone(),
                    l.kind.to_string(),
                    l.from_line,
                ),
                l.id.clone(),
            )
        })
        .collect()
}

/// §4.1 "positions don't move" and "unchanged links keep their ids": an op that edits one file,
/// deletes another and adds a third leaves every surviving window (including the edited file's)
/// at its place, and every link whose ends and line are unchanged under its old id; new links get
/// fresh ids. The background settle keeps both too.
#[test]
fn surviving_windows_keep_places_and_unchanged_links_keep_ids() {
    if !have_jj() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), USER).unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    jj(&repo, &["git", "init"]);
    write(
        &repo,
        "src/a.py",
        "def f0(x):\n    return x\n\n\ndef f1(x):\n    return x + 1\n",
    );
    write(
        &repo,
        "src/b.py",
        "from a import f0\n\n\ndef main():\n    return f0(1)\n",
    );
    write(
        &repo,
        "src/c.py",
        "from a import f1\n\n\ndef side():\n    return f1(2)\n",
    );
    write(
        &repo,
        "src/gone.py",
        "from a import f0\n\n\ndef g():\n    return f0(3)\n",
    );
    write(
        &repo,
        "tests/test_b.py",
        "from b import main\n\n\ndef test_main():\n    assert main() == 1\n",
    );
    jj(&repo, &["commit", "-m", "base"]);

    let reg = index::registry();
    let r0 = cv_vcs::JjRepo::open(&repo).unwrap();
    let c0 = r0.resolve_one("@-").unwrap();
    let spec = json!({ "mode": "snapshot", "rev": "@-" });
    let v0 = index::build_view(&r0, &reg, spec, &c0, Options { max_commits: 50 }).unwrap();
    let ids0 = link_ids(&v0);
    assert!(ids0.len() >= 3, "{ids0:?}");

    // c.py: an edit inside side() (same lines, same call); gone.py deleted; d.py added.
    write(
        &repo,
        "src/c.py",
        "from a import f1\n\n\ndef side():\n    return f1(20)\n",
    );
    std::fs::remove_file(repo.join("src/gone.py")).unwrap();
    write(
        &repo,
        "src/d.py",
        "from a import f1\n\n\ndef more():\n    return f1(4)\n",
    );
    jj(&repo, &["commit", "-m", "edit, delete, add"]);
    let r1 = cv_vcs::JjRepo::open(&repo).unwrap();
    let c1 = r1.resolve_one("@-").unwrap();
    let (v1, _) = cv_server::review_api::refresh_view(&v0, &reg, &r1, &c1, &r1.op_id()).unwrap();
    assert_ne!(
        v0.files[v0.by_path["src/c.py"]].blob, v1.files[v1.by_path["src/c.py"]].blob,
        "c.py changed"
    );

    // Every window that survives (by id) is where it was, the edited file's included; the edited
    // file's windows survive under their ids.
    let mut kept = 0;
    for w in &v0.windows {
        let h = &w.header;
        if h.path == "src/gone.py" {
            continue;
        }
        let i = *v1
            .by_id
            .get(&h.id)
            .unwrap_or_else(|| panic!("{}#{:?} lost its window", h.path, h.lines));
        let r = &v1.windows[i].header.rect;
        assert_eq!(
            (r.x, r.y, r.w, r.h),
            (h.rect.x, h.rect.y, h.rect.w, h.rect.h),
            "{} moved",
            h.path
        );
        kept += 1;
    }
    assert_eq!(
        kept,
        v0.windows.len() - v0.files[v0.by_path["src/gone.py"]].windows.len()
    );

    // Unchanged links keep their ids; the deleted file's are gone; new ones get unused ids.
    let ids1 = link_ids(&v1);
    let mut same = 0;
    for (k, id) in &ids0 {
        if let Some(id1) = ids1.get(k) {
            assert_eq!(id1, id, "link {k:?} changed its id");
            same += 1;
        }
    }
    let b_to_a = ids0
        .keys()
        .find(|k| v0.windows[v0.by_id[&k.0]].header.path == "src/b.py")
        .expect("b.py → a.py link");
    assert!(
        ids1.contains_key(b_to_a),
        "the untouched b.py → a.py link survives"
    );
    let c_to_a = ids0
        .keys()
        .find(|k| v0.windows[v0.by_id[&k.0]].header.path == "src/c.py")
        .expect("c.py → a.py link");
    assert!(
        ids1.contains_key(c_to_a),
        "the edited file's unchanged call keeps its link"
    );
    assert!(same >= 2);
    let old_ids: std::collections::HashSet<&String> = ids0.values().collect();
    let d_win = v1.files[v1.by_path["src/d.py"]].windows[0];
    let d_link = v1
        .links
        .iter()
        .find(|l| l.from == d_win)
        .expect("a link out of d.py");
    assert!(
        !old_ids.contains(&d_link.id),
        "new link reuses an old id: {}",
        d_link.id
    );
    let all: std::collections::HashSet<&str> = v1.links.iter().map(|l| l.id.as_str()).collect();
    assert_eq!(all.len(), v1.links.len(), "link ids stay unique");

    // The background settle keeps places and code link ids too.
    let v2 = cv_server::incremental::settle(&v1, &reg, &r1, 50)
        .unwrap()
        .view;
    for w in &v1.windows {
        let r = &v2.windows[v2.by_id[&w.header.id]].header.rect;
        assert_eq!((r.x, r.y), (w.header.rect.x, w.header.rect.y));
    }
    let ids2 = link_ids(&v2);
    for (k, id) in &ids1 {
        if let Some(id2) = ids2.get(k) {
            assert_eq!(id2, id, "settle changed link {k:?}'s id");
        }
    }
    assert!(ids2.contains_key(b_to_a));
}

fn file_heat(grade: u8, settled: bool, touches: u32, rate: f64) -> cv_churn::FileHeat {
    cv_churn::FileHeat {
        grade,
        rate,
        ci: [rate * 0.5, rate * 1.5],
        settled,
        touches,
    }
}

fn churn(files: &[(&str, cv_churn::FileHeat)]) -> cv_churn::ChurnResult {
    cv_churn::ChurnResult {
        files: files
            .iter()
            .map(|(p, h)| (p.to_string(), h.clone()))
            .collect(),
        scanned: 10,
        oldest_ms: None,
        settled_mass: 0.5,
        stop: cv_churn::Stop::HistoryExhausted,
        bands: [0.01, 0.02, 0.05, 0.1],
        effective_sample: 10.0,
        renames_followed: 0,
        co_change: vec![],
        checks: vec![],
    }
}

/// §4.1 / §4.9: "a window's heat grade changes only when the new grade is settled, or when its
/// file had no change in history before" (`apply_heat`, the background half of an update).
#[test]
fn heat_grade_changes_only_once_settled() {
    if !have_jj() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), USER).unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    jj(&repo, &["git", "init"]);
    for p in [
        "src/kept.py",
        "src/taken.py",
        "src/fresh.py",
        "src/same.py",
        "src/quiet.py",
    ] {
        write(&repo, p, "def f(x):\n    return x\n");
    }
    jj(&repo, &["commit", "-m", "base"]);
    let reg = index::registry();
    let r0 = cv_vcs::JjRepo::open(&repo).unwrap();
    let c0 = r0.resolve_one("@-").unwrap();
    let spec = json!({ "mode": "snapshot", "rev": "@-" });
    let mut v = index::build_view(&r0, &reg, spec, &c0, Options { max_commits: 50 }).unwrap();
    let win = |v: &cv_server::index::View, p: &str| v.files[v.by_path[p]].windows[0];
    // Start from known heat: kept/taken/same graded 1 with history, fresh with no history.
    let set = |v: &mut cv_server::index::View, p: &str, grade: u8, touches: u32| {
        let w = win(v, p);
        let h = &mut v.windows[w].header.heat;
        h.grade = grade;
        h.rate = 0.015;
        h.ci = [0.01, 0.02];
        h.settled = true;
        h.touches = touches;
    };
    set(&mut v, "src/kept.py", 1, 3);
    set(&mut v, "src/taken.py", 1, 3);
    set(&mut v, "src/same.py", 1, 3);
    set(&mut v, "src/fresh.py", 0, 0);
    set(&mut v, "src/quiet.py", 1, 3);
    let quiet_before = v.windows[win(&v, "src/quiet.py")].header.heat.clone();

    let c = churn(&[
        // Hot now, but the interval still spans bands: the grade waits.
        ("src/kept.py", file_heat(4, false, 9, 0.2)),
        // Hot and settled: the grade moves.
        ("src/taken.py", file_heat(4, true, 9, 0.2)),
        // No change in history before: takes the new grade even unsettled.
        ("src/fresh.py", file_heat(3, false, 2, 0.06)),
        // Same grade, unsettled: the grade stays and the rest updates.
        ("src/same.py", file_heat(1, false, 4, 0.018)),
        // Unchanged numbers (quiet.py): no change reported.
        ("src/quiet.py", file_heat(1, true, 3, 0.015)),
    ]);
    // quiet.py's ci must match too, for "no change".
    let mut c = c;
    c.files.get_mut("src/quiet.py").unwrap().ci = [0.01, 0.02];
    let changed = cv_server::incremental::apply_heat(&mut v, &c);

    let heat = |v: &cv_server::index::View, p: &str| v.windows[win(v, p)].header.heat.clone();
    let kept = heat(&v, "src/kept.py");
    assert_eq!(kept.grade, 1, "an unsettled grade change waits: {kept:?}");
    assert!(!kept.settled);
    assert_eq!(
        (kept.touches, kept.rate),
        (9, 0.2),
        "rate and touches still follow"
    );
    let taken = heat(&v, "src/taken.py");
    assert_eq!((taken.grade, taken.settled), (4, true), "{taken:?}");
    let fresh = heat(&v, "src/fresh.py");
    assert_eq!((fresh.grade, fresh.settled), (3, false), "{fresh:?}");
    let same = heat(&v, "src/same.py");
    assert_eq!(
        (same.grade, same.settled, same.touches),
        (1, false, 4),
        "{same:?}"
    );
    let quiet = heat(&v, "src/quiet.py");
    assert_eq!(
        (
            quiet.grade,
            quiet.rate,
            quiet.ci,
            quiet.settled,
            quiet.touches
        ),
        (
            quiet_before.grade,
            quiet_before.rate,
            quiet_before.ci,
            quiet_before.settled,
            quiet_before.touches
        )
    );
    let expect: std::collections::HashSet<usize> =
        ["src/kept.py", "src/taken.py", "src/fresh.py", "src/same.py"]
            .iter()
            .map(|p| win(&v, p))
            .collect();
    assert_eq!(changed, expect);

    // Once kept.py's grade settles, it moves; applying the same result again changes nothing.
    let mut c2 = c;
    c2.files
        .insert("src/kept.py".into(), file_heat(4, true, 9, 0.2));
    let changed = cv_server::incremental::apply_heat(&mut v, &c2);
    assert_eq!(heat(&v, "src/kept.py").grade, 4);
    assert!(changed.contains(&win(&v, "src/kept.py")));
    assert!(cv_server::incremental::apply_heat(&mut v, &c2).is_empty());
}
