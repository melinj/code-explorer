//! B8 acceptance on CLI-built repos: suggestions amend exactly their lines, re-anchoring (moved,
//! changed → outdated with lastGood, abandoned), stale base, immutable targets, conflicts and the
//! event sequence (including external jj ops through the watcher within 2 s).
//! Skipped when `jj` is not on PATH.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cv_contracts::review::*;
use cv_review::{OpChange, Review, ThreadFilter};
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

    fn jj(&self, args: &[&str]) -> String {
        let out = Command::new("jj")
            .args(args)
            .current_dir(&self.repo)
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

/// Waits for `cond` (polling it); the 60 s bound only guards against a hang.
fn wait_until(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn you() -> Actor {
    Actor::human("you")
}

fn thread_on(r: &Review, rev: &str, path: &str, start: u32, end: u32, body: &str) -> Thread {
    r.create_thread(
        NewThread {
            anchor: NewAnchor {
                path: path.into(),
                rev: rev.into(),
                start,
                end: Some(end),
                side: None,
                cols: None,
            },
            body: body.into(),
            kind: Some("suggestion".into()),
            severity: None,
            assignee: None,
        },
        &you(),
    )
    .unwrap()
}

fn patch_req(
    target: &str,
    path: &str,
    start: u32,
    end: u32,
    text: &str,
    base: Option<String>,
) -> EditRequest {
    EditRequest {
        target: target.into(),
        files: vec![EditFile {
            path: path.into(),
            base_blob: base,
            content: None,
            patches: Some(vec![LinePatch {
                start,
                end,
                text: text.into(),
            }]),
        }],
        mode: "amend".into(),
        message: None,
        absorb_into: None,
        dry_run: true,
        origin: None,
    }
}

fn kinds(r: &Review, after: u64) -> Vec<&'static str> {
    r.events()
        .since(after, None)
        .iter()
        .map(|(_, e)| e.kind())
        .collect()
}

#[test]
fn suggestion_amends_exactly_its_lines_and_the_thread_follows() {
    if !have_jj() {
        return;
    }
    let fx = Fx::new();
    let r = fx.review();
    let a = fx.change("A");
    let t = thread_on(
        &r,
        &a,
        "a.py",
        5,
        6,
        "Merge these:\n```suggestion\nline five-six\n```",
    );
    assert_eq!(t.anchor.change, a);
    let before = fx.show(&a, "a.py");
    let c0 = r.events().cursor();
    let res = r.apply_suggestion(&t.id, None, &you()).unwrap();
    assert_eq!(res.status, "applied", "{res:?}");
    assert!(res.rewritten.contains(&a));
    let after = fx.show(&a, "a.py");
    let (b, n): (Vec<&str>, Vec<&str>) = (before.lines().collect(), after.lines().collect());
    assert_eq!(n.len(), b.len() - 1);
    assert_eq!(&n[..4], &b[..4]);
    assert_eq!(n[4], "line five-six");
    assert_eq!(&n[5..], &b[6..], "only the anchored lines changed");
    let t = r.get_thread(&t.id).unwrap();
    assert_eq!((t.anchor.start, t.anchor.end), (5, 5));
    assert_ne!(t.status, "outdated");
    assert_eq!(t.produced_edits, vec![res.id.clone()]);
    let ks = kinds(&r, c0);
    let pos = |k| ks.iter().position(|x| *x == k).unwrap_or(usize::MAX);
    assert!(pos("edit.updated") < pos("op.changed"), "{ks:?}");
    assert!(pos("op.changed") < pos("thread.upserted"), "{ks:?}");
    assert!(ks.contains(&"workspace.updated"), "{ks:?}");
    // The re-anchoring pass reports itself as index.progress job "reanchor": 0/1, then 1/1.
    let re: Vec<(u32, u32)> = r
        .events()
        .since(c0, None)
        .into_iter()
        .filter_map(|(_, e)| match e {
            ServerEvent::IndexProgress {
                job, done, total, ..
            } if job == "reanchor" => Some((done, total)),
            _ => None,
        })
        .collect();
    assert_eq!(re, vec![(0, 1), (1, 1)], "{ks:?}");

    // Undo reverts exactly that operation.
    let u = r.undo_edit(&res.id, &you()).unwrap();
    assert_eq!(u.status, "undone");
    assert_eq!(fx.show(&a, "a.py"), before);
}

#[test]
fn reanchoring_moves_outdates_and_handles_abandon() {
    if !have_jj() {
        return;
    }
    let fx = Fx::new();
    let r = fx.review();
    let a = fx.change("A");
    let moved = thread_on(&r, &a, "a.py", 10, 11, "about 10-11");
    let changed = thread_on(&r, &a, "a.py", 15, 15, "about 15");
    let old_commit = changed.anchor.commit.clone();
    // Insert two lines at the top and rewrite line 15, in one amend of A.
    let mut req = patch_req(&a, "a.py", 1, 0, "new 1\nnew 2", None);
    req.files[0].patches.as_mut().unwrap().push(LinePatch {
        start: 15,
        end: 15,
        text: "line 15 rewritten".into(),
    });
    req.dry_run = false;
    let res = r.edit(req, &you()).unwrap();
    assert_eq!(res.status, "applied", "{res:?}");
    let m = r.get_thread(&moved.id).unwrap();
    assert_eq!(
        (m.anchor.start, m.anchor.end),
        (12, 13),
        "moved with its lines"
    );
    assert_eq!(m.status, "open");
    assert_ne!(m.anchor.commit, moved.anchor.commit);
    let c = r.get_thread(&changed.id).unwrap();
    assert_eq!(c.status, "outdated");
    let o = c.outdated.unwrap();
    assert_eq!(o.reason, "lines-changed");
    assert_eq!(o.last_good, old_commit);

    // An external `jj abandon` of B: B's thread is outdated (change-abandoned).
    let b = fx.change("B");
    let tb = thread_on(&r, &b, "b.py", 1, 1, "about b");
    fx.jj(&["abandon", &b]);
    r.sync().unwrap();
    let tb = r.get_thread(&tb.id).unwrap();
    assert_eq!(tb.status, "outdated");
    assert_eq!(tb.outdated.unwrap().reason, "change-abandoned");
}

/// a1: an agent answers a thread with one edit of several patches (and a reply). The thread's
/// anchor moves onto the new text in the new commit, so it stays addressed through the next
/// operations (a working-copy snapshot, a describe), never flipping to outdated.
#[test]
fn multi_patch_answer_keeps_the_thread_addressed_through_later_ops() {
    if !have_jj() {
        return;
    }
    let fx = Fx::new();
    let r = fx.review();
    let b = fx.change("B");
    // The thread is on the working copy's change (B, @), lines 5-6 of a.py.
    let t = thread_on(&r, &b, "a.py", 5, 6, "merge these two lines");
    let mut req = patch_req(&b, "a.py", 2, 2, "two\nTWO", None);
    req.files[0].patches.as_mut().unwrap().extend([
        LinePatch {
            start: 5,
            end: 6,
            text: "line five-six\n# merged".into(),
        },
        LinePatch {
            start: 15,
            end: 15,
            text: "line 15 rewritten".into(),
        },
    ]);
    req.dry_run = false;
    req.origin = Some(EditOrigin {
        thread: Some(t.id.clone()),
    });
    let res = r.edit(req, &you()).unwrap();
    assert_eq!(res.status, "applied", "{res:?}");
    let after = r.get_thread(&t.id).unwrap();
    assert!(after.outdated.is_none(), "{after:?}");
    assert_ne!(after.status, "outdated");
    let text = fx.show(&b, "a.py");
    let lines: Vec<&str> = text.lines().collect();
    let anchored = |t: &Thread| {
        let text = fx.show(&t.anchor.commit, "a.py");
        text.lines()
            .skip(t.anchor.start as usize - 1)
            .take((t.anchor.end - t.anchor.start + 1) as usize)
            .map(String::from)
            .collect::<Vec<_>>()
    };
    assert_eq!(anchored(&after), vec!["line five-six", "# merged"]);
    assert_eq!(lines[after.anchor.start as usize - 1], "line five-six");
    // The agent's reply marks it addressed (as MCP edit_code {thread, patches, reply} does).
    r.reply(&t.id, "merged them", Some("addressed"), &Actor::External)
        .unwrap();
    let commit_after_edit = r.get_thread(&t.id).unwrap().anchor.commit;

    // A working-copy snapshot: a new file on disk, `jj status` snapshots @ (a new commit of B).
    std::fs::write(fx.repo.join("c.py"), "c = 3\n").unwrap();
    fx.jj(&["status"]);
    r.sync().unwrap();
    let t1 = r.get_thread(&t.id).unwrap();
    assert_eq!(t1.status, "addressed", "{t1:?}");
    assert!(t1.outdated.is_none());
    assert_ne!(t1.anchor.commit, commit_after_edit, "followed the snapshot");
    assert_eq!(anchored(&t1), vec!["line five-six", "# merged"]);

    // One more op (a describe) and an edit above the anchor: still addressed, moved with its lines.
    std::fs::write(
        fx.repo.join("a.py"),
        format!("# header\n{}", fx.show(&b, "a.py")),
    )
    .unwrap();
    fx.jj(&["describe", "-m", "B2"]);
    r.sync().unwrap();
    let t2 = r.get_thread(&t.id).unwrap();
    assert_eq!(t2.status, "addressed", "{t2:?}");
    assert_eq!(anchored(&t2), vec!["line five-six", "# merged"]);
    assert_eq!(t2.anchor.start, t1.anchor.start + 1);
}

/// a1/reply: a thread a human says still applies (outdated → open) is re-anchored on the change's
/// current commit, so the next operation does not outdate it again.
#[test]
fn reopening_an_outdated_thread_reanchors_it() {
    if !have_jj() {
        return;
    }
    let fx = Fx::new();
    let r = fx.review();
    let a = fx.change("A");
    let t = thread_on(&r, &a, "a.py", 15, 15, "about 15");
    let mut req = patch_req(&a, "a.py", 15, 15, "line 15 rewritten", None);
    req.dry_run = false;
    assert_eq!(r.edit(req, &you()).unwrap().status, "applied");
    let o = r.get_thread(&t.id).unwrap();
    assert_eq!(o.status, "outdated");
    let reopened = r.patch_thread(&t.id, Some("open"), None, &you()).unwrap();
    assert!(reopened.outdated.is_none());
    assert_ne!(
        reopened.anchor.commit, o.anchor.commit,
        "on the current commit"
    );
    // Another rewrite of A (lines inserted at the top): the thread moves, it is not outdated.
    let mut req = patch_req(&a, "a.py", 1, 0, "new 1", None);
    req.dry_run = false;
    assert_eq!(r.edit(req, &you()).unwrap().status, "applied");
    let t2 = r.get_thread(&t.id).unwrap();
    assert_eq!(t2.status, "open", "{t2:?}");
    assert_eq!((t2.anchor.start, t2.anchor.end), (16, 16));
}

/// b11: a divergent change (the same change rewritten by two concurrent operations) re-anchors
/// to the commit whose evolog holds the anchor's commit, the same way on every sync.
#[test]
fn divergent_change_reanchors_along_its_evolog() {
    if !have_jj() {
        return;
    }
    let fx = Fx::new();
    let r = fx.review();
    let a = fx.change("A");
    let t = thread_on(&r, &a, "a.py", 10, 10, "about 10");
    let op0 = fx
        .jj(&["op", "log", "--no-graph", "-n1", "-T", "id"])
        .trim()
        .to_string();
    // op 1: A rewritten (described); the thread follows to that commit.
    fx.jj(&["describe", "-r", &a, "-m", "A v2", "--ignore-working-copy"]);
    r.sync().unwrap();
    let c2 = r.get_thread(&t.id).unwrap().anchor.commit;
    assert_ne!(c2, t.anchor.commit);
    // A concurrent op from before op 1 rewrites the original commit too: A is now divergent.
    fx.jj(&[
        "--at-op",
        &op0,
        "describe",
        "-r",
        &a,
        "-m",
        "A v3",
        "--ignore-working-copy",
    ]);
    fx.jj(&["log", "--ignore-working-copy", "-r", "all()"]); // merges the op heads
    let visible = fx.jj(&[
        "log",
        "--ignore-working-copy",
        "--no-graph",
        "-r",
        &format!("change_id({a})"),
        "-T",
        "commit_id ++ \"\\n\"",
    ]);
    assert_eq!(visible.lines().count(), 2, "A is divergent: {visible}");
    for _ in 0..3 {
        r.sync().unwrap();
        let now = r.get_thread(&t.id).unwrap();
        assert_eq!(now.anchor.commit, c2, "stays on the evolog successor");
        assert_eq!(now.status, "open");
    }
}

#[test]
fn stale_base_merges_or_rejects_and_immutable_only_branches() {
    if !have_jj() {
        return;
    }
    let fx = Fx::new();
    let r = fx.review();
    let a = fx.change("A");
    let base_blob = r
        .vcs()
        .file_at(&r.vcs().resolve_rev(&a).unwrap().commit, "a.py")
        .unwrap()
        .unwrap()
        .blob;
    let mut first = patch_req(&a, "a.py", 2, 2, "LINE 2", base_blob.clone());
    first.dry_run = false;
    assert_eq!(r.edit(first, &you()).unwrap().status, "applied");
    // Same stale base, a different line: rebased onto the new text with a three-way merge.
    let mut second = patch_req(&a, "a.py", 9, 9, "LINE 9", base_blob.clone());
    second.dry_run = false;
    assert_eq!(r.edit(second, &you()).unwrap().status, "applied");
    let text = fx.show(&a, "a.py");
    assert!(text.contains("LINE 2\n") && text.contains("LINE 9\n"));
    // Same stale base, the same line: stale-base.
    let clash = r
        .edit(patch_req(&a, "a.py", 2, 2, "other 2", base_blob), &you())
        .unwrap();
    assert_eq!(clash.status, "rejected");
    assert_eq!(clash.error.unwrap().code, "stale-base");

    // Immutable targets: amend is rejected, branch creates a change and rewrites nothing.
    fx.jj(&["bookmark", "create", "main", "-r", &a]);
    let w = JjWriter::open_with(
        &fx.repo,
        true,
        Some(&format!(
            "{USER}[revset-aliases]\n'immutable_heads()' = 'bookmarks(exact:\"main\")'\n"
        )),
    )
    .unwrap();
    let r = Review::open_with(w, None).unwrap();
    let rej = r
        .edit(patch_req(&a, "a.py", 1, 1, "x", None), &you())
        .unwrap();
    assert_eq!(rej.error.unwrap().code, "immutable");
    let mut br = patch_req(&a, "a.py", 1, 1, "x", None);
    br.mode = "branch".into();
    br.message = Some("fix on top".into());
    br.dry_run = false;
    let res = r.edit(br, &you()).unwrap();
    assert_eq!(res.status, "applied");
    assert!(res.created.is_some() && res.rewritten.is_empty());
}

#[test]
fn conflicts_are_tracked_resolved_and_reported() {
    if !have_jj() {
        return;
    }
    let fx = Fx::new();
    // B also edits line 3 of a.py.
    let body: String = (1..=20)
        .map(|i| {
            if i == 3 {
                "B3\n".into()
            } else {
                format!("line {i}\n")
            }
        })
        .collect();
    std::fs::write(fx.repo.join("a.py"), body).unwrap();
    fx.jj(&["status"]); // snapshot: previews read the repo, never the disk
    let r = fx.review();
    let a = fx.change("A");
    let b = fx.change("B");
    let mut req = patch_req(&a, "a.py", 3, 3, "A3", None);
    let preview = r.edit(req.clone(), &you()).unwrap();
    assert!(preview.predicted);
    assert!(
        preview.conflicts.iter().any(|c| c.change == b),
        "{preview:?}"
    );
    req.dry_run = false;
    let c0 = r.events().cursor();
    let applied = r.edit(req, &you()).unwrap();
    let key = |e: &EditResult| -> Vec<(String, String)> {
        e.conflicts
            .iter()
            .map(|c| (c.change.clone(), c.path.clone()))
            .collect()
    };
    assert_eq!(
        key(&preview),
        key(&applied),
        "preview equals the applied result"
    );
    let evs = r.events().since(c0, None);
    let added = evs.iter().find_map(|(_, e)| match e {
        ServerEvent::ConflictsChanged { added, .. } => Some(added.clone()),
        _ => None,
    });
    assert_eq!(
        added.unwrap()[0].introduced_by.as_deref(),
        Some(applied.id.as_str())
    );
    let list = r.list_conflicts(None).unwrap();
    assert_eq!(list.len(), 1);
    let d = r.conflict_detail(&list[0].commit, "a.py").unwrap();
    assert!(d.materialized.contains("<<<<<<<"));
    assert_eq!(d.regions.len(), 1);
    assert_eq!(d.record.introduced_by.as_deref(), Some(applied.id.as_str()));

    let resolved_text: String = (1..=20)
        .map(|i| {
            if i == 3 {
                "AB3\n".into()
            } else {
                format!("line {i}\n")
            }
        })
        .collect();
    let c1 = r.events().cursor();
    let res = r
        .resolve_conflict(&list[0].commit, "a.py", &resolved_text, &you())
        .unwrap();
    assert_eq!(res.status, "applied");
    assert!(r.list_conflicts(None).unwrap().is_empty());
    assert_eq!(fx.show(&b, "a.py"), resolved_text);
    assert!(r.events().since(c1, None).iter().any(|(_, e)| matches!(
        e,
        ServerEvent::ConflictsChanged { resolved, .. } if resolved.len() == 1
    )));

    // Undoing the resolution brings back the conflict that existed at its opBefore: not blocked.
    let c2 = r.events().cursor();
    let undone = r
        .undo_edit_checked(&res.id, &you())
        .unwrap()
        .expect("undo of a resolution is not undo-blocked");
    assert_eq!(undone.status, "undone");
    let back = r.list_conflicts(None).unwrap();
    assert_eq!(back.len(), 1, "{back:?}");
    assert_eq!(
        back[0].first_seen_op, list[0].first_seen_op,
        "keeps its first sighting"
    );
    let added: Vec<ConflictRecord> = r
        .events()
        .since(c2, None)
        .into_iter()
        .flat_map(|(_, e)| match e {
            ServerEvent::ConflictsChanged { added, .. } => added,
            _ => vec![],
        })
        .collect();
    assert_eq!(added.len(), 1, "{added:?}");
    assert_eq!(
        added[0].first_seen_op, back[0].first_seen_op,
        "conflicts.changed added == GET /conflicts for a reopened conflict"
    );
    assert_eq!(added[0].introduced_by, back[0].introduced_by);
}

/// review-flow on a real repo: take a side in one of two conflicted files of `@`'s change (which has
/// a descendant with the same conflicts), then leave the other alone. The other one must stay
/// listed by every GET /conflicts (also those that race the resolution) and must never be announced
/// as resolved, by the resolution itself, by the watcher's sync or by a later external snapshot.
#[test]
fn resolving_one_conflict_never_drops_the_other() {
    if !have_jj() {
        return;
    }
    let fx = Fx::new();
    let lines = |tag: &str| -> String {
        (1..=20)
            .map(|i| {
                if i == 3 {
                    format!("{tag}3\n")
                } else {
                    format!("line {i}\n")
                }
            })
            .collect()
    };
    // C edits line 3 of a.py and b.py; D (on top, the working copy) touches c.py only.
    fx.jj(&["new", "-m", "base"]);
    std::fs::write(fx.repo.join("b.py"), lines("line ")).unwrap();
    fx.jj(&["new", "-m", "C"]);
    std::fs::write(fx.repo.join("a.py"), lines("C")).unwrap();
    std::fs::write(fx.repo.join("b.py"), lines("C")).unwrap();
    fx.jj(&["new", "-m", "D"]);
    std::fs::write(fx.repo.join("c.py"), "z = 3\n").unwrap();
    fx.jj(&["describe", "-m", "D"]);
    let c = fx.change("C");
    let d = fx.change("D");
    // An upstream change under C rewrites the same lines differently: C and D conflict in both.
    fx.jj(&["new", "--insert-before", &c, "-m", "U"]);
    std::fs::write(fx.repo.join("a.py"), lines("U")).unwrap();
    std::fs::write(fx.repo.join("b.py"), lines("U")).unwrap();
    fx.jj(&["describe", "-m", "U"]);
    fx.jj(&["edit", &d]);
    let r = fx.review();
    r.start_watcher(Duration::from_millis(50));
    let keys = |l: &[ConflictRecord]| -> std::collections::BTreeSet<(String, String)> {
        l.iter()
            .map(|x| (x.change.clone(), x.path.clone()))
            .collect()
    };
    let before = keys(&r.list_conflicts(None).unwrap());
    let b_keys: Vec<(String, String)> =
        vec![(c.clone(), "b.py".into()), (d.clone(), "b.py".into())];
    for k in &b_keys {
        assert!(before.contains(k), "{k:?} not listed: {before:?}");
    }
    assert!(before.contains(&(c.clone(), "a.py".into())), "{before:?}");

    // GET /conflicts racing the resolution, as the UI's polls do.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let polled = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let poller = {
        let r = r.clone();
        let stop = stop.clone();
        let polled = polled.clone();
        let b_keys = b_keys.clone();
        std::thread::spawn(move || {
            let (mut polls, mut misses) = (0u32, Vec::new());
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                if let Ok(l) = r.list_conflicts(None) {
                    polls += 1;
                    polled.store(polls, std::sync::atomic::Ordering::Relaxed);
                    let now = keys(&l);
                    for k in &b_keys {
                        if !now.contains(k) {
                            misses.push(now.clone());
                        }
                    }
                }
            }
            (polls, misses)
        })
    };
    let c0 = r.events().cursor();
    let detail = r.conflict_detail(&c, "a.py").unwrap();
    let side = &detail.regions[0].sides[0].text;
    let text = lines("x").replace("x3\n", &format!("{side}\n"));
    let res = r.resolve_conflict(&c, "a.py", &text, &you()).unwrap();
    assert_eq!(res.status, "applied", "{res:?}");
    // An external jj command snapshots the working copy (D, rewritten by the resolution).
    fx.jj(&["status"]);
    // Sync (as the watcher does) until the service has caught up with that operation, and keep
    // going for some rounds and polls more while the poller races it — no fixed time window.
    let head = fx.head_op();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut rounds = 0;
    while r.op() != head || rounds < 10 || polled.load(std::sync::atomic::Ordering::Relaxed) < 10 {
        assert!(Instant::now() < deadline, "never caught up with {head}");
        std::thread::sleep(Duration::from_millis(20));
        r.sync().unwrap();
        rounds += 1;
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let (polls, misses) = poller.join().unwrap();
    r.stop_watcher();
    assert!(polls > 0);
    assert!(
        misses.is_empty(),
        "b.py dropped from {} of {polls} listings: {:?}",
        misses.len(),
        misses.first()
    );
    let after = keys(&r.list_conflicts(None).unwrap());
    for k in &b_keys {
        assert!(after.contains(k), "{k:?} not listed after: {after:?}");
    }
    assert!(!after.iter().any(|(_, p)| p == "a.py"), "{after:?}");
    let resolved: Vec<(String, String)> = r
        .events()
        .since(c0, None)
        .into_iter()
        .flat_map(|(_, e)| match e {
            ServerEvent::ConflictsChanged { resolved, .. } => resolved,
            _ => vec![],
        })
        .map(|x| (x.change, x.path))
        .collect();
    assert!(
        resolved.iter().all(|(_, p)| p == "a.py") && !resolved.is_empty(),
        "announced as resolved: {resolved:?}"
    );
}

#[test]
fn external_op_yields_op_changed_then_view_invalidated() {
    if !have_jj() {
        return;
    }
    let fx = Fx::new();
    let r = fx.review();
    r.add_op_hook(Box::new(|c: &OpChange| {
        vec![ServerEvent::ViewInvalidated {
            view_id: "v_test".into(),
            layout_version: 1,
            reason: "op".into(),
            windows: Some(vec![]),
            tiles: None,
        }]
        .into_iter()
        .filter(|_| !c.to_op.is_empty())
        .collect()
    }));
    r.start_watcher(Duration::from_millis(100));
    wait_until("the watcher is ready", || r.watcher_ready());
    let c0 = r.events().cursor();
    fx.jj(&["new", "-m", "external"]);
    // Delivered by the watcher (notify, or its 1 s safety poll); the bound is only a hang guard.
    let (cur, evs) = r.events().read(
        Some(c0),
        Some(&["view.invalidated".into()]),
        Duration::from_secs(60),
    );
    assert!(!evs.is_empty(), "no view.invalidated");
    let all = kinds(&r, c0);
    assert_eq!(&all[..2], &["op.changed", "view.invalidated"], "{all:?}");
    match &r.events().since(c0, None)[0].1 {
        ServerEvent::OpChanged { actor, .. } => assert_eq!(*actor, Actor::External),
        e => panic!("{e:?}"),
    }
    assert!(cur > c0);
    // Reconnect with a cursor: everything after it is replayed.
    assert_eq!(r.events().since(c0, None).len(), all.len());
    r.stop_watcher();
    assert!(r.list_threads(&ThreadFilter::default()).unwrap().is_empty());
}

#[test]
fn notify_wakes_the_watcher_without_polling() {
    if !have_jj() {
        return;
    }
    let fx = Fx::new();
    let r = fx.review();
    // A poll interval (one hour) far beyond the read's bound: only a notify event can deliver
    // op.changed before it, however slow the machine.
    r.start_watcher(Duration::from_secs(3600));
    // The watcher's first check (which would also catch an op) is done and notify is watching.
    wait_until("the watcher is ready", || r.watcher_ready());
    let c0 = r.events().cursor();
    fx.jj(&["new", "-m", "external"]);
    let (_, evs) = r.events().read(
        Some(c0),
        Some(&["op.changed".into()]),
        Duration::from_secs(120),
    );
    assert!(!evs.is_empty(), "no op.changed");
    r.stop_watcher();
}
