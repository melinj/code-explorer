//! Channel links end to end (B4, P1–P5, D11) on a scripted jj repo: bus topics through imported
//! constants and wildcards, an HTTP client with a wrapper prefix meeting a router-prefixed route,
//! pydantic settings with `env_prefix` meeting `.env.example`, Dockerfile and pre-commit deploy
//! wiring, a config-field read, dispatch to handlers, and dynamic stubs for runtime-built keys.
//! Skipped when `jj` is not on PATH.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use cv_contracts::*;
use cv_server::index::{self, Options};
use cv_server::routes;

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

/// A subscriber big enough that `start` and `on_build` get windows of their own.
fn listener() -> String {
    let pad = "    x = 1\n".repeat(14);
    let mut s =
        format!("def start(bus):\n    bus.on(\"map.district.build.*\", on_build)\n{pad}\n\n");
    s.push_str(&format!(
        "def on_build(envelope):\n{pad}    return envelope\n\n\n"
    ));
    for i in 0..8 {
        s.push_str(&format!("def other_{i}():\n{pad}\n\n"));
    }
    s
}

const FILES: &[(&str, &str)] = &[
    (
        "src/app/events.py",
        "MOVED = \"world.entity.moved\"\nBUILD = \"map.district.build.requested\"\n",
    ),
    (
        "src/app/presence.py",
        r#"from .events import MOVED


class Presence:
    def moved(self):
        self._bus.emit(EventEnvelope(topic=MOVED, payload=1))

    def anything(self, topic):
        self._bus.emit(EventEnvelope(topic=topic, payload=2))

    def guessed(self, kind):
        self._bus.emit(EventEnvelope(topic=f"zzz.{kind}.qqq", payload=3))
"#,
    ),
    (
        "src/app/builder.py",
        r#"from .events import BUILD


class Builder:
    def request(self):
        self._safe_emit(BUILD, {"x": 1})
"#,
    ),
    (
        "src/app/flow.yaml",
        "pool:\n  - id: moved_sub\n    config_id: event_subscribe\n    params:\n      topic: world.entity.moved\n",
    ),
    (
        "src/app/node.yaml",
        "name: node\nconfig:\n  capacity_limit: 3\n",
    ),
    (
        "src/app/loader.py",
        "def load(path):\n    cfg = read(\"node.yaml\")\n    return cfg.get(\"capacity_limit\")\n",
    ),
    (
        "src/web/routes.py",
        r#"router = APIRouter(prefix="/api/node")


@router.get("/{node_id}")
def get_node(node_id: str):
    return {"id": node_id}
"#,
    ),
    (
        "web/src/api.ts",
        r#"const API_BASE: string = import.meta.env.VITE_API_BASE || '/api';

export function getNode(id: string) {
  return request(`/node/${id}`);
}
"#,
    ),
    (
        "src/settings.py",
        r#"class Settings(BaseSettings):
    model_config = SettingsConfigDict(env_prefix="APP_")
    DB_URL: str = ""
"#,
    ),
    (".env.example", "APP_DB_URL=postgres://localhost/app\n"),
    ("main.py", "print('hi')\n"),
    (
        "Dockerfile",
        "FROM python:3.12\nCOPY . /app\nCMD [\"python\", \"main.py\"]\n",
    ),
    (
        "pre-commit.sh",
        "#!/bin/sh\nset -e\nuv run pytest -n 8 tests/\n",
    ),
    ("tests/conftest.py", "import pytest\n"),
    ("tests/test_app.py", "def test_x():\n    assert True\n"),
];

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
    std::fs::create_dir_all(&repo).unwrap();
    jj(&repo, &["git", "init"]);
    for (p, text) in FILES {
        let full = repo.join(p);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    std::fs::write(repo.join("src/app/listener.py"), listener()).unwrap();
    jj(&repo, &["commit", "-m", "init"]);
    Some(tmp)
}

#[test]
fn channels_link_hub_and_stub() {
    let Some(tmp) = fixture() else { return };
    let repo = cv_vcs::JjRepo::open(&tmp.path().join("repo")).unwrap();
    let reg = index::registry();
    let spec = serde_json::json!({ "mode": "snapshot", "rev": "@" });
    let head = repo.resolve_one("@").unwrap();
    let v = index::build_view(&repo, &reg, spec, &head, Options { max_commits: 50 }).unwrap();

    // Hubs: producers and consumers by path, each placed on a lane boundary.
    let hubs = &v.bootstrap.base.hubs;
    let hub = |id: &str| {
        hubs.iter().find(|h| h.id == id).unwrap_or_else(|| {
            panic!(
                "no hub {id}: {:?}",
                hubs.iter().map(|h| &h.id).collect::<Vec<_>>()
            )
        })
    };
    let paths = |s: &[SiteRef]| s.iter().map(|x| x.path.clone()).collect::<HashSet<_>>();
    let moved = hub("bus:world.entity.moved");
    assert_eq!(
        paths(&moved.producers),
        HashSet::from(["src/app/presence.py".to_string()])
    );
    assert_eq!(
        paths(&moved.consumers),
        HashSet::from(["src/app/flow.yaml".to_string()])
    );
    let build = hub("bus:map.district.build.requested");
    assert_eq!(
        paths(&build.consumers),
        HashSet::from(["src/app/listener.py".to_string()])
    );
    let http = hub("http:GET /api/node/{}");
    assert_eq!(
        paths(&http.producers),
        HashSet::from(["web/src/api.ts".to_string()])
    );
    assert_eq!(
        paths(&http.consumers),
        HashSet::from(["src/web/routes.py".to_string()])
    );
    let env = hub("env:APP_DB_URL");
    assert_eq!(
        paths(&env.producers),
        HashSet::from([".env.example".to_string()])
    );
    assert_eq!(
        paths(&env.consumers),
        HashSet::from(["src/settings.py".to_string()])
    );
    let cfg = hub("config:node.yaml#capacity_limit");
    assert_eq!(cfg.family.as_deref(), Some("node.yaml"));
    assert_eq!(
        paths(&cfg.consumers),
        HashSet::from(["src/app/loader.py".to_string()])
    );
    let lanes = &v.bootstrap.base.lanes;
    for h in hubs {
        let r = h.rect.expect("hub rect (few config hubs here, all drawn)");
        let cy = r.y + r.h / 2.0;
        assert!(
            lanes
                .iter()
                .all(|l| cy < l.rect.y || cy > l.rect.y + l.rect.h),
            "hub {} sits inside a lane band",
            h.id
        );
    }

    // Links by kind, from path to path.
    let edge = |l: &index::LinkRec| {
        (
            l.kind,
            v.windows[l.from].header.path.as_str(),
            v.windows[l.to].header.path.as_str(),
        )
    };
    let edges: HashSet<(&str, &str, &str)> = v
        .links
        .iter()
        .filter(|l| !l.is_dynamic())
        .map(edge)
        .collect();
    for want in [
        ("subscribe", "src/app/presence.py", "src/app/flow.yaml"),
        ("publish", "src/app/builder.py", "src/app/listener.py"),
        ("dispatch", "src/app/listener.py", "src/app/listener.py"),
        ("http-request", "web/src/api.ts", "src/web/routes.py"),
        ("define", ".env.example", "src/settings.py"),
        ("read", "src/app/node.yaml", "src/app/loader.py"),
        ("deploy", "Dockerfile", "main.py"),
        ("deploy", "pre-commit.sh", "tests/conftest.py"),
    ] {
        assert!(edges.contains(&want), "missing {want:?} in {edges:?}");
    }
    // Tiles carry the channel edges with their kinds.
    let t = routes::tile_response(&v, 12, 0, 0);
    let kinds: HashSet<&str> = t.edges.iter().map(|e| e.kind.as_str()).collect();
    for k in [
        "subscribe",
        "http-request",
        "define",
        "read",
        "deploy",
        "dispatch",
    ] {
        assert!(kinds.contains(k), "tile edges lack {k}: {kinds:?}");
    }

    // Dynamic stubs: a runtime topic never links; a partial one keeps its pattern.
    let presence = &v.files[v.by_path["src/app/presence.py"]];
    let ids: Vec<String> = presence
        .windows
        .iter()
        .map(|&w| v.windows[w].header.id.clone())
        .collect();
    let b = routes::batch_response(
        &v,
        &WindowBatchRequest {
            ids,
            parts: vec!["outline".into()],
            layout_version: 1,
        },
    );
    let at = |line: u32| b.links.iter().filter(move |l| l.from.line == line);
    let runtime: Vec<&Link> = at(9).collect();
    assert_eq!(runtime.len(), 1, "{runtime:?}");
    assert_eq!(runtime[0].to["dynamic"]["protocol"], "bus");
    assert_eq!(runtime[0].to["dynamic"]["hint"], "topic");
    assert!(runtime[0].confidence < 0.6);
    let partial: Vec<&Link> = at(12).collect();
    assert_eq!(partial.len(), 1);
    assert_eq!(partial[0].to["dynamic"]["partial"], "zzz.*.qqq");
    let ports: Vec<&Port> = b.outlines.iter().flat_map(|o| &o.ports).collect();
    assert!(ports.iter().any(|p| p.line == 9 && p.label == "⇢ ? topic"));
    assert!(
        ports
            .iter()
            .any(|p| p.line == 12 && p.label == "⇢ ? zzz.*.qqq")
    );
    assert!(
        ports
            .iter()
            .any(|p| p.line == 6 && p.label == "⇢ world.entity.moved")
    );

    // Bundles count channel kinds between clusters.
    let bundle_kinds: HashSet<&str> = v
        .bootstrap
        .base
        .bundles
        .iter()
        .flat_map(|b| b.kinds.keys().map(String::as_str))
        .collect();
    assert!(
        bundle_kinds
            .iter()
            .any(|k| ["subscribe", "http-request", "define", "read", "deploy"].contains(k))
    );
}
