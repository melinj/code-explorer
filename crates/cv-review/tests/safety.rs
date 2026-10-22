//! DESIGN §6 / §10 safety on CLI-built repos: targeted undo refuses to create divergence (the
//! audit case: amend through the service, external `jj describe` of the amended change, undo),
//! plain undo, a stale workspace recovered into a divergent sibling with `workspace.recovered`,
//! the "agent attached" preview warning, apply re-checking a stale preview, the `locked` queue
//! timeout and per-actor op descriptions. Skipped when `jj` is not on PATH.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use cv_contracts::review::*;
use cv_review::Review;
use cv_vcs::write::JjWriter;

const USER: &str = "[user]\nname = \"Test\"\nemail = \"test@example.com\"\n";

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

struct Fx {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    repo: PathBuf,
}

impl Fx {
    /// A (a.py, 20 lines) ← B (b.py) = @.
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::write(dir.join("jjconfig.toml"), USER).unwrap();
        let repo = dir.join("repo");
        std::fs::create_dir(&repo).unwrap();
        let fx = Fx {
            _tmp: tmp,
            dir,
            repo,
        };
        fx.jj(&["git", "init"]);
        let body: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        std::fs::write(fx.repo.join("a.py"), &body).unwrap();
        fx.jj(&["commit", "-m", "A"]);
        std::fs::write(fx.repo.join("b.py"), "x = 1\n").unwrap();
        fx.jj(&["describe", "-m", "B"]);
        fx
    }

    fn jj_in(&self, dir: &Path, args: &[&str]) -> String {
        let out = Command::new("jj")
            .args(args)
            .current_dir(dir)
            .env("JJ_CONFIG", self.dir.join("jjconfig.toml"))
            .output()
            .expect("run jj");
        assert!(
            out.status.success(),
            "jj {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn jj(&self, args: &[&str]) -> String {
        self.jj_in(&self.repo, args)
    }

    fn change(&self, desc: &str) -> String {
        self.jj(&[
            "log",
            "--ignore-working-copy",
            "--no-graph",
            "-r",
            &format!("description(exact:\"{desc}\\n\")"),
            "-T",
            "change_id",
        ])
    }

    /// Visible commits of a change.
    fn commits(&self, change: &str) -> Vec<String> {
        self.jj(&[
            "log",
            "--ignore-working-copy",
            "--no-graph",
            "-r",
            &format!("change_id({change})"),
            "-T",
            "commit_id ++ \"\\n\"",
        ])
        .lines()
        .map(String::from)
        .collect()
    }

    fn head_op(&self) -> String {
        self.jj(&[
            "op",
            "log",
            "--ignore-working-copy",
            "--no-graph",
            "-n1",
            "-T",
            "id",
        ])
    }

    fn show(&self, rev: &str, path: &str) -> String {
        self.jj(&["file", "show", "--ignore-working-copy", "-r", rev, path])
    }

    fn review(&self) -> Arc<Review> {
        let w = JjWriter::open_with(&self.repo, true, Some(USER)).unwrap();
        Review::open_with(w, None).unwrap()
    }
}

fn you() -> Actor {
    Actor::human("you")
}

fn amend(target: &str, line: u32, text: &str) -> EditRequest {
    EditRequest {
        target: target.into(),
        files: vec![EditFile {
            path: "a.py".into(),
            base_blob: None,
            content: None,
            patches: Some(vec![LinePatch {
                start: line,
                end: line,
                text: text.into(),
            }]),
        }],
        mode: "amend".into(),
        message: None,
        absorb_into: None,
        dry_run: false,
        origin: None,
    }
}

#[test]
fn undo_after_external_describe_is_blocked_and_changes_nothing() {
    if !have_jj() {
        return;
    }
    let fx = Fx::new();
    let r = fx.review();
    let a = fx.change("A");
    let res = r.edit(amend(&a, 1, "line one"), &you()).unwrap();
    assert_eq!(res.status, "applied", "{res:?}");
    // Someone else rewrites the amended change after our edit.
    fx.jj(&["describe", "-r", &a, "-m", "A, described elsewhere"]);
    let op_before = fx.head_op();
    let a_commits = fx.commits(&a);
    assert_eq!(a_commits.len(), 1);

    let blocked = r.undo_edit_checked(&res.id, &you()).unwrap().unwrap_err();
    assert!(blocked.divergent.contains(&a), "{blocked:?}");
    assert!(!blocked.later_ops.is_empty(), "{blocked:?}");
    assert!(
        blocked.later_ops[0].changes.contains(&a),
        "the describe touched A: {blocked:?}"
    );
    assert!(blocked.suggestion.contains("jj op revert"), "{blocked:?}");
    // The plain API answers 409 undo-blocked with the ops listed.
    let e = r.undo_edit(&res.id, &you()).unwrap_err();
    assert_eq!((e.status, e.code), (409, "undo-blocked"));
    assert!(e.message.contains(&a[..12]), "{}", e.message);
    // Nothing was written: same op head, A still has one visible commit, edit still applied.
    assert_eq!(fx.head_op(), op_before);
    assert_eq!(fx.commits(&a), a_commits);
    assert_eq!(r.get_edit(&res.id).unwrap().status, "applied");
    assert!(fx.show(&a, "a.py").starts_with("line one\n"));
}

/// c8: undo stays refused when the revert would create a conflict that was not there at the
/// edit's opBefore (here: a later child edits the same line; reverting our amend of the parent
/// rebases that child onto the old line), and nothing is written.
#[test]
fn undo_that_would_make_a_new_conflict_is_blocked() {
    if !have_jj() {
        return;
    }
    let fx = Fx::new();
    let r = fx.review();
    let a = fx.change("A");
    let res = r.edit(amend(&a, 3, "line three"), &you()).unwrap();
    assert_eq!(res.status, "applied", "{res:?}");
    // Later: a new child of A rewrites the same line.
    fx.jj(&["new", "--no-edit", &a, "-m", "C"]);
    let c = fx.change("C");
    let res_c = r.edit(amend(&c, 3, "line three, again"), &you()).unwrap();
    assert_eq!(res_c.status, "applied", "{res_c:?}");
    assert!(r.list_conflicts(None).unwrap().is_empty());
    let op_before = fx.head_op();

    let blocked = r.undo_edit_checked(&res.id, &you()).unwrap().unwrap_err();
    assert!(
        blocked
            .conflicted
            .iter()
            .any(|x| x.change == c && x.path == "a.py"),
        "the revert would conflict C: {blocked:?}"
    );
    let e = r.undo_edit(&res.id, &you()).unwrap_err();
    assert_eq!((e.status, e.code), (409, "undo-blocked"));
    assert_eq!(fx.head_op(), op_before, "nothing written");
    assert_eq!(r.get_edit(&res.id).unwrap().status, "applied");
    assert!(r.list_conflicts(None).unwrap().is_empty());
}

#[test]
fn plain_undo_reverts_exactly_our_op() {
    if !have_jj() {
        return;
    }
    let fx = Fx::new();
    let r = fx.review();
    let a = fx.change("A");
    let before = fx.show(&a, "a.py");
    let res = r.edit(amend(&a, 2, "line two"), &you()).unwrap();
    assert_eq!(res.status, "applied");
    let u = r.undo_edit(&res.id, &you()).unwrap();
    assert_eq!(u.status, "undone");
    assert!(u.divergent.is_empty() && u.warnings.is_empty(), "{u:?}");
    assert_eq!(fx.show(&a, "a.py"), before);
    assert_eq!(fx.commits(&a).len(), 1);
    assert_eq!(fx.commits(&fx.change("B")).len(), 1);
}

/// ws2's `@` sits on A. Its unsnapshotted file survives an edit that rewrites `@` under it while
/// it is stale: it lands in a divergent sibling, the result says so and `workspace.recovered`
/// is emitted.
#[test]
fn stale_workspace_is_recovered_into_a_divergent_sibling() {
    if !have_jj() {
        return;
    }
    let fx = Fx::new();
    let a = fx.change("A");
    let ws2 = fx.dir.join("ws2");
    fx.jj(&[
        "workspace",
        "add",
        "--name",
        "ws2",
        "-r",
        &a,
        ws2.to_str().unwrap(),
    ]);
    let r = fx.review();
    // ws2 has a file jj has not seen yet.
    std::fs::write(ws2.join("draft.txt"), "unsaved\n").unwrap();
    // Rewrite A's tree from the default workspace without snapshotting ws2: ws2 goes stale.
    std::fs::write(fx.repo.join("c.py"), "c = 3\n").unwrap();
    fx.jj(&["squash", "--into", &a, "c.py"]);
    let ws2_change = fx.jj(&[
        "log",
        "--ignore-working-copy",
        "--no-graph",
        "-r",
        "ws2@",
        "-T",
        "change_id",
    ]);
    let c0 = r.events().cursor();

    let res = r.edit(amend(&a, 1, "line one"), &you()).unwrap();
    assert_eq!(res.status, "applied", "{res:?}");
    let w = res
        .workspaces
        .iter()
        .find(|w| w.name == "ws2")
        .unwrap_or_else(|| panic!("ws2 affected: {res:?}"));
    let sibling = w
        .recovered_into
        .clone()
        .unwrap_or_else(|| panic!("ws2 recovered: {res:?}"));
    assert!(w.updated, "{res:?}");
    assert!(res.divergent.contains(&ws2_change), "{res:?}");
    assert!(
        res.warnings.iter().any(|x| x.code == "workspace-recovered"),
        "{res:?}"
    );
    // The rescued file is in the sibling; ws2's disk now shows its new @.
    assert_eq!(fx.show(&sibling, "draft.txt"), "unsaved\n");
    assert!(
        std::fs::read_to_string(ws2.join("a.py"))
            .unwrap()
            .starts_with("line one\n")
    );
    assert_eq!(
        std::fs::read_to_string(ws2.join("c.py")).unwrap(),
        "c = 3\n"
    );
    let ev = r
        .events()
        .since(c0, None)
        .into_iter()
        .find_map(|(_, e)| match e {
            ServerEvent::WorkspaceRecovered {
                workspace,
                divergent_commit,
                note,
            } => Some((workspace, divergent_commit, note)),
            _ => None,
        })
        .expect("workspace.recovered");
    assert_eq!(ev.0, "ws2");
    assert_eq!(ev.1, sibling);
    assert!(ev.2.contains("divergent"), "{}", ev.2);
    // ws2 is usable again (not stale).
    fx.jj_in(&ws2, &["status"]);
}

#[test]
fn preview_warns_about_agents_attached_to_affected_workspaces() {
    if !have_jj() {
        return;
    }
    let fx = Fx::new();
    let a = fx.change("A");
    let ws2 = fx.dir.join("ws2");
    fx.jj(&[
        "workspace",
        "add",
        "--name",
        "ws2",
        "-r",
        &a,
        ws2.to_str().unwrap(),
    ]);
    let r = fx.review();
    let claude = Actor::Agent {
        name: "claude".into(),
        client: Some("claude-code".into()),
        workspace: Some("ws2".into()),
    };
    r.bind_session("claude", "ws2", &claude, "editing").unwrap();
    let mut req = amend(&a, 1, "line one");
    req.dry_run = true;
    let p = r.edit(req.clone(), &you()).unwrap();
    assert_eq!(p.status, "previewed");
    let w = p
        .warnings
        .iter()
        .find(|w| w.code == "agent-attached")
        .unwrap_or_else(|| panic!("{p:?}"));
    assert_eq!(w.workspace.as_deref(), Some("ws2"));
    assert_eq!(
        w.agent.as_ref().map(Actor::label).as_deref(),
        Some("claude@ws2")
    );
    assert!(w.message.contains("claude@ws2"), "{}", w.message);
    // The agent editing under its own workspace is not warned about itself.
    let own = r.edit(req, &claude).unwrap();
    assert!(own.warnings.is_empty(), "{own:?}");
    // Applying keeps the warning and the op description names the actor and its workspace.
    let applied = r.apply_edit(&own.id, &claude).unwrap();
    assert_eq!(applied.status, "applied", "{applied:?}");
    let desc = fx.jj(&[
        "op",
        "log",
        "--ignore-working-copy",
        "--no-graph",
        "-n1",
        "-T",
        "description",
    ]);
    assert!(desc.contains("(claude@ws2)"), "{desc}");
    assert_eq!(applied.actor.workspace(), Some("ws2"));
}

#[test]
fn apply_refuses_a_preview_that_no_longer_holds() {
    if !have_jj() {
        return;
    }
    let fx = Fx::new();
    let r = fx.review();
    let a = fx.change("A");
    // An unrelated op in between: the re-check passes and the edit applies.
    let mut req = amend(&a, 1, "line one");
    req.dry_run = true;
    let p = r.edit(req, &you()).unwrap();
    fx.jj(&["describe", "-r", "@", "-m", "B, renamed"]);
    let ok = r.apply_edit(&p.id, &you()).unwrap();
    assert_eq!(ok.status, "applied", "{ok:?}");

    // A new child of A appears after the preview: applying would rewrite one more change.
    let mut req = amend(&a, 3, "line three");
    req.dry_run = true;
    let p = r.edit(req, &you()).unwrap();
    assert_eq!(p.status, "previewed");
    fx.jj(&["new", "--no-edit", &a, "-m", "sibling of B"]);
    let op = fx.head_op();
    let stale = r.apply_edit(&p.id, &you()).unwrap();
    assert_eq!(stale.status, "rejected", "{stale:?}");
    let e = stale.error.unwrap();
    assert_eq!(e.code, "stale-base");
    assert!(
        e.message.contains("moved on") && e.message.contains("preview it again"),
        "{}",
        e.message
    );
    assert_eq!(fx.head_op(), op, "nothing written");
    assert!(!fx.show(&a, "a.py").contains("line three\nline 4"));
}

#[test]
fn a_busy_queue_answers_locked() {
    if !have_jj() {
        return;
    }
    let fx = Fx::new();
    let r = fx.review();
    let a = fx.change("A");
    // The first sync after our edit blocks inside the queue until the test opens the gate (not
    // a sleep: however slow the machine, the queue stays busy until the 409 has been seen).
    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
    let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
    let hook = std::sync::Mutex::new(Some((entered_tx, gate_rx)));
    r.add_op_hook(Box::new(move |_| {
        if let Some((entered, gate)) = hook.lock().unwrap().take() {
            entered.send(()).unwrap();
            // The bound only keeps a failing test from hanging.
            let _ = gate.recv_timeout(Duration::from_secs(120));
        }
        vec![]
    }));
    r.set_lock_timeout(Duration::from_millis(200));
    let r1 = r.clone();
    let a1 = a.clone();
    let t = std::thread::spawn(move || r1.edit(amend(&a1, 1, "line one"), &you()).unwrap());
    // The first edit's op is written and its sync now holds the queue, so the next preview is
    // taken at the head the apply will see.
    entered_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("first edit never reached its sync");
    let mut req = amend(&a, 5, "line five");
    req.dry_run = true;
    let p = r.edit(req, &you()).unwrap();
    let e = r.apply_edit(&p.id, &you()).unwrap_err();
    assert_eq!((e.status, e.code), (409, "locked"), "{e}");
    gate_tx.send(()).unwrap();
    assert_eq!(t.join().unwrap().status, "applied");
    // Once the queue is free the same edit applies (it was not marked failed).
    r.set_lock_timeout(Duration::from_secs(30));
    assert_eq!(r.apply_edit(&p.id, &you()).unwrap().status, "applied");
}
