//! Builds a tiny jj repo with the jj CLI (0.45.1) in a temp dir and reads it back through jj-lib.
//! Skipped when `jj` is not on PATH.

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
fn reads_tree_blobs_and_history_without_writing_an_operation() {
    if !have_jj() {
        eprintln!("jj not found; skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), "").unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    jj(&repo, &["git", "init"]);
    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/a.py"), "def a():\n    return 1\n").unwrap();
    std::fs::write(repo.join("README.md"), "# hi\n").unwrap();
    jj(&repo, &["commit", "-m", "first"]);
    std::fs::write(repo.join("src/a.py"), "def a():\n    return 2\n").unwrap();
    std::fs::write(repo.join("src/b.py"), "import a\n").unwrap();
    jj(&repo, &["commit", "-m", "second"]);
    std::fs::write(repo.join("src/b.py"), "import a\na.a()\n").unwrap();
    jj(&repo, &["describe", "-m", "third (working copy)"]);
    let op_before = jj(
        &repo,
        &[
            "op",
            "log",
            "-n1",
            "--no-graph",
            "--ignore-working-copy",
            "-T",
            "id",
        ],
    );

    let r = cv_vcs::JjRepo::open(&repo.join("src")).expect("open from a subdirectory");
    assert_eq!(r.workspace_name(), "default");
    assert!(op_before.starts_with(&r.op_id()[..12]));

    let head = r.resolve_one("@").unwrap();
    assert_eq!(head.description.trim(), "third (working copy)");
    let parent = r.resolve_one("@-").unwrap();
    assert_eq!(head.parents, vec![parent.commit.clone()]);
    assert!(
        r.resolve_one("all()").is_err(),
        "several commits is an error for resolve_one"
    );
    assert!(r.resolve("nonsense(").is_err());

    let tree = r.list_tree(&head.commit).unwrap();
    let paths: Vec<&str> = tree.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, vec!["README.md", "src/a.py", "src/b.py"]);
    let b = tree.iter().find(|e| e.path == "src/b.py").unwrap();
    assert_eq!(r.read_blob(&b.path, &b.blob).unwrap(), b"import a\na.a()\n");
    // Blob ids are git blob ids.
    let git_id = Command::new("git")
        .args(["hash-object", "--stdin"])
        .current_dir(&repo)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            c.stdin.take().unwrap().write_all(b"import a\na.a()\n")?;
            c.wait_with_output()
        });
    if let Ok(out) = git_id {
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), b.blob);
    }

    let hist: Vec<cv_vcs::HistoryCommit> = r
        .history(&head.commit)
        .unwrap()
        .map(|c| c.unwrap())
        .collect();
    let files: Vec<Vec<&str>> = hist
        .iter()
        .map(|c| c.files.iter().map(String::as_str).collect())
        .collect();
    assert_eq!(
        files,
        vec![
            vec!["src/b.py"],
            vec!["src/a.py", "src/b.py"],
            vec!["README.md", "src/a.py"]
        ]
    );
    assert_eq!(hist[0].commit, head.commit);
    // Lazy: nothing is walked before the first next(); taking one commit discovers only its
    // parent (queued, not read), never the commit below that.
    let mut lazy = r.history(&head.commit).unwrap();
    assert_eq!(lazy.discovered(), 1);
    assert_eq!(lazy.next().unwrap().unwrap().commit, head.commit);
    assert_eq!(lazy.discovered(), 2);
    assert_eq!(lazy.next().unwrap().unwrap().commit, hist[1].commit);
    assert_eq!(lazy.discovered(), 3);
    // The full walk discovers each of the three commits once (the root is never queued).
    let mut full = r.history(&head.commit).unwrap();
    assert_eq!(full.by_ref().count(), 3);
    assert_eq!(full.discovered(), 3);

    let op_after = jj(
        &repo,
        &[
            "op",
            "log",
            "-n1",
            "--no-graph",
            "--ignore-working-copy",
            "-T",
            "id",
        ],
    );
    assert_eq!(op_before, op_after, "reads must never create an operation");
}
