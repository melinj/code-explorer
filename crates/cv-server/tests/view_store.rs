//! Saved views (index.db, D5/B2): a view loaded from index.db is the view the build produced
//! (every route payload the same), the R*Tree answers viewport queries like the in-memory grid,
//! FTS search ranks like the in-memory search, and a disk tile of an unloaded view matches.
//! Skipped when `jj` is not on PATH. `CV_STORE_REPO=<jj repo>` runs the same checks on a real
//! repo (`cargo test -p cv-server --test view_store -- --ignored --nocapture`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use cv_contracts::*;
use cv_server::index::{self, Options, View};
use cv_server::{layout_input, parse_cache, readside, routes, view_store};
use serde_json::{Value, json};

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

fn fixture(tmp: &Path) -> std::path::PathBuf {
    std::fs::write(tmp.join("jjconfig.toml"), "").unwrap();
    let repo = tmp.join("repo");
    for d in ["src/pkg", "tests", "web/src", "config"] {
        std::fs::create_dir_all(repo.join(d)).unwrap();
    }
    jj(&repo, &["git", "init"]);
    let mut big = String::from("import os\nfrom .util import helper\n\n");
    for i in 0..12 {
        big.push_str(&format!("def step_{i}(x):\n"));
        for k in 0..14 {
            big.push_str(&format!("    x = helper(x) + {k}\n"));
        }
        big.push_str("    return os.getenv(\"STEP_MODE\")\n\n");
    }
    let files = [
        ("src/pkg/__init__.py", String::new()),
        ("src/pkg/engine.py", big),
        (
            "src/pkg/util.py",
            "import os\n\nclass Helper:\n    def run(self, x):\n        return helper(x)\n\ndef helper(x):\n    return x * 2 + int(os.environ[\"STEP_MODE\"])  # é😀\n".into(),
        ),
        (
            "tests/test_engine.py",
            "from pkg.engine import step_0\n\ndef test_step():\n    assert step_0(1)\n".into(),
        ),
        (
            "web/src/api.ts",
            "export function get(id: string) {\n  return fetch(`/api/${id}`);\n}\n".into(),
        ),
        (
            "web/src/main.ts",
            "import { get } from \"./api\";\nget(\"x\");\n".into(),
        ),
        ("config/app.yaml", "server:\n  port: 8080\n".into()),
        (
            "Dockerfile",
            "FROM python:3.12\nENV STEP_MODE=1\nCMD [\"python\", \"-m\", \"pkg\"]\n".into(),
        ),
    ];
    for (p, t) in files {
        std::fs::write(repo.join(p), t).unwrap();
    }
    jj(&repo, &["commit", "-m", "init"]);
    std::fs::write(
        repo.join("src/pkg/util.py"),
        "import os\n\nclass Helper:\n    def run(self, x):\n        return helper(x)\n\ndef helper(x):\n    return x * 3 + int(os.environ[\"STEP_MODE\"])  # é😀\n",
    )
    .unwrap();
    jj(&repo, &["describe", "-m", "tweak"]);
    repo
}

/// Everything a route can read from a view, as comparable JSON.
fn view_json(v: &View) -> Value {
    let files: Vec<Value> = v
        .files
        .iter()
        .map(|f| {
            json!({
                "path": f.path, "blob": f.blob, "lang": f.lang, "lane": f.lane, "text": f.text,
                "starts": f.line_starts, "tokens": f.tokens, "sil": f.silhouette,
                "headings": f.headings, "windows": f.windows,
            })
        })
        .collect();
    let windows: Vec<Value> = v
        .windows
        .iter()
        .map(|w| json!({ "h": w.header, "file": w.file, "out": w.out_links, "in": w.in_links }))
        .collect();
    let links: Vec<Value> = v
        .links
        .iter()
        .map(|l| {
            json!({
                "id": l.id, "kind": l.kind, "from": l.from, "fl": l.from_line, "to": l.to,
                "tl": l.to_line, "c": l.confidence, "label": l.label,
                "chan": l.channel.as_ref().map(|c| json!([c.port, c.dynamic, c.provenance])),
            })
        })
        .collect();
    let tiles: BTreeMap<String, Value> = v
        .tiles
        .iter()
        .map(|(k, t)| (format!("{k:?}"), json!(t)))
        .collect();
    let by: BTreeMap<&String, &usize> = v.by_id.iter().collect();
    let renames: BTreeMap<&String, &String> = v.renames.iter().collect();
    json!({
        "bootstrap": v.bootstrap, "churn": v.bootstrap.churn, "files": files, "windows": windows,
        "links": links, "tiles": tiles, "by_id": by, "renames": renames,
    })
}

fn first_diff(a: &Value, b: &Value, path: &str) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for k in x.keys().chain(y.keys()) {
                let (p, q) = (
                    x.get(k).unwrap_or(&Value::Null),
                    y.get(k).unwrap_or(&Value::Null),
                );
                if let Some(d) = first_diff(p, q, &format!("{path}.{k}")) {
                    return Some(d);
                }
            }
            None
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                return Some(format!("{path}: len {} vs {}", x.len(), y.len()));
            }
            x.iter()
                .zip(y)
                .enumerate()
                .find_map(|(i, (p, q))| first_diff(p, q, &format!("{path}[{i}]")))
        }
        _ => (a != b).then(|| format!("{path}: {a} vs {b}")),
    }
}

/// Builds and saves, then loads as a restarted process would; returns (built, loaded).
fn build_then_load(repo_path: &Path, opts: Options) -> (View, View) {
    let repo = cv_vcs::JjRepo::open(repo_path).unwrap();
    let reg = index::registry();
    let spec = json!({ "mode": "snapshot", "rev": "@" });
    let head = repo.resolve_one("@").unwrap();
    let key = repo.root().display().to_string();
    layout_input::forget_placement(&key);
    let built =
        view_store::load_or_build(&repo, &reg, spec.clone(), &head, opts, &[], true).unwrap();
    let placement = layout_input::last_placement(&key).unwrap();
    layout_input::forget_placement(&key);
    let loaded = view_store::load_or_build(&repo, &reg, spec, &head, opts, &[], false).unwrap();
    assert!(
        loaded.timings.iter().any(|(l, _)| *l == "index.db rows"),
        "the second call loads the saved view"
    );
    assert_eq!(
        *layout_input::last_placement(&key).unwrap(),
        *placement,
        "the placement comes back"
    );
    (built, loaded)
}

fn check_same(built: &View, loaded: &View) {
    let (a, b) = (view_json(built), view_json(loaded));
    if let Some(d) = first_diff(&a, &b, "view") {
        panic!("loaded view differs: {d}");
    }
}

/// R*Tree (index.db) vs the in-memory grid for many viewports; returns (grid, rtree) timings.
fn check_viewports(
    db: &cv_store::IndexDb,
    v: &View,
    root: &Path,
    samples: usize,
) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let world = v.bootstrap.base.world;
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % 1_000_000) as f64 / 1_000_000.0
    };
    let (mut tg, mut tr, mut td) = (Vec::new(), Vec::new(), Vec::new());
    for i in 0..samples {
        let z = (i % 4) as u32;
        let span = (1i64 << z) as f64 * TILE_BASE_WU;
        let x = ((world.x + rnd() * world.w) / span).floor() as i64;
        let y = ((world.y + rnd() * world.h) / span).floor() as i64;
        let t = std::time::Instant::now();
        let mem = routes::tile_response(v, z, x, y);
        tg.push(t.elapsed().as_secs_f64() * 1e3);
        let rect = Rect {
            x: x as f64 * span,
            y: y as f64 * span,
            w: span,
            h: span,
        };
        let t = std::time::Instant::now();
        let ords = db.windows_in_rect(v.id(), &rect).unwrap();
        tr.push(t.elapsed().as_secs_f64() * 1e3);
        let a: BTreeSet<&str> = mem.windows.iter().map(|w| w.id.as_str()).collect();
        let b: BTreeSet<&str> = ords
            .iter()
            .map(|&o| v.windows[o].header.id.as_str())
            .collect();
        assert_eq!(a, b, "windows of tile {z}/{x}/{y}");
        let t = std::time::Instant::now();
        let disk = view_store::disk_tile(root, v.id(), z, x, y);
        td.push(t.elapsed().as_secs_f64() * 1e3);
        if let Some(disk) = disk {
            let ea: BTreeSet<&str> = mem.edges.iter().map(|e| e.id.as_str()).collect();
            let eb: BTreeSet<&str> = disk.edges.iter().map(|e| e.id.as_str()).collect();
            assert_eq!(ea, eb, "edges of tile {z}/{x}/{y}");
            let ha: BTreeMap<&str, String> = mem
                .windows
                .iter()
                .map(|w| (w.id.as_str(), serde_json::to_string(w).unwrap()))
                .collect();
            let hb: BTreeMap<&str, String> = disk
                .windows
                .iter()
                .map(|w| (w.id.as_str(), serde_json::to_string(w).unwrap()))
                .collect();
            assert_eq!(ha, hb, "disk headers of tile {z}/{x}/{y}");
            let pa: BTreeMap<&str, String> = mem
                .edges
                .iter()
                .map(|e| (e.id.as_str(), serde_json::to_string(e).unwrap()))
                .collect();
            let pb: BTreeMap<&str, String> = disk
                .edges
                .iter()
                .map(|e| (e.id.as_str(), serde_json::to_string(e).unwrap()))
                .collect();
            assert_eq!(pa, pb, "disk edges of tile {z}/{x}/{y}");
        }
    }
    (tg, tr, td)
}

/// FTS vs in-memory search for many queries; returns (memory, fts) timings.
fn check_search(db: &cv_store::IndexDb, v: &View, queries: &[String]) -> (Vec<f64>, Vec<f64>) {
    let reg = index::registry();
    // The in-memory index the route builds (re-analysing every file), as an app would.
    let symbols: Vec<readside::SymbolEntry> = {
        use rayon::prelude::*;
        v.files
            .par_iter()
            .flat_map_iter(|f| {
                let syms = reg
                    .pack_for(&f.path)
                    .map(|(_, p)| p.analyze(&f.path, &f.text).symbols)
                    .unwrap_or_default();
                syms.into_iter().filter_map(move |s| {
                    let w = f.windows.iter().copied().find(|&w| {
                        let l = &v.windows[w].header.lines;
                        l.start <= s.name_line && s.name_line <= l.end
                    })?;
                    Some(readside::SymbolEntry {
                        name: s.name,
                        qualname: s.qualname,
                        kind: s.kind.as_str().to_string(),
                        line: s.name_line,
                        window: w,
                    })
                })
            })
            .collect()
    };
    let (mut tm, mut tf) = (Vec::new(), Vec::new());
    for q in queries {
        for kinds in [&[][..], &["symbol"][..], &["file"][..]] {
            let t = std::time::Instant::now();
            let mem = readside::search_view(v, &symbols, q, kinds, 30);
            tm.push(t.elapsed().as_secs_f64() * 1e3);
            let t = std::time::Instant::now();
            let fts = readside::search_view_fts(v, db, q, kinds, 30)
                .expect("FTS rows for the saved view");
            tf.push(t.elapsed().as_secs_f64() * 1e3);
            assert_eq!(
                serde_json::to_value(&mem).unwrap(),
                serde_json::to_value(&fts).unwrap(),
                "search {q:?} {kinds:?}"
            );
        }
    }
    (tm, tf)
}

fn pct(mut xs: Vec<f64>, p: f64) -> f64 {
    xs.sort_by(f64::total_cmp);
    xs[((xs.len() as f64 - 1.0) * p).round() as usize]
}

#[test]
fn a_saved_view_loads_back_identical() {
    if !have_jj() {
        eprintln!("jj not found; skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");
    parse_cache::enable_in(&cache);
    let repo_path = fixture(tmp.path());
    let (built, loaded) = build_then_load(&repo_path, Options { max_commits: 50 });
    check_same(&built, &loaded);
    assert!(!loaded.links.is_empty());
    assert!(
        loaded.links.iter().any(|l| l.channel.is_some()),
        "channel links round-trip"
    );
    for (z, x, y) in [(0, 0, 0), (1, 0, 0), (3, 0, 0), (0, 1, 0)] {
        assert_eq!(
            serde_json::to_value(routes::tile_response(&built, z, x, y)).unwrap(),
            serde_json::to_value(routes::tile_response(&loaded, z, x, y)).unwrap()
        );
    }
    let db = view_store::db(&repo_path).unwrap();
    check_viewports(&db, &loaded, &repo_path, 64);
    // An unloaded view's bootstrap (an id from before a restart) comes from index.db, like its
    // tiles, and equals the served one.
    let disk = view_store::disk_bootstrap(&repo_path, loaded.id()).expect("saved bootstrap");
    assert_eq!(
        serde_json::to_value(&disk).unwrap(),
        serde_json::to_value(&loaded.bootstrap).unwrap()
    );
    assert!(view_store::disk_bootstrap(&repo_path, "v_nope").is_none());
    let queries: Vec<String> = [
        "step_3",
        "helper",
        "HELPER",
        "Helper.run",
        "he",
        "x",
        "engine",
        "api",
        "get",
        "util.py",
        "zzz",
        "é",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    check_search(&db, &loaded, &queries);

    // Different inputs (pins) build again; deleting index.db rebuilds from scratch.
    let repo = cv_vcs::JjRepo::open(&repo_path).unwrap();
    let head = repo.resolve_one("@").unwrap();
    let key = repo.root().display().to_string();
    layout_input::forget_placement(&key);
    let pins = [cv_layout::Pin {
        key: built.windows[0].header.key.clone(),
        x: 0.0,
        y: 0.0,
    }];
    let pinned = view_store::load_or_build(
        &repo,
        &index::registry(),
        json!({ "mode": "snapshot", "rev": "@" }),
        &head,
        Options { max_commits: 50 },
        &pins,
        false,
    )
    .unwrap();
    assert!(!pinned.timings.iter().any(|(l, _)| *l == "index.db rows"));
    parse_cache::disable();
}

/// The same checks on a real repo, with timings (DESIGN §9): `CV_STORE_REPO=/path/to/repo`.
#[test]
#[ignore]
fn real_repo_round_trip_and_timings() {
    let Ok(repo) = std::env::var("CV_STORE_REPO") else {
        eprintln!("CV_STORE_REPO not set; skipping");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    parse_cache::enable_in(tmp.path());
    let repo_path = Path::new(&repo).to_path_buf();
    let t = std::time::Instant::now();
    let (built, loaded) = build_then_load(&repo_path, Options { max_commits: 2000 });
    eprintln!("build + save + load: {:.1?}", t.elapsed());
    for (l, d) in &loaded.timings {
        eprintln!("  load {l:<22} {d:>10.1?}");
    }
    check_same(&built, &loaded);
    let db = view_store::db(&repo_path).unwrap();
    let (tg, tr, td) = check_viewports(&db, &loaded, &repo_path, 2000);
    eprintln!(
        "tiles z0-3, 2000 samples: grid p50 {:.3} ms p95 {:.3} ms max {:.3} ms; R*Tree windows_in_rect p50 {:.3} ms p95 {:.3} ms max {:.3} ms",
        pct(tg.clone(), 0.5),
        pct(tg.clone(), 0.95),
        pct(tg, 1.0),
        pct(tr.clone(), 0.5),
        pct(tr.clone(), 0.95),
        pct(tr, 1.0)
    );
    eprintln!(
        "whole tile from index.db (R*Tree windows + headers + edges): p50 {:.3} ms p95 {:.3} ms max {:.3} ms",
        pct(td.clone(), 0.5),
        pct(td.clone(), 0.95),
        pct(td, 1.0)
    );
    let mut queries: Vec<String> = loaded
        .windows
        .iter()
        .step_by(97)
        .filter_map(|w| w.header.symbol.as_ref().map(|s| s.name.clone()))
        .take(60)
        .collect();
    queries.extend(["config", "test", "id", "a", "event", "World", "zzzq"].map(String::from));
    let (tm, tf) = check_search(&db, &loaded, &queries);
    eprintln!(
        "search, {} queries × 3 kind sets: memory p50 {:.2} ms p95 {:.2} ms; FTS p50 {:.2} ms p95 {:.2} ms",
        queries.len(),
        pct(tm.clone(), 0.5),
        pct(tm, 0.95),
        pct(tf.clone(), 0.5),
        pct(tf, 0.95)
    );
    parse_cache::disable();
}
