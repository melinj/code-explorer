//! cv-mock: answers the canvas routes of `docs/contracts/http.ts` from a synthetic view.
//!
//! Usage: `cv-mock [--windows N] [--port P] [--latency-ms L] [--ui DIR]`. Defaults: 20000 windows,
//! port 7878. `--ui web/dist` also serves the built UI, so the mock alone is a demo (`start.sh --demo`).

mod events;
mod extras;
mod graph;
mod layout;
mod panels;
mod range;
mod review;
mod synth;
mod text;

#[cfg(test)]
mod route_tests;

use std::{sync::Arc, time::Duration};

use axum::http::HeaderMap;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::Engine;
use cv_contracts::*;
use serde::Deserialize;
use serde_json::json;
use tower_http::compression::CompressionLayer;
use tower_http::services::{ServeDir, ServeFile};

struct App {
    /// The synthetic view; a layout PATCH swaps in a moved copy.
    view: std::sync::RwLock<Arc<synth::View>>,
    latency: Duration,
    review: std::sync::Mutex<review::Review>,
    events: Arc<events::Events>,
    panels: panels::Panels,
    ranges: std::sync::Mutex<std::collections::HashMap<String, Arc<range::RangeView>>>,
    /// Off once a real agent binds (or with --no-sim-agent): the simulated one would race it.
    sim_agent: std::sync::atomic::AtomicBool,
    tours: std::sync::Mutex<Vec<serde_json::Value>>,
    /// Per-session permissions (DESIGN §10): UI session → { edit, moveCamera }. In force for the
    /// agent: it may edit while no session has editing off, and move the camera when any allows it.
    permissions: std::sync::Mutex<std::collections::HashMap<String, serde_json::Value>>,
    /// Saved ranges (SavedRange JSON), in creation order; in memory only.
    saved_ranges: std::sync::Mutex<Vec<serde_json::Value>>,
    /// Declared links, link ratings and review marks.
    extras: std::sync::Mutex<extras::Extras>,
    /// Bookmarks of the Graph view (an overlay on the synthetic history).
    graph: std::sync::Mutex<graph::Graph>,
}

impl App {
    fn view(&self) -> Arc<synth::View> {
        self.view.read().unwrap().clone()
    }

    /// The range view with this id, if it is one.
    fn range(&self, id: &str) -> Option<Arc<range::RangeView>> {
        self.ranges.lock().unwrap().get(id).cloned()
    }

    fn knows(&self, id: &str) -> bool {
        id == self.view().bootstrap.view.id || self.range(id).is_some()
    }
}

type St = State<Arc<App>>;

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

#[tokio::main]
async fn main() {
    let n: usize = arg("--windows")
        .and_then(|s| s.parse().ok())
        .unwrap_or(20_000);
    let port: u16 = arg("--port").and_then(|s| s.parse().ok()).unwrap_or(7878);
    let latency = Duration::from_millis(
        arg("--latency-ms")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
    );
    let sim_agent = !std::env::args().any(|a| a == "--no-sim-agent");
    let app = build(n, latency, sim_agent);
    let mut router = router(app);
    let ui = arg("--ui").map(std::path::PathBuf::from);
    if let Some(dir) = &ui {
        // As cv serve: static files, and index.html for any other path (the app has one page).
        router = router
            .fallback_service(ServeDir::new(dir).fallback(ServeFile::new(dir.join("index.html"))));
    }
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    eprintln!("cv-mock on http://127.0.0.1:{port}");
    if ui.is_some() {
        eprintln!("  UI:  http://127.0.0.1:{port}/");
    }
    axum::serve(listener, router).await.unwrap();
}

/// The synthetic view of `n` windows and its in-memory state.
fn build(n: usize, latency: Duration, sim_agent: bool) -> Arc<App> {
    let t = std::time::Instant::now();
    let view = synth::generate(n, 7);
    let b = &view.bootstrap;
    eprintln!(
        "synthetic view {}: {} windows, {} clusters, {} links, world {:.0}×{:.0} wu, built in {:?}",
        b.view.id,
        view.windows.len(),
        b.clusters.len(),
        view.links.len(),
        b.world.w,
        b.world.h,
        t.elapsed()
    );
    let review = std::sync::Mutex::new(review::Review::new(&view));
    let panels = panels::Panels::new(&view);
    let graph = std::sync::Mutex::new(graph::Graph::new(&panels.revisions));
    Arc::new(App {
        view: std::sync::RwLock::new(Arc::new(view)),
        latency,
        review,
        events: Arc::new(events::Events::new()),
        panels,
        ranges: Default::default(),
        sim_agent: std::sync::atomic::AtomicBool::new(sim_agent),
        tours: Default::default(),
        permissions: std::sync::Mutex::new(std::collections::HashMap::new()),
        saved_ranges: Default::default(),
        extras: Default::default(),
        graph,
    })
}

/// Every route of `docs/contracts/http.ts`.
fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/api/v1/views", post(create_view))
        .route("/api/v1/views/{view}", get(extras::get_view))
        .route("/api/v1/views/{view}/bootstrap", get(bootstrap))
        .route("/api/v1/views/{view}/tiles/{z}/{x}/{y}", get(tile))
        .route("/api/v1/views/{view}/windows:batch", post(batch))
        .route("/api/v1/views/{view}/windows/{window}/expand", post(expand))
        .route("/api/v1/views/{view}/windows/{window}/collapse", post(collapse))
        .route("/api/v1/views/{view}/fragments", get(fragments))
        .route("/api/v1/blobs/{blob}/text", get(blob_text))
        .route("/api/v1/threads", get(threads).post(new_thread))
        .route("/api/v1/threads/{id}/comments", post(thread_comment))
        .route(
            "/api/v1/threads/{id}",
            get(extras::get_thread).patch(thread_patch),
        )
        .route(
            "/api/v1/reviews/marks/{change}",
            axum::routing::put(extras::mark),
        )
        .route("/api/v1/edits", post(new_edit))
        .route("/api/v1/edits/{id}", get(extras::get_edit))
        .route("/api/v1/edits/{id}/apply", post(apply_edit))
        .route("/api/v1/edits/{id}/undo", post(extras::undo_edit))
        .route("/api/v1/ws", get(ws))
        .route("/api/v1/reviews:submit", post(submit_review))
        .route("/api/v1/events", get(events_poll))
        .route("/api/v1/agent/attention", post(agent_attention))
        .route("/api/v1/agent/bind", post(agent_bind))
        .route("/api/v1/agent/view-state", get(agent_view_state))
        .route("/api/v1/agent/focus", post(agent_focus))
        .route("/api/v1/agent/tours", post(agent_tour))
        .route("/api/v1/tours", get(tours))
        .route(
            "/api/v1/agent/permissions",
            get(get_permissions).put(put_permissions),
        )
        .route("/api/v1/conflicts", get(conflicts))
        .route(
            "/api/v1/conflicts/{commit}/{*path}",
            get(conflict_detail).post(resolve_conflict),
        )
        .route("/api/v1/revisions", get(revisions))
        .route(
            "/api/v1/setup",
            get(|| async {
                Json(json!({ "vcs": "jj", "root": "/home/dev/workspace/sample-app", "jj": { "installed": true, "version": "0.46.0" }, "initCommand": "jj git init --colocate" }))
            }),
        )
        .route("/api/v1/refs", get(graph::refs))
        .route("/api/v1/graph:op", post(graph::op))
        .route("/api/v1/graph:undo", post(graph::undo))
        .route("/api/v1/ops", get(graph::ops))
        .route("/api/v1/git:fetch", post(graph::no_remote))
        .route("/api/v1/git:push", post(graph::no_remote))
        .route("/api/v1/workspaces", get(workspaces))
        .route("/api/v1/workspaces:compare", post(compare_workspaces))
        .route("/api/v1/views/{view}/metrics", get(metrics))
        .route("/api/v1/views/{view}/structure", get(structure))
        .route("/api/v1/search", get(search))
        .route("/api/v1/links", get(links).post(extras::declare_link))
        .route("/api/v1/links/{link}/rate", post(extras::rate_link))
        .route("/api/v1/hubs/{*hub}", get(extras::get_hub))
        .route("/api/v1/interdiff", get(extras::interdiff))
        .route("/api/v1/diff", get(extras::diff))
        .route(
            "/api/v1/clusters/{cluster}/label",
            axum::routing::put(extras::label_cluster),
        )
        .route("/api/v1/agent/view", post(extras::agent_view))
        .route("/api/v1/revsets:validate", post(validate))
        .route(
            "/api/v1/views/{view}/layout",
            axum::routing::patch(patch_layout),
        )
        .route("/api/v1/ranges", get(saved_ranges))
        .route(
            "/api/v1/ranges/{name}",
            axum::routing::put(put_range).delete(delete_range),
        )
        .route("/api/v1/repo", get(repo_info))
        .route("/api/v1/lanes", get(lanes))
        .layer(CompressionLayer::new())
        .with_state(app)
}

async fn delay(app: &App) {
    if !app.latency.is_zero() {
        tokio::time::sleep(app.latency).await;
    }
}

/// 404 "view-unknown", as cv serve answers an id it has not loaded (http.ts: POST the spec again).
fn view_unknown(view: &str) -> Response {
    api_error(
        StatusCode::NOT_FOUND,
        "view-unknown",
        format!(
            "unknown view {view}: not loaded (the server restarted, or the view was dropped); POST /views with its spec again"
        ),
    )
}

fn not_found(what: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "code": "not-found", "message": what })),
    )
        .into_response()
}

async fn create_view(State(app): St, body: Option<Json<serde_json::Value>>) -> Response {
    let spec = body.map(|b| b.0).unwrap_or_default();
    // As cv serve: "districts", "folders" or "flow" (the mock draws all as its one synthetic map).
    let layout = &spec["options"]["layout"];
    if !layout.is_null() && !["districts", "folders", "flow"].iter().any(|l| layout == l) {
        return bad(format!(
            "options.layout {layout}: expected \"districts\", \"folders\" or \"flow\""
        ));
    }
    // As cv serve: bad options are a 400, never silently the defaults.
    if spec["mode"] == "range"
        && let Err(e) = range::check_options(&spec["options"])
    {
        return bad(e);
    }
    if spec["mode"] != "range" {
        let opts = &spec["options"];
        if !(opts.is_null() || opts.is_object()) {
            return bad("options must be an object");
        }
        let live = &opts["liveWorkingCopy"];
        if !(live.is_null() || live.is_boolean()) {
            return bad(format!(
                "options.liveWorkingCopy {live}: expected a boolean"
            ));
        }
        match &opts["workspace"] {
            serde_json::Value::Null => {}
            serde_json::Value::String(w) => {
                let known = app
                    .panels
                    .workspaces
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|c| c["name"] == w.as_str());
                if !known {
                    return bad(format!("options.workspace {w:?}: no such workspace"));
                }
            }
            other => {
                return bad(format!(
                    "options.workspace {other}: expected a workspace name"
                ));
            }
        }
        if !(spec["fileset"].is_null() || spec["fileset"].is_string()) {
            return api_error(
                StatusCode::BAD_REQUEST,
                "fileset-invalid",
                format!("fileset {}: expected a string", spec["fileset"]),
            );
        }
    }
    if spec["mode"] == "range" {
        let revset = spec["revset"].as_str().unwrap_or("trunk()..@");
        let r = Arc::new(range::RangeView::new(&app.view(), revset, &spec["options"]));
        let out = r.ref_json(&app.view().bootstrap.view.id);
        app.ranges.lock().unwrap().insert(r.id.clone(), r);
        return Json(out).into_response();
    }
    // The one synthetic snapshot serves every revision; its ViewRef echoes the spec asked for, with
    // the rev spelled out as cv serve does ("<workspace>@" when only options.workspace is given).
    let mut out = app.view().bootstrap.view.clone();
    if spec.is_object() {
        let mut s = spec.clone();
        s["mode"] = json!("snapshot");
        if s["rev"].is_null() {
            s["rev"] = json!(match s["options"]["workspace"].as_str() {
                Some(w) => format!("{w}@"),
                None => "@".into(),
            });
        }
        out.spec = s;
    }
    Json(out).into_response()
}

async fn bootstrap(State(app): St, Path(view): Path<String>) -> Response {
    delay(&app).await;
    if let Some(r) = app.range(&view) {
        return Json(&r.bootstrap).into_response();
    }
    if view != app.view().bootstrap.view.id {
        return view_unknown(&view);
    }
    Json(&app.view().bootstrap).into_response()
}

async fn tile(State(app): St, Path((view, z, x, y)): Path<(String, u32, i64, i64)>) -> Response {
    if !app.knows(&view) {
        return view_unknown(&view);
    }
    if z > 12 {
        return not_found("tile level out of range");
    }
    delay(&app).await;
    let v = &app.view();
    let range = app.range(&view);
    let shown = |w: usize| range.as_ref().is_none_or(|r| r.contains(w));
    let span = 1i64 << z;
    let rect = Rect {
        x: (x * span) as f64 * TILE_BASE_WU,
        y: (y * span) as f64 * TILE_BASE_WU,
        w: span as f64 * TILE_BASE_WU,
        h: span as f64 * TILE_BASE_WU,
    };
    let mut seen_w = vec![false; v.windows.len()];
    let mut seen_l = vec![false; v.links.len()];
    let (mut windows, mut edges) = (Vec::new(), Vec::new());
    for tx in x * span..(x + 1) * span {
        for (_, (ws, ls)) in v.tiles.range((tx, y * span)..(tx, (y + 1) * span)) {
            for &w in ws {
                if !seen_w[w] && shown(w) && v.windows[w].header.rect.intersects(&rect) {
                    seen_w[w] = true;
                    windows.push(match &range {
                        Some(r) => r.header(v, w),
                        None => serde_json::to_value(&v.windows[w].header).unwrap(),
                    });
                }
            }
            for &l in ls {
                if !seen_l[l] && shown(v.links[l].from) && shown(v.links[l].to) {
                    seen_l[l] = true;
                    let link = &v.links[l];
                    let (a, b) = synth::link_points(&v.windows, link);
                    edges.push(TileEdge {
                        id: link.id.clone(),
                        kind: link.kind.into(),
                        from: v.windows[link.from].header.id.clone(),
                        to: v.windows[link.to].header.id.clone(),
                        from_cluster: v.windows[link.from].header.cluster.clone(),
                        to_cluster: v.windows[link.to].header.cluster.clone(),
                        a,
                        b,
                        confidence: link.confidence,
                    });
                }
            }
        }
    }
    let res = json!({ "layoutVersion": v.bootstrap.view.layout_version, "windows": windows, "edges": edges });
    ([(header::CACHE_CONTROL, "no-cache")], Json(res)).into_response()
}

#[derive(serde::Deserialize)]
struct LinksQ {
    target: String,
    #[serde(default)]
    dir: Option<String>,
    #[serde(default)]
    headers: Option<u8>,
}

/// `GET /links?target=<window>&dir&headers=1`: a window's links one hop out (no depth, kinds or
/// hubs here), with the far ends' headers when asked.
async fn links(State(app): St, Query(q): Query<LinksQ>) -> Response {
    let v = &app.view();
    let Some(&w) = v.by_id.get(&q.target) else {
        return not_found("unknown window");
    };
    delay(&app).await;
    let dir = q.dir.as_deref().unwrap_or("both");
    let win = &v.windows[w];
    let mut ids: Vec<usize> = Vec::new();
    if dir != "in" {
        ids.extend(&win.out_links);
    }
    if dir != "out" {
        ids.extend(&win.in_links);
    }
    let mut seen = vec![w];
    let links: Vec<Link> = ids
        .iter()
        .map(|&li| {
            let l = &v.links[li];
            let (f, t) = (&v.windows[l.from].header, &v.windows[l.to].header);
            for o in [l.from, l.to] {
                if !seen.contains(&o) {
                    seen.push(o);
                }
            }
            Link {
                id: l.id.clone(),
                kind: l.kind.into(),
                from: SiteRef {
                    path: f.path.clone(),
                    line: l.from_line,
                    window: Some(f.id.clone()),
                    col: None,
                    symbol: None,
                },
                to: json!({ "path": t.path, "line": l.to_line, "window": t.id }),
                label: t
                    .symbol
                    .as_ref()
                    .map(|s| s.name.clone())
                    .unwrap_or_default(),
                confidence: l.confidence,
                provenance: "tree-sitter".into(),
                feedback: None,
                note: None,
            }
        })
        .collect();
    let mut out = json!({ "links": links, "hubs": [] });
    extras::decorate_links(&app, &q.target, dir, &mut out);
    if q.headers.unwrap_or(0) > 0 {
        out["windows"] = json!(
            seen.iter()
                .map(|&i| &v.windows[i].header)
                .collect::<Vec<_>>()
        );
    }
    Json(out).into_response()
}

async fn batch(
    State(app): St,
    Path(view): Path<String>,
    Json(req): Json<WindowBatchRequest>,
) -> Response {
    if !app.knows(&view) {
        return view_unknown(&view);
    }
    let range = app.range(&view);
    if req.ids.len() > 200 {
        return (StatusCode::BAD_REQUEST, "at most 200 ids").into_response();
    }
    delay(&app).await;
    let v = &app.view();
    let mut res = WindowBatchResponse {
        outlines: vec![],
        diffs: vec![],
        links: vec![],
        missing: vec![],
    };
    let mut file_cache: std::collections::HashMap<usize, Vec<text::Line>> = Default::default();
    for id in &req.ids {
        let Some(&wi) = v.by_id.get(id) else {
            res.missing.push(id.clone());
            continue;
        };
        let w = &v.windows[wi];
        let h = &w.header;
        let lines = file_cache
            .entry(w.file)
            .or_insert_with(|| app.review.lock().unwrap().lines(v, w.file));
        let mut ports = Vec::new();
        for (side, list) in [("out", &w.out_links), ("in", &w.in_links)] {
            for &li in list.iter() {
                let l = &v.links[li];
                let (line, other) = if side == "out" {
                    (l.from_line, l.to)
                } else {
                    (l.to_line, l.from)
                };
                let name = v.windows[other]
                    .header
                    .symbol
                    .as_ref()
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                let arrow = if side == "out" { "→" } else { "←" };
                ports.push(Port {
                    id: format!("{side}{li}"),
                    side: side.into(),
                    line,
                    label: format!("{arrow} {name}"),
                    kind: l.kind.into(),
                    links: vec![l.id.clone()],
                });
                res.links.push(Link {
                    id: l.id.clone(),
                    kind: l.kind.into(),
                    from: SiteRef {
                        path: v.windows[l.from].header.path.clone(),
                        line: l.from_line,
                        window: Some(v.windows[l.from].header.id.clone()),
                        col: None,
                        symbol: None,
                    },
                    to: serde_json::json!({
                        "path": v.windows[l.to].header.path,
                        "line": l.to_line,
                        "window": v.windows[l.to].header.id,
                    }),
                    label: format!("{arrow} {name}"),
                    confidence: l.confidence,
                    provenance: "tree-sitter".into(),
                    feedback: None,
                    note: None,
                });
            }
        }
        // Some publishers build their topic at runtime: no tracing (D11), so they end in a "?" stub.
        if synth::hash(&h.id).is_multiple_of(17) && h.lines.end > h.lines.start + 3 {
            let line = h.lines.start + 2;
            let kind = ["entity", "district", "player"][(synth::hash(&h.path) % 3) as usize];
            let id = format!("dyn-{}", h.id);
            ports.push(Port {
                id: id.clone(),
                side: "out".into(),
                line,
                label: format!("⇢ ? world.*.{kind}"),
                kind: "publish".into(),
                links: vec![id.clone()],
            });
            res.links.push(Link {
                id,
                kind: "publish".into(),
                from: SiteRef { path: h.path.clone(), line, window: Some(h.id.clone()), col: None,
 symbol: None },
                to: json!({ "dynamic": { "protocol": "bus", "hint": format!("f\"world.{{{kind}_kind}}.{kind}\""), "partial": format!("world.*.{kind}") } }),
                label: format!("⇢ ? world.*.{kind}"),
                confidence: 0.5,
                provenance: "tree-sitter".into(),
                feedback: None,
                note: None,
            });
        }
        let port_lines: Vec<u32> = ports.iter().map(|p| p.line).collect();
        let sil = text::silhouette(lines, h.lines.start, h.lines.end, &port_lines);
        let headings = (h.lines.start..=h.lines.end.min(lines.len() as u32))
            .filter(|&n| {
                let t = &lines[(n - 1) as usize].text;
                t.starts_with("def ") || t.starts_with("class ") || t.starts_with("## ")
            })
            .map(|n| Heading {
                line: n,
                depth: 0,
                label: lines[(n - 1) as usize].text.clone(),
            })
            .collect();
        if let Some(r) = &range
            && req.parts.iter().any(|p| p == "diff")
            && r.changed.contains(&wi)
        {
            res.diffs.push(r.diff(v, wi, lines));
        }
        res.outlines.push(WindowOutline {
            id: id.clone(),
            silhouette: base64::engine::general_purpose::STANDARD.encode(sil),
            headings,
            ports,
        });
    }
    res.links.sort_by(|a, b| a.id.cmp(&b.id));
    res.links.dedup_by(|a, b| a.id == b.id);
    Json(res).into_response()
}

#[derive(Deserialize)]
struct FragQ {
    path: String,
}

async fn fragments(State(app): St, Path(view): Path<String>, Query(q): Query<FragQ>) -> Response {
    if !app.knows(&view) {
        return view_unknown(&view);
    }
    let v = &app.view();
    let Some(f) = v.files.iter().find(|f| f.path == q.path) else {
        return not_found("unknown path");
    };
    let frags: Vec<serde_json::Value> = f
        .windows
        .iter()
        .enumerate()
        .map(|(i, &w)| {
            let h = &v.windows[w].header;
            serde_json::json!({ "window": h.id, "index": i + 1, "rect": h.rect, "cluster": h.cluster })
        })
        .collect();
    Json(serde_json::json!({
        "path": f.path,
        "chip": synth::hash(&f.path) % FILE_CHIP_SLOTS as u64,
        "fragments": frags,
    }))
    .into_response()
}

#[derive(Deserialize)]
struct TextQ {
    from: Option<u32>,
    to: Option<u32>,
}

async fn blob_text(State(app): St, Path(blob): Path<String>, Query(q): Query<TextQ>) -> Response {
    let v = &app.view();
    let Some(&fi) = v.by_blob.get(&blob) else {
        return not_found("unknown blob");
    };
    delay(&app).await;
    let f = &v.files[fi];
    let lines = app.review.lock().unwrap().lines(v, fi);
    let n = lines.len() as u32;
    let from = q.from.unwrap_or(1).max(1).min(n.max(1));
    let to = q.to.unwrap_or(n).min(n);
    let slice = &lines[(from - 1) as usize..to as usize];
    let res = BlobText {
        blob,
        lang: f.lang.clone(),
        from,
        to,
        lines: slice.iter().map(|l| l.text.clone()).collect(),
        tokens: slice.iter().map(|l| l.tokens.clone()).collect(),
    };
    // Real blobs are content-addressed and immutable; the mock edits files in place under the same
    // blob id, so it must not let the browser cache them.
    ([(header::CACHE_CONTROL, "no-store")], Json(res)).into_response()
}

#[derive(Deserialize)]
struct ThreadQ {
    path: Option<String>,
}

async fn threads(State(app): St, Query(q): Query<ThreadQ>) -> Response {
    Json(app.review.lock().unwrap().list(q.path.as_deref())).into_response()
}

fn bad(msg: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "code": "invalid", "message": msg.into() })),
    )
        .into_response()
}

async fn new_thread(
    State(app): St,
    headers: HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Response {
    let res = app.review.lock().unwrap().as_actor(&headers).create(&req);
    match res {
        Ok(t) => {
            app.events
                .emit(json!({ "type": "thread.upserted", "thread": t }));
            if t["assignee"] == "agent" {
                tokio::spawn(simulated_agent(
                    app.clone(),
                    t["id"].as_str().unwrap().to_string(),
                ));
            }
            Json(t).into_response()
        }
        Err(e) => bad(e),
    }
}

async fn thread_comment(
    State(app): St,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Response {
    let res = app
        .review
        .lock()
        .unwrap()
        .as_actor(&headers)
        .comment(&id, &req);
    match res {
        Some(t) => {
            app.events
                .emit(json!({ "type": "thread.upserted", "thread": t }));
            Json(t).into_response()
        }
        None => not_found("unknown thread"),
    }
}

async fn thread_patch(
    State(app): St,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Response {
    let res = app
        .review
        .lock()
        .unwrap()
        .as_actor(&headers)
        .patch(&id, &req);
    match res {
        Some(t) => {
            app.events
                .emit(json!({ "type": "thread.upserted", "thread": t }));
            if req.get("assignee").is_some_and(|a| a == "agent") {
                tokio::spawn(simulated_agent(app.clone(), id));
            }
            Json(t).into_response()
        }
        None => not_found("unknown thread"),
    }
}

async fn new_edit(
    State(app): St,
    headers: HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Response {
    if let Some(r) = agent_may_not_edit(&app, &headers) {
        return r;
    }
    let res = app
        .review
        .lock()
        .unwrap()
        .as_actor(&headers)
        .edit(&app.view(), &req);
    match res {
        Ok(r) => {
            if r["status"] == "applied" {
                applied(&app, &r, r["actor"].clone());
            }
            Json(r).into_response()
        }
        Err(e) => bad(e),
    }
}

async fn apply_edit(State(app): St, Path(id): Path<String>, headers: HeaderMap) -> Response {
    if let Some(r) = agent_may_not_edit(&app, &headers) {
        return r;
    }
    let mut rev = app.review.lock().unwrap();
    let actor = rev.as_actor(&headers).actor.clone();
    let res = rev.apply(&app.view(), &id);
    drop(rev);
    match res {
        Some(r) => {
            applied(&app, &r, actor);
            Json(r).into_response()
        }
        None => not_found("unknown edit"),
    }
}

/// After an edit lands: the edit, a new op, and which windows to refetch.
fn applied(app: &App, r: &serde_json::Value, actor: serde_json::Value) {
    let ev = &app.events;
    ev.emit(json!({ "type": "edit.updated", "edit": r }));
    ev.emit(json!({ "type": "op.changed", "op": r["opAfter"], "changes": r["rewritten"], "actor": actor }));
    let paths = app.review.lock().unwrap().paths_of(&r["id"]);
    let view = app.view();
    let windows: Vec<&String> = view
        .windows
        .iter()
        .filter(|w| paths.contains(&w.header.path))
        .map(|w| &w.header.id)
        .collect();
    ev.emit(json!({
        "type": "view.invalidated", "viewId": view.bootstrap.view.id, "layoutVersion": view.bootstrap.view.layout_version, "reason": "op", "windows": windows
    }));
}

/// The stand-in for an MCP-bound coding agent: picks up a thread assigned to it, looks at the code
/// (agent.attention), then either amends the code itself (tasks) or answers with a suggestion.
async fn simulated_agent(app: Arc<App>, thread: String) {
    if !app.sim_agent.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let Some(t) = app.review.lock().unwrap().get(&thread) else {
        return;
    };
    let path = t["anchor"]["path"].as_str().unwrap_or("").to_string();
    let start = t["anchor"]["start"].as_u64().unwrap_or(1) as u32;
    let end = t["anchor"]["end"].as_u64().unwrap_or(start as u64) as u32;
    let kind = t["kind"].as_str().unwrap_or("").to_string();
    let first_body = t["comments"][0]["body"].as_str().unwrap_or("").to_string();
    let view = app.view();
    let Some(fi) = view.files.iter().position(|f| f.path == path) else {
        return;
    };
    let window = view.files[fi]
        .windows
        .iter()
        .map(|&w| &view.windows[w].header)
        .find(|h| h.lines.start <= start && start <= h.lines.end)
        .map(|h| h.id.clone());
    let at = review::now;
    let attention = |tool: &str, note: &str| {
        json!({
            "type": "agent.attention", "agent": "claude", "workspace": "gym", "tool": tool,
            "windows": window.iter().collect::<Vec<_>>(), "note": note, "at": at()
        })
    };
    tokio::time::sleep(events::AGENT_STEP).await;
    app.events
        .emit(attention("get_thread", "reading the thread"));
    let t = app
        .review
        .lock()
        .unwrap()
        .patch(&thread, &json!({ "status": "in-progress" }));
    if let Some(t) = t {
        app.events
            .emit(json!({ "type": "thread.upserted", "thread": t }));
    }
    tokio::time::sleep(events::AGENT_STEP).await;
    app.events
        .emit(attention("read_window", "looking at the anchored lines"));
    let lines = app.review.lock().unwrap().lines(&app.view(), fi);
    let new: Vec<String> = (start..=end)
        .filter_map(|n| lines.get(n as usize - 1))
        .map(|l| events::refactor(&l.text))
        .collect();
    tokio::time::sleep(events::AGENT_STEP).await;
    let conflict = app
        .review
        .lock()
        .unwrap()
        .conflict_detail(&app.view(), &path);
    let asks_conflict = first_body.to_lowercase().contains("conflict");
    let (body, status) = if let Some(d) = conflict.filter(|_| asks_conflict) {
        // Conflicts: keep the rebased side's rename and the amend's retry note, then resolve.
        app.events
            .emit(attention("resolve_conflict", "merging both sides"));
        let region = &d["regions"][0];
        let ours: Vec<&str> = region["sides"][0]["text"]
            .as_str()
            .unwrap_or("")
            .split('\n')
            .collect();
        let theirs: Vec<&str> = region["sides"][1]["text"]
            .as_str()
            .unwrap_or("")
            .split('\n')
            .collect();
        let merged: Vec<String> = theirs
            .iter()
            .zip(ours.iter())
            .map(|(t, o)| match o.split_once("  # ") {
                Some((_, note)) => format!("{t}  # {note}"),
                None => t.to_string(),
            })
            .collect();
        let lines = app.review.lock().unwrap().lines(&app.view(), fi);
        let s0 = region["start"].as_u64().unwrap_or(1) as usize;
        let e0 = region["end"].as_u64().unwrap_or(1) as usize;
        let mut content: Vec<String> = lines[..s0 - 1].iter().map(|l| l.text.clone()).collect();
        content.extend(merged);
        content.extend(lines[e0..].iter().map(|l| l.text.clone()));
        let r = app.review.lock().unwrap().resolve_conflict(
            &app.view(),
            &path,
            &content.join("\n"),
            true,
        );
        match r {
            Some(r) => {
                conflict_resolved(&app, &path, &r);
                let file = path.rsplit('/').next().unwrap_or(&path);
                (
                    format!(
                        "Resolved the conflict in {file} (lines {s0}–{e0}): kept the rename from the rebased side and the retry note from the amend."
                    ),
                    "addressed",
                )
            }
            None => (
                "The conflict was already resolved.".to_string(),
                "addressed",
            ),
        }
    } else if kind == "task" && !app.agent_may("edit") {
        // Not allowed to edit: propose the change instead.
        (
            format!(
                "Editing is switched off for agents, so here is the change as a suggestion:\n```suggestion\n{}\n```",
                new.join("\n")
            ),
            "addressed",
        )
    } else if kind == "task" {
        // Tasks: the agent edits the change itself (amend) and reports back.
        app.events
            .emit(attention("apply_edit", "amending the change"));
        let req = json!({
            "target": "kxwqmnpo", "mode": "amend", "dryRun": false, "origin": { "thread": thread },
            "files": [{ "path": path, "patches": [{ "start": start, "end": end, "text": new.join("\n") }] }]
        });
        let r = app.review.lock().unwrap().edit(&app.view(), &req);
        match r {
            Ok(r) => {
                applied(
                    &app,
                    &r,
                    json!({ "kind": "agent", "name": "claude", "client": "claude-code" }),
                );
                (
                    format!("Done: amended kxwqmnpo, lines {start}–{end}. Please take a look."),
                    "addressed",
                )
            }
            Err(e) => (format!("I couldn't apply that: {e}"), "open"),
        }
    } else {
        (
            format!("Here's a fix:\n```suggestion\n{}\n```", new.join("\n")),
            "addressed",
        )
    };
    let t = app
        .review
        .lock()
        .unwrap()
        .agent_comment(&thread, &body, Some(status));
    if let Some(t) = t {
        app.events
            .emit(json!({ "type": "thread.upserted", "thread": t }));
    }
}

#[derive(Deserialize)]
struct WsQ {
    cursor: Option<u64>,
}

async fn ws(State(app): St, Query(q): Query<WsQ>, up: axum::extract::WebSocketUpgrade) -> Response {
    let events = app.events.clone();
    up.on_upgrade(move |socket| events.serve(socket, q.cursor))
}

async fn revisions(State(app): St) -> Response {
    let rows = app.graph.lock().unwrap().rows(&app.panels.revisions);
    Json(json!({ "rows": rows, "truncated": false })).into_response()
}

async fn workspaces(State(app): St) -> Response {
    Json(app.panels.workspaces.lock().unwrap().clone()).into_response()
}

#[derive(Deserialize)]
struct CompareReq {
    a: String,
    b: String,
}

/// POST /workspaces:compare { a, b } → ViewRef, as cv serve answers it (DESIGN §7 "Compare"): a
/// per-commit range view of both stacks filtered to the files both change. 404 for an unknown
/// workspace, 400 "invalid" for a == b, 400 "no-overlap" when the stacks share no file.
async fn compare_workspaces(State(app): St, Json(req): Json<CompareReq>) -> Response {
    let cards = app.panels.workspaces.lock().unwrap().clone();
    let files = |name: &str| -> Option<Vec<String>> {
        let card = cards.iter().find(|w| w["name"] == name)?;
        Some(
            card["files"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|f| f.as_str().map(String::from))
                .collect(),
        )
    };
    let (Some(fa), Some(fb)) = (files(&req.a), files(&req.b)) else {
        let missing = if files(&req.a).is_none() {
            &req.a
        } else {
            &req.b
        };
        return api_error(
            StatusCode::NOT_FOUND,
            "not-found",
            format!("no workspace {missing:?}; GET /workspaces lists them"),
        );
    };
    if req.a == req.b {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid",
            "compare two different workspaces".into(),
        );
    }
    let mut shared: Vec<String> = fa.into_iter().filter(|f| fb.contains(f)).collect();
    // The cards' overlap warnings name shared files too.
    for (me, other) in [(&req.a, &req.b), (&req.b, &req.a)] {
        for card in cards.iter().filter(|w| w["name"] == me.as_str()) {
            for o in card["overlaps"].as_array().into_iter().flatten() {
                if o["workspace"] == other.as_str() {
                    shared.extend(
                        o["paths"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|p| p.as_str().map(String::from)),
                    );
                }
            }
        }
    }
    shared.sort();
    shared.dedup();
    if shared.is_empty() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "no-overlap",
            format!(
                "the stacks of {} and {} change no file in common",
                req.a, req.b
            ),
        );
    }
    let revset = format!("(trunk()..{}@) | (trunk()..{}@)", req.a, req.b);
    let options = json!({ "diff": "per-commit", "contextHops": 0, "contextDisplay": "minimized",
        "contextLines": 3, "ignoreWhitespace": false, "allowNonChain": true });
    let r = Arc::new(range::RangeView::with_files(
        &app.view(),
        &revset,
        &options,
        &shared,
    ));
    let out = r.ref_json(&app.view().bootstrap.view.id);
    app.ranges.lock().unwrap().insert(r.id.clone(), r);
    Json(out).into_response()
}

#[derive(Deserialize)]
struct MetricsQ {
    level: Option<String>,
    limit: Option<usize>,
}

async fn metrics(State(app): St, Path(view): Path<String>, Query(q): Query<MetricsQ>) -> Response {
    if view != app.view().bootstrap.view.id {
        return view_unknown(&view);
    }
    Json(panels::Panels::metrics(
        &app.view(),
        q.level.as_deref().unwrap_or("cluster"),
        q.limit.unwrap_or(2000),
    ))
    .into_response()
}

/// As cv serve's structure_api: the synthetic map's folders against its links and heat.
async fn structure(State(app): St, Path(view): Path<String>) -> Response {
    let v = app.view();
    if view != v.bootstrap.view.id {
        return view_unknown(&view);
    }
    let first = |f: &synth::FileInfo| f.windows.first().map(|&w| &v.windows[w].header);
    let files: Vec<cv_layout::structure::FileIn> = v
        .files
        .iter()
        .map(|f| cv_layout::structure::FileIn {
            path: &f.path,
            lane: first(f).map_or("code", |h| h.lane.as_str()),
            heat: first(f).map_or(0, |h| h.heat.grade),
            rate: first(f).map_or(0.0, |h| h.heat.rate),
            lines: f.lines,
        })
        .collect();
    let windows: Vec<cv_layout::structure::WinIn> = v
        .windows
        .iter()
        .map(|w| cv_layout::structure::WinIn {
            key: &w.header.key,
            file: w.file,
            module: w
                .header
                .cluster
                .strip_prefix('c')
                .and_then(|n| n.parse().ok()),
        })
        .collect();
    let links: Vec<cv_layout::structure::LinkIn> = v
        .links
        .iter()
        .filter(|l| l.confidence >= 0.6)
        .map(|l| cv_layout::structure::LinkIn {
            from: l.from,
            to: l.to,
            import: l.kind == "import",
        })
        .collect();
    Json(cv_layout::structure::analyse(&files, &windows, &links)).into_response()
}

#[derive(Deserialize)]
struct SearchQ {
    q: String,
    limit: Option<usize>,
}

async fn search(State(app): St, Query(q): Query<SearchQ>) -> Response {
    let threads = app.review.lock().unwrap().list(None);
    Json(panels::Panels::search(
        &app.view(),
        &threads,
        &q.q,
        q.limit.unwrap_or(30),
    ))
    .into_response()
}

async fn validate(State(app): St, Json(req): Json<serde_json::Value>) -> Response {
    Json(app.panels.validate(req["revset"].as_str().unwrap_or(""))).into_response()
}

async fn conflicts(State(app): St) -> Response {
    Json(app.review.lock().unwrap().conflict_records(&app.view())).into_response()
}

async fn conflict_detail(
    State(app): St,
    Path((_commit, path)): Path<(String, String)>,
) -> Response {
    let d = app
        .review
        .lock()
        .unwrap()
        .conflict_detail(&app.view(), &path);
    match d {
        Some(d) => Json(d).into_response(),
        None => not_found("no open conflict in that file"),
    }
}

/// POST .../conflicts/{commit}/{path}/resolve: axum's wildcard takes "/resolve" with the path.
async fn resolve_conflict(
    State(app): St,
    Path((_commit, path)): Path<(String, String)>,
    Json(req): Json<serde_json::Value>,
) -> Response {
    let Some(path) = path.strip_suffix("/resolve") else {
        return bad("POST to .../resolve");
    };
    let content = req["content"].as_str().unwrap_or("");
    let r = app
        .review
        .lock()
        .unwrap()
        .resolve_conflict(&app.view(), path, content, false);
    match r {
        Some(r) => {
            conflict_resolved(&app, path, &r);
            Json(r).into_response()
        }
        None => not_found("no open conflict in that file"),
    }
}

fn conflict_resolved(app: &App, path: &str, r: &serde_json::Value) {
    app.events.emit(json!({
        "type": "conflicts.changed", "added": [],
        "resolved": [{ "change": "kxwqmnpo", "commit": "c0ffee", "path": path, "sides": 2, "firstSeenOp": "4eb0965a" }]
    }));
    applied(app, r, r["actor"].clone());
}

/// ExpandRequest: widen a window's line range by N lines (or to the file edge). The rect grows by
/// the same number of lines, upward for `before`, downward for `after`; neighbours do not move (the
/// client draws the expanded window on top).
async fn expand(
    State(app): St,
    Path((view, window)): Path<(String, String)>,
    Json(req): Json<serde_json::Value>,
) -> Response {
    if !app.knows(&view) {
        return view_unknown(&view);
    }
    let v = &app.view();
    let Some(&wi) = v.by_id.get(&window) else {
        return not_found("unknown window");
    };
    let mut h = serde_json::to_value(&v.windows[wi].header).unwrap();
    let start = h["lines"]["start"].as_u64().unwrap_or(1) as i64;
    let end = h["lines"]["end"].as_u64().unwrap_or(1) as i64;
    let file_lines = app
        .review
        .lock()
        .unwrap()
        .lines(v, v.windows[wi].file)
        .len() as i64;
    let amount = |x: &serde_json::Value, room: i64| match x {
        serde_json::Value::Number(n) => n.as_i64().unwrap_or(0).clamp(0, room),
        serde_json::Value::String(s) if s == "file" || s == "symbol" => room,
        _ => 0,
    };
    // The client sends the window's current range back in `lines` when it has already expanded it.
    let (cur_start, cur_end) = (
        req["lines"]["start"].as_i64().unwrap_or(start),
        req["lines"]["end"].as_i64().unwrap_or(end),
    );
    let before = amount(&req["before"], cur_start - 1);
    let after = amount(&req["after"], file_lines - cur_end);
    let (ns, ne) = (cur_start - before, cur_end + after);
    h["lines"]["start"] = json!(ns);
    h["lines"]["end"] = json!(ne);
    h["rect"]["y"] = json!(h["rect"]["y"].as_f64().unwrap() - (start - ns) as f64 * LINE_WU);
    h["rect"]["h"] = json!(HEADER_WU + (ne - ns + 1) as f64 * LINE_WU);
    h["flags"]["truncated"] = json!(ns > 1 || ne < file_lines);
    if (ns, ne) != (start, end) {
        h["expandedFrom"] = json!({ "start": start, "end": end });
    }
    Json(h).into_response()
}

/// Collapse: expands are overlays here, so the window's own header is its original size.
async fn collapse(State(app): St, Path((view, window)): Path<(String, String)>) -> Response {
    if !app.knows(&view) {
        return view_unknown(&view);
    }
    let v = &app.view();
    let Some(&wi) = v.by_id.get(&window) else {
        return not_found("unknown window");
    };
    Json(&v.windows[wi].header).into_response()
}

#[derive(Deserialize)]
struct EventsQ {
    cursor: Option<String>,
    kinds: Option<String>,
    #[serde(rename = "timeoutS")]
    timeout_s: Option<u64>,
}

/// GET /events: long-poll the event log (what MCP's wait_for_events uses).
async fn events_poll(State(app): St, Query(q): Query<EventsQ>) -> Response {
    let cursor = q.cursor.and_then(|c| c.parse().ok()).unwrap_or(0);
    let kinds = q
        .kinds
        .map(|k| k.split(',').map(|s| s.trim().to_string()).collect());
    let timeout = Duration::from_secs(q.timeout_s.unwrap_or(25).min(60));
    let (c, events) = app.events.poll(cursor, kinds, timeout).await;
    Json(json!({ "cursor": c.to_string(), "events": events })).into_response()
}

async fn agent_attention(State(app): St, Json(mut req): Json<serde_json::Value>) -> Response {
    req["type"] = json!("agent.attention");
    if req.get("at").is_none() {
        req["at"] = json!(review::now());
    }
    app.events.emit(req);
    Json(json!({ "ok": true, "cursor": app.events.cursor().to_string() })).into_response()
}

/// An agent binds to a workspace: its card shows the agent, and the simulated agent stands down.
async fn agent_bind(State(app): St, Json(req): Json<serde_json::Value>) -> Response {
    app.sim_agent
        .store(false, std::sync::atomic::Ordering::Relaxed);
    let ws = req["workspace"].as_str().unwrap_or("default");
    let mut cards = app.panels.workspaces.lock().unwrap();
    let Some(card) = cards.iter_mut().find(|c| c["name"] == ws) else {
        return not_found("unknown workspace");
    };
    card["agent"] =
        json!({ "name": req["agent"], "activity": "reading", "lastSeen": review::now() });
    if let Some(c) = req["client"].as_str() {
        card["agent"]["client"] = json!(c);
    }
    let out = card.clone();
    drop(cards);
    app.events
        .emit(json!({ "type": "workspace.updated", "workspace": out }));
    Json(out).into_response()
}

async fn agent_view_state(State(app): St) -> Response {
    let state = app.events.view_state.lock().unwrap().clone();
    match state {
        Some(s) => Json(json!({ "view": app.view().bootstrap.view, "state": s })).into_response(),
        None => Json(json!({ "view": null })).into_response(),
    }
}

/// The agent asks the human to look somewhere; the client follows only in "follow the agent" mode.
async fn agent_focus(
    State(app): St,
    headers: HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Response {
    let by = app.review.lock().unwrap().as_actor(&headers).actor.clone();
    app.events.emit(json!({ "type": "focus.request", "by": by, "target": req["target"], "lines": req["lines"], "note": req["note"] }));
    Json(json!({ "delivered": "suggested" })).into_response()
}

/// 403 for agent writes while the human has switched agent editing off.
fn agent_may_not_edit(app: &App, headers: &HeaderMap) -> Option<Response> {
    let agent = headers.contains_key("x-cv-agent");
    let allowed = app.agent_may("edit");
    (agent && !allowed).then(|| {
        (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": { "code": "agent-edit-disabled", "message": "The human has switched agent editing off for this session; propose the change in the thread instead." } })),
        )
            .into_response()
    })
}

async fn agent_tour(
    State(app): St,
    headers: HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Response {
    let by = app.review.lock().unwrap().as_actor(&headers).actor.clone();
    let mut tours = app.tours.lock().unwrap();
    let id = format!("tour{}", tours.len() + 1);
    let tour = json!({
        "id": id, "title": req["title"], "by": by, "steps": req["steps"],
        "createdAt": review::now()
    });
    tours.insert(0, tour.clone());
    drop(tours);
    app.events
        .emit(json!({ "type": "tour.created", "tour": tour }));
    Json(json!({ "tour": id })).into_response()
}

async fn tours(State(app): St) -> Response {
    Json(app.tours.lock().unwrap().clone()).into_response()
}

/// The UI session of a permissions request: `?session=`, the body's `session`, `X-CV-Session`, else "default".
fn perm_session(q: &PermQ, body: Option<&serde_json::Value>, headers: &HeaderMap) -> String {
    q.session
        .clone()
        .or_else(|| body.and_then(|b| b["session"].as_str()).map(String::from))
        .or_else(|| {
            headers
                .get("x-cv-session")
                .and_then(|v| v.to_str().ok())
                .map(String::from)
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "default".into())
}

#[derive(Deserialize, Default)]
struct PermQ {
    session: Option<String>,
}

impl App {
    /// A session's toggles (defaults: edit on, camera off).
    fn session_perms(&self, session: &str) -> serde_json::Value {
        let p = self.permissions.lock().unwrap();
        let row = p.get(session);
        json!({
            "edit": row.and_then(|r| r["edit"].as_bool()).unwrap_or(true),
            "moveCamera": row.and_then(|r| r["moveCamera"].as_bool()).unwrap_or(false),
            "session": session, "agent": "*",
        })
    }

    /// What is in force for the agent (http.ts): edit unless a session has it off; the camera
    /// when any session allows it.
    fn agent_may(&self, what: &str) -> bool {
        let p = self.permissions.lock().unwrap();
        match what {
            "edit" => p.values().all(|r| r["edit"].as_bool().unwrap_or(true)),
            _ => p
                .values()
                .any(|r| r["moveCamera"].as_bool().unwrap_or(false)),
        }
    }
}

async fn get_permissions(State(app): St, headers: HeaderMap, Query(q): Query<PermQ>) -> Response {
    // An agent without a session gets what is in force for it, as cv serve answers.
    let explicit =
        q.session.as_deref().is_some_and(|s| !s.is_empty()) || headers.contains_key("x-cv-session");
    if headers.contains_key("x-cv-agent") && !explicit {
        return Json(json!({ "edit": app.agent_may("edit"), "moveCamera": app.agent_may("moveCamera"), "effective": true })).into_response();
    }
    Json(app.session_perms(&perm_session(&q, None, &headers))).into_response()
}

async fn put_permissions(
    State(app): St,
    headers: HeaderMap,
    Query(q): Query<PermQ>,
    Json(req): Json<serde_json::Value>,
) -> Response {
    // As cv serve: unknown fields (or wrong types) are a 400.
    const KNOWN: [&str; 5] = ["edit", "moveCamera", "session", "agent", "effective"];
    let Some(obj) = req.as_object() else {
        return bad("agent permissions: expected an object");
    };
    if let Some(k) = obj.keys().find(|k| !KNOWN.contains(&k.as_str())) {
        return bad(format!("agent permissions: unknown field `{k}`"));
    }
    for k in ["edit", "moveCamera", "effective"] {
        if obj.get(k).is_some_and(|v| !v.is_boolean() && !v.is_null()) {
            return bad(format!("agent permissions: `{k}` must be a boolean"));
        }
    }
    let session = perm_session(&q, Some(&req), &headers);
    {
        let mut p = app.permissions.lock().unwrap();
        let row = p.entry(session.clone()).or_insert_with(|| json!({}));
        for k in ["edit", "moveCamera"] {
            if let Some(v) = req.get(k).and_then(|v| v.as_bool()) {
                row[k] = json!(v);
            }
        }
    }
    let out = app.session_perms(&session);
    // The event names the session: other tabs keep their own toggles (DESIGN §10).
    app.events
        .emit(json!({ "type": "agent.permissions", "permissions": out }));
    Json(out).into_response()
}

/// "Send to agent": assign the threads to the agent and tell it (review.submitted).
async fn submit_review(
    State(app): St,
    headers: HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Response {
    let ids: Vec<String> = serde_json::from_value(req["threads"].clone()).unwrap_or_default();
    let by = app.review.lock().unwrap().as_actor(&headers).actor.clone();
    for id in &ids {
        let t = app
            .review
            .lock()
            .unwrap()
            .patch(id, &json!({ "assignee": "agent" }));
        if let Some(t) = t {
            app.events
                .emit(json!({ "type": "thread.upserted", "thread": t }));
            tokio::spawn(simulated_agent(app.clone(), id.clone()));
        }
    }
    app.events
        .emit(json!({ "type": "review.submitted", "threads": ids, "by": by, "note": req["note"] }));
    Json(json!({ "cursor": app.events.cursor().to_string() })).into_response()
}

fn api_error(status: StatusCode, code: &str, message: String) -> Response {
    (
        status,
        Json(json!({ "error": { "code": code, "message": message }, "code": code, "message": message })),
    )
        .into_response()
}

/// PATCH /views/:id/layout: moves windows and clusters of the synthetic view (layout::apply),
/// then view.invalidated { reason: "layout", windows: moved }.
async fn patch_layout(
    State(app): St,
    Path(view): Path<String>,
    Json(p): Json<layout::LayoutPatch>,
) -> Response {
    if !app.knows(&view) {
        return view_unknown(&view);
    }
    if view != app.view().bootstrap.view.id {
        return not_found("layout edits are for the snapshot view");
    }
    let (version, ids) = {
        let mut slot = app.view.write().unwrap();
        match layout::apply(&slot, &p) {
            Ok((v, ids)) => {
                let version = v.bootstrap.view.layout_version;
                *slot = Arc::new(v);
                (version, ids)
            }
            Err(layout::PatchError::Stale(cur)) => {
                return api_error(
                    StatusCode::CONFLICT,
                    "layout-stale",
                    format!(
                        "baseLayoutVersion {} is stale: the view is at layoutVersion {cur}",
                        p.base_layout_version
                    ),
                );
            }
            Err(layout::PatchError::UnknownKey(k)) => {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "unknown-key",
                    format!("unknown or invalid move {k}"),
                );
            }
        }
    };
    app.events.emit(json!({
        "type": "view.invalidated", "viewId": view, "layoutVersion": version, "reason": "layout", "windows": ids
    }));
    Json(json!({ "layoutVersion": version })).into_response()
}

async fn saved_ranges(State(app): St) -> Response {
    Json(app.saved_ranges.lock().unwrap().clone()).into_response()
}

async fn put_range(
    State(app): St,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(mut body): Json<serde_json::Value>,
) -> Response {
    let name = name.trim().to_string();
    if name.is_empty() || body["revset"].as_str().is_none_or(|r| r.trim().is_empty()) {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid",
            "name and revset are required".into(),
        );
    }
    let actor = match headers.get("x-cv-agent").and_then(|v| v.to_str().ok()) {
        Some(n) => {
            json!({ "kind": "agent", "name": n, "client": headers.get("x-cv-agent-client").and_then(|v| v.to_str().ok()) })
        }
        None => json!({ "kind": "human", "name": "you" }),
    };
    body["name"] = json!(name);
    let mut list = app.saved_ranges.lock().unwrap();
    match list.iter_mut().find(|r| r["name"] == name.as_str()) {
        Some(old) => {
            body["createdBy"] = old["createdBy"].clone();
            *old = body.clone();
        }
        None => {
            body["createdBy"] = actor;
            list.push(body.clone());
        }
    }
    Json(body).into_response()
}

async fn delete_range(State(app): St, Path(name): Path<String>) -> Response {
    let mut list = app.saved_ranges.lock().unwrap();
    let n = list.len();
    list.retain(|r| r["name"] != name.as_str());
    if list.len() == n {
        return api_error(
            StatusCode::NOT_FOUND,
            "not-found",
            format!("no saved range {name}"),
        );
    }
    Json(json!({ "ok": true })).into_response()
}

async fn repo_info(State(app): St) -> Response {
    let v = app.view();
    let workspaces: Vec<serde_json::Value> = app
        .panels
        .workspaces
        .lock()
        .unwrap()
        .iter()
        .map(|w| json!({ "name": w["name"], "root": w["root"], "wc": w["wc"] }))
        .collect();
    Json(json!({
        "name": "sample-app",
        "root": "/home/dev/workspace/sample-app",
        "jjVersion": "0.45.1",
        "op": v.bootstrap.view.resolved.op,
        "trunk": "c0ffee",
        "workspaces": workspaces,
        "index": { "state": "idle", "lastOp": v.bootstrap.view.resolved.op, "files": v.files.len(), "blobsParsed": v.files.len() },
        "capabilities": { "scip": false, "runtimeLinks": false, "jjLib": true },
    }))
    .into_response()
}

async fn lanes(State(app): St) -> Response {
    Json(app.view().bootstrap.lane_defs.clone()).into_response()
}
