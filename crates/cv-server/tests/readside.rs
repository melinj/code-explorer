//! Read-side routes end to end on a scripted jj repo: history, revset validation, workspaces,
//! range views (hunks, context pills, diffs, in-range owners), metrics, search and expand — and
//! no operation written. Skipped when `jj` is not on PATH.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use cv_server::index::Options;
use cv_server::routes::{self, AppState};
use serde_json::{Value, json};
use tower::ServiceExt;

fn jj(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn op_head(repo: &Path) -> String {
    jj(
        repo,
        &[
            "op",
            "log",
            "-n1",
            "--no-graph",
            "--ignore-working-copy",
            "-T",
            "id",
        ],
    )
}

fn engine(step3_extra: &[&str]) -> String {
    let mut s = String::from("from .util import helper\n\n");
    for i in 0..12 {
        s.push_str(&format!("def step_{i}(x):\n"));
        for k in 0..14 {
            s.push_str(&format!("    x = helper(x) + {k}\n"));
            if i == 3 && k == 6 {
                for e in step3_extra {
                    s.push_str(e);
                }
            }
        }
        s.push_str("    return x\n\n");
    }
    s
}

/// init (bookmark main) ← s1 (helper ×3, a line in step_3) ← s2 (@, another line in step_3).
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
    std::fs::create_dir_all(repo.join("src/pkg")).unwrap();
    jj(&repo, &["git", "init"]);
    std::fs::write(repo.join("src/pkg/__init__.py"), "").unwrap();
    std::fs::write(repo.join("src/pkg/engine.py"), engine(&[])).unwrap();
    std::fs::write(
        repo.join("src/pkg/util.py"),
        "def helper(x):\n    return x * 2\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("src/pkg/app.py"),
        "from .engine import step_3\n\n\ndef run():\n    return step_3(1)\n",
    )
    .unwrap();
    jj(&repo, &["commit", "-m", "init"]);
    jj(&repo, &["bookmark", "create", "main", "-r", "@-"]);
    std::fs::write(
        repo.join("src/pkg/util.py"),
        "def helper(x):\n    return x * 3\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("src/pkg/engine.py"),
        engine(&["    x = x - 1  # s1\n"]),
    )
    .unwrap();
    jj(&repo, &["commit", "-m", "s1"]);
    std::fs::write(
        repo.join("src/pkg/engine.py"),
        engine(&["    x = x - 1  # s1\n", "    x = x + 7  # s2\n"]),
    )
    .unwrap();
    jj(&repo, &["describe", "-m", "s2"]);
    Some(tmp)
}

async fn call(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> Value {
    let req = Request::builder().method(method).uri(uri);
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert!(status.is_success(), "{method} {uri}: {status} {v}");
    v
}

/// Every window and edge of a view, from tiles at a level where one tile covers the world.
async fn all_tiles(app: &axum::Router, view: &str, world: &Value) -> (Vec<Value>, Vec<Value>) {
    let span = world["w"]
        .as_f64()
        .unwrap()
        .max(world["h"].as_f64().unwrap());
    let z = (span / cv_contracts::TILE_BASE_WU).log2().ceil().max(0.0) as u32;
    let t = call(
        app,
        "GET",
        &format!("/api/v1/views/{view}/tiles/{z}/0/0"),
        None,
    )
    .await;
    (
        t["windows"].as_array().unwrap().clone(),
        t["edges"].as_array().unwrap().clone(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn read_side_routes_on_a_scripted_repo() {
    let Some(tmp) = fixture() else { return };
    let repo_dir = tmp.path().join("repo");
    let before = op_head(&repo_dir);
    let repo = cv_vcs::JjRepo::open(&repo_dir).unwrap();
    let state = Arc::new(AppState::new(repo, Options { max_commits: 100 }));
    let app = routes::router(state.clone());

    // History.
    let h = call(&app, "GET", "/api/v1/revisions?revset=all()&limit=10", None).await;
    let rows = h["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 4, "root, init, s1, s2");
    assert_eq!(rows[0]["rev"]["description"].as_str().unwrap().trim(), "s2");
    assert_eq!(rows[0]["rev"]["flags"]["workingCopy"], true);
    assert_eq!(rows[0]["rev"]["workspaces"], json!(["default"]));
    assert_eq!(rows[0]["lane"], 0);
    assert_eq!(rows[0]["edges"][0]["toLane"], 0);
    let init = rows
        .iter()
        .find(|r| r["rev"]["bookmarks"] == json!(["main"]))
        .unwrap();
    assert!(init["rev"]["shortest"]["change"].as_u64().unwrap() >= 1);
    let h = call(&app, "GET", "/api/v1/revisions?revset=@|root()", None).await;
    assert_eq!(h["rows"][0]["edges"][0]["elided"], 2, "s1 and init hidden");

    // Revset validation.
    let ok = call(
        &app,
        "POST",
        "/api/v1/revsets:validate",
        Some(json!({ "revset": "main..@" })),
    )
    .await;
    assert_eq!(ok["ok"], true);
    assert_eq!(ok["count"], 2);
    assert_eq!(ok["files"], 2);
    assert_eq!(
        (ok["added"].as_u64(), ok["removed"].as_u64()),
        (Some(3), Some(1))
    );
    let bad = call(
        &app,
        "POST",
        "/api/v1/revsets:validate",
        Some(json!({ "revset": "trnk()..@" })),
    )
    .await;
    assert_eq!(bad["ok"], false);
    assert!(bad["hint"].as_str().unwrap().contains("trunk()"), "{bad}");
    let gap = call(
        &app,
        "POST",
        "/api/v1/revsets:validate",
        Some(json!({ "revset": "@ | main" })),
    )
    .await;
    assert_eq!(gap["warnings"][0]["kind"], "gap");
    assert_eq!(gap["warnings"][0]["missing"].as_array().unwrap().len(), 1);
    assert_eq!(call(&app, "GET", "/api/v1/ranges", None).await, json!([]));

    // Workspaces.
    let ws = call(&app, "GET", "/api/v1/workspaces", None).await;
    assert_eq!(ws.as_array().unwrap().len(), 1);
    assert_eq!(ws[0]["name"], "default");
    assert_eq!(ws[0]["serving"], true);
    assert_eq!(ws[0]["stack"]["revset"], "trunk()..default@");
    assert_eq!(ws[0]["unsnapshottedFiles"], 0);

    // Snapshot view, then the range view sharing its placement.
    let snap = call(
        &app,
        "POST",
        "/api/v1/views",
        Some(json!({ "mode": "snapshot", "rev": "@" })),
    )
    .await;
    let snap_id = snap["id"].as_str().unwrap().to_string();
    let spec = json!({ "mode": "range", "revset": "main..@", "options": { "contextHops": 1 } });
    let r = call(&app, "POST", "/api/v1/views", Some(spec.clone())).await;
    let again = call(&app, "POST", "/api/v1/views", Some(spec)).await;
    assert_eq!(r["id"], again["id"], "idempotent");
    let rid = r["id"].as_str().unwrap().to_string();
    assert_eq!(r["sharedPlacement"], snap_id.as_str());
    assert_eq!(r["resolved"]["commits"].as_array().unwrap().len(), 2);
    assert_eq!(
        (r["stats"]["added"].as_u64(), r["stats"]["removed"].as_u64()),
        (Some(3), Some(1))
    );
    assert_eq!(
        call(&app, "GET", &format!("/api/v1/views/{rid}"), None).await["id"],
        rid.as_str()
    );

    let sb = call(
        &app,
        "GET",
        &format!("/api/v1/views/{snap_id}/bootstrap"),
        None,
    )
    .await;
    let rb = call(&app, "GET", &format!("/api/v1/views/{rid}/bootstrap"), None).await;
    assert_eq!(rb["view"]["spec"]["mode"], "range");
    assert_eq!(rb["world"], sb["world"]);
    assert_eq!(rb["columns"], sb["columns"]);
    let (sc, rc) = (
        sb["clusters"].as_array().unwrap(),
        rb["clusters"].as_array().unwrap(),
    );
    assert_eq!(sc.len(), rc.len());
    for (a, b) in sc.iter().zip(rc) {
        assert_eq!(a["rect"], b["rect"], "clusters keep their place");
    }

    let (snap_ws, _) = all_tiles(&app, &snap_id, &sb["world"]).await;
    let (ws_, edges) = all_tiles(&app, &rid, &rb["world"]).await;
    let shown: u64 = rc.iter().map(|c| c["windows"].as_u64().unwrap()).sum();
    assert_eq!(shown as usize, ws_.len());
    assert!(rc.iter().any(|c| c["windows"] == 0) || sc.len() == 1 || shown > 0);
    let hunks: Vec<&Value> = ws_.iter().filter(|w| w["kind"] == "hunk").collect();
    let ctx: Vec<&Value> = ws_.iter().filter(|w| w["kind"] == "context").collect();
    assert_eq!(
        hunks.len(),
        2,
        "helper and step_3: {:?}",
        hunks.iter().map(|h| &h["key"]).collect::<Vec<_>>()
    );
    assert!(!ctx.is_empty());
    assert!(ctx.iter().all(|c| c["flags"]["minimized"] == true));
    assert!(
        ws_.len() < snap_ws.len(),
        "only changed and context windows"
    );
    let step3 = hunks
        .iter()
        .find(|h| h["symbol"]["name"] == "step_3")
        .expect("step_3 hunk");
    assert_eq!(step3["heat"]["inRange"], json!({ "commits": 2, "of": 2 }));
    assert_eq!(step3["diffStat"], json!({ "added": 2, "removed": 0 }));
    // Placed where step_3's lines sit in the full view.
    let full = snap_ws
        .iter()
        .find(|w| w["symbol"]["name"] == "step_3")
        .unwrap();
    assert_eq!(step3["rect"]["x"], full["rect"]["x"]);
    let dy = step3["rect"]["y"].as_f64().unwrap() - full["rect"]["y"].as_f64().unwrap();
    let dl = step3["lines"]["start"].as_u64().unwrap() - full["lines"]["start"].as_u64().unwrap();
    assert_eq!(dy, dl as f64 * cv_contracts::LINE_WU);
    let helper = hunks
        .iter()
        .find(|h| h["path"] == "src/pkg/util.py")
        .unwrap();
    assert_eq!(helper["diffStat"], json!({ "added": 1, "removed": 1 }));
    // Edges only among shown windows.
    let ids: std::collections::HashSet<&str> =
        ws_.iter().map(|w| w["id"].as_str().unwrap()).collect();
    assert!(
        edges
            .iter()
            .all(|e| ids.contains(e["from"].as_str().unwrap())
                && ids.contains(e["to"].as_str().unwrap()))
    );

    // Diffs with owners.
    let batch = call(
        &app,
        "POST",
        &format!("/api/v1/views/{rid}/windows:batch"),
        Some(json!({ "ids": [step3["id"], helper["id"]], "parts": ["outline", "diff"], "layoutVersion": 1 })),
    )
    .await;
    assert_eq!(batch["outlines"].as_array().unwrap().len(), 2);
    let diffs = batch["diffs"].as_array().unwrap();
    let d3 = diffs.iter().find(|d| d["id"] == step3["id"]).unwrap();
    let adds: Vec<&Value> = d3["lines"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| l["kind"] == "add")
        .collect();
    assert_eq!(adds.len(), 2);
    assert_ne!(adds[0]["owner"], adds[1]["owner"], "one line per commit");
    assert!(
        d3["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["kind"] == "ctx")
    );
    let dh = diffs.iter().find(|d| d["id"] == helper["id"]).unwrap();
    let del = dh["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "del")
        .unwrap();
    assert_eq!(del["text"], "    return x * 2");

    // Fragments list the range's windows of a file.
    let fr = call(
        &app,
        "GET",
        &format!("/api/v1/views/{rid}/fragments?path=src/pkg/engine.py"),
        None,
    )
    .await;
    let n_engine = ws_
        .iter()
        .filter(|w| w["path"] == "src/pkg/engine.py")
        .count();
    assert_eq!(fr["fragments"].as_array().unwrap().len(), n_engine);

    // Expand a context pill: comes back widened and not minimized; the frame grows upward.
    let c = ctx[0];
    let e = call(
        &app,
        "POST",
        &format!(
            "/api/v1/views/{rid}/windows/{}/expand",
            c["id"].as_str().unwrap()
        ),
        Some(json!({ "before": 2, "after": 1 })),
    )
    .await;
    assert_eq!(e["flags"]["minimized"], false);
    let start = c["lines"]["start"].as_u64().unwrap();
    assert_eq!(
        e["lines"]["start"].as_u64().unwrap(),
        start.saturating_sub(2).max(1)
    );
    assert!(e["rect"]["y"].as_f64().unwrap() <= c["rect"]["y"].as_f64().unwrap());

    // Per-commit: one hunk per commit touching step_3, each showing that commit's file version.
    let pc = call(
        &app,
        "POST",
        "/api/v1/views",
        Some(json!({ "mode": "range", "revset": "main..@", "options": { "diff": "per-commit", "contextHops": 0 } })),
    )
    .await;
    let pid = pc["id"].as_str().unwrap().to_string();
    let pb = call(&app, "GET", &format!("/api/v1/views/{pid}/bootstrap"), None).await;
    let (pws, _) = all_tiles(&app, &pid, &pb["world"]).await;
    assert!(
        pws.iter().all(|w| w["kind"] == "hunk"),
        "contextHops 0: no context"
    );
    let s3: Vec<&Value> = pws
        .iter()
        .filter(|w| w["symbol"]["name"] == "step_3")
        .collect();
    assert_eq!(s3.len(), 2);
    assert_ne!(s3[0]["rev"], s3[1]["rev"]);
    assert_ne!(s3[0]["blob"], s3[1]["blob"]);
    for w in &s3 {
        assert_eq!(w["heat"]["inRange"], json!({ "commits": 1, "of": 2 }));
        let t = call(
            &app,
            "GET",
            &format!("/api/v1/blobs/{}/text", w["blob"].as_str().unwrap()),
            None,
        )
        .await;
        assert!(!t["lines"].as_array().unwrap().is_empty());
    }

    // Metrics and search.
    let m = call(
        &app,
        "GET",
        &format!("/api/v1/views/{snap_id}/metrics?level=file"),
        None,
    )
    .await;
    let pts = m["points"].as_array().unwrap();
    assert!(!pts.is_empty());
    let risks: Vec<f64> = pts.iter().map(|p| p["risk"].as_f64().unwrap()).collect();
    assert!(
        risks.windows(2).all(|w| w[0] >= w[1]) && risks.iter().all(|r| (0.0..=1.0).contains(r))
    );
    let m = call(
        &app,
        "GET",
        &format!("/api/v1/views/{snap_id}/metrics?level=cluster&lane=code"),
        None,
    )
    .await;
    assert!(
        m["points"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["lane"] == "code")
    );
    let s = call(&app, "GET", "/api/v1/search?q=helper&limit=5", None).await;
    assert_eq!(s[0]["kind"], "symbol");
    assert_eq!(s[0]["label"], "helper");
    assert_eq!(s[0]["detail"], "src/pkg/util.py:1");
    let s = call(&app, "GET", "/api/v1/search?q=engine&kinds=file", None).await;
    assert!(s.as_array().unwrap().iter().all(|h| h["kind"] == "file"));
    assert_eq!(s[0]["label"], "src/pkg/engine.py");

    assert_eq!(op_head(&repo_dir), before, "read routes wrote an operation");
}
