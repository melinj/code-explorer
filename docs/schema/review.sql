-- review.db: human and agent data (threads, edits, pins, saved ranges, the event log).
-- Never touched by re-indexing; back it up. Location: $XDG_DATA_HOME/code-viewer/<repo-id>/review.db.
-- Owner: B8 (review & edits), except layout_pin/cluster_pin/saved_range (B6/B7) and event_log (B7).
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
) STRICT;

-- Threads anchor to a change (stable across rewrites) plus the commit last confirmed.
CREATE TABLE thread (
  thread_id       TEXT PRIMARY KEY,
  change_id       TEXT NOT NULL,
  commit_id       TEXT NOT NULL,
  path            TEXT NOT NULL,
  side            TEXT NOT NULL CHECK (side IN ('new', 'old')),
  start_line      INTEGER NOT NULL,
  end_line        INTEGER NOT NULL,
  col_start       INTEGER,
  col_end         INTEGER,
  fp_text         TEXT NOT NULL,  -- hash of anchored lines
  fp_before       TEXT NOT NULL,  -- hash of the 2 lines above
  fp_after        TEXT NOT NULL,  -- hash of the 2 lines below
  status          TEXT NOT NULL CHECK (status IN ('open', 'in-progress', 'addressed', 'resolved', 'outdated', 'wontfix')),
  kind            TEXT NOT NULL CHECK (kind IN ('comment', 'suggestion', 'question', 'task', 'finding')),
  severity        TEXT CHECK (severity IN ('info', 'minor', 'major', 'blocker')),
  assignee        TEXT CHECK (assignee IN ('agent', 'human')),
  created_by_json TEXT NOT NULL,  -- Actor
  created_at      TEXT NOT NULL,
  updated_at      TEXT NOT NULL,
  outdated_json   TEXT
) STRICT;
CREATE INDEX thread_change ON thread(change_id);
CREATE INDEX thread_queue ON thread(status, assignee);
CREATE INDEX thread_path ON thread(path);

CREATE TABLE comment (
  comment_id  TEXT PRIMARY KEY,
  thread_id   TEXT NOT NULL REFERENCES thread(thread_id) ON DELETE CASCADE,
  author_json TEXT NOT NULL,
  body        TEXT NOT NULL,  -- markdown; ```suggestion blocks allowed
  created_at  TEXT NOT NULL,
  edited_at   TEXT
) STRICT;
CREATE INDEX comment_thread ON comment(thread_id, created_at);

CREATE TABLE anchor_history (
  thread_id  TEXT NOT NULL REFERENCES thread(thread_id) ON DELETE CASCADE,
  op_id      TEXT NOT NULL,
  commit_id  TEXT NOT NULL,
  start_line INTEGER NOT NULL,
  end_line   INTEGER NOT NULL,
  outcome    TEXT NOT NULL CHECK (outcome IN ('moved', 'unchanged', 'outdated')),
  at         TEXT NOT NULL
) STRICT;
CREATE INDEX anchor_history_thread ON anchor_history(thread_id, at);

-- "I reviewed this change at this commit": drives "changed since review" and interdiff.
CREATE TABLE review_mark (
  change_id TEXT NOT NULL,
  reviewer  TEXT NOT NULL,
  commit_id TEXT NOT NULL,
  marked_at TEXT NOT NULL,
  PRIMARY KEY (change_id, reviewer)
) STRICT, WITHOUT ROWID;

CREATE TABLE edit (
  edit_id        TEXT PRIMARY KEY,
  actor_json     TEXT NOT NULL,
  request_json   TEXT NOT NULL,  -- EditRequest
  status         TEXT NOT NULL CHECK (status IN ('previewed', 'applied', 'rejected', 'failed', 'undone')),
  op_before      TEXT,
  op_after       TEXT,
  created_change TEXT,
  result_json    TEXT,           -- EditResult
  thread_id      TEXT REFERENCES thread(thread_id) ON DELETE SET NULL,
  created_at     TEXT NOT NULL,
  updated_at     TEXT NOT NULL
) STRICT;
CREATE INDEX edit_op_after ON edit(op_after);
CREATE INDEX edit_thread ON edit(thread_id);

CREATE TABLE conflict_seen (
  commit_id     TEXT NOT NULL,
  path          TEXT NOT NULL,
  change_id     TEXT NOT NULL,
  sides         INTEGER NOT NULL,
  first_seen_op TEXT NOT NULL,
  introduced_by TEXT REFERENCES edit(edit_id) ON DELETE SET NULL,
  resolved_op   TEXT,
  PRIMARY KEY (commit_id, path)
) STRICT, WITHOUT ROWID;

-- Drags and pins, keyed by revision-independent keys so they survive history moving on.
-- Window pins: view_key "snapshot", item_key a WindowKey. `missed` counts consecutive full
-- indexes without that key (dropped after 20). Cluster pins are in cluster_pin (they are matched
-- by lane and members, not by a key).
CREATE TABLE layout_pin (
  view_key   TEXT NOT NULL,  -- "snapshot" or "range:<revset>"
  item_key   TEXT NOT NULL,  -- WindowKey
  x          REAL NOT NULL,
  y          REAL NOT NULL,
  missed     INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (view_key, item_key)
) STRICT, WITHOUT ROWID;

-- Pinned clusters, found again by lane and member WindowKeys (Jaccard >= 0.6), in pin order.
CREATE TABLE cluster_pin (
  ord          INTEGER PRIMARY KEY,
  item_key     TEXT NOT NULL,  -- "cluster:<label when pinned>" (informative)
  lane         TEXT NOT NULL,
  members_json TEXT NOT NULL,  -- sorted member WindowKeys
  x            REAL NOT NULL,
  y            REAL NOT NULL,
  missed       INTEGER NOT NULL DEFAULT 0,  -- consecutive full indexes without a matching cluster
  updated_at   TEXT NOT NULL
) STRICT;

-- In creation order (rowid).
CREATE TABLE saved_range (
  name            TEXT PRIMARY KEY,
  revset          TEXT NOT NULL,
  fileset         TEXT,
  options_json    TEXT,  -- RangeOptions
  created_by_json TEXT NOT NULL,
  created_at      TEXT NOT NULL
) STRICT;

-- Human/agent verdicts on heuristic links, and links the agent declared by hand.
CREATE TABLE link_feedback (
  link_key   TEXT PRIMARY KEY,  -- "<kind>|<from path:line>|<to path:line or hub id>"
  link_id    TEXT NOT NULL,     -- the link id it was rated under (ids are per view; informative)
  verdict    TEXT NOT NULL CHECK (verdict IN ('confirmed', 'rejected')),
  actor_json TEXT NOT NULL,
  note       TEXT,
  at         TEXT NOT NULL,
  link_json  TEXT,              -- the Link as rated: a confirmed link a later index drops is re-added from it
  ord        INTEGER NOT NULL   -- rating order (a re-rated link moves last)
) STRICT, WITHOUT ROWID;

-- In declaration order (rowid); a re-declared id moves last.
CREATE TABLE declared_link (
  id         TEXT PRIMARY KEY,
  kind       TEXT NOT NULL,
  from_ref   TEXT NOT NULL,  -- {"path","line"}
  to_ref     TEXT NOT NULL,  -- {"path","line"} or {"hub"}
  label      TEXT NOT NULL,
  note       TEXT,
  actor_json TEXT NOT NULL,
  at         TEXT NOT NULL
) STRICT;

-- Human and agent cluster labels, found again by lane and members (Jaccard >= 0.6), in order.
CREATE TABLE cluster_label (
  ord          INTEGER PRIMARY KEY,
  members_hash TEXT NOT NULL,  -- hash of the lane and sorted member keys at labelling time
  lane         TEXT NOT NULL,
  members_json TEXT NOT NULL,  -- rewritten to the matched cluster after every full index (drift)
  label        TEXT NOT NULL,
  summary      TEXT,
  source       TEXT NOT NULL CHECK (source IN ('agent', 'human')),
  missed       INTEGER NOT NULL DEFAULT 0,  -- consecutive full indexes without a matching cluster
  at           TEXT NOT NULL
) STRICT;
CREATE INDEX cluster_label_members ON cluster_label(members_hash);

CREATE TABLE tour (
  tour_id         TEXT PRIMARY KEY,
  title           TEXT NOT NULL,
  created_by_json TEXT NOT NULL,
  created_at      TEXT NOT NULL
) STRICT;

CREATE TABLE tour_step (
  tour_id    TEXT NOT NULL REFERENCES tour(tour_id) ON DELETE CASCADE,
  ord        INTEGER NOT NULL,
  target     TEXT NOT NULL,  -- WindowKey, "path:line" or HubId
  start_line INTEGER,
  end_line   INTEGER,
  note       TEXT NOT NULL,
  PRIMARY KEY (tour_id, ord)
) STRICT, WITHOUT ROWID;

-- MCP sessions bound to a workspace (bind_workspace): attribution, presence, default revsets.
CREATE TABLE agent_session (
  session_id TEXT PRIMARY KEY,
  workspace  TEXT NOT NULL,
  agent_json TEXT NOT NULL,  -- Actor
  bound_at   TEXT NOT NULL,
  last_seen  TEXT NOT NULL,
  activity   TEXT NOT NULL DEFAULT 'idle' CHECK (activity IN ('reading', 'editing', 'idle'))
) STRICT;
CREATE INDEX agent_session_workspace ON agent_session(workspace, last_seen);

-- Per-session agent toggles (DESIGN §10), keyed by the UI session (the id the UI sends as
-- X-CV-Session, "default" without one) and the agent name ("*" = every agent). NULL = not set.
-- Added after schema_version 1; cv-store creates it on open when missing.
CREATE TABLE IF NOT EXISTS agent_permission (
  ui_session  TEXT NOT NULL,
  agent       TEXT NOT NULL,
  edit        INTEGER,
  move_camera INTEGER,
  updated_at  TEXT NOT NULL,
  PRIMARY KEY (ui_session, agent)
) STRICT, WITHOUT ROWID;

-- Append-only; seq is the EventCursor for WS replay, GET /events and MCP wait_for_events.
CREATE TABLE event_log (
  seq          INTEGER PRIMARY KEY AUTOINCREMENT,
  at           TEXT NOT NULL,
  type         TEXT NOT NULL,
  payload_json TEXT NOT NULL  -- ServerEvent
) STRICT;
CREATE INDEX event_log_type ON event_log(type, seq);

-- Version 2 (2026-10-19): layout_pin.missed, cluster_pin, cluster_label lane/missed/ord,
-- link_feedback link_id/link_json/ord. Version-1 files are upgraded on open, and the JSON records
-- earlier builds kept in meta (cv.links.v1, cv.ranges.v1, cv.layout.v1) move into these tables.
INSERT INTO meta (key, value) VALUES ('schema_version', '2');
