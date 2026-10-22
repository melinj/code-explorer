//! cv-mcp (B9): the agent interface of docs/contracts/mcp.ts on rmcp.
//!
//! - [`server::CvMcp`]: every tool (gated by the reader/reviewer/editor/full profiles), resources, prompts,
//!   session binding (`bind_workspace`) and `agent.attention`.
//! - [`backend::McpBackend`]: the narrow service trait the tools run on; [`http::HttpBackend`] calls a
//!   running cv server's /api/v1, [`fake::FakeBackend`] is in-memory for tests.
//! - Transports: [`serve_stdio`] (the `cv mcp` shim) and [`router`] (Streamable HTTP at /mcp).

pub mod annotate;
pub mod backend;
pub mod fake;
pub mod http;
pub mod params;
pub mod server;
pub mod subscribe;

use std::sync::Arc;

use rmcp::ServiceExt;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};

pub use backend::{AgentId, BackendError, McpBackend};
pub use server::{CvMcp, McpConfig, Profile};

/// Serve MCP over stdin/stdout until the client disconnects.
pub async fn serve_stdio(
    backend: Arc<dyn McpBackend>,
    config: McpConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = CvMcp::new(backend, config);
    let running = server.serve(rmcp::transport::stdio()).await?;
    running.waiting().await?;
    Ok(())
}

/// Streamable HTTP endpoints: `/mcp` (the configured profile, editor by default) and
/// `/mcp/reader`, `/mcp/reviewer`, `/mcp/editor`, `/mcp/full`. Sessions share bindings and caches.
pub fn router(backend: Arc<dyn McpBackend>, config: McpConfig) -> axum::Router {
    let default = config.profile;
    let shared = CvMcp::shared(backend, config);
    let service = |profile: Profile| {
        let shared = shared.clone();
        StreamableHttpService::new(
            move || Ok(CvMcp::from_shared(shared.clone(), profile)),
            Arc::new(LocalSessionManager::default()),
            StreamableHttpServerConfig::default(),
        )
    };
    axum::Router::new()
        .nest_service("/mcp", service(default))
        .nest_service("/mcp/reader", service(Profile::Reader))
        .nest_service("/mcp/reviewer", service(Profile::Reviewer))
        .nest_service("/mcp/editor", service(Profile::Editor))
        .nest_service("/mcp/full", service(Profile::Full))
}

/// Options shared by `cv mcp` and `cv-mcp`: `--profile`, `--agent`, `--workspace`, `--token`.
pub fn config_from_args(args: &[String]) -> Result<McpConfig, String> {
    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    };
    Ok(McpConfig {
        profile: arg("--profile")
            .map(|p| p.parse())
            .transpose()?
            .unwrap_or(Profile::Editor),
        agent: arg("--agent"),
        workspace: arg("--workspace"),
        cwd: None,
    })
}

/// `cv mcp --server URL [--profile P] [--agent NAME] [--workspace WS] [--token T]`: the stdio shim.
pub fn run_stdio_shim(args: &[String]) -> i32 {
    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let server = arg("--server").unwrap_or_else(|| "http://127.0.0.1:7878".into());
    let config = match config_from_args(args) {
        // Without --workspace, the jj workspace holding the client's cwd is the default one.
        Ok(c) => McpConfig {
            cwd: std::env::current_dir().ok(),
            ..c
        },
        Err(e) => {
            eprintln!("cv mcp: {e}");
            return 2;
        }
    };
    let token = arg("--token")
        .or_else(|| std::env::var("CV_TOKEN").ok().filter(|t| !t.is_empty()))
        .or_else(stored_token);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let backend: Arc<dyn McpBackend> = Arc::new(http::HttpBackend::new(&server, token));
    match rt.block_on(serve_stdio(backend, config)) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("cv mcp: {e}");
            1
        }
    }
}

/// The stable token `cv serve` keeps for local tools (`$XDG_DATA_HOME/code-viewer/token`, else
/// `~/.local/share/code-viewer/token`), so `cv mcp` needs no token argument on this machine.
pub fn stored_token() -> Option<String> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share"))
        })?;
    let t = std::fs::read_to_string(data.join("code-viewer").join("token")).ok()?;
    let t = t.trim();
    (!t.is_empty()).then(|| t.to_string())
}
