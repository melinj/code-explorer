//! B8 `cv-review`: threads, anchoring, edits through the one mutation queue, conflict tracking and
//! the live event log, as one in-process service ([`Review`]) that the HTTP routes, the WebSocket
//! and cv-mcp all call.
//!
//! # In-process API (for cv-mcp and cv-server)
//!
//! Construct once per repo with [`Review::open`] (or [`Review::open_with`]); share it as
//! `Arc<Review>`. Every method is blocking (run it on a blocking thread from async code) and
//! returns [`ReviewError`] with an HTTP status and a stable `code`.
//!
//! | MCP tool | Method |
//! |---|---|
//! | `list_threads` | [`Review::list_threads`], [`Review::list_threads_in`] (revset) |
//! | `get_thread` | [`Review::get_thread`] |
//! | `create_thread` | [`Review::create_thread`] |
//! | `reply` | [`Review::reply`] (body + optional status) |
//! | status / assignee | [`Review::patch_thread`] |
//! | `submit_review` | [`Review::submit_review`] |
//! | `preview_edit` / `apply_edit` / `undo_edit` | [`Review::edit`] (dryRun), [`Review::apply_edit`], [`Review::undo_edit`] |
//! | `apply_suggestion` | [`Review::apply_suggestion`] |
//! | `list_conflicts` / `get_conflict` / `resolve_conflict` | [`Review::list_conflicts`], [`Review::conflict_detail`], [`Review::resolve_conflict`] |
//! | `wait_for_events` | [`EventLog::read`] via [`Review::events`] |
//! | `get_view_state` | [`Review::view_state`] |
//! | `agent.attention`, `focus.request` | [`Review::emit`] with the event |
//! | `bind_workspace` | [`Review::bind_session`] |
//!
//! After every repository change (our own mutations synchronously, external `jj` commands through
//! the op-head watcher, [`Review::start_watcher`]) [`Review::sync`] emits, in order:
//! `op.changed`, whatever the registered op hooks return (cv-server: `view.invalidated`),
//! `thread.upserted` for re-anchored threads, `conflicts.changed`, `workspace.updated`.

pub mod anchor;
pub mod events;
mod graph;
mod ops;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::Duration;

use cv_contracts::review::*;
use cv_store::{EditRow, IndexDb, ReviewDb, SessionRow, now_iso};
use cv_vcs::write::{EditMode, FileWrite, JjWriter, MutationOutcome, MutationPlan, WorkspaceAt};
use serde_json::{Value, json};

pub use cv_store::{StorePaths, ThreadFilter};
pub use events::EventLog;

#[derive(Debug, Clone)]
pub struct ReviewError {
    /// HTTP status to answer with.
    pub status: u16,
    /// Stable machine-readable code ("not-found", "invalid", "immutable", "stale-base", …).
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for ReviewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ReviewError {}

pub type Result<T> = std::result::Result<T, ReviewError>;

/// With `notify` watching `op_heads`, the op watcher still checks this often (a lost event is
/// picked up within this time).
pub const NOTIFY_SAFETY_POLL: Duration = Duration::from_secs(1);
/// After a change event: how long to let jj finish swapping op heads before syncing.
const NOTIFY_SETTLE: Duration = Duration::from_millis(15);

fn not_found(m: impl Into<String>) -> ReviewError {
    ReviewError {
        status: 404,
        code: "not-found",
        message: m.into(),
    }
}

fn invalid(m: impl Into<String>) -> ReviewError {
    ReviewError {
        status: 400,
        code: "invalid",
        message: m.into(),
    }
}

fn jj_error(e: impl std::fmt::Display) -> ReviewError {
    ReviewError {
        status: 500,
        code: "jj-error",
        message: e.to_string(),
    }
}

fn store_error(e: cv_store::Error) -> ReviewError {
    ReviewError {
        status: 500,
        code: "store-error",
        message: e.0,
    }
}

/// What changed in one sync step; passed to op hooks.
#[derive(Debug, Clone)]
pub struct OpChange {
    pub from_op: String,
    pub to_op: String,
    /// Changes with a new visible commit.
    pub changes: Vec<String>,
    /// Changes no longer visible.
    pub abandoned: Vec<String>,
    pub actor: Actor,
}

/// Called after every new operation, before threads are re-anchored; returns events to emit
/// (cv-server re-indexes changed files and returns `view.invalidated`).
pub type OpHook = Box<dyn Fn(&OpChange) -> Vec<ServerEvent> + Send + Sync>;

/// Builds the full `WorkspaceSummary` of one workspace (cv-server: stack overlaps, unsnapshotted
/// files, ISO times); `None` falls back to [`Review::workspace_summary`]'s own minimal card.
/// Called while the sync lock is held: it must not call [`Review::sync`].
pub type WorkspaceSummarizer = Box<dyn Fn(&str) -> Option<Value> + Send + Sync>;

/// A rejected or failed edit, before it becomes an [`EditResult`].
struct Rejection {
    code: &'static str,
    message: String,
}

fn reject(code: &'static str, message: impl Into<String>) -> Rejection {
    Rejection {
        code,
        message: message.into(),
    }
}

/// How long a mutation waits for the queue before giving up with `locked` (DESIGN §6; model.ts
/// `EditErrorCode` "locked": another mutation holds the queue longer than the client timeout).
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// How many later operations undo inspects before giving up on listing them.
const LATER_OPS_LIMIT: usize = 200;
/// Evolution entries walked to re-anchor a thread through the evolog (b11).
const EVOLOG_LIMIT: usize = 64;

/// An [`EditRequest`] resolved against the current head.
struct Prepared {
    plan: MutationPlan,
    target_change: String,
    /// For edits that answer a thread: where its anchored lines are in the edited file.
    anchor_after: Option<AnchorAfter>,
}

struct StagedAnchor {
    thread: String,
    change: String,
    commit: String,
    start: u32,
    end: u32,
    fingerprint: Fingerprint,
}

/// A thread's anchor carried through the edit that answers it (a1): the lines in the edited text
/// that replaced or hold the anchored ones. After the apply the anchor moves to the commit that
/// holds the edit, so the next operation does not find the old lines gone and outdate it.
struct AnchorAfter {
    thread: String,
    path: String,
    start: u32,
    end: u32,
    /// The text the edit wrote (before any three-way merge with a moved-on file).
    text: String,
}

#[derive(Default)]
struct State {
    last_op: Option<String>,
    /// workspace → @ commit, as of `last_op`.
    wc: BTreeMap<String, String>,
    /// session → (time, ViewState JSON).
    view_states: HashMap<String, (String, Value)>,
    latest_view_state: Option<(String, String, Value)>,
    /// Anchors moved by an applied edit that answers their thread (a1), written by the sync that
    /// follows the edit (after its op.changed, like any re-anchoring).
    staged_anchors: Vec<StagedAnchor>,
    /// edit → fingerprint of the plan its preview ran (apply re-checks against it).
    previews: HashMap<String, u64>,
}

pub struct Review {
    vcs: JjWriter,
    db: Arc<ReviewDb>,
    index: Option<IndexDb>,
    events: EventLog,
    state: Mutex<State>,
    /// Held across a mutation and the sync that follows, so the watcher never claims our op.
    sync_lock: Mutex<()>,
    hooks: RwLock<Vec<OpHook>>,
    summarizer: RwLock<Option<WorkspaceSummarizer>>,
    session: String,
    seq: AtomicU64,
    stop: Arc<AtomicBool>,
    /// Set once the op watcher watches `op_heads` (or polls) and has made its first check.
    watching: Arc<AtomicBool>,
    /// Milliseconds a mutation waits for the queue ([`DEFAULT_LOCK_TIMEOUT`]).
    lock_timeout_ms: AtomicU64,
}

fn suggestion_of(body: &str) -> Option<String> {
    let start = body.find("```suggestion")?;
    let rest = &body[start + "```suggestion".len()..];
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))?;
    let end = rest.find("```")?;
    let text = &rest[..end];
    Some(text.strip_suffix('\n').unwrap_or(text).to_string())
}

fn short(id: &str) -> &str {
    &id[..id.len().min(12)]
}

impl Review {
    /// Opens the stores at their DESIGN §3 locations and the repo of the workspace at `root`.
    pub fn open(root: &Path) -> Result<Arc<Self>> {
        let vcs = JjWriter::open(root).map_err(jj_error)?;
        let paths = StorePaths::for_repo(vcs.root());
        Self::open_with(vcs, Some(paths))
    }

    /// `paths: None` keeps review.db in memory (tests).
    pub fn open_with(vcs: JjWriter, paths: Option<StorePaths>) -> Result<Arc<Self>> {
        let (db, index) = match &paths {
            Some(p) => (
                ReviewDb::open(&p.review_db).map_err(store_error)?,
                IndexDb::open(&p.index_db).ok(),
            ),
            None => (ReviewDb::in_memory().map_err(store_error)?, None),
        };
        let db = Arc::new(db);
        let r = Arc::new(Review {
            events: EventLog::new(db.clone()),
            vcs,
            db,
            index,
            state: Mutex::new(State::default()),
            sync_lock: Mutex::new(()),
            hooks: RwLock::new(Vec::new()),
            summarizer: RwLock::new(None),
            session: format!("s{:x}", now_ms()),
            seq: AtomicU64::new(0),
            stop: Arc::new(AtomicBool::new(false)),
            watching: Arc::new(AtomicBool::new(false)),
            lock_timeout_ms: AtomicU64::new(DEFAULT_LOCK_TIMEOUT.as_millis() as u64),
        });
        r.sync()?;
        Ok(r)
    }

    pub fn vcs(&self) -> &JjWriter {
        &self.vcs
    }

    pub fn events(&self) -> &EventLog {
        &self.events
    }

    pub fn db(&self) -> &ReviewDb {
        &self.db
    }

    /// Server session id (WS `hello.session`).
    pub fn session(&self) -> &str {
        &self.session
    }

    /// The operation the service last synced to.
    pub fn op(&self) -> String {
        self.state
            .lock()
            .unwrap()
            .last_op
            .clone()
            .unwrap_or_default()
    }

    /// Appends any event (agent.attention, focus.request, …) to the log.
    pub fn emit(&self, event: ServerEvent) -> u64 {
        self.events.append(event)
    }

    pub fn add_op_hook(&self, hook: OpHook) {
        self.hooks.write().unwrap().push(hook);
    }

    /// How long apply / undo / resolve wait for the mutation queue before answering 409
    /// `locked` (default [`DEFAULT_LOCK_TIMEOUT`]).
    pub fn set_lock_timeout(&self, d: Duration) {
        self.lock_timeout_ms
            .store(d.as_millis() as u64, Ordering::Relaxed);
    }

    /// Takes the mutation queue (the sync lock), waiting at most the lock timeout.
    fn queue(&self) -> Result<std::sync::MutexGuard<'_, ()>> {
        let wait = Duration::from_millis(self.lock_timeout_ms.load(Ordering::Relaxed));
        let deadline = std::time::Instant::now() + wait;
        loop {
            match self.sync_lock.try_lock() {
                Ok(g) => return Ok(g),
                Err(std::sync::TryLockError::Poisoned(p)) => return Ok(p.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) => {
                    if std::time::Instant::now() >= deadline {
                        return Err(ReviewError {
                            status: 409,
                            code: "locked",
                            message: format!(
                                "another mutation has held the queue for more than {:.1} s; nothing was done, retry when it finishes",
                                wait.as_secs_f64()
                            ),
                        });
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }

    /// Installs the builder of full workspace cards for `workspace.updated` (cv-server).
    pub fn set_workspace_summarizer(&self, f: WorkspaceSummarizer) {
        *self.summarizer.write().unwrap() = Some(f);
    }

    fn new_id(&self, prefix: &str) -> String {
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}{:x}{:03x}", now_ms(), n % 4096)
    }

    // ── View state (shared attention) ──

    /// Latest `view.state` of a WS session (throttled to 4 Hz by the client).
    pub fn set_view_state(&self, session: &str, state: Value) {
        let at = now_iso();
        let mut s = self.state.lock().unwrap();
        s.view_states
            .insert(session.to_string(), (at.clone(), state.clone()));
        s.latest_view_state = Some((session.to_string(), at, state));
    }

    /// A WS session closed: it no longer counts as a UI following the agent (the latest state
    /// stays readable for `get_view_state`).
    pub fn clear_view_state(&self, session: &str) {
        self.state.lock().unwrap().view_states.remove(session);
    }

    /// Whether a connected UI follows the agent (its last `view.state` has `followAgent: true`).
    pub fn following(&self) -> bool {
        self.state
            .lock()
            .unwrap()
            .view_states
            .values()
            .any(|(_, v)| v["followAgent"].as_bool() == Some(true))
    }

    /// What the human is looking at: the given session's last view state, or the most recent of
    /// any session. Returns (session, time, ViewState).
    pub fn view_state(&self, session: Option<&str>) -> Option<(String, String, Value)> {
        let s = self.state.lock().unwrap();
        match session {
            Some(id) => s
                .view_states
                .get(id)
                .map(|(at, v)| (id.to_string(), at.clone(), v.clone())),
            None => s.latest_view_state.clone(),
        }
    }

    /// MCP `bind_workspace`: records the agent session (attribution, presence on workspace cards)
    /// and emits `workspace.updated` for that workspace.
    pub fn bind_session(
        &self,
        session_id: &str,
        workspace: &str,
        agent: &Actor,
        activity: &str,
    ) -> Result<()> {
        let now = now_iso();
        let bound_at = self
            .db
            .sessions()
            .map_err(store_error)?
            .into_iter()
            .find(|s| s.session_id == session_id)
            .map(|s| s.bound_at)
            .unwrap_or_else(|| now.clone());
        self.db
            .put_session(&SessionRow {
                session_id: session_id.into(),
                workspace: workspace.into(),
                agent: agent.clone(),
                bound_at,
                last_seen: now,
                activity: activity.into(),
            })
            .map_err(store_error)?;
        if let Some(w) = self
            .vcs
            .workspaces()
            .map_err(jj_error)?
            .into_iter()
            .find(|w| w.name == workspace)
        {
            let summary = self.workspace_summary(&w);
            self.emit(ServerEvent::WorkspaceUpdated { workspace: summary });
        }
        Ok(())
    }

    // ── Threads ──

    pub fn list_threads(&self, filter: &ThreadFilter) -> Result<Vec<Thread>> {
        self.db.list_threads(filter).map_err(store_error)
    }

    /// Threads anchored to the changes of a revset (GET /threads?revset=).
    pub fn list_threads_in(&self, revset: &str, mut filter: ThreadFilter) -> Result<Vec<Thread>> {
        let changes = self
            .vcs
            .resolve(revset)
            .map_err(|e| ReviewError {
                status: 400,
                code: "revset-invalid",
                message: e.0,
            })?
            .into_iter()
            .map(|c| c.change)
            .collect();
        filter.changes = Some(changes);
        self.list_threads(&filter)
    }

    pub fn get_thread(&self, id: &str) -> Result<Thread> {
        self.db
            .get_thread(id)
            .map_err(store_error)?
            .ok_or_else(|| not_found(format!("unknown thread {id}")))
    }

    fn put_thread(&self, t: &Thread) -> Result<()> {
        self.db.put_thread(t).map_err(store_error)?;
        // Re-read so producedEdits reflects the edit table.
        let t = self.get_thread(&t.id)?;
        self.emit(ServerEvent::ThreadUpserted {
            thread: Box::new(t),
        });
        Ok(())
    }

    pub fn create_thread(&self, input: NewThread, actor: &Actor) -> Result<Thread> {
        let a = &input.anchor;
        let kind = input.kind.clone().unwrap_or_else(|| "comment".into());
        if !THREAD_KINDS.contains(&kind.as_str()) {
            return Err(invalid(format!("unknown thread kind {kind:?}")));
        }
        if let Some(s) = &input.severity
            && !SEVERITIES.contains(&s.as_str())
        {
            return Err(invalid(format!("unknown severity {s:?}")));
        }
        if let Some(s) = &input.assignee
            && s != "agent"
            && s != "human"
        {
            return Err(invalid("assignee is \"agent\" or \"human\""));
        }
        let side = a.side.clone().unwrap_or_else(|| "new".into());
        if side != "new" && side != "old" {
            return Err(invalid("side is \"new\" or \"old\""));
        }
        if a.start == 0 {
            return Err(invalid("lines are 1-based"));
        }
        let end = a.end.unwrap_or(a.start).max(a.start);
        let rev = self.vcs.resolve_rev(&a.rev).map_err(|e| ReviewError {
            status: 400,
            code: "invalid-target",
            message: e.0,
        })?;
        // The "old" side of a change is its (first) parent's file.
        let text_commit = if side == "old" {
            rev.parents
                .first()
                .cloned()
                .unwrap_or_else(|| rev.commit.clone())
        } else {
            rev.commit.clone()
        };
        let text = self
            .vcs
            .file_at(&text_commit, &a.path)
            .map_err(jj_error)?
            .map(|f| String::from_utf8_lossy(&f.bytes).into_owned())
            .ok_or_else(|| {
                invalid(format!(
                    "{} does not exist at {}",
                    a.path,
                    short(&text_commit)
                ))
            })?;
        let lines = anchor::split(&text);
        if end as usize > lines.len().max(1) {
            return Err(invalid(format!(
                "lines {}–{end} are outside {} ({} lines)",
                a.start,
                a.path,
                lines.len()
            )));
        }
        let now = now_iso();
        let t = Thread {
            id: self.new_id("t"),
            anchor: Anchor {
                change: rev.change.clone(),
                commit: rev.commit.clone(),
                path: a.path.clone(),
                side,
                start: a.start,
                end,
                cols: a.cols,
                fingerprint: anchor::fingerprint(&lines, a.start, end),
            },
            status: "open".into(),
            kind,
            severity: input.severity.clone(),
            assignee: input.assignee.clone(),
            created_by: actor.clone(),
            created_at: now.clone(),
            updated_at: now.clone(),
            comments: vec![Comment {
                id: self.new_id("cm"),
                author: actor.clone(),
                body: input.body.clone(),
                created_at: now,
                edited_at: None,
            }],
            produced_edits: vec![],
            outdated: None,
        };
        self.put_thread(&t)?;
        self.get_thread(&t.id)
    }

    /// Adds a comment (when `body` is not empty) and optionally moves the thread to `set_status`.
    pub fn reply(
        &self,
        id: &str,
        body: &str,
        set_status: Option<&str>,
        actor: &Actor,
    ) -> Result<Thread> {
        let mut t = self.get_thread(id)?;
        if let Some(s) = set_status {
            if !THREAD_STATUSES.contains(&s) {
                return Err(invalid(format!("unknown status {s:?}")));
            }
            if s != "outdated" && t.outdated.is_some() {
                self.refresh_anchor(&mut t)?;
                t.outdated = None;
            }
            t.status = s.to_string();
        }
        let now = now_iso();
        if !body.trim().is_empty() {
            t.comments.push(Comment {
                id: self.new_id("cm"),
                author: actor.clone(),
                body: body.to_string(),
                created_at: now.clone(),
                edited_at: None,
            });
        }
        t.updated_at = now;
        self.put_thread(&t)?;
        self.get_thread(id)
    }

    /// PATCH /threads/:id. `assignee: Some(None)` clears the assignee.
    pub fn patch_thread(
        &self,
        id: &str,
        status: Option<&str>,
        assignee: Option<Option<&str>>,
        _actor: &Actor,
    ) -> Result<Thread> {
        let mut t = self.get_thread(id)?;
        if let Some(s) = status {
            if !THREAD_STATUSES.contains(&s) {
                return Err(invalid(format!("unknown status {s:?}")));
            }
            if s != "outdated" && t.outdated.is_some() {
                self.refresh_anchor(&mut t)?;
                t.outdated = None;
            }
            t.status = s.into();
        }
        if let Some(a) = assignee {
            if a.is_some_and(|a| a != "agent" && a != "human") {
                return Err(invalid("assignee is \"agent\", \"human\" or null"));
            }
            t.assignee = a.map(String::from);
        }
        t.updated_at = now_iso();
        self.put_thread(&t)?;
        self.get_thread(id)
    }

    /// "Send to agent": assigns the threads to the agent and emits `review.submitted`.
    /// Returns that event's cursor.
    pub fn submit_review(
        &self,
        threads: &[String],
        note: Option<&str>,
        actor: &Actor,
    ) -> Result<u64> {
        for id in threads {
            let mut t = self.get_thread(id)?;
            if t.assignee.as_deref() != Some("agent") {
                t.assignee = Some("agent".into());
                t.updated_at = now_iso();
                self.put_thread(&t)?;
            }
        }
        Ok(self.emit(ServerEvent::ReviewSubmitted {
            threads: threads.to_vec(),
            by: actor.clone(),
            note: note.map(String::from),
        }))
    }

    pub fn mark_reviewed(&self, change: &str, commit: &str, reviewer: &str) -> Result<()> {
        self.db
            .set_review_mark(change, reviewer, commit)
            .map_err(store_error)?;
        self.emit(ServerEvent::ReviewMarked {
            change: change.to_string(),
            commit: commit.to_string(),
            reviewer: reviewer.to_string(),
        });
        Ok(())
    }

    // ── Edits ──

    fn prepare(&self, req: &EditRequest) -> std::result::Result<Prepared, Rejection> {
        if !EDIT_MODES.contains(&req.mode.as_str()) {
            return Err(reject(
                "invalid-target",
                format!("unknown mode {:?}", req.mode),
            ));
        }
        if req.files.is_empty() {
            return Err(reject("invalid-target", "an edit needs at least one file"));
        }
        let target = self
            .vcs
            .resolve_rev(&req.target)
            .map_err(|e| reject("invalid-target", e.0))?;
        let immutable = self
            .vcs
            .is_immutable(&target.commit)
            .map_err(|e| reject("jj-error", e.0))?;
        if immutable && req.mode != "branch" {
            return Err(reject(
                "immutable",
                format!(
                    "{} is immutable: nothing is rewritten there; use mode \"branch\" to start a new change on top of it",
                    short(&target.change)
                ),
            ));
        }
        let mut files = Vec::new();
        let mut anchor_after = None;
        for f in &req.files {
            let cur = self
                .vcs
                .file_at(&target.commit, &f.path)
                .map_err(|e| reject("jj-error", e.0))?;
            let cur_blob = cur.as_ref().and_then(|c| c.blob.clone());
            let base: Option<Vec<u8>> = match &f.base_blob {
                Some(b) if Some(b) != cur_blob.as_ref() => Some(
                    self.vcs
                        .read_blob(&f.path, b)
                        .map_err(|_| reject("stale-base", format!("unknown base blob {b}")))?,
                ),
                _ => None,
            };
            let start_text = base
                .clone()
                .or_else(|| cur.as_ref().map(|c| c.bytes.clone()))
                .unwrap_or_default();
            let new: Option<Vec<u8>> = if let Some(c) = &f.content {
                Some(c.clone().into_bytes())
            } else if let Some(ps) = &f.patches {
                let s = String::from_utf8_lossy(&start_text);
                let out = anchor::apply_patches(&s, ps).map_err(|e| reject("invalid-target", e))?;
                Some(out.into_bytes())
            } else {
                None
            };
            if anchor_after.is_none()
                && let (Some(thread), Some(new)) = (
                    req.origin.as_ref().and_then(|o| o.thread.as_deref()),
                    new.as_ref(),
                )
            {
                anchor_after = self.anchor_through_edit(
                    thread,
                    &f.path,
                    &start_text,
                    new,
                    f.patches.as_deref(),
                );
            }
            // Optimistic concurrency: the file moved on since baseBlob. Rebase the edit onto it
            // with a three-way merge; only overlapping changes are stale.
            let new = match (&base, new) {
                (Some(base), Some(ours)) => {
                    let Some(theirs) = cur
                        .as_ref()
                        .filter(|c| !c.conflict)
                        .map(|c| c.bytes.clone())
                    else {
                        return Err(reject(
                            "stale-base",
                            format!(
                                "{} changed since it was read (now absent or conflicted)",
                                f.path
                            ),
                        ));
                    };
                    Some(self.vcs.merge3(base, &ours, &theirs).ok_or_else(|| {
                        reject(
                            "stale-base",
                            format!(
                                "{} changed since it was read and the edit overlaps those changes; reload and retry",
                                f.path
                            ),
                        )
                    })?)
                }
                (_, new) => new,
            };
            files.push(FileWrite {
                path: f.path.clone(),
                content: new,
            });
        }
        let plan = if req.mode == "absorb" {
            MutationPlan::Absorb {
                tip: target.commit.clone(),
                files,
                into: req
                    .absorb_into
                    .clone()
                    .unwrap_or_else(|| "mutable()".into()),
            }
        } else {
            let mode = match req.mode.as_str() {
                "insert-after" => EditMode::InsertAfter,
                "branch" => EditMode::Branch,
                "keep-descendants" => EditMode::KeepDescendants,
                _ => EditMode::Amend,
            };
            MutationPlan::WriteFiles {
                target: target.commit.clone(),
                files,
                mode,
                message: req.message.clone(),
            }
        };
        Ok(Prepared {
            plan,
            target_change: target.change,
            anchor_after,
        })
    }

    fn result_from(
        &self,
        id: &str,
        status: &str,
        actor: &Actor,
        out: &MutationOutcome,
        predicted: bool,
    ) -> EditResult {
        let first_seen = out
            .op_after
            .clone()
            .unwrap_or_else(|| out.op_before.clone());
        EditResult {
            id: id.to_string(),
            status: status.into(),
            actor: actor.clone(),
            op_before: Some(out.op_before.clone()),
            op_after: out.op_after.clone(),
            created: out.created.clone(),
            rewritten: out.rewritten.clone(),
            workspaces: out
                .workspaces
                .iter()
                .map(|w| EditWorkspace {
                    name: w.name.clone(),
                    snapshotted: w.snapshotted,
                    updated: w.updated,
                    recovered_into: w.recovered_into.clone(),
                })
                .collect(),
            conflicts: out
                .new_conflicts
                .iter()
                .map(|c| ConflictRecord {
                    change: c.change.clone(),
                    commit: c.commit.clone(),
                    path: c.path.clone(),
                    sides: c.sides,
                    first_seen_op: first_seen.clone(),
                    introduced_by: Some(id.to_string()),
                })
                .collect(),
            predicted,
            error: None,
            files: vec![],
            warnings: vec![],
            divergent: out.divergent.clone(),
        }
    }

    /// Each edited file's blob in the commit that holds an applied edit: the created change, else
    /// the rewritten target. Empty for absorb (several commits) or when it cannot be read.
    fn edited_files(
        &self,
        out: &MutationOutcome,
        target_change: &str,
        req: &EditRequest,
    ) -> Vec<EditedFile> {
        if req.mode == "absorb" {
            return vec![];
        }
        let holder = out.created.as_deref().unwrap_or(target_change);
        let Some((_, commit)) = out.new_commits.iter().find(|(c, _)| c == holder) else {
            return vec![];
        };
        req.files
            .iter()
            .map(|f| EditedFile {
                path: f.path.clone(),
                new_blob: self
                    .vcs
                    .file_at(commit, &f.path)
                    .ok()
                    .flatten()
                    .and_then(|x| x.blob),
            })
            .collect()
    }

    fn failed(&self, id: &str, status: &str, actor: &Actor, r: Rejection) -> EditResult {
        EditResult {
            id: id.into(),
            status: status.into(),
            actor: actor.clone(),
            op_before: None,
            op_after: None,
            created: None,
            rewritten: vec![],
            workspaces: vec![],
            conflicts: vec![],
            predicted: false,
            error: Some(EditError {
                code: r.code.into(),
                message: r.message,
            }),
            files: vec![],
            warnings: vec![],
            divergent: vec![],
        }
    }

    fn store_edit(&self, req: &EditRequest, result: &EditResult) -> Result<()> {
        let thread = req
            .origin
            .as_ref()
            .and_then(|o| o.thread.clone())
            .filter(|t| self.db.get_thread(t).ok().flatten().is_some());
        self.db
            .put_edit(&EditRow {
                request: req.clone(),
                result: result.clone(),
                thread,
            })
            .map_err(store_error)?;
        self.emit(ServerEvent::EditUpdated {
            edit: Box::new(result.clone()),
        });
        Ok(())
    }

    /// POST /edits: an exact preview (status "previewed"; nothing written). With `dryRun: false`
    /// it is applied right away. Validation failures come back as status "rejected" with an error
    /// code ("immutable", "stale-base", "invalid-target").
    pub fn edit(&self, req: EditRequest, actor: &Actor) -> Result<EditResult> {
        let id = self.new_id("e");
        let result = match self.prepare(&req) {
            Err(r) => self.failed(&id, "rejected", actor, r),
            Ok(p) => match self.vcs.preview(&p.plan) {
                Ok(out) => {
                    self.state
                        .lock()
                        .unwrap()
                        .previews
                        .insert(id.clone(), plan_fingerprint(&p.plan));
                    let mut r = self.result_from(&id, "previewed", actor, &out, true);
                    r.warnings = self.agent_warnings(&r.workspaces, actor);
                    r
                }
                Err(e) => self.failed(&id, "failed", actor, reject("jj-error", e.0)),
            },
        };
        self.store_edit(&req, &result)?;
        if !req.dry_run && result.status == "previewed" {
            return self.apply_edit(&id, actor);
        }
        Ok(result)
    }

    /// Applies a previewed edit through the mutation queue (one at a time per repo).
    pub fn apply_edit(&self, id: &str, actor: &Actor) -> Result<EditResult> {
        let _sync = self.queue()?;
        self.sync_locked(None)?;
        let row = self
            .db
            .get_edit(id)
            .map_err(store_error)?
            .ok_or_else(|| not_found(format!("unknown edit {id}")))?;
        match row.result.status.as_str() {
            "applied" => return Ok(row.result),
            "previewed" => {}
            s => {
                return Err(ReviewError {
                    status: 409,
                    code: "invalid",
                    message: format!("edit {id} is {s}; preview it again"),
                });
            }
        }
        let result = match self
            .prepare(&row.request)
            .and_then(|p| self.recheck(id, &row.result, p))
        {
            Err(r) => self.failed(id, "rejected", actor, r),
            Ok(p) => {
                let desc = format!(
                    "cv: {} {} ({})",
                    row.request.mode,
                    short(&p.target_change),
                    actor.label()
                );
                match self.vcs.mutate(&p.plan, &desc) {
                    Ok(out) => {
                        if let Some(a) = &p.anchor_after
                            && row.request.mode != "absorb"
                        {
                            let holder = out.created.as_deref().unwrap_or(&p.target_change);
                            if let Some((change, commit)) =
                                out.new_commits.iter().find(|(c, _)| c == holder)
                                && let Err(e) = self.stage_anchor(a, change, commit)
                            {
                                eprintln!(
                                    "cv: thread {} keeps its anchor: {}",
                                    a.thread, e.message
                                );
                            }
                        }
                        let mut r = self.result_from(id, "applied", actor, &out, false);
                        r.files = self.edited_files(&out, &p.target_change, &row.request);
                        r.warnings = row.result.warnings.clone();
                        r
                    }
                    Err(e) => self.failed(id, "failed", actor, reject("jj-error", e.0)),
                }
            }
        };
        let result = self.note_recoveries(result);
        self.state.lock().unwrap().previews.remove(id);
        self.store_edit(&row.request, &result)?;
        if result.status == "applied" {
            self.sync_locked(Some((actor, Some(id))))?;
        }
        self.emit_recoveries(&result);
        Ok(result)
    }

    /// Apply never silently runs something other than what was previewed (DESIGN §6). When the
    /// repository moved on since the preview, the plan is previewed again and must do the same
    /// thing: same writes, same rewritten changes, same new conflicts, same affected workspaces.
    fn recheck(
        &self,
        id: &str,
        previewed: &EditResult,
        p: Prepared,
    ) -> std::result::Result<Prepared, Rejection> {
        let head = self.op();
        if previewed.op_before.as_deref() == Some(head.as_str()) {
            return Ok(p);
        }
        let stale = |what: String| {
            reject(
                "stale-base",
                format!(
                    "the repository moved on since edit {id} was previewed (op {} → {}) and applying it now would {what}; preview it again",
                    short(previewed.op_before.as_deref().unwrap_or("?")),
                    short(&head)
                ),
            )
        };
        if let Some(fp) = self.state.lock().unwrap().previews.get(id)
            && *fp != plan_fingerprint(&p.plan)
        {
            return Err(stale(
                "write different content or target another commit".into(),
            ));
        }
        let now = self
            .vcs
            .preview(&p.plan)
            .map_err(|e| reject("jj-error", e.0))?;
        let set = |v: &[String]| v.iter().cloned().collect::<BTreeSet<String>>();
        if set(&now.rewritten) != set(&previewed.rewritten) {
            return Err(stale(format!(
                "rewrite {:?} instead of the previewed {:?}",
                now.rewritten.iter().map(|c| short(c)).collect::<Vec<_>>(),
                previewed
                    .rewritten
                    .iter()
                    .map(|c| short(c))
                    .collect::<Vec<_>>()
            )));
        }
        if now.created.is_some() != previewed.created.is_some() {
            return Err(stale("create a different set of changes".into()));
        }
        let conflicts = |v: Vec<(String, String)>| v.into_iter().collect::<BTreeSet<_>>();
        let now_c = conflicts(
            now.new_conflicts
                .iter()
                .map(|c| (c.change.clone(), c.path.clone()))
                .collect(),
        );
        let was_c = conflicts(
            previewed
                .conflicts
                .iter()
                .map(|c| (c.change.clone(), c.path.clone()))
                .collect(),
        );
        if now_c != was_c {
            return Err(stale(format!(
                "leave {} new conflict(s) instead of the previewed {}",
                now_c.len(),
                was_c.len()
            )));
        }
        let ws_now: BTreeSet<&str> = now.workspaces.iter().map(|w| w.name.as_str()).collect();
        let ws_was: BTreeSet<&str> = previewed
            .workspaces
            .iter()
            .map(|w| w.name.as_str())
            .collect();
        if ws_now != ws_was {
            return Err(stale(format!(
                "affect workspaces {ws_now:?} instead of the previewed {ws_was:?}"
            )));
        }
        Ok(p)
    }

    /// §6: an affected workspace (its `@` is rewritten) with an attached agent, other than the
    /// actor itself, gets a warning in the preview.
    fn agent_warnings(&self, workspaces: &[EditWorkspace], actor: &Actor) -> Vec<EditWarning> {
        let Ok(sessions) = self.db.sessions() else {
            return vec![];
        };
        let mut out = Vec::new();
        for w in workspaces {
            for s in sessions.iter().filter(|s| s.workspace == w.name) {
                let same = actor.agent_name() == s.agent.agent_name()
                    && actor.workspace().is_none_or(|ws| ws == w.name);
                if same || !matches!(s.agent, Actor::Agent { .. }) {
                    continue;
                }
                let agent = match &s.agent {
                    Actor::Agent { name, client, .. } => Actor::Agent {
                        name: name.clone(),
                        client: client.clone(),
                        workspace: Some(w.name.clone()),
                    },
                    a => a.clone(),
                };
                out.push(EditWarning {
                    code: "agent-attached".into(),
                    message: format!(
                        "workspace {} has agent {} attached ({}, last seen {}); this edit rewrites its @ under it — its next jj command sees the new commits",
                        w.name,
                        agent.label(),
                        s.activity,
                        s.last_seen
                    ),
                    workspace: Some(w.name.clone()),
                    agent: Some(agent),
                    changes: vec![],
                });
            }
        }
        out
    }

    /// Adds a warning for each divergent change and each workspace whose files were recovered.
    fn note_recoveries(&self, mut r: EditResult) -> EditResult {
        if !r.divergent.is_empty() {
            r.warnings.push(EditWarning {
                code: "divergent".into(),
                message: format!(
                    "{} now ha{} more than one visible commit; keep one (jj abandon the other) or squash them together",
                    r.divergent.iter().map(|c| short(c)).collect::<Vec<_>>().join(", "),
                    if r.divergent.len() == 1 { "s" } else { "ve" }
                ),
                workspace: None,
                agent: None,
                changes: r.divergent.clone(),
            });
        }
        for w in &r.workspaces {
            if let Some(c) = &w.recovered_into {
                r.warnings.push(EditWarning {
                    code: "workspace-recovered".into(),
                    message: recovered_note(&w.name, c),
                    workspace: Some(w.name.clone()),
                    agent: None,
                    changes: vec![],
                });
            }
        }
        r
    }

    /// `workspace.recovered` for each workspace whose edits now sit in a divergent sibling.
    fn emit_recoveries(&self, r: &EditResult) {
        for w in &r.workspaces {
            if let Some(c) = &w.recovered_into {
                self.emit(ServerEvent::WorkspaceRecovered {
                    workspace: w.name.clone(),
                    divergent_commit: c.clone(),
                    note: recovered_note(&w.name, c),
                });
            }
        }
    }

    /// `jj op revert <opAfter>`: undoes exactly this edit's operation, never "the last op".
    /// Refuses (409 `undo-blocked`, nothing written) when the revert would leave a change
    /// divergent or conflicted, e.g. because a later operation rewrote one of the edit's changes.
    pub fn undo_edit(&self, id: &str, actor: &Actor) -> Result<EditResult> {
        match self.undo_edit_checked(id, actor)? {
            Ok(r) => Ok(r),
            Err(b) => Err(ReviewError {
                status: 409,
                code: "undo-blocked",
                message: undo_blocked_message(&b),
            }),
        }
    }

    /// [`Review::undo_edit`] with the refusal as data (`ApiError.details` of `undo-blocked`).
    pub fn undo_edit_checked(
        &self,
        id: &str,
        actor: &Actor,
    ) -> Result<std::result::Result<EditResult, UndoBlocked>> {
        let _sync = self.queue()?;
        self.sync_locked(None)?;
        let row = self
            .db
            .get_edit(id)
            .map_err(store_error)?
            .ok_or_else(|| not_found(format!("unknown edit {id}")))?;
        let (Some(op), "applied") = (row.result.op_after.clone(), row.result.status.as_str())
        else {
            return Err(ReviewError {
                status: 409,
                code: "invalid",
                message: format!(
                    "edit {id} is {}; only applied edits can be undone",
                    row.result.status
                ),
            });
        };
        let plan = MutationPlan::Revert { op: op.clone() };
        // Later operations (external jj commands, other actors) that touched our changes.
        let mut ours: BTreeSet<String> = row.result.rewritten.iter().cloned().collect();
        ours.extend(row.result.created.iter().cloned());
        let later = self.later_ops(&op, &ours)?;
        let mut preview = self.vcs.preview(&plan).map_err(jj_error)?;
        // Reverting a resolution brings back the conflict the edit resolved: that is the undo
        // working, not a new conflict. Only conflicts absent at the edit's opBefore block.
        if !preview.new_conflicts.is_empty()
            && let Some(before) = row.result.op_before.as_deref()
        {
            let had: BTreeSet<(String, String)> = self
                .vcs
                .conflicts_at_op(before, "all()")
                .map_err(jj_error)?
                .into_iter()
                .map(|c| (c.change, c.path))
                .collect();
            preview
                .new_conflicts
                .retain(|c| !had.contains(&(c.change.clone(), c.path.clone())));
        }
        if !preview.divergent.is_empty() || !preview.new_conflicts.is_empty() {
            let head = self.op();
            return Ok(Err(UndoBlocked {
                edit: id.to_string(),
                op: op.clone(),
                suggestion: undo_suggestion(&later, &preview.divergent),
                later_ops: later,
                divergent: preview.divergent.clone(),
                conflicted: preview
                    .new_conflicts
                    .iter()
                    .map(|c| ConflictRecord {
                        change: c.change.clone(),
                        commit: c.commit.clone(),
                        path: c.path.clone(),
                        sides: c.sides,
                        first_seen_op: head.clone(),
                        introduced_by: None,
                    })
                    .collect(),
            }));
        }
        let out = self
            .vcs
            .mutate(
                &plan,
                &format!(
                    "cv: undo edit {id} (revert {}) ({})",
                    short(&op),
                    actor.label()
                ),
            )
            .map_err(jj_error)?;
        let mut result = row.result.clone();
        result.status = "undone".into();
        let after = self.result_from(id, "undone", actor, &out, false);
        result.workspaces = after.workspaces;
        result.divergent = out.divergent.clone();
        result.warnings.clear();
        let result = self.note_recoveries(result);
        self.store_edit(&row.request, &result)?;
        self.sync_locked(Some((actor, None)))?;
        self.emit_recoveries(&result);
        Ok(Ok(result))
    }

    /// Operations after `op` that rewrote or abandoned any of `changes`, newest first.
    fn later_ops(&self, op: &str, changes: &BTreeSet<String>) -> Result<Vec<LaterOp>> {
        let mut out = Vec::new();
        for o in self.vcs.ops_since(op, LATER_OPS_LIMIT).map_err(jj_error)? {
            let Some(parent) = o.parents.first() else {
                continue;
            };
            let Ok(d) = self.vcs.op_diff(parent, &o.id) else {
                continue;
            };
            let touched: Vec<String> = d
                .changed
                .iter()
                .chain(d.abandoned.iter())
                .filter(|c| changes.contains(*c))
                .cloned()
                .collect();
            if !touched.is_empty() {
                out.push(LaterOp {
                    op: o.id,
                    description: o.description,
                    time: cv_store::iso_from_ms(o.time_ms),
                    user: o.user,
                    changes: touched,
                });
            }
        }
        Ok(out)
    }

    /// Applies the thread's (latest) ```suggestion block to its anchored lines. Amends the change
    /// the thread anchors to unless `mode` says otherwise.
    pub fn apply_suggestion(
        &self,
        thread: &str,
        mode: Option<&str>,
        actor: &Actor,
    ) -> Result<EditResult> {
        let t = self.get_thread(thread)?;
        let text = t
            .comments
            .iter()
            .rev()
            .find_map(|c| suggestion_of(&c.body))
            .ok_or_else(|| invalid(format!("thread {thread} has no ```suggestion block")))?;
        let base_blob = self
            .vcs
            .file_at(&t.anchor.commit, &t.anchor.path)
            .ok()
            .flatten()
            .and_then(|f| f.blob);
        let req = EditRequest {
            target: t.anchor.change.clone(),
            files: vec![EditFile {
                path: t.anchor.path.clone(),
                base_blob,
                content: None,
                patches: Some(vec![LinePatch {
                    start: t.anchor.start,
                    end: t.anchor.end,
                    text,
                }]),
            }],
            mode: mode.unwrap_or("amend").into(),
            message: None,
            absorb_into: None,
            dry_run: false,
            origin: Some(EditOrigin {
                thread: Some(thread.into()),
            }),
        };
        self.edit(req, actor)
    }

    pub fn get_edit(&self, id: &str) -> Result<EditResult> {
        Ok(self
            .db
            .get_edit(id)
            .map_err(store_error)?
            .ok_or_else(|| not_found(format!("unknown edit {id}")))?
            .result)
    }

    // ── Conflicts ──

    fn first_seen(&self, commit: &str, path: &str) -> (String, Option<String>) {
        match self.db.conflict_record(commit, path) {
            Ok(Some(r)) => (r.first_seen_op, r.introduced_by),
            _ => (self.op(), None),
        }
    }

    /// Conflicted files in `revset` (default `mutable()`).
    pub fn list_conflicts(&self, revset: Option<&str>) -> Result<Vec<ConflictRecord>> {
        let list = self
            .vcs
            .conflicts(revset.unwrap_or("mutable()"))
            .map_err(|e| ReviewError {
                status: 400,
                code: "revset-invalid",
                message: e.0,
            })?;
        Ok(list
            .into_iter()
            .map(|c| {
                let (first_seen_op, introduced_by) = self.first_seen(&c.commit, &c.path);
                ConflictRecord {
                    change: c.change,
                    commit: c.commit,
                    path: c.path,
                    sides: c.sides,
                    first_seen_op,
                    introduced_by,
                }
            })
            .collect())
    }

    /// A commit id or a change id to the visible commit (an old commit id of a rewritten change
    /// maps to the change's current commit).
    fn visible_commit(&self, rev: &str) -> Result<cv_vcs::CommitInfo> {
        let c = self.vcs.resolve_rev(rev).map_err(|e| not_found(e.0))?;
        let now = self.vcs.change_commits(&c.change).map_err(jj_error)?;
        Ok(now.into_iter().next().unwrap_or(c))
    }

    pub fn conflict_detail(&self, commit: &str, path: &str) -> Result<ConflictDetail> {
        let c = self.visible_commit(commit)?;
        let t = self
            .vcs
            .conflict_text(&c.commit, path)
            .map_err(jj_error)?
            .ok_or_else(|| {
                not_found(format!("{path} is not conflicted at {}", short(&c.commit)))
            })?;
        let (first_seen_op, introduced_by) = self.first_seen(&c.commit, path);
        Ok(ConflictDetail {
            record: ConflictRecord {
                change: t.info.change,
                commit: t.info.commit,
                path: t.info.path,
                sides: t.info.sides,
                first_seen_op,
                introduced_by,
            },
            materialized: t.materialized,
            regions: t
                .regions
                .into_iter()
                .map(|r| ConflictRegion {
                    start: r.start,
                    end: r.end,
                    base: r.base,
                    sides: r
                        .sides
                        .into_iter()
                        .map(|(label, text)| ConflictSide { label, text })
                        .collect(),
                })
                .collect(),
        })
    }

    /// `jj resolve` at any commit with the whole new file `content` (markers left in it stay
    /// conflicted). Recorded as an applied edit.
    pub fn resolve_conflict(
        &self,
        commit: &str,
        path: &str,
        content: &str,
        actor: &Actor,
    ) -> Result<EditResult> {
        let _sync = self.queue()?;
        self.sync_locked(None)?;
        let c = self.visible_commit(commit)?;
        if self
            .vcs
            .conflict_text(&c.commit, path)
            .map_err(jj_error)?
            .is_none()
        {
            return Err(not_found(format!(
                "{path} is not conflicted at {}",
                short(&c.commit)
            )));
        }
        if self.vcs.is_immutable(&c.commit).map_err(jj_error)? {
            return Err(ReviewError {
                status: 409,
                code: "immutable",
                message: format!("{} is immutable", short(&c.change)),
            });
        }
        let id = self.new_id("e");
        let req = EditRequest {
            target: c.commit.clone(),
            files: vec![EditFile {
                path: path.into(),
                base_blob: None,
                content: Some(content.into()),
                patches: None,
            }],
            mode: "amend".into(),
            message: None,
            absorb_into: None,
            dry_run: false,
            origin: None,
        };
        let result = match self.vcs.mutate(
            &MutationPlan::Resolve {
                commit: c.commit.clone(),
                path: path.into(),
                content: content.as_bytes().to_vec(),
            },
            &format!(
                "cv: resolve {path} in {} ({})",
                short(&c.change),
                actor.label()
            ),
        ) {
            Ok(out) => {
                let mut r = self.result_from(&id, "applied", actor, &out, false);
                r.files = self.edited_files(&out, &c.change, &req);
                r
            }
            Err(e) => self.failed(&id, "failed", actor, reject("jj-error", e.0)),
        };
        let result = self.note_recoveries(result);
        self.store_edit(&req, &result)?;
        if result.status == "applied" {
            self.sync_locked(Some((actor, Some(&id))))?;
        }
        self.emit_recoveries(&result);
        Ok(result)
    }

    // ── Workspaces ──

    /// A `WorkspaceSummary` (model.ts) for `workspace.updated`.
    pub fn workspace_summary(&self, w: &WorkspaceAt) -> Value {
        if let Some(f) = self.summarizer.read().unwrap().as_ref()
            && let Some(v) = f(&w.name)
        {
            return v;
        }
        let revset = format!("trunk()..{}@", w.name);
        let stack = self.vcs.resolve(&revset).unwrap_or_default();
        let conflicts = self.vcs.conflicts(&revset).map(|c| c.len()).unwrap_or(0);
        let mut files: BTreeSet<String> = BTreeSet::new();
        if !stack.is_empty()
            && let Ok(reader) = self.vcs.reader()
            && let Ok(hist) = reader.history(&w.commit)
        {
            let want: BTreeSet<&str> = stack.iter().map(|c| c.commit.as_str()).collect();
            let mut found = 0;
            for c in hist.take(stack.len() * 4 + 8).flatten() {
                if want.contains(c.commit.as_str()) {
                    files.extend(c.files);
                    found += 1;
                    if found == want.len() {
                        break;
                    }
                }
            }
        }
        let last_op = self.op();
        let op_info = self.vcs.op_info(&last_op).ok();
        let agent = self
            .db
            .sessions()
            .ok()
            .and_then(|s| s.into_iter().find(|s| s.workspace == w.name))
            .map(|s| {
                let (name, client) = match &s.agent {
                    Actor::Agent { name, client, .. } => (name.clone(), client.clone()),
                    a => (a.label(), None),
                };
                json!({ "name": name, "client": client, "activity": s.activity, "lastSeen": s.last_seen })
            });
        let mut v = json!({
            "name": w.name,
            "root": w.root.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
            "wc": { "change": w.change, "commit": w.commit },
            "serving": w.name == self.vcs.workspace_name(),
            "stack": { "revset": revset, "changes": stack.len(), "files": files.len(), "conflicts": conflicts },
            "unsnapshottedFiles": 0,
            "lastOp": {
                "op": last_op,
                "time": op_info.as_ref().map(|o| cv_store::iso_from_ms(o.time_ms)).unwrap_or_default(),
                "description": op_info.map(|o| o.description).unwrap_or_default(),
            },
            "overlaps": [],
        });
        if let Some(a) = agent {
            v["agent"] = a;
        }
        v
    }

    // ── Sync (op changes) ──

    /// Catches up with the repository: emits events for every operation since the last sync.
    pub fn sync(&self) -> Result<()> {
        let _sync = self.sync_lock.lock().unwrap();
        self.sync_locked(None)
    }

    fn sync_locked(&self, by: Option<(&Actor, Option<&str>)>) -> Result<()> {
        let head = self.vcs.head_op().map_err(jj_error)?;
        let last = self.state.lock().unwrap().last_op.clone();
        if last.as_deref() == Some(head.as_str()) {
            return Ok(());
        }
        let workspaces = self.vcs.workspaces().map_err(jj_error)?;
        let wc_now: BTreeMap<String, String> = workspaces
            .iter()
            .map(|w| (w.name.clone(), w.commit.clone()))
            .collect();
        let Some(last) = last else {
            // First sync: record the state without emitting anything.
            {
                let mut s = self.state.lock().unwrap();
                s.last_op = Some(head.clone());
                s.wc = wc_now;
            }
            self.track_conflicts(&head, None, false)?;
            return Ok(());
        };
        let diff = self.vcs.op_diff(&last, &head).map_err(jj_error)?;
        self.state.lock().unwrap().last_op = Some(head.clone());
        let actor = by.map(|(a, _)| a.clone()).unwrap_or(Actor::External);
        let mut changes: Vec<String> = diff.changed.clone();
        changes.extend(diff.abandoned.iter().cloned());
        self.emit(ServerEvent::OpChanged {
            op: head.clone(),
            changes,
            actor: actor.clone(),
        });
        let change = OpChange {
            from_op: last,
            to_op: head.clone(),
            changes: diff.changed.clone(),
            abandoned: diff.abandoned.clone(),
            actor,
        };
        for hook in self.hooks.read().unwrap().iter() {
            for ev in hook(&change) {
                self.emit(ev);
            }
        }
        self.reanchor(&head, &diff.changed, &diff.abandoned)?;
        self.track_conflicts(&head, by.and_then(|(_, e)| e), true)?;
        let old_wc = std::mem::replace(&mut self.state.lock().unwrap().wc, wc_now);
        for w in &workspaces {
            if old_wc.get(&w.name) != Some(&w.commit) {
                let summary = self.workspace_summary(w);
                self.emit(ServerEvent::WorkspaceUpdated { workspace: summary });
            }
        }
        if let Some(index) = &self.index {
            let _ = index.set_meta("last_synced_op", &head);
        }
        Ok(())
    }

    /// Follows threads of rewritten/abandoned changes; returns the threads that changed.
    fn reanchor(&self, op: &str, changed: &[String], abandoned: &[String]) -> Result<Vec<String>> {
        let threads = self
            .db
            .list_threads(&ThreadFilter::default())
            .map_err(store_error)?;
        let mut touched = Vec::new();
        self.write_staged_anchors(op, &mut touched)?;
        let threads = if touched.is_empty() {
            threads
        } else {
            self.db
                .list_threads(&ThreadFilter::default())
                .map_err(store_error)?
        };
        let todo: Vec<Thread> = threads
            .into_iter()
            .filter(|t| !matches!(t.status.as_str(), "resolved" | "wontfix" | "outdated"))
            .filter(|t| changed.contains(&t.anchor.change) || abandoned.contains(&t.anchor.change))
            .collect();
        // index.progress job "reanchor" (ws.ts IndexJobKind): start and end of the pass, only
        // when an operation rewrote changes that carry open threads.
        let total = todo.len() as u32;
        let progress = |done: u32| {
            self.emit(ServerEvent::IndexProgress {
                job: "reanchor".into(),
                done,
                total,
                view_id: None,
            });
        };
        if total > 0 {
            progress(0);
        }
        let result = self.reanchor_threads(op, todo, &mut touched);
        if total > 0 {
            progress(total);
        }
        result?;
        Ok(touched)
    }

    fn reanchor_threads(
        &self,
        op: &str,
        todo: Vec<Thread>,
        touched: &mut Vec<String>,
    ) -> Result<()> {
        for mut t in todo {
            let Some(now) = self.current_commit(&t)? else {
                self.outdate(&mut t, op, "change-abandoned")?;
                touched.push(t.id.clone());
                continue;
            };
            if now.commit == t.anchor.commit {
                continue;
            }
            let Some(new_text) = self.text_at(&now.commit, &t.anchor.path) else {
                self.outdate(&mut t, op, "file-removed")?;
                touched.push(t.id.clone());
                continue;
            };
            let new_lines = anchor::split(&new_text);
            let (s, e, kind) = match self.follow(&t, &now.commit, &new_text) {
                anchor::Reanchor::Outdated => {
                    self.outdate(&mut t, op, "lines-changed")?;
                    touched.push(t.id.clone());
                    continue;
                }
                anchor::Reanchor::Moved { start, end } => (start, end, "moved"),
                anchor::Reanchor::Unchanged => (t.anchor.start, t.anchor.end, "unchanged"),
            };
            t.anchor.commit = now.commit.clone();
            t.anchor.start = s;
            t.anchor.end = e;
            t.anchor.fingerprint = anchor::fingerprint(&new_lines, s, e);
            t.updated_at = now_iso();
            self.db
                .add_anchor_history(&t.id, op, &now.commit, s, e, kind)
                .map_err(store_error)?;
            self.put_thread(&t)?;
            touched.push(t.id.clone());
        }
        Ok(())
    }

    fn text_at(&self, commit: &str, path: &str) -> Option<String> {
        self.vcs
            .file_at(commit, path)
            .ok()
            .flatten()
            .map(|f| String::from_utf8_lossy(&f.bytes).into_owned())
    }

    /// The commit a thread's change is at now; None when it was abandoned. A divergent change
    /// (several visible commits) is settled deterministically (b11): the commit whose evolog holds
    /// the anchor's commit through the fewest rewrites, then the newest, then the lowest id.
    fn current_commit(&self, t: &Thread) -> Result<Option<cv_vcs::CommitInfo>> {
        let mut commits = self
            .vcs
            .change_commits(&t.anchor.change)
            .map_err(jj_error)?;
        if commits.len() <= 1 {
            return Ok(commits.pop());
        }
        let hops = |c: &cv_vcs::CommitInfo| {
            self.vcs
                .evolution_path(&t.anchor.commit, &c.commit, EVOLOG_LIMIT)
                .ok()
                .flatten()
                .map_or(usize::MAX, |p| p.len())
        };
        let mut ranked: Vec<(usize, i64, cv_vcs::CommitInfo)> = commits
            .into_iter()
            .map(|c| (hops(&c), c.committer_time_ms, c))
            .collect();
        ranked.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(b.1.cmp(&a.1))
                .then(a.2.commit.cmp(&b.2.commit))
        });
        Ok(ranked.into_iter().next().map(|(_, _, c)| c))
    }

    /// Re-anchors a thread from its commit to `now` through the evolog (DESIGN §6: "through the
    /// evolog and diff"): each rewrite between them is diffed in turn, so a block that moved in one
    /// rewrite and was edited around in the next is still followed. Falls back to one diff from
    /// the anchor's commit when the evolog does not reach it (or loses the lines on the way).
    fn follow(&self, t: &Thread, now: &str, now_text: &str) -> anchor::Reanchor {
        let a = &t.anchor;
        let old_text = self.text_at(&a.commit, &a.path).unwrap_or_default();
        let direct = || {
            anchor::reanchor(
                &anchor::split(&old_text),
                &anchor::split(now_text),
                a.start,
                a.end,
                &a.fingerprint,
            )
        };
        let Some(path) = self
            .vcs
            .evolution_path(&a.commit, now, EVOLOG_LIMIT)
            .ok()
            .flatten()
            .filter(|p| p.len() > 2)
        else {
            return direct();
        };
        let (mut cur, mut s, mut e, mut fp) =
            (old_text.clone(), a.start, a.end, a.fingerprint.clone());
        for commit in &path[1..] {
            let next = if commit == now {
                now_text.to_string()
            } else {
                match self.text_at(commit, &a.path) {
                    Some(x) => x,
                    None => continue,
                }
            };
            let next_lines = anchor::split(&next);
            match anchor::reanchor(&anchor::split(&cur), &next_lines, s, e, &fp) {
                anchor::Reanchor::Outdated => return direct(),
                anchor::Reanchor::Moved { start, end } => (s, e) = (start, end),
                anchor::Reanchor::Unchanged => {}
            }
            fp = anchor::fingerprint(&next_lines, s, e);
            cur = next;
        }
        if (s, e) == (a.start, a.end) {
            anchor::Reanchor::Unchanged
        } else {
            anchor::Reanchor::Moved { start: s, end: e }
        }
    }

    /// Where an edit that answers `thread` leaves the thread's lines (a1), or None when the edit
    /// is to another file or the anchored lines cannot be found in the text the edit started from.
    fn anchor_through_edit(
        &self,
        thread: &str,
        path: &str,
        start_text: &[u8],
        new: &[u8],
        patches: Option<&[cv_contracts::review::LinePatch]>,
    ) -> Option<AnchorAfter> {
        let t = self.db.get_thread(thread).ok().flatten()?;
        if t.anchor.path != path {
            return None;
        }
        let start_text = String::from_utf8_lossy(start_text);
        let start_lines = anchor::split(&start_text);
        let a = &t.anchor;
        let anchored = self.text_at(&a.commit, path).unwrap_or_default();
        let (s, e) = if anchored == start_text {
            (a.start, a.end)
        } else {
            match anchor::reanchor(
                &anchor::split(&anchored),
                &start_lines,
                a.start,
                a.end,
                &a.fingerprint,
            ) {
                anchor::Reanchor::Unchanged => (a.start, a.end),
                anchor::Reanchor::Moved { start, end } => (start, end),
                anchor::Reanchor::Outdated => return None,
            }
        };
        let text = String::from_utf8_lossy(new).into_owned();
        let new_lines = anchor::split(&text);
        let (start, end) = match patches {
            Some(ps) => anchor::map_through_patches(s, e, ps, new_lines.len()),
            None => anchor::map_through_texts(&start_lines, &new_lines, s, e),
        };
        Some(AnchorAfter {
            thread: thread.to_string(),
            path: path.to_string(),
            start,
            end,
            text,
        })
    }

    /// Moves a thread's anchor onto the commit that holds the edit answering it (a1). Staged here
    /// and written by the sync right after the apply, before it re-anchors anything else: that pass
    /// then finds the anchor current instead of the old lines gone.
    fn stage_anchor(&self, a: &AnchorAfter, change: &str, commit: &str) -> Result<()> {
        let Some(now_text) = self.text_at(commit, &a.path) else {
            return Err(invalid(format!("{} is not in {}", a.path, short(commit))));
        };
        let now_lines = anchor::split(&now_text);
        let (start, end) = if now_text == a.text {
            (a.start, a.end)
        } else {
            // The edit was merged with changes made since its base: follow the lines into it.
            anchor::map_through_texts(&anchor::split(&a.text), &now_lines, a.start, a.end)
        };
        self.state
            .lock()
            .unwrap()
            .staged_anchors
            .push(StagedAnchor {
                thread: a.thread.clone(),
                change: change.to_string(),
                commit: commit.to_string(),
                start,
                end,
                fingerprint: anchor::fingerprint(&now_lines, start, end),
            });
        Ok(())
    }

    fn write_staged_anchors(&self, op: &str, touched: &mut Vec<String>) -> Result<()> {
        let staged = std::mem::take(&mut self.state.lock().unwrap().staged_anchors);
        for a in staged {
            let Ok(mut t) = self.get_thread(&a.thread) else {
                continue;
            };
            t.anchor.change = a.change;
            t.anchor.commit = a.commit.clone();
            t.anchor.start = a.start;
            t.anchor.end = a.end;
            t.anchor.cols = None;
            t.anchor.fingerprint = a.fingerprint;
            if t.status == "outdated" {
                t.status = "open".into();
            }
            t.outdated = None;
            t.updated_at = now_iso();
            self.db
                .add_anchor_history(&t.id, op, &a.commit, a.start, a.end, "moved")
                .map_err(store_error)?;
            self.put_thread(&t)?;
            touched.push(t.id.clone());
        }
        Ok(())
    }

    /// A thread leaving `outdated` (a human or agent says it still applies) is re-anchored on its
    /// change's current commit: through the evolog when the lines survived, else on the same line
    /// numbers clamped to the file. Without this the next operation would outdate it again.
    fn refresh_anchor(&self, t: &mut Thread) -> Result<()> {
        let Some(now) = self.current_commit(t)? else {
            return Ok(());
        };
        let Some(text) = self.text_at(&now.commit, &t.anchor.path) else {
            return Ok(());
        };
        let lines = anchor::split(&text);
        let (s, e) = if now.commit == t.anchor.commit {
            (t.anchor.start, t.anchor.end)
        } else {
            match self.follow(t, &now.commit, &text) {
                anchor::Reanchor::Moved { start, end } => (start, end),
                _ => {
                    let max = lines.len().max(1) as u32;
                    let s = t.anchor.start.clamp(1, max);
                    (s, t.anchor.end.clamp(s, max))
                }
            }
        };
        t.anchor.commit = now.commit;
        t.anchor.start = s;
        t.anchor.end = e;
        t.anchor.fingerprint = anchor::fingerprint(&lines, s, e);
        Ok(())
    }

    fn outdate(&self, t: &mut Thread, op: &str, reason: &str) -> Result<()> {
        t.outdated = Some(Outdated {
            reason: reason.into(),
            last_good: t.anchor.commit.clone(),
        });
        t.status = "outdated".into();
        t.updated_at = now_iso();
        self.db
            .add_anchor_history(
                &t.id,
                op,
                &t.anchor.commit,
                t.anchor.start,
                t.anchor.end,
                "outdated",
            )
            .map_err(store_error)?;
        self.put_thread(t)
    }

    /// Diffs the repo's conflicts against review.db's `conflict_seen`, keyed by (change, path) so a
    /// rebase that keeps a conflict is not reported as resolved + added.
    fn track_conflicts(&self, op: &str, edit: Option<&str>, emit: bool) -> Result<()> {
        // A failed read (e.g. op heads mid-write by another process) must not look like "every
        // conflict resolved": keep the previous state and let the next sync catch up.
        let now = match self.vcs.conflicts("mutable()") {
            Ok(now) => now,
            Err(e) => {
                eprintln!("cv: conflict check skipped at op {op}: {e}");
                return Ok(());
            }
        };
        let open = self.db.open_conflicts().map_err(store_error)?;
        let key = |change: &str, path: &str| format!("{change}\u{0}{path}");
        let open_by: HashMap<String, &ConflictRecord> =
            open.iter().map(|c| (key(&c.change, &c.path), c)).collect();
        let now_keys: BTreeSet<String> = now.iter().map(|c| key(&c.change, &c.path)).collect();
        let mut added = Vec::new();
        for c in &now {
            let prev = open_by.get(&key(&c.change, &c.path));
            if prev.is_some_and(|p| p.commit == c.commit) {
                continue;
            }
            let rec = ConflictRecord {
                change: c.change.clone(),
                commit: c.commit.clone(),
                path: c.path.clone(),
                sides: c.sides,
                first_seen_op: prev
                    .map(|p| p.first_seen_op.clone())
                    .unwrap_or_else(|| op.to_string()),
                introduced_by: prev
                    .and_then(|p| p.introduced_by.clone())
                    .or_else(|| edit.map(String::from)),
            };
            self.db.conflict_seen(&rec).map_err(store_error)?;
            if let Some(p) = prev {
                self.db
                    .conflict_gone(&p.commit, &p.path, op)
                    .map_err(store_error)?;
            } else {
                // A reopened conflict (an undo brought its commit back) keeps its stored first
                // sighting; report what GET /conflicts will list.
                let stored = self
                    .db
                    .conflict_record(&rec.commit, &rec.path)
                    .map_err(store_error)?;
                added.push(stored.unwrap_or(rec));
            }
        }
        let mut resolved = Vec::new();
        for c in &open {
            if !now_keys.contains(&key(&c.change, &c.path)) {
                self.db
                    .conflict_gone(&c.commit, &c.path, op)
                    .map_err(store_error)?;
                resolved.push(c.clone());
            }
        }
        if emit && (!added.is_empty() || !resolved.is_empty()) {
            self.emit(ServerEvent::ConflictsChanged { added, resolved });
        }
        Ok(())
    }

    /// Watches `.jj/repo/op_heads` and syncs when it changes (external `jj` commands, other
    /// agents): through `notify`, with a check every [`NOTIFY_SAFETY_POLL`]; if `notify` cannot
    /// watch, by polling every `interval`. Stops when the service is dropped or
    /// [`Review::stop_watcher`].
    pub fn start_watcher(self: &Arc<Self>, interval: Duration) {
        let weak: Weak<Review> = Arc::downgrade(self);
        let stop = self.stop.clone();
        let watching = self.watching.clone();
        let dir = self.vcs.repo_path().join("op_heads").join("heads");
        std::thread::Builder::new()
            .name("cv-op-watcher".into())
            .spawn(move || {
                // DESIGN §4.1: `notify` wakes the loop when `op_heads/heads` changes. A check every
                // NOTIFY_SAFETY_POLL still runs in case an event is lost; when notify cannot watch
                // the directory, the loop polls every `interval` instead.
                let (tx, rx) = std::sync::mpsc::channel::<()>();
                let watcher =
                    notify::recommended_watcher(move |_: notify::Result<notify::Event>| {
                        let _ = tx.send(());
                    })
                    .and_then(|mut w| {
                        notify::Watcher::watch(&mut w, &dir, notify::RecursiveMode::NonRecursive)?;
                        Ok(w)
                    });
                let wait = match &watcher {
                    Ok(_) => NOTIFY_SAFETY_POLL.max(interval),
                    Err(e) => {
                        eprintln!(
                            "cv: op watcher: cannot watch {} ({e}); polling every {interval:?}",
                            dir.display()
                        );
                        interval
                    }
                };
                let mut last_sig = String::new();
                while !stop.load(Ordering::Relaxed) {
                    let Some(r) = weak.upgrade() else { break };
                    let sig = r.vcs.op_heads_signature();
                    if sig != last_sig {
                        match r.sync() {
                            Ok(()) => last_sig = sig,
                            Err(e) => eprintln!("cv: op sync failed: {e}"),
                        }
                    }
                    drop(r);
                    watching.store(true, Ordering::Release);
                    match rx.recv_timeout(wait) {
                        Ok(()) => {
                            // jj adds the new head and then removes the old one: let the burst
                            // settle, then check once.
                            std::thread::sleep(NOTIFY_SETTLE);
                            while rx.try_recv().is_ok() {}
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            std::thread::sleep(interval)
                        }
                    }
                }
                drop(watcher);
            })
            .expect("spawn op watcher");
    }

    /// Whether the op watcher started by [`Review::start_watcher`] is watching (its `notify`
    /// watch is in place, or it polls) and has made its first check: an op after this is seen.
    pub fn watcher_ready(&self) -> bool {
        self.watching.load(Ordering::Acquire)
    }

    pub fn stop_watcher(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for Review {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn plan_fingerprint(plan: &MutationPlan) -> u64 {
    cv_store::fnv64(format!("{plan:?}").as_bytes())
}

fn recovered_note(workspace: &str, commit: &str) -> String {
    format!(
        "workspace {workspace}'s @ was rewritten while it had files jj had not snapshotted; they are kept in the divergent commit {} (jj squash --from {} --into {workspace}@ to bring them back, or jj abandon {} to drop them)",
        short(commit),
        short(commit),
        short(commit)
    )
}

fn undo_suggestion(later: &[LaterOp], divergent: &[String]) -> String {
    if later.is_empty() {
        return format!(
            "Reverting would leave {} divergent or conflicted. Make a new edit that reverses this one instead.",
            if divergent.is_empty() {
                "changes".to_string()
            } else {
                divergent
                    .iter()
                    .map(|c| short(c))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
    }
    let ops: Vec<String> = later.iter().map(|o| short(&o.op).to_string()).collect();
    format!(
        "Later operations changed the same changes ({}). Undo those first, newest first (jj op revert {}), then undo this edit; or keep them and make a new edit that reverses this one.",
        ops.join(", "),
        ops.join(" ; jj op revert ")
    )
}

/// The human-readable message of a 409 `undo-blocked`.
pub fn undo_blocked_text(b: &UndoBlocked) -> String {
    undo_blocked_message(b)
}

fn undo_blocked_message(b: &UndoBlocked) -> String {
    let mut m = format!(
        "undoing edit {} (revert op {}) would leave",
        b.edit,
        short(&b.op)
    );
    if !b.divergent.is_empty() {
        m.push_str(&format!(
            " divergent changes {}",
            b.divergent
                .iter()
                .map(|c| short(c))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !b.conflicted.is_empty() {
        if !b.divergent.is_empty() {
            m.push_str(" and");
        }
        m.push_str(&format!(
            " new conflicts in {}",
            b.conflicted
                .iter()
                .map(|c| format!("{} {}", short(&c.change), c.path))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    m.push_str("; nothing was changed.");
    for o in &b.later_ops {
        m.push_str(&format!(
            " Later op {} ({}, {}) touched {}.",
            short(&o.op),
            o.description.lines().next().unwrap_or_default(),
            o.user,
            o.changes
                .iter()
                .map(|c| short(c))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    m.push(' ');
    m.push_str(&b.suggestion);
    m
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestion_blocks_parse() {
        assert_eq!(
            suggestion_of("Rename:\n```suggestion\nrenamed_value = 1\n```").as_deref(),
            Some("renamed_value = 1")
        );
        assert_eq!(suggestion_of("no block"), None);
        assert_eq!(
            suggestion_of("```suggestion\na\nb\n```\n").as_deref(),
            Some("a\nb")
        );
    }
}
