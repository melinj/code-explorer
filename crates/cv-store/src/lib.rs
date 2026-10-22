//! B2 `cv-store`: SQLite (rusqlite, bundled) over two files per repo.
//!
//! - `index.db` — derived data, safe to delete; lives in `$XDG_CACHE_HOME/code-viewer/<repo-id>/`.
//! - `review.db` — human and agent data (threads, comments, edits, conflicts seen, sessions, the
//!   event log); lives in `$XDG_DATA_HOME/code-viewer/<repo-id>/`. Re-indexing never touches it.
//!
//! The repo-id hashes the real path of `<workspace>/.jj/repo`, so every workspace of one repo
//! shares the same data (DESIGN §3). Migrations are `docs/schema/{index,review}.sql`, verbatim.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cv_contracts::review::{
    Actor, Comment, ConflictRecord, EditRequest, EditResult, Thread, ThreadId,
};
use rusqlite::{Connection, OptionalExtension, params};

pub use rusqlite;

mod records;
pub mod views;

pub use records::{
    ClusterLabelRow, ClusterPinRow, DeclaredLinkRow, LayoutRows, LinkFeedbackRow, SavedRangeRow,
    WindowPinRow,
};
pub use views::{SearchRow, StoredFile, StoredLink, StoredView};

pub const INDEX_SCHEMA: &str = include_str!("../../../docs/schema/index.sql");
pub const REVIEW_SCHEMA: &str = include_str!("../../../docs/schema/review.sql");

/// Tables added after schema version 1, created on open when missing (also in review.sql, so a
/// fresh database and an upgraded one end up the same).
const REVIEW_ADDITIONS: &str = "
CREATE TABLE IF NOT EXISTS agent_permission (
  ui_session  TEXT NOT NULL,
  agent       TEXT NOT NULL,
  edit        INTEGER,
  move_camera INTEGER,
  updated_at  TEXT NOT NULL,
  PRIMARY KEY (ui_session, agent)
) STRICT, WITHOUT ROWID;
";

#[derive(Debug, Clone)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error(e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// FNV-1a 64: stable across Rust versions (unlike `DefaultHasher`).
pub fn fnv64(s: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// `<repo-id>`: hash of the real path of `.jj/repo` (resolving the `.jj/repo` file that secondary
/// workspaces use to point at the main repo).
pub fn repo_id(workspace_root: &Path) -> String {
    let jj_repo = workspace_root.join(".jj").join("repo");
    let real = if jj_repo.is_file() {
        // Secondary workspace: the file holds the (relative or absolute) path of the repo dir.
        std::fs::read_to_string(&jj_repo)
            .ok()
            .map(|p| workspace_root.join(".jj").join(p.trim()))
            .unwrap_or(jj_repo)
    } else {
        jj_repo
    };
    let real = real.canonicalize().unwrap_or(real);
    format!("{:016x}", fnv64(real.to_string_lossy().as_bytes()))
}

#[derive(Debug, Clone)]
pub struct StorePaths {
    pub index_db: PathBuf,
    pub review_db: PathBuf,
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            home.join(fallback)
        }
    }
}

impl StorePaths {
    /// DESIGN §3 locations for the repo of the workspace at `workspace_root`.
    pub fn for_repo(workspace_root: &Path) -> Self {
        let id = repo_id(workspace_root);
        StorePaths {
            index_db: xdg("XDG_CACHE_HOME", ".cache")
                .join("code-viewer")
                .join(&id)
                .join("index.db"),
            review_db: xdg("XDG_DATA_HOME", ".local/share")
                .join("code-viewer")
                .join(&id)
                .join("review.db"),
        }
    }

    /// The per-user data directory every repo's review data sits under
    /// (`$XDG_DATA_HOME/code-viewer`, else `~/.local/share/code-viewer`).
    pub fn data_root() -> PathBuf {
        xdg("XDG_DATA_HOME", ".local/share").join("code-viewer")
    }

    /// Both files under one directory (tests, `--data-dir`).
    pub fn in_dir(dir: &Path) -> Self {
        StorePaths {
            index_db: dir.join("index.db"),
            review_db: dir.join("review.db"),
        }
    }
}

/// ISO-8601 UTC with milliseconds, e.g. "2026-10-18T13:25:45.123Z".
pub fn now_iso() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    iso_from_ms(ms)
}

pub fn iso_from_ms(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        ms.rem_euclid(1000)
    )
}

fn open_conn(path: Option<&Path>) -> Result<Connection> {
    let conn = match path {
        Some(p) => {
            if let Some(dir) = p.parent() {
                // Owner-only: the stores hold code, review threads and edits (folders this creates;
                // an existing one keeps its mode).
                let mut b = std::fs::DirBuilder::new();
                b.recursive(true);
                #[cfg(unix)]
                std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
                b.create(dir)
                    .map_err(|e| Error(format!("cannot create {}: {e}", dir.display())))?;
            }
            Connection::open(p)?
        }
        None => Connection::open_in_memory()?,
    };
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

/// Applies `schema` once: a database whose `meta` table has a `schema_version` is left alone.
fn migrate(conn: &Connection, schema: &str) -> Result<()> {
    let has_meta: bool = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'meta'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)?;
    if !has_meta {
        conn.execute_batch(schema)?;
    }
    Ok(())
}

fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()?)
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// `index.db`: derived, deletable: the parse cache, saved snapshot views (windows with their
/// R*Tree, links, clusters, files) and the search index (see [`views`]).
pub struct IndexDb {
    conn: Mutex<Connection>,
    path: Option<PathBuf>,
}

impl IndexDb {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = open_conn(Some(path))?;
        migrate(&conn, INDEX_SCHEMA)?;
        conn.execute_batch(PARSE_CACHE_SCHEMA)?;
        views::upgrade(&conn)?;
        Ok(IndexDb {
            conn: Mutex::new(conn),
            path: Some(path.to_path_buf()),
        })
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        get_meta(&self.conn.lock().unwrap(), key)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        set_meta(&self.conn.lock().unwrap(), key, value)
    }

    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        Ok(f(&self.conn.lock().unwrap())?)
    }

    /// Cached facts for each (path, blob, pack) key: the stored bytes when the row exists for
    /// that exact pack string, else `None`.
    pub fn parse_cache_get(&self, keys: &[(&str, &str, &str)]) -> Result<Vec<Option<Vec<u8>>>> {
        let conn = self.conn.lock().unwrap();
        let mut st = conn.prepare_cached(
            "SELECT facts FROM parse_cache WHERE path = ?1 AND blob_id = ?2 AND pack = ?3",
        )?;
        let mut out = Vec::with_capacity(keys.len());
        for (path, blob, pack) in keys {
            out.push(
                st.query_row(params![path, blob, pack], |r| r.get::<_, Vec<u8>>(0))
                    .optional()?,
            );
        }
        Ok(out)
    }

    /// Stores facts (one transaction), replacing any row for the same path and blob.
    pub fn parse_cache_put(&self, rows: &[(&str, &str, &str, &[u8])]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        {
            let mut st = tx.prepare_cached(
                "INSERT OR REPLACE INTO parse_cache (path, blob_id, pack, facts) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (path, blob, pack, facts) in rows {
                st.execute(params![path, blob, pack, facts])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Keeps the cache from growing without bound: when it holds more than `max_rows` rows, only
    /// the (path, blob) pairs in `keep` stay.
    pub fn parse_cache_trim(&self, keep: &[(&str, &str)], max_rows: usize) -> Result<usize> {
        let mut conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row("SELECT count(*) FROM parse_cache", [], |r| r.get(0))?;
        if n as usize <= max_rows {
            return Ok(0);
        }
        let tx = conn.transaction()?;
        tx.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS parse_keep (path TEXT, blob_id TEXT, PRIMARY KEY (path, blob_id)); DELETE FROM parse_keep;",
        )?;
        {
            let mut st = tx.prepare_cached(
                "INSERT OR IGNORE INTO parse_keep (path, blob_id) VALUES (?1, ?2)",
            )?;
            for (p, b) in keep {
                st.execute(params![p, b])?;
            }
        }
        let gone = tx.execute(
            "DELETE FROM parse_cache WHERE NOT EXISTS (SELECT 1 FROM parse_keep k WHERE k.path = parse_cache.path AND k.blob_id = parse_cache.blob_id)",
            [],
        )?;
        tx.commit()?;
        Ok(gone)
    }
}

/// The parse cache table, for index.db files created before it existed.
const PARSE_CACHE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS parse_cache (path TEXT NOT NULL, blob_id TEXT NOT NULL, pack TEXT NOT NULL, facts BLOB NOT NULL, PRIMARY KEY (path, blob_id)) STRICT;";

/// Thread list filter (GET /threads query).
#[derive(Debug, Default, Clone)]
pub struct ThreadFilter {
    /// Comma-separated statuses.
    pub status: Option<String>,
    pub assignee: Option<String>,
    pub path: Option<String>,
    /// Only threads anchored to these changes.
    pub changes: Option<Vec<String>>,
}

/// One stored edit.
#[derive(Debug, Clone)]
pub struct EditRow {
    pub request: EditRequest,
    pub result: EditResult,
    pub thread: Option<ThreadId>,
}

#[derive(Debug, Clone)]
pub struct SessionRow {
    pub session_id: String,
    pub workspace: String,
    pub agent: Actor,
    pub bound_at: String,
    pub last_seen: String,
    pub activity: String,
}

/// One UI session's agent toggles (`agent_permission`). `None` = not set by that session.
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRow {
    pub ui_session: String,
    /// Agent name, or "*" for every agent.
    pub agent: String,
    pub edit: Option<bool>,
    pub move_camera: Option<bool>,
    pub updated_at: String,
}

/// `review.db`: threads, comments, edits, conflicts seen, sessions and the event log.
pub struct ReviewDb {
    conn: Mutex<Connection>,
}

impl ReviewDb {
    /// Opens (creating from review.sql, or upgrading a version-1 file and moving its JSON
    /// records into tables).
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(open_conn(Some(path))?)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(open_conn(None)?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        migrate(&conn, REVIEW_SCHEMA)?;
        conn.execute_batch(REVIEW_ADDITIONS)?;
        records::upgrade(&mut conn)?;
        Ok(ReviewDb {
            conn: Mutex::new(conn),
        })
    }

    // ── Declared links and verdicts (B4/B8) ──

    /// Declared links in declaration order and verdicts in rating order.
    pub fn link_records(&self) -> Result<(Vec<DeclaredLinkRow>, Vec<LinkFeedbackRow>)> {
        records::read_links(&self.conn.lock().unwrap())
    }

    /// Replaces every declared link and verdict (one transaction) and bumps [`Self::links_rev`].
    pub fn put_link_records(
        &self,
        declared: &[DeclaredLinkRow],
        ratings: &[LinkFeedbackRow],
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        records::write_links(&tx, declared, ratings)?;
        tx.commit()?;
        Ok(())
    }

    /// Changes with every write of declared links or verdicts; `None` before the first.
    pub fn links_rev(&self) -> Result<Option<i64>> {
        records::links_rev(&self.conn.lock().unwrap())
    }

    // ── Saved ranges (B7) ──

    /// Saved ranges in creation order.
    pub fn saved_ranges(&self) -> Result<Vec<SavedRangeRow>> {
        records::read_ranges(&self.conn.lock().unwrap())
    }

    /// Replaces the saved ranges (one transaction); a name kept keeps its creation time.
    pub fn put_saved_ranges(&self, rows: &[SavedRangeRow]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        records::write_ranges(&tx, rows)?;
        tx.commit()?;
        Ok(())
    }

    // ── Pins and cluster labels (B6) ──

    pub fn layout_records(&self) -> Result<LayoutRows> {
        records::read_layout(&self.conn.lock().unwrap())
    }

    /// Replaces window pins, cluster pins and cluster labels (one transaction).
    pub fn put_layout_records(&self, rows: &LayoutRows) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        records::write_layout(&tx, rows)?;
        tx.commit()?;
        Ok(())
    }

    /// Raw access for tables other crates own (layout_pin, saved_range, tours …).
    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        Ok(f(&self.conn.lock().unwrap())?)
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        get_meta(&self.conn.lock().unwrap(), key)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        set_meta(&self.conn.lock().unwrap(), key, value)
    }

    // ── Threads ──

    /// Inserts or updates the thread row and inserts any comments not stored yet.
    pub fn put_thread(&self, t: &Thread) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let a = &t.anchor;
        tx.execute(
            "INSERT INTO thread (thread_id, change_id, commit_id, path, side, start_line, end_line, col_start, col_end,
                fp_text, fp_before, fp_after, status, kind, severity, assignee, created_by_json, created_at, updated_at, outdated_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
             ON CONFLICT(thread_id) DO UPDATE SET change_id = excluded.change_id, commit_id = excluded.commit_id,
                path = excluded.path, side = excluded.side, start_line = excluded.start_line, end_line = excluded.end_line,
                col_start = excluded.col_start, col_end = excluded.col_end, fp_text = excluded.fp_text,
                fp_before = excluded.fp_before, fp_after = excluded.fp_after, status = excluded.status,
                kind = excluded.kind, severity = excluded.severity, assignee = excluded.assignee,
                updated_at = excluded.updated_at, outdated_json = excluded.outdated_json",
            params![
                t.id,
                a.change,
                a.commit,
                a.path,
                a.side,
                a.start,
                a.end,
                a.cols.map(|c| c[0]),
                a.cols.map(|c| c[1]),
                a.fingerprint.text,
                a.fingerprint.before,
                a.fingerprint.after,
                t.status,
                t.kind,
                t.severity,
                t.assignee,
                serde_json::to_string(&t.created_by)?,
                t.created_at,
                t.updated_at,
                t.outdated.as_ref().map(serde_json::to_string).transpose()?,
            ],
        )?;
        for c in &t.comments {
            tx.execute(
                "INSERT INTO comment (comment_id, thread_id, author_json, body, created_at, edited_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(comment_id) DO UPDATE SET body = excluded.body, edited_at = excluded.edited_at",
                params![
                    c.id,
                    t.id,
                    serde_json::to_string(&c.author)?,
                    c.body,
                    c.created_at,
                    c.edited_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_thread(&self, id: &str) -> Result<Option<Thread>> {
        let conn = self.conn.lock().unwrap();
        let mut ts = query_threads(&conn, "WHERE thread_id = ?1", &[&id])?;
        Ok(ts.pop())
    }

    pub fn list_threads(&self, f: &ThreadFilter) -> Result<Vec<Thread>> {
        let conn = self.conn.lock().unwrap();
        let mut ts = query_threads(&conn, "", &[])?;
        ts.retain(|t| {
            f.status
                .as_ref()
                .is_none_or(|s| s.split(',').any(|s| s.trim() == t.status))
                && f.assignee
                    .as_ref()
                    .is_none_or(|a| t.assignee.as_deref() == Some(a.as_str()))
                && f.path.as_ref().is_none_or(|p| &t.anchor.path == p)
                && f.changes
                    .as_ref()
                    .is_none_or(|cs| cs.iter().any(|c| t.anchor.change.starts_with(c.as_str())))
        });
        Ok(ts)
    }

    pub fn add_anchor_history(
        &self,
        thread: &str,
        op: &str,
        commit: &str,
        start: u32,
        end: u32,
        outcome: &str,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO anchor_history (thread_id, op_id, commit_id, start_line, end_line, outcome, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![thread, op, commit, start, end, outcome, now_iso()],
        )?;
        Ok(())
    }

    // ── Edits ──

    pub fn put_edit(&self, row: &EditRow) -> Result<()> {
        let r = &row.result;
        let now = now_iso();
        self.conn.lock().unwrap().execute(
            "INSERT INTO edit (edit_id, actor_json, request_json, status, op_before, op_after, created_change, result_json, thread_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
             ON CONFLICT(edit_id) DO UPDATE SET status = excluded.status, op_before = excluded.op_before,
                op_after = excluded.op_after, created_change = excluded.created_change,
                result_json = excluded.result_json, updated_at = excluded.updated_at",
            params![
                r.id,
                serde_json::to_string(&r.actor)?,
                serde_json::to_string(&row.request)?,
                r.status,
                r.op_before,
                r.op_after,
                r.created,
                serde_json::to_string(r)?,
                row.thread,
                now
            ],
        )?;
        Ok(())
    }

    pub fn get_edit(&self, id: &str) -> Result<Option<EditRow>> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT request_json, result_json, thread_id FROM edit WHERE edit_id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((req, res, thread)) = row else {
            return Ok(None);
        };
        Ok(Some(EditRow {
            request: serde_json::from_str(&req)?,
            result: serde_json::from_str(res.as_deref().unwrap_or("null"))?,
            thread,
        }))
    }

    /// The applied edit whose operation is `op`, if any (attribution of our own ops).
    pub fn edit_by_op_after(&self, op: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT edit_id FROM edit WHERE op_after = ?1", [op], |r| {
                r.get(0)
            })
            .optional()?)
    }

    // ── Conflicts ──

    /// Records a conflict the first time it is seen, or reopens one resolved earlier that is back
    /// (e.g. an undo restored the conflicted commit). Returns true when inserted or reopened.
    pub fn conflict_seen(&self, c: &ConflictRecord) -> Result<bool> {
        let n = self.conn.lock().unwrap().execute(
            "INSERT INTO conflict_seen (commit_id, path, change_id, sides, first_seen_op, introduced_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(commit_id, path) DO UPDATE SET resolved_op = NULL WHERE resolved_op IS NOT NULL",
            params![c.commit, c.path, c.change, c.sides, c.first_seen_op, c.introduced_by],
        )?;
        Ok(n > 0)
    }

    /// The stored record of a conflict (first-seen op, introducing edit).
    pub fn conflict_record(&self, commit: &str, path: &str) -> Result<Option<ConflictRecord>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT change_id, commit_id, path, sides, first_seen_op, introduced_by FROM conflict_seen
                 WHERE commit_id = ?1 AND path = ?2",
                params![commit, path],
                conflict_row,
            )
            .optional()?)
    }

    /// Conflicts recorded as open (not resolved and not superseded).
    pub fn open_conflicts(&self) -> Result<Vec<ConflictRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut st = conn.prepare(
            "SELECT change_id, commit_id, path, sides, first_seen_op, introduced_by FROM conflict_seen
             WHERE resolved_op IS NULL ORDER BY path, commit_id",
        )?;
        let rows = st.query_map([], conflict_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Marks a conflict gone at `op` (resolved, or its commit was rewritten/abandoned).
    pub fn conflict_gone(&self, commit: &str, path: &str, op: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE conflict_seen SET resolved_op = ?3 WHERE commit_id = ?1 AND path = ?2 AND resolved_op IS NULL",
            params![commit, path, op],
        )?;
        Ok(())
    }

    // ── Review marks ──

    pub fn set_review_mark(&self, change: &str, reviewer: &str, commit: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO review_mark (change_id, reviewer, commit_id, marked_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(change_id, reviewer) DO UPDATE SET commit_id = excluded.commit_id, marked_at = excluded.marked_at",
            params![change, reviewer, commit, now_iso()],
        )?;
        Ok(())
    }

    // ── Agent sessions ──

    pub fn put_session(&self, s: &SessionRow) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO agent_session (session_id, workspace, agent_json, bound_at, last_seen, activity)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(session_id) DO UPDATE SET workspace = excluded.workspace, agent_json = excluded.agent_json,
                last_seen = excluded.last_seen, activity = excluded.activity",
            params![
                s.session_id,
                s.workspace,
                serde_json::to_string(&s.agent)?,
                s.bound_at,
                s.last_seen,
                s.activity
            ],
        )?;
        Ok(())
    }

    pub fn sessions(&self) -> Result<Vec<SessionRow>> {
        let conn = self.conn.lock().unwrap();
        let mut st = conn.prepare(
            "SELECT session_id, workspace, agent_json, bound_at, last_seen, activity FROM agent_session ORDER BY last_seen DESC",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (session_id, workspace, agent, bound_at, last_seen, activity) = row?;
            out.push(SessionRow {
                session_id,
                workspace,
                agent: serde_json::from_str(&agent)?,
                bound_at,
                last_seen,
                activity,
            });
        }
        Ok(out)
    }

    // ── Agent permissions (DESIGN §10) ──

    /// Sets the toggles one UI session chose for one agent (`agent` "*" = every agent). `None`
    /// leaves a toggle as it was.
    pub fn put_permission(
        &self,
        ui_session: &str,
        agent: &str,
        edit: Option<bool>,
        move_camera: Option<bool>,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO agent_permission (ui_session, agent, edit, move_camera, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(ui_session, agent) DO UPDATE SET
                edit = coalesce(excluded.edit, agent_permission.edit),
                move_camera = coalesce(excluded.move_camera, agent_permission.move_camera),
                updated_at = excluded.updated_at",
            params![ui_session, agent, edit, move_camera, now_iso()],
        )?;
        Ok(())
    }

    /// Every stored toggle row, newest first.
    pub fn permissions(&self) -> Result<Vec<PermissionRow>> {
        let conn = self.conn.lock().unwrap();
        let mut st = conn.prepare(
            "SELECT ui_session, agent, edit, move_camera, updated_at FROM agent_permission
             ORDER BY updated_at DESC",
        )?;
        let rows = st.query_map([], |r| {
            Ok(PermissionRow {
                ui_session: r.get(0)?,
                agent: r.get(1)?,
                edit: r.get(2)?,
                move_camera: r.get(3)?,
                updated_at: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ── Tours ──

    /// Stores a tour (http.ts `Tour`); `steps` are `TourStep` JSON values.
    pub fn put_tour(
        &self,
        id: &str,
        title: &str,
        by: &Actor,
        created_at: &str,
        steps: &[serde_json::Value],
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO tour (tour_id, title, created_by_json, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(tour_id) DO UPDATE SET title = excluded.title",
            params![id, title, serde_json::to_string(by)?, created_at],
        )?;
        tx.execute("DELETE FROM tour_step WHERE tour_id = ?1", [id])?;
        for (i, step) in steps.iter().enumerate() {
            let lines = step["lines"].as_array();
            let line = |k: usize| lines.and_then(|l| l.get(k)).and_then(|v| v.as_i64());
            tx.execute(
                "INSERT INTO tour_step (tour_id, ord, target, start_line, end_line, note)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id,
                    i as i64,
                    serde_json::to_string(&step["target"])?,
                    line(0),
                    line(1),
                    step["note"].as_str().unwrap_or_default()
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Stored tours as http.ts `Tour` JSON, newest first.
    pub fn tours(&self) -> Result<Vec<serde_json::Value>> {
        let conn = self.conn.lock().unwrap();
        let mut st = conn.prepare(
            "SELECT tour_id, title, created_by_json, created_at FROM tour ORDER BY created_at DESC, rowid DESC",
        )?;
        let heads = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut sst = conn.prepare_cached(
            "SELECT target, start_line, end_line, note FROM tour_step WHERE tour_id = ?1 ORDER BY ord",
        )?;
        let mut out = Vec::new();
        for (id, title, by, created_at) in heads {
            let steps = sst
                .query_map([&id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<i64>>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let steps: Vec<serde_json::Value> = steps
                .into_iter()
                .map(|(target, s, e, note)| {
                    let mut v = serde_json::json!({
                        "target": serde_json::from_str::<serde_json::Value>(&target).unwrap_or(serde_json::Value::Null),
                        "note": note,
                    });
                    if let (Some(s), Some(e)) = (s, e) {
                        v["lines"] = serde_json::json!([s, e]);
                    }
                    v
                })
                .collect();
            out.push(serde_json::json!({
                "id": id,
                "title": title,
                "by": serde_json::from_str::<serde_json::Value>(&by)?,
                "createdAt": created_at,
                "steps": steps,
            }));
        }
        Ok(out)
    }

    // ── Event log ──

    /// Appends one event (its JSON with a `type` field); returns its seq.
    pub fn append_event(&self, kind: &str, payload_json: &str) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO event_log (at, type, payload_json) VALUES (?1, ?2, ?3)",
            params![now_iso(), kind, payload_json],
        )?;
        Ok(conn.last_insert_rowid() as u64)
    }

    /// Events with seq > `after`, oldest first, at most `limit`.
    pub fn events_after(
        &self,
        after: u64,
        kinds: Option<&[String]>,
        limit: usize,
    ) -> Result<Vec<(u64, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut st = conn.prepare_cached(
            "SELECT seq, type, payload_json FROM event_log WHERE seq > ?1 ORDER BY seq LIMIT ?2",
        )?;
        // Filtering by kind in Rust keeps one prepared statement; the log is append-only and small
        // per poll.
        let rows = st.query_map(params![after as i64, (limit * 4).max(64) as i64], |r| {
            Ok((
                r.get::<_, i64>(0)? as u64,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, kind, payload) = row?;
            if kinds.is_none_or(|ks| ks.iter().any(|k| k == &kind)) {
                out.push((seq, payload));
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Retention: deletes all but the newest `keep_max` events, and events older than `cutoff`
    /// (ISO-8601, as `at`) except the newest `keep_min`. Seqs are never reused (AUTOINCREMENT), so
    /// cursors stay monotonic. Returns the number of events deleted.
    pub fn prune_events(&self, keep_max: u64, keep_min: u64, cutoff: &str) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let last: i64 = conn.query_row("SELECT coalesce(max(seq), 0) FROM event_log", [], |r| {
            r.get(0)
        })?;
        let n = conn.execute(
            "DELETE FROM event_log WHERE seq <= ?1 OR (seq <= ?2 AND at < ?3)",
            params![last - keep_max as i64, last - keep_min as i64, cutoff],
        )?;
        Ok(n)
    }

    /// The oldest seq still in the log (None when it is empty).
    pub fn first_seq(&self) -> Result<Option<u64>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT min(seq) FROM event_log", [], |r| {
                r.get::<_, Option<i64>>(0)
            })?
            .map(|s| s as u64))
    }

    pub fn last_seq(&self) -> Result<u64> {
        Ok(self.conn.lock().unwrap().query_row(
            "SELECT coalesce(max(seq), 0) FROM event_log",
            [],
            |r| r.get::<_, i64>(0),
        )? as u64)
    }
}

fn conflict_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ConflictRecord> {
    Ok(ConflictRecord {
        change: r.get(0)?,
        commit: r.get(1)?,
        path: r.get(2)?,
        sides: r.get(3)?,
        first_seen_op: r.get(4)?,
        introduced_by: r.get(5)?,
    })
}

fn query_threads(
    conn: &Connection,
    where_clause: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<Vec<Thread>> {
    let sql = format!(
        "SELECT thread_id, change_id, commit_id, path, side, start_line, end_line, col_start, col_end,
            fp_text, fp_before, fp_after, status, kind, severity, assignee, created_by_json, created_at,
            updated_at, outdated_json FROM thread {where_clause} ORDER BY created_at, thread_id"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(args, |r| {
        let cols = match (r.get::<_, Option<u32>>(7)?, r.get::<_, Option<u32>>(8)?) {
            (Some(a), Some(b)) => Some([a, b]),
            _ => None,
        };
        Ok((
            Thread {
                id: r.get(0)?,
                anchor: cv_contracts::review::Anchor {
                    change: r.get(1)?,
                    commit: r.get(2)?,
                    path: r.get(3)?,
                    side: r.get(4)?,
                    start: r.get(5)?,
                    end: r.get(6)?,
                    cols,
                    fingerprint: cv_contracts::review::Fingerprint {
                        text: r.get(9)?,
                        before: r.get(10)?,
                        after: r.get(11)?,
                    },
                },
                status: r.get(12)?,
                kind: r.get(13)?,
                severity: r.get(14)?,
                assignee: r.get(15)?,
                created_by: Actor::External,
                created_at: r.get(17)?,
                updated_at: r.get(18)?,
                comments: vec![],
                produced_edits: vec![],
                outdated: None,
            },
            r.get::<_, String>(16)?,
            r.get::<_, Option<String>>(19)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (mut t, by, outdated) = row?;
        t.created_by = serde_json::from_str(&by)?;
        t.outdated = outdated.map(|o| serde_json::from_str(&o)).transpose()?;
        out.push(t);
    }
    let mut cst = conn.prepare_cached(
        "SELECT comment_id, author_json, body, created_at, edited_at FROM comment WHERE thread_id = ?1
         ORDER BY created_at, rowid",
    )?;
    let mut est = conn.prepare_cached(
        "SELECT edit_id FROM edit WHERE thread_id = ?1 AND status IN ('applied', 'undone') ORDER BY created_at",
    )?;
    for t in &mut out {
        let cs = cst.query_map([&t.id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?;
        for c in cs {
            let (id, author, body, created_at, edited_at) = c?;
            t.comments.push(Comment {
                id,
                author: serde_json::from_str(&author)?,
                body,
                created_at,
                edited_at,
            });
        }
        t.produced_edits = est
            .query_map([&t.id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cv_contracts::review::{Anchor, Fingerprint};

    fn thread(id: &str) -> Thread {
        Thread {
            id: id.into(),
            anchor: Anchor {
                change: "kxwq".into(),
                commit: "c0ffee".into(),
                path: "src/a.py".into(),
                side: "new".into(),
                start: 3,
                end: 4,
                cols: None,
                fingerprint: Fingerprint::default(),
            },
            status: "open".into(),
            kind: "comment".into(),
            severity: None,
            assignee: Some("agent".into()),
            created_by: Actor::human("you"),
            created_at: now_iso(),
            updated_at: now_iso(),
            comments: vec![Comment {
                id: format!("{id}-c1"),
                author: Actor::human("you"),
                body: "hi".into(),
                created_at: now_iso(),
                edited_at: None,
            }],
            produced_edits: vec![],
            outdated: None,
        }
    }

    #[test]
    fn parse_cache_round_trip_replace_and_trim() {
        let tmp = tempfile::tempdir().unwrap();
        let db = IndexDb::open(&tmp.path().join("index.db")).unwrap();
        db.parse_cache_put(&[
            ("a.py", "b1", "py@1#1", b"one"),
            ("c.py", "b2", "py@1#1", b"two"),
        ])
        .unwrap();
        let got = db
            .parse_cache_get(&[
                ("a.py", "b1", "py@1#1"),
                ("a.py", "b1", "py@2#1"),
                ("x", "b1", "py@1#1"),
            ])
            .unwrap();
        assert_eq!(got, vec![Some(b"one".to_vec()), None, None]);
        // A new pack version replaces the row.
        db.parse_cache_put(&[("a.py", "b1", "py@2#1", b"uno")])
            .unwrap();
        assert_eq!(
            db.parse_cache_get(&[("a.py", "b1", "py@2#1")]).unwrap(),
            vec![Some(b"uno".to_vec())]
        );
        assert_eq!(db.parse_cache_trim(&[("a.py", "b1")], 5).unwrap(), 0);
        assert_eq!(db.parse_cache_trim(&[("a.py", "b1")], 1).unwrap(), 1);
        assert_eq!(
            db.parse_cache_get(&[("c.py", "b2", "py@1#1")]).unwrap(),
            vec![None]
        );
        // Reopening keeps it (and an older database without the table gets it).
        drop(db);
        let db = IndexDb::open(&tmp.path().join("index.db")).unwrap();
        assert_eq!(
            db.parse_cache_get(&[("a.py", "b1", "py@2#1")])
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn migrations_are_the_schema_files() {
        let db = ReviewDb::in_memory().unwrap();
        assert_eq!(db.meta("schema_version").unwrap().as_deref(), Some("2"));
        let tables: Vec<String> = db
            .with(|c| {
                let mut st = c.prepare(
                    "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
                )?;
                st.query_map([], |r| r.get(0))?.collect()
            })
            .unwrap();
        for t in [
            "thread",
            "comment",
            "edit",
            "conflict_seen",
            "event_log",
            "agent_session",
            "layout_pin",
            "cluster_pin",
            "cluster_label",
            "saved_range",
            "link_feedback",
            "declared_link",
        ] {
            assert!(tables.iter().any(|x| x == t), "{t} missing: {tables:?}");
        }
    }

    #[test]
    fn threads_round_trip_and_filter() {
        let db = ReviewDb::in_memory().unwrap();
        let mut t = thread("t1");
        db.put_thread(&t).unwrap();
        db.put_thread(&thread("t2")).unwrap();
        t.status = "resolved".into();
        t.comments.push(Comment {
            id: "t1-c2".into(),
            author: Actor::agent("claude", Some("gym")),
            body: "done".into(),
            created_at: now_iso(),
            edited_at: None,
        });
        db.put_thread(&t).unwrap();
        let back = db.get_thread("t1").unwrap().unwrap();
        assert_eq!(back, t);
        let open = db
            .list_threads(&ThreadFilter {
                status: Some("open,in-progress".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, "t2");
    }

    #[test]
    fn event_log_is_append_only_with_cursors() {
        let db = ReviewDb::in_memory().unwrap();
        assert_eq!(db.last_seq().unwrap(), 0);
        let a = db
            .append_event("op.changed", r#"{"type":"op.changed"}"#)
            .unwrap();
        let b = db
            .append_event("thread.upserted", r#"{"type":"thread.upserted"}"#)
            .unwrap();
        assert!(b > a);
        assert_eq!(db.events_after(a, None, 10).unwrap().len(), 1);
        let only = db
            .events_after(0, Some(&["thread.upserted".to_string()]), 10)
            .unwrap();
        assert_eq!(only, vec![(b, r#"{"type":"thread.upserted"}"#.to_string())]);
    }

    #[test]
    fn event_retention_keeps_seqs_monotonic() {
        let db = ReviewDb::in_memory().unwrap();
        assert_eq!(db.first_seq().unwrap(), None);
        for _ in 0..10 {
            db.append_event("op.changed", "{}").unwrap();
        }
        // By count: the newest 4.
        assert_eq!(
            db.prune_events(4, 1, "1970-01-01T00:00:00.000Z").unwrap(),
            6
        );
        assert_eq!(db.first_seq().unwrap(), Some(7));
        // By age (everything is older than the cutoff): all but the newest 2.
        assert_eq!(
            db.prune_events(100, 2, "9999-01-01T00:00:00.000Z").unwrap(),
            2
        );
        assert_eq!(db.first_seq().unwrap(), Some(9));
        assert_eq!(db.last_seq().unwrap(), 10);
        assert_eq!(
            db.append_event("op.changed", "{}").unwrap(),
            11,
            "seqs are never reused"
        );
    }

    #[test]
    fn a_conflict_that_comes_back_is_open_again() {
        let dir = tempfile::tempdir().unwrap();
        let db = ReviewDb::open(&StorePaths::in_dir(dir.path()).review_db).unwrap();
        let rec = cv_contracts::review::ConflictRecord {
            change: "kxyz".into(),
            commit: "c1".into(),
            path: "a.py".into(),
            sides: 2,
            first_seen_op: "op1".into(),
            introduced_by: None,
        };
        assert!(db.conflict_seen(&rec).unwrap());
        assert!(
            !db.conflict_seen(&rec).unwrap(),
            "seen again while open: no change"
        );
        db.conflict_gone("c1", "a.py", "op2").unwrap();
        assert!(db.open_conflicts().unwrap().is_empty());
        // An undo restores the conflicted commit: it must be listed as open again, not stay
        // resolved (which made every sync announce it as new while the list never showed it).
        assert!(db.conflict_seen(&rec).unwrap(), "reopened");
        let open = db.open_conflicts().unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].first_seen_op, "op1", "keeps its first sighting");
    }

    #[test]
    fn permissions_and_tours_survive_reopen_and_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let paths = StorePaths::in_dir(dir.path());
        let db = ReviewDb::open(&paths.review_db).unwrap();
        db.put_permission("ui1", "*", Some(false), None).unwrap();
        db.put_permission("ui1", "*", None, Some(true)).unwrap();
        db.put_permission("ui2", "claude", None, Some(false))
            .unwrap();
        let by = Actor::agent("claude", Some("gym"));
        let steps = vec![
            serde_json::json!({ "target": { "path": "a.py", "line": 3 }, "lines": [3, 5], "note": "start" }),
            serde_json::json!({ "target": "h:env:X", "note": "hub" }),
        ];
        db.put_tour("tour1", "Walk", &by, "2026-10-18T00:00:00.000Z", &steps)
            .unwrap();
        drop(db);
        let db = ReviewDb::open(&paths.review_db).unwrap();
        let mut rows = db.permissions().unwrap();
        rows.sort_by(|a, b| a.ui_session.cmp(&b.ui_session));
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[0].edit, rows[0].move_camera),
            (Some(false), Some(true))
        );
        assert_eq!((rows[1].edit, rows[1].move_camera), (None, Some(false)));
        let tours = db.tours().unwrap();
        assert_eq!(tours.len(), 1);
        assert_eq!(tours[0]["by"]["workspace"], "gym");
        assert_eq!(tours[0]["steps"], serde_json::Value::Array(steps));
        // A review.db from before the table existed gets it on open.
        db.with(|c| c.execute_batch("DROP TABLE agent_permission"))
            .unwrap();
        drop(db);
        let db = ReviewDb::open(&paths.review_db).unwrap();
        assert!(db.permissions().unwrap().is_empty());
    }

    #[test]
    fn deleting_index_db_never_touches_review_db() {
        let dir = tempfile::tempdir().unwrap();
        let paths = StorePaths::in_dir(dir.path());
        let review = ReviewDb::open(&paths.review_db).unwrap();
        review.put_thread(&thread("t1")).unwrap();
        let index = IndexDb::open(&paths.index_db).unwrap();
        index.set_meta("last_synced_op", "abc").unwrap();
        drop(index);
        drop(review);
        let before = std::fs::read(&paths.review_db).unwrap();
        std::fs::remove_file(&paths.index_db).unwrap();
        for ext in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", paths.index_db.display()));
        }
        // Re-indexing recreates index.db from scratch; review.db is byte-for-byte unchanged.
        let index = IndexDb::open(&paths.index_db).unwrap();
        assert_eq!(index.meta("last_synced_op").unwrap(), None);
        assert_eq!(std::fs::read(&paths.review_db).unwrap(), before);
        let review = ReviewDb::open(&paths.review_db).unwrap();
        assert_eq!(review.get_thread("t1").unwrap().unwrap().comments.len(), 1);
    }

    /// A version-1 review.db as earlier builds left it: the v1 schema, with pins, labels, saved
    /// ranges, declared links and verdicts as JSON in meta.
    fn v1_review_db(path: &Path) {
        let v1 = REVIEW_SCHEMA
            .split("-- Drags and pins")
            .next()
            .unwrap()
            .to_string()
            + "
CREATE TABLE layout_pin (view_key TEXT NOT NULL, item_key TEXT NOT NULL, x REAL NOT NULL, y REAL NOT NULL,
  updated_at TEXT NOT NULL, PRIMARY KEY (view_key, item_key)) STRICT, WITHOUT ROWID;
CREATE TABLE saved_range (name TEXT PRIMARY KEY, revset TEXT NOT NULL, fileset TEXT, options_json TEXT,
  created_by_json TEXT NOT NULL, created_at TEXT NOT NULL) STRICT;
CREATE TABLE link_feedback (link_key TEXT PRIMARY KEY, verdict TEXT NOT NULL CHECK (verdict IN ('confirmed', 'rejected')),
  actor_json TEXT NOT NULL, note TEXT, at TEXT NOT NULL) STRICT, WITHOUT ROWID;
CREATE TABLE declared_link (id TEXT PRIMARY KEY, kind TEXT NOT NULL, from_ref TEXT NOT NULL, to_ref TEXT NOT NULL,
  label TEXT NOT NULL, note TEXT, actor_json TEXT NOT NULL, at TEXT NOT NULL) STRICT;
CREATE TABLE cluster_label (members_hash TEXT PRIMARY KEY, members_json TEXT NOT NULL, label TEXT NOT NULL,
  summary TEXT, source TEXT NOT NULL CHECK (source IN ('agent', 'human')), at TEXT NOT NULL) STRICT;
CREATE TABLE event_log (seq INTEGER PRIMARY KEY AUTOINCREMENT, at TEXT NOT NULL, type TEXT NOT NULL,
  payload_json TEXT NOT NULL) STRICT;
INSERT INTO meta (key, value) VALUES ('schema_version', '1');
";
        let c = Connection::open(path).unwrap();
        c.execute_batch(&v1).unwrap();
        let links = serde_json::json!({
            "declared": [{
                "id": "d1", "kind": "call", "from": { "path": "a.py", "line": 3 },
                "to": { "hub": "env:X" }, "label": "uses X", "by": { "kind": "human", "name": "you" },
                "at": "2026-10-18T00:00:00.000Z"
            }],
            "ratings": [
                { "key": "call|a.py:3|b.py:9", "id": "l4", "verdict": "rejected", "note": "nope",
                  "by": { "kind": "human", "name": "you" }, "at": "2026-10-18T00:00:01.000Z" },
                { "key": "call|a.py:5|b.py:1", "id": "l7", "verdict": "confirmed",
                  "by": { "kind": "human", "name": "you" }, "at": "2026-10-18T00:00:02.000Z",
                  "link": { "id": "l7", "kind": "call" } }
            ]
        });
        let ranges = serde_json::json!([
            { "name": "mine", "revset": "trunk()..@", "createdBy": { "kind": "human", "name": "you" } },
            { "name": "agent", "revset": "@-", "fileset": "src/", "options": { "contextLines": 3 },
              "createdBy": { "kind": "agent", "name": "claude" } }
        ]);
        let layout = serde_json::json!({
            "pins": { "src/a.py#f": [1.5, 2.5], "src/b.py#g": [3.0, 4.0] },
            "pinMissed": { "src/b.py#g": 4 },
            "clusterPins": [{ "key": "cluster:core", "lane": "code", "members": ["src/a.py#f"], "x": 10.0, "y": 20.0, "missed": 2 }],
            "labels": [
                { "lane": "code", "members": ["src/a.py#f", "src/b.py#g"], "label": "Core", "labelSource": "human" },
                { "lane": "tests", "members": ["t.py#t"], "label": "Tests", "labelSource": "agent", "summary": "the tests", "missed": 3 }
            ]
        });
        for (k, v) in [
            (records::LINKS_KEY, links.to_string()),
            (records::RANGES_KEY, ranges.to_string()),
            (records::LAYOUT_KEY, layout.to_string()),
        ] {
            c.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)",
                params![k, v],
            )
            .unwrap();
        }
    }

    #[test]
    fn version_1_json_records_move_into_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("review.db");
        v1_review_db(&path);
        let db = ReviewDb::open(&path).unwrap();
        assert_eq!(db.meta("schema_version").unwrap().as_deref(), Some("2"));
        for k in [records::LINKS_KEY, records::RANGES_KEY, records::LAYOUT_KEY] {
            assert_eq!(db.meta(k).unwrap(), None, "{k} left in meta");
        }
        let (declared, ratings) = db.link_records().unwrap();
        assert_eq!(declared.len(), 1);
        assert_eq!(declared[0].to_json, r#"{"hub":"env:X"}"#);
        assert_eq!(
            ratings
                .iter()
                .map(|r| r.link_id.as_str())
                .collect::<Vec<_>>(),
            ["l4", "l7"]
        );
        assert_eq!(ratings[0].note.as_deref(), Some("nope"));
        assert_eq!(ratings[0].link_json, None);
        assert_eq!(
            ratings[1].link_json.as_deref(),
            Some(r#"{"id":"l7","kind":"call"}"#)
        );
        assert!(db.links_rev().unwrap().is_some());
        let ranges = db.saved_ranges().unwrap();
        assert_eq!(
            ranges.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["mine", "agent"]
        );
        assert_eq!(ranges[1].fileset.as_deref(), Some("src/"));
        assert_eq!(
            ranges[1].options_json.as_deref(),
            Some(r#"{"contextLines":3}"#)
        );
        let layout = db.layout_records().unwrap();
        assert_eq!(
            layout.pins,
            vec![
                WindowPinRow {
                    key: "src/a.py#f".into(),
                    x: 1.5,
                    y: 2.5,
                    missed: 0
                },
                WindowPinRow {
                    key: "src/b.py#g".into(),
                    x: 3.0,
                    y: 4.0,
                    missed: 4
                },
            ]
        );
        assert_eq!(layout.cluster_pins[0].missed, 2);
        assert_eq!(layout.labels.len(), 2);
        assert_eq!(layout.labels[1].summary.as_deref(), Some("the tests"));
        assert_eq!(layout.labels[1].missed, 3);
        assert_eq!(layout.labels[1].source, "agent");
        // Reopening changes nothing.
        drop(db);
        let db = ReviewDb::open(&path).unwrap();
        assert_eq!(db.layout_records().unwrap(), layout);
        assert_eq!(db.saved_ranges().unwrap(), ranges);
    }

    #[test]
    fn records_keep_order_and_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("review.db");
        let db = ReviewDb::open(&path).unwrap();
        assert_eq!(db.links_rev().unwrap(), None);
        let fb = |k: &str| LinkFeedbackRow {
            key: k.into(),
            link_id: "l1".into(),
            verdict: "confirmed".into(),
            note: None,
            actor_json: "{}".into(),
            at: now_iso(),
            link_json: None,
        };
        db.put_link_records(&[], &[fb("z"), fb("a")]).unwrap();
        let r1 = db.links_rev().unwrap().unwrap();
        db.put_link_records(&[], &[fb("a"), fb("z")]).unwrap();
        assert!(db.links_rev().unwrap().unwrap() > r1);
        let range = |n: &str| SavedRangeRow {
            name: n.into(),
            revset: "@".into(),
            fileset: None,
            options_json: None,
            created_by_json: "{}".into(),
        };
        db.put_saved_ranges(&[range("b"), range("a")]).unwrap();
        let first: String = db
            .with(|c| {
                c.query_row(
                    "SELECT created_at FROM saved_range WHERE name = 'b'",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert!(first.ends_with('Z'), "{first}");
        // Backdate it, so a rewrite would show whatever the clock says now.
        let created = "2001-02-03T04:05:06.007Z".to_string();
        db.with(|c| {
            c.execute(
                "UPDATE saved_range SET created_at = ?1 WHERE name = 'b'",
                [&created],
            )
        })
        .unwrap();
        db.put_saved_ranges(&[range("b"), range("a"), range("c")])
            .unwrap();
        let rows = LayoutRows {
            pins: vec![WindowPinRow {
                key: "k".into(),
                x: 1.0,
                y: 2.0,
                missed: 1,
            }],
            cluster_pins: vec![ClusterPinRow {
                key: "cluster:x".into(),
                lane: "code".into(),
                members: vec!["k".into()],
                x: 0.0,
                y: 0.0,
                missed: 0,
            }],
            labels: vec![
                ClusterLabelRow {
                    lane: "code".into(),
                    members: vec!["k".into()],
                    label: "B".into(),
                    source: "human".into(),
                    summary: None,
                    missed: 0,
                },
                ClusterLabelRow {
                    lane: "code".into(),
                    members: vec!["k".into()],
                    label: "A".into(),
                    source: "agent".into(),
                    summary: Some("s".into()),
                    missed: 2,
                },
            ],
        };
        db.put_layout_records(&rows).unwrap();
        drop(db);
        let db = ReviewDb::open(&path).unwrap();
        let (_, ratings) = db.link_records().unwrap();
        assert_eq!(
            ratings.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
            ["a", "z"]
        );
        let names: Vec<String> = db
            .saved_ranges()
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(names, ["b", "a", "c"]);
        let created2: String = db
            .with(|c| {
                c.query_row(
                    "SELECT created_at FROM saved_range WHERE name = 'b'",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert_eq!(created, created2);
        assert_eq!(db.layout_records().unwrap(), rows);
    }

    #[test]
    fn paths_follow_xdg_layout() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".jj/repo")).unwrap();
        let p = StorePaths::for_repo(dir.path());
        let id = repo_id(dir.path());
        assert!(p.review_db.ends_with(format!("code-viewer/{id}/review.db")));
        assert!(p.index_db.ends_with(format!("code-viewer/{id}/index.db")));
        assert_eq!(iso_from_ms(0), "1970-01-01T00:00:00.000Z");
    }
}
