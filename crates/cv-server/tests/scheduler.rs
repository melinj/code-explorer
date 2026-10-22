//! IndexScheduler (WORKSTREAMS B7, DESIGN §4.1/§5): after a jj operation that changes many files,
//! the view on screen is refreshed viewport-first. With a `view.state` focused on one window, the
//! first `view.invalidated` names only windows of changed files in the focus region (preceded by
//! `index.progress` with the view id); a second one names the rest. The final view equals a plain
//! one-pass refresh. Without a focus there is one pass and one event. Skipped when `jj` is not on
//! PATH.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use cv_contracts::review::ServerEvent;
use cv_server::index::Options;
use cv_server::routes::AppState;
use cv_server::scheduler::{self, Focus};
use serde_json::json;

const USER: &str = "[user]\nname = \"Test\"\nemail = \"test@example.com\"\n";
const FILES: usize = 96;

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

fn body(i: usize, rev: usize) -> String {
    let mut s = String::new();
    for f in 0..3 {
        s.push_str(&format!(
            "def f{i}_{f}(x):\n    y = x + {rev}\n    z = y * {f}\n    return z\n\n\n"
        ));
    }
    s
}

fn setup() -> (tempfile::TempDir, Arc<AppState>, Arc<cv_review::Review>) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), USER).unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    jj(&repo, &["git", "init"]);
    for i in 0..FILES {
        let p = repo.join(format!("pkg{}/m{i}.py", i % 6));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body(i, 0)).unwrap();
    }
    jj(&repo, &["commit", "-m", "base"]);
    let state = Arc::new(AppState::new(
        cv_vcs::JjRepo::open(&repo).unwrap(),
        Options { max_commits: 50 },
    ));
    let writer = cv_vcs::write::JjWriter::open_with(&repo, true, Some(USER)).unwrap();
    let review = cv_review::Review::open_with(writer, None).unwrap();
    state.set_review(review.clone());
    review.sync().unwrap();
    (tmp, state, review)
}

/// Rewrites every file and snapshots it as a new operation.
fn change_all(repo: &Path) {
    for i in 0..FILES {
        std::fs::write(repo.join(format!("pkg{}/m{i}.py", i % 6)), body(i, 1)).unwrap();
    }
    jj(repo, &["describe", "-m", "work"]);
}

fn invalidations(events: &[(u64, ServerEvent)]) -> Vec<Vec<String>> {
    events
        .iter()
        .filter_map(|(_, e)| match e {
            ServerEvent::ViewInvalidated { windows, .. } => {
                Some(windows.clone().unwrap_or_default())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn op_refresh_is_viewport_first() {
    if !have_jj() {
        return;
    }
    let (tmp, state, review) = setup();
    let repo = tmp.path().join("repo");
    let spec = json!({ "mode": "snapshot", "rev": "@" });
    let v0 = state.view_for_spec(&spec).unwrap();
    let vid = v0.id().to_string();
    // Look at the top-left corner of one window of pkg3/m3.py, 40 × 30 wu.
    let w = v0.files[v0.by_path["pkg3/m3.py"]].windows[0];
    let r = v0.windows[w].header.rect;
    let state_json = json!({
        "viewId": vid,
        "viewport": { "rect": { "x": r.x, "y": r.y, "w": 40.0, "h": 30.0 }, "zoom": 1.0, "tier": "L3" },
        "lens": "none",
        "followAgent": false,
    });
    review.set_view_state("ui-1", state_json);
    let focus = Focus::latest(&review).unwrap();
    assert_eq!(focus.view_id, vid);
    let all: Vec<usize> = (0..v0.files.len()).collect();
    let plan = scheduler::plan(&v0, &all, Some(&focus));
    assert_eq!(plan.first[0], v0.by_path["pkg3/m3.py"], "nearest first");
    assert!(plan.rest.len() >= scheduler::STAGE_MIN_FILES, "{plan:?}");
    let win_ids = |files: &[usize]| -> Vec<String> {
        let mut ids: Vec<String> = files
            .iter()
            .flat_map(|&fi| {
                v0.files[fi]
                    .windows
                    .iter()
                    .map(|&w| v0.windows[w].header.id.clone())
            })
            .collect();
        ids.sort();
        ids
    };

    let cursor = review.events().cursor();
    change_all(&repo);
    review.sync().unwrap();
    let events = review.events().since(cursor, None);
    let kinds: Vec<&str> = events.iter().map(|(_, e)| e.kind()).collect();
    let i_progress = kinds
        .iter()
        .position(|k| *k == "index.progress")
        .expect("progress");
    let i_inval = kinds.iter().position(|k| *k == "view.invalidated").unwrap();
    assert!(
        kinds[0] == "op.changed" && i_progress < i_inval,
        "{kinds:?}"
    );
    match &events[i_progress].1 {
        ServerEvent::IndexProgress {
            job,
            done,
            total,
            view_id,
        } => {
            assert_eq!(
                (job.as_str(), view_id.as_deref()),
                ("parse", Some(vid.as_str()))
            );
            assert_eq!((*done as usize, *total as usize), (plan.first.len(), FILES));
        }
        e => panic!("{e:?}"),
    }
    let inv = invalidations(&events);
    assert_eq!(inv.len(), 2, "two stages: {kinds:?}");
    let (mut first, mut rest) = (inv[0].clone(), inv[1].clone());
    first.sort();
    rest.sort();
    assert_eq!(first, win_ids(&plan.first), "stage 1 = the files in view");
    assert_eq!(rest, win_ids(&plan.rest), "stage 2 = everything else");

    // The staged result equals a plain refresh of the old view.
    let v1 = state.view_for_spec(&spec).unwrap();
    assert_eq!(v1.id(), vid);
    let r1 = review.vcs().reader().unwrap();
    let c1 = r1.resolve_one("@").unwrap();
    assert_eq!(v1.bootstrap.base.view.resolved.head, c1.commit);
    let (plain, _) = cv_server::review_api::refresh_view(
        &v0,
        &cv_server::index::registry(),
        &r1,
        &c1,
        &r1.op_id(),
    )
    .unwrap();
    for (a, b) in v1.windows.iter().zip(&plain.windows) {
        let (a, b) = (&a.header, &b.header);
        assert_eq!(
            (
                &a.id,
                &a.blob,
                &a.rev,
                a.lines.start,
                a.lines.end,
                a.rect.y,
                a.rect.h
            ),
            (
                &b.id,
                &b.blob,
                &b.rev,
                b.lines.start,
                b.lines.end,
                b.rect.y,
                b.rect.h
            )
        );
    }
    for (a, b) in v1.files.iter().zip(&plain.files) {
        assert_eq!((&a.path, &a.blob, &a.text), (&b.path, &b.blob, &b.text));
    }
}

#[test]
fn without_a_focus_the_refresh_is_one_pass() {
    if !have_jj() {
        return;
    }
    let (tmp, state, review) = setup();
    let repo = tmp.path().join("repo");
    let v0 = state
        .view_for_spec(&json!({ "mode": "snapshot", "rev": "@" }))
        .unwrap();
    let cursor = review.events().cursor();
    change_all(&repo);
    review.sync().unwrap();
    let events = review.events().since(cursor, None);
    let inv = invalidations(&events);
    assert_eq!(inv.len(), 1);
    let n: usize = v0.files.iter().map(|f| f.windows.len()).sum();
    assert_eq!(inv[0].len(), n);
    assert!(
        !events.iter().any(|(_, e)| e.kind() == "index.progress"),
        "no staged progress without a focus"
    );
}

#[test]
fn the_focused_view_is_refreshed_first() {
    let (tmp, state, _review) = if have_jj() { setup() } else { return };
    let _keep = tmp;
    let v = state
        .view_for_spec(&json!({ "mode": "snapshot", "rev": "@" }))
        .unwrap();
    let mut views = vec![("b".to_string(), v.clone())];
    let mut other = (*v).clone();
    other.bootstrap.base.view.id = "v_000000000000".into();
    views.push(("a".to_string(), Arc::new(other)));
    scheduler::order_views(&mut views, None);
    assert_eq!(views[0].1.id(), "v_000000000000");
    let focus = Focus {
        view_id: v.id().to_string(),
        rect: v.windows[0].header.rect,
    };
    scheduler::order_views(&mut views, Some(&focus));
    assert_eq!(views[0].1.id(), v.id());
}
