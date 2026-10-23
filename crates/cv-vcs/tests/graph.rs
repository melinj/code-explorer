//! Graph-view operations ([`GraphOp`]): bookmarks move only forward unless allowed, a workspace's
//! stack lands on the trunk bookmark by fast-forward, rebase or merge (its `@` follows, on disk
//! too), the history edits behave like their jj commands, previews never write, immutable commits
//! are refused, and the jj CLI fallback lands the same graph as jj-lib.
//! Repos are built with the jj CLI in temp dirs; skipped when `jj` is not on PATH.

use std::path::{Path, PathBuf};
use std::process::Command;

use cv_vcs::write::{GraphOp, JjWriter, LandStrategy, MutationPlan, RebaseMode};

const USER: &str = "[user]\nname = \"Test\"\nemail = \"test@example.com\"\n";

struct Repo {
    _tmp: tempfile::TempDir,
    cfg: PathBuf,
    root: PathBuf,
    ws: PathBuf,
}

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

impl Repo {
    /// main = T0 (a.txt); workspace `ws` with S1 (s.txt) on T0 and an empty `ws@` on S1. With
    /// `diverge`, main moves on to T1 (t.txt) first, so the stack no longer fast-forwards.
    fn new(diverge: bool) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("jjconfig.toml");
        std::fs::write(&cfg, format!("{USER}[ui]\npaginate = \"never\"\n")).unwrap();
        let root = tmp.path().join("repo");
        let ws = tmp.path().join("ws");
        std::fs::create_dir(&root).unwrap();
        let r = Repo {
            _tmp: tmp,
            cfg,
            root,
            ws,
        };
        r.jj(&r.root, &["git", "init"]);
        std::fs::write(r.root.join("a.txt"), "base\n").unwrap();
        r.jj(&r.root, &["commit", "-m", "T0"]);
        r.jj(&r.root, &["bookmark", "create", "main", "-r", "@-"]);
        let ws_s = r.ws.to_string_lossy().into_owned();
        r.jj(
            &r.root,
            &["workspace", "add", "--name", "ws", "-r", "main", &ws_s],
        );
        std::fs::write(r.ws.join("s.txt"), "stack\n").unwrap();
        r.jj(&r.ws, &["commit", "-m", "S1"]);
        r.jj(&r.ws, &["bookmark", "create", "feature", "-r", "@-"]);
        if diverge {
            r.jj(&r.root, &["new", "main"]);
            std::fs::write(r.root.join("t.txt"), "trunk\n").unwrap();
            r.jj(&r.root, &["commit", "-m", "T1"]);
            r.jj(&r.root, &["bookmark", "set", "main", "-r", "@-"]);
        }
        r
    }

    fn jj(&self, cwd: &Path, args: &[&str]) -> String {
        let out = Command::new("jj")
            .args(args)
            .current_dir(cwd)
            .env("JJ_CONFIG", &self.cfg)
            .output()
            .expect("run jj");
        assert!(
            out.status.success(),
            "jj {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// `jj log -r <rev> -T <template>` without snapshotting.
    fn log(&self, rev: &str, template: &str) -> String {
        self.jj(
            &self.root,
            &[
                "log",
                "--ignore-working-copy",
                "--no-graph",
                "-r",
                rev,
                "-T",
                template,
            ],
        )
    }

    fn commit(&self, rev: &str) -> String {
        self.log(rev, "commit_id")
    }

    /// First lines of the descriptions of `rev`'s parents, sorted.
    fn parent_titles(&self, rev: &str) -> Vec<String> {
        let mut v: Vec<String> = self
            .log(
                &format!("parents({rev})"),
                "description.first_line() ++ \"\\n\"",
            )
            .lines()
            .map(str::to_string)
            .collect();
        v.sort();
        v
    }

    fn title(&self, rev: &str) -> String {
        self.log(rev, "description.first_line()")
    }

    fn writer(&self, extra: &str) -> JjWriter {
        JjWriter::open_with(&self.root, true, Some(&format!("{USER}{extra}"))).unwrap()
    }

    fn land(&self, w: &JjWriter, strategy: LandStrategy) -> cv_vcs::write::MutationOutcome {
        let (head, carry) = w.stack_head(&self.commit("ws@")).unwrap();
        assert_eq!(
            head,
            self.commit("feature"),
            "an empty ws@ lands its parent"
        );
        let plan = MutationPlan::Graph(GraphOp::Land {
            head,
            trunk: "main".into(),
            strategy,
            message: None,
            carry,
            delete_bookmark: Some("feature".into()),
        });
        w.mutate(&plan, "land ws").unwrap()
    }
}

#[test]
fn land_fast_forwards_main_onto_the_stack() {
    if !have_jj() {
        return;
    }
    let r = Repo::new(false);
    let w = r.writer("");
    let s1 = r.commit("feature");
    let out = r.land(&w, LandStrategy::FastForward);
    assert_eq!(r.commit("main"), s1);
    assert_eq!(
        r.parent_titles("ws@"),
        ["S1"],
        "ws@ stays on the landed head"
    );
    assert!(out.rewritten.is_empty(), "nothing is rewritten: {out:?}");
    assert!(
        out.bookmarks
            .iter()
            .any(|(n, _, a)| n == "main" && a.as_deref() == Some(s1.as_str()))
    );
    assert!(
        out.bookmarks
            .iter()
            .any(|(n, _, a)| n == "feature" && a.is_none()),
        "feature deleted"
    );
}

#[test]
fn fast_forward_is_refused_when_main_moved_on() {
    if !have_jj() {
        return;
    }
    let r = Repo::new(true);
    let w = r.writer("");
    let (head, carry) = w.stack_head(&r.commit("ws@")).unwrap();
    let e = w
        .mutate(
            &MutationPlan::Graph(GraphOp::Land {
                head,
                trunk: "main".into(),
                strategy: LandStrategy::FastForward,
                message: None,
                carry,
                delete_bookmark: None,
            }),
            "land",
        )
        .unwrap_err();
    assert!(e.0.contains("cannot fast-forward"), "{e}");
}

#[test]
fn land_by_rebase_moves_the_stack_and_its_workspace_onto_main() {
    if !have_jj() {
        return;
    }
    let r = Repo::new(true);
    let w = r.writer("");
    let out = r.land(&w, LandStrategy::Rebase);
    assert_eq!(r.title("main"), "S1");
    assert_eq!(r.parent_titles("main"), ["T1"]);
    assert_eq!(r.parent_titles("ws@"), ["S1"]);
    assert_eq!(r.commit("parents(ws@)"), r.commit("main"));
    assert!(
        out.workspaces.iter().any(|e| e.name == "ws" && e.updated),
        "{out:?}"
    );
    assert_eq!(
        std::fs::read_to_string(r.ws.join("t.txt")).unwrap(),
        "trunk\n",
        "the workspace's files follow its rebased @"
    );
}

#[test]
fn land_by_merge_creates_a_merge_and_carries_the_workspace() {
    if !have_jj() {
        return;
    }
    let r = Repo::new(true);
    let w = r.writer("");
    let out = r.land(&w, LandStrategy::Merge);
    assert_eq!(r.title("main"), "Merge \"S1\" into main");
    assert_eq!(r.parent_titles("main"), ["S1", "T1"]);
    assert_eq!(
        r.commit("parents(ws@)"),
        r.commit("main"),
        "ws@ moved onto the merge"
    );
    assert!(out.created.is_some());
    assert!(std::fs::read_to_string(r.ws.join("t.txt")).is_ok());
    assert!(std::fs::read_to_string(r.ws.join("s.txt")).is_ok());
}

#[test]
fn cli_fallback_lands_the_same_graph() {
    if !have_jj() {
        return;
    }
    for strategy in [LandStrategy::Rebase, LandStrategy::Merge] {
        let r = Repo::new(true);
        let w = r.writer("");
        w.set_force_cli_fallback(true);
        r.land(&w, strategy);
        match strategy {
            LandStrategy::Rebase => {
                assert_eq!(r.title("main"), "S1");
                assert_eq!(r.parent_titles("main"), ["T1"]);
            }
            _ => {
                assert_eq!(r.parent_titles("main"), ["S1", "T1"]);
                assert_eq!(r.commit("parents(ws@)"), r.commit("main"));
            }
        }
        assert_eq!(
            r.log("bookmarks(exact:\"feature\")", "commit_id"),
            "",
            "feature deleted"
        );
    }
}

#[test]
fn bookmarks_move_forward_and_backwards_only_when_allowed() {
    if !have_jj() {
        return;
    }
    let r = Repo::new(true);
    let w = r.writer("");
    let (t0, t1, s1) = (r.commit("main-"), r.commit("main"), r.commit("feature"));
    let set = |to: &str, allow_backwards| {
        MutationPlan::Graph(GraphOp::SetBookmark {
            name: "main".into(),
            to: to.into(),
            allow_backwards,
        })
    };
    // Sideways (T1 → S1) and backwards (T1 → T0) need the flag.
    assert!(
        w.mutate(&set(&s1, false), "x")
            .unwrap_err()
            .0
            .contains("backwards or sideways")
    );
    assert!(w.mutate(&set(&t0, false), "x").is_err());
    // A preview reports the move and writes nothing.
    let op = w.head_op().unwrap();
    let p = w.preview(&set(&t0, true)).unwrap();
    assert_eq!(
        p.bookmarks,
        vec![("main".into(), Some(t1.clone()), Some(t0.clone()))]
    );
    assert_eq!(w.head_op().unwrap(), op);
    w.mutate(&set(&t0, true), "x").unwrap();
    assert_eq!(r.commit("main"), t0);
    // Forward needs nothing; a new name creates the bookmark.
    w.mutate(&set(&t1, false), "x").unwrap();
    assert_eq!(r.commit("main"), t1);
    w.mutate(
        &MutationPlan::Graph(GraphOp::SetBookmark {
            name: "release/1".into(),
            to: t0.clone(),
            allow_backwards: false,
        }),
        "x",
    )
    .unwrap();
    assert_eq!(r.commit("release/1"), t0);
    w.mutate(
        &MutationPlan::Graph(GraphOp::DeleteBookmark {
            name: "release/1".into(),
        }),
        "x",
    )
    .unwrap();
    assert!(w.bookmarks().unwrap().iter().all(|b| b.name != "release/1"));
    let bad = MutationPlan::Graph(GraphOp::SetBookmark {
        name: "x@origin".into(),
        to: t0,
        allow_backwards: false,
    });
    assert!(w.mutate(&bad, "x").is_err());
}

#[test]
fn history_edits_behave_like_their_jj_commands() {
    if !have_jj() {
        return;
    }
    let r = Repo::new(true);
    let w = r.writer("");
    // describe
    let s1 = r.commit("feature");
    w.mutate(
        &MutationPlan::Graph(GraphOp::Describe {
            commit: s1,
            message: "S1 renamed".into(),
        }),
        "x",
    )
    .unwrap();
    assert_eq!(r.title("feature"), "S1 renamed");
    // rebase -s feature onto main: the stack and ws@ move.
    w.mutate(
        &MutationPlan::Graph(GraphOp::Rebase {
            source: r.commit("feature"),
            mode: RebaseMode::Source,
            dest: vec![r.commit("main")],
        }),
        "x",
    )
    .unwrap();
    assert_eq!(r.parent_titles("feature"), ["T1"]);
    assert_eq!(r.commit("parents(ws@)"), r.commit("feature"));
    // new --no-edit on main: nobody's @ moves.
    let ws_at = r.commit("ws@");
    let out = w
        .mutate(
            &MutationPlan::Graph(GraphOp::New {
                parents: vec![r.commit("main")],
                message: Some("N".into()),
            }),
            "x",
        )
        .unwrap();
    let n = out.created.expect("created change");
    assert_eq!(r.title(&n), "N");
    assert_eq!(r.commit("ws@"), ws_at);
    // abandon N.
    w.mutate(
        &MutationPlan::Graph(GraphOp::Abandon {
            commits: vec![r.commit(&n)],
        }),
        "x",
    )
    .unwrap();
    assert_eq!(r.log("description(exact:\"N\\n\")", "commit_id"), "");
    // A rebase onto its own descendant is refused.
    let e = w
        .mutate(
            &MutationPlan::Graph(GraphOp::Rebase {
                source: r.commit("feature"),
                mode: RebaseMode::Source,
                dest: vec![r.commit("ws@")],
            }),
            "x",
        )
        .unwrap_err();
    assert!(e.0.contains("being moved"), "{e}");
}

#[test]
fn immutable_commits_are_refused() {
    if !have_jj() {
        return;
    }
    let r = Repo::new(true);
    let w = r.writer("[revset-aliases]\n'immutable_heads()' = 'main'\n");
    let e = w
        .mutate(
            &MutationPlan::Graph(GraphOp::Describe {
                commit: r.commit("main"),
                message: "nope".into(),
            }),
            "x",
        )
        .unwrap_err();
    assert!(e.0.contains("immutable"), "{e}");
    let e = w
        .mutate(
            &MutationPlan::Graph(GraphOp::Abandon {
                commits: vec![r.commit("main-")],
            }),
            "x",
        )
        .unwrap_err();
    assert!(e.0.contains("immutable"), "{e}");
    // Moving a bookmark rewrites nothing: allowed.
    w.mutate(
        &MutationPlan::Graph(GraphOp::SetBookmark {
            name: "main".into(),
            to: r.commit("feature"),
            allow_backwards: true,
        }),
        "x",
    )
    .unwrap();
}

#[test]
fn colocated_repos_see_bookmark_moves_as_git_branches() {
    if !have_jj() || Command::new("git").arg("--version").output().is_err() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("jjconfig.toml");
    std::fs::write(&cfg, USER).unwrap();
    let root = tmp.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    let jj = |args: &[&str]| {
        let o = Command::new("jj")
            .args(args)
            .current_dir(&root)
            .env("JJ_CONFIG", &cfg)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    jj(&["git", "init", "--colocate"]);
    std::fs::write(root.join("a.txt"), "1\n").unwrap();
    jj(&["commit", "-m", "A"]);
    std::fs::write(root.join("a.txt"), "2\n").unwrap();
    jj(&["commit", "-m", "B"]);
    jj(&["bookmark", "create", "main", "-r", "@--"]);
    let b = jj(&["log", "--no-graph", "-r", "@-", "-T", "commit_id"]);
    let w = JjWriter::open_with(&root, true, Some(USER)).unwrap();
    assert!(w.colocated());
    w.mutate(
        &MutationPlan::Graph(GraphOp::SetBookmark {
            name: "main".into(),
            to: b.clone(),
            allow_backwards: false,
        }),
        "x",
    )
    .unwrap();
    let git = Command::new("git")
        .args(["rev-parse", "refs/heads/main"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&git.stdout).trim(), b);
}

#[test]
fn commit_describes_at_and_starts_a_new_one_like_jj_commit() {
    if !have_jj() {
        return;
    }
    for cli in [false, true] {
        let r = Repo::new(false);
        let w = r.writer("");
        w.set_force_cli_fallback(cli);
        // Edits on disk that no snapshot has seen yet: the commit takes them.
        std::fs::write(r.ws.join("w.txt"), "work\n").unwrap();
        let before = r.commit("ws@");
        let main0 = r.commit("main");
        let plan = MutationPlan::Graph(GraphOp::Commit {
            workspace: "ws".into(),
            message: "S2: work".into(),
            bookmark: Some("feature".into()),
        });
        w.preview(&plan).unwrap();
        assert_eq!(r.commit("ws@"), before, "a preview writes nothing");
        let out = w.mutate(&plan, "commit").unwrap();
        assert_eq!(r.title("ws@-"), "S2: work", "cli={cli}");
        assert_eq!(r.parent_titles("ws@-"), ["S1"]);
        assert_eq!(r.log("ws@", "empty"), "true", "a new empty @ on top");
        assert_eq!(r.log("ws@", "description"), "");
        assert_eq!(
            r.jj(
                &r.root,
                &[
                    "file",
                    "show",
                    "--ignore-working-copy",
                    "-r",
                    "ws@-",
                    "w.txt"
                ]
            ),
            "work\n",
            "the files on disk were snapshotted into the commit"
        );
        assert_eq!(
            r.commit("feature"),
            r.commit("ws@-"),
            "feature moved forward"
        );
        assert_eq!(r.commit("main"), main0, "main untouched");
        assert!(out.created.is_some(), "{out:?}");
        assert!(r.ws.join("w.txt").exists(), "files on disk stay");
        r.jj(&r.ws, &["status"]);
        assert_eq!(r.log("ws@", "empty"), "true", "the workspace is not stale");
    }
    // Refusals: no message, an unknown workspace, a bookmark that would move sideways.
    let r = Repo::new(true);
    let w = r.writer("");
    let commit = |ws: &str, m: &str, b: Option<&str>| {
        w.mutate(
            &MutationPlan::Graph(GraphOp::Commit {
                workspace: ws.into(),
                message: m.into(),
                bookmark: b.map(str::to_string),
            }),
            "commit",
        )
        .unwrap_err()
        .0
    };
    assert!(commit("ws", " ", None).contains("needs a message"));
    assert!(commit("nope", "x", None).contains("no workspace"));
    assert!(commit("ws", "x", Some("main")).contains("backwards or sideways"));
}

#[test]
fn squash_moves_changes_into_the_parent_or_a_picked_change() {
    if !have_jj() {
        return;
    }
    for cli in [false, true] {
        // ws@ (with edits on disk no snapshot has seen) into its parent S1: the edits land in S1
        // and ws gets a new empty @ on top.
        let r = Repo::new(false);
        let w = r.writer("");
        w.set_force_cli_fallback(cli);
        std::fs::write(r.ws.join("w.txt"), "work\n").unwrap();
        w.mutate(
            &MutationPlan::Graph(GraphOp::Squash {
                source: r.commit("ws@"),
                into: None,
                message: None,
            }),
            "squash",
        )
        .unwrap();
        assert_eq!(r.title("feature"), "S1", "cli={cli}");
        assert_eq!(
            r.jj(
                &r.root,
                &[
                    "file",
                    "show",
                    "--ignore-working-copy",
                    "-r",
                    "feature",
                    "w.txt"
                ]
            ),
            "work\n",
            "the edits on disk moved into S1"
        );
        assert_eq!(r.commit("ws@-"), r.commit("feature"));
        assert_eq!(r.log("ws@", "empty"), "true");
        r.jj(&r.ws, &["status"]);

        // S1 into T0 (a picked change): both descriptions kept, S1 abandoned, feature follows.
        w.mutate(
            &MutationPlan::Graph(GraphOp::Squash {
                source: r.commit("feature"),
                into: Some(r.commit("main")),
                message: None,
            }),
            "squash",
        )
        .unwrap();
        assert_eq!(r.log("main", "description"), "T0\n\nS1\n", "cli={cli}");
        assert_eq!(r.commit("feature"), r.commit("main"));
        assert_eq!(r.commit("ws@-"), r.commit("main"));
        for f in ["a.txt", "s.txt", "w.txt"] {
            r.jj(
                &r.root,
                &["file", "show", "--ignore-working-copy", "-r", "main", f],
            );
        }
    }
    // An explicit message wins; a change cannot go into itself or the root.
    let r = Repo::new(false);
    let w = r.writer("");
    let squash = |source: String, into: Option<String>, message: Option<&str>| {
        w.mutate(
            &MutationPlan::Graph(GraphOp::Squash {
                source,
                into,
                message: message.map(str::to_string),
            }),
            "squash",
        )
    };
    let s1 = r.commit("feature");
    assert!(
        squash(s1.clone(), Some(s1.clone()), None)
            .unwrap_err()
            .0
            .contains("into itself")
    );
    assert!(
        squash(r.commit("main"), None, None)
            .unwrap_err()
            .0
            .contains("root")
    );
    squash(s1, None, Some("T0 and S1")).unwrap();
    assert_eq!(r.log("main", "description"), "T0 and S1\n");
}

#[test]
fn op_log_lists_operations_and_restore_goes_back_to_one() {
    if !have_jj() {
        return;
    }
    for cli in [false, true] {
        let r = Repo::new(false);
        let w = r.writer("");
        w.set_force_cli_fallback(cli);
        w.mutate(
            &MutationPlan::Graph(GraphOp::SetBookmark {
                name: "extra".into(),
                to: r.commit("feature"),
                allow_backwards: false,
            }),
            "cv: bookmark extra",
        )
        .unwrap();
        let (log, more) = w.op_log(3).unwrap();
        assert_eq!(log.len(), 3);
        assert!(more, "the repo has more than three operations");
        assert_eq!(log[0].id, w.head_op().unwrap());
        assert_eq!(log[1].id, log[0].parents[0], "newest first, along parents");
        if !cli {
            assert_eq!(log[0].description, "cv: bookmark extra");
        }
        let d = w.op_diff(&log[1].id, &log[0].id).unwrap();
        assert!(
            d.bookmarks
                .iter()
                .any(|(n, a, b)| n == "extra" && a.is_none() && b.is_some()),
            "{d:?}"
        );
        // Restore to before the bookmark, then to the operation that made it.
        let (made, before) = (log[0].id.clone(), log[1].id.clone());
        w.mutate(&MutationPlan::Restore { op: before }, "restore")
            .unwrap();
        assert_eq!(
            r.log("bookmarks(exact:\"extra\")", "commit_id"),
            "",
            "cli={cli}"
        );
        w.mutate(&MutationPlan::Restore { op: made }, "restore")
            .unwrap();
        assert_eq!(r.commit("extra"), r.commit("feature"));
        let head = w.head_op().unwrap();
        assert!(
            w.check(&MutationPlan::Restore { op: head })
                .unwrap_err()
                .0
                .contains("current operation")
        );
    }
}

#[test]
fn fetch_and_push_go_through_the_users_git_remotes() {
    if !have_jj() {
        return;
    }
    let git = |cwd: &Path, args: &[&str]| -> String {
        let o = Command::new("git")
            .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    let r = Repo::new(false);
    let w = r.writer("");
    assert!(w.git_fetch(None).unwrap_err().0.contains("no git remote"));
    let remote = r._tmp.path().join("remote.git");
    std::fs::create_dir(&remote).unwrap();
    git(&remote, &["init", "-q", "--bare"]);
    r.jj(
        &r.root,
        &["git", "remote", "add", "origin", remote.to_str().unwrap()],
    );
    assert_eq!(w.git_remotes().unwrap(), ["origin"]);

    // A dry run says what it would push and pushes nothing.
    let plan = w.git_push(&["main".into()], None, true).unwrap();
    assert!(plan.output.contains("main"), "{}", plan.output);
    assert_eq!(git(&remote, &["for-each-ref"]), "");
    let pushed = w.git_push(&["main".into()], Some("origin"), false).unwrap();
    assert_eq!(git(&remote, &["rev-parse", "main"]), r.commit("main"));
    assert!(
        pushed
            .moved
            .iter()
            .any(|m| m.name == "main@origin" && m.to.as_deref() == Some(r.commit("main").as_str())),
        "{pushed:?}"
    );

    // Someone else pushes to main; a fetch brings it in and the tracked main follows.
    let other = r._tmp.path().join("other");
    git(
        r._tmp.path(),
        &[
            "clone",
            "-q",
            "-b",
            "main",
            remote.to_str().unwrap(),
            "other",
        ],
    );
    std::fs::write(other.join("o.txt"), "other\n").unwrap();
    git(&other, &["add", "."]);
    git(&other, &["commit", "-qm", "T-other"]);
    git(&other, &["push", "-q", "origin", "HEAD:main"]);
    let fetched = w.git_fetch(Some("origin")).unwrap();
    assert_eq!(r.title("main"), "T-other");
    assert!(
        fetched.moved.iter().any(|m| m.name == "main@origin"),
        "{fetched:?}"
    );
    assert!(
        fetched.moved.iter().any(|m| m.name == "main"),
        "{fetched:?}"
    );

    // Names are checked before jj runs.
    assert!(
        w.git_push(&["nope".into()], None, true)
            .unwrap_err()
            .0
            .contains("no bookmark")
    );
    assert!(w.git_push(&["--all".into()], None, true).is_err());
    assert!(w.git_push(&[], None, true).is_err());
    assert!(
        w.git_fetch(Some("--upload-pack=x"))
            .unwrap_err()
            .0
            .contains("no git remote")
    );
}
