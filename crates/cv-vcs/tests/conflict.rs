//! DESIGN §4.1: a conflicted tree is listed once per path and read with jj's own conflict markers,
//! byte for byte what `jj` writes into the working copy. Skipped when `jj` is not on PATH.

use std::path::Path;
use std::process::Command;

fn jj(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("jj")
        .args(args)
        .current_dir(dir)
        .env("JJ_CONFIG", dir.join("../jjconfig.toml"))
        .env("JJ_USER", "Test")
        .env("JJ_EMAIL", "test@example.com")
        .output()
        .expect("run jj");
    assert!(
        out.status.success(),
        "jj {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
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

#[test]
fn conflicted_file_lists_once_and_reads_with_markers() {
    if !have_jj() {
        eprintln!("jj not found; skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), "").unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    jj(&repo, &["git", "init"]);
    std::fs::write(repo.join("f.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(repo.join("g.txt"), "plain\n").unwrap();
    jj(&repo, &["commit", "-m", "base"]);
    jj(&repo, &["bookmark", "create", "base", "-r", "@-"]);
    std::fs::write(repo.join("f.txt"), "one\nTWO-left\nthree\n").unwrap();
    jj(&repo, &["commit", "-m", "left"]);
    jj(&repo, &["bookmark", "create", "left", "-r", "@-"]);
    jj(&repo, &["new", "base"]);
    std::fs::write(repo.join("f.txt"), "one\nTWO-right\nthree\n").unwrap();
    jj(&repo, &["commit", "-m", "right"]);
    jj(&repo, &["bookmark", "create", "right", "-r", "@-"]);
    // A merge of both sides: f.txt is conflicted there, and jj writes it with markers.
    jj(&repo, &["new", "left", "right"]);
    let on_disk = std::fs::read(repo.join("f.txt")).unwrap();

    let r = cv_vcs::JjRepo::open(&repo).unwrap();
    let head = r.resolve_one("@").unwrap();
    let tree = r.list_tree(&head.commit).unwrap();
    let f: Vec<_> = tree.iter().filter(|e| e.path == "f.txt").collect();
    assert_eq!(f.len(), 1, "listed once: {tree:?}");
    let f = f[0];
    assert!(f.conflict);
    assert!(f.blob.starts_with(cv_vcs::CONFLICT_BLOB_PREFIX));
    let g = tree.iter().find(|e| e.path == "g.txt").unwrap();
    assert!(!g.conflict);

    let text = r.read_blob(&f.path, &f.blob).unwrap();
    let s = String::from_utf8_lossy(&text);
    assert!(s.contains("<<<<<<<") && s.contains(">>>>>>>"), "{s}");
    assert!(s.contains("TWO-left") && s.contains("TWO-right"), "{s}");
    assert_eq!(
        s,
        String::from_utf8_lossy(&on_disk),
        "same bytes as the working copy jj wrote"
    );

    // Stable (same id on a second listing and from a fresh reader) and distinct per path/commit.
    let again = cv_vcs::JjRepo::open(&repo).unwrap();
    let tree2 = again.list_tree(&head.commit).unwrap();
    assert_eq!(
        tree2.iter().find(|e| e.path == "f.txt").unwrap().blob,
        f.blob
    );
    assert_eq!(again.read_blob(&f.path, &f.blob).unwrap(), text);
    assert_ne!(
        cv_vcs::conflict_blob_id(&head.commit, "f.txt"),
        cv_vcs::conflict_blob_id(&head.commit, "h.txt")
    );
    // A resolved path under a synthetic id is an error, not the wrong text.
    assert!(
        r.read_blob("g.txt", &cv_vcs::conflict_blob_id(&head.commit, "g.txt"))
            .is_err()
    );
}

/// Range diffs show a conflicted file the way snapshot views do: the side that is conflicted
/// reads as jj's materialized markers, in the user's `ui.conflict-marker-style` (here the repo
/// config sets "git"), byte for byte what `jj` writes to the working copy.
#[test]
fn range_diff_of_conflicted_file_reads_with_markers() {
    if !have_jj() {
        eprintln!("jj not found; skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), "").unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    jj(&repo, &["git", "init"]);
    std::fs::write(repo.join("f.txt"), "one\ntwo\nthree\n").unwrap();
    jj(&repo, &["commit", "-m", "base"]);
    jj(&repo, &["bookmark", "create", "base", "-r", "@-"]);
    std::fs::write(repo.join("f.txt"), "one\nTWO-left\nthree\n").unwrap();
    jj(&repo, &["commit", "-m", "left"]);
    jj(&repo, &["bookmark", "create", "left", "-r", "@-"]);
    jj(&repo, &["new", "base"]);
    std::fs::write(repo.join("f.txt"), "one\nTWO-x\nthree\n").unwrap();
    jj(&repo, &["describe", "-m", "x"]);
    jj(&repo, &["bookmark", "create", "x", "-r", "@"]);
    // Rebased onto left, x conflicts in f.txt; checking it out writes the markers.
    std::fs::write(
        repo.join(".jj/repo/config.toml"),
        "[ui]\nconflict-marker-style = \"git\"\n",
    )
    .unwrap();
    jj(&repo, &["rebase", "-r", "x", "-d", "left"]);
    jj(&repo, &["edit", "x"]);
    let on_disk = std::fs::read(repo.join("f.txt")).unwrap();

    let r = cv_vcs::JjRepo::open(&repo).unwrap();
    assert_eq!(
        r.marker_style(),
        jj_lib::conflicts::ConflictMarkerStyle::Git
    );
    let x = r.resolve_one("x").unwrap();
    let diff = r.range_diff(&x.commit, &x.commit).unwrap();
    let f = diff
        .iter()
        .find(|c| c.path == "f.txt")
        .expect("f.txt changed");
    let after = f.after.as_deref().unwrap();
    assert!(after.starts_with(cv_vcs::CONFLICT_BLOB_PREFIX), "{after}");
    // The base side (left) is resolved: a plain blob.
    let before = f.before.as_deref().unwrap();
    assert!(!before.starts_with(cv_vcs::CONFLICT_BLOB_PREFIX));
    assert_eq!(
        r.read_blob("f.txt", before).unwrap(),
        b"one\nTWO-left\nthree\n"
    );
    let text = r.read_blob("f.txt", after).unwrap();
    let s = String::from_utf8_lossy(&text);
    assert!(
        s.contains("<<<<<<<") && s.contains("|||||||"),
        "git style: {s}"
    );
    assert_eq!(s, String::from_utf8_lossy(&on_disk), "as jj wrote it");
    // The snapshot listing of x reads the same text under the same id.
    let tree = r.list_tree(&x.commit).unwrap();
    assert_eq!(tree.iter().find(|e| e.path == "f.txt").unwrap().blob, after);

    // A child of x: its base side is x's conflicted tree, named `<x>^`, also read with markers.
    jj(&repo, &["new", "x"]);
    std::fs::write(repo.join("f.txt"), "one\nresolved\nthree\n").unwrap();
    jj(&repo, &["describe", "-m", "fix"]);
    let r = cv_vcs::JjRepo::open(&repo).unwrap();
    let fix = r.resolve_one("@").unwrap();
    let diff = r.commit_diff(&fix.commit).unwrap();
    let f = diff.iter().find(|c| c.path == "f.txt").unwrap();
    let base = f.before.as_deref().unwrap();
    assert!(base.starts_with(cv_vcs::CONFLICT_BLOB_PREFIX), "{base}");
    assert_eq!(r.read_blob("f.txt", base).unwrap(), text);
}
