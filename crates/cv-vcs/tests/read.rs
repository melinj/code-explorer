//! Read side on a scripted jj repo: history graph, range analysis, diffs, in-range blame and
//! workspace state, all without writing an operation. Skipped when `jj` is not on PATH.

use std::path::Path;
use std::process::Command;

use cv_vcs::JjRepo;
use cv_vcs::read::{self, RevsetEnv};

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

fn op_head(repo: &Path) -> String {
    jj(
        repo,
        &[
            "op",
            "log",
            "-n1",
            "--no-graph",
            "--ignore-working-copy",
            "-T",
            "id",
        ],
    )
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

fn env_with_trunk(r: &JjRepo) -> RevsetEnv {
    let mut env = r.revset_env();
    env.aliases.insert("trunk()", "main", None).unwrap();
    env
}

/// main: base ← t1 (bookmark main). Stack on main: s1 ← s2 ← s3 (@). Side branch from base: x1.
fn fixture() -> Option<tempfile::TempDir> {
    if !have_jj() {
        eprintln!("jj not found; skipping");
        return None;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), "").unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    jj(&repo, &["git", "init"]);
    std::fs::write(
        repo.join("a.py"),
        "def a():\n    return 1\n\n\ndef b():\n    return 2\n",
    )
    .unwrap();
    std::fs::write(repo.join(".gitignore"), "*.log\n").unwrap();
    jj(&repo, &["commit", "-m", "base"]);
    std::fs::write(repo.join("c.py"), "C = 1\n").unwrap();
    jj(&repo, &["commit", "-m", "t1"]);
    jj(&repo, &["bookmark", "create", "main", "-r", "@-"]);
    std::fs::write(
        repo.join("a.py"),
        "def a():\n    return 10\n\n\ndef b():\n    return 2\n",
    )
    .unwrap();
    jj(&repo, &["commit", "-m", "s1"]);
    std::fs::write(repo.join("d.py"), "D = 1\n").unwrap();
    jj(&repo, &["commit", "-m", "s2"]);
    std::fs::write(
        repo.join("a.py"),
        "def a():\n    return 10\n\n\ndef b():\n    return 20\n    # done\n",
    )
    .unwrap();
    jj(&repo, &["describe", "-m", "s3"]);
    jj(&repo, &["new", "description(substring:base)", "-m", "x1"]);
    std::fs::write(repo.join("x.py"), "X = 1\n").unwrap();
    jj(&repo, &["edit", "description(substring:s3)"]);
    Some(tmp)
}

#[test]
fn history_graph_ranges_blame_and_workspaces() {
    let Some(tmp) = fixture() else { return };
    let repo_dir = tmp.path().join("repo");
    let _first = op_head(&repo_dir);
    let r = JjRepo::open(&repo_dir).unwrap();
    let env = env_with_trunk(&r);

    // History: all visible commits.
    let (rows, truncated) = r.graph(&env, "all()", 100).unwrap();
    assert!(!truncated);
    let desc = |i: usize| rows[i].info.description.trim().to_string();
    assert_eq!(desc(0), "s3", "the working copy's branch comes first");
    assert!(rows[0].working_copy && rows[0].workspaces == ["default"]);
    let t1 = rows
        .iter()
        .find(|r| r.info.description.trim() == "t1")
        .unwrap();
    assert_eq!(t1.bookmarks, ["main"]);
    assert!(t1.immutable);
    assert!(!rows[0].immutable);
    assert!(
        rows.iter()
            .all(|r| r.shortest_change >= 1 && r.shortest_commit >= 1)
    );
    // Elided edges: with only s3 and base shown, s3's edge to base hides s2, s1, t1.
    let (rows, _) = r
        .graph(
            &env,
            "description(substring:s3) | description(substring:base)",
            10,
        )
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].edges.len(), 1);
    assert_eq!(rows[0].edges[0].elided, 3);
    assert_eq!(rows[0].edges[0].parent, rows[1].info.commit);

    // Ranges: trunk()..@ is the stack; a selection with a hole reports the gap.
    let range = r.analyze_range(&env, "trunk()..@", 100).unwrap();
    assert_eq!(range.commits.len(), 3);
    assert!(range.gap.is_empty() && range.roots.len() == 1 && range.heads.len() == 1);
    let holey = r
        .analyze_range(
            &env,
            "description(substring:s1) | description(substring:s3)",
            100,
        )
        .unwrap();
    assert_eq!(holey.gap.len(), 1);
    assert_eq!(holey.gap[0].description.trim(), "s2");
    let multi = r
        .analyze_range(
            &env,
            "description(substring:s3) | description(substring:x1)",
            100,
        )
        .unwrap();
    assert_eq!(multi.heads.len(), 2);
    let imm = r
        .analyze_range(&env, "description(substring:t1)::@", 100)
        .unwrap();
    assert_eq!(imm.immutable.len(), 1);

    // Errors carry jj's message and a hint.
    let e = r.analyze_range(&env, "trnk()..@", 100).unwrap_err();
    assert!(e.message.contains("trnk"), "{e}");
    assert!(e.hint.as_deref().unwrap_or("").contains("trunk()"), "{e:?}");
    let e = r.analyze_range(&env, "nosuchbookmark", 100).unwrap_err();
    assert!(e.message.contains("nosuchbookmark"), "{e}");

    // Cumulative diff of the stack and in-range blame.
    let root = &range.roots[0].commit;
    let head = &range.heads[0].commit;
    let diff = r.range_diff(root, head).unwrap();
    let paths: Vec<&str> = diff.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["a.py", "d.py"]);
    let members: Vec<String> = range.commits.iter().map(|c| c.commit.clone()).collect();
    let owners = r.annotate_in_range(head, "a.py", &members).unwrap();
    let change = |d: &str| {
        range
            .commits
            .iter()
            .find(|c| c.description.trim() == d)
            .unwrap()
            .change
            .clone()
    };
    assert_eq!(owners.len(), 7);
    assert_eq!(owners[0], None, "def a(): predates the range");
    assert_eq!(owners[1].as_deref(), Some(change("s1").as_str()));
    assert_eq!(owners[5].as_deref(), Some(change("s3").as_str()));
    assert_eq!(owners[6].as_deref(), Some(change("s3").as_str()));
    let old = r
        .read_blob("a.py", diff[0].before.as_ref().unwrap())
        .unwrap();
    let new = r
        .read_blob("a.py", diff[0].after.as_ref().unwrap())
        .unwrap();
    assert_eq!(read::line_stat(&old, &new), (3, 2));

    // Workspaces: a second one with unsnapshotted edits (one modified, one new, one ignored).
    jj(
        &repo_dir,
        &[
            "workspace",
            "add",
            "../ws2",
            "-r",
            "description(substring:t1)",
        ],
    );
    let ws2 = tmp.path().join("ws2");
    std::fs::write(ws2.join("c.py"), "C = 2\n").unwrap();
    std::fs::write(ws2.join("new.py"), "N = 1\n").unwrap();
    std::fs::write(ws2.join("debug.log"), "noise\n").unwrap();
    std::fs::write(repo_dir.join("late.py"), "L = 1\n").unwrap();
    let before = op_head(&repo_dir);
    let r = JjRepo::open(&repo_dir).unwrap();
    let ws = r.workspaces(&env);
    let w2 = ws.iter().find(|w| w.name == "ws2").expect("ws2");
    assert_eq!(
        w2.root.as_deref().map(|p| p.canonicalize().unwrap()),
        Some(ws2.canonicalize().unwrap())
    );
    assert_eq!(w2.unsnapshotted, Some(2));
    let def = ws.iter().find(|w| w.name == "default").unwrap();
    assert_eq!(
        def.unsnapshotted,
        Some(1),
        "late.py is new on disk, not snapshotted"
    );
    let ops = r.last_ops(100);
    assert!(ops.contains_key("default"));
    let stack = r
        .eval(
            &env,
            &format!("trunk()..{}", read::workspace_wc_revset("ws2")),
            10,
        )
        .unwrap();
    assert_eq!(stack.len(), 1);
    assert_eq!(op_head(&repo_dir), before, "reading wrote an operation");
    let _ = r.reload().unwrap();
    assert_eq!(op_head(&repo_dir), before);
}

#[test]
fn line_runs_respect_whitespace_option() {
    let old = b"a\nb\n  c\nd\n";
    let new = b"a\nB\nc\nd\ne\n";
    let runs = read::line_runs(old, new, false);
    let changed: usize = runs.iter().filter(|r| !r.same).map(|r| r.new.len()).sum();
    assert_eq!(changed, 3);
    let runs = read::line_runs(old, new, true);
    let changed: usize = runs.iter().filter(|r| !r.same).map(|r| r.new.len()).sum();
    assert_eq!(changed, 2, "  c vs c is the same when ignoring whitespace");
}
