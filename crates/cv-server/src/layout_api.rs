//! Layout edits and what survives a re-index (docs/contracts/http.ts `PATCH /views/:id/layout`,
//! DESIGN §4.10.6 "pins are honoured"):
//!
//! - `PATCH /views/:viewId/layout` moves windows (`key`: a WindowKey) and clusters (`key`:
//!   `cluster:<Cluster.id>`) in place, recomputes what depends on positions (grid index, cluster
//!   rects, world, hub anchors; link endpoints are derived from window rects), bumps
//!   `layoutVersion` and emits `view.invalidated { reason: "layout", windows: <moved ids> }`.
//! - Pins (`pinned: true`) persist in review.db (`layout_pin`, `cluster_pin`): windows by WindowKey,
//!   clusters by lane and member WindowKeys (matched again by member Jaccard ≥ 0.6, as labels are).
//!   Every (re)index feeds window pins to cv-layout and then applies cluster pins, so pinned things
//!   keep their spots across restarts and new revisions. `pinned: false` is a plain move and unpins.
//! - Human and agent cluster labels (with `summary`) persist in `cluster_label` and are applied
//!   to every newly built view the same way.
//! - **Drift:** clusters change a little with every revision, so after each full (re)index the
//!   stored member list (and lane) of every matched label and cluster pin is rewritten to the
//!   cluster it matched; a label survives any amount of cumulative drift as long as each index
//!   sees ≥ 0.6 overlap with the previous one. A record is matched in its own lane first, then in
//!   any lane (a `.cv/lanes.toml` change can move a cluster). An unmatched record is kept and
//!   counts `missed`; it is dropped after [`MAX_MISSES`] consecutive full indexes without a match.
//!   Two records matching the same cluster collapse into the later one (the one that was shown).
//! - Window pins are keyed by WindowKey (`path#symbol-key`). When the file was renamed (the
//!   rename pairs the churn walk follows, old path → path at the head), the pin moves to the new
//!   key. A renamed symbol changes the key and is not carried: that pin counts misses like a
//!   cluster record (`pinMissed`) and is dropped after [`MAX_MISSES`].
//!
//! A plain move also updates the in-memory placement of the repo, so a later full re-index in the
//! same process reuses the position (where it still fits its cell); only pins survive a restart.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::patch,
};
use cv_contracts::review::ServerEvent;
use cv_contracts::{HEADER_WU, LINE_WU, Rect};
use cv_review::Review;
use cv_vcs::{CommitInfo, JjRepo};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::index::{self, View};
use crate::routes::{ApiErr, AppState};

/// Consecutive full indexes a stored label, cluster pin or window pin may go unmatched before
/// it is dropped.
pub const MAX_MISSES: u32 = 20;

/// Room kept around a cluster's windows when its rect grows to contain them (cv-layout's
/// cluster padding and title band).
const CLUSTER_PAD: f64 = 80.0;
const CLUSTER_TITLE: f64 = 120.0;

type R<T> = Result<T, ApiErr>;
type St = State<Arc<AppState>>;

fn err(status: StatusCode, code: &'static str, message: impl Into<String>) -> ApiErr {
    ApiErr {
        status,
        code,
        message: message.into(),
    }
}

fn internal(e: impl std::fmt::Display) -> ApiErr {
    err(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string())
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/views/{view}/layout", patch(layout_route))
}

// ─── Store ───────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LayoutStore {
    /// Pinned windows: WindowKey → world origin [x, y].
    #[serde(default)]
    pub pins: BTreeMap<String, [f64; 2]>,
    #[serde(default)]
    pub cluster_pins: Vec<ClusterPin>,
    #[serde(default)]
    pub labels: Vec<ClusterLabel>,
    /// Window pins whose key was missing from the last full indexes: key → consecutive misses.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pin_missed: BTreeMap<String, u32>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// A pinned cluster: `key` is `cluster:<label when pinned>` (informative); it is found again by
/// lane and member keys.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClusterPin {
    pub key: String,
    pub lane: String,
    pub members: Vec<String>,
    pub x: f64,
    pub y: f64,
    /// Consecutive full indexes without a matching cluster.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub missed: u32,
}

/// A human or agent cluster label, found again by lane and member keys.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClusterLabel {
    pub lane: String,
    pub members: Vec<String>,
    pub label: String,
    pub label_source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Consecutive full indexes without a matching cluster.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub missed: u32,
}

/// Serializes read-modify-write of the store and of views being patched.
fn lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(Default::default)
}

/// Pins and labels from review.db (`layout_pin`, `cluster_pin`, `cluster_label`).
pub fn load(r: &Review) -> LayoutStore {
    let Ok(rows) = r.db().layout_records() else {
        return LayoutStore::default();
    };
    let mut s = LayoutStore::default();
    for p in rows.pins {
        if p.missed > 0 {
            s.pin_missed.insert(p.key.clone(), p.missed);
        }
        s.pins.insert(p.key, [p.x, p.y]);
    }
    s.cluster_pins = rows
        .cluster_pins
        .into_iter()
        .map(|p| ClusterPin {
            key: p.key,
            lane: p.lane,
            members: p.members,
            x: p.x,
            y: p.y,
            missed: p.missed,
        })
        .collect();
    s.labels = rows
        .labels
        .into_iter()
        .map(|l| ClusterLabel {
            lane: l.lane,
            members: l.members,
            label: l.label,
            label_source: l.source,
            summary: l.summary,
            missed: l.missed,
        })
        .collect();
    s
}

fn save(r: &Review, s: &LayoutStore) -> R<()> {
    let rows = cv_store::LayoutRows {
        pins: s
            .pins
            .iter()
            .map(|(k, &[x, y])| cv_store::WindowPinRow {
                key: k.clone(),
                x,
                y,
                missed: s.pin_missed.get(k).copied().unwrap_or(0),
            })
            .collect(),
        cluster_pins: s
            .cluster_pins
            .iter()
            .map(|p| cv_store::ClusterPinRow {
                key: p.key.clone(),
                lane: p.lane.clone(),
                members: p.members.clone(),
                x: p.x,
                y: p.y,
                missed: p.missed,
            })
            .collect(),
        labels: s
            .labels
            .iter()
            .map(|l| cv_store::ClusterLabelRow {
                lane: l.lane.clone(),
                members: l.members.clone(),
                label: l.label.clone(),
                // The table holds agent and human labels only (auto labels are not stored).
                source: if l.label_source == "agent" {
                    "agent".into()
                } else {
                    "human".into()
                },
                summary: l.summary.clone(),
                missed: l.missed,
            })
            .collect(),
    };
    r.db().put_layout_records(&rows).map_err(internal)
}

fn open_review(app: &AppState) -> Option<Arc<Review>> {
    app.review.lock().unwrap().clone()
}

// ─── Cluster identity ────────────────────────────────────────────────────────

fn jaccard(a: &[String], b: &[String]) -> f64 {
    let a: BTreeSet<&str> = a.iter().map(String::as_str).collect();
    let b: BTreeSet<&str> = b.iter().map(String::as_str).collect();
    let union = a.union(&b).count();
    if union == 0 {
        return 0.0;
    }
    a.intersection(&b).count() as f64 / union as f64
}

/// Window indexes per cluster id.
fn members_by_cluster(v: &View) -> HashMap<String, Vec<usize>> {
    let mut m: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, w) in v.windows.iter().enumerate() {
        m.entry(w.header.cluster.clone()).or_default().push(i);
    }
    m
}

/// Sorted, de-duplicated WindowKeys of some windows.
fn keys_of(v: &View, ws: &[usize]) -> Vec<String> {
    let mut k: Vec<String> = ws
        .iter()
        .map(|&w| v.windows[w].header.key.clone())
        .collect();
    k.sort();
    k.dedup();
    k
}

/// The cluster (index into the bootstrap clusters) whose members best match `members`, at member
/// Jaccard ≥ 0.6 (cv-layout's label carry-over rule): in `lane` first, else in any lane.
fn match_cluster(
    v: &View,
    groups: &HashMap<String, Vec<usize>>,
    lane: &str,
    members: &[String],
) -> Option<usize> {
    let best = |same_lane: bool| {
        v.bootstrap
            .base
            .clusters
            .iter()
            .enumerate()
            .filter(|(_, c)| !same_lane || c.lane == lane)
            .map(|(ci, c)| {
                let ws = groups.get(&c.id).map(Vec::as_slice).unwrap_or(&[]);
                (ci, jaccard(&keys_of(v, ws), members))
            })
            .filter(|&(_, j)| j >= cv_layout::LABEL_JACCARD)
            .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)))
            .map(|(ci, _)| ci)
    };
    best(true).or_else(|| best(false))
}

/// Follows the view's clusters and renames (see the module docs, "Drift"): matched records take
/// the matched cluster's members and lane, unmatched ones count a miss and go after
/// [`MAX_MISSES`]. Returns whether the store changed.
pub fn track_drift(v: &View, store: &mut LayoutStore) -> bool {
    let before = store.clone();
    let groups = members_by_cluster(v);
    let ident = |ci: usize| {
        let c = &v.bootstrap.base.clusters[ci];
        let ws = groups.get(&c.id).map(Vec::as_slice).unwrap_or(&[]);
        (c.lane.clone(), keys_of(v, ws))
    };
    // Later records win a cluster (they are applied last), so walk from the end.
    let mut taken: BTreeSet<usize> = BTreeSet::new();
    let mut labels: Vec<ClusterLabel> = Vec::new();
    for mut l in std::mem::take(&mut store.labels).into_iter().rev() {
        match match_cluster(v, &groups, &l.lane, &l.members) {
            Some(ci) if !taken.insert(ci) => continue,
            Some(ci) => {
                (l.lane, l.members) = ident(ci);
                l.missed = 0;
            }
            None => l.missed += 1,
        }
        if l.missed < MAX_MISSES {
            labels.push(l);
        }
    }
    labels.reverse();
    store.labels = labels;
    taken.clear();
    let mut pins: Vec<ClusterPin> = Vec::new();
    for mut p in std::mem::take(&mut store.cluster_pins).into_iter().rev() {
        match match_cluster(v, &groups, &p.lane, &p.members) {
            Some(ci) if !taken.insert(ci) => continue,
            Some(ci) => {
                (p.lane, p.members) = ident(ci);
                p.missed = 0;
            }
            None => p.missed += 1,
        }
        if p.missed < MAX_MISSES {
            pins.push(p);
        }
    }
    pins.reverse();
    store.cluster_pins = pins;
    let keys: BTreeSet<&str> = v.windows.iter().map(|w| w.header.key.as_str()).collect();
    for (key, at) in std::mem::take(&mut store.pins) {
        if keys.contains(key.as_str()) {
            store.pin_missed.remove(&key);
            store.pins.insert(key, at);
            continue;
        }
        let renamed = key.split_once('#').and_then(|(path, sym)| {
            let to = v.renames.get(path)?;
            let k = format!("{to}#{sym}");
            keys.contains(k.as_str()).then_some(k)
        });
        if let Some(k) = renamed.filter(|k| !store.pins.contains_key(k)) {
            store.pin_missed.remove(&key);
            store.pins.insert(k, at);
            continue;
        }
        let n = store.pin_missed.entry(key.clone()).or_default();
        *n += 1;
        if *n < MAX_MISSES {
            store.pins.insert(key, at);
        } else {
            store.pin_missed.remove(&key);
        }
    }
    let pins = &store.pins;
    store.pin_missed.retain(|k, _| pins.contains_key(k));
    *store != before
}

// ─── Geometry ────────────────────────────────────────────────────────────────

fn union(a: Rect, b: Rect) -> Rect {
    let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
    let (x1, y1) = ((a.x + a.w).max(b.x + b.w), (a.y + a.h).max(b.y + b.h));
    Rect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    }
}

/// Moves cluster `ci` to (x, y) with its windows, except pinned ones (pins never move). Returns
/// the indexes of the windows moved.
fn move_cluster(
    v: &mut View,
    groups: &HashMap<String, Vec<usize>>,
    ci: usize,
    x: f64,
    y: f64,
) -> Vec<usize> {
    let c = &mut v.bootstrap.base.clusters[ci];
    let (dx, dy) = (x - c.rect.x, y - c.rect.y);
    c.rect.x = x;
    c.rect.y = y;
    let mut moved = Vec::new();
    for &w in groups.get(&c.id).map(Vec::as_slice).unwrap_or(&[]) {
        let h = &mut v.windows[w].header;
        if h.flags.pinned {
            continue;
        }
        h.rect.x += dx;
        h.rect.y += dy;
        moved.push(w);
    }
    moved
}

/// Recomputes everything derived from window positions: cluster rects grow to contain their
/// windows, the world grows to contain the clusters, hub anchors follow their windows and the
/// grid index is rebuilt. Link endpoints are computed from window rects on demand.
pub fn refresh_geometry(v: &mut View) {
    let groups = members_by_cluster(v);
    let b = &mut v.bootstrap.base;
    for c in b.clusters.iter_mut() {
        for &w in groups.get(&c.id).map(Vec::as_slice).unwrap_or(&[]) {
            let r = v.windows[w].header.rect;
            c.rect = union(
                c.rect,
                Rect {
                    x: r.x - CLUSTER_PAD,
                    y: r.y - CLUSTER_TITLE,
                    w: r.w + 2.0 * CLUSTER_PAD,
                    h: r.h + CLUSTER_TITLE + CLUSTER_PAD,
                },
            );
        }
        b.world = union(b.world, c.rect);
    }
    let by_id = &v.by_id;
    let windows = &v.windows;
    let port = |s: &cv_contracts::SiteRef, out: bool| -> Option<[f64; 2]> {
        let h = &windows[*by_id.get(s.window.as_ref()?)?].header;
        let y = h.rect.y
            + HEADER_WU
            + (s.line.clamp(h.lines.start, h.lines.end) - h.lines.start) as f64 * LINE_WU
            + LINE_WU / 2.0;
        Some([if out { h.rect.x + h.rect.w } else { h.rect.x }, y])
    };
    for hub in b.hubs.iter_mut() {
        if hub.anchors.is_none() {
            continue;
        }
        let anchors: Vec<[f64; 2]> = hub
            .producers
            .iter()
            .filter_map(|s| port(s, true))
            .chain(hub.consumers.iter().filter_map(|s| port(s, false)))
            .collect();
        hub.anchors = Some(anchors);
    }
    v.tiles = index::grid_index(&v.windows, &v.links);
}

/// Applies stored labels and pins to a freshly built view: window pins (already placed by
/// cv-layout) get `flags.pinned`, pinned clusters move to their spot with their unpinned windows,
/// labels and summaries are restored.
pub fn apply_persisted(v: &mut View, store: &LayoutStore) {
    if *store == LayoutStore::default() {
        return;
    }
    for w in v.windows.iter_mut() {
        if let Some(&[x, y]) = store.pins.get(&w.header.key) {
            w.header.rect.x = x;
            w.header.rect.y = y;
            w.header.flags.pinned = true;
        }
    }
    let groups = members_by_cluster(v);
    for l in &store.labels {
        if let Some(ci) = match_cluster(v, &groups, &l.lane, &l.members) {
            let c = &mut v.bootstrap.base.clusters[ci];
            c.label = l.label.clone();
            c.label_source = l.label_source.clone();
            if l.summary.is_some() {
                c.summary = l.summary.clone();
            }
        }
    }
    for p in &store.cluster_pins {
        if let Some(ci) = match_cluster(v, &groups, &p.lane, &p.members) {
            move_cluster(v, &groups, ci, p.x, p.y);
            v.bootstrap.base.clusters[ci].pinned = true;
        }
    }
    refresh_geometry(v);
}

/// Builds a snapshot view with the stored pins and labels applied (every full index of a new spec
/// or revision goes through here), then rewrites the stored records to the clusters they matched
/// (see the module docs, "Drift").
pub fn build_snapshot(
    app: &AppState,
    repo: &JjRepo,
    spec: Value,
    commit: &CommitInfo,
) -> Result<View, String> {
    let review = open_review(app);
    let store = review.as_ref().map(|r| load(r)).unwrap_or_default();
    let pins: Vec<cv_layout::Pin> = store
        .pins
        .iter()
        .map(|(k, &[x, y])| cv_layout::Pin {
            key: k.clone(),
            x,
            y,
        })
        .collect();
    // Saved views (index.db) stand in for a build with the same inputs (crate::view_store).
    let mut v = crate::view_store::load_or_build(
        repo,
        &app.registry,
        spec,
        commit,
        app.opts,
        &pins,
        crate::view_store::wait_for_save(),
    )?;
    // A fileset view shows part of the map: its clusters must not count as misses or rewrite the
    // members of label and pin records (drift follows full views only).
    let partial = !v.bootstrap.base.view.spec["fileset"].is_null();
    let Some(r) = review.filter(|_| !partial) else {
        apply_persisted(&mut v, &store);
        return Ok(v);
    };
    let _g = lock().lock().unwrap();
    let mut store = load(&r);
    if track_drift(&v, &mut store)
        && let Err(e) = save(&r, &store)
    {
        eprintln!("cv: saving layout records: {}", e.message);
    }
    apply_persisted(&mut v, &store);
    Ok(v)
}

// ─── Labels ──────────────────────────────────────────────────────────────────

/// Stores a human or agent label for cluster `c{index}` (members from the repo's last placement,
/// else from a cached view showing that cluster id), replacing any record for the same cluster.
pub fn persist_label(
    app: &Arc<AppState>,
    cluster: &str,
    index: usize,
    label: &str,
    source: &str,
    summary: Option<&str>,
) -> R<()> {
    let repo_key = app.repo.root().display().to_string();
    let ident = crate::layout_input::last_placement(&repo_key)
        .and_then(|p| {
            let c = p.clusters.get(index)?;
            Some((c.lane.clone(), c.members.clone()))
        })
        .or_else(|| {
            let views: Vec<Arc<View>> = app.views.read().unwrap().values().cloned().collect();
            views.iter().find_map(|v| {
                let c = v.bootstrap.base.clusters.iter().find(|c| c.id == cluster)?;
                let groups = members_by_cluster(v);
                let ws = groups.get(&c.id).map(Vec::as_slice).unwrap_or(&[]);
                Some((c.lane.clone(), keys_of(v, ws)))
            })
        });
    let Some((lane, members)) = ident else {
        return Ok(());
    };
    let r = app
        .review()
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.code, e.message))?;
    let _g = lock().lock().unwrap();
    let mut store = load(&r);
    let mut old_summary = None;
    store.labels.retain(|l| {
        let same = l.lane == lane && jaccard(&l.members, &members) >= cv_layout::LABEL_JACCARD;
        if same && old_summary.is_none() {
            old_summary = l.summary.clone();
        }
        !same
    });
    store.labels.push(ClusterLabel {
        lane,
        members,
        label: label.to_string(),
        label_source: source.to_string(),
        summary: summary.map(str::to_string).or(old_summary),
        missed: 0,
    });
    save(&r, &store)
}

// ─── PATCH /views/:id/layout ─────────────────────────────────────────────────

/// The body (http.ts LayoutPatch), from cv-contracts.
pub use cv_contracts::api::{LayoutMove as Move, LayoutPatch};

/// What one move resolved to in a view.
enum Target {
    Windows(Vec<usize>),
    Cluster(usize),
}

fn resolve(v: &View, m: &Move) -> R<Target> {
    if !(m.x.is_finite() && m.y.is_finite()) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "invalid",
            format!("move of {} has a non-finite position", m.key),
        ));
    }
    if let Some(id) = m.key.strip_prefix("cluster:") {
        let cs = &v.bootstrap.base.clusters;
        // The bootstrap id (`cluster:c42`), else a label (`cluster:<label>`).
        return cs
            .iter()
            .position(|c| c.id == id)
            .or_else(|| cs.iter().position(|c| c.label == id))
            .map(Target::Cluster)
            .ok_or_else(|| {
                err(
                    StatusCode::BAD_REQUEST,
                    "unknown-key",
                    format!("unknown cluster {id}"),
                )
            });
    }
    let ws: Vec<usize> = v
        .windows
        .iter()
        .enumerate()
        .filter(|(_, w)| w.header.key == m.key)
        .map(|(i, _)| i)
        .collect();
    if ws.is_empty() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "unknown-key",
            format!("no window with key {} in this view", m.key),
        ));
    }
    Ok(Target::Windows(ws))
}

/// Applies a layout patch to the view `id` in place and persists its pins. Returns the new
/// layout version.
pub fn patch_layout(app: &Arc<AppState>, view_id: &str, p: &LayoutPatch) -> R<Value> {
    let review = app
        .review()
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.code, e.message))?;
    let _g = lock().lock().unwrap();
    let old = app.view(view_id)?;
    let current = old.bootstrap.base.view.layout_version;
    if p.base_layout_version != current {
        return Err(err(
            StatusCode::CONFLICT,
            "layout-stale",
            format!(
                "baseLayoutVersion {} is stale: the view is at layoutVersion {current}",
                p.base_layout_version
            ),
        ));
    }
    let targets: Vec<Target> = p.moves.iter().map(|m| resolve(&old, m)).collect::<R<_>>()?;
    let mut v = (*old).clone();
    let mut store = load(&review);
    let before = store.clone();
    let mut moved: Vec<usize> = Vec::new();
    let mut moved_clusters: Vec<usize> = Vec::new();
    for (m, t) in p.moves.iter().zip(targets) {
        match t {
            Target::Windows(ws) => {
                for w in ws {
                    let h = &mut v.windows[w].header;
                    h.rect.x = m.x;
                    h.rect.y = m.y;
                    h.flags.pinned = m.pinned;
                    moved.push(w);
                }
                if m.pinned {
                    store.pins.insert(m.key.clone(), [m.x, m.y]);
                } else {
                    store.pins.remove(&m.key);
                }
            }
            Target::Cluster(ci) => {
                let groups = members_by_cluster(&v);
                moved.extend(move_cluster(&mut v, &groups, ci, m.x, m.y));
                moved_clusters.push(ci);
                let c = &mut v.bootstrap.base.clusters[ci];
                c.pinned = m.pinned;
                let (lane, label) = (c.lane.clone(), c.label.clone());
                let ws = groups.get(&c.id).cloned().unwrap_or_default();
                let members = keys_of(&v, &ws);
                store.cluster_pins.retain(|cp| {
                    !(cp.lane == lane && jaccard(&cp.members, &members) >= cv_layout::LABEL_JACCARD)
                });
                if m.pinned {
                    store.cluster_pins.push(ClusterPin {
                        key: format!("cluster:{label}"),
                        lane,
                        members,
                        x: m.x,
                        y: m.y,
                        missed: 0,
                    });
                }
            }
        }
    }
    refresh_geometry(&mut v);
    v.bootstrap.base.view.layout_version = current + 1;
    let version = current + 1;
    moved.sort_unstable();
    moved.dedup();
    remember_moves(app, &v, &moved, &moved_clusters);
    let ids: Vec<String> = moved
        .iter()
        .map(|&w| v.windows[w].header.id.clone())
        .collect();
    let v = Arc::new(v);
    app.views
        .write()
        .unwrap()
        .insert(view_id.to_string(), v.clone());
    crate::readside::range::replace_view(app, view_id, v.clone());
    if store != before {
        save(&review, &store)?;
    }
    review.emit(ServerEvent::ViewInvalidated {
        view_id: view_id.to_string(),
        layout_version: version,
        reason: "layout".into(),
        windows: Some(ids),
        tiles: None,
    });
    propagate(app, &review, view_id, &v, &moved, &moved_clusters);
    Ok(json!({ "layoutVersion": version }))
}

/// Range views share their snapshot's placement (D16), and other open views show the same windows:
/// give every other cached view the moved windows' new places (by WindowKey) and the moved clusters'
/// rects (by id, same lane), bump its layoutVersion and tell its clients.
fn propagate(
    app: &Arc<AppState>,
    review: &Review,
    from: &str,
    src: &View,
    moved: &[usize],
    clusters: &[usize],
) {
    if moved.is_empty() && clusters.is_empty() {
        return;
    }
    let places: HashMap<&str, (Rect, bool)> = moved
        .iter()
        .map(|&w| {
            let h = &src.windows[w].header;
            (h.key.as_str(), (h.rect, h.flags.pinned))
        })
        .collect();
    let cluster_rects: Vec<(&str, &str, Rect, bool)> = clusters
        .iter()
        .map(|&ci| {
            let c = &src.bootstrap.base.clusters[ci];
            (c.id.as_str(), c.lane.as_str(), c.rect, c.pinned)
        })
        .collect();
    // Range views drawn on this snapshot are rebuilt from it (their hunk windows have their own keys).
    for (id, version) in crate::readside::range::rebuild_on(app, from) {
        review.emit(ServerEvent::ViewInvalidated {
            view_id: id,
            layout_version: version,
            reason: "layout".into(),
            windows: Some(vec![]),
            tiles: None,
        });
    }
    let others: Vec<(String, Arc<View>)> = app
        .views
        .read()
        .unwrap()
        .iter()
        .filter(|(id, _)| id.as_str() != from && app.read.range(id).is_none())
        .map(|(id, v)| (id.clone(), v.clone()))
        .collect();
    for (id, old) in others {
        let mut v = (*old).clone();
        let mut ids = Vec::new();
        for w in v.windows.iter_mut() {
            if let Some(&(r, pinned)) = places.get(w.header.key.as_str()) {
                w.header.rect.x = r.x;
                w.header.rect.y = r.y;
                w.header.flags.pinned = pinned;
                ids.push(w.header.id.clone());
            }
        }
        let mut touched = !ids.is_empty();
        for c in v.bootstrap.base.clusters.iter_mut() {
            if let Some(&(_, _, r, pinned)) = cluster_rects
                .iter()
                .find(|(cid, lane, _, _)| *cid == c.id && *lane == c.lane)
            {
                c.rect = r;
                c.pinned = pinned;
                touched = true;
            }
        }
        if !touched {
            continue;
        }
        refresh_geometry(&mut v);
        v.bootstrap.base.view.layout_version += 1;
        let version = v.bootstrap.base.view.layout_version;
        let v = Arc::new(v);
        app.views.write().unwrap().insert(id.clone(), v.clone());
        crate::readside::range::replace_view(app, &id, v);
        review.emit(ServerEvent::ViewInvalidated {
            view_id: id,
            layout_version: version,
            reason: "layout".into(),
            windows: Some(ids),
            tiles: None,
        });
    }
}

/// Carries moved positions into the repo's in-memory placement, so a later re-index in this
/// process starts from them.
fn remember_moves(app: &AppState, v: &View, moved: &[usize], clusters: &[usize]) {
    if moved.is_empty() && clusters.is_empty() {
        return;
    }
    let repo_key = app.repo.root().display().to_string();
    let rect_of = |w: usize| (v.windows[w].header.key.clone(), v.windows[w].header.rect);
    let rects: Vec<(String, Rect)> = moved.iter().map(|&w| rect_of(w)).collect();
    let groups = members_by_cluster(v);
    // A moved cluster: its placed twin (the cluster most of its windows sat in) takes its rect.
    let cluster_rects: Vec<(Vec<String>, Rect)> = clusters
        .iter()
        .map(|&ci| {
            let c = &v.bootstrap.base.clusters[ci];
            let ws = groups.get(&c.id).map(Vec::as_slice).unwrap_or(&[]);
            (keys_of(v, ws), c.rect)
        })
        .collect();
    crate::layout_input::update_placement(&repo_key, |p| {
        for (keys, r) in &cluster_rects {
            let mut votes: HashMap<usize, usize> = HashMap::new();
            for k in keys {
                if let Some((pc, _)) = p.windows.get(k) {
                    *votes.entry(*pc).or_default() += 1;
                }
            }
            let best = votes
                .into_iter()
                .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)));
            if let Some((pc, _)) = best
                && let Some(c) = p.clusters.get_mut(pc)
            {
                c.rect = *r;
            }
        }
        for (key, r) in &rects {
            let Some((pc, pr)) = p.windows.get_mut(key) else {
                continue;
            };
            pr.x = r.x;
            pr.y = r.y;
            let pc = *pc;
            if let Some(c) = p.clusters.get_mut(pc) {
                c.rect = union(
                    c.rect,
                    Rect {
                        x: r.x - CLUSTER_PAD,
                        y: r.y - CLUSTER_TITLE,
                        w: r.w + 2.0 * CLUSTER_PAD,
                        h: r.h + CLUSTER_TITLE + CLUSTER_PAD,
                    },
                );
            }
        }
    });
}

/// Whether two rects share some x range (one column of the layout).
fn x_overlap(a: &Rect, b: &Rect) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w
}

/// Expanding a window as a layout change (snapshot views): the window takes the widened span `h`
/// (from [`crate::readside::expand_header`]) with its top where it was, and what it would cover
/// moves down instead: the rows of its cluster below it, together and just far enough to keep the
/// layout's gap under the window, then the clusters of its lane below the cluster (in an
/// overlapping column), if the cluster grew. Same bookkeeping as a PATCH (geometry, layoutVersion, `view.invalidated
/// "layout"`), but nothing is pinned, remembered for re-indexes or carried to other views: the
/// widened span belongs to this view, and a re-index lays the file out at its normal size again.
/// Returns the stored header and the new layoutVersion.
pub fn expand_in_layout(
    app: &Arc<AppState>,
    view_id: &str,
    window: &str,
    lines: (u32, u32),
) -> R<(cv_contracts::WindowHeader, u32)> {
    let review = app
        .review()
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.code, e.message))?;
    let _g = lock().lock().unwrap();
    let old = app.view(view_id)?;
    let &wi = old.by_id.get(window).ok_or_else(|| {
        err(
            StatusCode::NOT_FOUND,
            "not-found",
            format!("unknown window {window}"),
        )
    })?;
    let mut v = (*old).clone();
    let before = v.windows[wi].header.rect;
    {
        let h = &mut v.windows[wi].header;
        h.rect.h += (h.lines.start.saturating_sub(lines.0) + lines.1.saturating_sub(h.lines.end))
            as f64
            * LINE_WU;
        h.lines.start = lines.0.min(h.lines.start);
        h.lines.end = lines.1.max(h.lines.end);
        h.flags.truncated = false;
    }
    let grow = v.windows[wi].header.rect.h - before.h;
    let mut moved = vec![wi];
    if grow > 0.0 {
        let cluster = v.windows[wi].header.cluster.clone();
        let wide = v.windows[wi].header.rect;
        let below: Vec<usize> = (0..v.windows.len())
            .filter(|&w| {
                let h = &v.windows[w].header;
                w != wi
                    && h.cluster == cluster
                    && !h.flags.pinned
                    && h.rect.y >= before.y + before.h - 1.0
            })
            .collect();
        // The rows below move down together (the grid stays tidy), just far enough that the
        // widened window keeps the layout's gap to whatever is under it.
        let push = below
            .iter()
            .map(|&w| v.windows[w].header.rect)
            .filter(|r| x_overlap(&wide, r))
            .map(|r| wide.y + wide.h + cv_layout::WIN_GAP - r.y)
            .fold(0.0, f64::max);
        if push > 0.0 {
            for w in below {
                v.windows[w].header.rect.y += push;
                moved.push(w);
            }
        }
        let ci = v
            .bootstrap
            .base
            .clusters
            .iter()
            .position(|c| c.id == cluster);
        let was = ci.map(|ci| v.bootstrap.base.clusters[ci].rect);
        refresh_geometry(&mut v);
        if let (Some(ci), Some(was)) = (ci, was) {
            let now = v.bootstrap.base.clusters[ci].rect;
            let cgrow = (now.y + now.h) - (was.y + was.h);
            if cgrow > 0.0 {
                let lane = v.bootstrap.base.clusters[ci].lane.clone();
                let mut order: Vec<usize> = (0..v.bootstrap.base.clusters.len())
                    .filter(|&k| {
                        let c = &v.bootstrap.base.clusters[k];
                        k != ci && c.lane == lane && !c.pinned && c.rect.y >= was.y + was.h - 1.0
                    })
                    .collect();
                order.sort_by(|&a, &b| {
                    let cs = &v.bootstrap.base.clusters;
                    cs[a].rect.y.total_cmp(&cs[b].rect.y)
                });
                let groups = members_by_cluster(&v);
                let mut pushed = vec![was];
                for k in order {
                    let r = v.bootstrap.base.clusters[k].rect;
                    if pushed.iter().any(|p| x_overlap(p, &r)) {
                        pushed.push(r);
                        moved.extend(move_cluster(&mut v, &groups, k, r.x, r.y + cgrow));
                    }
                }
            }
        }
    }
    refresh_geometry(&mut v);
    let version = v.bootstrap.base.view.layout_version + 1;
    v.bootstrap.base.view.layout_version = version;
    moved.sort_unstable();
    moved.dedup();
    let ids: Vec<String> = moved
        .iter()
        .map(|&w| v.windows[w].header.id.clone())
        .collect();
    let header = v.windows[wi].header.clone();
    let v = Arc::new(v);
    app.views
        .write()
        .unwrap()
        .insert(view_id.to_string(), v.clone());
    crate::readside::range::replace_view(app, view_id, v);
    review.emit(ServerEvent::ViewInvalidated {
        view_id: view_id.to_string(),
        layout_version: version,
        reason: "layout".into(),
        windows: Some(ids),
        tiles: None,
    });
    Ok((header, version))
}

async fn layout_route(
    State(app): St,
    Path(view): Path<String>,
    Json(p): Json<LayoutPatch>,
) -> R<Json<Value>> {
    tokio::task::spawn_blocking(move || patch_layout(&app, &view, &p).map(Json))
        .await
        .map_err(internal)?
}
