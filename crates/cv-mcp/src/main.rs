//! `cv-mcp`: the MCP server in front of any running cv server (cv serve or cv-mock).
//!
//! ```text
//! cv-mcp stdio --server http://127.0.0.1:7878 [--profile editor] [--agent claude] [--workspace gym]
//! cv-mcp http  --server http://127.0.0.1:7878 [--port 7879] [--profile editor] …
//! ```
//! `stdio` is the same shim as `cv mcp`; `http` serves Streamable HTTP at /mcp (and /mcp/{profile})
//! for servers that do not mount /mcp themselves, such as cv-mock.

use std::sync::Arc;

use cv_mcp::{McpBackend, http::HttpBackend};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    };
    match args.get(1).map(String::as_str) {
        Some("stdio") => std::process::exit(cv_mcp::run_stdio_shim(&args)),
        Some("http") => {
            let server = arg("--server").unwrap_or_else(|| "http://127.0.0.1:7878".into());
            let port: u16 = arg("--port").and_then(|p| p.parse().ok()).unwrap_or(7879);
            let config = cv_mcp::config_from_args(&args).unwrap_or_else(|e| {
                eprintln!("cv-mcp: {e}");
                std::process::exit(2)
            });
            let token = arg("--token")
                .or_else(|| std::env::var("CV_TOKEN").ok().filter(|t| !t.is_empty()))
                .or_else(cv_mcp::stored_token);
            let backend: Arc<dyn McpBackend> = Arc::new(HttpBackend::new(&server, token));
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                    .await
                    .unwrap_or_else(|e| {
                        eprintln!("cv-mcp: cannot bind 127.0.0.1:{port}: {e}");
                        std::process::exit(1)
                    });
                eprintln!("cv-mcp: MCP at http://127.0.0.1:{port}/mcp → {server}");
                axum::serve(listener, cv_mcp::router(backend, config))
                    .await
                    .expect("server");
            });
        }
        _ => {
            eprintln!(
                "usage: cv-mcp stdio --server URL [--profile reader|reviewer|editor|full] [--agent NAME] [--workspace WS] [--token T]\n       cv-mcp http  --server URL [--port 7879] [same options]"
            );
            std::process::exit(2);
        }
    }
}
