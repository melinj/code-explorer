//! `cv`: the Code Viewer server.
//!
//! ```text
//! cv serve [path] [--port 7878] [--max-commits 2000] [--token T | --no-auth] [--ui DIR | --no-ui]
//!                                                      index the snapshot at @, serve /api/v1, /mcp and the UI
//! cv index [path] [--max-commits 2000]                 index once, print timings and exit
//! cv mcp [--server http://127.0.0.1:7878] [--profile editor] [--agent NAME] [--workspace WS]
//!                                                      MCP over stdio, backed by a running server
//! ```
//! `cv serve` also serves MCP (Streamable HTTP) at /mcp. Both require a bearer token (§10): from
//! `--token T` or `CV_TOKEN` (this run only), else the stable per-user one in
//! ~/.local/share/code-viewer/token (generated on first use, never replaced); `--no-auth` turns the
//! check off for local development.
//!
//! Reads go through jj-lib at the head operation and never snapshot or write to the repo.

use std::path::PathBuf;
use std::sync::Arc;

use cv_server::index::{Options, View};
use cv_server::routes;

/// The value of `--name V` or `--name=V`.
fn arg(args: &[String], name: &str) -> Option<String> {
    let eq = format!("{name}=");
    args.iter()
        .enumerate()
        .find_map(|(i, a)| match a.strip_prefix(&eq) {
            Some(v) => Some(v.to_string()),
            None if a == name => args.get(i + 1).cloned(),
            None => None,
        })
}

/// A numeric option: its value, the default when absent, or exit with a message when malformed.
fn num_arg<T: std::str::FromStr>(args: &[String], name: &str, default: T) -> T {
    match arg(args, name) {
        None => default,
        Some(v) => v.parse().unwrap_or_else(|_| {
            eprintln!("cv: {name} needs a number, got '{v}'");
            std::process::exit(2);
        }),
    }
}

fn positional(args: &[String]) -> Option<String> {
    let mut i = 2;
    while i < args.len() {
        if args[i] == "--no-auth"
            || args[i] == "--no-ui"
            || (args[i].starts_with("--") && args[i].contains('='))
        {
            i += 1;
            continue;
        }
        if args[i].starts_with("--") {
            i += 2;
            continue;
        }
        return Some(args[i].clone());
    }
    None
}

/// §9 budgets for a repo of about 6k files, by timing label.
fn budget(label: &str) -> Option<&'static str> {
    match label {
        "parse" => Some("< 20 s"),
        "churn (parallel)" => Some("< 10 s"),
        "links + connectedness" => Some("< 2 s"),
        _ => None,
    }
}

fn log_view(v: &View) {
    let b = &v.bootstrap.base;
    eprintln!(
        "view {} @ {}: {} files, {} windows, {} clusters, {} links, world {:.0}×{:.0} wu; churn {} commits ({}, {:.0}% settled)",
        b.view.id,
        &b.view.resolved.head[..12.min(b.view.resolved.head.len())],
        v.files.len(),
        v.windows.len(),
        b.clusters.len(),
        v.links.len(),
        b.world.w,
        b.world.h,
        v.bootstrap.churn.scanned_commits,
        v.bootstrap.churn.stop,
        v.bootstrap.churn.settled_mass * 100.0
    );
    conn_stats(v);
    for (label, d) in &v.timings {
        match budget(label) {
            Some(bud) => eprintln!("  {label:<24} {:>10.1?}   (budget {bud})", d),
            None => eprintln!("  {label:<24} {:>10.1?}", d),
        }
    }
}

/// Code-lane connectedness at a glance (DESIGN §4.8): windows per grade, the 2-hop spread, and how
/// many windows reach at least half of the lane (saturation, which D7's 2-hop measure avoids).
fn conn_stats(v: &View) {
    let code: Vec<&cv_contracts::Connectedness> = v
        .windows
        .iter()
        .filter(|w| w.header.lane == "code")
        .map(|w| &w.header.conn)
        .collect();
    if code.is_empty() {
        return;
    }
    let mut grades = [0usize; 5];
    for c in &code {
        grades[c.grade.min(4) as usize] += 1;
    }
    let mut d: Vec<u32> = code.iter().map(|c| c.dependents2).collect();
    d.sort_unstable();
    let distinct = {
        let mut x = d.clone();
        x.dedup();
        x.len()
    };
    let half = code.len() as u32 / 2;
    let saturated = d.iter().filter(|&&x| x >= half.max(1)).count();
    let tested = code.iter().filter(|c| c.tests > 0).count();
    let mut top: Vec<(u32, &str)> = v
        .windows
        .iter()
        .filter(|w| w.header.lane == "code")
        .map(|w| (w.header.conn.dependents2, w.header.key.as_str()))
        .collect();
    top.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    let top: Vec<String> = top
        .iter()
        .take(5)
        .map(|(d, k)| format!("{k} ({d})"))
        .collect();
    let cycle = v
        .windows
        .iter()
        .filter_map(|w| w.header.conn.cycle)
        .max()
        .unwrap_or(0);
    let candidates = v
        .links
        .iter()
        .filter(|l| l.confidence <= 0.5 && l.channel.is_none())
        .count();
    eprintln!(
        "  connectedness (code lane, {} windows): core {} · high {} · medium {} · low {} · isolated {}; dependents2 max {} median {} distinct {}; {} saturated (≥ ½ lane); {} with tests; largest cycle {} files; {} lexical candidates\n  most depended on: {}",
        code.len(),
        grades[4],
        grades[3],
        grades[2],
        grades[1],
        grades[0],
        d.last().copied().unwrap_or(0),
        d[d.len() / 2],
        distinct,
        saturated,
        tested,
        cycle,
        candidates,
        top.join(", ")
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("");
    if matches!(cmd, "--version" | "-V" | "version") {
        println!("cv {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    const USAGE: &str = "usage: cv serve [path] [--port 7878] [--max-commits 2000] [--token T | --no-auth] [--ui DIR | --no-ui]\n       cv index [path] [--max-commits 2000]\n       cv mcp [--server http://127.0.0.1:7878] [--profile reader|reviewer|editor|full] [--agent NAME] [--workspace WS]";
    // `cv --help`, `cv help`, `cv serve --help`, `cv index -h`: print the usage and exit; never
    // start a server or index anything.
    let wants_help = matches!(cmd, "help" | "--help" | "-h")
        || (matches!(cmd, "serve" | "index" | "mcp")
            && args[2..].iter().any(|a| a == "--help" || a == "-h"));
    if wants_help {
        println!("{USAGE}");
        return;
    }
    if cmd == "mcp" {
        std::process::exit(cv_mcp::run_stdio_shim(&args));
    }
    if !matches!(cmd, "serve" | "index") {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    let path = PathBuf::from(positional(&args).unwrap_or_else(|| ".".into()));
    // B3: reuse parses across runs (index.db; CV_PARSE_CACHE=off to disable).
    cv_server::parse_cache::enable();
    // `cv index` always indexes and leaves the result in index.db for the next `cv serve`.
    cv_server::view_store::set_wait_for_save(cmd == "index");
    let port: u16 = num_arg(&args, "--port", 7878);
    let opts = Options {
        max_commits: num_arg(&args, "--max-commits", 2000),
    };

    let repo = match cv_vcs::JjRepo::open(&path) {
        Ok(r) => r,
        Err(e) => {
            // Name the folder unless the error already does.
            let msg = e.to_string();
            if msg.contains(&*path.to_string_lossy()) {
                eprintln!("cv: {msg}");
            } else {
                eprintln!("cv: {}: {msg}", path.display());
            }
            // A plain git repository: jj can be added alongside it.
            let git = path.ancestors().any(|p| p.join(".git").exists());
            let jj = path.ancestors().any(|p| p.join(".jj").exists());
            if git && !jj {
                eprintln!(
                    "cv: this is a git repository without jj; run `jj git init --colocate` at its root (git keeps working)"
                );
            }
            std::process::exit(1);
        }
    };
    eprintln!(
        "repo {} (workspace {}, op {})",
        repo.root().display(),
        repo.workspace_name(),
        &repo.op_id()[..12]
    );
    let root = repo.root().to_path_buf();
    let state = Arc::new(routes::AppState::new(repo, opts));
    // Review, edits and live events: review.db, the event log and the op-head watcher. Opened
    // before the first index so stored pins and cluster labels apply to it.
    let review = if cmd == "serve" {
        Some(state.review())
    } else {
        None
    };
    let spec = serde_json::json!({ "mode": "snapshot", "rev": "@" });
    match state.view_for_spec(&spec) {
        Ok(v) => log_view(&v),
        Err(e) => {
            eprintln!("cv: indexing failed: {}", e.message);
            std::process::exit(1);
        }
    }
    let Some(review) = review else {
        return;
    };
    match review {
        Ok(r) => {
            let events = r.clone();
            cv_server::index::on_progress(move |job, done, total| {
                events.emit(cv_contracts::review::ServerEvent::IndexProgress {
                    job: job.into(),
                    done,
                    total,
                    view_id: None,
                });
            });
            eprintln!(
                "review data in {} (event cursor {})",
                cv_store::StorePaths::for_repo(r.vcs().root())
                    .review_db
                    .display(),
                r.events().cursor()
            );
        }
        Err(e) => eprintln!("cv: review service unavailable: {e}"),
    }
    // Workspaces (D12): live overlays of unsnapshotted files, overlaps, workspace.updated.
    cv_server::workspaces::start(&state);
    let token_file = cv_server::auth::token_file();
    let token = if args.iter().any(|a| a == "--no-auth") {
        eprintln!("cv: auth disabled (--no-auth)");
        None
    } else if let Some(t) =
        arg(&args, "--token").or_else(|| std::env::var("CV_TOKEN").ok().filter(|t| !t.is_empty()))
    {
        // An explicit token is for this run only: the stable one in the token file stays as it is.
        Some(t)
    } else {
        // The stable token: generated once, then the same for every run on every repo.
        let legacy = cv_store::StorePaths::for_repo(&root)
            .review_db
            .with_file_name("token");
        match cv_server::auth::stable_token(&token_file, Some(&legacy)) {
            Ok((t, new)) => {
                eprintln!(
                    "bearer token in {}{}",
                    token_file.display(),
                    if new { " (new)" } else { "" }
                );
                Some(t)
            }
            Err(e) => {
                eprintln!(
                    "cv: cannot write {}: {e}; using a token for this run only",
                    token_file.display()
                );
                Some(cv_server::auth::generate())
            }
        }
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .unwrap_or_else(|e| {
                eprintln!("cv: cannot bind 127.0.0.1:{port}: {e}");
                std::process::exit(1);
            });
        let base = format!("http://127.0.0.1:{port}");
        let q = token.as_deref().map(|t| format!("?token={t}")).unwrap_or_default();
        eprintln!("cv serving on {base}/api/v1");
        let app = routes::router(state.clone()).merge(cv_server::mcp::router(state));
        let app = match &token {
            Some(t) => cv_server::auth::protect_with(app, t, Some(&token_file)),
            None => app,
        };
        // The UI's static files sit outside the token check (the page reads ?token= itself).
        let app = match (args.iter().any(|a| a == "--no-ui"), cv_server::ui::find_dir(arg(&args, "--ui"))) {
            (true, _) => app,
            (false, Some(dir)) => {
                eprintln!("\n  UI:  {base}/{q}\n");
                app.merge(cv_server::ui::router(&dir))
            }
            (false, None) => {
                eprintln!("cv: no built UI found (run `npm install && npm run build` in web/, or pass --ui DIR)");
                app
            }
        };
        // Only requests addressed to the loopback interface (no DNS rebinding from a web page).
        let app = cv_server::auth::local_hosts_only(app);
        let header = token.as_deref().map(|t| format!(" --header \"Authorization: Bearer {t}\"")).unwrap_or_default();
        eprintln!("  MCP: claude mcp add --transport http cv {base}/mcp/editor{header}\n");
        axum::serve(listener, app).await.expect("server");
    });
}
