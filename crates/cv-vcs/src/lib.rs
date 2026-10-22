//! B1 `cv-vcs`, read-only part: jj-lib 0.45.1 (the installed CLI's exact version).
//!
//! Every read is pinned to the operation the repo was loaded at, and nothing here snapshots a
//! working copy or writes an operation. jj-lib's API is async in places; we drive it from the
//! caller's (blocking) thread with `pollster`, as the stack spike verified.

use std::collections::{BinaryHeap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::AsyncReadExt;
use futures::StreamExt;
use jj_lib::backend::{CommitId as JjCommitId, FileId, TreeValue};
use jj_lib::commit::Commit;
use jj_lib::config::StackedConfig;
use jj_lib::conflicts;
use jj_lib::matchers::EverythingMatcher;
use jj_lib::object_id::ObjectId;
use jj_lib::ref_name::WorkspaceNameBuf;
use jj_lib::repo::{ReadonlyRepo, Repo};
use jj_lib::repo_path::RepoPath;
use jj_lib::revset::{
    self, RevsetAliasesMap, RevsetDiagnostics, RevsetExtensions, RevsetParseContext,
    RevsetWorkspaceContext, SymbolResolver, SymbolResolverExtension,
};
use jj_lib::settings::UserSettings;
use jj_lib::workspace::Workspace;

pub mod read;

/// A jj fileset (`jj diff <FILESETS>` syntax: `src/`, `"a b.py"`, `root-file:"x"`,
/// `glob:"**/*.py"`, `~`, `&`, `|`), parsed by jj-lib. Bare strings are paths relative to the
/// repo root (the server has no cwd). Empty text or `all()` matches everything.
pub struct Fileset(Option<Box<dyn jj_lib::matchers::Matcher>>);

impl Fileset {
    pub fn parse(text: Option<&str>) -> Result<Self> {
        let text = text.unwrap_or("").trim();
        if text.is_empty() {
            return Ok(Fileset(None));
        }
        let aliases = jj_lib::fileset::FilesetAliasesMap::new();
        let root = PathBuf::from("/");
        let path_converter = jj_lib::ui_path::RepoPathUiConverter::Fs {
            cwd: root.clone(),
            base: root,
        };
        let context = jj_lib::fileset::FilesetParseContext {
            aliases_map: &aliases,
            path_converter: &path_converter,
        };
        let mut diagnostics = jj_lib::fileset::FilesetDiagnostics::new();
        let expr = jj_lib::fileset::parse_maybe_bare(&mut diagnostics, text, &context)
            .map_err(|e| Error(format!("fileset {text:?}: {e}")))?;
        Ok(Fileset(Some(expr.to_matcher())))
    }

    pub fn matches(&self, path: &str) -> bool {
        match &self.0 {
            None => true,
            Some(m) => RepoPath::from_internal_string(path).is_ok_and(|p| m.matches(p)),
        }
    }
}
pub mod renames;

#[derive(Debug, Clone)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn err(e: impl std::fmt::Display) -> Error {
    Error(e.to_string())
}

/// One file of a tree. Symlinks, submodules and conflicted paths are reported with `conflict` or
/// skipped (symlinks/submodules), since the viewer only shows regular file content.
#[derive(Debug, Clone)]
pub struct TreeEntry {
    pub path: String,
    /// git blob id (hex). Content-addressed cache key.
    pub blob: String,
    pub executable: bool,
    pub conflict: bool,
}

/// Prefix of the synthetic blob id of a conflicted path (not a git blob id: never valid hex).
pub const CONFLICT_BLOB_PREFIX: &str = "conflict-";

/// Stable, distinct id of a conflicted path's materialized text: `conflict-<commit>-<path hash>`.
/// [`JjRepo::read_blob`] decodes it, so it needs no cache and works on any reader of the repo.
pub fn conflict_blob_id(commit: &str, path: &str) -> String {
    // FNV-1a: distinct per path, stable across processes.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    format!("{CONFLICT_BLOB_PREFIX}{commit}-{h:016x}")
}

#[derive(Debug, Clone)]
pub struct CommitInfo {
    /// Commit id, 40 hex.
    pub commit: String,
    /// Change id, reverse hex.
    pub change: String,
    pub parents: Vec<String>,
    pub committer_time_ms: i64,
    pub description: String,
}

/// One commit of `history()`: its time and the paths it changed against its (merged) parents.
#[derive(Debug, Clone)]
pub struct HistoryCommit {
    pub commit: String,
    pub change: String,
    pub committer_time_ms: i64,
    pub files: Vec<String>,
    /// (old path, new path) pairs detected by content ([`renames`]); both paths are in `files`.
    pub renames: Vec<(String, String)>,
}

/// A jj repo loaded read-only at its head operation.
pub struct JjRepo {
    repo: Arc<ReadonlyRepo>,
    workspace_name: WorkspaceNameBuf,
    workspace_root: PathBuf,
    /// The user's `ui.conflict-marker-style` (jj's default "diff"), for materialized conflicts.
    marker_style: conflicts::ConflictMarkerStyle,
}

impl JjRepo {
    /// Loads the workspace at `path` (or an ancestor) and pins the repo at the head operation.
    pub fn open(path: &Path) -> Result<Self> {
        let mut root = path.canonicalize().map_err(err)?;
        while !root.join(".jj").is_dir() {
            root = root
                .parent()
                .ok_or_else(|| Error(format!("{} is not inside a jj workspace", path.display())))?
                .to_path_buf();
        }
        let settings = UserSettings::from_config(StackedConfig::with_defaults()).map_err(err)?;
        let ws = Workspace::load(
            &settings,
            &root,
            &jj_lib::default_backend_factories::default_backend_factories(),
            &jj_lib::default_backend_factories::default_working_copy_factories(),
        )
        .map_err(err)?;
        let repo = pollster::block_on(ws.repo_loader().load_at_head()).map_err(err)?;
        Ok(JjRepo {
            repo,
            workspace_name: ws.workspace_name().to_owned(),
            workspace_root: root,
            marker_style: conflicts::ConflictMarkerStyle::Diff,
        }
        .with_user_marker_style())
    }

    pub fn root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn workspace_name(&self) -> &str {
        self.workspace_name.as_str()
    }

    /// The operation every read is pinned to.
    pub fn op_id(&self) -> String {
        self.repo.op_id().hex()
    }

    /// Resolves a revset (default aliases only; `@` is this workspace's working-copy commit).
    /// Commits come newest first.
    pub fn resolve(&self, revset_str: &str) -> Result<Vec<CommitInfo>> {
        let repo = self.repo.as_ref();
        let aliases = RevsetAliasesMap::new();
        let fileset_aliases = jj_lib::fileset::FilesetAliasesMap::new();
        let extensions = RevsetExtensions::default();
        let path_converter = jj_lib::ui_path::RepoPathUiConverter::Fs {
            cwd: self.workspace_root.clone(),
            base: self.workspace_root.clone(),
        };
        let context = RevsetParseContext {
            aliases_map: &aliases,
            local_variables: Default::default(),
            user_email: "",
            date_pattern_context: chrono_now(),
            default_ignored_remote: None,
            fileset_aliases_map: &fileset_aliases,
            extensions: &extensions,
            workspace: Some(RevsetWorkspaceContext {
                path_converter: &path_converter,
                workspace_name: &self.workspace_name,
            }),
        };
        let mut diagnostics = RevsetDiagnostics::new();
        let expr = revset::parse(&mut diagnostics, revset_str, &context).map_err(err)?;
        let no_ext: [Box<dyn SymbolResolverExtension>; 0] = [];
        let resolver = SymbolResolver::new(repo, &no_ext);
        let resolved = expr.resolve_user_expression(repo, &resolver).map_err(err)?;
        let set = resolved.evaluate(repo).map_err(err)?;
        let ids: Vec<JjCommitId> = pollster::block_on(async {
            let mut out = Vec::new();
            let mut s = set.stream();
            while let Some(id) = s.next().await {
                out.push(id.map_err(err)?);
            }
            Ok::<_, Error>(out)
        })?;
        ids.iter().map(|id| self.commit_info(id)).collect()
    }

    /// Resolves a revset that must name exactly one commit.
    pub fn resolve_one(&self, revset_str: &str) -> Result<CommitInfo> {
        let mut v = self.resolve(revset_str)?;
        match v.len() {
            1 => Ok(v.remove(0)),
            0 => Err(Error(format!(
                "revset {revset_str:?} resolved to no commits"
            ))),
            n => Err(Error(format!(
                "revset {revset_str:?} resolved to {n} commits, expected one"
            ))),
        }
    }

    fn get(&self, id: &JjCommitId) -> Result<Commit> {
        self.repo.store().get_commit(id).map_err(err)
    }

    fn parse_commit_id(hex: &str) -> Result<JjCommitId> {
        JjCommitId::try_from_hex(hex).ok_or_else(|| Error(format!("bad commit id {hex}")))
    }

    fn commit_info(&self, id: &JjCommitId) -> Result<CommitInfo> {
        let c = self.get(id)?;
        Ok(info(&c))
    }

    /// Every regular file of a commit's tree, in path order.
    pub fn list_tree(&self, commit: &str) -> Result<Vec<TreeEntry>> {
        let c = self.get(&Self::parse_commit_id(commit)?)?;
        let tree = c.tree();
        let mut out = Vec::new();
        for (path, value) in tree.entries() {
            let value = value.map_err(err)?;
            let path = path.as_internal_file_string().to_string();
            match value.as_resolved() {
                Some(Some(TreeValue::File { id, executable, .. })) => out.push(TreeEntry {
                    path,
                    blob: id.hex(),
                    executable: *executable,
                    conflict: false,
                }),
                Some(_) => {}
                None => {
                    // Conflicted: listed once, read back with jj's conflict markers through a
                    // synthetic id (see [`conflict_blob_id`]).
                    if let Some(Some(TreeValue::File { executable, .. })) = value
                        .adds()
                        .find(|v| matches!(v, Some(TreeValue::File { .. })))
                    {
                        out.push(TreeEntry {
                            blob: conflict_blob_id(commit, &path),
                            path,
                            executable: *executable,
                            conflict: true,
                        });
                    }
                }
            }
        }
        Ok(out)
    }

    /// The conflicted file `path` at `commit`, materialized with jj's markers in the user's
    /// `ui.conflict-marker-style` (default "diff") and the tree's conflict labels, as `jj` writes
    /// it to a working copy. `commit` may end in `^`: the merged parents' tree of that commit (the
    /// base side of a range diff). `None` when the path is not a conflict between files there.
    pub fn conflict_bytes(&self, commit: &str, path: &str) -> Result<Option<Vec<u8>>> {
        let tree = match commit.strip_suffix('^') {
            Some(c) => {
                let c = self.get(&Self::parse_commit_id(c)?)?;
                pollster::block_on(c.parent_tree(self.repo.as_ref())).map_err(err)?
            }
            None => self.get(&Self::parse_commit_id(commit)?)?.tree(),
        };
        let rp = RepoPath::from_internal_string(path).map_err(err)?;
        let value = pollster::block_on(tree.path_value(rp)).map_err(err)?;
        if value.is_resolved() {
            return Ok(None);
        }
        let Some(file) = pollster::block_on(conflicts::try_materialize_file_conflict_value(
            self.repo.store(),
            rp,
            &value,
            tree.labels(),
        ))
        .map_err(err)?
        else {
            return Ok(None);
        };
        let options = conflicts::ConflictMaterializeOptions {
            marker_style: self.marker_style,
            marker_len: None,
            merge: self.repo.store().merge_options().clone(),
        };
        Ok(Some(
            conflicts::materialize_merge_result_to_bytes(&file.contents, &file.labels, &options)
                .to_vec(),
        ))
    }

    /// The marker style conflicts are materialized with (`ui.conflict-marker-style`).
    pub fn marker_style(&self) -> conflicts::ConflictMarkerStyle {
        self.marker_style
    }

    /// Reads `ui.conflict-marker-style` from the user, repo and workspace config.
    pub(crate) fn with_user_marker_style(mut self) -> Self {
        self.marker_style = read::marker_style(&self.repo_path(), &self.workspace_root);
        self
    }

    /// Content of a file blob. Callable from many threads at once.
    /// A conflicted path's synthetic id from [`JjRepo::list_tree`] reads the materialized text.
    pub fn read_blob(&self, path: &str, blob: &str) -> Result<Vec<u8>> {
        if let Some(commit) = blob.strip_prefix(CONFLICT_BLOB_PREFIX) {
            let commit = commit.split('-').next().unwrap_or(commit);
            return self
                .conflict_bytes(commit, path)?
                .ok_or_else(|| Error(format!("{path} is not conflicted at {commit}")));
        }
        let id = FileId::try_from_hex(blob).ok_or_else(|| Error(format!("bad blob id {blob}")))?;
        let path = RepoPath::from_internal_string(path).map_err(err)?;
        pollster::block_on(async {
            let mut reader = self.repo.store().read_file(path, &id).await.map_err(err)?;
            let mut buf = Vec::new();
            reader.read_to_end(&mut buf).await.map_err(err)?;
            Ok(buf)
        })
    }

    /// Ancestors of `head` (inclusive), newest committer time first, computed lazily: each `next()`
    /// reads one commit and diffs it against its parents. The root commit is never yielded.
    pub fn history(&self, head: &str) -> Result<History<'_>> {
        let id = Self::parse_commit_id(head)?;
        let c = self.get(&id)?;
        let mut heap = BinaryHeap::new();
        let mut seen = HashSet::new();
        seen.insert(id.clone());
        heap.push(HeapItem(c.committer().timestamp.timestamp.0, id));
        Ok(History {
            repo: self,
            heap,
            seen,
        })
    }
}

fn chrono_now() -> jj_lib::time_util::DatePatternContext {
    jj_lib::time_util::DatePatternContext::from(chrono::Local::now())
}

fn info(c: &Commit) -> CommitInfo {
    CommitInfo {
        commit: c.id().hex(),
        change: c.change_id().reverse_hex(),
        parents: c.parent_ids().iter().map(|p| p.hex()).collect(),
        committer_time_ms: c.committer().timestamp.timestamp.0,
        description: c.description().to_string(),
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct HeapItem(i64, JjCommitId);

pub struct History<'a> {
    repo: &'a JjRepo,
    heap: BinaryHeap<HeapItem>,
    seen: HashSet<JjCommitId>,
}

impl History<'_> {
    /// Commits the walk has discovered so far (yielded or queued): the head, plus the parents of
    /// every commit yielded. For tests and diagnostics: a lazy walk discovers no further.
    pub fn discovered(&self) -> usize {
        self.seen.len()
    }

    fn step(&mut self, id: JjCommitId) -> Result<Option<HistoryCommit>> {
        let repo = self.repo.repo.as_ref();
        let root = repo.store().root_commit_id().clone();
        if id == root {
            return Ok(None);
        }
        let c = self.repo.get(&id)?;
        for p in c.parent_ids() {
            if *p != root && self.seen.insert(p.clone()) {
                let pc = self.repo.get(p)?;
                self.heap
                    .push(HeapItem(pc.committer().timestamp.timestamp.0, p.clone()));
            }
        }
        let (files, deleted, added) = pollster::block_on(async {
            let before = c.parent_tree(repo).await.map_err(err)?;
            let after = c.tree();
            let (mut files, mut deleted, mut added) = (Vec::new(), Vec::new(), Vec::new());
            let mut stream = before.diff_stream(&after, &EverythingMatcher);
            while let Some(entry) = stream.next().await {
                let is_file = |v: &jj_lib::backend::MergedTreeValue| {
                    v.adds().any(|t| matches!(t, Some(TreeValue::File { .. })))
                };
                if let Ok(diff) = &entry.values
                    && (is_file(&diff.before) || is_file(&diff.after))
                {
                    let path = entry.path.as_internal_file_string().to_string();
                    match (read::file_id(&diff.before), read::file_id(&diff.after)) {
                        (Some(b), None) => deleted.push((path.clone(), b)),
                        (None, Some(a)) => added.push((path.clone(), a)),
                        _ => {}
                    }
                    files.push(path);
                }
            }
            Ok::<_, Error>((files, deleted, added))
        })?;
        let renames = renames::detect(&deleted, &added, |p, b| self.repo.read_blob(p, b).ok());
        let i = info(&c);
        Ok(Some(HistoryCommit {
            commit: i.commit,
            change: i.change,
            committer_time_ms: i.committer_time_ms,
            files,
            renames,
        }))
    }
}

impl Iterator for History<'_> {
    type Item = Result<HistoryCommit>;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(HeapItem(_, id)) = self.heap.pop() {
            match self.step(id) {
                Ok(Some(c)) => return Some(Ok(c)),
                Ok(None) => continue,
                Err(e) => return Some(Err(e)),
            }
        }
        None
    }
}

pub mod write;
