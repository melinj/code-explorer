-- index.db: derived data. Safe to delete: everything here is rebuilt from the jj repo.
-- Location: $XDG_CACHE_HOME/code-viewer/<repo-id>/index.db, where repo-id hashes the real path of
-- <repo>/.jj/repo, so every workspace of one repo shares one index.
-- One writer (the indexer, B2 API), many readers. Table prefixes name the owning workstream's data:
-- vcs_ (B1), blob_/fact_ (B3), link_ (B4), churn_ (B5), canvas_ (B6), search_ (B2).
--
-- Filled today (cv-store `views`, cv-server `view_store`): every fresh snapshot build is saved as
-- canvas_view + canvas_file + canvas_cluster + canvas_window (+ R*Tree) + link_edge, and its
-- symbols and files go to search_fts. `cv serve` loads a saved view instead of indexing again
-- when spec, op, head commit and build inputs match; parse_cache keeps facts per blob. The vcs_,
-- blob, fact_, link_hub, link_metric and churn_ tables are the target shape for B1/B3/B4/B5 and
-- stay empty for now (facts live in parse_cache, hubs and churn in canvas_view JSON).
-- A file with another schema_version is rebuilt on open (derived data; parse_cache is kept).
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE meta (
  key   TEXT PRIMARY KEY,  -- schema_version, repo_root, repo_dir, pack_version, last_synced_op
  value TEXT NOT NULL
) STRICT;

-- ── B1: operations, revisions, refs, workspaces ──────────────────────────────

CREATE TABLE vcs_op (
  op_id       TEXT PRIMARY KEY,
  time        TEXT NOT NULL,
  description TEXT NOT NULL,
  synced_at   TEXT
) STRICT;

CREATE TABLE vcs_revision (
  commit_id      TEXT PRIMARY KEY,
  change_id      TEXT NOT NULL,
  description    TEXT NOT NULL,
  author_name    TEXT NOT NULL,
  author_email   TEXT NOT NULL,
  author_time    TEXT NOT NULL,
  committer_time TEXT NOT NULL,
  -- bit 0 conflict · 1 empty · 2 immutable · 3 divergent · 4 hidden · 5 working copy
  flags          INTEGER NOT NULL DEFAULT 0,
  seen_op        TEXT NOT NULL REFERENCES vcs_op(op_id)
) STRICT;
CREATE INDEX vcs_revision_change ON vcs_revision(change_id);

CREATE TABLE vcs_parent (
  commit_id TEXT NOT NULL REFERENCES vcs_revision(commit_id) ON DELETE CASCADE,
  ord       INTEGER NOT NULL,
  parent_id TEXT NOT NULL,
  PRIMARY KEY (commit_id, ord)
) STRICT, WITHOUT ROWID;
CREATE INDEX vcs_parent_parent ON vcs_parent(parent_id);

CREATE TABLE vcs_ref (
  kind      TEXT NOT NULL CHECK (kind IN ('bookmark', 'tag')),
  name      TEXT NOT NULL,
  remote    TEXT NOT NULL DEFAULT '',  -- '' = local
  commit_id TEXT NOT NULL,
  PRIMARY KEY (kind, name, remote)
) STRICT, WITHOUT ROWID;

CREATE TABLE vcs_workspace (
  name        TEXT PRIMARY KEY,
  root        TEXT NOT NULL,
  wc_change   TEXT NOT NULL,
  wc_commit   TEXT NOT NULL,
  dirty_files INTEGER NOT NULL DEFAULT 0,  -- files on disk not yet snapshotted
  last_op     TEXT
) STRICT;

-- Files per snapshot. commit_id may be 'wc:<workspace>' for the live overlay of unsnapshotted edits.
CREATE TABLE vcs_tree_entry (
  commit_id  TEXT NOT NULL,
  path       TEXT NOT NULL,
  blob_id    TEXT NOT NULL,
  executable INTEGER NOT NULL DEFAULT 0,
  conflict   INTEGER NOT NULL DEFAULT 0,
  lane       TEXT NOT NULL DEFAULT 'code',  -- from the lane rules (path + content sniff)
  PRIMARY KEY (commit_id, path)
) STRICT, WITHOUT ROWID;
CREATE INDEX vcs_tree_entry_blob ON vcs_tree_entry(blob_id);

-- ── B3: content-addressed facts (one parse per blob × pack version) ──────────

CREATE TABLE blob (
  blob_id       TEXT PRIMARY KEY,
  size          INTEGER NOT NULL,
  lines         INTEGER,
  lang          TEXT NOT NULL,
  flags         INTEGER NOT NULL DEFAULT 0,  -- bit 0 binary · 1 generated · 2 vendored · 3 too large
  pack_version  TEXT,                         -- NULL until parsed
  syntax_errors INTEGER
) STRICT;

CREATE TABLE fact_symbol (
  blob_id    TEXT NOT NULL REFERENCES blob(blob_id) ON DELETE CASCADE,
  sid        INTEGER NOT NULL,
  parent_sid INTEGER,
  kind       TEXT NOT NULL,
  name       TEXT NOT NULL,
  qualname   TEXT NOT NULL,
  start_line INTEGER NOT NULL,
  end_line   INTEGER NOT NULL,
  sig_line   INTEGER NOT NULL,
  signature  TEXT NOT NULL,
  PRIMARY KEY (blob_id, sid)
) STRICT, WITHOUT ROWID;
CREATE INDEX fact_symbol_name ON fact_symbol(name);

CREATE TABLE fact_ref (
  blob_id       TEXT NOT NULL REFERENCES blob(blob_id) ON DELETE CASCADE,
  kind          TEXT NOT NULL CHECK (kind IN ('call', 'inherit', 'implement', 'import', 'type', 'decorator')),
  name          TEXT NOT NULL,
  qualifier     TEXT,
  line          INTEGER NOT NULL,
  col_start     INTEGER NOT NULL,
  col_end       INTEGER NOT NULL,
  enclosing_sid INTEGER
) STRICT;
CREATE INDEX fact_ref_blob ON fact_ref(blob_id);
CREATE INDEX fact_ref_name ON fact_ref(name);

CREATE TABLE fact_import (
  blob_id  TEXT NOT NULL REFERENCES blob(blob_id) ON DELETE CASCADE,
  local    TEXT NOT NULL,
  module   TEXT NOT NULL,
  imported TEXT,
  line     INTEGER NOT NULL
) STRICT;
CREATE INDEX fact_import_blob ON fact_import(blob_id);

CREATE TABLE fact_endpoint (
  blob_id       TEXT NOT NULL REFERENCES blob(blob_id) ON DELETE CASCADE,
  rule          TEXT NOT NULL,
  protocol      TEXT NOT NULL,
  role          TEXT NOT NULL CHECK (role IN ('producer', 'consumer', 'client', 'server', 'sender', 'receiver')),
  key_json      TEXT NOT NULL,  -- KeyExpr
  line          INTEGER NOT NULL,
  col_start     INTEGER NOT NULL,
  col_end       INTEGER NOT NULL,
  enclosing_sid INTEGER,
  handler       TEXT,
  confidence    REAL NOT NULL
) STRICT;
CREATE INDEX fact_endpoint_blob ON fact_endpoint(blob_id);

-- Config languages (YAML, TOML, JSON, .env): every key path with its line, for config-field links (P5).
CREATE TABLE fact_config_key (
  blob_id    TEXT NOT NULL REFERENCES blob(blob_id) ON DELETE CASCADE,
  key_path   TEXT NOT NULL,  -- "inputs.capacity"
  leaf       TEXT NOT NULL,  -- "capacity"
  line       INTEGER NOT NULL,
  value_kind TEXT NOT NULL CHECK (value_kind IN ('scalar', 'mapping', 'sequence', 'block')),
  PRIMARY KEY (blob_id, key_path, line)
) STRICT, WITHOUT ROWID;
CREATE INDEX fact_config_key_leaf ON fact_config_key(leaf);

CREATE TABLE fact_silhouette (
  blob_id TEXT PRIMARY KEY REFERENCES blob(blob_id) ON DELETE CASCADE,
  data    BLOB NOT NULL  -- OutlineEncoding: 3 bytes per file line
) STRICT;

-- ── B4: links and hubs, resolved per snapshot commit ─────────────────────────

CREATE TABLE link_hub (
  commit_id TEXT NOT NULL,
  hub_id    TEXT NOT NULL,  -- "<protocol>:<normalized key>"
  protocol  TEXT NOT NULL,
  key       TEXT NOT NULL,
  family    TEXT,  -- config hubs: file family, e.g. "node.yaml" (one hub per family and key)
  display   TEXT NOT NULL,
  PRIMARY KEY (commit_id, hub_id)
) STRICT, WITHOUT ROWID;

-- Links of one saved view, in the view's order (link ids are per view). from_window/to_window
-- are window ids of the same view; a runtime-only target (D11) has to_dynamic set and to_window
-- = from_window. `port` is the channel port label ("⇢ …"), NULL for tree-sitter code links.
CREATE TABLE link_edge (
  view_id        TEXT NOT NULL REFERENCES canvas_view(view_id) ON DELETE CASCADE,
  ord            INTEGER NOT NULL,
  commit_id      TEXT NOT NULL,
  link_id        TEXT NOT NULL,
  kind           TEXT NOT NULL,
  from_window    TEXT NOT NULL,
  from_path      TEXT NOT NULL,
  from_line      INTEGER NOT NULL,
  from_col_start INTEGER,
  from_col_end   INTEGER,
  from_symbol    TEXT,
  to_window      TEXT NOT NULL,
  to_path        TEXT,
  to_line        INTEGER NOT NULL,
  to_symbol      TEXT,
  to_hub         TEXT,
  to_dynamic     TEXT,  -- hint for a link whose target is only known at runtime (drawn as an open stub)
  label          TEXT NOT NULL,
  confidence     REAL NOT NULL CHECK (confidence BETWEEN 0 AND 1),
  provenance     TEXT NOT NULL,
  port           TEXT,
  PRIMARY KEY (view_id, ord),
  CHECK ((to_path IS NOT NULL) + (to_hub IS NOT NULL) + (to_dynamic IS NOT NULL) = 1)
) STRICT, WITHOUT ROWID;
CREATE INDEX link_edge_id ON link_edge(view_id, link_id);
CREATE INDEX link_edge_from ON link_edge(view_id, from_window);
CREATE INDEX link_edge_to ON link_edge(view_id, to_window);

-- Connectedness per window key: dependents within 2 hops (code lane, tests excluded) and details.
CREATE TABLE link_metric (
  commit_id   TEXT NOT NULL,
  window_key  TEXT NOT NULL,
  dependents2 INTEGER NOT NULL,
  fan_in      INTEGER NOT NULL,
  fan_out     INTEGER NOT NULL,
  tests       INTEGER NOT NULL,  -- direct test dependents
  reach       INTEGER NOT NULL,  -- transitive dependents (detail only; saturates in cycles)
  cycle_size  INTEGER,           -- import cycle size when > 1
  grade       INTEGER NOT NULL CHECK (grade BETWEEN 0 AND 4),  -- rank quantiles of dependents2
  PRIMARY KEY (commit_id, window_key)
) STRICT, WITHOUT ROWID;

-- ── B5: history mining and churn grades ──────────────────────────────────────

CREATE TABLE churn_commit_file (
  commit_id   TEXT NOT NULL,
  path        TEXT NOT NULL,
  status      TEXT NOT NULL,
  source_path TEXT,
  added       INTEGER NOT NULL,
  removed     INTEGER NOT NULL,
  hunks       BLOB NOT NULL,  -- packed little-endian int32 quads [oldStart, oldLen, newStart, newLen]
  PRIMARY KEY (commit_id, path)
) STRICT, WITHOUT ROWID;

CREATE TABLE churn_run (
  run_id       INTEGER PRIMARY KEY,
  head         TEXT NOT NULL,
  options_json TEXT NOT NULL,  -- ChurnOptions
  scanned      INTEGER NOT NULL,
  oldest       TEXT,
  settled_mass REAL NOT NULL,
  stop         TEXT NOT NULL CHECK (stop IN ('settled', 'max-commits', 'max-age', 'history-exhausted')),
  bands_json   TEXT NOT NULL,
  finished_at  TEXT NOT NULL
) STRICT;

CREATE TABLE churn_grade (
  run_id     INTEGER NOT NULL REFERENCES churn_run(run_id) ON DELETE CASCADE,
  key        TEXT NOT NULL,  -- path, or "<path>#<qualname>" for symbol churn
  touches    INTEGER NOT NULL,
  weighted   REAL NOT NULL,
  rate       REAL NOT NULL,
  ci_low     REAL NOT NULL,
  ci_high    REAL NOT NULL,
  grade      INTEGER NOT NULL CHECK (grade BETWEEN 0 AND 4),
  settled    INTEGER NOT NULL,
  last_touch TEXT,
  PRIMARY KEY (run_id, key)
) STRICT, WITHOUT ROWID;

CREATE TABLE churn_cochange (
  run_id  INTEGER NOT NULL REFERENCES churn_run(run_id) ON DELETE CASCADE,
  a       TEXT NOT NULL,
  b       TEXT NOT NULL,
  support INTEGER NOT NULL,
  jaccard REAL NOT NULL,
  PRIMARY KEY (run_id, a, b),
  CHECK (a < b)
) STRICT, WITHOUT ROWID;

-- ── B6: views, clusters, windows, ports ──────────────────────────────────────

CREATE TABLE canvas_view (
  view_id        TEXT PRIMARY KEY,
  spec_json      TEXT NOT NULL,  -- ViewSpec
  op_id          TEXT NOT NULL,
  head           TEXT NOT NULL,
  base           TEXT,
  status         TEXT NOT NULL CHECK (status IN ('ready', 'indexing', 'stale')),
  layout_version INTEGER NOT NULL,
  world_json     TEXT NOT NULL,  -- Rect
  grid_json      TEXT NOT NULL DEFAULT '{}',  -- {laneDefs: LaneDef[], lanes: LaneBand[], columns: GridColumn[]}
  inputs         TEXT NOT NULL DEFAULT '',    -- hash of what the build read beyond spec, op and commit (code, packs, pins, options)
  rest_json      TEXT NOT NULL DEFAULT '{}',  -- the rest of the ViewBootstrap: hubs, bundles, churn, heatLabels, warnings
  renames_json   TEXT NOT NULL DEFAULT '{}',  -- old path -> path at head, from the churn walk
  placement_json TEXT,                        -- the cv-layout placement the next layout starts from
  created_at     TEXT NOT NULL
) STRICT;
CREATE INDEX canvas_view_at ON canvas_view(head, op_id);

-- The files a saved view shows, in view order (text is read again by blob id; facts come from
-- parse_cache).
CREATE TABLE canvas_file (
  view_id TEXT NOT NULL REFERENCES canvas_view(view_id) ON DELETE CASCADE,
  ord     INTEGER NOT NULL,
  path    TEXT NOT NULL,
  blob_id TEXT NOT NULL,
  lang    TEXT NOT NULL,
  lane    TEXT NOT NULL,
  lines   INTEGER NOT NULL,
  PRIMARY KEY (view_id, ord)
) STRICT, WITHOUT ROWID;

CREATE TABLE canvas_cluster (
  view_id      TEXT NOT NULL REFERENCES canvas_view(view_id) ON DELETE CASCADE,
  cluster_id   TEXT NOT NULL,
  parent_id    TEXT,
  label        TEXT NOT NULL,
  label_source TEXT NOT NULL CHECK (label_source IN ('auto', 'agent', 'human')),
  members_json TEXT NOT NULL,  -- file paths; labels carry over to the new cluster with max Jaccard ≥ 0.6
  lane         TEXT NOT NULL,
  heat         INTEGER NOT NULL CHECK (heat BETWEEN 0 AND 4),  -- district column
  heat_level   REAL,                                            -- continuous heat 0..1 (Cluster.heatLevel)
  review       REAL,                                            -- review-first share 0..1 (Cluster.review)
  conn         INTEGER NOT NULL CHECK (conn BETWEEN 0 AND 4),  -- district row
  x REAL NOT NULL, y REAL NOT NULL, w REAL NOT NULL, h REAL NOT NULL,
  ord          INTEGER NOT NULL,
  summary      TEXT,
  windows      INTEGER NOT NULL DEFAULT 0,
  open_threads INTEGER NOT NULL DEFAULT 0,
  conflicts    INTEGER NOT NULL DEFAULT 0,
  pinned       INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (view_id, cluster_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE canvas_window (
  wid             INTEGER PRIMARY KEY,  -- rowid, shared with canvas_window_rtree
  view_id         TEXT NOT NULL REFERENCES canvas_view(view_id) ON DELETE CASCADE,
  ord             INTEGER NOT NULL,     -- position in the view's window list
  window_id       TEXT NOT NULL,
  window_key      TEXT NOT NULL,
  kind            TEXT NOT NULL CHECK (kind IN ('symbol', 'chunk', 'hunk', 'conflict', 'context')),
  cluster_id      TEXT NOT NULL,
  lane            TEXT NOT NULL,
  path            TEXT NOT NULL,
  commit_id       TEXT NOT NULL,
  change_id       TEXT NOT NULL,
  blob_id         TEXT NOT NULL,
  symbol_qualname TEXT,
  symbol_name     TEXT,
  symbol_kind     TEXT,
  lang            TEXT NOT NULL DEFAULT 'other',
  start_line      INTEGER NOT NULL,
  end_line        INTEGER NOT NULL,
  file_lines      INTEGER NOT NULL,
  heat_json       TEXT NOT NULL,  -- Heat
  conn_json       TEXT NOT NULL,  -- Connectedness
  risk            REAL NOT NULL DEFAULT 0,  -- geometric mean of heat and connectedness percentiles
  review_first    INTEGER NOT NULL DEFAULT 0,
  fragment_index  INTEGER,  -- set when the file is split into several windows
  fragment_of     INTEGER,
  chip            INTEGER CHECK (chip BETWEEN 0 AND 9),  -- file-chip palette slot, stable per path
  minimized       INTEGER NOT NULL DEFAULT 0,  -- range-view context window shown as a header-only pill
  x REAL NOT NULL, y REAL NOT NULL, w REAL NOT NULL, h REAL NOT NULL,
  pinned          INTEGER NOT NULL DEFAULT 0,
  truncated       INTEGER NOT NULL DEFAULT 0,
  ports           INTEGER NOT NULL DEFAULT 0,
  open_threads    INTEGER NOT NULL DEFAULT 0,
  conflicts       INTEGER NOT NULL DEFAULT 0,
  diff_stat_json  TEXT,
  UNIQUE (view_id, window_id)
) STRICT;
CREATE INDEX canvas_window_key ON canvas_window(view_id, window_key);
CREATE INDEX canvas_window_path ON canvas_window(view_id, path);  -- all fragments of a file (file chip)

-- Viewport queries: SELECT … FROM canvas_window_rtree r JOIN canvas_window w ON w.wid = r.wid
--   WHERE r.max_x >= :x0 AND r.min_x <= :x1 AND r.max_y >= :y0 AND r.min_y <= :y1 AND w.view_id = :v
-- R*Tree coordinates are 32-bit (rounded outward), so callers re-check the exact x/y/w/h.
CREATE VIRTUAL TABLE canvas_window_rtree USING rtree(wid, min_x, max_x, min_y, max_y);

CREATE TABLE canvas_port (
  view_id       TEXT NOT NULL,
  window_id     TEXT NOT NULL,
  port_id       TEXT NOT NULL,
  side          TEXT NOT NULL CHECK (side IN ('in', 'out')),
  line          INTEGER NOT NULL,
  kind          TEXT NOT NULL,
  label         TEXT NOT NULL,
  link_ids_json TEXT NOT NULL,
  PRIMARY KEY (view_id, window_id, port_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE canvas_window_diff (
  view_id    TEXT NOT NULL,
  window_id  TEXT NOT NULL,
  lines_json TEXT NOT NULL,  -- DiffLine[]
  PRIMARY KEY (view_id, window_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE canvas_hub_place (
  view_id TEXT NOT NULL REFERENCES canvas_view(view_id) ON DELETE CASCADE,
  hub_id  TEXT NOT NULL,
  x REAL NOT NULL, y REAL NOT NULL, w REAL NOT NULL, h REAL NOT NULL,
  PRIMARY KEY (view_id, hub_id)
) STRICT, WITHOUT ROWID;

-- ── B3: parse cache ─────────────────────────────────────────────────────────

-- A file's parsed facts (cv-lang codec form) for one blob at one path, valid for one pack
-- version: re-indexing reuses them instead of parsing again. The path is part of the key because
-- a pack's analysis may depend on it (qualnames, dialects). Also created on open for databases
-- made before this table existed.
CREATE TABLE IF NOT EXISTS parse_cache (
  path    TEXT NOT NULL,
  blob_id TEXT NOT NULL,
  pack    TEXT NOT NULL,   -- "<pack id>@<pack version>#<codec version>"
  facts   BLOB NOT NULL,
  PRIMARY KEY (path, blob_id)
) STRICT;

-- ── B2: search ───────────────────────────────────────────────────────────────

-- Symbols and files of one view at one head commit (scope "<view id>|<head>"). The trigram
-- tokenizer matches any substring of 3+ characters, case-insensitively, as the in-memory search
-- did; cv-server ranks the candidates (exact symbol name first).
CREATE VIRTUAL TABLE search_fts USING fts5(
  scope UNINDEXED,
  kind UNINDEXED,  -- symbol | file
  label,           -- symbol qualname | file path
  detail,          -- symbol name | ''
  ref UNINDEXED,   -- symbol: "<window id> <line> <kind>"; file: "<file ordinal>"
  tokenize = 'trigram'
);
CREATE TABLE search_scope (
  scope      TEXT PRIMARY KEY,
  view_id    TEXT NOT NULL,
  rows       INTEGER NOT NULL,
  written_at TEXT NOT NULL
) STRICT;

INSERT INTO meta (key, value) VALUES ('schema_version', '2');
