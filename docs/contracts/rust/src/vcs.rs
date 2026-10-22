//! B1 `cv-vcs`: the only crate that touches the repository. jj-lib pinned to the CLI's version.

use crate::contracts::{ConflictRecord, FileDiff, Revision};
use crate::ids::{BlobId, ChangeId, CommitId, OpId, RepoPath, Result, WorkspaceName};

pub trait Vcs: Send + Sync {
    fn head_op(&self) -> Result<OpId>;
    /// Calls `on_op` for every new operation (watcher on `.jj/repo/op_heads/heads`).
    fn watch_ops(&self, on_op: Box<dyn Fn(OpId) + Send + Sync>) -> Result<WatchGuard>;
    /// Changes whose commits differ between two operations; drives incremental re-indexing.
    fn op_diff(&self, from: &OpId, to: &OpId) -> Result<Vec<ChangeId>>;

    // Reads are pinned to an operation and never snapshot a working copy.
    fn log(&self, revset: &str, at: &OpId, limit: Option<usize>) -> Result<Vec<Revision>>;
    fn resolve(&self, revset: &str, at: &OpId) -> Result<Vec<CommitId>>;
    fn list_tree(&self, commit: &CommitId) -> Result<Vec<TreeEntry>>;
    fn read_blob(&self, path: &RepoPath, blob: &BlobId) -> Result<Vec<u8>>;
    /// Renders conflict markers for conflicted paths.
    fn read_file(&self, commit: &CommitId, path: &RepoPath) -> Result<String>;
    fn diff(&self, from: &CommitId, to: &CommitId, paths: Option<&[RepoPath]>, context: u32) -> Result<Vec<FileDiff>>;
    /// Newest first; the consumer stops pulling when its statistics settle.
    fn history(&self, head: &str, at: &OpId) -> Result<Box<dyn Iterator<Item = Result<HistoryCommit>> + Send>>;
    fn evolog(&self, change: &ChangeId, at: &OpId) -> Result<Vec<EvologEntry>>;
    fn interdiff(&self, from: &CommitId, to: &CommitId) -> Result<Vec<FileDiff>>;
    fn conflicts(&self, revset: &str, at: &OpId) -> Result<Vec<ConflictRecord>>;
    fn workspaces(&self, at: &OpId) -> Result<Vec<WorkspaceState>>;
    /// Files on disk that differ from the workspace's @ tree. Read-only.
    fn working_copy_overlay(&self, workspace: &WorkspaceName) -> Result<Vec<OverlayFile>>;

    /// Exact preview: rewrite and rebase inside a jj-lib transaction, then drop it.
    fn preview(&self, plan: &MutationPlan) -> Result<MutationOutcome>;
    /// Snapshot affected workspaces, commit the transaction, update their checkouts.
    fn mutate(&self, plan: &MutationPlan) -> Result<MutationOutcome>;
}

/// Dropping it stops the watcher.
pub struct WatchGuard;

pub struct TreeEntry {
    pub path: RepoPath,
    pub blob: BlobId,
    pub executable: bool,
    pub conflict: bool,
}

pub struct OverlayFile {
    pub path: RepoPath,
    /// None = deleted on disk.
    pub content: Option<Vec<u8>>,
}

pub struct WorkspaceState {
    pub name: WorkspaceName,
    pub root: std::path::PathBuf,
    pub wc_change: ChangeId,
    pub wc_commit: CommitId,
}

pub struct EvologEntry {
    pub commit: CommitId,
    pub op: OpId,
    pub op_description: String,
}

pub struct HistoryCommit {
    pub change: ChangeId,
    pub commit: CommitId,
    pub parents: Vec<CommitId>,
    pub committer_time_unix: i64,
    pub files: Vec<HistoryFile>,
}

pub struct HistoryFile {
    pub path: RepoPath,
    pub source: Option<RepoPath>,
    /// [old_start, old_len, new_start, new_len]
    pub hunks: Vec<[u32; 4]>,
    pub added: u32,
    pub removed: u32,
}

pub enum EditMode {
    /// Default whenever the target is mutable.
    Amend,
    InsertAfter,
    /// The only mode for immutable targets: nothing is rewritten.
    Branch,
    KeepDescendants,
}

pub enum MutationPlan {
    WriteFiles { target: CommitId, files: Vec<(RepoPath, Option<Vec<u8>>)>, mode: EditMode, message: Option<String> },
    Absorb { from: CommitId, into: String },
    Resolve { commit: CommitId, path: RepoPath, content: Vec<u8> },
    Revert { op: OpId },
}

pub struct MutationOutcome {
    pub op_before: OpId,
    /// None for a preview.
    pub op_after: Option<OpId>,
    pub created: Option<ChangeId>,
    pub rewritten: Vec<ChangeId>,
    pub new_conflicts: Vec<ConflictRecord>,
    pub workspaces: Vec<WorkspaceEffect>,
}

pub struct WorkspaceEffect {
    pub name: WorkspaceName,
    pub snapshotted: bool,
    pub updated: bool,
    /// Commit holding edits rescued from a divergent @, if that happened.
    pub recovered_into: Option<CommitId>,
}
