//! B8 `cv-review`: threads, anchoring, edits through the mutation queue, conflicts, events.

use crate::contracts::{Actor, EditRequest, EditResult, NewThread, ServerEvent, Thread};
use crate::ids::{ChangeId, Cursor, EditId, OpId, Result, ThreadId};

pub trait ReviewService: Send + Sync {
    fn create(&self, input: NewThread, actor: &Actor) -> Result<Thread>;
    fn reply(&self, thread: &ThreadId, body: &str, actor: &Actor, set_status: Option<&str>) -> Result<Thread>;
    fn submit_to_agent(&self, threads: &[ThreadId], note: Option<&str>, actor: &Actor) -> Result<Cursor>;
    /// After every op: follow rewritten changes through evolog and diff; move anchors or mark outdated.
    fn reanchor(&self, op: &OpId, rewritten: &[ChangeId]) -> Result<Vec<ThreadId>>;
}

pub trait EditService: Send + Sync {
    /// Validates (immutability, stale base) and returns an exact preview from a dropped jj-lib transaction.
    fn preview(&self, request: &EditRequest, actor: &Actor) -> Result<EditResult>;
    /// Queues the mutation; one at a time per repo.
    fn apply(&self, edit: &EditId, actor: &Actor) -> Result<EditResult>;
    fn undo(&self, edit: &EditId, actor: &Actor) -> Result<EditResult>;
    fn apply_suggestion(&self, thread: &ThreadId, actor: &Actor) -> Result<EditResult>;
}

/// One append-only log behind WS frames, GET /events and MCP wait_for_events.
pub trait EventLog: Send + Sync {
    fn append(&self, event: ServerEvent) -> Result<Cursor>;
    /// Events after `cursor`, blocking up to `timeout_ms` when there are none yet.
    fn read(&self, after: Option<Cursor>, kinds: Option<&[&str]>, timeout_ms: u64) -> Result<(Cursor, Vec<ServerEvent>)>;
}
