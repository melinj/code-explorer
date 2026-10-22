//! B1 write acceptance: previews never write, every mutation plan produces the same trees as the
//! jj CLI recipe (DESIGN appendix A, the oracle), immutable targets only branch, unsnapshotted
//! edits in another workspace survive an ancestor amend, revert restores trees, and the jj CLI
//! fallback (N11) produces the same trees as the jj-lib path.
//! Repos are built with the jj CLI (0.45.1) in temp dirs; skipped when `jj` is not on PATH.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use cv_vcs::write::{EditMode, FileWrite, JjWriter, MutationPlan};

/// Shared by the jj CLI (JJ_CONFIG) and the writer. A fixed committer timestamp makes commit ids,
/// and so the change/commit labels in conflict markers, independent of the wall clock: git
/// timestamps have one-second resolution, so a writer amend and its CLI oracle straddling a second
/// boundary used to produce different ids (and different marker text).
const USER: &str = "[user]\nname = \"Test\"\nemail = \"test@example.com\"\n\
                    [debug]\ncommit-timestamp = \"2001-02-03T04:05:06+07:00\"\n";

struct Fixture {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let tool = dir.join("cv-write.sh");
        // A diffedit tool that copies $CV_CONTENT into $right/$CV_PATH (the appendix A recipe).
        std::fs::write(
            &tool,
            "#!/bin/sh\nmkdir -p \"$(dirname \"$1/$CV_PATH\")\"\ncp \"$CV_CONTENT\" \"$1/$CV_PATH\"\n",
        )
        .unwrap();
        Command::new("chmod").arg("+x").arg(&tool).status().unwrap();
        // A merge tool that writes $CV_CONTENT as the resolution (`jj resolve --tool cv-resolve`).
        let resolve = dir.join("cv-resolve.sh");
        std::fs::write(&resolve, "#!/bin/sh\ncp \"$CV_CONTENT\" \"$1\"\n").unwrap();
        Command::new("chmod")
            .arg("+x")
            .arg(&resolve)
            .status()
            .unwrap();
        std::fs::write(
            dir.join("jjconfig.toml"),
            format!(
                "{USER}[ui]\npaginate = \"never\"\n[merge-tools.cv-write]\nprogram = \"{}\"\nedit-args = [\"$right\"]\n\
                 [merge-tools.cv-resolve]\nprogram = \"{}\"\nmerge-args = [\"$output\"]\nmerge-tool-edits-conflict-markers = true\n",
                tool.display(),
                resolve.display()
            ),
        )
        .unwrap();
        Fixture { _tmp: tmp, dir }
    }

    fn jj(&self, repo: &Path, args: &[&str]) -> String {
        let out = Command::new("jj")
            .args(args)
            .current_dir(repo)
            .env("JJ_CONFIG", self.dir.join("jjconfig.toml"))
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

    /// A → B → C(@), each a described change touching f.txt and g.txt.
    fn chain(&self, name: &str) -> PathBuf {
        let repo = self.dir.join(name);
        std::fs::create_dir(&repo).unwrap();
        self.jj(&repo, &["git", "init"]);
        std::fs::write(repo.join("f.txt"), "1\n2\n3\n4\n5\n6\n7\n8\n").unwrap();
        std::fs::write(repo.join("g.txt"), "alpha\nbeta\n").unwrap();
        self.jj(&repo, &["commit", "-m", "A"]);
        std::fs::write(repo.join("f.txt"), "1\n2\n3\n4\n5\n6\n7\nB8\n").unwrap();
        self.jj(&repo, &["commit", "-m", "B"]);
        std::fs::write(repo.join("g.txt"), "alpha\nbeta\ngamma\n").unwrap();
        self.jj(&repo, &["describe", "-m", "C"]);
        repo
    }

    fn copy(&self, from: &Path, name: &str) -> PathBuf {
        let to = self.dir.join(name);
        let st = Command::new("cp")
            .arg("-a")
            .arg(from)
            .arg(&to)
            .status()
            .unwrap();
        assert!(st.success());
        to
    }

    fn change(&self, repo: &Path, desc: &str) -> String {
        self.jj(
            repo,
            &[
                "log",
                "--ignore-working-copy",
                "--no-graph",
                "-r",
                &format!("description(exact:\"{desc}\\n\")"),
                "-T",
                "change_id",
            ],
        )
    }

    fn commit_of(&self, repo: &Path, rev: &str) -> String {
        self.jj(
            repo,
            &[
                "log",
                "--ignore-working-copy",
                "--no-graph",
                "-r",
                rev,
                "-T",
                "commit_id",
            ],
        )
    }

    fn op_head(&self, repo: &Path) -> String {
        self.jj(
            repo,
            &[
                "op",
                "log",
                "--ignore-working-copy",
                "--no-graph",
                "-n1",
                "-T",
                "id",
            ],
        )
    }

    /// description → (path → content) for every visible non-root commit: compares trees across
    /// repos whose change ids differ for created changes.
    fn trees(&self, repo: &Path, paths: &[&str]) -> BTreeMap<String, BTreeMap<String, String>> {
        let list = self.jj(
            repo,
            &[
                "log",
                "--ignore-working-copy",
                "--no-graph",
                "-r",
                "all() ~ root()",
                "-T",
                "commit_id ++ \"\\t\" ++ description.first_line() ++ \"\\n\"",
            ],
        );
        let mut out = BTreeMap::new();
        for line in list.lines() {
            let (commit, desc) = line.split_once('\t').unwrap();
            if desc.is_empty() {
                continue; // working-copy scratch commits differ between the two recipes
            }
            let mut files = BTreeMap::new();
            for p in paths {
                let o = Command::new("jj")
                    .args(["file", "show", "--ignore-working-copy", "-r", commit, p])
                    .current_dir(repo)
                    .env("JJ_CONFIG", self.dir.join("jjconfig.toml"))
                    .output()
                    .unwrap();
                if o.status.success() {
                    files.insert(
                        p.to_string(),
                        String::from_utf8_lossy(&o.stdout).into_owned(),
                    );
                }
            }
            out.insert(desc.to_string(), files);
        }
        out
    }

    /// The appendix A recipe: `jj diffedit --from root() --to X --tool cv-write -- <path>`.
    fn cli_write(&self, repo: &Path, rev: &str, path: &str, content: &str) {
        self.cli_write_with(repo, rev, path, content, &[]);
    }

    /// `cli_write` with extra diffedit flags (e.g. `--restore-descendants`).
    fn cli_write_with(&self, repo: &Path, rev: &str, path: &str, content: &str, extra: &[&str]) {
        let content_file = self.dir.join("content.txt");
        std::fs::write(&content_file, content).unwrap();
        let mut args = vec![
            "diffedit", "--from", "root()", "--to", rev, "--tool", "cv-write",
        ];
        args.extend_from_slice(extra);
        args.extend(["--", path]);
        let out = Command::new("jj")
            .args(&args)
            .current_dir(repo)
            .env("JJ_CONFIG", self.dir.join("jjconfig.toml"))
            .env("CV_PATH", path)
            .env("CV_CONTENT", &content_file)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "diffedit: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The appendix A recipe: `jj resolve -r K --tool cv-resolve -- <path>`.
    fn cli_resolve(&self, repo: &Path, rev: &str, path: &str, content: &str) {
        let content_file = self.dir.join("resolution.txt");
        std::fs::write(&content_file, content).unwrap();
        let out = Command::new("jj")
            .args(["resolve", "-r", rev, "--tool", "cv-resolve", "--", path])
            .current_dir(repo)
            .env("JJ_CONFIG", self.dir.join("jjconfig.toml"))
            .env("CV_CONTENT", &content_file)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "resolve: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn writer(&self, repo: &Path, extra: &str) -> JjWriter {
        JjWriter::open_with(repo, true, Some(&format!("{USER}{extra}"))).unwrap()
    }
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

fn write(path: &str, content: &str) -> FileWrite {
    FileWrite {
        path: path.into(),
        content: Some(content.as_bytes().to_vec()),
    }
}

const PATHS: [&str; 3] = ["f.txt", "g.txt", "new/h.txt"];

#[test]
fn preview_never_writes_and_amend_matches_cli() {
    if !have_jj() {
        return;
    }
    let fx = Fixture::new();
    let repo = fx.chain("repo");
    let oracle = fx.copy(&repo, "oracle");
    let w = fx.writer(&repo, "");
    let a = fx.change(&repo, "A");
    let a_commit = fx.commit_of(&repo, &a);
    let plan = MutationPlan::WriteFiles {
        target: a_commit.clone(),
        files: vec![write("f.txt", "one\n2\n3\n4\n5\n6\n7\n8\n")],
        mode: EditMode::Amend,
        message: None,
    };

    let op0 = fx.op_head(&repo);
    let sig0 = w.op_heads_signature();
    let p = w.preview(&plan).unwrap();
    assert_eq!(fx.op_head(&repo), op0, "preview wrote an operation");
    assert_eq!(w.op_heads_signature(), sig0);
    assert!(p.op_after.is_none());
    assert_eq!(p.rewritten.len(), 3, "A and its two descendants: {p:?}");
    assert!(p.rewritten.contains(&a));
    assert!(p.new_conflicts.is_empty());
    assert_eq!(p.workspaces.len(), 1, "default@ is a descendant");

    let m = w.mutate(&plan, "cv: amend A").unwrap();
    assert_ne!(fx.op_head(&repo), op0);
    assert_eq!(
        m.rewritten, p.rewritten,
        "preview equals the applied result"
    );
    assert!(m.workspaces[0].updated);
    // The working copy on disk follows the rewritten @ (B's line 8 kept, A's line 1 amended).
    assert_eq!(
        std::fs::read_to_string(repo.join("f.txt")).unwrap(),
        "one\n2\n3\n4\n5\n6\n7\nB8\n"
    );

    fx.cli_write(&oracle, &a, "f.txt", "one\n2\n3\n4\n5\n6\n7\n8\n");
    assert_eq!(fx.trees(&repo, &PATHS), fx.trees(&oracle, &PATHS));
    // jj itself sees a clean, non-stale working copy.
    fx.jj(&repo, &["status"]);
}

#[test]
fn new_file_at_non_wc_commit_matches_cli_squash_recipe() {
    if !have_jj() {
        return;
    }
    let fx = Fixture::new();
    let repo = fx.chain("repo");
    let oracle = fx.copy(&repo, "oracle");
    let w = fx.writer(&repo, "");
    let a = fx.change(&repo, "A");
    w.mutate(
        &MutationPlan::WriteFiles {
            target: fx.commit_of(&repo, &a),
            files: vec![write("new/h.txt", "hello\n")],
            mode: EditMode::Amend,
            message: None,
        },
        "cv: new file",
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(repo.join("new/h.txt")).unwrap(),
        "hello\n"
    );
    // Oracle: a scratch child of A, the file written there, squashed into A.
    let at = fx.jj(
        &oracle,
        &["log", "--no-graph", "-r", "@", "-T", "change_id"],
    );
    fx.jj(&oracle, &["new", &a]);
    std::fs::create_dir_all(oracle.join("new")).unwrap();
    std::fs::write(oracle.join("new/h.txt"), "hello\n").unwrap();
    fx.jj(&oracle, &["squash", "--into", &a]);
    fx.jj(&oracle, &["edit", &at]);
    assert_eq!(fx.trees(&repo, &PATHS), fx.trees(&oracle, &PATHS));
}

#[test]
fn insert_after_and_branch_match_cli() {
    if !have_jj() {
        return;
    }
    let fx = Fixture::new();
    let repo = fx.chain("repo");
    let oracle = fx.copy(&repo, "oracle");
    let w = fx.writer(&repo, "");
    let a = fx.change(&repo, "A");
    let m = w
        .mutate(
            &MutationPlan::WriteFiles {
                target: fx.commit_of(&repo, &a),
                files: vec![write("g.txt", "ALPHA\nbeta\n")],
                mode: EditMode::InsertAfter,
                message: Some("inserted".into()),
            },
            "cv: insert after A",
        )
        .unwrap();
    let created = m.created.clone().unwrap();
    assert!(m.rewritten.len() >= 2, "B and C rebase: {m:?}");
    fx.jj(
        &oracle,
        &["new", "--insert-after", &a, "--no-edit", "-m", "inserted"],
    );
    let ins = fx.change(&oracle, "inserted");
    fx.cli_write(&oracle, &ins, "g.txt", "ALPHA\nbeta\n");
    assert_eq!(fx.trees(&repo, &PATHS), fx.trees(&oracle, &PATHS));
    assert_eq!(fx.change(&repo, "inserted"), created);

    // Branch: a new head on A; nothing rewritten.
    let b = w
        .mutate(
            &MutationPlan::WriteFiles {
                target: fx.commit_of(&repo, &a),
                files: vec![write("f.txt", "branched\n")],
                mode: EditMode::Branch,
                message: Some("side".into()),
            },
            "cv: branch",
        )
        .unwrap();
    assert!(b.rewritten.is_empty());
    assert!(b.workspaces.is_empty());
    fx.jj(&oracle, &["new", &a, "--no-edit", "-m", "side"]);
    let side = fx.change(&oracle, "side");
    fx.cli_write(&oracle, &side, "f.txt", "branched\n");
    assert_eq!(fx.trees(&repo, &PATHS), fx.trees(&oracle, &PATHS));
}

#[test]
fn immutable_targets_only_branch() {
    if !have_jj() {
        return;
    }
    let fx = Fixture::new();
    let repo = fx.chain("repo");
    let a = fx.change(&repo, "A");
    fx.jj(&repo, &["bookmark", "create", "main", "-r", &a]);
    let w = fx.writer(
        &repo,
        "[revset-aliases]\n'immutable_heads()' = 'bookmarks(exact:\"main\")'\n",
    );
    let target = fx.commit_of(&repo, &a);
    assert!(w.is_immutable(&target).unwrap());
    let op0 = fx.op_head(&repo);
    for mode in [
        EditMode::Amend,
        EditMode::InsertAfter,
        EditMode::KeepDescendants,
    ] {
        let e = w
            .preview(&MutationPlan::WriteFiles {
                target: target.clone(),
                files: vec![write("f.txt", "x\n")],
                mode,
                message: None,
            })
            .unwrap_err();
        assert!(e.0.contains("immutable"), "{e}");
    }
    let r = w
        .mutate(
            &MutationPlan::WriteFiles {
                target: target.clone(),
                files: vec![write("f.txt", "x\n")],
                mode: EditMode::Branch,
                message: Some("fix".into()),
            },
            "cv: branch",
        )
        .unwrap();
    assert!(r.created.is_some());
    assert!(r.rewritten.is_empty());
    assert_ne!(fx.op_head(&repo), op0);
    assert_eq!(
        fx.commit_of(&repo, &a),
        target,
        "immutable commit untouched"
    );
}

#[test]
fn insert_conflicts_preview_equals_apply_then_resolve_and_revert() {
    if !have_jj() {
        return;
    }
    let fx = Fixture::new();
    let repo = fx.chain("repo");
    let w = fx.writer(&repo, "");
    let a = fx.change(&repo, "A");
    let b = fx.change(&repo, "B");
    // A rewrites line 8, which B also changed: B (and C on top) conflict.
    let plan = MutationPlan::WriteFiles {
        target: fx.commit_of(&repo, &a),
        files: vec![write("f.txt", "1\n2\n3\n4\n5\n6\n7\nA8\n")],
        mode: EditMode::Amend,
        message: None,
    };
    let p = w.preview(&plan).unwrap();
    assert!(
        p.new_conflicts
            .iter()
            .any(|c| c.change == b && c.path == "f.txt"),
        "{p:?}"
    );
    let m = w.mutate(&plan, "cv: amend A (conflicts)").unwrap();
    let key = |o: &cv_vcs::write::MutationOutcome| {
        let mut v: Vec<(String, String)> = o
            .new_conflicts
            .iter()
            .map(|c| (c.change.clone(), c.path.clone()))
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        key(&p),
        key(&m),
        "predicted conflicts equal the applied ones"
    );
    assert_eq!(p.rewritten, m.rewritten);
    let listed = w.conflicts("all()").unwrap();
    assert!(listed.iter().any(|c| c.change == b));

    // Materialized markers at the non-@ commit B.
    let b_commit = fx.commit_of(&repo, &b);
    let t = w.conflict_text(&b_commit, "f.txt").unwrap().unwrap();
    assert!(t.materialized.contains("<<<<<<<"), "{}", t.materialized);
    assert_eq!(t.regions.len(), 1, "{:?}", t.regions);
    assert_eq!(t.info.sides, 2);
    let cli = fx.jj(
        &repo,
        &["file", "show", "--ignore-working-copy", "-r", &b, "f.txt"],
    );
    assert_eq!(t.materialized, cli, "same markers as the CLI");

    // Resolve at B (not @): descendants rebase clean.
    let r = w
        .mutate(
            &MutationPlan::Resolve {
                commit: b_commit,
                path: "f.txt".into(),
                content: b"1\n2\n3\n4\n5\n6\n7\nAB8\n".to_vec(),
            },
            "cv: resolve",
        )
        .unwrap();
    assert!(r.new_conflicts.is_empty());
    assert!(w.conflicts("all()").unwrap().is_empty());
    assert_eq!(
        std::fs::read_to_string(repo.join("f.txt")).unwrap(),
        "1\n2\n3\n4\n5\n6\n7\nAB8\n"
    );

    // Targeted undo of the resolve restores the conflicted trees.
    let undo = w
        .mutate(
            &MutationPlan::Revert {
                op: r.op_after.clone().unwrap(),
            },
            "cv: undo",
        )
        .unwrap();
    assert!(undo.op_after.is_some());
    assert!(!w.conflicts("all()").unwrap().is_empty());
    fx.jj(&repo, &["status"]);
}

#[test]
fn absorb_moves_hunks_into_owning_commits() {
    if !have_jj() {
        return;
    }
    let fx = Fixture::new();
    let repo = fx.chain("repo");
    let oracle = fx.copy(&repo, "oracle");
    let w = fx.writer(&repo, "");
    let tip = fx.commit_of(&repo, "@");
    // Line 1 belongs to A, line 8 to B, line 3 of g.txt to C (the tip).
    let files = vec![
        write("f.txt", "uno\n2\n3\n4\n5\n6\n7\nB8!\n"),
        write("g.txt", "alpha\nbeta\nGAMMA\n"),
    ];
    w.mutate(
        &MutationPlan::Absorb {
            tip,
            files,
            into: "all()".into(),
        },
        "cv: absorb",
    )
    .unwrap();
    fx.jj(&oracle, &["new"]);
    std::fs::write(oracle.join("f.txt"), "uno\n2\n3\n4\n5\n6\n7\nB8!\n").unwrap();
    std::fs::write(oracle.join("g.txt"), "alpha\nbeta\nGAMMA\n").unwrap();
    fx.jj(&oracle, &["absorb"]);
    // The oracle's leftover scratch commit is dropped from the comparison (empty description);
    // our leftovers amend the tip instead, so compare A and B exactly and C by content.
    let ours = fx.trees(&repo, &PATHS);
    let theirs = fx.trees(&oracle, &PATHS);
    assert_eq!(ours["A"], theirs["A"]);
    assert_eq!(ours["B"], theirs["B"]);
    assert_eq!(ours["C"]["g.txt"], "alpha\nbeta\nGAMMA\n");
    assert_eq!(ours["A"]["f.txt"], "uno\n2\n3\n4\n5\n6\n7\n8\n");
}

#[test]
fn unsnapshotted_edit_in_second_workspace_survives_ancestor_amend() {
    if !have_jj() {
        return;
    }
    let fx = Fixture::new();
    let repo = fx.chain("repo");
    let b = fx.change(&repo, "B");
    let ws2 = fx.dir.join("ws2");
    fx.jj(
        &repo,
        &[
            "workspace",
            "add",
            "--name",
            "ws2",
            ws2.to_str().unwrap(),
            "-r",
            &b,
        ],
    );
    // An edit jj has not snapshotted yet.
    std::fs::write(ws2.join("wip.txt"), "agent was here\n").unwrap();
    let w = fx.writer(&repo, "");
    let a = fx.change(&repo, "A");
    let m = w
        .mutate(
            &MutationPlan::WriteFiles {
                target: fx.commit_of(&repo, &a),
                files: vec![write("g.txt", "ALPHA\nbeta\n")],
                mode: EditMode::Amend,
                message: None,
            },
            "cv: amend A",
        )
        .unwrap();
    let e = m.workspaces.iter().find(|e| e.name == "ws2").unwrap();
    assert!(e.snapshotted && e.updated, "{e:?}");
    assert!(e.recovered_into.is_none());
    // The edit is still on disk and now part of ws2@, which sits on the rewritten B.
    assert_eq!(
        std::fs::read_to_string(ws2.join("wip.txt")).unwrap(),
        "agent was here\n"
    );
    assert_eq!(
        std::fs::read_to_string(ws2.join("g.txt")).unwrap(),
        "ALPHA\nbeta\n"
    );
    let shown = fx.jj(
        &ws2,
        &[
            "file",
            "show",
            "--ignore-working-copy",
            "-r",
            "ws2@",
            "wip.txt",
        ],
    );
    assert_eq!(shown, "agent was here\n");
    // Not stale: a plain `jj status` in ws2 succeeds and does not create a divergent @.
    fx.jj(&ws2, &["status"]);
    let divergent = fx.jj(
        &repo,
        &[
            "log",
            "--ignore-working-copy",
            "--no-graph",
            "-r",
            "divergent()",
            "-T",
            "change_id",
        ],
    );
    assert!(divergent.is_empty(), "divergent: {divergent}");
}

#[test]
fn keep_descendants_matches_cli_restore_descendants() {
    if !have_jj() {
        return;
    }
    let fx = Fixture::new();
    let repo = fx.chain("repo");
    let oracle = fx.copy(&repo, "oracle");
    let w = fx.writer(&repo, "");
    let a = fx.change(&repo, "A");
    let before = fx.trees(&repo, &PATHS);
    let m = w
        .mutate(
            &MutationPlan::WriteFiles {
                target: fx.commit_of(&repo, &a),
                files: vec![write("f.txt", "one\n2\n3\n4\n5\n6\n7\n8\n")],
                mode: EditMode::KeepDescendants,
                message: None,
            },
            "cv: amend A, keep descendants",
        )
        .unwrap();
    assert!(m.new_conflicts.is_empty(), "{m:?}");
    fx.cli_write_with(
        &oracle,
        &a,
        "f.txt",
        "one\n2\n3\n4\n5\n6\n7\n8\n",
        &["--restore-descendants"],
    );
    let ours = fx.trees(&repo, &PATHS);
    assert_eq!(ours, fx.trees(&oracle, &PATHS));
    // Descendants keep their content: B and C look exactly as before.
    assert_eq!(ours["B"], before["B"]);
    assert_eq!(ours["C"], before["C"]);
    assert_eq!(ours["A"]["f.txt"], "one\n2\n3\n4\n5\n6\n7\n8\n");
    fx.jj(&repo, &["status"]);
}

/// Amend A so that B (and C) conflict, the same way on both repos.
fn conflict_b(fx: &Fixture, repo: &Path, oracle: &Path, w: &JjWriter) {
    let a = fx.change(repo, "A");
    w.mutate(
        &MutationPlan::WriteFiles {
            target: fx.commit_of(repo, &a),
            files: vec![write("f.txt", "1\n2\n3\n4\n5\n6\n7\nA8\n")],
            mode: EditMode::Amend,
            message: None,
        },
        "cv: amend A (conflicts)",
    )
    .unwrap();
    fx.cli_write(oracle, &a, "f.txt", "1\n2\n3\n4\n5\n6\n7\nA8\n");
    // Same commit ids on both sides (fixed commit timestamp), so the marker labels agree.
    assert_eq!(fx.commit_of(repo, &a), fx.commit_of(oracle, &a));
    for r in [repo, oracle] {
        let at = fx.jj(
            r,
            &[
                "log",
                "--ignore-working-copy",
                "--no-graph",
                "-r",
                &a,
                "-T",
                "committer.timestamp()",
            ],
        );
        assert!(
            at.starts_with("2001-02-03 04:05:06"),
            "committer timestamp {at:?}"
        );
    }
    assert_eq!(fx.trees(repo, &PATHS), fx.trees(oracle, &PATHS));
}

#[test]
fn resolve_matches_cli_resolve() {
    if !have_jj() {
        return;
    }
    let fx = Fixture::new();
    let repo = fx.chain("repo");
    let oracle = fx.copy(&repo, "oracle");
    let w = fx.writer(&repo, "");
    conflict_b(&fx, &repo, &oracle, &w);
    let b = fx.change(&repo, "B");
    // A full resolution at B (not @).
    let r = w
        .mutate(
            &MutationPlan::Resolve {
                commit: fx.commit_of(&repo, &b),
                path: "f.txt".into(),
                content: b"1\n2\n3\n4\n5\n6\n7\nAB8\n".to_vec(),
            },
            "cv: resolve",
        )
        .unwrap();
    assert!(r.new_conflicts.is_empty());
    fx.cli_resolve(&oracle, &b, "f.txt", "1\n2\n3\n4\n5\n6\n7\nAB8\n");
    assert_eq!(fx.trees(&repo, &PATHS), fx.trees(&oracle, &PATHS));
    assert!(w.conflicts("all()").unwrap().is_empty());
    fx.jj(&repo, &["status"]);
}

#[test]
fn revert_matches_cli_op_revert() {
    if !have_jj() {
        return;
    }
    let fx = Fixture::new();
    let repo = fx.chain("repo");
    let oracle = fx.copy(&repo, "oracle");
    let original = fx.trees(&repo, &PATHS);
    let w = fx.writer(&repo, "");
    conflict_b(&fx, &repo, &oracle, &w);
    let ours_op = fx.op_head(&repo);
    let theirs_op = fx.op_head(&oracle);
    w.mutate(&MutationPlan::Revert { op: ours_op }, "cv: undo")
        .unwrap();
    fx.jj(&oracle, &["op", "revert", &theirs_op]);
    let ours = fx.trees(&repo, &PATHS);
    assert_eq!(ours, fx.trees(&oracle, &PATHS));
    assert_eq!(ours, original, "revert restores the trees");
    assert!(w.conflicts("all()").unwrap().is_empty());
    fx.jj(&repo, &["status"]);
}

/// N11: with jj-lib failing (forced), every plan goes through the jj CLI fallback and leaves the
/// same trees as the jj-lib path; the working copy follows, nothing is stale, no temporary
/// workspace is left behind, and user errors are not retried.
#[test]
fn cli_fallback_matches_jj_lib() {
    if !have_jj() {
        return;
    }
    let fx = Fixture::new();
    let base = fx.chain("base");
    let lib = fx.copy(&base, "lib");
    let cli = fx.copy(&base, "cli");
    let wl = fx.writer(&lib, "");
    let wc = fx.writer(&cli, "");
    wc.set_force_cli_fallback(true);
    let a = fx.change(&base, "A");
    let b = fx.change(&base, "B");
    // Conflict marker labels name commit ids, which differ between the two repos.
    let norm = |text: &str| {
        text.lines()
            .map(|l| match l.get(..7) {
                Some(m)
                    if m.chars().all(|c| c == m.chars().next().unwrap())
                        && "<>%+-\\".contains(&m[..1]) =>
                {
                    m
                }
                _ => l,
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let trees = |r: &Path| {
        let mut t = fx.trees(r, &PATHS);
        for files in t.values_mut() {
            for text in files.values_mut() {
                *text = norm(text);
            }
        }
        t
    };
    let check = |what: &str| {
        assert_eq!(
            trees(&lib),
            trees(&cli),
            "{what}: CLI fallback differs from jj-lib"
        );
        assert_eq!(
            norm(&std::fs::read_to_string(lib.join("f.txt")).unwrap()),
            norm(&std::fs::read_to_string(cli.join("f.txt")).unwrap()),
            "{what}: working copy"
        );
        let ws = fx.jj(&cli, &["workspace", "list"]);
        assert_eq!(
            ws.lines().count(),
            1,
            "{what}: temporary workspace left: {ws}"
        );
        fx.jj(&cli, &["status"]);
    };
    let both = |plan: &dyn Fn(&Path) -> MutationPlan| {
        let l = wl.mutate(&plan(&lib), "cv: lib").unwrap();
        let c = wc.mutate(&plan(&cli), "cv: cli").unwrap();
        assert!(c.op_after.is_some());
        (l, c)
    };

    // Amend A: B, C and @ rebase; the working copy follows.
    let (l, c) = both(&|r| MutationPlan::WriteFiles {
        target: fx.commit_of(r, &a),
        files: vec![write("f.txt", "one\n2\n3\n4\n5\n6\n7\n8\n")],
        mode: EditMode::Amend,
        message: None,
    });
    assert_eq!(l.rewritten.len(), c.rewritten.len(), "{l:?}\n{c:?}");
    check("amend");
    // Keep descendants, with a new file and a deletion.
    both(&|r| MutationPlan::WriteFiles {
        target: fx.commit_of(r, &a),
        files: vec![write("new/h.txt", "h\n")],
        mode: EditMode::KeepDescendants,
        message: None,
    });
    check("keep-descendants");
    // Insert after and branch create changes with the given descriptions.
    let (l, c) = both(&|r| MutationPlan::WriteFiles {
        target: fx.commit_of(r, &a),
        files: vec![write("g.txt", "ALPHA\n")],
        mode: EditMode::InsertAfter,
        message: Some("inserted".into()),
    });
    assert_eq!(
        c.created.as_deref(),
        Some(fx.change(&cli, "inserted").as_str())
    );
    assert!(l.created.is_some());
    check("insert-after");
    both(&|r| MutationPlan::WriteFiles {
        target: fx.commit_of(r, &a),
        files: vec![
            write("f.txt", "branched\n"),
            FileWrite {
                path: "g.txt".into(),
                content: None,
            },
        ],
        mode: EditMode::Branch,
        message: Some("side".into()),
    });
    check("branch");
    // Range edit: absorb into the owning commits, leftovers amend the tip.
    both(&|r| MutationPlan::Absorb {
        tip: fx.commit_of(r, "@"),
        files: vec![write("f.txt", "uno\n2\n3\n4\n5\n6\n7\nB8!\n")],
        into: "all()".into(),
    });
    check("absorb");
    // Conflict B, then resolve it.
    let (_, c) = both(&|r| MutationPlan::WriteFiles {
        target: fx.commit_of(r, &a),
        files: vec![write("f.txt", "uno\n2\n3\n4\n5\n6\n7\nA8\n")],
        mode: EditMode::Amend,
        message: None,
    });
    assert!(c.new_conflicts.iter().any(|x| x.change == b), "{c:?}");
    check("conflicting amend");
    let (_, c) = both(&|r| MutationPlan::Resolve {
        commit: fx.commit_of(r, &b),
        path: "f.txt".into(),
        content: b"uno\n2\n3\n4\n5\n6\n7\nAB8\n".to_vec(),
    });
    assert!(c.new_conflicts.is_empty());
    let at = |w: &JjWriter| {
        let mut v: Vec<(String, String)> = w
            .conflicts("all()")
            .unwrap()
            .into_iter()
            .map(|c| (c.change, c.path))
            .collect();
        v.sort();
        v
    };
    assert!(!at(&wc).iter().any(|(ch, _)| *ch == b), "B is resolved");
    // The created "inserted" change has a different change id in each repo: compare paths.
    let paths = |v: Vec<(String, String)>| {
        let mut p: Vec<String> = v.into_iter().map(|(_, p)| p).collect();
        p.sort();
        p
    };
    assert_eq!(paths(at(&wl)), paths(at(&wc)));
    check("resolve");
    // Revert the resolve: the conflicts come back.
    let lop = fx.op_head(&lib);
    let cop = fx.op_head(&cli);
    wl.mutate(&MutationPlan::Revert { op: lop }, "cv: undo")
        .unwrap();
    wc.mutate(&MutationPlan::Revert { op: cop }, "cv: undo")
        .unwrap();
    assert!(!wc.conflicts("all()").unwrap().is_empty());
    check("revert");

    // User errors are reported, not retried through the CLI.
    let op = fx.op_head(&cli);
    let e = wc
        .mutate(
            &MutationPlan::Resolve {
                commit: fx.commit_of(&cli, &a),
                path: "f.txt".into(),
                content: b"x\n".to_vec(),
            },
            "cv: resolve",
        )
        .unwrap_err();
    assert!(e.0.contains("not conflicted"), "{e}");
    assert!(!e.0.contains("fallback"), "{e}");
    assert_eq!(fx.op_head(&cli), op);
}

/// A client's path must name a file inside the tree: `..` would put an entry outside it (and the
/// CLI fallback would write there), and VCS metadata is off limits. The CLI fallback also never
/// writes through a symlink checked out from the commit.
#[test]
fn edits_stay_inside_the_tree() {
    if !have_jj() {
        return;
    }
    let fx = Fixture::new();
    let repo = fx.chain("repo");
    let outside = fx.dir.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, repo.join("link")).unwrap();
    fx.jj(&repo, &["describe", "-m", "C with a link"]);
    let c = fx.commit_of(&repo, "@");
    let plan = |path: &str| MutationPlan::WriteFiles {
        target: c.clone(),
        files: vec![write(path, "x\n")],
        mode: EditMode::Amend,
        message: None,
    };
    for cli in [false, true] {
        let w = fx.writer(&repo, "");
        w.set_force_cli_fallback(cli);
        for path in [
            "../escape.txt",
            "new/../../escape.txt",
            "./f.txt",
            "new//h.txt",
            ".git/config",
            ".jj/repo/config.toml",
            "",
        ] {
            let op0 = fx.op_head(&repo);
            assert!(
                w.mutate(&plan(path), "cv: escape").is_err(),
                "{path:?} accepted (cli {cli})"
            );
            assert_eq!(
                fx.op_head(&repo),
                op0,
                "{path:?} wrote an operation (cli {cli})"
            );
        }
    }
    let w = fx.writer(&repo, "");
    w.set_force_cli_fallback(true);
    assert!(w.mutate(&plan("link/x.txt"), "cv: through a link").is_err());
    assert!(!outside.join("x.txt").exists(), "wrote through the symlink");
    assert!(!fx.dir.join("escape.txt").exists());
}
