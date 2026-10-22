//! Read side for history, ranges and workspaces (B1 read part for F4): revsets with the user's
//! aliases, the history graph, range analysis, tree diffs, in-range blame and workspace state.
//!
//! Nothing here snapshots a working copy, merges divergent operation heads or writes to the repo.
//! Working copies are compared to `@` by reading their `tree_state` and stat-ing files on disk.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::StreamExt;
use jj_lib::annotate::FileAnnotator;
use jj_lib::backend::{CommitId as JjCommitId, TreeValue};
use jj_lib::commit::Commit;
use jj_lib::config::{ConfigLayer, ConfigSource, StackedConfig};
use jj_lib::conflicts::ConflictMarkerStyle;
use jj_lib::diff::{
    CompareBytesExactly, CompareBytesIgnoreAllWhitespace, ContentDiff, DiffHunkKind,
    find_line_ranges,
};
use jj_lib::gitignore::GitIgnoreFile;
use jj_lib::graph::{GraphEdgeType, TopoGroupedGraph};
use jj_lib::id_prefix::IdPrefixContext;
use jj_lib::local_working_copy::{FileType, TreeState, TreeStateSettings};
use jj_lib::matchers::EverythingMatcher;
use jj_lib::merged_tree::MergedTree;
use jj_lib::object_id::ObjectId;
use jj_lib::op_store::OperationId;
use jj_lib::operation::Operation;
use jj_lib::repo::{ReadonlyRepo, Repo};
use jj_lib::repo_path::{RepoPath, RepoPathBuf};
use jj_lib::revset::{
    self, ResolvedRevsetExpression, RevsetAliasesMap, RevsetDiagnostics, RevsetExpression,
    RevsetExtensions, RevsetParseContext, RevsetParseErrorKind, RevsetResolutionError,
    RevsetWorkspaceContext, SymbolResolver, SymbolResolverExtension, UserRevsetExpression,
};
use jj_lib::settings::UserSettings;
use jj_lib::workspace_store::{SimpleWorkspaceStore, WorkspaceStore};

use crate::{CommitInfo, Error, JjRepo, Result, err, info};

/// The jj CLI's built-in revset aliases (0.45.1), which jj-lib doesn't define. User and repo
/// config layers override them.
const CLI_DEFAULTS: &str = r#"
[revset-aliases]
'trunk()' = '''
latest(
  remote_bookmarks(exact:"main", exact:"origin") |
  remote_bookmarks(exact:"master", exact:"origin") |
  remote_bookmarks(exact:"trunk", exact:"origin") |
  remote_bookmarks(exact:"main", exact:"upstream") |
  remote_bookmarks(exact:"master", exact:"upstream") |
  remote_bookmarks(exact:"trunk", exact:"upstream") |
  root()
)
'''
'builtin_log()' = 'present(@) | ancestors(immutable_heads().., 2) | trunk()'
'builtin_immutable_heads()' = 'trunk() | tags() | untracked_remote_bookmarks() | untracked_remote_tags()'
'immutable_heads()' = 'builtin_immutable_heads()'
'immutable()' = '::(immutable_heads() | root())'
'mutable()' = '~immutable()'
'visible()' = '::visible_heads()'
'hidden()' = '~visible()'

[revsets]
log = "builtin_log()"
"#;

/// The user's revset environment: aliases and the revsets jj's CLI would use.
pub struct RevsetEnv {
    pub aliases: RevsetAliasesMap,
    /// `revsets.log`.
    pub log: String,
    /// `revsets.short-prefixes`, else `revsets.log`.
    pub short_prefixes: String,
    pub user_email: String,
    pub settings: UserSettings,
    extensions: Arc<RevsetExtensions>,
}

/// A revset error as jj reports it, plus a hint (e.g. "did you mean `trunk()`?").
#[derive(Debug, Clone)]
pub struct RevsetError {
    pub message: String,
    pub hint: Option<String>,
}

impl std::fmt::Display for RevsetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)?;
        if let Some(h) = &self.hint {
            write!(f, " ({h})")?;
        }
        Ok(())
    }
}

fn rerr(e: impl std::fmt::Display) -> RevsetError {
    RevsetError {
        message: e.to_string(),
        hint: None,
    }
}

fn did_you_mean(candidates: &[String], call: bool) -> Option<String> {
    let names: Vec<String> = candidates
        .iter()
        .take(3)
        .map(|c| if call { format!("{c}()") } else { c.clone() })
        .collect();
    (!names.is_empty()).then(|| format!("did you mean {}?", names.join(", ")))
}

/// One edge of a history row: the parent it leads to and how many commits it hides.
#[derive(Debug, Clone)]
pub struct GraphEdgeOut {
    pub parent: String,
    /// Commits hidden on an indirect edge (0 for a direct edge).
    pub elided: u32,
}

/// One revision of the history graph, with everything the history panel shows.
#[derive(Debug, Clone)]
pub struct RevRow {
    pub info: CommitInfo,
    pub author: (String, String, i64),
    pub bookmarks: Vec<String>,
    pub tags: Vec<String>,
    pub workspaces: Vec<String>,
    pub conflict: bool,
    pub empty: bool,
    pub immutable: bool,
    pub divergent: bool,
    pub hidden: bool,
    pub working_copy: bool,
    pub shortest_change: usize,
    pub shortest_commit: usize,
    /// Direct and indirect edges; edges to commits outside the revset ("missing") are dropped.
    pub edges: Vec<GraphEdgeOut>,
}

/// A resolved range: members newest first, and what is odd about it.
#[derive(Debug, Clone, Default)]
pub struct RangeInfo {
    pub commits: Vec<CommitInfo>,
    pub roots: Vec<CommitInfo>,
    pub heads: Vec<CommitInfo>,
    /// `(roots(R)::heads(R)) ~ R`: commits left out of a non-convex selection.
    pub gap: Vec<CommitInfo>,
    pub immutable: Vec<CommitInfo>,
    pub conflicted: Vec<CommitInfo>,
}

impl RangeInfo {
    /// The newest head: the commit the range is viewed at.
    pub fn head(&self) -> Option<&CommitInfo> {
        self.heads.first()
    }
}

/// A file changed between two trees. Blob ids are `None` when the side is absent, not a regular
/// file, or conflicted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// One jj workspace as the Workspaces panel shows it.
#[derive(Debug, Clone)]
pub struct WorkspaceState {
    pub name: String,
    pub root: Option<PathBuf>,
    pub wc: CommitInfo,
    /// Files that differ between disk and `@` (modified, added or deleted), not yet snapshotted.
    /// `None` when the working copy can't be read (missing directory, no tree state).
    pub unsnapshotted: Option<u32>,
    /// The paths behind `unsnapshotted` (repo-relative, sorted; the live overlay reads them).
    pub unsnapshotted_paths: Vec<String>,
    /// The files on disk were checked out from another tree than `@`'s: `@` was rewritten
    /// elsewhere (another workspace, an agent) and nobody ran `jj workspace update-stale` there
    /// yet. `unsnapshotted` then counts edits against the tree on disk, not against `@`.
    pub stale: bool,
    /// Other visible commits of `@`'s change (`@` is divergent), newest first.
    pub divergent: Vec<String>,
}

/// The newest operation recorded for a workspace.
#[derive(Debug, Clone)]
pub struct OpSummary {
    pub op: String,
    pub time_ms: i64,
    pub description: String,
}

/// A line diff between two texts, as runs of (kind, old range, new range), 0-based line indexes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineRun {
    pub same: bool,
    pub old: std::ops::Range<usize>,
    pub new: std::ops::Range<usize>,
}

fn config_layers(env_dir: Option<&Path>, repo_path: &Path, ws_root: &Path) -> StackedConfig {
    let mut config = StackedConfig::with_defaults();
    if let Ok(l) = ConfigLayer::parse(ConfigSource::Default, CLI_DEFAULTS) {
        config.add_layer(l);
    }
    let load = |config: &mut StackedConfig, source, path: PathBuf| {
        if path.is_file()
            && let Ok(l) = ConfigLayer::load_from_file(source, path)
        {
            config.add_layer(l);
        }
    };
    // User config: $JJ_CONFIG (a path list) or ~/.config/jj/config.toml + conf.d/*.toml.
    let user_dir = env_dir.map(Path::to_path_buf);
    match std::env::var_os("JJ_CONFIG") {
        Some(list) => {
            for p in std::env::split_paths(&list) {
                if p.is_dir() {
                    let mut files: Vec<PathBuf> = std::fs::read_dir(&p)
                        .into_iter()
                        .flatten()
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
                        .collect();
                    files.sort();
                    for f in files {
                        load(&mut config, ConfigSource::User, f);
                    }
                } else {
                    load(&mut config, ConfigSource::User, p);
                }
            }
        }
        None => {
            if let Some(dir) = &user_dir {
                load(&mut config, ConfigSource::User, dir.join("config.toml"));
                let mut files: Vec<PathBuf> = std::fs::read_dir(dir.join("conf.d"))
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|e| e == "toml"))
                    .collect();
                files.sort();
                for f in files {
                    load(&mut config, ConfigSource::User, f);
                }
            }
        }
    }
    // Repo config: legacy in-repo file, then the secure per-repo file keyed by `config-id`.
    load(
        &mut config,
        ConfigSource::Repo,
        repo_path.join("config.toml"),
    );
    if let (Some(dir), Ok(id)) = (
        &user_dir,
        std::fs::read_to_string(repo_path.join("config-id")),
    ) {
        load(
            &mut config,
            ConfigSource::Repo,
            dir.join("repos").join(id.trim()).join("config.toml"),
        );
    }
    let ws_jj = ws_root.join(".jj");
    load(
        &mut config,
        ConfigSource::Workspace,
        ws_jj.join("workspace-config.toml"),
    );
    if let (Some(dir), Ok(id)) = (&user_dir, std::fs::read_to_string(ws_jj.join("config-id"))) {
        load(
            &mut config,
            ConfigSource::Workspace,
            dir.join("workspaces").join(id.trim()).join("config.toml"),
        );
    }
    config
}

/// `ui.conflict-marker-style` from the CLI defaults, user, repo and workspace config ("diff" when
/// unset or invalid, as jj).
pub(crate) fn marker_style(repo_path: &Path, ws_root: &Path) -> ConflictMarkerStyle {
    config_layers(user_config_dir().as_deref(), repo_path, ws_root)
        .get::<ConflictMarkerStyle>("ui.conflict-marker-style")
        .unwrap_or(ConflictMarkerStyle::Diff)
}

fn user_config_dir() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(x).join("jj"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config").join("jj"))
}

async fn collect_ids(
    set: &dyn revset::Revset,
    cap: usize,
) -> std::result::Result<Vec<JjCommitId>, RevsetError> {
    let mut out = Vec::new();
    let mut s = set.stream();
    while let Some(id) = s.next().await {
        out.push(id.map_err(rerr)?);
        if out.len() >= cap {
            break;
        }
    }
    Ok(out)
}

impl JjRepo {
    /// The same workspace reloaded at the current operation head. When several heads exist (a
    /// concurrent op not yet merged), loads the newest instead of merging them: merging writes.
    pub fn reload(&self) -> Result<JjRepo> {
        let loader = self.repo.loader();
        let repo = pollster::block_on(async {
            let heads = loader.op_heads_store().get_op_heads().await.map_err(err)?;
            let mut best: Option<Operation> = None;
            for id in &heads {
                let op = loader.load_operation(id).await.map_err(err)?;
                if best.as_ref().is_none_or(|b| {
                    op.metadata().time.end.timestamp > b.metadata().time.end.timestamp
                }) {
                    best = Some(op);
                }
            }
            let op = best.ok_or_else(|| Error("no operation heads".into()))?;
            if op.id() == self.repo.op_id() {
                return Ok::<_, Error>(self.repo.clone());
            }
            loader.load_at(&op).await.map_err(err)
        })?;
        Ok(JjRepo {
            repo,
            workspace_name: self.workspace_name.clone(),
            workspace_root: self.workspace_root.clone(),
            marker_style: self.marker_style,
        })
    }

    /// `<main workspace>/.jj/repo` (secondary workspaces point at it from a file).
    pub fn repo_path(&self) -> PathBuf {
        let p = self.workspace_root.join(".jj").join("repo");
        if p.is_file()
            && let Ok(target) = std::fs::read_to_string(&p)
        {
            let t = PathBuf::from(target.trim());
            let t = if t.is_relative() {
                self.workspace_root.join(".jj").join(t)
            } else {
                t
            };
            return t.canonicalize().unwrap_or(t);
        }
        p
    }

    pub fn readonly(&self) -> &Arc<ReadonlyRepo> {
        &self.repo
    }

    /// The user's revset aliases and revsets (CLI defaults < user < repo < workspace config).
    pub fn revset_env(&self) -> RevsetEnv {
        // Scoped (`--scope`) tables are not resolved: jj-lib keeps its resolver private.
        let config = config_layers(
            user_config_dir().as_deref(),
            &self.repo_path(),
            &self.workspace_root,
        );
        let mut aliases = RevsetAliasesMap::new();
        let decls: Vec<String> = config
            .table_keys("revset-aliases")
            .map(str::to_string)
            .collect();
        for decl in decls {
            if let Ok(defn) = config.get::<String>(["revset-aliases", decl.as_str()]) {
                let _ = aliases.insert(&decl, defn, None);
            }
        }
        let log = config
            .get::<String>(["revsets", "log"])
            .unwrap_or_else(|_| "builtin_log()".into());
        let short_prefixes = config
            .get::<String>(["revsets", "short-prefixes"])
            .unwrap_or_else(|_| log.clone());
        let user_email = config.get::<String>(["user", "email"]).unwrap_or_default();
        let settings = UserSettings::from_config(config)
            .or_else(|_| UserSettings::from_config(StackedConfig::with_defaults()))
            .expect("default settings");
        RevsetEnv {
            aliases,
            log,
            short_prefixes,
            user_email,
            settings,
            extensions: Arc::new(RevsetExtensions::default()),
        }
    }

    /// Parses a revset with the user's aliases; errors carry jj's message and a hint.
    pub fn parse_revset(
        &self,
        env: &RevsetEnv,
        revset_str: &str,
    ) -> std::result::Result<Arc<UserRevsetExpression>, RevsetError> {
        if revset_str.trim().is_empty() {
            return Err(RevsetError {
                message: "The revset is empty".into(),
                hint: Some("try trunk()..@".into()),
            });
        }
        let fileset_aliases = jj_lib::fileset::FilesetAliasesMap::new();
        let path_converter = jj_lib::ui_path::RepoPathUiConverter::Fs {
            cwd: self.workspace_root.clone(),
            base: self.workspace_root.clone(),
        };
        let context = RevsetParseContext {
            aliases_map: &env.aliases,
            local_variables: Default::default(),
            user_email: &env.user_email,
            date_pattern_context: crate::chrono_now(),
            default_ignored_remote: None,
            fileset_aliases_map: &fileset_aliases,
            extensions: &env.extensions,
            workspace: Some(RevsetWorkspaceContext {
                path_converter: &path_converter,
                workspace_name: &self.workspace_name,
            }),
        };
        let mut diagnostics = RevsetDiagnostics::new();
        revset::parse(&mut diagnostics, revset_str, &context).map_err(|e| {
            // Report the innermost error (inside an alias) but keep jj's wording.
            let mut inner = &e;
            while let Some(o) = inner.origin() {
                inner = o;
            }
            match inner.kind() {
                RevsetParseErrorKind::NoSuchFunction { name, candidates } => RevsetError {
                    message: format!("Function `{name}` doesn't exist"),
                    hint: did_you_mean(candidates, true),
                },
                RevsetParseErrorKind::SyntaxError => RevsetError {
                    message: format!(
                        "Syntax error: {}",
                        e.to_string()
                            .lines()
                            .last()
                            .unwrap_or("")
                            .trim_start_matches([' ', '='])
                    ),
                    hint: Some("see `jj help -k revsets`".into()),
                },
                kind => RevsetError {
                    message: kind.to_string(),
                    hint: None,
                },
            }
        })
    }

    fn id_prefix_context(&self, env: &RevsetEnv) -> IdPrefixContext {
        let ctx = IdPrefixContext::new(env.extensions.clone());
        match self.parse_revset(env, &env.short_prefixes) {
            Ok(expr) => ctx.disambiguate_within(expr),
            Err(_) => ctx,
        }
    }

    /// Resolves symbols in a parsed revset (bookmarks, change/commit prefixes, `name@`).
    pub fn resolve_expr(
        &self,
        env: &RevsetEnv,
        expr: &Arc<UserRevsetExpression>,
    ) -> std::result::Result<Arc<ResolvedRevsetExpression>, RevsetError> {
        let repo = self.repo.as_ref();
        let no_ext: [Box<dyn SymbolResolverExtension>; 0] = [];
        let id_ctx = self.id_prefix_context(env);
        let resolver = SymbolResolver::new(repo, &no_ext).with_id_prefix_context(&id_ctx);
        expr.resolve_user_expression(repo, &resolver)
            .map_err(|e| match &e {
                RevsetResolutionError::NoSuchRevision { name, candidates } => RevsetError {
                    message: format!("Revision `{name}` doesn't exist"),
                    hint: did_you_mean(candidates, false),
                },
                _ => rerr(e),
            })
    }

    /// Evaluates a resolved expression to commit ids, newest first (at most `cap`).
    pub fn eval_ids(
        &self,
        expr: &Arc<ResolvedRevsetExpression>,
        cap: usize,
    ) -> std::result::Result<Vec<JjCommitId>, RevsetError> {
        let set = expr.clone().evaluate(self.repo.as_ref()).map_err(rerr)?;
        pollster::block_on(collect_ids(set.as_ref(), cap))
    }

    fn infos(&self, ids: &[JjCommitId]) -> Result<Vec<CommitInfo>> {
        ids.iter().map(|id| self.commit_info(id)).collect()
    }

    /// Parses, resolves and evaluates `revset` (newest first).
    pub fn eval(
        &self,
        env: &RevsetEnv,
        revset_str: &str,
        cap: usize,
    ) -> std::result::Result<Vec<CommitInfo>, RevsetError> {
        let e = self.parse_revset(env, revset_str)?;
        let r = self.resolve_expr(env, &e)?;
        let ids = self.eval_ids(&r, cap)?;
        self.infos(&ids).map_err(rerr)
    }

    /// The history graph of `revset` in jj's grouped topological order (the serving workspace's
    /// branch first), at most `limit` rows. Returns the rows and whether more existed.
    pub fn graph(
        &self,
        env: &RevsetEnv,
        revset_str: &str,
        limit: usize,
    ) -> std::result::Result<(Vec<RevRow>, bool), RevsetError> {
        let repo = self.repo.as_ref();
        let user = self.parse_revset(env, revset_str)?;
        let resolved = self.resolve_expr(env, &user)?;
        let set = resolved.evaluate(repo).map_err(rerr)?;
        let wc = repo.view().get_wc_commit_id(&self.workspace_name).cloned();
        let nodes = pollster::block_on(async {
            let contains = set.containing_fn();
            let wc_in = match &wc {
                Some(id) => contains(id).await.unwrap_or(false),
                None => false,
            };
            let mut topo = TopoGroupedGraph::new(set.stream_graph(), |id: &JjCommitId| id);
            if wc_in && let Some(id) = &wc {
                topo.prioritize_branch(id.clone());
            }
            let mut out = Vec::new();
            let mut s = Box::pin(topo.stream());
            while let Some(n) = s.next().await {
                out.push(n.map_err(rerr)?);
                if out.len() > limit {
                    break;
                }
            }
            Ok::<_, RevsetError>(out)
        })?;
        let truncated = nodes.len() > limit;
        let nodes = &nodes[..nodes.len().min(limit)];
        let ids: Vec<JjCommitId> = nodes.iter().map(|(id, _)| id.clone()).collect();
        let shown = RevsetExpression::commits(ids.clone());
        let in_set = |e: Arc<ResolvedRevsetExpression>| -> HashSet<JjCommitId> {
            self.eval_ids(&e, usize::MAX)
                .unwrap_or_default()
                .into_iter()
                .collect()
        };
        let immutable: HashSet<JjCommitId> = self
            .parse_revset(env, "immutable()")
            .and_then(|e| self.resolve_expr(env, &e))
            .map(|imm| in_set(shown.intersection(&imm)))
            .unwrap_or_default();
        let divergent = in_set(shown.intersection(&RevsetExpression::divergent()));
        let id_ctx = self.id_prefix_context(env);
        let id_index = id_ctx.populate(repo).ok();

        // Refs per commit.
        let mut bookmarks: HashMap<JjCommitId, Vec<String>> = HashMap::new();
        let view = repo.view();
        let mut local_targets: HashMap<String, Vec<JjCommitId>> = HashMap::new();
        for (name, target) in view.local_bookmarks() {
            for id in target.added_ids() {
                bookmarks
                    .entry(id.clone())
                    .or_default()
                    .push(name.as_str().to_string());
            }
            local_targets.insert(
                name.as_str().to_string(),
                target.added_ids().cloned().collect(),
            );
        }
        for (sym, remote) in view.all_remote_bookmarks() {
            if sym.remote.as_str() == "git" {
                continue;
            }
            let ids: Vec<JjCommitId> = remote.target.added_ids().cloned().collect();
            if local_targets.get(sym.name.as_str()) == Some(&ids) {
                continue; // in sync with the local bookmark: jj shows only the local name
            }
            for id in ids {
                bookmarks.entry(id).or_default().push(format!(
                    "{}@{}",
                    sym.name.as_str(),
                    sym.remote.as_str()
                ));
            }
        }
        let mut tags: HashMap<JjCommitId, Vec<String>> = HashMap::new();
        for (name, target) in view.local_tags() {
            for id in target.added_ids() {
                tags.entry(id.clone())
                    .or_default()
                    .push(name.as_str().to_string());
            }
        }
        let mut rows = Vec::with_capacity(nodes.len());
        for (id, edges) in nodes {
            let c = self.get(id).map_err(rerr)?;
            let (empty, hidden) = pollster::block_on(async {
                (
                    c.is_empty(repo).await.unwrap_or(false),
                    c.is_hidden(repo).await.unwrap_or(false),
                )
            });
            let mut out_edges = Vec::new();
            for e in edges {
                match e.edge_type {
                    GraphEdgeType::Missing => {}
                    GraphEdgeType::Direct => out_edges.push(GraphEdgeOut {
                        parent: e.target.hex(),
                        elided: 0,
                    }),
                    GraphEdgeType::Indirect => {
                        // target..node, minus node and the rows shown.
                        let hidden_on_edge = RevsetExpression::commit(e.target.clone())
                            .range(&RevsetExpression::commit(id.clone()))
                            .minus(&RevsetExpression::commit(id.clone()))
                            .minus(&shown);
                        let n = self
                            .eval_ids(&hidden_on_edge, 100_000)
                            .map(|v| v.len())
                            .unwrap_or(0);
                        out_edges.push(GraphEdgeOut {
                            parent: e.target.hex(),
                            elided: n as u32,
                        });
                    }
                }
            }
            let (shortest_change, shortest_commit) = match &id_index {
                Some(ix) => (
                    pollster::block_on(ix.shortest_change_prefix_len(repo, c.change_id()))
                        .unwrap_or(12),
                    ix.shortest_commit_prefix_len(repo, id).unwrap_or(12),
                ),
                None => (12, 12),
            };
            let a = c.author();
            rows.push(RevRow {
                info: info(&c),
                author: (
                    a.name.clone(),
                    a.email.clone(),
                    a.timestamp.timestamp.0 / 1000,
                ),
                bookmarks: bookmarks.remove(id).unwrap_or_default(),
                tags: tags.remove(id).unwrap_or_default(),
                workspaces: view
                    .workspaces_for_wc_commit_id(id)
                    .iter()
                    .map(|w| w.as_str().to_string())
                    .collect(),
                conflict: c.has_conflict(),
                empty,
                immutable: immutable.contains(id),
                divergent: divergent.contains(id),
                hidden,
                working_copy: wc.as_ref() == Some(id),
                shortest_change,
                shortest_commit,
                edges: out_edges,
            });
        }
        Ok((rows, truncated))
    }

    /// Resolves a range revset and checks its shape: roots, heads, gap, immutable and conflicted
    /// members. Members are capped at `cap` (newest first).
    pub fn analyze_range(
        &self,
        env: &RevsetEnv,
        revset_str: &str,
        cap: usize,
    ) -> std::result::Result<RangeInfo, RevsetError> {
        let user = self.parse_revset(env, revset_str)?;
        let r = self.resolve_expr(env, &user)?;
        let ids = self.eval_ids(&r, cap)?;
        let roots = self.eval_ids(&r.roots(), 1000)?;
        let heads = self.eval_ids(&r.heads(), 1000)?;
        let gap = self.eval_ids(&r.roots().dag_range_to(&r.heads()).minus(&r), 1000)?;
        let immutable = self
            .parse_revset(env, "immutable()")
            .and_then(|e| self.resolve_expr(env, &e))
            .and_then(|imm| self.eval_ids(&r.intersection(&imm), 1000))
            .unwrap_or_default();
        let commits = self.infos(&ids).map_err(rerr)?;
        let mut conflicted = Vec::new();
        for id in &ids {
            if self.get(id).map_err(rerr)?.has_conflict() {
                conflicted.push(self.commit_info(id).map_err(rerr)?);
            }
        }
        Ok(RangeInfo {
            commits,
            roots: self.infos(&roots).map_err(rerr)?,
            heads: self.infos(&heads).map_err(rerr)?,
            gap: self.infos(&gap).map_err(rerr)?,
            immutable: self.infos(&immutable).map_err(rerr)?,
            conflicted,
        })
    }

    /// Whether a commit's tree has conflicts.
    pub fn is_conflicted(&self, commit: &str) -> bool {
        self.commit_by_hex(commit).is_ok_and(|c| c.has_conflict())
    }

    fn commit_by_hex(&self, hex: &str) -> Result<Commit> {
        self.get(&Self::parse_commit_id(hex)?)
    }

    /// Files changed from the merged parents of `root` to `head`: the range's cumulative diff.
    pub fn range_diff(&self, root: &str, head: &str) -> Result<Vec<FileChange>> {
        let repo = self.repo.as_ref();
        let r = self.commit_by_hex(root)?;
        let h = self.commit_by_hex(head)?;
        let before = pollster::block_on(r.parent_tree(repo)).map_err(err)?;
        let (base, tip) = (format!("{}^", r.id().hex()), h.id().hex());
        diff_named_trees((&before, Some(&base)), (&h.tree(), Some(&tip)))
    }

    /// Files that differ between the trees of two commits (hidden ones too, e.g. a change's
    /// earlier commit for an interdiff).
    pub fn tree_diff(&self, from: &str, to: &str) -> Result<Vec<FileChange>> {
        let f = self.commit_by_hex(from)?;
        let t = self.commit_by_hex(to)?;
        let (fh, th) = (f.id().hex(), t.id().hex());
        diff_named_trees((&f.tree(), Some(&fh)), (&t.tree(), Some(&th)))
    }

    /// `jj interdiff --from <from> --to <to>`: `from` rebased onto `to`'s parents, diffed against
    /// `to`, so a rebase between the two shows nothing but the change's own edits. Hidden commits
    /// work (e.g. the commit a review mark recorded).
    pub fn interdiff(&self, from: &str, to: &str) -> Result<Vec<FileChange>> {
        let repo = self.repo.as_ref();
        let f = self.commit_by_hex(from)?;
        let t = self.commit_by_hex(to)?;
        let rebased = pollster::block_on(async {
            let from_base = f.parent_tree(repo).await?;
            let to_base = t.parent_tree(repo).await?;
            MergedTree::merge(jj_lib::merge::Merge::from_vec(vec![
                (to_base, "to parents".to_string()),
                (from_base, "from parents".to_string()),
                (f.tree(), "from".to_string()),
            ]))
            .await
        })
        .map_err(err)?;
        // The rebased tree has no commit: its conflicts read as their first side.
        diff_named_trees((&rebased, None), (&t.tree(), Some(&t.id().hex())))
    }

    /// One commit by full hex id, visible or not.
    pub fn commit_by_id(&self, hex: &str) -> Result<CommitInfo> {
        Ok(info(&self.commit_by_hex(hex)?))
    }

    /// Files one commit changes against its merged parents.
    pub fn commit_diff(&self, commit: &str) -> Result<Vec<FileChange>> {
        self.range_diff(commit, commit)
    }

    /// The tree id of the merged parents of `commit` (the base of a range rooted there), as the
    /// blob of `path` in it.
    pub fn base_blob(&self, root: &str, path: &str) -> Result<Option<String>> {
        let repo = self.repo.as_ref();
        let r = self.commit_by_hex(root)?;
        let tree = pollster::block_on(r.parent_tree(repo)).map_err(err)?;
        blob_in(&tree, path)
    }

    /// The blob of `path` at `commit`, if it is a regular file there.
    pub fn blob_at(&self, commit: &str, path: &str) -> Result<Option<String>> {
        blob_in(&self.commit_by_hex(commit)?.tree(), path)
    }

    /// For each line of `path` at `head`: the change id of the range commit that last touched it,
    /// or `None` when the line comes from before the range.
    pub fn annotate_in_range(
        &self,
        head: &str,
        path: &str,
        range: &[String],
    ) -> Result<Vec<Option<String>>> {
        let repo = self.repo.as_ref();
        let h = self.commit_by_hex(head)?;
        let rp = RepoPath::from_internal_string(path).map_err(err)?;
        let ids: Vec<JjCommitId> = range
            .iter()
            .map(|c| Self::parse_commit_id(c))
            .collect::<Result<_>>()?;
        let members: HashSet<&JjCommitId> = ids.iter().collect();
        let domain = RevsetExpression::commits(ids.clone());
        let mut changes: HashMap<JjCommitId, String> = HashMap::new();
        for id in &ids {
            changes.insert(id.clone(), self.get(id)?.change_id().reverse_hex());
        }
        let ann = pollster::block_on(async {
            let mut a = FileAnnotator::from_commit(&h, rp).await.map_err(err)?;
            a.compute(repo, &domain).await.map_err(err)?;
            Ok::<_, Error>(a.to_annotation())
        })?;
        Ok(ann
            .lines()
            .map(|(origin, _)| match origin {
                Ok(id) if members.contains(id) => changes.get(id).cloned(),
                _ => None,
            })
            .collect())
    }

    /// Every workspace: root, `@`, and how many files differ on disk from `@` (read-only).
    pub fn workspaces(&self, env: &RevsetEnv) -> Vec<WorkspaceState> {
        let repo = self.repo.as_ref();
        let repo_path = &self.repo_path();
        let store_dir = repo_path.join("workspace_store");
        // SimpleWorkspaceStore::load creates the directory when missing: only load an existing one.
        let ws_store = store_dir
            .is_dir()
            .then(|| SimpleWorkspaceStore::load(repo_path).ok())
            .flatten();
        let settings = TreeStateSettings::try_from_user_settings(&env.settings).ok();
        let mut out = Vec::new();
        for (name, id) in repo.view().wc_commit_ids() {
            let Ok(c) = self.get(id) else { continue };
            let root = if name.as_str() == self.workspace_name.as_str() {
                Some(self.workspace_root.clone())
            } else {
                ws_store
                    .as_ref()
                    .and_then(|s| s.get_workspace_path(name).ok().flatten())
                    .map(|p| {
                        let p = if p.is_relative() {
                            repo_path.join(p)
                        } else {
                            p
                        };
                        p.canonicalize().unwrap_or(p)
                    })
            };
            let divergent: Vec<String> = pollster::block_on(repo.resolve_change_id(c.change_id()))
                .ok()
                .flatten()
                .map(|t| {
                    t.visible_with_offsets()
                        .map(|(_, x)| x.hex())
                        .filter(|x| *x != id.hex())
                        .collect()
                })
                .unwrap_or_default();
            out.push((
                WorkspaceState {
                    name: name.as_str().to_string(),
                    root,
                    wc: info(&c),
                    unsnapshotted: None,
                    unsnapshotted_paths: Vec::new(),
                    stale: false,
                    divergent,
                },
                c.tree(),
            ));
        }
        // Each working copy is stat-ed on its own thread.
        std::thread::scope(|scope| {
            for (w, tree) in out.iter_mut() {
                if let (Some(r), Some(s)) = (w.root.clone(), settings.as_ref()) {
                    scope.spawn(move || {
                        if let Ok((mut paths, stale)) = unsnapshotted_files(repo, &r, tree, s) {
                            paths.sort();
                            w.unsnapshotted = Some(paths.len() as u32);
                            w.unsnapshotted_paths = paths;
                            w.stale = stale;
                        }
                    });
                }
            }
        });
        out.into_iter().map(|(w, _)| w).collect()
    }

    /// The newest operation run in each current workspace, walking back at most `max_ops`
    /// operations (a workspace nobody ran jj in has none).
    pub fn last_ops(&self, max_ops: usize) -> HashMap<String, OpSummary> {
        let mut out: HashMap<String, OpSummary> = HashMap::new();
        let wanted: HashSet<String> = self
            .repo
            .view()
            .wc_commit_ids()
            .keys()
            .map(|w| w.as_str().to_string())
            .collect();
        let mut seen: HashSet<OperationId> = HashSet::new();
        let mut queue = std::collections::VecDeque::from([self.repo.operation().clone()]);
        let mut visited = 0;
        while let Some(op) = queue.pop_front() {
            if !seen.insert(op.id().clone()) {
                continue;
            }
            visited += 1;
            let m = op.metadata();
            if let Some(ws) = &m.workspace_name
                && wanted.contains(ws.as_str())
            {
                out.entry(ws.as_str().to_string())
                    .or_insert_with(|| OpSummary {
                        op: op.id().hex(),
                        time_ms: m.time.end.timestamp.0,
                        description: m.description.clone(),
                    });
            }
            if out.len() >= wanted.len() || visited >= max_ops {
                break;
            }
            if let Ok(parents) = pollster::block_on(op.parents()) {
                queue.extend(parents);
            }
        }
        out
    }
}

fn blob_in(tree: &MergedTree, path: &str) -> Result<Option<String>> {
    let rp = RepoPath::from_internal_string(path).map_err(err)?;
    let v = pollster::block_on(tree.path_value(rp)).map_err(err)?;
    Ok(match v.as_resolved() {
        Some(Some(TreeValue::File { id, .. })) => Some(id.hex()),
        _ => None,
    })
}

pub(crate) fn file_id(v: &jj_lib::backend::MergedTreeValue) -> Option<String> {
    named_file_id(v, None, "")
}

/// The blob id of one side of a diff. A conflicted path reads as jj's materialized markers when
/// the tree is named (`tree`: a commit id, or `<commit>^` for its merged parents, see
/// [`crate::conflict_blob_id`]), as in snapshot views; else its first side that is a file.
fn named_file_id(
    v: &jj_lib::backend::MergedTreeValue,
    tree: Option<&str>,
    path: &str,
) -> Option<String> {
    match v.as_resolved() {
        Some(Some(TreeValue::File { id, .. })) => Some(id.hex()),
        Some(_) => None,
        None => {
            let first = v.adds().find_map(|t| match t {
                Some(TreeValue::File { id, .. }) => Some(id.hex()),
                _ => None,
            })?;
            Some(match tree {
                Some(t) => crate::conflict_blob_id(t, path),
                None => first,
            })
        }
    }
}

/// Regular-file changes between two trees, in path order. Conflicted sides read as their first
/// file side (use [`diff_named_trees`] to get jj's conflict markers).
pub fn diff_trees(before: &MergedTree, after: &MergedTree) -> Result<Vec<FileChange>> {
    diff_named_trees((before, None), (after, None))
}

/// [`diff_trees`] where each tree may be named (a commit id, or `<commit>^` for its merged
/// parents), so conflicted paths get synthetic blob ids that read back with jj's markers.
pub fn diff_named_trees(
    before: (&MergedTree, Option<&str>),
    after: (&MergedTree, Option<&str>),
) -> Result<Vec<FileChange>> {
    pollster::block_on(async {
        let mut out = Vec::new();
        let mut stream = before.0.diff_stream(after.0, &EverythingMatcher);
        while let Some(entry) = stream.next().await {
            let diff = entry.values.map_err(err)?;
            let path = entry.path.as_internal_file_string();
            let (b, a) = (
                named_file_id(&diff.before, before.1, path),
                named_file_id(&diff.after, after.1, path),
            );
            if (b.is_some() || a.is_some()) && b != a {
                out.push(FileChange {
                    path: path.to_string(),
                    before: b,
                    after: a,
                });
            }
        }
        Ok(out)
    })
}

/// Line runs between two texts (jj's line diff), optionally ignoring all whitespace.
pub fn line_runs(old: &[u8], new: &[u8], ignore_whitespace: bool) -> Vec<LineRun> {
    let diff = if ignore_whitespace {
        ContentDiff::for_tokenizer(
            [old, new],
            find_line_ranges,
            CompareBytesIgnoreAllWhitespace,
        )
    } else {
        ContentDiff::for_tokenizer([old, new], find_line_ranges, CompareBytesExactly)
    };
    let count = |b: &[u8]| {
        b.iter().filter(|&&c| c == b'\n').count()
            + usize::from(!b.is_empty() && !b.ends_with(b"\n"))
    };
    let (mut o, mut n) = (0usize, 0usize);
    let mut out: Vec<LineRun> = Vec::new();
    for h in diff.hunks() {
        let (co, cn) = (count(h.contents[0]), count(h.contents[1]));
        let run = LineRun {
            same: h.kind == DiffHunkKind::Matching,
            old: o..o + co,
            new: n..n + cn,
        };
        o += co;
        n += cn;
        if run.old.is_empty() && run.new.is_empty() {
            continue;
        }
        // Whitespace-insensitive matching hunks still count as same.
        match out.last_mut() {
            Some(last) if last.same == run.same => {
                last.old.end = run.old.end;
                last.new.end = run.new.end;
            }
            _ => out.push(run),
        }
    }
    out
}

/// (added, removed) line counts between two texts.
pub fn line_stat(old: &[u8], new: &[u8]) -> (u32, u32) {
    line_runs(old, new, false)
        .iter()
        .filter(|r| !r.same)
        .fold((0, 0), |(a, d), r| {
            (a + r.new.len() as u32, d + r.old.len() as u32)
        })
}

/// Files that differ between the working copy on disk and the tree it was checked out from
/// (normally the workspace's `@`, `tree`): tracked files whose stat changed and whose content
/// differs, deleted tracked files, and new files that aren't ignored; and whether the workspace is
/// stale (that tree is not `@`'s, as `jj` would report "the working copy is stale"). Reads only:
/// the tree state is loaded, never saved.
fn unsnapshotted_files(
    repo: &ReadonlyRepo,
    root: &Path,
    tree: &MergedTree,
    settings: &TreeStateSettings,
) -> Result<(Vec<String>, bool)> {
    let state_path = state_path_for(root);
    if !state_path.join("tree_state").is_file() {
        // TreeState::load would initialize (write) a missing state.
        return Err(Error("no tree state".into()));
    }
    let ts = TreeState::load(
        repo.store().clone(),
        root.to_path_buf(),
        state_path,
        settings,
    )
    .map_err(err)?;
    let empty = repo.store().empty_merged_tree_id();
    if *ts.current_tree().tree_ids() == empty
        && ts.file_states().is_empty()
        && *tree.tree_ids() != empty
    {
        // `jj workspace add` records the new workspace's `@` before it checks the files out:
        // until the checkout ends, the tree state is the empty tree. That is neither a stale
        // workspace nor a disk full of edits (N8), so report "unknown" until it settles.
        return Err(Error("checkout in progress".into()));
    }
    let tree_stale = ts.current_tree().tree_ids_and_labels().0 != tree.tree_ids_and_labels().0;
    // jj also calls a workspace stale when its `@` was rewritten since the working copy was last
    // updated, even with the same tree (e.g. `jj describe` from another workspace).
    let stale = tree_stale || stale_by_operation(repo, root) == Some(true);
    // Content comparisons go against what is checked out (a stale workspace's old `@`).
    let tree = if tree_stale { ts.current_tree() } else { tree };
    // Like jj: a file whose recorded mtime isn't older than the tree state itself may have been
    // modified within the same clock tick, so its content is checked.
    let own_mtime = mtime_ms(&state_path_for(root).join("tree_state"));
    let states = ts.file_states();
    let mut dirty: Vec<String> = Vec::new();
    let mut tracked: HashSet<RepoPathBuf> = HashSet::new();
    for (path, state) in states.iter() {
        tracked.insert(path.to_owned());
        let disk = path.to_fs_path_unchecked(root);
        let Ok(meta) = disk.symlink_metadata() else {
            dirty.push(path.as_internal_file_string().to_string()); // deleted
            continue;
        };
        let mtime = meta_mtime_ms(&meta);
        let is_file = meta.file_type().is_file();
        let clean = matches!(state.file_type, FileType::Normal { .. })
            && is_file
            && state.size == meta.len()
            && state.mtime.0 == mtime
            && state.mtime.0 < own_mtime;
        if clean {
            continue;
        }
        // Stat changed: compare content with @ before counting it.
        if is_file
            && let Ok(Some(blob)) = blob_in(tree, path.as_internal_file_string())
            && let Ok(bytes) = std::fs::read(&disk)
            && let Ok(stored) = read_blob(repo, path, &blob)
            && bytes == stored
        {
            continue;
        }
        if matches!(state.file_type, FileType::Normal { .. }) || is_file {
            dirty.push(path.as_internal_file_string().to_string());
        }
    }
    // New files: walk the tree, honouring .gitignore files and skipping .jj/.git.
    let mut stack: Vec<(PathBuf, RepoPathBuf, Arc<GitIgnoreFile>)> = vec![(
        root.to_path_buf(),
        RepoPathBuf::root(),
        GitIgnoreFile::empty()
            .chain_with_file(
                RepoPath::root(),
                root.join(".git").join("info").join("exclude"),
            )
            .unwrap_or_else(|_| GitIgnoreFile::empty()),
    )];
    let mut budget = 200_000usize;
    while let Some((dir, rdir, ignore)) = stack.pop() {
        let ignore = ignore
            .chain_with_file(&rdir, dir.join(".gitignore"))
            .unwrap_or(ignore);
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            budget = budget.saturating_sub(1);
            if budget == 0 {
                return Ok((dirty, stale));
            }
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            if rdir.is_root() && (name == ".jj" || name == ".git") {
                continue;
            }
            let Ok(component) = jj_lib::repo_path::RepoPathComponentBuf::new(name) else {
                continue;
            };
            let rp = rdir.join(&component);
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if !ignore.matches_dir(&rp) {
                    stack.push((e.path(), rp, ignore.clone()));
                }
            } else if !tracked.contains(&rp) && !ignore.matches_file(&rp) {
                dirty.push(rp.as_internal_file_string().to_string());
            }
        }
    }
    Ok((dirty, stale))
}

/// jj's staleness rule: the operation the working copy was last updated at (`checkout`) shows a
/// different `@` commit for this workspace than `repo` does. `None` when the checkout record or
/// that operation can't be read (then only the tree comparison decides).
fn stale_by_operation(repo: &ReadonlyRepo, root: &Path) -> Option<bool> {
    use prost::Message;
    let buf = std::fs::read(state_path_for(root).join("checkout")).ok()?;
    let proto = jj_lib::protos::local_working_copy::Checkout::decode(&*buf).ok()?;
    let op_id = OperationId::new(proto.operation_id);
    if &op_id == repo.op_id() {
        return Some(false);
    }
    let name = if proto.workspace_name.is_empty() {
        jj_lib::ref_name::WorkspaceName::DEFAULT.to_owned()
    } else {
        jj_lib::ref_name::WorkspaceNameBuf::from(proto.workspace_name)
    };
    let then = pollster::block_on(async {
        let op = repo.loader().load_operation(&op_id).await.ok()?;
        op.view().await.ok()
    })?;
    let now = repo.view().get_wc_commit_id(&name)?;
    Some(then.get_wc_commit_id(&name) != Some(now))
}

fn state_path_for(root: &Path) -> PathBuf {
    root.join(".jj").join("working_copy")
}

fn meta_mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn mtime_ms(path: &Path) -> i64 {
    path.symlink_metadata()
        .map(|m| meta_mtime_ms(&m))
        .unwrap_or(0)
}

fn read_blob(repo: &ReadonlyRepo, path: &RepoPath, blob: &str) -> Result<Vec<u8>> {
    use futures::AsyncReadExt;
    let id = jj_lib::backend::FileId::try_from_hex(blob)
        .ok_or_else(|| Error(format!("bad blob id {blob}")))?;
    pollster::block_on(async {
        let mut reader = repo.store().read_file(path, &id).await.map_err(err)?;
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.map_err(err)?;
        Ok(buf)
    })
}

/// Quotes a workspace name for a revset (`"gym-fix"@`).
pub fn workspace_wc_revset(name: &str) -> String {
    format!("{}@", quote(name))
}

fn quote(s: &str) -> String {
    let simple = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    if simple {
        s.to_string()
    } else {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// Paths touched by any of `commits` (union of per-commit diffs), with per-commit file lists.
pub fn files_of(
    repo: &JjRepo,
    commits: &[CommitInfo],
) -> (BTreeSet<String>, BTreeMap<String, Vec<String>>) {
    let mut all = BTreeSet::new();
    let mut per = BTreeMap::new();
    for c in commits {
        let files: Vec<String> = repo
            .commit_diff(&c.commit)
            .map(|v| v.into_iter().map(|f| f.path).collect())
            .unwrap_or_default();
        all.extend(files.iter().cloned());
        per.insert(c.commit.clone(), files);
    }
    (all, per)
}
