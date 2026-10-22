//! B1 `cv-vcs`, write part: exact previews and mutations through jj-lib 0.45.1 transactions.
//!
//! - [`JjWriter::preview`] runs a [`MutationPlan`] inside a transaction and drops it: nothing is
//!   written to the op log (DESIGN §6 "Preview, exactly").
//! - [`JjWriter::mutate`] runs the plan once as a dry run to find the *affected workspaces* (those
//!   whose `@` the plan rewrites), snapshots them, then runs the plan for real, commits the
//!   transaction and checks the new `@`s out on disk. One mutation at a time (the queue).
//!
//! Every read here is at the current head operation (reloaded when `op_heads` changes on disk).
//! The CLI recipes in DESIGN appendix A are the test oracle (`tests/write.rs`) and a fallback
//! (D4, N11): when the jj-lib path of [`JjWriter::mutate`] fails with an internal or unsupported
//! error before it commits anything, the same plan is replayed through the `jj` CLI ([`cli`]):
//! argv only (no shell) and a server-owned `--config-file` (DESIGN §10), inside the same mutation
//! queue. Plans are validated first (bad ids, immutable targets, unconflicted paths, root/merge
//! reverts), so user errors are reported as such and never retried.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use jj_lib::absorb::{AbsorbSource, absorb_hunks, split_hunks_to_trees};
use jj_lib::backend::{
    CommitId as JjCommitId, CopyId, MergedTreeValue, MergedTreeValueExt as _, TreeValue,
};
use jj_lib::commit::Commit;
use jj_lib::config::{ConfigLayer, ConfigSource, StackedConfig};
use jj_lib::conflicts::{
    ConflictMarkerStyle, ConflictMaterializeOptions, choose_materialized_conflict_marker_len,
    extract_as_single_hunk, materialize_merge_result_to_bytes, update_from_content,
};
use jj_lib::files::{self, MergeResult};
use jj_lib::gitignore::GitIgnoreFile;
use jj_lib::matchers::{EverythingMatcher, NothingMatcher};
use jj_lib::merge::Merge;
use jj_lib::merged_tree::MergedTree;
use jj_lib::merged_tree_builder::MergedTreeBuilder;
use jj_lib::object_id::ObjectId;
use jj_lib::op_store::OperationId;
use jj_lib::ref_name::{WorkspaceName, WorkspaceNameBuf};
use jj_lib::repo::{MutableRepo, ReadonlyRepo, Repo, RepoLoader};
use jj_lib::repo_path::RepoPathBuf;
use jj_lib::revset::{
    self, ResolvedRevsetExpression, RevsetAliasesMap, RevsetDiagnostics, RevsetExtensions,
    RevsetParseContext, RevsetWorkspaceContext, SymbolResolver, SymbolResolverExtension,
};
use jj_lib::rewrite::rebase_commit;
use jj_lib::settings::UserSettings;
use jj_lib::transaction::Transaction;
use jj_lib::working_copy::{SnapshotOptions, WorkingCopyFreshness};
use jj_lib::workspace::Workspace;
use jj_lib::workspace_store::{SimpleWorkspaceStore, WorkspaceStore};

use crate::{CommitInfo, Error, Result, chrono_now, err, info};

mod cli;

/// jj-cli 0.45.1's built-in revset aliases (`jj config list --include-defaults revset-aliases`),
/// so `immutable()`, `mutable()` and `trunk()` mean what they mean in the user's jj.
const BUILTIN_ALIASES: &str = r#"
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
'builtin_immutable_heads()' = 'trunk() | tags() | untracked_remote_bookmarks() | untracked_remote_tags()'
'immutable_heads()' = 'builtin_immutable_heads()'
'immutable()' = '::(immutable_heads() | root())'
'mutable()' = '~immutable()'
'visible()' = '::visible_heads()'
'hidden()' = '~visible()'
"#;

/// Largest untracked file a snapshot starts tracking (jj's `snapshot.max-new-file-size` default).
const MAX_NEW_FILE_SIZE: u64 = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditMode {
    /// Rewrite the target; descendants rebase. The default whenever the target is mutable.
    Amend,
    /// New change between the target and its children; descendants rebase.
    InsertAfter,
    /// New change on top of the target as a new head; nothing is rewritten. The only mode for
    /// immutable targets.
    Branch,
    /// Amend the target but keep every descendant's content (`--restore-descendants`).
    KeepDescendants,
}

/// One file to write. `content: None` deletes the file.
#[derive(Debug, Clone, PartialEq)]
pub struct FileWrite {
    pub path: String,
    pub content: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub enum MutationPlan {
    /// Write files into `target` (a commit id) the way `mode` says. Paths absent at the target
    /// are created there (a new file at a non-@ commit, no temporary workspace needed).
    WriteFiles {
        target: String,
        files: Vec<FileWrite>,
        mode: EditMode,
        message: Option<String>,
    },
    /// Range edit: write `files` on top of `tip`, then `jj absorb` the hunks into the commits of
    /// `into` that own the lines; what cannot be absorbed amends the tip.
    Absorb {
        tip: String,
        files: Vec<FileWrite>,
        into: String,
    },
    /// Replace a conflicted file at `commit` (markers left in `content` stay conflicts).
    Resolve {
        commit: String,
        path: String,
        content: Vec<u8>,
    },
    /// `jj op revert <op>`: targeted undo.
    Revert { op: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ConflictInfo {
    pub change: String,
    pub commit: String,
    pub path: String,
    pub sides: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceEffect {
    pub name: String,
    pub snapshotted: bool,
    pub updated: bool,
    /// Commit holding edits rescued from a divergent @, if that happened.
    pub recovered_into: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MutationOutcome {
    pub op_before: String,
    /// None for a preview.
    pub op_after: Option<String>,
    /// Change created by insert-after / branch.
    pub created: Option<String>,
    /// Changes rewritten: the target and every rebased descendant.
    pub rewritten: Vec<String>,
    /// (change, new commit) for every commit the plan wrote, created ones included.
    pub new_commits: Vec<(String, String)>,
    /// Conflicted paths in new commits that their predecessor did not have.
    pub new_conflicts: Vec<ConflictInfo>,
    pub workspaces: Vec<WorkspaceEffect>,
    /// Changes the plan leaves with more than one visible commit that had at most one before
    /// (for a mutation: also a workspace's `@` split by recovering its unsnapshotted files).
    pub divergent: Vec<String>,
}

/// A file read at a commit. Conflicted files come back materialized with jj's markers.
#[derive(Debug, Clone)]
pub struct FileAt {
    pub blob: Option<String>,
    pub bytes: Vec<u8>,
    pub executable: bool,
    pub conflict: bool,
}

#[derive(Debug, Clone)]
pub struct WorkspaceAt {
    pub name: String,
    pub root: Option<PathBuf>,
    pub change: String,
    pub commit: String,
}

/// One conflicted region of a materialized file (1-based lines of the materialized text).
#[derive(Debug, Clone)]
pub struct ConflictRegionText {
    pub start: u32,
    pub end: u32,
    pub base: Option<String>,
    pub sides: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct ConflictText {
    pub info: ConflictInfo,
    pub materialized: String,
    pub regions: Vec<ConflictRegionText>,
}

/// Changes between two operations, for re-anchoring and incremental re-indexing.
#[derive(Debug, Clone, Default)]
pub struct OpDiff {
    /// Changes with a new visible commit.
    pub changed: Vec<String>,
    /// Changes visible before and not any more (abandoned, squashed away).
    pub abandoned: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct OpInfo {
    pub id: String,
    pub description: String,
    pub time_ms: i64,
    pub user: String,
    pub is_snapshot: bool,
    pub parents: Vec<String>,
}

/// Where a plan's writes go, from a dry run: which workspaces' `@` it rewrites.
struct DryRun {
    affected: Vec<String>,
}

pub struct JjWriter {
    root: PathBuf,
    repo_path: PathBuf,
    workspace_name: WorkspaceNameBuf,
    settings: UserSettings,
    aliases: RevsetAliasesMap,
    loader: RepoLoader,
    queue: Mutex<()>,
    cache: Mutex<Option<(String, Arc<ReadonlyRepo>)>>,
    /// Opened without the user's and the repo's jj config (the CLI fallback isolates the same way).
    isolated: bool,
    /// TOML layered on top of the config (`open_with`); the CLI fallback passes it as a config file.
    extra: Option<String>,
    /// Make the jj-lib mutation path fail with an internal error after its dry run, so every
    /// mutation goes through the CLI fallback (tests; `CV_FORCE_JJ_CLI=1`).
    force_cli: AtomicBool,
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "localhost".into())
}

fn toml_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn user_config_files() -> Vec<PathBuf> {
    if let Some(v) = std::env::var_os("JJ_CONFIG") {
        return std::env::split_paths(&v).collect();
    }
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    let mut out = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        out.push(PathBuf::from(home).join(".jjconfig.toml"));
    }
    if let Some(c) = config_home {
        out.push(c.join("jj").join("config.toml"));
        out.push(c.join("jj").join("conf.d"));
    }
    out
}

fn add_config_path(config: &mut StackedConfig, source: ConfigSource, path: &Path) -> Result<()> {
    if path.is_dir() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(path)
            .map_err(err)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "toml"))
            .collect();
        files.sort();
        for f in files {
            config.add_layer(ConfigLayer::load_from_file(source, f).map_err(err)?);
        }
    } else if path.is_file() {
        config.add_layer(ConfigLayer::load_from_file(source, path.to_path_buf()).map_err(err)?);
    }
    Ok(())
}

/// Finds the workspace root at or above `path`.
fn find_root(path: &Path) -> Result<PathBuf> {
    let mut root = path.canonicalize().map_err(err)?;
    while !root.join(".jj").is_dir() {
        root = root
            .parent()
            .ok_or_else(|| Error(format!("{} is not inside a jj workspace", path.display())))?
            .to_path_buf();
    }
    Ok(root)
}

/// The `.jj/repo` directory of a workspace (secondary workspaces point at it with a file).
fn repo_dir(root: &Path) -> PathBuf {
    let p = root.join(".jj").join("repo");
    if p.is_file()
        && let Ok(s) = std::fs::read_to_string(&p)
    {
        let target = root.join(".jj").join(s.trim());
        return target.canonicalize().unwrap_or(target);
    }
    p
}

/// Configuration in force for the repo: jj defaults, the CLI's revset aliases, then (unless
/// `isolated`) the user's and the repo's jj config, then `extra` TOML.
fn build_config(root: &Path, isolated: bool, extra: Option<&str>) -> Result<StackedConfig> {
    let mut config = StackedConfig::with_defaults();
    let user = std::env::var("USER").unwrap_or_else(|_| "cv".into());
    let base = format!(
        "{BUILTIN_ALIASES}\n[operation]\nhostname = {}\nusername = {}\n",
        toml_str(&hostname()),
        toml_str(&user)
    );
    config.add_layer(ConfigLayer::parse(ConfigSource::EnvBase, &base).map_err(err)?);
    if !isolated {
        for p in user_config_files() {
            add_config_path(&mut config, ConfigSource::User, &p)?;
        }
        let repo = repo_dir(root);
        if let Ok(id) = std::fs::read_to_string(repo.join("config-id"))
            && let Some(dir) = user_config_files()
                .into_iter()
                .find(|p| p.ends_with("jj/config.toml"))
                .and_then(|p| p.parent().map(Path::to_path_buf))
        {
            add_config_path(
                &mut config,
                ConfigSource::Repo,
                &dir.join("repos").join(id.trim()).join("config.toml"),
            )?;
        }
        add_config_path(&mut config, ConfigSource::Repo, &repo.join("config.toml"))?;
    }
    if let Some(extra) = extra {
        config.add_layer(ConfigLayer::parse(ConfigSource::CommandArg, extra).map_err(err)?);
    }
    Ok(config)
}

fn aliases_from(config: &StackedConfig) -> Result<RevsetAliasesMap> {
    let mut map = RevsetAliasesMap::new();
    let keys: Vec<String> = config
        .table_keys("revset-aliases")
        .map(str::to_string)
        .collect();
    for k in keys {
        let v: String = config.get(["revset-aliases", k.as_str()]).map_err(err)?;
        map.insert(&k, v, None).map_err(err)?;
    }
    Ok(map)
}

fn block<F: std::future::Future>(f: F) -> F::Output {
    pollster::block_on(f)
}

fn parse_commit_id(hex: &str) -> Result<JjCommitId> {
    JjCommitId::try_from_hex(hex).ok_or_else(|| Error(format!("bad commit id {hex}")))
}

/// Whether a client's path names a file inside the tree: relative, with no empty, `.` or `..`
/// component and nothing under `.jj` or `.git`. jj's own parser accepts `.` and `..`, so an edit
/// to `../x` would otherwise put an entry outside the tree (and the CLI fallback would write there).
pub(crate) fn tree_path_ok(p: &str) -> bool {
    !p.is_empty()
        && p.split('/')
            .all(|c| !matches!(c, "" | "." | ".." | ".jj" | ".git"))
}

fn repo_path(p: &str) -> Result<RepoPathBuf> {
    if !tree_path_ok(p) {
        return Err(Error(format!("invalid repository path {p:?}")));
    }
    RepoPathBuf::from_internal_string(p).map_err(err)
}

fn file_executable(v: &MergedTreeValue) -> bool {
    match v.as_resolved() {
        Some(Some(TreeValue::File { executable, .. })) => *executable,
        _ => false,
    }
}

async fn apply_files(base: &MergedTree, files: &[FileWrite]) -> Result<MergedTree> {
    let store = base.store().clone();
    let mut b = MergedTreeBuilder::new(base.clone());
    for f in files {
        let path = repo_path(&f.path)?;
        match &f.content {
            None => b.set_or_remove(path, Merge::absent()),
            Some(bytes) => {
                let old = base.path_value(&path).await.map_err(err)?;
                let executable = file_executable(&old);
                let id = store
                    .write_file(&path, &mut bytes.as_slice())
                    .await
                    .map_err(err)?;
                b.set_or_remove(
                    path,
                    Merge::normal(TreeValue::File {
                        id,
                        executable,
                        copy_id: CopyId::placeholder(),
                    }),
                );
            }
        }
    }
    b.write_tree().await.map_err(err)
}

fn immutable_error(c: &Commit) -> Error {
    Error(format!(
        "commit {} is immutable; use mode branch",
        &c.id().hex()[..12]
    ))
}

fn conflicts_of(commit: &Commit) -> Vec<(String, u32)> {
    commit
        .tree()
        .conflicts()
        .map(|(p, v)| {
            let sides = v.map(|v| v.num_sides() as u32).unwrap_or(2);
            (p.as_internal_file_string().to_string(), sides)
        })
        .collect()
}

impl JjWriter {
    /// Opens the workspace at `path` (or an ancestor) with the user's jj configuration.
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with(path, false, None)
    }

    /// `isolated` skips the user's and the repo's jj config (tests); `extra` is TOML layered on
    /// top (e.g. `user.name`).
    pub fn open_with(path: &Path, isolated: bool, extra: Option<&str>) -> Result<Self> {
        let root = find_root(path)?;
        let config = build_config(&root, isolated, extra)?;
        let aliases = aliases_from(&config)?;
        let settings = UserSettings::from_config(config).map_err(err)?;
        let ws = Workspace::load(
            &settings,
            &root,
            &jj_lib::default_backend_factories::default_backend_factories(),
            &jj_lib::default_backend_factories::default_working_copy_factories(),
        )
        .map_err(err)?;
        Ok(JjWriter {
            repo_path: ws.repo_path().to_path_buf(),
            workspace_name: ws.workspace_name().to_owned(),
            loader: ws.repo_loader().clone(),
            root,
            settings,
            aliases,
            queue: Mutex::new(()),
            cache: Mutex::new(None),
            isolated,
            extra: extra.map(str::to_string),
            force_cli: AtomicBool::new(std::env::var("CV_FORCE_JJ_CLI").is_ok_and(|v| v == "1")),
        })
    }

    /// Force the CLI fallback: jj-lib mutations fail with an injected internal error before they
    /// commit, and [`Self::mutate`] retries them through the jj CLI (tests of the fallback).
    pub fn set_force_cli_fallback(&self, on: bool) {
        self.force_cli.store(on, Ordering::Relaxed);
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The `.jj/repo` directory (shared by every workspace of the repo).
    pub fn repo_path(&self) -> &Path {
        &self.repo_path
    }

    pub fn workspace_name(&self) -> &str {
        self.workspace_name.as_str()
    }

    /// Cheap change detector for the op watcher: the names in `.jj/repo/op_heads/heads`.
    pub fn op_heads_signature(&self) -> String {
        let dir = self.repo_path.join("op_heads").join("heads");
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names.join(",")
    }

    /// The repo at the current head operation (cached until `op_heads` changes).
    fn head(&self) -> Result<Arc<ReadonlyRepo>> {
        let sig = self.op_heads_signature();
        let mut cache = self.cache.lock().unwrap();
        if let Some((s, repo)) = cache.as_ref()
            && *s == sig
        {
            return Ok(repo.clone());
        }
        let repo = block(self.loader.load_at_head()).map_err(err)?;
        *cache = Some((sig, repo.clone()));
        Ok(repo)
    }

    pub fn head_op(&self) -> Result<String> {
        Ok(self.head()?.op_id().hex())
    }

    /// A read-only [`crate::JjRepo`] pinned at the current head.
    pub fn reader(&self) -> Result<crate::JjRepo> {
        Ok(crate::JjRepo {
            repo: self.head()?,
            workspace_name: self.workspace_name.clone(),
            workspace_root: self.root.clone(),
            marker_style: jj_lib::conflicts::ConflictMarkerStyle::Diff,
        }
        .with_user_marker_style())
    }

    fn eval_in(&self, repo: &dyn Repo, text: &str) -> Result<Vec<JjCommitId>> {
        let expr = self.resolve_expr(repo, text)?;
        let set = expr.evaluate(repo).map_err(err)?;
        block(async {
            let mut out = Vec::new();
            let mut s = set.stream();
            while let Some(id) = s.next().await {
                out.push(id.map_err(err)?);
            }
            Ok(out)
        })
    }

    fn resolve_expr(&self, repo: &dyn Repo, text: &str) -> Result<Arc<ResolvedRevsetExpression>> {
        let fileset_aliases = jj_lib::fileset::FilesetAliasesMap::new();
        let extensions = RevsetExtensions::default();
        let path_converter = jj_lib::ui_path::RepoPathUiConverter::Fs {
            cwd: self.root.clone(),
            base: self.root.clone(),
        };
        let email = self.settings.user_email().to_string();
        let ignored = jj_lib::git::get_git_backend(repo.store())
            .is_ok()
            .then_some(jj_lib::git::REMOTE_NAME_FOR_LOCAL_GIT_REPO);
        let context = RevsetParseContext {
            aliases_map: &self.aliases,
            local_variables: Default::default(),
            user_email: &email,
            date_pattern_context: chrono_now(),
            default_ignored_remote: ignored,
            fileset_aliases_map: &fileset_aliases,
            extensions: &extensions,
            workspace: Some(RevsetWorkspaceContext {
                path_converter: &path_converter,
                workspace_name: &self.workspace_name,
            }),
        };
        let mut diagnostics = RevsetDiagnostics::new();
        let expr = revset::parse(&mut diagnostics, text, &context).map_err(err)?;
        let no_ext: [Box<dyn SymbolResolverExtension>; 0] = [];
        let resolver = SymbolResolver::new(repo, &no_ext);
        expr.resolve_user_expression(repo, &resolver).map_err(err)
    }

    /// Commits of a revset at head (with the user's aliases: `immutable()`, `trunk()` …).
    pub fn resolve(&self, revset_str: &str) -> Result<Vec<CommitInfo>> {
        let repo = self.head()?;
        let ids = self.eval_in(repo.as_ref(), revset_str)?;
        ids.iter()
            .map(|id| Ok(info(&repo.store().get_commit(id).map_err(err)?)))
            .collect()
    }

    /// A change id or commit id (prefixes allowed) to its one visible commit.
    pub fn resolve_rev(&self, rev: &str) -> Result<CommitInfo> {
        let mut v = self.resolve(rev)?;
        match v.len() {
            1 => Ok(v.remove(0)),
            0 => Err(Error(format!("{rev:?} resolved to no commits"))),
            n => Err(Error(format!(
                "{rev:?} resolved to {n} commits (divergent?), expected one"
            ))),
        }
    }

    /// The visible commit(s) of a full change id; empty when the change was abandoned.
    pub fn change_commits(&self, change: &str) -> Result<Vec<CommitInfo>> {
        let repo = self.head()?;
        let Some(id) = jj_lib::backend::ChangeId::try_from_reverse_hex(change) else {
            return Err(Error(format!("bad change id {change}")));
        };
        let targets = block(repo.resolve_change_id(&id)).map_err(err)?;
        let Some(t) = targets else {
            return Ok(vec![]);
        };
        t.visible_with_offsets()
            .map(|(_, c)| Ok(info(&repo.store().get_commit(c).map_err(err)?)))
            .collect()
    }

    /// The evolog path from `from` to `to` (`jj evolog`): the chain of commits, oldest first, that
    /// rewrote `from` into `to` (both included; `[to]` when they are equal). None when `from` is not
    /// among `to`'s predecessors within `max` evolution entries (e.g. a divergent sibling, or a
    /// rewrite older than the walk allows).
    pub fn evolution_path(&self, from: &str, to: &str, max: usize) -> Result<Option<Vec<String>>> {
        if from == to {
            return Ok(Some(vec![to.to_string()]));
        }
        let repo = self.head()?;
        let (from_id, to_id) = (parse_commit_id(from)?, parse_commit_id(to)?);
        // The successor of each predecessor seen, so the chain can be walked forward from `from`.
        let mut next: HashMap<JjCommitId, JjCommitId> = HashMap::new();
        let mut found = false;
        block(async {
            let mut walk = std::pin::pin!(jj_lib::evolution::walk_predecessors(
                &repo,
                std::slice::from_ref(&to_id)
            ));
            let mut n = 0;
            while let Some(entry) = walk.next().await {
                let Ok(entry) = entry else { break };
                for p in entry.predecessor_ids() {
                    next.entry(p.clone())
                        .or_insert_with(|| entry.commit.id().clone());
                    found |= *p == from_id;
                }
                n += 1;
                if found || n >= max {
                    break;
                }
            }
        });
        if !found {
            return Ok(None);
        }
        let mut path = vec![from.to_string()];
        let mut at = from_id;
        while at != to_id {
            let Some(n) = next.get(&at) else {
                return Ok(None);
            };
            path.push(n.hex());
            at = n.clone();
            if path.len() > max + 1 {
                return Ok(None);
            }
        }
        Ok(Some(path))
    }

    pub fn is_immutable(&self, commit: &str) -> Result<bool> {
        let repo = self.head()?;
        Ok(!self
            .eval_in(repo.as_ref(), &format!("{commit} & immutable()"))?
            .is_empty())
    }

    /// The file at `path` in `commit`, or None when absent (or not a regular file).
    pub fn file_at(&self, commit: &str, path: &str) -> Result<Option<FileAt>> {
        let repo = self.head()?;
        let c = repo
            .store()
            .get_commit(&parse_commit_id(commit)?)
            .map_err(err)?;
        let p = repo_path(path)?;
        let tree = c.tree();
        let value = block(tree.path_value(&p)).map_err(err)?;
        match value.into_resolved() {
            Ok(Some(TreeValue::File { id, executable, .. })) => {
                let bytes = self.reader()?.read_blob(path, &id.hex())?;
                Ok(Some(FileAt {
                    blob: Some(id.hex()),
                    bytes,
                    executable,
                    conflict: false,
                }))
            }
            Ok(_) => Ok(None),
            Err(_) => Ok(self.conflict_text(commit, path)?.map(|t| FileAt {
                blob: None,
                bytes: t.materialized.into_bytes(),
                executable: false,
                conflict: true,
            })),
        }
    }

    pub fn read_blob(&self, path: &str, blob: &str) -> Result<Vec<u8>> {
        self.reader()?.read_blob(path, blob)
    }

    /// Every workspace's `@` at head, with its root on disk when known.
    pub fn workspaces(&self) -> Result<Vec<WorkspaceAt>> {
        let repo = self.head()?;
        let store = SimpleWorkspaceStore::load(&self.repo_path).map_err(err)?;
        let mut out = Vec::new();
        for (name, id) in repo.view().wc_commit_ids() {
            let c = repo.store().get_commit(id).map_err(err)?;
            let root = if *name == self.workspace_name {
                Some(self.root.clone())
            } else {
                store
                    .get_workspace_path(name)
                    .ok()
                    .flatten()
                    .map(|p| self.repo_path.join(p))
                    .map(|p| p.canonicalize().unwrap_or(p))
            };
            out.push(WorkspaceAt {
                name: name.as_str().to_string(),
                root,
                change: c.change_id().reverse_hex(),
                commit: id.hex(),
            });
        }
        Ok(out)
    }

    /// Conflicted files in the commits of `revset`.
    pub fn conflicts(&self, revset_str: &str) -> Result<Vec<ConflictInfo>> {
        self.conflicts_in(self.head()?, revset_str)
    }

    /// [`JjWriter::conflicts`] as of operation `op` (the repo that op left behind).
    pub fn conflicts_at_op(&self, op: &str, revset_str: &str) -> Result<Vec<ConflictInfo>> {
        let id = OperationId::try_from_hex(op).ok_or_else(|| Error(format!("bad op id {op}")))?;
        let o = block(self.loader.load_operation(&id)).map_err(err)?;
        let repo = block(self.loader.load_at(&o)).map_err(err)?;
        self.conflicts_in(repo, revset_str)
    }

    fn conflicts_in(&self, repo: Arc<ReadonlyRepo>, revset_str: &str) -> Result<Vec<ConflictInfo>> {
        let ids = self.eval_in(repo.as_ref(), &format!("({revset_str}) & conflicts()"))?;
        let mut out = Vec::new();
        for id in ids {
            let c = repo.store().get_commit(&id).map_err(err)?;
            for (path, sides) in conflicts_of(&c) {
                out.push(ConflictInfo {
                    change: c.change_id().reverse_hex(),
                    commit: id.hex(),
                    path,
                    sides,
                });
            }
        }
        Ok(out)
    }

    /// The conflicted file with jj's markers (the user's `ui.conflict-marker-style`, "diff" by
    /// default) and its regions. None when the file is not conflicted there.
    pub fn conflict_text(&self, commit: &str, path: &str) -> Result<Option<ConflictText>> {
        let repo = self.head()?;
        let c = repo
            .store()
            .get_commit(&parse_commit_id(commit)?)
            .map_err(err)?;
        let p = repo_path(path)?;
        let tree = c.tree();
        let value = block(tree.path_value(&p)).map_err(err)?;
        if value.is_resolved() {
            return Ok(None);
        }
        let Some(file) = block(jj_lib::conflicts::try_materialize_file_conflict_value(
            repo.store(),
            &p,
            &value,
            tree.labels(),
        ))
        .map_err(err)?
        else {
            return Err(Error(format!(
                "{path} at {commit} is a conflict that is not between files"
            )));
        };
        let style: ConflictMarkerStyle = self
            .settings
            .get("ui.conflict-marker-style")
            .unwrap_or(ConflictMarkerStyle::Diff);
        let options = ConflictMaterializeOptions {
            marker_style: style,
            marker_len: None,
            merge: repo.store().merge_options().clone(),
        };
        let materialized =
            materialize_merge_result_to_bytes(&file.contents, &file.labels, &options);
        let materialized = String::from_utf8_lossy(&materialized).into_owned();
        // Regions: walk the merge hunks, counting lines of the materialized text the way the
        // marker writer lays them out (one opening marker, per-term header lines, one closing).
        let mut regions = Vec::new();
        if let MergeResult::Conflict(hunks) = files::merge_hunks(&file.contents, &options.merge) {
            let marker_lines: Vec<(usize, &str)> = materialized
                .lines()
                .enumerate()
                .filter(|(_, l)| l.starts_with("<<<<<<<") || l.starts_with(">>>>>>>"))
                .collect();
            let mut pairs = marker_lines.chunks(2);
            let labels: Vec<String> = file
                .labels
                .as_slice()
                .iter()
                .map(|l| l.to_string())
                .collect();
            for hunk in hunks.iter().filter(|h| !h.is_resolved()) {
                let (start, end) = match pairs.next() {
                    Some([(s, _), (e, _)]) => (*s as u32 + 1, *e as u32 + 1),
                    _ => (0, 0),
                };
                let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
                let base = hunk.removes().next().map(|b| text(b));
                let n_adds = hunk.adds().len();
                let sides = hunk
                    .adds()
                    .enumerate()
                    .map(|(i, a)| {
                        // ConflictLabels interleave as [add0, remove0, add1, …].
                        let label = labels
                            .get(i * 2)
                            .cloned()
                            .unwrap_or_else(|| format!("side #{} of {n_adds}", i + 1));
                        (label, text(a))
                    })
                    .collect();
                regions.push(ConflictRegionText {
                    start,
                    end,
                    base,
                    sides,
                });
            }
        }
        let sides = value.num_sides() as u32;
        Ok(Some(ConflictText {
            info: ConflictInfo {
                change: c.change_id().reverse_hex(),
                commit: commit.to_string(),
                path: path.to_string(),
                sides,
            },
            materialized,
            regions,
        }))
    }

    /// Three-way line merge: `ours` and `theirs` both derived from `base`. None on conflict.
    pub fn merge3(&self, base: &[u8], ours: &[u8], theirs: &[u8]) -> Option<Vec<u8>> {
        let repo = self.head().ok()?;
        let m = Merge::from_vec(vec![ours.to_vec(), base.to_vec(), theirs.to_vec()]);
        match files::merge_hunks(&m, repo.store().merge_options()) {
            MergeResult::Resolved(b) => Some(b.to_vec()),
            MergeResult::Conflict(_) => None,
        }
    }

    pub fn op_info(&self, op: &str) -> Result<OpInfo> {
        let id = OperationId::try_from_hex(op).ok_or_else(|| Error(format!("bad op id {op}")))?;
        let o = block(self.loader.load_operation(&id)).map_err(err)?;
        let m = o.metadata();
        Ok(OpInfo {
            id: op.to_string(),
            description: m.description.clone(),
            time_ms: m.time.end.timestamp.0,
            user: format!("{}@{}", m.username, m.hostname),
            is_snapshot: m.is_snapshot,
            parents: o.parent_ids().iter().map(|p| p.hex()).collect(),
        })
    }

    /// Changes whose visible commits differ between two operations.
    pub fn op_diff(&self, from: &str, to: &str) -> Result<OpDiff> {
        let load = |op: &str| -> Result<Arc<ReadonlyRepo>> {
            let id =
                OperationId::try_from_hex(op).ok_or_else(|| Error(format!("bad op id {op}")))?;
            let o = block(self.loader.load_operation(&id)).map_err(err)?;
            block(self.loader.load_at(&o)).map_err(err)
        };
        let (old, new) = (load(from)?, load(to)?);
        let old_heads: Vec<JjCommitId> = old.view().heads().iter().cloned().collect();
        let new_heads: Vec<JjCommitId> = new.view().heads().iter().cloned().collect();
        let collect =
            |repo: &ReadonlyRepo, a: &[JjCommitId], b: &[JjCommitId]| -> Result<Vec<Commit>> {
                let expr = ResolvedRevsetExpression::commits(a.to_vec())
                    .ancestors()
                    .minus(&ResolvedRevsetExpression::commits(b.to_vec()).ancestors());
                let set = expr.evaluate(repo).map_err(err)?;
                block(async {
                    let mut out = Vec::new();
                    let mut s = set.stream();
                    while let Some(id) = s.next().await {
                        out.push(repo.store().get_commit(&id.map_err(err)?).map_err(err)?);
                    }
                    Ok(out)
                })
            };
        let mut changed: BTreeSet<String> = BTreeSet::new();
        for c in collect(&new, &new_heads, &old_heads)? {
            changed.insert(c.change_id().reverse_hex());
        }
        let mut abandoned = BTreeSet::new();
        // Old visible commits are still in the new repo's index (the op log only grows).
        for c in collect(&new, &old_heads, &new_heads)? {
            let targets = block(new.resolve_change_id(c.change_id())).map_err(err)?;
            if targets.is_none_or(|t| t.visible_with_offsets().next().is_none()) {
                abandoned.insert(c.change_id().reverse_hex());
            }
        }
        Ok(OpDiff {
            changed: changed.into_iter().collect(),
            abandoned: abandoned.into_iter().collect(),
        })
    }

    // ── Mutations ──

    /// Exact preview: rewrite and rebase inside a transaction, then drop it.
    pub fn preview(&self, plan: &MutationPlan) -> Result<MutationOutcome> {
        let _q = self.queue.lock().unwrap();
        let repo = self.head()?;
        let mut tx = repo.start_transaction();
        let created = self.run_plan(&mut tx, plan)?;
        let mut out = self.summarize(&repo, tx.repo(), created)?;
        out.workspaces = self
            .changed_workspaces(&repo, tx.repo())
            .into_iter()
            .map(|name| WorkspaceEffect {
                name,
                snapshotted: false,
                updated: false,
                recovered_into: None,
            })
            .collect();
        drop(tx);
        Ok(out)
    }

    /// Snapshot affected workspaces, commit the transaction, update their checkouts. User errors
    /// (see [`Self::validate`]) are returned as they are; if jj-lib fails otherwise before the
    /// transaction is committed, the plan is retried through the jj CLI ([`cli`]).
    pub fn mutate(&self, plan: &MutationPlan, description: &str) -> Result<MutationOutcome> {
        let _q = self.queue.lock().unwrap();
        self.validate(plan)?;
        let mut committed = false;
        match self.mutate_lib(plan, description, &mut committed) {
            Err(e) if !committed => {
                eprintln!(
                    "cv-vcs: jj-lib {} failed ({e}); falling back to the jj CLI",
                    cli::plan_name(plan)
                );
                *self.cache.lock().unwrap() = None;
                self.mutate_cli(plan)
                    .map_err(|c| Error(format!("{e}; jj CLI fallback failed too: {c}")))
            }
            r => r,
        }
    }

    /// The checks every path shares, so their failures are user errors and never fall back:
    /// the commit or operation exists, the target is mutable (except for branch), the path is
    /// a file conflict, the operation has one parent.
    fn validate(&self, plan: &MutationPlan) -> Result<()> {
        let repo = self.head()?;
        let commit = |hex: &str| {
            repo.store()
                .get_commit(&parse_commit_id(hex)?)
                .map_err(|e| Error(format!("unknown commit {hex}: {e}")))
        };
        match plan {
            MutationPlan::WriteFiles {
                target,
                files,
                mode,
                ..
            } => {
                let c = commit(target)?;
                for f in files {
                    repo_path(&f.path)?;
                }
                if *mode != EditMode::Branch
                    && !self
                        .eval_in(repo.as_ref(), &format!("{} & immutable()", c.id().hex()))?
                        .is_empty()
                {
                    return Err(immutable_error(&c));
                }
            }
            MutationPlan::Absorb { tip, files, .. } => {
                commit(tip)?;
                for f in files {
                    repo_path(&f.path)?;
                }
            }
            MutationPlan::Resolve {
                commit: hex, path, ..
            } => {
                let c = commit(hex)?;
                let value = block(c.tree().path_value(&repo_path(path)?)).map_err(err)?;
                if value.is_resolved() {
                    return Err(Error(format!("{path} is not conflicted at {hex}")));
                }
                if value.to_file_merge().is_none() {
                    return Err(Error(format!(
                        "{path}: only file conflicts can be resolved"
                    )));
                }
            }
            MutationPlan::Revert { op } => {
                let id = OperationId::try_from_hex(op)
                    .ok_or_else(|| Error(format!("bad op id {op}")))?;
                let target = block(self.loader.load_operation(&id))
                    .map_err(|e| Error(format!("unknown operation {op}: {e}")))?;
                if target.parent_ids().len() != 1 {
                    return Err(Error("cannot revert the root or a merge operation".into()));
                }
            }
        }
        Ok(())
    }

    /// The jj-lib path of [`Self::mutate`]; sets `committed` once the operation is written.
    fn mutate_lib(
        &self,
        plan: &MutationPlan,
        description: &str,
        committed: &mut bool,
    ) -> Result<MutationOutcome> {
        let repo0 = self.head()?;
        let op_before = repo0.op_id().hex();
        // Dry run: which workspaces' @ does the plan rewrite?
        let dry = {
            let mut tx = repo0.start_transaction();
            self.run_plan(&mut tx, plan)?;
            DryRun {
                affected: self.changed_workspaces(&repo0, tx.repo()),
            }
        };
        if self.force_cli.load(Ordering::Relaxed) {
            return Err(Error(
                "injected jj-lib failure (CV_FORCE_JJ_CLI / set_force_cli_fallback)".into(),
            ));
        }
        let roots: HashMap<String, Option<PathBuf>> = self
            .workspaces()?
            .into_iter()
            .map(|w| (w.name, w.root))
            .collect();
        let mut effects: BTreeMap<String, WorkspaceEffect> = BTreeMap::new();
        for name in &dry.affected {
            let snapshotted = match roots.get(name).cloned().flatten() {
                Some(root) if root.join(".jj").is_dir() => self.snapshot_workspace(name, &root)?,
                _ => false,
            };
            effects.insert(
                name.clone(),
                WorkspaceEffect {
                    name: name.clone(),
                    snapshotted,
                    updated: false,
                    recovered_into: None,
                },
            );
        }
        // The real run, on the repo as the snapshots left it.
        let repo = self.head()?;
        let mut tx = repo.start_transaction();
        tx.set_workspace_name(&self.workspace_name);
        let created = self.run_plan(&mut tx, plan)?;
        let mut out = self.summarize(&repo, tx.repo(), created)?;
        let changed = self.changed_workspaces(&repo, tx.repo());
        let colocated: Vec<&String> = changed
            .iter()
            .filter(|n| {
                roots
                    .get(*n)
                    .cloned()
                    .flatten()
                    .is_some_and(|r| r.join(".git").exists())
            })
            .collect();
        if !colocated.is_empty() && jj_lib::git::get_git_backend(repo.store()).is_ok() {
            for name in &colocated {
                let ws_name = WorkspaceNameBuf::from(name.as_str());
                let root = roots.get(*name).cloned().flatten().unwrap_or_default();
                if let Some(id) = tx.repo().view().get_wc_commit_id(&ws_name).cloned() {
                    let c = tx.repo().store().get_commit(&id).map_err(err)?;
                    let _ = block(jj_lib::git::reset_head(tx.repo_mut(), &ws_name, &root, &c));
                }
            }
            jj_lib::git::export_refs(tx.repo_mut()).map_err(err)?;
        }
        let new_repo = block(tx.commit(description)).map_err(err)?;
        *committed = true;
        out.op_before = op_before;
        out.op_after = Some(new_repo.op_id().hex());
        // Check the new @s out on disk.
        for name in changed {
            let e = effects
                .entry(name.clone())
                .or_insert_with(|| WorkspaceEffect {
                    name: name.clone(),
                    snapshotted: false,
                    updated: false,
                    recovered_into: None,
                });
            if let Some(root) = roots.get(&name).cloned().flatten()
                && root.join(".jj").is_dir()
            {
                e.updated = self.check_out(&name, &root, &repo, &new_repo)?;
                if !e.updated {
                    // The files on disk are not what the old `@` recorded: the workspace was
                    // stale (rewritten elsewhere without a snapshot). Recover it (§6 step 3).
                    *self.cache.lock().unwrap() = None;
                    let (updated, recovered) = self.recover_stale(&name, &root)?;
                    e.updated = updated;
                    e.recovered_into = recovered;
                }
            }
        }
        *self.cache.lock().unwrap() = None;
        // A workspace whose `@` is divergent now has its edits in the sibling (§6 step 3).
        let names: Vec<String> = effects.keys().cloned().collect();
        for (name, _at, others) in self.divergent_wcs(&names)? {
            if let Some(e) = effects.get_mut(&name)
                && e.recovered_into.is_none()
            {
                e.recovered_into = others.first().cloned();
            }
            let repo = self.head()?;
            if let Some(id) = repo
                .view()
                .get_wc_commit_id(&WorkspaceNameBuf::from(name.as_str()))
            {
                let change = repo
                    .store()
                    .get_commit(id)
                    .map_err(err)?
                    .change_id()
                    .reverse_hex();
                if !out.divergent.contains(&change) {
                    out.divergent.push(change);
                }
            }
        }
        out.workspaces = effects.into_values().collect();
        Ok(out)
    }

    /// Workspaces whose `@` differs between two repo states.
    fn changed_workspaces(&self, before: &ReadonlyRepo, after: &MutableRepo) -> Vec<String> {
        let a = before.view().wc_commit_ids();
        let b = after.view().wc_commit_ids();
        let mut names: BTreeSet<String> = BTreeSet::new();
        for (n, id) in a {
            if b.get(n) != Some(id) {
                names.insert(n.as_str().to_string());
            }
        }
        for (n, id) in b {
            if a.get(n) != Some(id) {
                names.insert(n.as_str().to_string());
            }
        }
        names.into_iter().collect()
    }

    /// `jj` auto-snapshot of one workspace: its files on disk become its `@`. Returns false when
    /// the workspace is stale and could not be snapshotted.
    fn snapshot_workspace(&self, name: &str, root: &Path) -> Result<bool> {
        let mut ws = Workspace::load(
            &self.settings,
            root,
            &jj_lib::default_backend_factories::default_backend_factories(),
            &jj_lib::default_backend_factories::default_working_copy_factories(),
        )
        .map_err(err)?;
        let ws_name = WorkspaceNameBuf::from(name);
        // The working copy's trees must come from the same Store instance as its repo.
        let mut repo = block(ws.repo_loader().load_at_head()).map_err(err)?;
        let mut locked = block(ws.start_working_copy_mutation()).map_err(err)?;
        let Some(wc_id) = repo.view().get_wc_commit_id(&ws_name).cloned() else {
            return Ok(false);
        };
        let mut wc_commit = repo.store().get_commit(&wc_id).map_err(err)?;
        match block(WorkingCopyFreshness::check_stale(
            locked.locked_wc(),
            &wc_commit,
            &repo,
        ))
        .map_err(err)?
        {
            WorkingCopyFreshness::Fresh => {}
            WorkingCopyFreshness::Updated(op) => {
                repo = block(repo.reload_at(&op)).map_err(err)?;
                let Some(id) = repo.view().get_wc_commit_id(&ws_name).cloned() else {
                    return Ok(false);
                };
                wc_commit = repo.store().get_commit(&id).map_err(err)?;
            }
            WorkingCopyFreshness::WorkingCopyStale | WorkingCopyFreshness::SiblingOperation => {
                return Ok(false);
            }
        }
        let options = SnapshotOptions {
            base_ignores: GitIgnoreFile::empty(),
            progress: None,
            start_tracking_matcher: &EverythingMatcher,
            force_tracking_matcher: &NothingMatcher,
            max_new_file_size: MAX_NEW_FILE_SIZE,
        };
        let (new_tree, _stats) = block(locked.locked_wc().snapshot(&options)).map_err(err)?;
        let op = if new_tree.tree_ids_and_labels() != wc_commit.tree().tree_ids_and_labels() {
            let mut tx = repo.start_transaction();
            tx.set_is_snapshot(true);
            tx.set_workspace_name(&ws_name);
            let immutable = !self
                .eval_in(
                    tx.repo(),
                    &format!("{} & immutable()", wc_commit.id().hex()),
                )?
                .is_empty();
            let new_wc = if immutable {
                block(
                    tx.repo_mut()
                        .new_commit(vec![wc_commit.id().clone()], new_tree)
                        .write(),
                )
            } else {
                block(
                    tx.repo_mut()
                        .rewrite_commit(&wc_commit)
                        .set_tree(new_tree)
                        .write(),
                )
            }
            .map_err(err)?;
            tx.repo_mut()
                .set_wc_commit(ws_name.clone(), new_wc.id().clone())
                .map_err(err)?;
            block(tx.repo_mut().rebase_descendants()).map_err(err)?;
            let new_repo = block(tx.commit("snapshot working copy")).map_err(err)?;
            new_repo.op_id().clone()
        } else {
            repo.op_id().clone()
        };
        block(locked.finish(op)).map_err(err)?;
        *self.cache.lock().unwrap() = None;
        Ok(true)
    }

    /// Updates a workspace's files to its new `@`. False when the files on disk are not what the
    /// old `@` recorded (the workspace stays stale; `jj workspace update-stale` recovers it).
    fn check_out(
        &self,
        name: &str,
        root: &Path,
        before: &ReadonlyRepo,
        after: &Arc<ReadonlyRepo>,
    ) -> Result<bool> {
        let ws_name = WorkspaceNameBuf::from(name);
        let Some(new_id) = after.view().get_wc_commit_id(&ws_name) else {
            return Ok(false);
        };
        let mut ws = Workspace::load(
            &self.settings,
            root,
            &jj_lib::default_backend_factories::default_backend_factories(),
            &jj_lib::default_backend_factories::default_working_copy_factories(),
        )
        .map_err(err)?;
        // Commits and trees through the workspace's own Store instance.
        let store = ws.repo_loader().store().clone();
        let new_commit = store.get_commit(new_id).map_err(err)?;
        let old_tree = before
            .view()
            .get_wc_commit_id(&ws_name)
            .and_then(|id| store.get_commit(id).ok())
            .map(|c| c.tree());
        match block(ws.check_out(after.op_id().clone(), old_tree.as_ref(), &new_commit)) {
            Ok(_) => Ok(true),
            Err(jj_lib::working_copy::CheckoutError::ConcurrentCheckout) => Ok(false),
            Err(e) => Err(err(e)),
        }
    }

    /// Applies `plan` inside `tx` and rebases descendants. Returns the created commit, if any.
    fn run_plan(&self, tx: &mut Transaction, plan: &MutationPlan) -> Result<Option<Commit>> {
        let created = match plan {
            MutationPlan::WriteFiles {
                target,
                files,
                mode,
                message,
            } => self.write_files(tx, target, files, *mode, message.as_deref())?,
            MutationPlan::Absorb { tip, files, into } => {
                self.absorb(tx, tip, files, into)?;
                None
            }
            MutationPlan::Resolve {
                commit,
                path,
                content,
            } => {
                self.resolve_file(tx, commit, path, content)?;
                None
            }
            MutationPlan::Revert { op } => {
                self.revert(tx, op)?;
                None
            }
        };
        if tx.repo().has_rewrites() {
            block(tx.repo_mut().rebase_descendants()).map_err(err)?;
        }
        Ok(created)
    }

    fn write_files(
        &self,
        tx: &mut Transaction,
        target: &str,
        files: &[FileWrite],
        mode: EditMode,
        message: Option<&str>,
    ) -> Result<Option<Commit>> {
        let target = tx
            .repo()
            .store()
            .get_commit(&parse_commit_id(target)?)
            .map_err(err)?;
        if mode != EditMode::Branch
            && !self
                .eval_in(tx.repo(), &format!("{} & immutable()", target.id().hex()))?
                .is_empty()
        {
            return Err(immutable_error(&target));
        }
        let new_tree = block(apply_files(&target.tree(), files))?;
        match mode {
            EditMode::Amend => {
                block(
                    tx.repo_mut()
                        .rewrite_commit(&target)
                        .set_tree(new_tree)
                        .write(),
                )
                .map_err(err)?;
                block(tx.repo_mut().rebase_descendants()).map_err(err)?;
                Ok(None)
            }
            EditMode::KeepDescendants => {
                let target_id = target.id().clone();
                block(tx.repo_mut().transform_descendants(
                    vec![target_id.clone()],
                    async |rewriter| {
                        if *rewriter.old_commit().id() == target_id {
                            rewriter
                                .reparent()
                                .set_tree(new_tree.clone())
                                .write()
                                .await?;
                        } else {
                            let old_tree = rewriter.old_commit().tree();
                            rewriter.reparent().set_tree(old_tree).write().await?;
                        }
                        Ok(())
                    },
                ))
                .map_err(err)?;
                Ok(None)
            }
            EditMode::Branch | EditMode::InsertAfter => {
                let children = if mode == EditMode::InsertAfter {
                    self.eval_in(tx.repo(), &format!("children({})", target.id().hex()))?
                } else {
                    vec![]
                };
                let mut b = tx
                    .repo_mut()
                    .new_commit(vec![target.id().clone()], new_tree);
                if let Some(m) = message.filter(|m| !m.trim().is_empty()) {
                    // Like the CLI: descriptions end with a newline.
                    let m = if m.ends_with('\n') {
                        m.to_string()
                    } else {
                        format!("{m}\n")
                    };
                    b = b.set_description(m);
                }
                let new_commit = block(b.write()).map_err(err)?;
                for child in children {
                    let child = tx.repo().store().get_commit(&child).map_err(err)?;
                    let parents: Vec<JjCommitId> = child
                        .parent_ids()
                        .iter()
                        .map(|p| {
                            if p == target.id() {
                                new_commit.id().clone()
                            } else {
                                p.clone()
                            }
                        })
                        .collect();
                    block(rebase_commit(tx.repo_mut(), child, parents)).map_err(err)?;
                }
                block(tx.repo_mut().rebase_descendants()).map_err(err)?;
                Ok(Some(new_commit))
            }
        }
    }

    fn absorb(
        &self,
        tx: &mut Transaction,
        tip: &str,
        files: &[FileWrite],
        into: &str,
    ) -> Result<()> {
        let tip = tx
            .repo()
            .store()
            .get_commit(&parse_commit_id(tip)?)
            .map_err(err)?;
        let new_tree = block(apply_files(&tip.tree(), files))?;
        // The edit as a commit on top of the tip: the absorb source.
        let source = block(
            tx.repo_mut()
                .new_commit(vec![tip.id().clone()], new_tree)
                .write(),
        )
        .map_err(err)?;
        let dest_ids = self.eval_in(
            tx.repo(),
            &format!("({into}) & mutable() & ::{}", tip.id().hex()),
        )?;
        let destinations = ResolvedRevsetExpression::commits(dest_ids);
        let abs = block(AbsorbSource::from_commit(tx.repo(), source.clone())).map_err(err)?;
        let selected = block(split_hunks_to_trees(
            tx.repo(),
            &abs,
            &destinations,
            &EverythingMatcher,
        ))
        .map_err(err)?;
        let stats =
            block(absorb_hunks(tx.repo_mut(), &abs, selected.target_commits)).map_err(err)?;
        // Leftovers (hunks no commit owns) amend the tip: squash the remaining source into it.
        if let Some(rest) = stats.rewritten_source {
            let tip_now = tx
                .repo()
                .store()
                .get_commit(&rest.parent_ids()[0])
                .map_err(err)?;
            let rest_tree = rest.tree();
            tx.repo_mut().record_abandoned_commit(&rest);
            block(
                tx.repo_mut()
                    .rewrite_commit(&tip_now)
                    .set_tree(rest_tree)
                    .write(),
            )
            .map_err(err)?;
            block(tx.repo_mut().rebase_descendants()).map_err(err)?;
        }
        Ok(())
    }

    fn resolve_file(
        &self,
        tx: &mut Transaction,
        commit: &str,
        path: &str,
        content: &[u8],
    ) -> Result<()> {
        let c = tx
            .repo()
            .store()
            .get_commit(&parse_commit_id(commit)?)
            .map_err(err)?;
        let p = repo_path(path)?;
        let tree = c.tree();
        let value = block(tree.path_value(&p)).map_err(err)?;
        if value.is_resolved() {
            return Err(Error(format!("{path} is not conflicted at {commit}")));
        }
        let store = tx.repo().store().clone();
        let file_ids = value
            .to_file_merge()
            .ok_or_else(|| Error(format!("{path}: only file conflicts can be resolved")))?;
        let contents =
            block(extract_as_single_hunk(&file_ids.simplify(), &store, &p)).map_err(err)?;
        let marker_len = choose_materialized_conflict_marker_len(&contents);
        let new_ids = block(update_from_content(
            &file_ids, &store, &p, content, marker_len,
        ))
        .map_err(err)?;
        let executable = jj_lib::conflicts::resolve_file_executable(
            &value.to_executable_merge().unwrap_or(Merge::resolved(None)),
        )
        .unwrap_or(false);
        let new_value: MergedTreeValue = match new_ids.as_resolved() {
            Some(Some(id)) => Merge::normal(TreeValue::File {
                id: id.clone(),
                executable,
                copy_id: CopyId::placeholder(),
            }),
            Some(None) => Merge::absent(),
            None => value.with_new_file_ids(&new_ids),
        };
        let mut b = MergedTreeBuilder::new(tree);
        b.set_or_remove(p, new_value);
        let new_tree = block(b.write_tree()).map_err(err)?;
        block(tx.repo_mut().rewrite_commit(&c).set_tree(new_tree).write()).map_err(err)?;
        block(tx.repo_mut().rebase_descendants()).map_err(err)?;
        Ok(())
    }

    fn revert(&self, tx: &mut Transaction, op: &str) -> Result<()> {
        let id = OperationId::try_from_hex(op).ok_or_else(|| Error(format!("bad op id {op}")))?;
        let target = block(self.loader.load_operation(&id)).map_err(err)?;
        let parents = block(target.parents()).map_err(err)?;
        let [parent] = parents.as_slice() else {
            return Err(Error("cannot revert the root or a merge operation".into()));
        };
        let at_target = block(self.loader.load_at(&target)).map_err(err)?;
        let at_parent = block(self.loader.load_at(parent)).map_err(err)?;
        block(tx.repo_mut().merge(&at_target, &at_parent)).map_err(err)?;
        // `jj op revert` restores the repo and remote-tracking portions; git refs stay current.
        let current = tx.base_repo().view().store_view().clone();
        let mut view = tx.repo().view().store_view().clone();
        view.git_refs = current.git_refs;
        view.git_heads = current.git_heads;
        tx.repo_mut().set_view(view);
        Ok(())
    }

    /// Rewritten / created changes and new conflicts between `before` and the transaction.
    fn summarize(
        &self,
        before: &Arc<ReadonlyRepo>,
        after: &MutableRepo,
        created: Option<Commit>,
    ) -> Result<MutationOutcome> {
        let old_heads: Vec<JjCommitId> = before.view().heads().iter().cloned().collect();
        let new_heads: Vec<JjCommitId> = after.view().heads().iter().cloned().collect();
        let expr = ResolvedRevsetExpression::commits(new_heads)
            .ancestors()
            .minus(&ResolvedRevsetExpression::commits(old_heads).ancestors());
        let set = expr.evaluate(after).map_err(err)?;
        let new_ids: Vec<JjCommitId> = block(async {
            let mut out = Vec::new();
            let mut s = set.stream();
            while let Some(id) = s.next().await {
                out.push(id.map_err(err)?);
            }
            Ok::<_, Error>(out)
        })?;
        let mut rewritten = Vec::new();
        let mut new_commits = Vec::new();
        let mut new_conflicts = Vec::new();
        let mut divergent: BTreeSet<String> = BTreeSet::new();
        let visible = |repo: &dyn Repo, change: &jj_lib::backend::ChangeId| -> Result<usize> {
            Ok(block(repo.resolve_change_id(change))
                .map_err(err)?
                .map(|t| t.visible_with_offsets().count())
                .unwrap_or(0))
        };
        for id in new_ids.iter().rev() {
            let c = after.store().get_commit(id).map_err(err)?;
            let change = c.change_id().reverse_hex();
            new_commits.push((change.clone(), id.hex()));
            if !divergent.contains(&change)
                && visible(after, c.change_id())? > 1
                && visible(before.as_ref(), c.change_id())? <= 1
            {
                divergent.insert(change.clone());
            }
            let pred = block(before.resolve_change_id(c.change_id()))
                .map_err(err)?
                .and_then(|t| t.visible_with_offsets().next().map(|(_, id)| id.clone()))
                .and_then(|id| before.store().get_commit(&id).ok());
            if pred.is_some() {
                rewritten.push(change.clone());
            }
            let old: BTreeSet<String> = pred
                .as_ref()
                .map(|p| conflicts_of(p).into_iter().map(|(p, _)| p).collect())
                .unwrap_or_default();
            for (path, sides) in conflicts_of(&c) {
                if !old.contains(&path) {
                    new_conflicts.push(ConflictInfo {
                        change: change.clone(),
                        commit: id.hex(),
                        path,
                        sides,
                    });
                }
            }
        }
        Ok(MutationOutcome {
            op_before: before.op_id().hex(),
            op_after: None,
            created: created.map(|c| c.change_id().reverse_hex()),
            rewritten,
            new_commits,
            new_conflicts,
            workspaces: vec![],
            divergent: divergent.into_iter().collect(),
        })
    }

    /// Operations after `op` up to the current head, newest first (at most `limit`). Walks back
    /// from head and stops at `op`; on branches of concurrent operations that never reach it,
    /// stops at operations that ended before `op` started.
    pub fn ops_since(&self, op: &str, limit: usize) -> Result<Vec<OpInfo>> {
        let target_id =
            OperationId::try_from_hex(op).ok_or_else(|| Error(format!("bad op id {op}")))?;
        let target = block(self.loader.load_operation(&target_id)).map_err(err)?;
        let started = target.metadata().time.start.timestamp.0;
        let head = self.head()?;
        let mut out = Vec::new();
        let mut seen: BTreeSet<OperationId> = BTreeSet::new();
        let mut queue = std::collections::VecDeque::from([head.operation().clone()]);
        while let Some(o) = queue.pop_front() {
            if *o.id() == target_id || !seen.insert(o.id().clone()) || out.len() >= limit {
                continue;
            }
            let m = o.metadata();
            if m.time.end.timestamp.0 < started || o.parent_ids().is_empty() {
                continue;
            }
            out.push(OpInfo {
                id: o.id().hex(),
                description: m.description.clone(),
                time_ms: m.time.end.timestamp.0,
                user: format!("{}@{}", m.username, m.hostname),
                is_snapshot: m.is_snapshot,
                parents: o.parent_ids().iter().map(|p| p.hex()).collect(),
            });
            for p in block(o.parents()).map_err(err)? {
                queue.push_back(p);
            }
        }
        out.sort_by_key(|o| std::cmp::Reverse(o.time_ms));
        Ok(out)
    }

    /// Visible commits of each workspace's `@` change at head, for workspaces whose `@` change
    /// is divergent: (workspace, @ commit, other visible commits of the change).
    fn divergent_wcs(&self, names: &[String]) -> Result<Vec<(String, String, Vec<String>)>> {
        let repo = self.head()?;
        let mut out = Vec::new();
        for name in names {
            let ws_name = WorkspaceNameBuf::from(name.as_str());
            let Some(id) = repo.view().get_wc_commit_id(&ws_name).cloned() else {
                continue;
            };
            let c = repo.store().get_commit(&id).map_err(err)?;
            let others: Vec<String> = block(repo.resolve_change_id(c.change_id()))
                .map_err(err)?
                .map(|t| {
                    t.visible_with_offsets()
                        .map(|(_, x)| x.hex())
                        .filter(|x| *x != id.hex())
                        .collect()
                })
                .unwrap_or_default();
            if !others.is_empty() {
                out.push((name.clone(), id.hex(), others));
            }
        }
        Ok(out)
    }

    /// Brings a stale workspace to its `@` at head the way `jj workspace update-stale` does
    /// (DESIGN appendix A): files on disk that its last recorded `@` did not have are snapshotted
    /// into a rewrite of that old commit (a divergent sibling of the new `@`, so nothing is lost),
    /// then the new `@` is checked out. Returns (updated, the sibling holding the rescued files).
    fn recover_stale(&self, name: &str, root: &Path) -> Result<(bool, Option<String>)> {
        let mut ws = Workspace::load(
            &self.settings,
            root,
            &jj_lib::default_backend_factories::default_backend_factories(),
            &jj_lib::default_backend_factories::default_working_copy_factories(),
        )
        .map_err(err)?;
        let ws_name = WorkspaceNameBuf::from(name);
        let loader = ws.repo_loader().clone();
        let repo = block(loader.load_at_head()).map_err(err)?;
        let Some(desired_id) = repo.view().get_wc_commit_id(&ws_name).cloned() else {
            return Ok((false, None));
        };
        let desired = repo.store().get_commit(&desired_id).map_err(err)?;
        let mut locked = block(ws.start_working_copy_mutation()).map_err(err)?;
        match block(WorkingCopyFreshness::check_stale(
            locked.locked_wc(),
            &desired,
            &repo,
        ))
        .map_err(err)?
        {
            WorkingCopyFreshness::Fresh => return Ok((true, None)),
            WorkingCopyFreshness::Updated(_) => return Ok((false, None)),
            WorkingCopyFreshness::WorkingCopyStale | WorkingCopyFreshness::SiblingOperation => {}
        }
        // The `@` the files on disk were last recorded against.
        let wc_op = locked.locked_wc().old_operation_id().clone();
        let Ok(old_op) = block(loader.load_operation(&wc_op)) else {
            return Ok((false, None));
        };
        let old_repo = block(loader.load_at(&old_op)).map_err(err)?;
        let Some(stale_id) = old_repo.view().get_wc_commit_id(&ws_name).cloned() else {
            return Ok((false, None));
        };
        let stale = repo.store().get_commit(&stale_id).map_err(err)?;
        let options = SnapshotOptions {
            base_ignores: GitIgnoreFile::empty(),
            progress: None,
            start_tracking_matcher: &EverythingMatcher,
            force_tracking_matcher: &NothingMatcher,
            max_new_file_size: MAX_NEW_FILE_SIZE,
        };
        let (disk_tree, _stats) = block(locked.locked_wc().snapshot(&options)).map_err(err)?;
        let mut op = repo.op_id().clone();
        let mut recovered = None;
        if disk_tree.tree_ids_and_labels() != stale.tree().tree_ids_and_labels() {
            let mut tx = repo.start_transaction();
            tx.set_workspace_name(&ws_name);
            let sibling = block(
                tx.repo_mut()
                    .rewrite_commit(&stale)
                    .set_tree(disk_tree)
                    .write(),
            )
            .map_err(err)?;
            block(tx.repo_mut().rebase_descendants()).map_err(err)?;
            let new_repo = block(tx.commit(&format!(
                "cv: recover unsnapshotted files of workspace {name} into {}",
                &sibling.id().hex()[..12]
            )))
            .map_err(err)?;
            op = new_repo.op_id().clone();
            recovered = Some(sibling.id().hex());
        }
        block(locked.locked_wc().check_out(&desired)).map_err(err)?;
        block(locked.finish(op)).map_err(err)?;
        *self.cache.lock().unwrap() = None;
        Ok((true, recovered))
    }
}

/// `WorkspaceName` from a `&str` for callers outside jj-lib.
pub fn workspace_name(s: &str) -> WorkspaceNameBuf {
    WorkspaceNameBuf::from(s)
}

#[allow(dead_code)]
fn _assert_send_sync() {
    fn is<T: Send + Sync>() {}
    is::<JjWriter>();
    let _: Option<&WorkspaceName> = None;
}
