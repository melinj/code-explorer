//! `IndexScheduler` (WORKSTREAMS B7, DESIGN §4.1 and §5): index work after a jj operation is done
//! viewport-first.
//!
//! The UI reports what it shows with `view.state` (throttled to 4 Hz, kept by the review service).
//! The latest one is the scheduler's [`Focus`]: a view id and the world rectangle on screen. When
//! an operation moves snapshot views, the op hook ([`crate::review_api::refresh_views`]) asks the
//! scheduler for
//!
//! - the **order of views**: the focused view first, then the others (oldest first), so the view
//!   on screen is refreshed and invalidated before views nobody looks at;
//! - a **staged plan for each view** ([`plan`]): changed files with a window in the focus region
//!   (the viewport grown by one viewport on every side: the client's predicted viewport, §5) come
//!   first, nearest the viewport centre first. When enough work is left outside that region the
//!   refresh runs in two stages ([`refresh_staged`]): stage 1 re-parses the files in view,
//!   publishes the view and emits `view.invalidated` for their windows plus `index.progress` with
//!   the view id; stage 2 does everything else (other changed files, deleted and new files) and
//!   emits the rest. The client repaints what it shows while the rest is still parsing.
//!
//! Both stages run inside the op hook, so `Review::sync` still returns only after the whole
//! refresh: a read after a write (HTTP or MCP) never sees a half-refreshed view.
//!
//! Not scheduled here (see WORKSTREAMS B7): the first full index of a spec. Its layout is global
//! (cluster places depend on every file), and the UI has no viewport before the bootstrap, so
//! there is nothing to put first; the client side of §5 (tile and batch requests by distance to
//! the predicted viewport, at most 6 in flight) is F2's.

use std::collections::HashMap;
use std::sync::Arc;

use cv_contracts::Rect;
use cv_contracts::review::ServerEvent;
use cv_vcs::{CommitInfo, JjRepo};
use serde_json::Value;

use crate::index::View;

/// Stage only when at least this many changed files lie outside the focus region (or
/// [`STAGE_MIN_BYTES`] of their text). Staging costs one more copy of the view and tile grid
/// (about 0.25 s on a 5.6k-file repo, 2k windows): below this, one pass is about as fast.
pub const STAGE_MIN_FILES: usize = 64;
/// See [`STAGE_MIN_FILES`].
pub const STAGE_MIN_BYTES: usize = 1024 * 1024;

/// What the human is looking at, from the latest `view.state` (docs/contracts/ws.ts `ViewState`).
#[derive(Clone, Debug, PartialEq)]
pub struct Focus {
    pub view_id: String,
    /// World-space rectangle on screen.
    pub rect: Rect,
}

impl Focus {
    /// The focus of a `ViewState` JSON value; `None` without a view id or a usable viewport.
    pub fn from_view_state(state: &Value) -> Option<Focus> {
        let view_id = state.get("viewId")?.as_str()?.to_string();
        let r = state.get("viewport")?.get("rect")?;
        let num = |k: &str| r.get(k).and_then(Value::as_f64).filter(|x| x.is_finite());
        let rect = Rect {
            x: num("x")?,
            y: num("y")?,
            w: num("w")?.max(0.0),
            h: num("h")?.max(0.0),
        };
        Some(Focus { view_id, rect })
    }

    /// The latest `view.state` the review service holds (any UI session).
    pub fn latest(review: &cv_review::Review) -> Option<Focus> {
        review
            .view_state(None)
            .and_then(|(_, _, state)| Focus::from_view_state(&state))
    }

    /// The viewport grown by one viewport on every side: what the client prefetches (§5).
    pub fn region(&self) -> Rect {
        let r = &self.rect;
        Rect {
            x: r.x - r.w,
            y: r.y - r.h,
            w: r.w * 3.0,
            h: r.h * 3.0,
        }
    }

    /// Distance in world units from `r` to the viewport centre (0 when `r` contains it).
    pub fn distance(&self, r: &Rect) -> f64 {
        let (cx, cy) = (
            self.rect.x + self.rect.w / 2.0,
            self.rect.y + self.rect.h / 2.0,
        );
        let dx = (r.x - cx).max(cx - (r.x + r.w)).max(0.0);
        let dy = (r.y - cy).max(cy - (r.y + r.h)).max(0.0);
        dx.hypot(dy)
    }
}

/// Snapshot views in refresh order: the focused one first, then by id (stable).
pub fn order_views(views: &mut [(String, Arc<View>)], focus: Option<&Focus>) {
    views.sort_by(|a, b| a.1.id().cmp(b.1.id()));
    if let Some(f) = focus {
        views.sort_by_key(|(_, v)| v.id() != f.view_id);
    }
}

/// A staged refresh plan for one view: file indices (into `View::files`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    /// Changed files with a window in the focus region, nearest first. Empty when the view is
    /// not focused.
    pub first: Vec<usize>,
    /// The other changed files, nearest first.
    pub rest: Vec<usize>,
}

impl Plan {
    /// Whether a refresh should publish `first` on its own before doing `rest`.
    pub fn staged(&self, rest_bytes: usize) -> bool {
        !self.first.is_empty()
            && (self.rest.len() >= STAGE_MIN_FILES || rest_bytes >= STAGE_MIN_BYTES)
    }
}

/// Orders the `changed` files of `v` viewport-first (see the module docs).
pub fn plan(v: &View, changed: &[usize], focus: Option<&Focus>) -> Plan {
    let Some(f) = focus.filter(|f| f.view_id == v.id()) else {
        return Plan {
            first: vec![],
            rest: changed.to_vec(),
        };
    };
    let region = f.region();
    let mut ranked: Vec<(bool, f64, usize)> = changed
        .iter()
        .map(|&fi| {
            let rects = v.files[fi]
                .windows
                .iter()
                .map(|&w| &v.windows[w].header.rect);
            let near = rects.clone().any(|r| r.intersects(&region));
            let d = rects.map(|r| f.distance(r)).fold(f64::INFINITY, f64::min);
            (near, d, fi)
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.total_cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut p = Plan::default();
    for (near, _, fi) in ranked {
        if near { &mut p.first } else { &mut p.rest }.push(fi);
    }
    p
}

/// Files of `v` whose blob at `tree` differs (deleted and new files are not listed).
pub fn changed_files(v: &View, tree: &HashMap<String, String>) -> Vec<usize> {
    (0..v.files.len())
        .filter(|&fi| {
            tree.get(&v.files[fi].path)
                .is_some_and(|b| *b != v.files[fi].blob)
        })
        .collect()
}

/// [`crate::review_api::refresh_view`], viewport-first: when the [`plan`] is worth staging, the
/// files in view are refreshed first and handed to `publish` (the intermediate view and its
/// changed window ids) before the rest. Returns the final view and the window ids changed after
/// the intermediate publish (all of them when nothing was staged).
pub fn refresh_staged(
    old: &View,
    reg: &cv_lang::Registry,
    repo: &JjRepo,
    commit: &CommitInfo,
    op: &str,
    focus: Option<&Focus>,
    publish: &mut dyn FnMut(Arc<View>, Vec<String>, (usize, usize)),
) -> Result<(View, Vec<String>), String> {
    let full = || crate::review_api::refresh_view(old, reg, repo, commit, op);
    if focus.is_none_or(|f| f.view_id != old.id()) {
        return full();
    }
    let tree: HashMap<String, String> = repo
        .list_tree(&commit.commit)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|e| (e.path, e.blob))
        .collect();
    let changed = changed_files(old, &tree);
    let p = plan(old, &changed, focus);
    let rest_bytes: usize = p.rest.iter().map(|&fi| old.files[fi].text.len()).sum();
    if !p.staged(rest_bytes) {
        return full();
    }
    // Stage 1: the files in view. The view keeps its old `resolved` head, so stage 2 (a plain
    // refresh) skips them (same blob) and does everything else.
    let t = std::time::Instant::now();
    let mut v = old.clone();
    let mut ids = Vec::new();
    for &fi in &p.first {
        let path = v.files[fi].path.clone();
        let blob = tree[&path].clone();
        let bytes = repo.read_blob(&path, &blob).map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        ids.extend(crate::review_api::replace_file(
            &mut v,
            fi,
            reg,
            Some(repo.root()),
            blob,
            text,
        ));
        for &w in &v.files[fi].windows {
            let h = &mut v.windows[w].header;
            h.rev = commit.commit.clone();
            h.change = commit.change.clone();
        }
    }
    crate::review_api::finish_refresh(&mut v, old, true);
    let total = changed.len();
    let v = Arc::new(v);
    publish(v.clone(), ids, (p.first.len(), total));
    let t1 = t.elapsed();
    let out = crate::review_api::refresh_view(&v, reg, repo, commit, op);
    eprintln!(
        "  refresh {} viewport-first: {} of {total} changed files in view after {t1:.0?}, the rest after {:.0?}",
        old.id(),
        p.first.len(),
        t.elapsed()
    );
    out
}

/// The events of an intermediate publish: the windows refreshed so far and the progress.
pub fn stage_events(v: &View, windows: Vec<String>, done: usize, total: usize) -> Vec<ServerEvent> {
    vec![
        ServerEvent::IndexProgress {
            job: "parse".into(),
            done: done as u32,
            total: total as u32,
            view_id: Some(v.id().to_string()),
        },
        ServerEvent::ViewInvalidated {
            view_id: v.id().to_string(),
            layout_version: v.bootstrap.base.view.layout_version,
            reason: "op".into(),
            windows: Some(windows),
            tiles: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn focus_reads_view_state() {
        let s = json!({ "viewId": "v_1", "viewport": { "rect": { "x": 10, "y": 20, "w": 100, "h": 50 }, "zoom": 1, "tier": "L3" }, "lens": "none", "followAgent": false });
        let f = Focus::from_view_state(&s).unwrap();
        assert_eq!(f.view_id, "v_1");
        assert_eq!(
            (f.rect.x, f.rect.y, f.rect.w, f.rect.h),
            (10.0, 20.0, 100.0, 50.0)
        );
        let g = f.region();
        assert_eq!((g.x, g.y, g.w, g.h), (-90.0, -30.0, 300.0, 150.0));
        assert!(Focus::from_view_state(&json!({ "viewId": "v_1" })).is_none());
        assert!(
            Focus::from_view_state(&json!({ "viewport": { "rect": s["viewport"]["rect"] } }))
                .is_none()
        );
    }

    #[test]
    fn distance_is_zero_over_the_centre_and_grows_outside() {
        let f = Focus {
            view_id: "v".into(),
            rect: Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            },
        };
        let at = |x: f64, y: f64| Rect {
            x,
            y,
            w: 10.0,
            h: 10.0,
        };
        assert_eq!(f.distance(&at(45.0, 45.0)), 0.0);
        assert_eq!(f.distance(&at(60.0, 45.0)), 10.0);
        assert!(f.distance(&at(500.0, 500.0)) > f.distance(&at(200.0, 50.0)));
    }

    #[test]
    fn staging_needs_work_on_both_sides() {
        let p = Plan {
            first: vec![1],
            rest: (2..2 + STAGE_MIN_FILES).collect(),
        };
        assert!(p.staged(0));
        let few = Plan {
            first: vec![1],
            rest: vec![2],
        };
        assert!(!few.staged(0));
        assert!(few.staged(STAGE_MIN_BYTES));
        let none_in_view = Plan {
            first: vec![],
            rest: (0..100).collect(),
        };
        assert!(!none_in_view.staged(usize::MAX));
    }
}
