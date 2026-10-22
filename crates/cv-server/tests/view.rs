//! End to end on a scripted jj repo: index the snapshot at @, then check the route payloads.
//! Skipped when `jj` is not on PATH.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use cv_contracts::*;
use cv_server::index::{self, Options};
use cv_server::routes::{self, AppState};
use tower::ServiceExt;

fn jj(dir: &Path, args: &[&str]) {
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
}

fn fixture() -> Option<tempfile::TempDir> {
    if !Command::new("jj")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("jj not found; skipping");
        return None;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), "").unwrap();
    let repo = tmp.path().join("repo");
    for d in ["src/pkg", "tests", "web/src", "config", "docs"] {
        std::fs::create_dir_all(repo.join(d)).unwrap();
    }
    jj(&repo, &["git", "init"]);
    let mut big = String::from("from .util import helper\n\n");
    for i in 0..12 {
        big.push_str(&format!("def step_{i}(x):\n"));
        for k in 0..14 {
            big.push_str(&format!("    x = helper(x) + {k}\n"));
        }
        big.push_str("    return x\n\n");
    }
    std::fs::write(repo.join("src/pkg/__init__.py"), "").unwrap();
    std::fs::write(repo.join("src/pkg/engine.py"), &big).unwrap();
    std::fs::write(
        repo.join("src/pkg/util.py"),
        "def helper(x):\n    return x * 2  # é😀\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("tests/test_engine.py"),
        "from pkg.engine import step_0\n\ndef test_step():\n    assert step_0(1)\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("web/src/api.ts"),
        "export function get(id: string) {\n  return fetch(`/api/${id}`);\n}\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("web/src/main.ts"),
        "import { get } from \"./api\";\nget(\"x\");\n",
    )
    .unwrap();
    std::fs::write(repo.join("config/app.yaml"), "server:\n  port: 8080\n").unwrap();
    std::fs::write(repo.join("docs/guide.md"), "# Guide\n\nText.\n").unwrap();
    std::fs::write(
        repo.join("Dockerfile"),
        "FROM python:3.12\nCMD [\"python\", \"-m\", \"pkg\"]\n",
    )
    .unwrap();
    jj(&repo, &["commit", "-m", "init"]);
    std::fs::write(
        repo.join("src/pkg/util.py"),
        "def helper(x):\n    return x * 3  # é😀\n",
    )
    .unwrap();
    jj(&repo, &["describe", "-m", "tweak"]);
    Some(tmp)
}

#[test]
fn indexes_a_snapshot_and_serves_mock_shaped_payloads() {
    let Some(tmp) = fixture() else { return };
    let repo_path = tmp.path().join("repo");
    let repo = cv_vcs::JjRepo::open(&repo_path).unwrap();
    let opts = Options { max_commits: 100 };
    let reg = index::registry();
    let spec = serde_json::json!({ "mode": "snapshot", "rev": "@" });
    let head = repo.resolve_one("@").unwrap();
    let v = index::build_view(&repo, &reg, spec.clone(), &head, opts).unwrap();
    // Deterministic for identical input.
    let again = index::build_view(&repo, &reg, spec.clone(), &head, opts).unwrap();
    assert_eq!(
        serde_json::to_string(&v.bootstrap).unwrap(),
        serde_json::to_string(&again.bootstrap).unwrap()
    );

    let lanes: Vec<&str> = v
        .bootstrap
        .base
        .lanes
        .iter()
        .map(|l| l.lane.as_str())
        .collect();
    assert_eq!(lanes, vec!["infra", "config", "code", "test", "docs"]);
    let lane_of = |p: &str| {
        v.windows[v.files[v.by_path[p]].windows[0]]
            .header
            .lane
            .clone()
    };
    assert_eq!(lane_of("Dockerfile"), "infra");
    assert_eq!(lane_of("tests/test_engine.py"), "test");
    assert_eq!(lane_of("web/src/api.ts"), "code");

    // engine.py (≈ 200 lines) splits into one window per step function, all with chips.
    let engine = &v.files[v.by_path["src/pkg/engine.py"]];
    assert_eq!(engine.windows.len(), 12);
    let frag = v.windows[engine.windows[3]]
        .header
        .fragment
        .clone()
        .unwrap();
    assert_eq!(
        (frag.index, frag.of, frag.chip),
        (4, 12, cv_lang::chunk::chip_slot("src/pkg/engine.py"))
    );

    // Links: step_i → helper (imported), main.ts → get.
    let label = |l: &index::LinkRec| (v.windows[l.from].header.path.clone(), l.label.clone());
    let links: Vec<(String, String)> = v.links.iter().map(label).collect();
    assert!(
        links.contains(&("src/pkg/engine.py".into(), "helper".into())),
        "{links:?}"
    );
    assert!(
        links.contains(&("web/src/main.ts".into(), "get".into())),
        "{links:?}"
    );
    assert!(
        links.contains(&("tests/test_engine.py".into(), "step_0".into())),
        "{links:?}"
    );
    // util.py's helper is called from the 12 step windows of engine.py (connectedness counts
    // windows over typed links, §4.8); the test reaches it only through two hops, not directly.
    let util = &v.windows[v.files[v.by_path["src/pkg/util.py"]].windows[0]].header;
    assert_eq!((util.conn.dependents2, util.conn.tests), (12, 0));
    let step0 = v.files[v.by_path["src/pkg/engine.py"]]
        .windows
        .iter()
        .map(|&w| &v.windows[w].header)
        .find(|h| h.symbol.as_ref().is_some_and(|s| s.name == "step_0"))
        .unwrap();
    assert_eq!(step0.conn.tests, 1, "covered by the test that calls it");
    assert!(util.heat.grade > 0, "changed in both commits");

    // T1: every window comes back from the tile that holds its corner; edges carry clusters.
    let mut seen = std::collections::HashSet::new();
    for w in &v.windows {
        let r = w.header.rect;
        let t = routes::tile_response(
            &v,
            0,
            (r.x / TILE_BASE_WU).floor() as i64,
            (r.y / TILE_BASE_WU).floor() as i64,
        );
        assert!(t.windows.iter().any(|h| h.id == w.header.id));
        seen.extend(t.edges.into_iter().map(|e| e.id));
    }
    assert_eq!(seen.len(), v.links.len());

    // T2: silhouettes are 3 bytes per displayed line with port bits on callsite lines.
    use base64::Engine;
    let ids: Vec<String> = v.windows.iter().map(|w| w.header.id.clone()).collect();
    let b = routes::batch_response(
        &v,
        &WindowBatchRequest {
            ids: ids.clone(),
            parts: vec!["outline".into()],
            layout_version: 1,
        },
    );
    assert!(b.missing.is_empty());
    for (o, w) in b.outlines.iter().zip(&v.windows) {
        let sil = base64::engine::general_purpose::STANDARD
            .decode(&o.silhouette)
            .unwrap();
        assert_eq!(
            sil.len() as u32,
            3 * (w.header.lines.end - w.header.lines.start + 1)
        );
        for p in &o.ports {
            assert_ne!(
                sil[(p.line - w.header.lines.start) as usize * 3 + 2] & 0x40,
                0
            );
        }
    }
    assert!(!b.links.is_empty());

    // Through the router: view creation is idempotent, blob text is immutable with UTF-16 tokens.
    let state = Arc::new(AppState::new(repo, opts));
    let app = routes::router(state);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let post = || {
            Request::post("/api/v1/views")
                .header("content-type", "application/json")
                .body(Body::from(spec.to_string()))
                .unwrap()
        };
        let r1 = app.clone().oneshot(post()).await.unwrap();
        let r2 = app.clone().oneshot(post()).await.unwrap();
        let body = |r: axum::response::Response| async {
            axum::body::to_bytes(r.into_body(), usize::MAX)
                .await
                .unwrap()
        };
        let v1: ViewRef = serde_json::from_slice(&body(r1).await).unwrap();
        let v2: ViewRef = serde_json::from_slice(&body(r2).await).unwrap();
        assert_eq!(v1.id, v2.id);
        assert_eq!(v1.id, v.bootstrap.base.view.id);
        let blob = &v.files[v.by_path["src/pkg/util.py"]].blob;
        let r = app
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/blobs/{blob}/text?from=2&to=2"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            r.headers()["cache-control"],
            "public, max-age=31536000, immutable"
        );
        let t: BlobText = serde_json::from_slice(&body(r).await).unwrap();
        assert_eq!(t.lines, vec!["    return x * 3  # é😀"]);
        // The comment starts at UTF-16 column 18 and is 5 units long (é = 1, 😀 = 2).
        assert!(
            t.tokens[0].chunks(3).any(|c| c == [18, 5, 3]),
            "{:?}",
            t.tokens
        );
        let r = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/v1/views/{}/fragments?path=src/pkg/engine.py",
                    v1.id
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        let f: serde_json::Value = serde_json::from_slice(&body(r).await).unwrap();
        assert_eq!(f["fragments"].as_array().unwrap().len(), 12);
        let r = app
            .oneshot(
                Request::get("/api/v1/views/v_nope/bootstrap")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 404);
    });
}
