//! Serving a plain git repository (no jj yet, http.ts `GET /setup`): the history graph read-only,
//! from the `git` CLI, and initialising jj from the UI (`jj git init --colocate`), after which `cv
//! serve` restarts itself on the now-jj repository. Every other API route answers 409
//! "jj-required", so the UI can say what is missing instead of failing piecemeal.
//!
//! git runs argv only (no shell), without a pager, prompts, signature checks or fsmonitor; revisions
//! from the client are checked (no options except `--all`, `--branches`, `--tags`, `--remotes`) and
//! passed after `--end-of-options`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path as UrlPath, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use cv_contracts::api::{GitCommitDetail, GitFileStat, JjInstall, SetupInfo};
use cv_contracts::readside::{
    GraphEdge, GraphRow, Revision, RevisionFlags, RevisionsResponse, ShortestPrefix, Signature,
};
use cv_contracts::review::{BookmarkInfo, BookmarkRemote, RefsResponse};
use serde::Deserialize;
use serde_json::json;

/// What "Initialise jj" runs at the repository root.
pub const INIT_COMMAND: &str = "jj git init --colocate";
/// Set while "Initialise jj" from the UI runs (it restarts the server itself): a watcher for a
/// terminal `jj git init` stays out of its way.
pub static INIT_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Revision options a client may pass in git mode.
const ALLOWED_FLAGS: [&str; 4] = ["--all", "--branches", "--tags", "--remotes"];

/// A git repository without jj, at its top level.
pub struct GitRepo {
    pub root: PathBuf,
}

fn jj_bin() -> String {
    std::env::var("CV_JJ").unwrap_or_else(|_| "jj".into())
}

/// The jj CLI on PATH (`CV_JJ` overrides), with its version.
pub fn jj_install() -> JjInstall {
    match Command::new(jj_bin())
        .arg("--version")
        .stdin(Stdio::null())
        .output()
    {
        Ok(o) if o.status.success() => JjInstall {
            installed: true,
            version: Some(
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .trim_start_matches("jj ")
                    .to_string(),
            ),
        },
        _ => JjInstall {
            installed: false,
            version: None,
        },
    }
}

/// GET /setup of a jj repository (the full server).
pub fn jj_setup(root: &Path) -> SetupInfo {
    SetupInfo {
        vcs: "jj".into(),
        root: root.display().to_string(),
        jj: jj_install(),
        init_command: INIT_COMMAND.into(),
    }
}

/// Unix seconds → ISO-8601.
fn iso_secs(s: i64) -> String {
    cv_store::iso_from_ms(s * 1000)
}

/// Whether a client's revision token is one git log may take: an allowed option, or a revision
/// spelled with ref-name characters (`main`, `HEAD~20..HEAD`, `origin/x`, `@{u}`, `v1.0^{}`).
fn rev_token_ok(t: &str) -> bool {
    if t.starts_with('-') {
        return ALLOWED_FLAGS.contains(&t);
    }
    !t.is_empty()
        && t.len() <= 200
        && t.chars()
            .all(|c| c.is_alphanumeric() || "._/@^~:{}-+".contains(c))
}

fn hex_ok(s: &str) -> bool {
    (4..=64).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit())
}

impl GitRepo {
    /// The git repository holding `path`, unless it (or a folder above it) is a jj repository.
    pub fn discover(path: &Path) -> Option<GitRepo> {
        if path.ancestors().any(|p| p.join(".jj").is_dir()) {
            return None;
        }
        let out = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(path)
            .stdin(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let root = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        Some(GitRepo { root })
    }

    /// `git <args>` at the root, argv only; stdout on success, git's message otherwise.
    fn git(&self, args: &[&str]) -> Result<String, String> {
        let out = Command::new("git")
            .args([
                "--no-pager",
                "-c",
                "log.showSignature=false",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.quotepath=false",
            ])
            .args(args)
            .current_dir(&self.root)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run git: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Branches (local and remote, `name@remote` like jj) and tags per commit.
    fn refs_by_commit(&self) -> (HashMap<String, Vec<String>>, HashMap<String, Vec<String>>) {
        let mut branches: HashMap<String, Vec<String>> = HashMap::new();
        let mut tags: HashMap<String, Vec<String>> = HashMap::new();
        let Ok(out) = self.git(&[
            "for-each-ref",
            "--format=%(refname)%1f%(objectname)%1f%(*objectname)",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ]) else {
            return (branches, tags);
        };
        for line in out.lines() {
            let mut f = line.split('\x1f');
            let (Some(name), Some(id), peeled) = (f.next(), f.next(), f.next()) else {
                continue;
            };
            let id = peeled.filter(|p| !p.is_empty()).unwrap_or(id).to_string();
            if let Some(b) = name.strip_prefix("refs/heads/") {
                branches.entry(id).or_default().push(b.to_string());
            } else if let Some(r) = name.strip_prefix("refs/remotes/") {
                let Some((remote, b)) = r.split_once('/') else {
                    continue;
                };
                if b != "HEAD" {
                    branches
                        .entry(id)
                        .or_default()
                        .push(format!("{b}@{remote}"));
                }
            } else if let Some(t) = name.strip_prefix("refs/tags/") {
                tags.entry(id).or_default().push(t.to_string());
            }
        }
        (branches, tags)
    }

    /// Worktrees per checked-out commit: the main one is "HEAD", others by folder name.
    fn worktrees(&self) -> (Option<String>, HashMap<String, Vec<String>>) {
        let mut out: HashMap<String, Vec<String>> = HashMap::new();
        let mut head = None;
        let Ok(text) = self.git(&["worktree", "list", "--porcelain"]) else {
            return (
                self.git(&["rev-parse", "--verify", "-q", "HEAD"])
                    .ok()
                    .map(|s| s.trim().to_string()),
                out,
            );
        };
        for (i, block) in text
            .split("\n\n")
            .filter(|b| !b.trim().is_empty())
            .enumerate()
        {
            let path = block
                .lines()
                .find_map(|l| l.strip_prefix("worktree "))
                .unwrap_or("");
            let Some(id) = block.lines().find_map(|l| l.strip_prefix("HEAD ")) else {
                continue;
            };
            let name = if i == 0 {
                head = Some(id.to_string());
                "HEAD".to_string()
            } else {
                Path::new(path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.into())
            };
            out.entry(id.to_string()).or_default().push(name);
        }
        (head, out)
    }

    /// GET /revisions in git mode: `git log --topo-order` of `revs` (default `--all`), with the
    /// server's lanes, branches as bookmarks and worktrees as workspaces.
    pub fn graph(&self, revs: Option<&str>, limit: usize) -> Result<RevisionsResponse, String> {
        let tokens: Vec<&str> = revs.unwrap_or("").split_whitespace().collect();
        if let Some(bad) = tokens.iter().find(|t| !rev_token_ok(t)) {
            return Err(format!(
                "{bad:?}: in a git repository, give revisions (main, HEAD~20..HEAD, origin/x) or --all, --branches, --tags, --remotes"
            ));
        }
        let (flags, names): (Vec<&str>, Vec<&str>) =
            tokens.into_iter().partition(|t| t.starts_with('-'));
        let n = format!("-n{}", limit + 1);
        let mut args = vec![
            "log",
            "--topo-order",
            &n,
            "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%ct%x1f%B%x1e",
        ];
        if names.is_empty() && flags.is_empty() {
            args.push("--all");
        }
        args.extend(flags);
        args.push("--end-of-options");
        args.extend(names);
        args.push("--");
        let text = match self.git(&args) {
            Ok(t) => t,
            // A repository without commits yet.
            Err(e)
                if e.contains("does not have any commits")
                    || e.contains("bad default revision") =>
            {
                String::new()
            }
            Err(e) => return Err(e),
        };
        struct C {
            id: String,
            parents: Vec<String>,
            author: (String, String, i64),
            committed: i64,
            message: String,
        }
        let mut commits: Vec<C> = text
            .split('\x1e')
            .map(|r| r.trim_start_matches('\n'))
            .filter(|r| !r.is_empty())
            .filter_map(|r| {
                let f: Vec<&str> = r.splitn(7, '\x1f').collect();
                if f.len() < 7 {
                    return None;
                }
                Some(C {
                    id: f[0].to_string(),
                    parents: f[1].split_whitespace().map(String::from).collect(),
                    author: (f[2].into(), f[3].into(), f[4].parse().unwrap_or(0)),
                    committed: f[5].parse().unwrap_or(0),
                    message: f[6].trim_end().to_string(),
                })
            })
            .collect();
        let truncated = commits.len() > limit;
        commits.truncate(limit);
        let lanes = crate::readside::lanes::assign(
            &commits
                .iter()
                .map(|c| (c.id.clone(), c.parents.clone()))
                .collect::<Vec<_>>(),
        );
        let (mut branches, mut tags) = self.refs_by_commit();
        let (head, mut worktrees) = self.worktrees();
        let rows = commits
            .into_iter()
            .zip(lanes)
            .map(|(c, (lane, to))| GraphRow {
                lane,
                edges: c
                    .parents
                    .iter()
                    .zip(to)
                    .map(|(p, to_lane)| GraphEdge {
                        to_lane,
                        parent: p.clone(),
                        elided: None,
                    })
                    .collect(),
                rev: Revision {
                    // git has no change ids: the commit stands in for one.
                    change: c.id.clone(),
                    commit: c.id.clone(),
                    parents: c.parents,
                    description: c.message,
                    author: Signature {
                        name: c.author.0,
                        email: c.author.1,
                        time: iso_secs(c.author.2),
                    },
                    committer_time: iso_secs(c.committed),
                    bookmarks: branches.remove(&c.id).unwrap_or_default(),
                    tags: tags.remove(&c.id).unwrap_or_default(),
                    workspaces: worktrees.remove(&c.id).unwrap_or_default(),
                    flags: RevisionFlags {
                        conflict: false,
                        empty: false,
                        immutable: false,
                        divergent: false,
                        hidden: false,
                        working_copy: head.as_deref() == Some(c.id.as_str()),
                    },
                    shortest: ShortestPrefix {
                        change: 7,
                        commit: 7,
                    },
                },
                review: None,
            })
            .collect();
        Ok(RevisionsResponse { rows, truncated })
    }

    /// GET /refs in git mode: local branches with their remote counterparts; the trunk is main,
    /// master or trunk when there is one, else the checked-out branch.
    pub fn refs(&self) -> RefsResponse {
        let mut map: std::collections::BTreeMap<String, BookmarkInfo> = Default::default();
        let (branches, _) = self.refs_by_commit();
        for (id, names) in branches {
            for n in names {
                let (name, remote) = match n.rsplit_once('@') {
                    Some((b, r)) => (b.to_string(), Some(r.to_string())),
                    None => (n.clone(), None),
                };
                let e = map.entry(name.clone()).or_insert_with(|| BookmarkInfo {
                    name,
                    targets: vec![],
                    remotes: vec![],
                });
                match remote {
                    Some(remote) => e.remotes.push(BookmarkRemote {
                        remote,
                        targets: vec![id.clone()],
                        tracked: true,
                        ahead: None,
                        behind: None,
                    }),
                    None => e.targets.push(id.clone()),
                }
            }
        }
        let current = self
            .git(&["symbolic-ref", "-q", "--short", "HEAD"])
            .ok()
            .map(|s| s.trim().to_string());
        let trunk = ["main", "master", "trunk"]
            .iter()
            .find(|t| map.get(**t).is_some_and(|b| !b.targets.is_empty()))
            .map(|t| t.to_string())
            .or(current);
        RefsResponse {
            bookmarks: map.into_values().collect(),
            trunk,
            colocated: false,
            // Read-only: no fetch or push before jj is set up.
            remotes: vec![],
        }
    }

    /// GET /git/commits/:commit: files changed against the first parent (no renames).
    pub fn commit_files(&self, commit: &str) -> Result<GitCommitDetail, String> {
        if !hex_ok(commit) {
            return Err(format!("{commit:?} is not a commit id"));
        }
        let full = self
            .git(&[
                "rev-parse",
                "--verify",
                "-q",
                "--end-of-options",
                &format!("{commit}^{{commit}}"),
            ])
            .map_err(|_| format!("no commit {commit}"))?
            .trim()
            .to_string();
        let out = self.git(&[
            "show",
            "--numstat",
            "-z",
            "--no-renames",
            "--format=",
            &full,
            "--",
        ])?;
        let files = out
            .split('\0')
            .filter(|r| !r.trim().is_empty())
            .filter_map(|r| {
                let mut f = r.trim_start_matches('\n').splitn(3, '\t');
                let (a, d, p) = (f.next()?, f.next()?, f.next()?);
                Some(GitFileStat {
                    path: p.to_string(),
                    added: a.parse().ok(),
                    removed: d.parse().ok(),
                })
            })
            .collect();
        Ok(GitCommitDetail {
            commit: full,
            files,
        })
    }

    /// `jj git init --colocate` at the root (git keeps working alongside).
    pub fn init_jj(&self) -> Result<String, String> {
        let out = Command::new(jj_bin())
            .args(["git", "init", "--colocate"])
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .env_remove("JJ_EDITOR")
            .output()
            .map_err(|e| format!("cannot run {}: {e}", jj_bin()))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if out.status.success() {
            Ok(text.trim().to_string())
        } else {
            Err(text.trim().to_string())
        }
    }
}

/// What git mode needs: the repository and how to restart once jj is there.
pub struct GitState {
    pub repo: GitRepo,
    /// Replaces the process with a fresh `cv serve` (same arguments); None where that is not
    /// supported, and the human restarts it.
    pub restart: Option<Box<dyn Fn() + Send + Sync>>,
}

type St = State<Arc<GitState>>;

fn err(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    let message = message.into();
    (
        status,
        Json(json!({ "error": { "code": code, "message": message }, "code": code, "message": message })),
    )
        .into_response()
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.expect("blocking task")
}

async fn setup(State(s): St) -> Json<SetupInfo> {
    let root = s.repo.root.display().to_string();
    let jj = blocking(jj_install).await;
    Json(SetupInfo {
        vcs: "git".into(),
        root,
        jj,
        init_command: INIT_COMMAND.into(),
    })
}

#[derive(Deserialize)]
struct RevQ {
    revset: Option<String>,
    limit: Option<usize>,
}

async fn revisions(State(s): St, Query(q): Query<RevQ>) -> Response {
    let limit = q.limit.unwrap_or(200).clamp(1, 5000);
    match blocking(move || {
        s.repo
            .graph(q.revset.as_deref().filter(|r| !r.trim().is_empty()), limit)
    })
    .await
    {
        Ok(r) => Json(r).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, "revset-invalid", e),
    }
}

async fn refs(State(s): St) -> Json<RefsResponse> {
    Json(blocking(move || s.repo.refs()).await)
}

async fn commit(State(s): St, UrlPath(c): UrlPath<String>) -> Response {
    match blocking(move || s.repo.commit_files(&c)).await {
        Ok(d) => Json(d).into_response(),
        Err(e) => err(StatusCode::NOT_FOUND, "not-found", e),
    }
}

#[derive(Deserialize)]
struct InitReq {
    #[serde(default)]
    confirm: bool,
}

async fn init_jj(State(s): St, headers: HeaderMap, body: Option<Json<InitReq>>) -> Response {
    if headers.contains_key("x-cv-agent") {
        return err(
            StatusCode::FORBIDDEN,
            "agent-not-allowed",
            "only the human initialises jj in their repository",
        );
    }
    if !body.is_some_and(|b| b.confirm) {
        return err(
            StatusCode::BAD_REQUEST,
            "invalid",
            "send { \"confirm\": true } to run `jj git init --colocate`",
        );
    }
    let s2 = s.clone();
    let res = blocking(move || {
        if !jj_install().installed {
            return Err((
                StatusCode::CONFLICT,
                "jj-missing",
                "jj is not installed (https://jj-vcs.github.io/jj/latest/install-and-setup/)"
                    .to_string(),
            ));
        }
        INIT_RUNNING.store(true, std::sync::atomic::Ordering::SeqCst);
        let r = s2
            .repo
            .init_jj()
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, "jj-error", e));
        if r.is_err() || s2.restart.is_none() {
            INIT_RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
        }
        r
    })
    .await;
    match res {
        Ok(out) => {
            eprintln!("cv: {INIT_COMMAND} in {}: {out}", s.repo.root.display());
            let restarting = s.restart.is_some();
            if restarting {
                // Answer first, then become the full server (the port is free again after exec).
                let s3 = s.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    if let Some(r) = &s3.restart {
                        r();
                    }
                });
            }
            Json(json!({
                "ok": true,
                "restarting": restarting,
                "message": if restarting { "jj is set up; the server restarts and indexes the repository".to_string() } else { format!("jj is set up; restart cv serve ({out})") },
            }))
            .into_response()
        }
        Err((status, code, message)) => err(status, code, message),
    }
}

async fn jj_required() -> Response {
    err(
        StatusCode::CONFLICT,
        "jj-required",
        format!(
            "this is a git repository without jj: the map, reviews and edits need it. Initialise it from the UI, or run `{INIT_COMMAND}` at the repository root"
        ),
    )
}

/// Every API route of git mode; the rest answer 409 "jj-required".
pub fn router(state: Arc<GitState>) -> Router {
    Router::new()
        .route("/api/v1/setup", get(setup))
        .route("/api/v1/setup:init-jj", post(init_jj))
        .route("/api/v1/revisions", get(revisions))
        .route("/api/v1/refs", get(refs))
        .route("/api/v1/workspaces", get(|| async { Json(json!([])) }))
        .route("/api/v1/git/commits/{commit}", get(commit))
        .route("/api/v1/{*rest}", any(jj_required))
        .route("/mcp/{*rest}", any(jj_required))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::{hex_ok, rev_token_ok};

    #[test]
    fn client_revisions_cannot_be_options() {
        for ok in [
            "main",
            "HEAD~20..HEAD",
            "origin/feature-x",
            "@{u}",
            "v1.0^{}",
            "--all",
            "--branches",
        ] {
            assert!(rev_token_ok(ok), "{ok}");
        }
        for bad in [
            "--output=/tmp/x",
            "-p",
            "--exec=x",
            "a b",
            "$(x)",
            "x;y",
            "",
        ] {
            assert!(!rev_token_ok(bad), "{bad:?}");
        }
        assert!(hex_ok("abcdef0") && !hex_ok("abc") && !hex_ok("HEAD") && !hex_ok("--x"));
    }
}
