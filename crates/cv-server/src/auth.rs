//! §10 bearer-token auth for `cv serve`: every /api and /mcp request carries
//! `Authorization: Bearer <token>`, or `?token=<token>` where a header can't be set (the browser's
//! WebSocket, the first page load). The token comes from `--token` / `CV_TOKEN`, or is generated
//! once and kept in one per-user file (`~/.local/share/code-viewer/token`) that every later run, on
//! any repo, reuses: MCP registrations and bookmarked URLs keep working. `--token` / `CV_TOKEN`
//! apply to that run only and never overwrite the file.
//!
//! There is no auth cookie: browsers send a cookie for 127.0.0.1 to every port on it, so any other
//! local server would receive the token. The UI keeps it in its own origin's storage (one per port)
//! and sends it as a header. Requests a browser marks with another site's `Origin` are refused
//! ([`local_hosts_only`]), so a page on another port cannot use the live event stream either.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// A fresh 128-bit token, hex encoded.
pub fn generate() -> String {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf).expect("os randomness");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Where `cv serve` keeps its token for local tools: one owner-only file per user, shared by every
/// repo, so the key stays the same across restarts, repos and ports.
pub fn token_file() -> PathBuf {
    cv_store::StorePaths::data_root().join("token")
}

/// The token a previous run left in `path`, if any: reused so URLs and registered MCP headers keep
/// working across restarts.
pub fn read_token(path: &Path) -> Option<String> {
    let t = std::fs::read_to_string(path).ok()?;
    let t = t.trim();
    (t.len() >= 16 && t.chars().all(|c| c.is_ascii_alphanumeric())).then(|| t.to_string())
}

pub fn write_token(path: &Path, token: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        let mut b = std::fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        b.create(dir)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    std::io::Write::write_all(&mut opts.open(path)?, token.as_bytes())
}

/// The stable token in `file`, created on first use: the one already there; else, the first time,
/// `legacy` (the per-repo token earlier versions kept, so an existing registration keeps working);
/// else a new one. Once written, the file is never replaced. Returns the token and whether the
/// file was written just now.
pub fn stable_token(file: &Path, legacy: Option<&Path>) -> std::io::Result<(String, bool)> {
    if let Some(t) = read_token(file) {
        return Ok((t, false));
    }
    let t = legacy.and_then(read_token).unwrap_or_else(generate);
    write_token(file, &t)?;
    Ok((t, true))
}

/// Constant-time comparison, so a wrong guess learns nothing from timing.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn query_token(req: &Request) -> Option<&str> {
    req.uri()
        .query()?
        .split('&')
        .find_map(|kv| kv.strip_prefix("token="))
}

/// Tokens the request presents, in order: bearer header, `?token=`.
fn presented(req: &Request) -> Vec<&str> {
    let mut out = Vec::new();
    if let Some(v) = req.headers().get(header::AUTHORIZATION)
        && let Some(t) = v.to_str().ok().and_then(|v| v.strip_prefix("Bearer "))
    {
        out.push(t.trim());
    }
    out.extend(query_token(req));
    out
}

/// The 401 message: where the right token is.
pub fn unauthorized_message(token_file: Option<&Path>) -> String {
    let at = match token_file {
        Some(f) => format!("also in {}", f.display()),
        None => "also in ~/.local/share/code-viewer/token".to_string(),
    };
    format!(
        "missing or wrong bearer token: use the token `cv serve` prints at startup ({at}) as \
         `Authorization: Bearer <token>` or ?token=<token> (open the UI with the URL it prints)"
    )
}

/// Middleware: 401 unless the request presents `token`; `message` says where the token is.
pub async fn require(token: Arc<str>, message: Arc<str>, req: Request, next: Next) -> Response {
    let ok = presented(&req)
        .iter()
        .any(|t| same(t.as_bytes(), token.as_bytes()));
    match ok {
        true => next.run(req).await,
        false => (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            axum::Json(serde_json::json!({
                "error": { "code": "unauthorized", "message": &*message }
            })),
        )
            .into_response(),
    }
}

/// Whether a Host header names this machine's loopback interface (127.0.0.1, localhost or [::1],
/// any port). An empty Host (HTTP/1.0) passes: a browser always sends the page's own.
pub fn is_local_host(host: &str) -> bool {
    let name = match host.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => host.rsplit_once(':').map_or(host, |(h, _)| h),
    };
    host.is_empty()
        || matches!(
            name.to_ascii_lowercase().as_str(),
            "127.0.0.1" | "localhost" | "::1"
        )
}

/// Whether a browser's `Origin` header (absent for non-browser clients and same-origin GETs) is
/// the page this server serves: the same scheme-less host and port the request is addressed to.
pub fn same_origin(origin: Option<&str>, host: &str) -> bool {
    match origin {
        None => true,
        Some(o) => o
            .strip_prefix("http://")
            .is_some_and(|h| h.eq_ignore_ascii_case(host)),
    }
}

/// Refuse requests whose Host is not the loopback interface, and browser requests from another
/// origin. The server listens on 127.0.0.1 only, but a web page elsewhere can rebind its own name
/// to 127.0.0.1 (DNS rebinding) and then read and edit code through the browser, which sends the
/// attacker's name as Host; with `--no-auth` nothing else would stop it. A page on another local
/// port is a different origin too: the WebSocket handshake carries its `Origin`, so it cannot
/// listen to the live events.
pub fn local_hosts_only(router: axum::Router) -> axum::Router {
    router.layer(axum::middleware::from_fn(
        |req: Request, next: Next| async move {
            let refused = {
                let h = |name| req.headers().get(name).and_then(|v| v.to_str().ok());
                let host = h(header::HOST).unwrap_or("");
                if !is_local_host(host) {
                    Some((
                        "forbidden-host",
                        "cv serve answers only requests addressed to 127.0.0.1 or localhost",
                    ))
                } else if !same_origin(h(header::ORIGIN), host) {
                    Some((
                        "forbidden-origin",
                        "cv serve answers only its own pages, not other sites or ports",
                    ))
                } else {
                    None
                }
            };
            match refused {
                None => next.run(req).await,
                Some((code, message)) => (
                    StatusCode::FORBIDDEN,
                    axum::Json(
                        serde_json::json!({ "error": { "code": code, "message": message } }),
                    ),
                )
                    .into_response(),
            }
        },
    ))
}

/// Wrap `router` so every route requires `token`.
pub fn protect(router: axum::Router, token: &str) -> axum::Router {
    protect_with(router, token, None)
}

/// [`protect`], naming the token file in the 401 body.
pub fn protect_with(router: axum::Router, token: &str, token_file: Option<&Path>) -> axum::Router {
    let token: Arc<str> = token.into();
    let message: Arc<str> = unauthorized_message(token_file).into();
    router.layer(axum::middleware::from_fn(move |req, next| {
        require(token.clone(), message.clone(), req, next)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::get;
    use tower::ServiceExt;

    async fn status(app: &axum::Router, req: axum::http::Request<Body>) -> StatusCode {
        app.clone().oneshot(req).await.unwrap().status()
    }

    #[test]
    fn the_stable_token_is_made_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cv").join("token");
        let legacy = dir.path().join("legacy");
        std::fs::write(&legacy, "0123456789abcdef0123456789abcdef").unwrap();
        // First run: the per-repo token of earlier versions is adopted.
        let (t, new) = stable_token(&file, Some(&legacy)).unwrap();
        assert_eq!(
            (t.as_str(), new),
            ("0123456789abcdef0123456789abcdef", true)
        );
        // Later runs, from any repo, get the same one and never rewrite it.
        let other = dir.path().join("other");
        std::fs::write(&other, "ffffffffffffffffffffffffffffffff").unwrap();
        assert_eq!(
            stable_token(&file, Some(&other)).unwrap(),
            (t.clone(), false)
        );
        assert_eq!(stable_token(&file, None).unwrap(), (t, false));
        // Without a usable legacy token a new one is generated.
        let fresh = dir.path().join("fresh");
        let (g, new) = stable_token(&fresh, Some(&dir.path().join("missing"))).unwrap();
        assert!(new && g.len() == 32 && read_token(&fresh) == Some(g));
    }

    #[tokio::test]
    async fn answers_only_loopback_hosts() {
        let app = local_hosts_only(axum::Router::new().route("/api/v1/x", get(|| async { "ok" })));
        let req = |host: &str| {
            axum::http::Request::builder()
                .uri("/api/v1/x")
                .header(header::HOST, host)
                .body(Body::empty())
                .unwrap()
        };
        for ok in [
            "127.0.0.1:7878",
            "localhost:5178",
            "LOCALHOST",
            "[::1]:7878",
            "",
        ] {
            assert_eq!(status(&app, req(ok)).await, StatusCode::OK, "{ok}");
        }
        for bad in [
            "evil.example:7878",
            "127.0.0.1.evil.example",
            "localhost.evil.example:80",
        ] {
            assert_eq!(status(&app, req(bad)).await, StatusCode::FORBIDDEN, "{bad}");
        }
        // A browser's Origin must be the page this server serves (WebSocket handshakes included).
        let from = |origin: &str| {
            axum::http::Request::builder()
                .uri("/api/v1/x")
                .header(header::HOST, "127.0.0.1:7878")
                .header(header::ORIGIN, origin)
                .body(Body::empty())
                .unwrap()
        };
        assert_eq!(
            status(&app, from("http://127.0.0.1:7878")).await,
            StatusCode::OK
        );
        for bad in [
            "http://127.0.0.1:5000",
            "http://localhost:7878",
            "https://evil.example",
            "null",
        ] {
            assert_eq!(
                status(&app, from(bad)).await,
                StatusCode::FORBIDDEN,
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn requires_the_token() {
        let app = protect(
            axum::Router::new().route("/api/v1/x", get(|| async { "ok" })),
            "s3cret",
        );
        let req = |uri: &str, auth: Option<&str>| {
            let mut b = axum::http::Request::builder().uri(uri);
            if let Some(a) = auth {
                b = b.header(header::AUTHORIZATION, a);
            }
            b.body(Body::empty()).unwrap()
        };
        assert_eq!(
            status(&app, req("/api/v1/x", None)).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status(&app, req("/api/v1/x", Some("Bearer nope"))).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status(&app, req("/api/v1/x?token=s3cre", None)).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status(&app, req("/api/v1/x", Some("Bearer s3cret"))).await,
            StatusCode::OK
        );
        assert_eq!(
            status(&app, req("/api/v1/x?a=1&token=s3cret", None)).await,
            StatusCode::OK
        );
        let res = app
            .clone()
            .oneshot(req("/api/v1/x", Some("Bearer nope")))
            .await
            .unwrap();
        let body = axum::body::to_bytes(res.into_body(), 1 << 16)
            .await
            .unwrap();
        let body = String::from_utf8_lossy(&body);
        assert!(
            body.contains("cv serve") && body.contains("token"),
            "{body}"
        );
        assert_eq!(generate().len(), 32);
        assert_ne!(generate(), generate());
    }
}
