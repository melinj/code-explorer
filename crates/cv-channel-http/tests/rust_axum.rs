//! P2 end to end with the Rust pack: axum routes (literal, constant and `format!` keys, method
//! routers `get(a).post(b)`, `axum::routing::put`, `.nest` prefixes) meet TypeScript `fetch` and
//! Rust reqwest clients at one `METHOD /path` hub each.

use cv_channel::{Binding, FileIn, Project, Target};

const PATHS: &str = r#"pub const BLOBS: &str = "/api/v1/blobs/{blob}/text";
"#;

const ROUTES: &str = r#"use axum::{Router, routing::{get, post}};
use crate::paths::BLOBS;

const VIEWS: &str = "/api/v1/views";

pub fn router() -> Router {
    Router::new()
        .route(VIEWS, post(create_view))
        .route("/api/v1/threads", get(list_threads).post(create_thread))
        .route(BLOBS, get(blob_text))
        .route(&format!("{VIEWS}/{{view}}"), axum::routing::put(put_view))
        .nest("/api/v2", v2())
}

fn v2() -> Router {
    Router::new().route("/repo", get(repo))
}
"#;

const CLIENT_TS: &str = r#"export async function load(id: string, blob: string) {
  await fetch("/api/v1/views", { method: "POST" });
  await fetch(`/api/v1/views/${id}`, { method: "PUT" });
  await fetch("/api/v1/threads");
  await fetch("/api/v1/threads", { method: "POST" });
  await fetch(`/api/v1/blobs/${blob}/text?from=1`);
  await fetch("/api/v2/repo");
  await fetch("/api/v1/nowhere");
}
"#;

const CLIENT_RS: &str = r#"pub async fn repo(client: &reqwest::Client, base: &str) {
    client.get(format!("{base}/api/v2/repo")).send().await.unwrap();
}
"#;

#[test]
fn axum_routes_meet_fetch_and_reqwest_clients() {
    let mut langs = cv_lang::Registry::new();
    langs.register(Box::new(cv_lang_rust::RustPack::default()));
    langs.register(Box::new(cv_lang_typescript::TypeScriptPack));
    let mut chan = cv_channel::Registry::new();
    chan.register(Box::new(cv_channel_http::HttpPack::new()));
    let file = |path: &'static str, lang: &'static str, text: &'static str| FileIn {
        path,
        lane: "code",
        lang: Some(lang),
        conv: Default::default(),
        text,
        config_keys: &[],
        imports: vec![],
        symbols: vec![],
    };
    let mut files = vec![
        file("src/paths.rs", "rust", PATHS),
        file("src/routes.rs", "rust", ROUTES),
        file("web/api.ts", "typescript", CLIENT_TS),
        file("src/client.rs", "rust", CLIENT_RS),
    ];
    files[1].imports = vec![Binding {
        local: "BLOBS",
        imported: Some("BLOBS"),
        target: Some(0),
    }];
    let project = Project::new(&files);
    let out = chan.run(&langs, &project, &[Some(0), Some(0), Some(1), Some(0)]);

    let mut hubs: Vec<&str> = out.hubs.iter().map(|h| h.display.as_str()).collect();
    hubs.sort_unstable();
    assert_eq!(
        hubs,
        vec![
            "GET /api/v1/blobs/{}/text",
            "GET /api/v1/threads",
            "GET /api/v2/repo",
            "POST /api/v1/threads",
            "POST /api/v1/views",
            "PUT /api/v1/views/{}",
        ],
        "{:?}",
        out.notes
    );
    let mut links: Vec<(usize, u32, usize, u32)> = out
        .links
        .iter()
        .filter_map(|l| match l.to {
            Target::Site { file, line } => Some((l.from_file, l.from_line, file, line)),
            _ => None,
        })
        .collect();
    links.sort_unstable();
    assert_eq!(
        links,
        vec![
            (2, 2, 1, 8),
            (2, 3, 1, 11),
            (2, 4, 1, 9),
            (2, 5, 1, 9),
            (2, 6, 1, 10),
            (2, 7, 1, 16),
            (3, 2, 1, 16),
        ]
    );
    // No route for /api/v1/nowhere: no link from that line.
    assert!(
        !out.links
            .iter()
            .any(|l| l.from_file == 2 && l.from_line == 8 && l.hub.is_some())
    );
}
