//! Navigation and feedback routes the canvas and cv-mcp share (docs/contracts/http.ts):
//! `GET /links` (BFS over a view's links, channel links and hubs), `GET /hubs/:hub`, declared links
//! and link ratings (`POST /links`, `POST /links/:id/rate`, persisted in review.db), cluster labels
//! (`PUT /clusters/:id/label`), agent navigation (`POST /agent/view`) and `GET /interdiff`.
//!
//! Every route's logic is a `pub fn` taking the [`AppState`] so an in-process MCP backend can call
//! the same code: [`get_links`], [`get_hub`], [`declare_link`], [`rate_link`], [`label_cluster`],
//! [`set_view`], [`interdiff`].
//!
//! Declared links and ratings live in review.db (`declared_link`, `link_feedback`) and are merged into a
//! view's responses through [`overlay_batch`] and [`overlay_tile`]. Ratings are keyed by what a
//! link connects (kind, from site, to site), not by its per-view id, so they survive re-indexing.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post, put},
};
use base64::Engine;
use cv_contracts::readside::DiffLine;
use cv_contracts::review::{Actor, ServerEvent};
use cv_contracts::{
    Hub, Link, Port, Rect, SiteRef, TILE_BASE_WU, TileEdge, TileResponse, WindowBatchRequest,
    WindowBatchResponse,
};
use cv_lang::silhouette::PORT_BIT;
use cv_review::{Review, ReviewError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::index::{LinkRec, View, link_points};
use crate::routes::{ApiErr, AppState};

/// Every `LinkKind` of model.ts.
pub const LINK_KINDS: [&str; 15] = [
    "call",
    "inherit",
    "implement",
    "import",
    "reference",
    "publish",
    "subscribe",
    "dispatch",
    "http-request",
    "http-route",
    "config",
    "define",
    "read",
    "deploy",
    "ipc",
];

/// Kinds drawn through channels (hubs): `GET /links` returns hubs only when one of these is asked.
const CHANNEL_KINDS: [&str; 10] = [
    "publish",
    "subscribe",
    "dispatch",
    "http-request",
    "http-route",
    "config",
    "define",
    "read",
    "deploy",
    "ipc",
];

type St = State<Arc<AppState>>;
type R<T> = Result<T, ApiErr>;

fn err(status: StatusCode, code: &'static str, message: impl Into<String>) -> ApiErr {
    ApiErr {
        status,
        code,
        message: message.into(),
    }
}

fn not_found(message: impl Into<String>) -> ApiErr {
    err(StatusCode::NOT_FOUND, "not-found", message)
}

fn invalid(message: impl Into<String>) -> ApiErr {
    err(StatusCode::BAD_REQUEST, "invalid", message)
}

fn from_review(e: ReviewError) -> ApiErr {
    err(
        StatusCode::from_u16(e.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        e.code,
        e.message,
    )
}

fn internal(e: impl std::fmt::Display) -> ApiErr {
    err(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string())
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/links", get(links_route).post(declare_route))
        .route("/api/v1/links/{link}/rate", post(rate_route))
        .route("/api/v1/hubs/{*hub}", get(hub_route))
        .route("/api/v1/clusters/{cluster}/label", put(label_route))
        .route("/api/v1/agent/view", post(set_view_route))
        .route("/api/v1/interdiff", get(interdiff_route))
        .route("/api/v1/diff", get(diff_route))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> R<T> + Send + 'static) -> R<T> {
    tokio::task::spawn_blocking(f).await.map_err(internal)?
}

// ─── Store (review.db) ───────────────────────────────────────────────────────

/// A code site as stored: independent of any view (windows are found per view).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Site {
    pub path: String,
    pub line: u32,
}

/// The far end of a declared link: a code site or a hub.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum End {
    Site(Site),
    Hub { hub: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Declared {
    pub id: String,
    pub kind: String,
    pub from: Site,
    pub to: End,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub by: Actor,
    pub at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rating {
    /// [`link_key`] of the rated link.
    pub key: String,
    /// The link id it was rated under (informative: ids are per view).
    pub id: String,
    pub verdict: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub by: Actor,
    pub at: String,
    /// The link as rated. A confirmed link that a later index no longer produces (its score fell
    /// below the draw threshold, or it is no longer among a ref's candidates) is re-added from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<Link>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub declared: Vec<Declared>,
    #[serde(default)]
    pub ratings: Vec<Rating>,
}

/// Serializes read-modify-write of the store.
fn store_lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(Default::default)
}

/// Declared links and ratings from review.db (a row that no longer parses is skipped).
pub fn load_store(r: &Review) -> Store {
    let Ok((declared, ratings)) = r.db().link_records() else {
        return Store::default();
    };
    let declared = declared
        .into_iter()
        .filter_map(|d| {
            Some(Declared {
                id: d.id,
                kind: d.kind,
                from: serde_json::from_str(&d.from_json).ok()?,
                to: serde_json::from_str(&d.to_json).ok()?,
                label: d.label,
                note: d.note,
                by: serde_json::from_str(&d.actor_json).ok()?,
                at: d.at,
            })
        })
        .collect();
    let ratings = ratings
        .into_iter()
        .filter_map(|x| {
            Some(Rating {
                key: x.key,
                id: x.link_id,
                verdict: x.verdict,
                note: x.note,
                by: serde_json::from_str(&x.actor_json).ok()?,
                at: x.at,
                link: x.link_json.and_then(|l| serde_json::from_str(&l).ok()),
            })
        })
        .collect();
    Store { declared, ratings }
}

fn save_store(r: &Review, s: &Store) -> R<()> {
    fn js<T: Serialize>(v: &T) -> String {
        serde_json::to_string(v).unwrap_or_else(|_| "null".into())
    }
    let declared: Vec<cv_store::DeclaredLinkRow> = s
        .declared
        .iter()
        .map(|d| cv_store::DeclaredLinkRow {
            id: d.id.clone(),
            kind: d.kind.clone(),
            from_json: js(&d.from),
            to_json: js(&d.to),
            label: d.label.clone(),
            note: d.note.clone(),
            actor_json: js(&d.by),
            at: d.at.clone(),
        })
        .collect();
    let ratings: Vec<cv_store::LinkFeedbackRow> = s
        .ratings
        .iter()
        .map(|x| cv_store::LinkFeedbackRow {
            key: x.key.clone(),
            link_id: x.id.clone(),
            verdict: x.verdict.clone(),
            note: x.note.clone(),
            actor_json: js(&x.by),
            at: x.at.clone(),
            link_json: x.link.as_ref().map(js),
        })
        .collect();
    r.db()
        .put_link_records(&declared, &ratings)
        .map_err(internal)
}

/// The review service when it is already open (read paths never open review.db themselves).
fn open_review(app: &AppState) -> Option<Arc<Review>> {
    app.review.lock().unwrap().clone()
}

fn review(app: &Arc<AppState>) -> R<Arc<Review>> {
    app.review().map_err(from_review)
}

fn now() -> String {
    cv_store::now_iso()
}

// ─── Rendering ───────────────────────────────────────────────────────────────

/// Name of a window's symbol (or its file name), for `←` port labels.
pub fn symbol_name(v: &View, w: usize) -> String {
    let h = &v.windows[w].header;
    h.symbol
        .as_ref()
        .map(|s| s.qualname.clone())
        .unwrap_or_else(|| h.path.rsplit('/').next().unwrap_or(&h.path).to_string())
}

/// One indexed link as the wire `Link`: channel links keep their port label, provenance and
/// `{ dynamic }` target (D11).
pub fn render_link(v: &View, l: &LinkRec) -> Link {
    let chan = l.channel.as_deref();
    let (from, to) = (&v.windows[l.from].header, &v.windows[l.to].header);
    Link {
        id: l.id.clone(),
        kind: l.kind.into(),
        from: SiteRef {
            path: from.path.clone(),
            line: l.from_line,
            window: Some(from.id.clone()),
            col: None,
            symbol: None,
        },
        to: match chan.and_then(|c| c.dynamic.clone()) {
            Some(d) => d,
            None => json!({ "path": to.path, "line": l.to_line, "window": to.id }),
        },
        label: match chan {
            Some(c) => c.port.clone(),
            None => format!("→ {}", l.label),
        },
        confidence: l.confidence,
        provenance: chan
            .map(|c| c.provenance.clone())
            .unwrap_or_else(|| "tree-sitter".into()),
        feedback: None,
        note: None,
    }
}

/// What a link connects, independent of view ids: `kind|from path:line|to`.
pub fn link_key(l: &Link) -> String {
    let to = if let Some(p) = l.to.get("path").and_then(Value::as_str) {
        format!(
            "{p}:{}",
            l.to.get("line").and_then(Value::as_u64).unwrap_or(0)
        )
    } else if let Some(h) = l.to.get("hub").and_then(Value::as_str) {
        format!("hub:{h}")
    } else {
        format!(
            "dyn:{}",
            l.to.pointer("/dynamic/hint")
                .and_then(Value::as_str)
                .unwrap_or("")
        )
    };
    format!("{}|{}:{}|{to}", l.kind, l.from.path, l.from.line)
}

/// The window of `v` showing `path:line`.
pub fn window_at(v: &View, path: &str, line: u32) -> Option<usize> {
    let &fi = v.by_path.get(path)?;
    v.files[fi].windows.iter().copied().find(|&w| {
        let l = &v.windows[w].header.lines;
        l.start <= line && line <= l.end
    })
}

fn provenance(by: &Actor) -> String {
    match by {
        Actor::Agent { .. } => "agent".into(),
        _ => "human".into(),
    }
}

/// A declared link as seen from `v` (sites carry the window when `v` shows them).
pub fn render_declared(v: Option<&View>, d: &Declared) -> Link {
    let site = |s: &Site| SiteRef {
        path: s.path.clone(),
        line: s.line,
        window: v
            .and_then(|v| window_at(v, &s.path, s.line).map(|w| v.windows[w].header.id.clone())),
        col: None,
        symbol: None,
    };
    Link {
        id: d.id.clone(),
        kind: d.kind.clone(),
        from: site(&d.from),
        to: match &d.to {
            End::Site(s) => serde_json::to_value(site(s)).unwrap_or_default(),
            End::Hub { hub } => json!({ "hub": hub }),
        },
        label: d.label.clone(),
        confidence: 1.0,
        provenance: provenance(&d.by),
        feedback: None,
        note: d.note.clone(),
    }
}

// ─── Per-view overlay of the store ───────────────────────────────────────────

/// A declared link placed in one view.
pub struct Placed {
    pub link: Link,
    pub from_w: Option<usize>,
    pub to_w: Option<usize>,
    pub from_line: u32,
    pub to_line: u32,
}

/// The store as it applies to one view: verdicts by this view's link ids, and declared links.
#[derive(Default)]
pub struct Overlay {
    pub feedback: HashMap<String, String>,
    pub declared: Vec<Placed>,
}

impl Overlay {
    fn is_empty(&self) -> bool {
        self.feedback.is_empty() && self.declared.is_empty()
    }

    fn rejected(&self, id: &str) -> bool {
        self.feedback.get(id).is_some_and(|f| f == "rejected")
    }
}

fn build_overlay(v: &View, s: &Store) -> Overlay {
    let verdicts: HashMap<&str, &str> = s
        .ratings
        .iter()
        .map(|r| (r.key.as_str(), r.verdict.as_str()))
        .collect();
    let mut o = Overlay::default();
    let mut present: HashSet<String> = HashSet::new();
    if !verdicts.is_empty() {
        for l in &v.links {
            let k = link_key(&render_link(v, l));
            if let Some(f) = verdicts.get(k.as_str()) {
                o.feedback.insert(l.id.clone(), f.to_string());
                present.insert(k);
            }
        }
    }
    for d in &s.declared {
        let mut link = render_declared(Some(v), d);
        if let Some(f) = verdicts.get(link_key(&link).as_str()) {
            link.feedback = Some(f.to_string());
            o.feedback.insert(d.id.clone(), f.to_string());
        }
        let (to_w, to_line) = match &d.to {
            End::Site(t) => (window_at(v, &t.path, t.line), t.line),
            End::Hub { .. } => (None, 0),
        };
        present.insert(link_key(&link));
        o.declared.push(Placed {
            link,
            from_w: window_at(v, &d.from.path, d.from.line),
            to_w,
            from_line: d.from.line,
            to_line,
        });
    }
    // Confirmed links this index did not produce come back as they were rated (§4.5 tier 3).
    for r in s.ratings.iter().filter(|r| r.verdict == "confirmed") {
        let Some(rated) = &r.link else { continue };
        if present.contains(&r.key) {
            continue;
        }
        let mut link = rated.clone();
        link.id = format!("rc-{:012x}", cv_lang::hash(&r.key) & 0xffff_ffff_ffff);
        link.feedback = Some("confirmed".into());
        let from_w = window_at(v, &link.from.path, link.from.line);
        link.from.window = from_w.map(|w| v.windows[w].header.id.clone());
        let (to_w, to_line) = match (
            link.to.get("path").and_then(Value::as_str),
            link.to.get("line").and_then(Value::as_u64),
        ) {
            (Some(p), Some(line)) => {
                let w = window_at(v, p, line as u32);
                link.to = json!({
                    "path": p,
                    "line": line,
                    "window": w.map(|w| v.windows[w].header.id.clone()),
                });
                (w, line as u32)
            }
            // A hub end (config field, topic): drawn through the hub, no second window.
            _ if link.to.get("hub").is_some() => (None, 0),
            _ => continue,
        };
        o.feedback.insert(link.id.clone(), "confirmed".into());
        o.declared.push(Placed {
            from_line: link.from.line,
            link,
            from_w,
            to_w,
            to_line,
        });
    }
    o
}

type OverlayCache = Mutex<HashMap<String, (u64, Arc<Overlay>)>>;

fn overlay_cache() -> &'static OverlayCache {
    static C: OnceLock<OverlayCache> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// The store's overlay for `v`, cached per view (instance) and store content. `None` while
/// review.db is not open or nothing was declared or rated.
pub fn overlay(app: &AppState, v: &View) -> Option<Arc<Overlay>> {
    let r = open_review(app)?;
    // Bumped by every write of declared links or ratings; None until the first.
    let h = r.db().links_rev().ok().flatten()? as u64;
    let key = format!("{}@{:p}@{:p}", v.id(), v as *const View, Arc::as_ptr(&r));
    if let Some((hh, o)) = overlay_cache().lock().unwrap().get(&key)
        && *hh == h
    {
        return (!o.is_empty()).then(|| o.clone());
    }
    let store = load_store(&r);
    let o = Arc::new(build_overlay(v, &store));
    let mut c = overlay_cache().lock().unwrap();
    if c.len() > 64 {
        c.clear();
    }
    c.insert(key, (h, o.clone()));
    (!o.is_empty()).then_some(o)
}

/// T2: verdicts on the returned links, declared links touching the requested windows (with their
/// ports and port bits).
pub fn overlay_batch(
    app: &AppState,
    v: &View,
    req: &WindowBatchRequest,
    res: &mut WindowBatchResponse,
) {
    let Some(o) = overlay(app, v) else { return };
    for l in &mut res.links {
        if let Some(f) = o.feedback.get(&l.id) {
            l.feedback = Some(f.clone());
        }
    }
    let wanted: HashSet<&str> = req.ids.iter().map(String::as_str).collect();
    for p in &o.declared {
        for (side, w, line) in [("out", p.from_w, p.from_line), ("in", p.to_w, p.to_line)] {
            let Some(w) = w else { continue };
            let h = &v.windows[w].header;
            if !wanted.contains(h.id.as_str()) {
                continue;
            }
            if !res.links.iter().any(|l| l.id == p.link.id) {
                res.links.push(p.link.clone());
            }
            let Some(out) = res.outlines.iter_mut().find(|x| x.id == h.id) else {
                continue;
            };
            match out
                .ports
                .iter_mut()
                .find(|x| x.side == side && x.line == line)
            {
                Some(port) => {
                    if !port.links.contains(&p.link.id) {
                        port.links.push(p.link.id.clone());
                    }
                }
                None => out.ports.push(Port {
                    id: format!("{side}{line}"),
                    side: side.into(),
                    line,
                    label: if side == "out" {
                        format!("→ {}", p.link.label)
                    } else {
                        format!("← {}", p.link.label)
                    },
                    kind: p.link.kind.clone(),
                    links: vec![p.link.id.clone()],
                }),
            }
            let b64 = base64::engine::general_purpose::STANDARD;
            if let Ok(mut sil) = b64.decode(&out.silhouette)
                && line >= h.lines.start
                && let Some(b) = sil.get_mut((line - h.lines.start) as usize * 3 + 2)
            {
                *b |= PORT_BIT;
                out.silhouette = b64.encode(sil);
            }
        }
    }
    res.links.sort_by(|a, b| a.id.cmp(&b.id));
    res.links.dedup_by(|a, b| a.id == b.id);
}

/// T1: rejected links leave the tile; declared links between two windows join it.
pub fn overlay_tile(app: &AppState, v: &View, z: u32, x: i64, y: i64, res: &mut TileResponse) {
    let Some(o) = overlay(app, v) else { return };
    res.edges.retain(|e| !o.rejected(&e.id));
    let span = 1i64 << z.min(40);
    let rect = Rect {
        x: (x * span) as f64 * TILE_BASE_WU,
        y: (y * span) as f64 * TILE_BASE_WU,
        w: span as f64 * TILE_BASE_WU,
        h: span as f64 * TILE_BASE_WU,
    };
    let inside = |p: [f64; 2]| {
        p[0] >= rect.x && p[0] < rect.x + rect.w && p[1] >= rect.y && p[1] < rect.y + rect.h
    };
    for p in &o.declared {
        let (Some(f), Some(t)) = (p.from_w, p.to_w) else {
            continue;
        };
        if f == t || o.rejected(&p.link.id) {
            continue;
        }
        let rec = LinkRec {
            id: p.link.id.clone(),
            kind: "reference",
            from: f,
            from_line: p.from_line,
            to: t,
            to_line: p.to_line,
            confidence: 1.0,
            label: String::new(),
            channel: None,
        };
        let (a, b) = link_points(&v.windows, &rec);
        if !(inside(a) || inside(b)) {
            continue;
        }
        let (fh, th) = (&v.windows[f].header, &v.windows[t].header);
        res.edges.push(TileEdge {
            id: p.link.id.clone(),
            kind: p.link.kind.clone(),
            from: fh.id.clone(),
            to: th.id.clone(),
            from_cluster: fh.cluster.clone(),
            to_cluster: th.cluster.clone(),
            a,
            b,
            confidence: 1.0,
        });
    }
}

// ─── GET /links ──────────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LinksQuery {
    pub view_id: String,
    pub target: String,
    #[serde(default)]
    pub dir: Option<String>,
    #[serde(default)]
    pub kinds: Option<String>,
    #[serde(default)]
    pub depth: Option<u8>,
    /// `headers=1`: also return the headers of every window the walk reached (the UI's linked-code
    /// panel shows ends that aren't loaded).
    #[serde(default)]
    pub headers: Option<u8>,
}

/// Windows of `v` a target names: a WindowId, `path:line`, a path, a hub id (its sites) or a
/// symbol (exact qualname or name, else a `.name` / `::name` suffix).
pub fn target_windows(app: &AppState, v: &View, target: &str) -> Vec<usize> {
    if let Some(&w) = v.by_id.get(target) {
        return vec![w];
    }
    if let Some((path, line)) = target.rsplit_once(':')
        && let Ok(line) = line.parse::<u32>()
        && let Some(w) = window_at(v, path, line)
    {
        return vec![w];
    }
    if let Some(&fi) = v.by_path.get(target) {
        return v.files[fi].windows.clone();
    }
    if let Some(h) = v.bootstrap.base.hubs.iter().find(|h| h.id == target) {
        let mut ws: Vec<usize> = h
            .producers
            .iter()
            .chain(&h.consumers)
            .filter_map(|s| window_at(v, &s.path, s.line))
            .collect();
        ws.sort_unstable();
        ws.dedup();
        return ws;
    }
    let by = |f: &dyn Fn(&cv_contracts::SymbolRef) -> bool| -> Vec<usize> {
        (0..v.windows.len())
            .filter(|&w| v.windows[w].header.symbol.as_ref().is_some_and(f))
            .collect()
    };
    let exact = by(&|s| s.qualname == target);
    if !exact.is_empty() {
        return exact;
    }
    let name = by(&|s| s.name == target);
    if !name.is_empty() {
        return name;
    }
    let dot = format!(".{target}");
    let colons = format!("::{target}");
    let suffix = by(&|s| s.qualname.ends_with(&dot) || s.qualname.ends_with(&colons));
    if !suffix.is_empty() {
        return suffix;
    }
    // Declarations inside windows (a small file is one window): the read side's symbol index.
    let syms = crate::readside::symbols_of(app, v);
    let mut ws: Vec<usize> = syms
        .iter()
        .filter(|s| s.qualname == target)
        .map(|s| s.window)
        .collect();
    if ws.is_empty() {
        ws = syms
            .iter()
            .filter(|s| {
                s.name == target || s.qualname.ends_with(&dot) || s.qualname.ends_with(&colons)
            })
            .map(|s| s.window)
            .collect();
    }
    ws.sort_unstable();
    ws.dedup();
    ws
}

/// `GET /links`: links around a target, breadth-first over windows up to `depth` (1–3) hops,
/// with verdicts and declared links, plus the hubs the channel links touch.
pub fn get_links(app: &AppState, q: &LinksQuery) -> R<Value> {
    let v = app.view(&q.view_id)?;
    let dir = q.dir.as_deref().unwrap_or("both");
    if !matches!(dir, "in" | "out" | "both") {
        return Err(invalid(format!("dir must be in, out or both, not {dir:?}")));
    }
    let depth = q.depth.unwrap_or(1).clamp(1, 3);
    let kinds: Option<HashSet<&str>> = q
        .kinds
        .as_deref()
        .filter(|k| !k.trim().is_empty())
        .map(|k| k.split(',').map(str::trim).collect());
    let kind_ok = |k: &str| kinds.as_ref().is_none_or(|ks| ks.contains(k));
    let start = target_windows(app, &v, &q.target);
    let hub_target = v.bootstrap.base.hubs.iter().find(|h| h.id == q.target);
    if start.is_empty() && hub_target.is_none() {
        return Err(not_found(format!("no window for target {:?}", q.target)));
    }
    let o = overlay(app, &v);
    let (out, inn) = (dir != "in", dir != "out");
    let mut seen: HashSet<usize> = start.iter().copied().collect();
    let mut frontier = start.clone();
    let mut links: Vec<Link> = Vec::new();
    let mut have: HashSet<String> = HashSet::new();
    let mut touched: HashSet<usize> = start.iter().copied().collect();
    let mut hub_ids: HashSet<String> = HashSet::new();
    let mut channel_hit = false;
    for _ in 0..depth {
        let mut next = Vec::new();
        for &w in &frontier {
            let win = &v.windows[w];
            let mut step = |link: Link, other: Option<usize>, links: &mut Vec<Link>| {
                if !have.insert(link.id.clone()) {
                    return;
                }
                if CHANNEL_KINDS.contains(&link.kind.as_str()) {
                    channel_hit = true;
                    touched.insert(w);
                    if let Some(o) = other {
                        touched.insert(o);
                    }
                }
                if let Some(h) = link.to.get("hub").and_then(Value::as_str) {
                    hub_ids.insert(h.to_string());
                }
                if let Some(o) = other
                    && seen.insert(o)
                {
                    next.push(o);
                }
                links.push(link);
            };
            if out {
                for &li in &win.out_links {
                    let l = &v.links[li];
                    if kind_ok(l.kind) {
                        let other = (!l.is_dynamic()).then_some(l.to);
                        step(render_link(&v, l), other, &mut links);
                    }
                }
            }
            if inn {
                for &li in &win.in_links {
                    let l = &v.links[li];
                    if kind_ok(l.kind) {
                        step(render_link(&v, l), Some(l.from), &mut links);
                    }
                }
            }
            if let Some(o) = &o {
                for p in &o.declared {
                    if !kind_ok(&p.link.kind) {
                        continue;
                    }
                    if out && p.from_w == Some(w) {
                        step(p.link.clone(), p.to_w, &mut links);
                    }
                    if inn && p.to_w == Some(w) {
                        step(p.link.clone(), p.from_w, &mut links);
                    }
                }
            }
        }
        frontier = next;
    }
    if let Some(o) = &o {
        for l in &mut links {
            if let Some(f) = o.feedback.get(&l.id) {
                l.feedback = Some(f.clone());
            }
        }
    }
    let want_hubs = kinds
        .as_ref()
        .is_none_or(|ks| ks.iter().any(|k| CHANNEL_KINDS.contains(k)));
    let touched_ids: HashSet<&str> = touched
        .iter()
        .map(|&w| v.windows[w].header.id.as_str())
        .collect();
    let hubs: Vec<&Hub> = v
        .bootstrap
        .base
        .hubs
        .iter()
        .filter(|h| {
            hub_target.is_some_and(|t| t.id == h.id)
                || hub_ids.contains(&h.id)
                || (want_hubs
                    && (channel_hit || hub_target.is_some())
                    && h.producers.iter().chain(&h.consumers).any(|s| {
                        s.window
                            .as_deref()
                            .is_some_and(|id| touched_ids.contains(id))
                    }))
        })
        .collect();
    if q.headers.unwrap_or(0) > 0 {
        let windows: Vec<&cv_contracts::WindowHeader> =
            seen.iter().map(|&w| &v.windows[w].header).collect();
        return Ok(json!({ "links": links, "hubs": hubs, "windows": windows }));
    }
    Ok(json!({ "links": links, "hubs": hubs }))
}

async fn links_route(State(app): St, Query(q): Query<LinksQuery>) -> R<Json<Value>> {
    Ok(Json(blocking(move || get_links(&app, &q)).await?))
}

// ─── GET /hubs/:hub ──────────────────────────────────────────────────────────

/// `GET /hubs/:hub`: a hub of the view's bootstrap, by id, or by key (optionally with protocol,
/// as MCP get_hub asks).
pub fn get_hub(app: &AppState, view: &str, hub: &str, protocol: Option<&str>) -> R<Hub> {
    let v = app.view(view)?;
    let hubs = &v.bootstrap.base.hubs;
    hubs.iter()
        .find(|h| h.id == hub)
        .or_else(|| {
            hubs.iter()
                .find(|h| h.key == hub && protocol.is_none_or(|p| h.protocol == p))
        })
        .cloned()
        .ok_or_else(|| not_found(format!("unknown hub {hub}")))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HubQ {
    view_id: String,
    protocol: Option<String>,
}

async fn hub_route(State(app): St, Path(hub): Path<String>, Query(q): Query<HubQ>) -> R<Json<Hub>> {
    Ok(Json(get_hub(
        &app,
        &q.view_id,
        &hub,
        q.protocol.as_deref(),
    )?))
}

// ─── Declared links and ratings ──────────────────────────────────────────────

fn cached_views(app: &AppState) -> Vec<Arc<View>> {
    app.views.read().unwrap().values().cloned().collect()
}

/// The view whose window ids responses should carry: the one asked for, else the human's.
fn preferred_view(app: &AppState, view: Option<&str>) -> Option<Arc<View>> {
    if let Some(id) = view {
        return app.view(id).ok();
    }
    let r = open_review(app)?;
    let (_, _, state) = r.view_state(None)?;
    app.view(state["viewId"].as_str()?).ok()
}

/// A SiteRef-ish string: a hub id, a WindowId, `path:line`, or a symbol of a cached view.
pub fn resolve_end(app: &AppState, s: &str) -> R<End> {
    let views = cached_views(app);
    for v in &views {
        if v.bootstrap.base.hubs.iter().any(|h| h.id == s) {
            return Ok(End::Hub { hub: s.into() });
        }
    }
    for v in &views {
        if let Some(&w) = v.by_id.get(s) {
            let h = &v.windows[w].header;
            return Ok(End::Site(Site {
                path: h.path.clone(),
                line: h.lines.start,
            }));
        }
    }
    if let Some((path, line)) = s.rsplit_once(':')
        && let Ok(line) = line.parse::<u32>()
        && !path.is_empty()
    {
        return Ok(End::Site(Site {
            path: path.into(),
            line: line.max(1),
        }));
    }
    for v in &views {
        if let Some(&w) = target_windows(app, v, s).first() {
            let h = &v.windows[w].header;
            return Ok(End::Site(Site {
                path: h.path.clone(),
                line: h.lines.start,
            }));
        }
    }
    Err(invalid(format!(
        "{s:?} is not a window id, path:line, hub id or known symbol"
    )))
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DeclareBody {
    pub from: String,
    pub to: String,
    pub kind: String,
    pub label: String,
    #[serde(default)]
    pub note: Option<String>,
    /// Additive: whose window ids the returned Link carries (default: the human's view).
    #[serde(default)]
    pub view_id: Option<String>,
}

/// view.invalidated (reason "index") for every cached view showing one of the sites.
fn invalidate_sites(app: &Arc<AppState>, sites: &[(&str, u32)]) -> R<()> {
    let r = review(app)?;
    for v in cached_views(app) {
        let mut windows: Vec<String> = sites
            .iter()
            .filter_map(|(p, l)| window_at(&v, p, *l).map(|w| v.windows[w].header.id.clone()))
            .collect();
        windows.dedup();
        if windows.is_empty() {
            continue;
        }
        r.emit(ServerEvent::ViewInvalidated {
            view_id: v.id().to_string(),
            layout_version: v.bootstrap.base.view.layout_version,
            reason: "index".into(),
            windows: Some(windows),
            tiles: None,
        });
    }
    Ok(())
}

fn link_sites(l: &Link) -> Vec<(String, u32)> {
    let mut s = vec![(l.from.path.clone(), l.from.line)];
    if let Some(p) = l.to.get("path").and_then(Value::as_str) {
        s.push((
            p.to_string(),
            l.to.get("line").and_then(Value::as_u64).unwrap_or(0) as u32,
        ));
    }
    s
}

/// `POST /links`: declare a link (provenance "agent", or "human" without X-CV-Agent). Idempotent
/// per (kind, from, to): declaring again updates label and note.
pub fn declare_link(app: &Arc<AppState>, who: &Actor, b: &DeclareBody) -> R<Link> {
    if !LINK_KINDS.contains(&b.kind.as_str()) {
        return Err(invalid(format!("unknown link kind {:?}", b.kind)));
    }
    if b.label.trim().is_empty() {
        return Err(invalid("label is empty"));
    }
    let from = match resolve_end(app, &b.from)? {
        End::Site(s) => s,
        End::Hub { .. } => {
            return Err(invalid(
                "from must be a code site (window id, path:line or symbol)",
            ));
        }
    };
    let to = resolve_end(app, &b.to)?;
    let to_key = match &to {
        End::Site(s) => format!("{}:{}", s.path, s.line),
        End::Hub { hub } => format!("hub:{hub}"),
    };
    let id = format!(
        "dl-{:012x}",
        cv_lang::hash(&format!("{}|{}:{}|{to_key}", b.kind, from.path, from.line))
            & 0xffff_ffff_ffff
    );
    let d = Declared {
        id: id.clone(),
        kind: b.kind.clone(),
        from,
        to,
        label: b.label.clone(),
        note: b.note.clone(),
        by: who.clone(),
        at: now(),
    };
    let r = review(app)?;
    let feedback = {
        let _g = store_lock().lock().unwrap();
        let mut s = load_store(&r);
        s.declared.retain(|x| x.id != id);
        s.declared.push(d.clone());
        save_store(&r, &s)?;
        let key = link_key(&render_declared(None, &d));
        s.ratings
            .iter()
            .find(|x| x.key == key)
            .map(|x| x.verdict.clone())
    };
    let v = preferred_view(app, b.view_id.as_deref());
    let mut link = render_declared(v.as_deref(), &d);
    link.feedback = feedback;
    let sites = link_sites(&link);
    let sites: Vec<(&str, u32)> = sites.iter().map(|(p, l)| (p.as_str(), *l)).collect();
    invalidate_sites(app, &sites)?;
    Ok(link)
}

async fn declare_route(
    State(app): St,
    headers: HeaderMap,
    Json(b): Json<DeclareBody>,
) -> R<Json<Link>> {
    let who = crate::review_api::actor(&headers);
    Ok(Json(blocking(move || declare_link(&app, &who, &b)).await?))
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RateBody {
    pub verdict: String,
    #[serde(default)]
    pub note: Option<String>,
    /// Additive: the view the link id comes from (ids are per view). Default: the human's view,
    /// then any cached view.
    #[serde(default)]
    pub view_id: Option<String>,
}

/// The link with id `id`: a declared link, else an indexed link of the preferred view or any
/// cached view.
fn find_link(app: &AppState, id: &str, view: Option<&str>) -> R<Link> {
    let pref = preferred_view(app, view);
    if let Some(r) = open_review(app)
        && let Some(d) = load_store(&r).declared.iter().find(|d| d.id == id)
    {
        return Ok(render_declared(pref.as_deref(), d));
    }
    if view.is_some() && pref.is_none() {
        return Err(crate::routes::unknown_view(view.unwrap_or("")));
    }
    let mut views: Vec<Arc<View>> = pref.into_iter().collect();
    if view.is_none() {
        views.extend(cached_views(app));
    }
    for v in views {
        if let Some(l) = v.links.iter().find(|l| l.id == id) {
            return Ok(render_link(&v, l));
        }
    }
    Err(not_found(format!("unknown link {id}")))
}

/// `POST /links/:id/rate`: confirm or reject a link. The verdict sticks to what the link connects
/// and shows as `feedback` wherever the link is served; rejected links leave the tiles.
pub fn rate_link(app: &Arc<AppState>, who: &Actor, id: &str, b: &RateBody) -> R<Link> {
    if !matches!(b.verdict.as_str(), "confirmed" | "rejected") {
        return Err(invalid("verdict must be confirmed or rejected"));
    }
    // Open review.db first so declared links are found.
    let r = review(app)?;
    let mut link = find_link(app, id, b.view_id.as_deref())?;
    let key = link_key(&link);
    {
        let _g = store_lock().lock().unwrap();
        let mut s = load_store(&r);
        s.ratings.retain(|x| x.key != key);
        s.ratings.push(Rating {
            key,
            id: id.into(),
            verdict: b.verdict.clone(),
            note: b.note.clone(),
            by: who.clone(),
            at: now(),
            link: Some(Link {
                feedback: None,
                ..link.clone()
            }),
        });
        save_store(&r, &s)?;
    }
    link.feedback = Some(b.verdict.clone());
    let sites = link_sites(&link);
    let sites: Vec<(&str, u32)> = sites.iter().map(|(p, l)| (p.as_str(), *l)).collect();
    invalidate_sites(app, &sites)?;
    Ok(link)
}

async fn rate_route(
    State(app): St,
    headers: HeaderMap,
    Path(link): Path<String>,
    Json(b): Json<RateBody>,
) -> R<Json<Link>> {
    let who = crate::review_api::actor(&headers);
    Ok(Json(
        blocking(move || rate_link(&app, &who, &link, &b)).await?,
    ))
}

// ─── Cluster labels ──────────────────────────────────────────────────────────

#[derive(Deserialize, Clone)]
pub struct LabelBody {
    pub label: String,
    #[serde(default)]
    pub summary: Option<String>,
}

/// `PUT /clusters/:id/label`: a human or agent label (and summary) for cluster `c{index}`. Kept
/// across re-layouts (layout_input::set_cluster_label), set on every cached view, which then gets
/// view.invalidated (reason "layout", no windows: clients reload the bootstrap).
pub fn label_cluster(app: &Arc<AppState>, who: &Actor, cluster: &str, b: &LabelBody) -> R<Value> {
    let label = b.label.trim();
    if label.is_empty() {
        return Err(invalid("label is empty"));
    }
    let index: usize = cluster
        .strip_prefix('c')
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| not_found(format!("unknown cluster {cluster}")))?;
    let source = match who {
        Actor::Agent { .. } => "agent",
        _ => "human",
    };
    let mut found = crate::layout_input::set_cluster_label(
        &app.repo.root().display().to_string(),
        index,
        label,
        source,
    );
    let mut changed = Vec::new();
    {
        let mut views = app.views.write().unwrap();
        for v in views.values_mut() {
            // Cluster ids name the district grid's clusters; the folder tree's are directories.
            if crate::layout_api::is_folders(v)
                || !v.bootstrap.base.clusters.iter().any(|c| c.id == cluster)
            {
                continue;
            }
            let mut nv = (**v).clone();
            for c in nv
                .bootstrap
                .base
                .clusters
                .iter_mut()
                .filter(|c| c.id == cluster)
            {
                c.label = label.to_string();
                c.label_source = source.into();
                if b.summary.is_some() {
                    c.summary = b.summary.clone();
                }
            }
            changed.push((nv.id().to_string(), nv.bootstrap.base.view.layout_version));
            *v = Arc::new(nv);
            found = true;
        }
    }
    if !found {
        return Err(not_found(format!("unknown cluster {cluster}")));
    }
    crate::layout_api::persist_label(app, cluster, index, label, source, b.summary.as_deref())?;
    let r = review(app)?;
    for (view_id, layout_version) in changed {
        r.emit(ServerEvent::ViewInvalidated {
            view_id,
            layout_version,
            reason: "layout".into(),
            windows: Some(vec![]),
            tiles: None,
        });
    }
    Ok(json!({ "ok": true }))
}

async fn label_route(
    State(app): St,
    headers: HeaderMap,
    Path(cluster): Path<String>,
    Json(b): Json<LabelBody>,
) -> R<Json<Value>> {
    let who = crate::review_api::actor(&headers);
    Ok(Json(
        blocking(move || label_cluster(&app, &who, &cluster, &b)).await?,
    ))
}

// ─── Agent navigation ────────────────────────────────────────────────────────

/// Whether the human lets this agent move the camera (agent permissions, DESIGN §10).
pub fn may_move_camera(app: &Arc<AppState>, who: &Actor) -> bool {
    crate::agent_api::may_move_camera(app, who.agent_name())
}

/// `POST /agent/view`: open (or find) the view for `spec` and ask the UI to switch to it
/// (`view.requested`). 403 "not-allowed" unless the human allowed camera moves.
pub fn set_view(app: &Arc<AppState>, who: &Actor, spec: &Value) -> R<Value> {
    if !may_move_camera(app, who) {
        return Err(err(
            StatusCode::FORBIDDEN,
            "not-allowed",
            "The human has not allowed the agent to move the camera; use focus to suggest a place instead.",
        ));
    }
    let view_ref = if spec["mode"] == "range" {
        crate::readside::range::get_or_build(app, spec)?
            .view_ref
            .clone()
    } else {
        serde_json::to_value(&app.view_for_spec(spec)?.bootstrap.base.view).map_err(internal)?
    };
    let view_id = view_ref["id"].as_str().unwrap_or_default().to_string();
    review(app)?.emit(ServerEvent::ViewRequested {
        view_id,
        spec: spec.clone(),
        by: who.clone(),
    });
    Ok(view_ref)
}

#[derive(Deserialize)]
struct SetViewBody {
    spec: Value,
}

async fn set_view_route(
    State(app): St,
    headers: HeaderMap,
    Json(b): Json<SetViewBody>,
) -> R<Json<Value>> {
    let who = crate::review_api::actor(&headers);
    Ok(Json(blocking(move || set_view(&app, &who, &b.spec)).await?))
}

// ─── Interdiff ───────────────────────────────────────────────────────────────

pub use cv_contracts::api::{FileDiff, FileHunk as Hunk, InterdiffResponse};

const CONTEXT: usize = 3;

/// Hunks between two texts with [`CONTEXT`] lines around each change. Every line carries its
/// text (an interdiff has no blob to read the new side from).
pub fn hunks(old: &str, new: &str) -> Vec<Hunk> {
    let (ol, nl) = (cv_lang::split_lines(old), cv_lang::split_lines(new));
    let text = |ls: &[&str], i: usize| {
        ls.get(i)
            .map(|s| s.trim_end_matches(['\n', '\r']).to_string())
    };
    // (kind, old index, new index) per line, and the old/new cursor before it.
    let mut ops: Vec<(&'static str, Option<usize>, Option<usize>)> = Vec::new();
    for run in cv_vcs::read::line_runs(old.as_bytes(), new.as_bytes(), false) {
        if run.same {
            for (o, n) in run.old.clone().zip(run.new.clone()) {
                ops.push(("ctx", Some(o), Some(n)));
            }
        } else {
            ops.extend(run.old.clone().map(|o| ("del", Some(o), None)));
            ops.extend(run.new.clone().map(|n| ("add", None, Some(n))));
        }
    }
    let changed: Vec<usize> = (0..ops.len()).filter(|&i| ops[i].0 != "ctx").collect();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for &i in &changed {
        let (s, e) = (i.saturating_sub(CONTEXT), (i + CONTEXT + 1).min(ops.len()));
        match spans.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => spans.push((s, e)),
        }
    }
    // Lines of each side consumed before op i.
    let mut before = Vec::with_capacity(ops.len() + 1);
    let (mut o, mut n) = (0u32, 0u32);
    for op in &ops {
        before.push((o, n));
        o += u32::from(op.1.is_some());
        n += u32::from(op.2.is_some());
    }
    spans
        .into_iter()
        .map(|(s, e)| {
            let lines: Vec<DiffLine> = ops[s..e]
                .iter()
                .map(|&(kind, oi, ni)| DiffLine {
                    kind: kind.into(),
                    old_line: oi.map(|i| i as u32 + 1),
                    new_line: ni.map(|i| i as u32 + 1),
                    text: match (oi, ni) {
                        (_, Some(i)) => text(&nl, i),
                        (Some(i), None) => text(&ol, i),
                        _ => None,
                    },
                    owner: None,
                })
                .collect();
            let oc = lines.iter().filter(|l| l.old_line.is_some()).count() as u32;
            let nc = lines.iter().filter(|l| l.new_line.is_some()).count() as u32;
            let (bo, bn) = before[s];
            Hunk {
                old: [if oc > 0 { bo + 1 } else { bo }, oc],
                new: [if nc > 0 { bn + 1 } else { bn }, nc],
                lines,
            }
        })
        .collect()
}

/// `GET /interdiff`: what changed in `change` since commit `from` (e.g. the commit the human
/// reviewed): file diffs from `from`'s tree to the change's current commit, and the description
/// pair when it changed.
pub fn interdiff(app: &AppState, change: &str, from: &str) -> R<InterdiffResponse> {
    let repo = app.head_repo()?;
    let cur = repo
        .resolve_one(change)
        .map_err(|e| not_found(format!("change {change}: {e}")))?;
    let old = if from.len() == 40 && from.bytes().all(|b| b.is_ascii_hexdigit()) {
        repo.commit_by_id(from)
    } else {
        repo.resolve_one(from)
    }
    .map_err(|e| not_found(format!("commit {from}: {e}")))?;
    // jj interdiff: the reviewed commit rebased onto the current parents first.
    let changes = repo.interdiff(&old.commit, &cur.commit).map_err(internal)?;
    let read = |path: &str, blob: &Option<String>| -> R<String> {
        Ok(match blob {
            Some(b) => {
                String::from_utf8_lossy(&repo.read_blob(path, b).map_err(internal)?).into_owned()
            }
            None => String::new(),
        })
    };
    let mut files = Vec::new();
    for c in &changes {
        let status = match (&c.before, &c.after) {
            (None, Some(_)) => "added",
            (Some(_), None) => "removed",
            _ => "modified",
        };
        files.push(FileDiff {
            path: c.path.clone(),
            old_path: None,
            status: status.into(),
            hunks: hunks(&read(&c.path, &c.before)?, &read(&c.path, &c.after)?),
            conflicted: false,
        });
    }
    Ok(InterdiffResponse {
        files,
        description: (old.description != cur.description)
            .then(|| (old.description.clone(), cur.description.clone())),
    })
}

/// The cumulative diff of a linear range for MCP get_diff: from the merged parents of its root
/// to its head, every line with its text, with the real file status. `fileset` is a jj fileset.
pub fn range_diff(app: &Arc<AppState>, revset: &str, fileset: Option<&str>) -> R<Value> {
    let repo = app.head_repo()?;
    let filter = cv_vcs::Fileset::parse(fileset).map_err(|e| invalid(e.to_string()))?;
    // Resolved as range views resolve it (revset aliases such as trunk(), workspace@ symbols).
    let env = repo.revset_env();
    let info = repo
        .analyze_range(&env, revset, 5000)
        .map_err(|e| invalid(format!("revset {revset:?}: {e}")))?;
    // The oldest root and the newest head, as the range view's base and tip.
    let (Some(root), Some(head)) = (info.roots.last().cloned(), info.head().cloned()) else {
        return Ok(json!({ "files": [] }));
    };
    let changes = repo
        .range_diff(&root.commit, &head.commit)
        .map_err(internal)?;
    let base = format!("parents({})", root.commit);
    let files = file_diffs(app, &repo, &changes, &base, &head.commit, &filter)?;
    Ok(json!({ "files": files, "root": root.commit, "head": head.commit }))
}

/// MCP get_diff {from, to}: the files that differ between the trees of two revisions, as
/// `jj diff --from <from> --to <to>` (no range, no ancestry needed). Each side must resolve to
/// exactly one commit.
pub fn tree_diff(app: &Arc<AppState>, from: &str, to: &str, fileset: Option<&str>) -> R<Value> {
    let repo = app.head_repo()?;
    let filter = cv_vcs::Fileset::parse(fileset).map_err(|e| invalid(e.to_string()))?;
    let env = repo.revset_env();
    let one = |rev: &str| -> R<cv_vcs::CommitInfo> {
        let mut found = repo
            .eval(&env, rev, 2)
            .map_err(|e| invalid(format!("revset {rev:?}: {e}")))?;
        if found.len() != 1 {
            return Err(invalid(format!(
                "revset {rev:?} resolved to {} commits, expected one",
                found.len()
            )));
        }
        Ok(found.remove(0))
    };
    let (f, t) = (one(from)?, one(to)?);
    let changes = repo.tree_diff(&f.commit, &t.commit).map_err(internal)?;
    let files = file_diffs(app, &repo, &changes, &f.commit, &t.commit, &filter)?;
    Ok(json!({ "files": files, "from": f.commit, "to": t.commit }))
}

/// FileDiffs with every line's text; files conflicted on either side (`base`: the old side's
/// revset, `head`: the new side's) have no hunks: a conflicted side has no blob to diff.
fn file_diffs(
    app: &Arc<AppState>,
    repo: &cv_vcs::JjRepo,
    changes: &[cv_vcs::read::FileChange],
    base: &str,
    head: &str,
    filter: &cv_vcs::Fileset,
) -> R<Vec<FileDiff>> {
    let read = |path: &str, blob: &Option<String>| -> R<String> {
        Ok(match blob {
            Some(b) => {
                String::from_utf8_lossy(&repo.read_blob(path, b).map_err(internal)?).into_owned()
            }
            None => String::new(),
        })
    };
    // A conflicted file has no blob on its side: it is neither removed nor added.
    let conflicted_at = |rev: &str| -> HashSet<String> {
        review(app)
            .ok()
            .and_then(|r| r.list_conflicts(Some(rev)).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|c| c.path)
            .collect()
    };
    let (old_conflicted, new_conflicted) = (conflicted_at(base), conflicted_at(head));
    let mut files = Vec::new();
    for c in changes.iter().filter(|c| filter.matches(&c.path)) {
        let (was, is) = (
            old_conflicted.contains(&c.path),
            new_conflicted.contains(&c.path),
        );
        if was || is {
            let before = c.before.is_some() || was;
            let after = c.after.is_some() || is;
            files.push(FileDiff {
                path: c.path.clone(),
                old_path: None,
                status: match (before, after) {
                    (false, _) => "added",
                    (true, false) => "removed",
                    _ => "modified",
                }
                .into(),
                hunks: vec![],
                conflicted: true,
            });
            continue;
        }
        let status = match (&c.before, &c.after) {
            (None, Some(_)) => "added",
            (Some(_), None) => "removed",
            _ => "modified",
        };
        files.push(FileDiff {
            path: c.path.clone(),
            old_path: None,
            status: status.into(),
            hunks: hunks(&read(&c.path, &c.before)?, &read(&c.path, &c.after)?),
            conflicted: false,
        });
    }
    Ok(files)
}

#[derive(Deserialize)]
struct InterdiffQ {
    change: String,
    from: String,
}

#[derive(Deserialize)]
struct DiffQ {
    from: String,
    to: String,
    fileset: Option<String>,
}

/// `GET /diff?from&to[&fileset]`: [`tree_diff`] (MCP get_diff {from, to} over HTTP).
async fn diff_route(State(app): St, Query(q): Query<DiffQ>) -> R<Json<Value>> {
    Ok(Json(
        blocking(move || tree_diff(&app, &q.from, &q.to, q.fileset.as_deref())).await?,
    ))
}

async fn interdiff_route(
    State(app): St,
    Query(q): Query<InterdiffQ>,
) -> R<Json<InterdiffResponse>> {
    Ok(Json(
        blocking(move || interdiff(&app, &q.change, &q.from)).await?,
    ))
}
