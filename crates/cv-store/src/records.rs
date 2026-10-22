//! review.db records that earlier builds kept as JSON in `meta` (cv.links.v1, cv.ranges.v1,
//! cv.layout.v1): declared links and link verdicts, saved ranges, window and cluster pins, and
//! cluster labels. Each kind is read and written as a whole list (the callers' read-modify-write
//! under their own lock), in the order the list has: tables keep it in `ord` or rowid order.
//!
//! [`upgrade`] brings a version-1 file to version 2 on open: the redefined tables (never written
//! by version 1) are recreated, and the JSON records move into them.

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::Value;

use crate::{Result, fnv64, get_meta, now_iso};

/// Pre-tables JSON record keys in `meta`.
pub const LINKS_KEY: &str = "cv.links.v1";
pub const RANGES_KEY: &str = "cv.ranges.v1";
pub const LAYOUT_KEY: &str = "cv.layout.v1";
/// Bumped on every write of declared links or verdicts (cheap change check for caches).
const LINKS_REV: &str = "links_rev";

pub const REVIEW_SCHEMA_VERSION: &str = "2";

/// Version-2 definitions of the tables version 1 declared but never wrote.
const V2_TABLES: &str = "
CREATE TABLE layout_pin (
  view_key   TEXT NOT NULL,
  item_key   TEXT NOT NULL,
  x          REAL NOT NULL,
  y          REAL NOT NULL,
  missed     INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (view_key, item_key)
) STRICT, WITHOUT ROWID;
CREATE TABLE cluster_pin (
  ord          INTEGER PRIMARY KEY,
  item_key     TEXT NOT NULL,
  lane         TEXT NOT NULL,
  members_json TEXT NOT NULL,
  x            REAL NOT NULL,
  y            REAL NOT NULL,
  missed       INTEGER NOT NULL DEFAULT 0,
  updated_at   TEXT NOT NULL
) STRICT;
CREATE TABLE link_feedback (
  link_key   TEXT PRIMARY KEY,
  link_id    TEXT NOT NULL,
  verdict    TEXT NOT NULL CHECK (verdict IN ('confirmed', 'rejected')),
  actor_json TEXT NOT NULL,
  note       TEXT,
  at         TEXT NOT NULL,
  link_json  TEXT,
  ord        INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
CREATE TABLE cluster_label (
  ord          INTEGER PRIMARY KEY,
  members_hash TEXT NOT NULL,
  lane         TEXT NOT NULL,
  members_json TEXT NOT NULL,
  label        TEXT NOT NULL,
  summary      TEXT,
  source       TEXT NOT NULL CHECK (source IN ('agent', 'human')),
  missed       INTEGER NOT NULL DEFAULT 0,
  at           TEXT NOT NULL
) STRICT;
CREATE INDEX cluster_label_members ON cluster_label(members_hash);
";

fn table_exists(c: &Connection, name: &str) -> rusqlite::Result<bool> {
    c.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
}

/// Upgrades a version-1 review.db in place (one transaction) and moves the JSON records out of
/// `meta`. A no-op on version 2.
pub(crate) fn upgrade(conn: &mut Connection) -> Result<()> {
    let version = get_meta(conn, "schema_version")?.unwrap_or_default();
    let tx = conn.transaction()?;
    if version == "1" {
        for t in [
            "layout_pin",
            "cluster_pin",
            "link_feedback",
            "cluster_label",
        ] {
            if !table_exists(&tx, t)? {
                continue;
            }
            let rows: i64 = tx.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))?;
            if rows == 0 {
                tx.execute_batch(&format!("DROP TABLE {t}"))?;
            } else {
                // Not written by version 1; kept aside rather than lost if something did.
                tx.execute_batch(&format!("ALTER TABLE {t} RENAME TO {t}_v1"))?;
            }
        }
        tx.execute_batch("DROP INDEX IF EXISTS cluster_label_members")?;
        tx.execute_batch(V2_TABLES)?;
        tx.execute(
            "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
            [REVIEW_SCHEMA_VERSION],
        )?;
    }
    migrate_json(&tx)?;
    tx.commit()?;
    Ok(())
}

/// Moves the JSON records of earlier builds into the tables, then removes the meta keys. A record
/// that does not parse stays in meta under `<key>.unreadable` (earlier builds read it as empty).
fn migrate_json(tx: &Transaction<'_>) -> Result<()> {
    for key in [LINKS_KEY, RANGES_KEY, LAYOUT_KEY] {
        let Some(raw) = get_meta(tx, key)? else {
            continue;
        };
        let parsed: std::result::Result<Value, _> = serde_json::from_str(&raw);
        match parsed {
            Ok(v) => match key {
                LINKS_KEY => migrate_links(tx, &v)?,
                RANGES_KEY => migrate_ranges(tx, &v)?,
                _ => migrate_layout(tx, &v)?,
            },
            Err(_) if raw.trim().is_empty() => {}
            Err(_) => {
                tx.execute(
                    "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
                    params![format!("{key}.unreadable"), raw],
                )?;
            }
        }
        tx.execute("DELETE FROM meta WHERE key = ?1", [key])?;
    }
    Ok(())
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn opt_s(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

fn js(v: &Value) -> String {
    v.to_string()
}

fn migrate_links(tx: &Transaction<'_>, v: &Value) -> Result<()> {
    let declared: Vec<DeclaredLinkRow> = v["declared"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|d| DeclaredLinkRow {
            id: s(&d["id"]),
            kind: s(&d["kind"]),
            from_json: js(&d["from"]),
            to_json: js(&d["to"]),
            label: s(&d["label"]),
            note: opt_s(&d["note"]),
            actor_json: js(&d["by"]),
            at: s(&d["at"]),
        })
        .collect();
    let ratings: Vec<LinkFeedbackRow> = v["ratings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| matches!(r["verdict"].as_str(), Some("confirmed" | "rejected")))
        .map(|r| LinkFeedbackRow {
            key: s(&r["key"]),
            link_id: s(&r["id"]),
            verdict: s(&r["verdict"]),
            note: opt_s(&r["note"]),
            actor_json: js(&r["by"]),
            at: s(&r["at"]),
            link_json: (!r["link"].is_null()).then(|| js(&r["link"])),
        })
        .collect();
    write_links(tx, &declared, &ratings)
}

fn migrate_ranges(tx: &Transaction<'_>, v: &Value) -> Result<()> {
    let rows: Vec<SavedRangeRow> = v
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| SavedRangeRow {
            name: s(&r["name"]),
            revset: s(&r["revset"]),
            fileset: opt_s(&r["fileset"]),
            options_json: (!r["options"].is_null()).then(|| js(&r["options"])),
            created_by_json: js(&r["createdBy"]),
        })
        .collect();
    write_ranges(tx, &rows)
}

fn migrate_layout(tx: &Transaction<'_>, v: &Value) -> Result<()> {
    let members = |m: &Value| -> Vec<String> {
        m.as_array()
            .into_iter()
            .flatten()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect()
    };
    let missed = |m: &Value| m.as_u64().unwrap_or(0) as u32;
    let mut rows = LayoutRows::default();
    if let Some(pins) = v["pins"].as_object() {
        for (k, xy) in pins {
            let (x, y) = (xy[0].as_f64().unwrap_or(0.0), xy[1].as_f64().unwrap_or(0.0));
            rows.pins.push(WindowPinRow {
                key: k.clone(),
                x,
                y,
                missed: missed(&v["pinMissed"][k]),
            });
        }
    }
    for p in v["clusterPins"].as_array().into_iter().flatten() {
        rows.cluster_pins.push(ClusterPinRow {
            key: s(&p["key"]),
            lane: s(&p["lane"]),
            members: members(&p["members"]),
            x: p["x"].as_f64().unwrap_or(0.0),
            y: p["y"].as_f64().unwrap_or(0.0),
            missed: missed(&p["missed"]),
        });
    }
    for l in v["labels"].as_array().into_iter().flatten() {
        let source = match l["labelSource"].as_str() {
            Some("agent") => "agent",
            _ => "human",
        };
        rows.labels.push(ClusterLabelRow {
            lane: s(&l["lane"]),
            members: members(&l["members"]),
            label: s(&l["label"]),
            source: source.into(),
            summary: opt_s(&l["summary"]),
            missed: missed(&l["missed"]),
        });
    }
    write_layout(tx, &rows)
}

// ─── Declared links and verdicts ─────────────────────────────────────────────

/// A declared link (`declared_link`); `from_json`/`to_json` are `{path, line}` or `{hub}`.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclaredLinkRow {
    pub id: String,
    pub kind: String,
    pub from_json: String,
    pub to_json: String,
    pub label: String,
    pub note: Option<String>,
    pub actor_json: String,
    pub at: String,
}

/// A verdict on a link (`link_feedback`), keyed by what the link connects.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkFeedbackRow {
    pub key: String,
    pub link_id: String,
    pub verdict: String,
    pub note: Option<String>,
    pub actor_json: String,
    pub at: String,
    pub link_json: Option<String>,
}

pub(crate) fn write_links(
    tx: &Transaction<'_>,
    declared: &[DeclaredLinkRow],
    ratings: &[LinkFeedbackRow],
) -> Result<()> {
    tx.execute("DELETE FROM declared_link", [])?;
    tx.execute("DELETE FROM link_feedback", [])?;
    {
        let mut st = tx.prepare_cached(
            "INSERT OR REPLACE INTO declared_link (id, kind, from_ref, to_ref, label, note, actor_json, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for d in declared {
            st.execute(params![
                d.id,
                d.kind,
                d.from_json,
                d.to_json,
                d.label,
                d.note,
                d.actor_json,
                d.at
            ])?;
        }
        let mut st = tx.prepare_cached(
            "INSERT OR REPLACE INTO link_feedback (link_key, link_id, verdict, actor_json, note, at, link_json, ord)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for (i, r) in ratings.iter().enumerate() {
            st.execute(params![
                r.key,
                r.link_id,
                r.verdict,
                r.actor_json,
                r.note,
                r.at,
                r.link_json,
                i as i64
            ])?;
        }
    }
    tx.execute(
        "INSERT INTO meta (key, value) VALUES (?1, '1')
         ON CONFLICT(key) DO UPDATE SET value = CAST(value AS INTEGER) + 1",
        [LINKS_REV],
    )?;
    Ok(())
}

pub(crate) fn read_links(c: &Connection) -> Result<(Vec<DeclaredLinkRow>, Vec<LinkFeedbackRow>)> {
    let mut st = c.prepare_cached(
        "SELECT id, kind, from_ref, to_ref, label, note, actor_json, at FROM declared_link ORDER BY rowid",
    )?;
    let declared = st
        .query_map([], |r| {
            Ok(DeclaredLinkRow {
                id: r.get(0)?,
                kind: r.get(1)?,
                from_json: r.get(2)?,
                to_json: r.get(3)?,
                label: r.get(4)?,
                note: r.get(5)?,
                actor_json: r.get(6)?,
                at: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut st = c.prepare_cached(
        "SELECT link_key, link_id, verdict, note, actor_json, at, link_json FROM link_feedback ORDER BY ord",
    )?;
    let ratings = st
        .query_map([], |r| {
            Ok(LinkFeedbackRow {
                key: r.get(0)?,
                link_id: r.get(1)?,
                verdict: r.get(2)?,
                note: r.get(3)?,
                actor_json: r.get(4)?,
                at: r.get(5)?,
                link_json: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((declared, ratings))
}

/// The links revision: `None` until declared links or verdicts were first written.
pub(crate) fn links_rev(c: &Connection) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT CAST(value AS INTEGER) FROM meta WHERE key = ?1",
        [LINKS_REV],
        |r| r.get(0),
    )
    .optional()?)
}

// ─── Saved ranges ────────────────────────────────────────────────────────────

/// A saved range (`saved_range`); `options_json` is RangeOptions, `created_by_json` an Actor.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedRangeRow {
    pub name: String,
    pub revset: String,
    pub fileset: Option<String>,
    pub options_json: Option<String>,
    pub created_by_json: String,
}

/// Replaces the saved ranges with `rows` (in this order); a name kept keeps its `created_at`.
pub(crate) fn write_ranges(tx: &Transaction<'_>, rows: &[SavedRangeRow]) -> Result<()> {
    let created: std::collections::HashMap<String, String> = {
        let mut st = tx.prepare_cached("SELECT name, created_at FROM saved_range")?;
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    tx.execute("DELETE FROM saved_range", [])?;
    let now = now_iso();
    let mut st = tx.prepare_cached(
        "INSERT OR REPLACE INTO saved_range (name, revset, fileset, options_json, created_by_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for r in rows {
        let at = created.get(&r.name).unwrap_or(&now);
        st.execute(params![
            r.name,
            r.revset,
            r.fileset,
            r.options_json,
            r.created_by_json,
            at
        ])?;
    }
    Ok(())
}

pub(crate) fn read_ranges(c: &Connection) -> Result<Vec<SavedRangeRow>> {
    let mut st = c.prepare_cached(
        "SELECT name, revset, fileset, options_json, created_by_json FROM saved_range ORDER BY rowid",
    )?;
    Ok(st
        .query_map([], |r| {
            Ok(SavedRangeRow {
                name: r.get(0)?,
                revset: r.get(1)?,
                fileset: r.get(2)?,
                options_json: r.get(3)?,
                created_by_json: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

// ─── Pins and labels ─────────────────────────────────────────────────────────

/// A pinned window of snapshot views (`layout_pin`, view_key "snapshot").
#[derive(Debug, Clone, PartialEq)]
pub struct WindowPinRow {
    pub key: String,
    pub x: f64,
    pub y: f64,
    /// Consecutive full indexes without this key.
    pub missed: u32,
}

/// A pinned cluster (`cluster_pin`).
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterPinRow {
    pub key: String,
    pub lane: String,
    pub members: Vec<String>,
    pub x: f64,
    pub y: f64,
    pub missed: u32,
}

/// A human or agent cluster label (`cluster_label`).
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterLabelRow {
    pub lane: String,
    pub members: Vec<String>,
    pub label: String,
    /// "agent" or "human".
    pub source: String,
    pub summary: Option<String>,
    pub missed: u32,
}

/// Pins and labels, each list in its stored order (window pins by key).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayoutRows {
    pub pins: Vec<WindowPinRow>,
    pub cluster_pins: Vec<ClusterPinRow>,
    pub labels: Vec<ClusterLabelRow>,
}

const SNAPSHOT: &str = "snapshot";

fn members_hash(lane: &str, members: &[String]) -> String {
    let mut sorted: Vec<&str> = members.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    format!(
        "{:016x}",
        fnv64(format!("{lane}\n{}", sorted.join("\n")).as_bytes())
    )
}

pub(crate) fn write_layout(tx: &Transaction<'_>, rows: &LayoutRows) -> Result<()> {
    let now = now_iso();
    tx.execute("DELETE FROM layout_pin WHERE view_key = ?1", [SNAPSHOT])?;
    tx.execute("DELETE FROM cluster_pin", [])?;
    tx.execute("DELETE FROM cluster_label", [])?;
    let mut st = tx.prepare_cached(
        "INSERT OR REPLACE INTO layout_pin (view_key, item_key, x, y, missed, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for p in &rows.pins {
        st.execute(params![SNAPSHOT, p.key, p.x, p.y, p.missed, now])?;
    }
    let mut st = tx.prepare_cached(
        "INSERT INTO cluster_pin (ord, item_key, lane, members_json, x, y, missed, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for (i, p) in rows.cluster_pins.iter().enumerate() {
        st.execute(params![
            i as i64,
            p.key,
            p.lane,
            serde_json::to_string(&p.members)?,
            p.x,
            p.y,
            p.missed,
            now
        ])?;
    }
    let mut st = tx.prepare_cached(
        "INSERT INTO cluster_label (ord, members_hash, lane, members_json, label, summary, source, missed, at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for (i, l) in rows.labels.iter().enumerate() {
        st.execute(params![
            i as i64,
            members_hash(&l.lane, &l.members),
            l.lane,
            serde_json::to_string(&l.members)?,
            l.label,
            l.summary,
            l.source,
            l.missed,
            now
        ])?;
    }
    Ok(())
}

pub(crate) fn read_layout(c: &Connection) -> Result<LayoutRows> {
    let mut out = LayoutRows::default();
    let mut st = c.prepare_cached(
        "SELECT item_key, x, y, missed FROM layout_pin WHERE view_key = ?1 ORDER BY item_key",
    )?;
    out.pins = st
        .query_map([SNAPSHOT], |r| {
            Ok(WindowPinRow {
                key: r.get(0)?,
                x: r.get(1)?,
                y: r.get(2)?,
                missed: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut st = c.prepare_cached(
        "SELECT item_key, lane, members_json, x, y, missed FROM cluster_pin ORDER BY ord",
    )?;
    let pins = st
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, u32>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (key, lane, members, x, y, missed) in pins {
        out.cluster_pins.push(ClusterPinRow {
            key,
            lane,
            members: serde_json::from_str(&members)?,
            x,
            y,
            missed,
        });
    }
    let mut st = c.prepare_cached(
        "SELECT lane, members_json, label, source, summary, missed FROM cluster_label ORDER BY ord",
    )?;
    let labels = st
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, u32>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (lane, members, label, source, summary, missed) in labels {
        out.labels.push(ClusterLabelRow {
            lane,
            members: serde_json::from_str(&members)?,
            label,
            source,
            summary,
            missed,
        });
    }
    Ok(out)
}
