//! Saved snapshot views in index.db (D5, B2): what a view build produced, so `cv serve` can load
//! it instead of indexing again, plus the R*Tree viewport query and the FTS search index.
//!
//! Tables (docs/schema/index.sql): `canvas_view` (bootstrap pieces as JSON), `canvas_file`,
//! `canvas_cluster`, `canvas_window` + `canvas_window_rtree`, `link_edge`, `search_fts` +
//! `search_scope`. Everything is per view id; a view is written in one transaction and replaces
//! any earlier copy.

use cv_contracts::{
    Cluster, Connectedness, DiffStat, FileFragment, HEADER_WU, Heat, LINE_WU, LineSpan, Rect,
    SymbolRef, TileEdge, ViewBootstrap, ViewRef, ViewStats, WindowCounts, WindowFlags,
    WindowHeader,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::{IndexDb, Result, now_iso};

/// A file of a saved view (text is read again by blob id; facts come from the parse cache).
#[derive(Debug, Clone, PartialEq)]
pub struct StoredFile {
    pub path: String,
    pub blob: String,
    pub lang: String,
    pub lane: String,
    pub lines: u32,
}

/// A link of a saved view; `from`/`to` index [`StoredView::windows`].
#[derive(Debug, Clone, PartialEq)]
pub struct StoredLink {
    pub id: String,
    pub kind: String,
    pub from: usize,
    pub from_line: u32,
    pub to: usize,
    pub to_line: u32,
    pub confidence: f64,
    pub label: String,
    /// Channel port label; `None` for a tree-sitter code link.
    pub port: Option<String>,
    /// Runtime-only target (D11), as JSON.
    pub dynamic: Option<String>,
    pub provenance: String,
}

/// Everything a snapshot build produced that is not re-derived on load.
#[derive(Debug, Clone)]
pub struct StoredView {
    /// Hash of the build inputs beyond spec, op and head (code, packs, pins, options).
    pub inputs: String,
    pub bootstrap: ViewBootstrap,
    pub files: Vec<StoredFile>,
    pub windows: Vec<WindowHeader>,
    pub links: Vec<StoredLink>,
    /// Old path → path at head (JSON object).
    pub renames_json: String,
    /// cv-layout placement (JSON), when the caller keeps one.
    pub placement_json: Option<String>,
}

/// One search candidate row.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchRow {
    /// "symbol" or "file".
    pub kind: String,
    pub label: String,
    pub detail: String,
    pub reference: String,
}

const INDEX_SCHEMA_VERSION: &str = "3";
/// Search scopes kept (each is one view at one revision).
const MAX_SEARCH_SCOPES: i64 = 12;

/// Rebuilds an index.db whose schema_version differs (derived data), keeping the parse cache.
pub(crate) fn upgrade(conn: &Connection) -> Result<()> {
    let version: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if version.as_deref() == Some(INDEX_SCHEMA_VERSION) {
        return Ok(());
    }
    let objects: Vec<(String, String)> = {
        let mut st = conn.prepare(
            "SELECT type, name FROM sqlite_master WHERE type IN ('table', 'view')
             AND name NOT LIKE 'sqlite_%' AND name <> 'parse_cache'",
        )?;
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN;")?;
    let res = (|| -> rusqlite::Result<()> {
        for (kind, name) in &objects {
            // Shadow tables of virtual tables go with their parent.
            let exists: i64 = conn.query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = ?1",
                [name],
                |r| r.get(0),
            )?;
            if exists > 0 {
                conn.execute_batch(&format!(
                    "DROP {} IF EXISTS \"{}\"",
                    kind.to_uppercase(),
                    name
                ))?;
            }
        }
        conn.execute_batch(crate::INDEX_SCHEMA)?;
        Ok(())
    })();
    match res {
        Ok(()) => conn.execute_batch("COMMIT; PRAGMA foreign_keys = ON;")?,
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK; PRAGMA foreign_keys = ON;");
            return Err(e.into());
        }
    }
    Ok(())
}

fn b(v: bool) -> i64 {
    v as i64
}

impl IndexDb {
    /// Saves `v` (replacing any earlier copy of the same view id) and keeps at most `keep` saved
    /// views, dropping the oldest.
    pub fn put_view(&self, v: &StoredView, keep: usize) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let b0 = &v.bootstrap;
        let vr = &b0.view;
        delete_view(&tx, &vr.id)?;
        tx.execute(
            "INSERT INTO canvas_view (view_id, spec_json, op_id, head, base, status, layout_version,
                world_json, grid_json, inputs, rest_json, renames_json, placement_json, created_at)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                vr.id,
                vr.spec.to_string(),
                vr.resolved.op,
                vr.resolved.head,
                vr.status,
                vr.layout_version,
                serde_json::to_string(&b0.world)?,
                json!({ "laneDefs": b0.lane_defs, "lanes": b0.lanes, "columns": b0.columns })
                    .to_string(),
                v.inputs,
                json!({
                    "hubs": b0.hubs,
                    "bundles": b0.bundles,
                    "churn": b0.churn,
                    "heatLabels": b0.heat_labels,
                    "warnings": vr.warnings,
                    "commits": vr.resolved.commits,
                    "sharedPlacement": vr.shared_placement,
                })
                .to_string(),
                v.renames_json,
                v.placement_json,
                now_iso(),
            ],
        )?;
        {
            let mut st = tx.prepare_cached(
                "INSERT INTO canvas_file (view_id, ord, path, blob_id, lang, lane, lines)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for (i, f) in v.files.iter().enumerate() {
                st.execute(params![
                    vr.id, i as i64, f.path, f.blob, f.lang, f.lane, f.lines
                ])?;
            }
            let mut st = tx.prepare_cached(
                "INSERT INTO canvas_cluster (view_id, cluster_id, parent_id, label, label_source,
                    members_json, lane, heat, conn, x, y, w, h, ord, summary, windows, open_threads,
                    conflicts, pinned, heat_level, review)
                 VALUES (?1, ?2, ?3, ?4, ?5, '[]', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            )?;
            for (i, c) in b0.clusters.iter().enumerate() {
                st.execute(params![
                    vr.id,
                    c.id,
                    c.parent,
                    c.label,
                    c.label_source,
                    c.lane,
                    c.heat,
                    c.conn,
                    c.rect.x,
                    c.rect.y,
                    c.rect.w,
                    c.rect.h,
                    i as i64,
                    c.summary,
                    c.windows,
                    c.open_threads,
                    c.conflicts,
                    b(c.pinned),
                    c.heat_level,
                    c.review
                ])?;
            }
            let mut st = tx.prepare_cached(
                "INSERT INTO canvas_window (view_id, ord, window_id, window_key, kind, cluster_id, lane,
                    path, commit_id, change_id, blob_id, symbol_qualname, symbol_name, symbol_kind, lang,
                    start_line, end_line, file_lines, heat_json, conn_json, risk, review_first,
                    fragment_index, fragment_of, chip, minimized, x, y, w, h, pinned, truncated, ports,
                    open_threads, conflicts, diff_stat_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                    ?19, ?20, ?21, ?22, ?23, ?24, ?25, 0, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35)",
            )?;
            let mut rt = tx.prepare_cached(
                "INSERT INTO canvas_window_rtree (wid, min_x, max_x, min_y, max_y) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for (i, w) in v.windows.iter().enumerate() {
                let sym = w.symbol.as_ref();
                let frag = w.fragment.as_ref();
                st.execute(params![
                    vr.id,
                    i as i64,
                    w.id,
                    w.key,
                    w.kind,
                    w.cluster,
                    w.lane,
                    w.path,
                    w.rev,
                    w.change,
                    w.blob,
                    sym.map(|s| &s.qualname),
                    sym.map(|s| &s.name),
                    sym.map(|s| &s.kind),
                    w.lang,
                    w.lines.start,
                    w.lines.end,
                    w.lines.file_lines,
                    serde_json::to_string(&w.heat)?,
                    serde_json::to_string(&w.conn)?,
                    w.risk,
                    b(w.review_first),
                    frag.map(|f| f.index),
                    frag.map(|f| f.of),
                    frag.map(|f| f.chip),
                    w.rect.x,
                    w.rect.y,
                    w.rect.w,
                    w.rect.h,
                    b(w.flags.pinned),
                    b(w.flags.truncated),
                    w.counts.ports,
                    w.counts.open_threads,
                    w.counts.conflicts,
                    w.diff_stat
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()?,
                ])?;
                let wid = tx.last_insert_rowid();
                let r = &w.rect;
                rt.execute(params![wid, r.x, r.x + r.w, r.y, r.y + r.h])?;
            }
            let mut st = tx.prepare_cached(
                "INSERT INTO link_edge (view_id, ord, commit_id, link_id, kind, from_window, from_path,
                    from_line, to_window, to_path, to_line, to_dynamic, label, confidence, provenance, port)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            )?;
            for (i, l) in v.links.iter().enumerate() {
                let (f, t) = (&v.windows[l.from], &v.windows[l.to]);
                st.execute(params![
                    vr.id,
                    i as i64,
                    vr.resolved.head,
                    l.id,
                    l.kind,
                    f.id,
                    f.path,
                    l.from_line,
                    t.id,
                    l.dynamic.is_none().then_some(&t.path),
                    l.to_line,
                    l.dynamic,
                    l.label,
                    l.confidence.clamp(0.0, 1.0),
                    l.provenance,
                    l.port
                ])?;
            }
        }
        // Keep the newest `keep` views.
        let old: Vec<String> = {
            let mut st = tx.prepare(
                "SELECT view_id FROM canvas_view ORDER BY created_at DESC, rowid DESC LIMIT -1 OFFSET ?1",
            )?;
            st.query_map([keep as i64], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        for id in old {
            delete_view(&tx, &id)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The saved view built from exactly these inputs, if any.
    pub fn find_view(
        &self,
        spec_json: &str,
        op: &str,
        head: &str,
        inputs: &str,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT view_id FROM canvas_view WHERE head = ?1 AND op_id = ?2 AND spec_json = ?3
                 AND inputs = ?4 ORDER BY created_at DESC LIMIT 1",
                params![head, op, spec_json, inputs],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Ids of every saved view.
    pub fn view_ids(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut st = conn.prepare("SELECT view_id FROM canvas_view ORDER BY created_at")?;
        Ok(st
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// When the view was saved (ISO time).
    pub fn view_created_at(&self, view_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT created_at FROM canvas_view WHERE view_id = ?1",
                [view_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn delete_view(&self, view_id: &str) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        delete_view(&tx, view_id)?;
        tx.commit()?;
        Ok(())
    }

    /// Loads a saved view.
    pub fn load_view(&self, view_id: &str) -> Result<Option<StoredView>> {
        let conn = self.conn.lock().unwrap();
        let Some((inputs, renames, placement)) = conn
            .query_row(
                "SELECT inputs, renames_json, placement_json FROM canvas_view WHERE view_id = ?1",
                [view_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?
        else {
            return Ok(None);
        };
        let files: Vec<StoredFile> = {
            let mut st = conn.prepare_cached(
                "SELECT path, blob_id, lang, lane, lines FROM canvas_file WHERE view_id = ?1 ORDER BY ord",
            )?;
            st.query_map([view_id], |r| {
                Ok(StoredFile {
                    path: r.get(0)?,
                    blob: r.get(1)?,
                    lang: r.get(2)?,
                    lane: r.get(3)?,
                    lines: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        let windows = load_windows(&conn, view_id)?;
        let index: std::collections::HashMap<&str, usize> = windows
            .iter()
            .enumerate()
            .map(|(i, w)| (w.id.as_str(), i))
            .collect();
        let links: Vec<StoredLink> = {
            let mut st = conn.prepare_cached(
                "SELECT link_id, kind, from_window, from_line, to_window, to_line, confidence, label,
                    port, to_dynamic, provenance FROM link_edge WHERE view_id = ?1 ORDER BY ord",
            )?;
            let rows = st
                .query_map([view_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, u32>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, u32>(5)?,
                        r.get::<_, f64>(6)?,
                        r.get::<_, String>(7)?,
                        r.get::<_, Option<String>>(8)?,
                        r.get::<_, Option<String>>(9)?,
                        r.get::<_, String>(10)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut out = Vec::with_capacity(rows.len());
            for (id, kind, fw, from_line, tw, to_line, confidence, label, port, dynamic, prov) in
                rows
            {
                let (Some(&from), Some(&to)) = (index.get(fw.as_str()), index.get(tw.as_str()))
                else {
                    return Err(crate::Error(format!(
                        "saved view {view_id}: link {id} names an unknown window"
                    )));
                };
                out.push(StoredLink {
                    id,
                    kind,
                    from,
                    from_line,
                    to,
                    to_line,
                    confidence,
                    label,
                    port,
                    dynamic,
                    provenance: prov,
                });
            }
            out
        };
        let Some(bootstrap) =
            bootstrap_of(&conn, view_id, windows.len() as u32, files.len() as u32)?
        else {
            return Ok(None);
        };
        Ok(Some(StoredView {
            inputs,
            bootstrap,
            files,
            windows,
            links,
            renames_json: renames,
            placement_json: placement,
        }))
    }

    /// Only the bootstrap of saved view `id` (GET /views/:id/bootstrap for a view that is not
    /// loaded, e.g. an id from before a restart), without its windows, files and links.
    pub fn load_bootstrap(&self, view_id: &str) -> Result<Option<ViewBootstrap>> {
        let conn = self.conn.lock().unwrap();
        let count = |table: &str| -> Result<u32> {
            Ok(conn.query_row(
                &format!("SELECT count(*) FROM {table} WHERE view_id = ?1"),
                [view_id],
                |r| r.get(0),
            )?)
        };
        let (windows, files) = (count("canvas_window")?, count("canvas_file")?);
        bootstrap_of(&conn, view_id, windows, files)
    }

    /// Ordinals (positions in the view's window list) of the windows of a saved view that
    /// intersect `rect` (open intervals, as `Rect::intersects`), from the R*Tree, ascending.
    pub fn windows_in_rect(&self, view_id: &str, rect: &Rect) -> Result<Vec<usize>> {
        let conn = self.conn.lock().unwrap();
        windows_in_rect(&conn, view_id, rect)
    }

    /// Headers of the windows of a saved view that intersect `rect`, in window order (the disk
    /// path for tiles of a view that is not loaded).
    pub fn window_headers_in_rect(
        &self,
        view_id: &str,
        rect: &Rect,
    ) -> Result<Vec<(usize, WindowHeader)>> {
        let conn = self.conn.lock().unwrap();
        let ords = windows_in_rect(&conn, view_id, rect)?;
        if ords.is_empty() {
            return Ok(vec![]);
        }
        let (x0, x1, y0, y1) = (rect.x, rect.x + rect.w, rect.y, rect.y + rect.h);
        let all = select_windows(
            &conn,
            "WHERE w.wid IN (SELECT t.wid FROM canvas_window_rtree t
                WHERE t.max_x >= ?2 AND t.min_x <= ?3 AND t.max_y >= ?4 AND t.min_y <= ?5)
               AND +w.view_id = ?1
               AND w.x < ?3 AND w.x + w.w > ?2 AND w.y < ?5 AND w.y + w.h > ?4",
            params![view_id, x0, x1, y0, y1],
            false,
        )?;
        debug_assert_eq!(all.len(), ords.len());
        Ok(ords.into_iter().zip(all).collect())
    }

    /// The layout version of a saved view.
    pub fn view_layout_version(&self, view_id: &str) -> Result<Option<u32>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT layout_version FROM canvas_view WHERE view_id = ?1",
                [view_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Edges of a saved view with a port inside `rect` (half-open, as the grid's base cells):
    /// out ports on the right edge of the source window at the callsite line, in ports on the
    /// left edge of the target. Candidates come from the windows the R*Tree finds around `rect`.
    pub fn tile_edges(&self, view_id: &str, rect: &Rect) -> Result<Vec<TileEdge>> {
        let conn = self.conn.lock().unwrap();
        let (x0, x1, y0, y1) = (rect.x, rect.x + rect.w, rect.y, rect.y + rect.h);
        let mut st = conn.prepare_cached(
            "WITH c AS MATERIALIZED (
               SELECT w.window_id FROM canvas_window_rtree t JOIN canvas_window w ON w.wid = t.wid
               WHERE t.max_x >= ?2 AND t.min_x <= ?3 AND t.max_y >= ?4 AND t.min_y <= ?5
                 AND +w.view_id = ?1),
             l AS MATERIALIZED (
               SELECT e.* FROM c CROSS JOIN link_edge e INDEXED BY link_edge_from
                 ON e.view_id = ?1 AND e.from_window = c.window_id
               UNION
               SELECT e.* FROM c CROSS JOIN link_edge e INDEXED BY link_edge_to
                 ON e.view_id = ?1 AND e.to_window = c.window_id)
             SELECT l.link_id, l.kind, l.from_window, l.to_window, f.cluster_id, t.cluster_id,
                    f.x + f.w, f.y + ?6 + (l.from_line - f.start_line) * ?7 + ?7 / 2.0,
                    t.x, t.y + ?6 + (l.to_line - t.start_line) * ?7 + ?7 / 2.0, l.confidence
             FROM l
             CROSS JOIN canvas_window f ON f.view_id = ?1 AND f.window_id = l.from_window
             CROSS JOIN canvas_window t ON t.view_id = ?1 AND t.window_id = l.to_window
             WHERE l.to_dynamic IS NULL
             ORDER BY l.ord",
        )?;
        let rows = st.query_map(params![view_id, x0, x1, y0, y1, HEADER_WU, LINE_WU], |r| {
            Ok(TileEdge {
                id: r.get(0)?,
                kind: r.get(1)?,
                from: r.get(2)?,
                to: r.get(3)?,
                from_cluster: r.get(4)?,
                to_cluster: r.get(5)?,
                a: [r.get(6)?, r.get(7)?],
                b: [r.get(8)?, r.get(9)?],
                confidence: r.get(10)?,
            })
        })?;
        let inside = |p: [f64; 2]| p[0] >= x0 && p[0] < x1 && p[1] >= y0 && p[1] < y1;
        let mut out = Vec::new();
        for e in rows {
            let e = e?;
            if inside(e.a) || inside(e.b) {
                out.push(e);
            }
        }
        Ok(out)
    }

    // ── Search ──

    /// Whether search rows exist for `scope`.
    pub fn has_search(&self, scope: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT 1 FROM search_scope WHERE scope = ?1",
                [scope],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Replaces the search rows of `view_id` with `rows` under `scope`, in this order.
    pub fn put_search(&self, view_id: &str, scope: &str, rows: &[SearchRow]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        delete_search(&tx, view_id)?;
        {
            let mut st = tx.prepare_cached(
                "INSERT INTO search_fts (scope, kind, label, detail, ref) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for r in rows {
                st.execute(params![scope, r.kind, r.label, r.detail, r.reference])?;
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO search_scope (scope, view_id, rows, written_at) VALUES (?1, ?2, ?3, ?4)",
            params![scope, view_id, rows.len() as i64, now_iso()],
        )?;
        // Bounded: the newest scopes of views that are no longer saved go first.
        let stale: Vec<String> = {
            let mut st = tx.prepare(
                "SELECT s.scope FROM search_scope s
                 ORDER BY (s.view_id IN (SELECT view_id FROM canvas_view)) DESC, s.written_at DESC
                 LIMIT -1 OFFSET ?1",
            )?;
            st.query_map([MAX_SEARCH_SCOPES], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        for s in stale {
            tx.execute("DELETE FROM search_fts WHERE scope = ?1", [&s])?;
            tx.execute("DELETE FROM search_scope WHERE scope = ?1", [&s])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Rows of `scope` whose label or detail contains `q` (case-insensitive), in insertion order.
    /// `q` of fewer than 3 characters has no trigram, so every row of the scope is returned (the
    /// caller filters).
    pub fn search(&self, scope: &str, q: &str) -> Result<Vec<SearchRow>> {
        let conn = self.conn.lock().unwrap();
        let map = |r: &rusqlite::Row<'_>| {
            Ok(SearchRow {
                kind: r.get(0)?,
                label: r.get(1)?,
                detail: r.get(2)?,
                reference: r.get(3)?,
            })
        };
        if q.chars().count() < 3 {
            let mut st = conn.prepare_cached(
                "SELECT kind, label, detail, ref FROM search_fts WHERE scope = ?1 ORDER BY rowid",
            )?;
            return Ok(st
                .query_map([scope], map)?
                .collect::<rusqlite::Result<_>>()?);
        }
        let phrase = format!("\"{}\"", q.replace('"', "\"\""));
        let mut st = conn.prepare_cached(
            "SELECT kind, label, detail, ref FROM search_fts WHERE search_fts MATCH ?1 AND scope = ?2
             ORDER BY rowid",
        )?;
        Ok(st
            .query_map(params![phrase, scope], map)?
            .collect::<rusqlite::Result<_>>()?)
    }
}

fn delete_search(c: &Connection, view_id: &str) -> rusqlite::Result<()> {
    let scopes: Vec<String> = {
        let mut st = c.prepare_cached("SELECT scope FROM search_scope WHERE view_id = ?1")?;
        st.query_map([view_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for s in scopes {
        c.execute("DELETE FROM search_fts WHERE scope = ?1", [&s])?;
        c.execute("DELETE FROM search_scope WHERE scope = ?1", [&s])?;
    }
    Ok(())
}

fn delete_view(c: &Connection, view_id: &str) -> rusqlite::Result<()> {
    c.execute(
        "DELETE FROM canvas_window_rtree WHERE wid IN (SELECT wid FROM canvas_window WHERE view_id = ?1)",
        [view_id],
    )?;
    for t in [
        "link_edge",
        "canvas_window",
        "canvas_cluster",
        "canvas_file",
        "canvas_view",
    ] {
        c.execute(&format!("DELETE FROM {t} WHERE view_id = ?1"), [view_id])?;
    }
    delete_search(c, view_id)
}

fn windows_in_rect(conn: &Connection, view_id: &str, r: &Rect) -> Result<Vec<usize>> {
    let (x0, x1, y0, y1) = (r.x, r.x + r.w, r.y, r.y + r.h);
    let mut st = conn.prepare_cached(
        "SELECT w.ord FROM canvas_window_rtree t JOIN canvas_window w ON w.wid = t.wid
         WHERE t.max_x >= ?2 AND t.min_x <= ?3 AND t.max_y >= ?4 AND t.min_y <= ?5
           AND +w.view_id = ?1
           AND w.x < ?3 AND w.x + w.w > ?2 AND w.y < ?5 AND w.y + w.h > ?4
         ORDER BY w.ord",
    )?;
    Ok(st
        .query_map(params![view_id, x0, x1, y0, y1], |r| {
            r.get::<_, i64>(0).map(|o| o as usize)
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// The bootstrap stored for `view_id` (counts given: they come from the loaded rows or a count).
fn bootstrap_of(
    conn: &Connection,
    view_id: &str,
    n_windows: u32,
    n_files: u32,
) -> Result<Option<ViewBootstrap>> {
    let head = conn
        .query_row(
            "SELECT spec_json, op_id, head, status, layout_version, world_json, grid_json, rest_json
             FROM canvas_view WHERE view_id = ?1",
            [view_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, u32>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                ))
            },
        )
        .optional()?;
    let Some((spec, op, head, status, lv, world, grid, rest)) = head else {
        return Ok(None);
    };
    let grid: Value = serde_json::from_str(&grid)?;
    let mut rest: Value = serde_json::from_str(&rest)?;

    let clusters: Vec<Cluster> = {
        let mut st = conn.prepare_cached(
                "SELECT cluster_id, parent_id, lane, label, label_source, summary, x, y, w, h, heat, conn,
                    windows, open_threads, conflicts, pinned, heat_level, review
                 FROM canvas_cluster WHERE view_id = ?1 ORDER BY ord",
            )?;
        st.query_map([view_id], |r| {
            Ok(Cluster {
                id: r.get(0)?,
                parent: r.get(1)?,
                lane: r.get(2)?,
                label: r.get(3)?,
                label_source: r.get(4)?,
                summary: r.get(5)?,
                rect: Rect {
                    x: r.get(6)?,
                    y: r.get(7)?,
                    w: r.get(8)?,
                    h: r.get(9)?,
                },
                heat: r.get(10)?,
                conn: r.get(11)?,
                windows: r.get(12)?,
                open_threads: r.get(13)?,
                conflicts: r.get(14)?,
                pinned: r.get::<_, i64>(15)? != 0,
                heat_level: r.get(16)?,
                review: r.get(17)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    let n_clusters = clusters.len() as u32;
    let bootstrap = ViewBootstrap {
        view: ViewRef {
            id: view_id.to_string(),
            spec: serde_json::from_str(&spec)?,
            resolved: cv_contracts::Resolved {
                op,
                head: head.clone(),
                commits: serde_json::from_value(rest["commits"].take())
                    .unwrap_or_else(|_| vec![head.clone()]),
            },
            status,
            layout_version: lv,
            shared_placement: serde_json::from_value(rest["sharedPlacement"].take())
                .unwrap_or_default(),
            stats: ViewStats {
                windows: n_windows,
                files: n_files,
                clusters: n_clusters,
            },
            warnings: serde_json::from_value(rest["warnings"].take()).unwrap_or_default(),
        },
        world: serde_json::from_str(&world)?,
        lane_defs: serde_json::from_value(grid["laneDefs"].clone())?,
        lanes: serde_json::from_value(grid["lanes"].clone())?,
        columns: serde_json::from_value(grid["columns"].clone())?,
        clusters,
        hubs: serde_json::from_value(rest["hubs"].take())?,
        bundles: serde_json::from_value(rest["bundles"].take())?,
        churn: serde_json::from_value(rest["churn"].take())?,
        heat_labels: serde_json::from_value(rest["heatLabels"].take())?,
    };
    Ok(Some(bootstrap))
}

fn load_windows(conn: &Connection, view_id: &str) -> Result<Vec<WindowHeader>> {
    select_windows(conn, "WHERE w.view_id = ?1", params![view_id], true)
}

/// Window headers in window order. Fragment siblings are the windows of the same path that
/// carry a fragment index, in index order.
fn select_windows(
    conn: &Connection,
    where_clause: &str,
    args: impl rusqlite::Params,
    whole_view: bool,
) -> Result<Vec<WindowHeader>> {
    let sql = format!(
        "SELECT w.window_id, w.window_key, w.kind, w.cluster_id, w.lane, w.path, w.lang, w.commit_id,
            w.change_id, w.blob_id, w.symbol_name, w.symbol_qualname, w.symbol_kind, w.start_line,
            w.end_line, w.file_lines, w.x, w.y, w.w, w.h, w.heat_json, w.conn_json, w.risk,
            w.review_first, w.fragment_index, w.fragment_of, w.chip, w.open_threads, w.ports,
            w.conflicts, w.pinned, w.truncated, w.diff_stat_json, w.view_id
         FROM canvas_window w {where_clause} ORDER BY w.ord"
    );
    let mut st = conn.prepare_cached(&sql)?;
    type Row = (
        WindowHeader,
        String,
        String,
        Option<String>,
        Option<(u32, u32, u32)>,
        String,
    );
    let rows: Vec<Row> = st
        .query_map(args, |r| {
            let symbol = match (
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<String>>(11)?,
                r.get::<_, Option<String>>(12)?,
            ) {
                (Some(name), Some(qualname), Some(kind)) => Some(SymbolRef {
                    name,
                    qualname,
                    kind,
                }),
                _ => None,
            };
            let frag = match (
                r.get::<_, Option<u32>>(24)?,
                r.get::<_, Option<u32>>(25)?,
                r.get::<_, Option<u32>>(26)?,
            ) {
                (Some(i), Some(of), Some(chip)) => Some((i, of, chip)),
                _ => None,
            };
            Ok((
                WindowHeader {
                    id: r.get(0)?,
                    key: r.get(1)?,
                    kind: r.get(2)?,
                    cluster: r.get(3)?,
                    lane: r.get(4)?,
                    path: r.get(5)?,
                    lang: r.get(6)?,
                    rev: r.get(7)?,
                    change: r.get(8)?,
                    blob: r.get(9)?,
                    symbol,
                    lines: LineSpan {
                        start: r.get(13)?,
                        end: r.get(14)?,
                        file_lines: r.get(15)?,
                    },
                    rect: Rect {
                        x: r.get(16)?,
                        y: r.get(17)?,
                        w: r.get(18)?,
                        h: r.get(19)?,
                    },
                    heat: Heat {
                        grade: 0,
                        rate: 0.0,
                        ci: [0.0, 0.0],
                        settled: false,
                        touches: 0,
                        in_range: None,
                    },
                    conn: Connectedness {
                        grade: 0,
                        dependents2: 0,
                        fan_in: 0,
                        fan_out: 0,
                        tests: 0,
                        reach: 0,
                        cycle: None,
                    },
                    risk: r.get(22)?,
                    review_first: r.get::<_, i64>(23)? != 0,
                    fragment: None,
                    counts: WindowCounts {
                        open_threads: r.get(27)?,
                        ports: r.get(28)?,
                        conflicts: r.get(29)?,
                    },
                    flags: WindowFlags {
                        pinned: r.get::<_, i64>(30)? != 0,
                        truncated: r.get::<_, i64>(31)? != 0,
                    },
                    diff_stat: None,
                },
                r.get::<_, String>(20)?,
                r.get::<_, String>(21)?,
                r.get::<_, Option<String>>(32)?,
                frag,
                r.get::<_, String>(33)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::with_capacity(rows.len());
    /// (position in `out`, view, path, (index, of, chip)).
    type Frag = (usize, String, String, (u32, u32, u32));
    let mut frags: Vec<Frag> = Vec::new();
    for (mut h, heat, conn_json, diff, frag, view) in rows {
        h.heat = serde_json::from_str(&heat)?;
        h.conn = serde_json::from_str(&conn_json)?;
        h.diff_stat = diff
            .map(|d| serde_json::from_str::<DiffStat>(&d))
            .transpose()?;
        if let Some(f) = frag {
            frags.push((out.len(), view, h.path.clone(), f));
        }
        out.push(h);
    }
    if frags.is_empty() {
        return Ok(out);
    }
    let mut siblings: std::collections::HashMap<(String, String), Vec<(u32, String)>> =
        std::collections::HashMap::new();
    if whole_view {
        for (i, view, path, (index, _, _)) in &frags {
            siblings
                .entry((view.clone(), path.clone()))
                .or_default()
                .push((*index, out[*i].id.clone()));
        }
    } else {
        // Siblings come from the whole view, not only the selected rows.
        let mut sib = conn.prepare_cached(
            "SELECT fragment_index, window_id FROM canvas_window WHERE view_id = ?1 AND path = ?2
             AND fragment_index IS NOT NULL ORDER BY ord",
        )?;
        for (_, view, path, _) in &frags {
            if let std::collections::hash_map::Entry::Vacant(e) =
                siblings.entry((view.clone(), path.clone()))
            {
                let ids = sib
                    .query_map(params![view, path], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                e.insert(ids);
            }
        }
    }
    for list in siblings.values_mut() {
        list.sort_by_key(|(index, _)| *index);
    }
    for (i, view, path, (index, of, chip)) in frags {
        let ids = &siblings[&(view, path)];
        out[i].fragment = Some(FileFragment {
            index,
            of,
            chip,
            siblings: ids.iter().map(|(_, id)| id.clone()).collect(),
        });
    }
    Ok(out)
}
