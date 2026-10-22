//! Identifiers and the shared error type.

macro_rules! id {
    ($($(#[$doc:meta])* $name:ident),* $(,)?) => {$(
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub String);
    )*};
}

id! {
    /// jj change id: stable across rewrites.
    ChangeId,
    /// jj/git commit id: changes on every rewrite.
    CommitId,
    /// jj operation id.
    OpId,
    /// Content-addressed blob id: the cache key for parse results and text.
    BlobId,
    /// Repo-relative path, forward slashes.
    RepoPath,
    WorkspaceName,
    /// Lane id from the lane rules ("infra", "config", "code", "test", "docs", or user-defined).
    LaneId,
    /// Revision-independent layout key ("<path>#<qualname>").
    WindowKey,
    WindowId,
    ViewId,
    ThreadId,
    EditId,
    /// "<protocol>:<normalized key>".
    HubId,
}

/// Monotonic position in the event log.
pub type Cursor = u64;

#[derive(Debug, Clone)]
pub enum Error {
    /// The target commit is immutable (jj `immutable_heads()`).
    Immutable(CommitId),
    /// The file moved on since the client read it (optimistic concurrency).
    StaleBase { path: RepoPath, expected: BlobId },
    InvalidRevset { message: String, hint: Option<String> },
    NotFound(String),
    Jj(String),
    Io(String),
}

pub type Result<T> = std::result::Result<T, Error>;
