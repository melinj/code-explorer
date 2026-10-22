//! `PATCH /views/:id/layout` for the synthetic view, in memory: same semantics as cv-server
//! (crates/cv-server/src/layout_api.rs) without persistence. Window keys move one window,
//! `cluster:<id>` moves a cluster with its unpinned windows; cluster rects and the world grow to
//! contain their windows, hub anchors follow, the tile index is rebuilt and layoutVersion bumps.

use std::collections::{BTreeMap, HashMap};

use cv_contracts::{HEADER_WU, LINE_WU, Rect, SiteRef, TILE_BASE_WU};
use serde::Deserialize;

use crate::synth::{self, View};

const CLUSTER_PAD: f64 = 80.0;
const CLUSTER_TITLE: f64 = 120.0;

#[derive(Deserialize)]
pub struct Move {
    pub key: String,
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutPatch {
    pub moves: Vec<Move>,
    pub base_layout_version: u32,
}

pub enum PatchError {
    Stale(u32),
    UnknownKey(String),
}

fn union(a: Rect, b: Rect) -> Rect {
    let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
    let (x1, y1) = ((a.x + a.w).max(b.x + b.w), (a.y + a.h).max(b.y + b.h));
    Rect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    }
}

/// A moved copy of `old` and the ids of the windows that moved.
pub fn apply(old: &View, p: &LayoutPatch) -> Result<(View, Vec<String>), PatchError> {
    let current = old.bootstrap.view.layout_version;
    if p.base_layout_version != current {
        return Err(PatchError::Stale(current));
    }
    let mut members: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, w) in old.windows.iter().enumerate() {
        members.entry(w.header.cluster.clone()).or_default().push(i);
    }
    // Resolve every key first: a patch applies whole or not at all.
    enum T {
        Win(Vec<usize>),
        Cluster(usize),
    }
    let mut targets = Vec::new();
    for m in &p.moves {
        if !(m.x.is_finite() && m.y.is_finite()) {
            return Err(PatchError::UnknownKey(m.key.clone()));
        }
        let t = match m.key.strip_prefix("cluster:") {
            Some(id) => {
                let cs = &old.bootstrap.clusters;
                cs.iter()
                    .position(|c| c.id == id)
                    .or_else(|| cs.iter().position(|c| c.label == id))
                    .map(T::Cluster)
            }
            None => {
                let ws: Vec<usize> = (0..old.windows.len())
                    .filter(|&w| old.windows[w].header.key == m.key)
                    .collect();
                (!ws.is_empty()).then_some(T::Win(ws))
            }
        };
        targets.push(t.ok_or_else(|| PatchError::UnknownKey(m.key.clone()))?);
    }
    let mut v = old.clone();
    let mut moved: Vec<usize> = Vec::new();
    for (m, t) in p.moves.iter().zip(targets) {
        match t {
            T::Win(ws) => {
                for w in ws {
                    let h = &mut v.windows[w].header;
                    h.rect.x = m.x;
                    h.rect.y = m.y;
                    h.flags.pinned = m.pinned;
                    moved.push(w);
                }
            }
            T::Cluster(ci) => {
                let c = &mut v.bootstrap.clusters[ci];
                let (dx, dy) = (m.x - c.rect.x, m.y - c.rect.y);
                c.rect.x = m.x;
                c.rect.y = m.y;
                c.pinned = m.pinned;
                for &w in members.get(&c.id).map(Vec::as_slice).unwrap_or(&[]) {
                    let h = &mut v.windows[w].header;
                    if h.flags.pinned {
                        continue;
                    }
                    h.rect.x += dx;
                    h.rect.y += dy;
                    moved.push(w);
                }
            }
        }
    }
    // Derived geometry: cluster rects and the world grow, hub anchors follow, tiles rebuild.
    let b = &mut v.bootstrap;
    for c in b.clusters.iter_mut() {
        for &w in members.get(&c.id).map(Vec::as_slice).unwrap_or(&[]) {
            let r = v.windows[w].header.rect;
            c.rect = union(
                c.rect,
                Rect {
                    x: r.x - CLUSTER_PAD,
                    y: r.y - CLUSTER_TITLE,
                    w: r.w + 2.0 * CLUSTER_PAD,
                    h: r.h + CLUSTER_TITLE + CLUSTER_PAD,
                },
            );
        }
        b.world = union(b.world, c.rect);
    }
    let (by_id, windows) = (&v.by_id, &v.windows);
    let port = |s: &SiteRef, out: bool| -> Option<[f64; 2]> {
        let h = &windows[*by_id.get(s.window.as_ref()?)?].header;
        let y = h.rect.y
            + HEADER_WU
            + (s.line.clamp(h.lines.start, h.lines.end) - h.lines.start) as f64 * LINE_WU
            + LINE_WU / 2.0;
        Some([if out { h.rect.x + h.rect.w } else { h.rect.x }, y])
    };
    for hub in b.hubs.iter_mut() {
        if hub.anchors.is_some() {
            let a: Vec<[f64; 2]> = hub
                .producers
                .iter()
                .filter_map(|s| port(s, true))
                .chain(hub.consumers.iter().filter_map(|s| port(s, false)))
                .collect();
            hub.anchors = Some(a);
        }
    }
    v.tiles = tiles(&v);
    v.bootstrap.view.layout_version = current + 1;
    moved.sort_unstable();
    moved.dedup();
    let ids = moved
        .iter()
        .map(|&w| v.windows[w].header.id.clone())
        .collect();
    Ok((v, ids))
}

/// Base tiles → windows intersecting them and links with a port in them (as synth builds it).
fn tiles(v: &View) -> BTreeMap<(i64, i64), (Vec<usize>, Vec<usize>)> {
    let mut tiles: BTreeMap<(i64, i64), (Vec<usize>, Vec<usize>)> = BTreeMap::new();
    let cell = |x: f64| (x / TILE_BASE_WU).floor() as i64;
    for (i, w) in v.windows.iter().enumerate() {
        let r = &w.header.rect;
        for tx in cell(r.x)..=cell(r.x + r.w) {
            for ty in cell(r.y)..=cell(r.y + r.h) {
                tiles.entry((tx, ty)).or_default().0.push(i);
            }
        }
    }
    for (li, l) in v.links.iter().enumerate() {
        let (a, b) = synth::link_points(&v.windows, l);
        let (ca, cb) = ((cell(a[0]), cell(a[1])), (cell(b[0]), cell(b[1])));
        tiles.entry(ca).or_default().1.push(li);
        if ca != cb {
            tiles.entry(cb).or_default().1.push(li);
        }
    }
    tiles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_windows_and_clusters_and_rebuilds_tiles() {
        let v = synth::generate(400, 3);
        let w0 = v.windows[0].header.clone();
        let far = (v.bootstrap.world.x + v.bootstrap.world.w + 10_000.0, 50.0);
        let patch = LayoutPatch {
            moves: vec![Move {
                key: w0.key.clone(),
                x: far.0,
                y: far.1,
                pinned: true,
            }],
            base_layout_version: 1,
        };
        let (v2, ids) = apply(&v, &patch).ok().unwrap();
        assert_eq!(ids, vec![w0.id.clone()]);
        assert_eq!(v2.bootstrap.view.layout_version, 2);
        let h = &v2.windows[0].header;
        assert_eq!((h.rect.x, h.rect.y), far);
        assert!(h.flags.pinned);
        let cell = (
            (far.0 / TILE_BASE_WU).floor() as i64,
            (far.1 / TILE_BASE_WU).floor() as i64,
        );
        assert!(v2.tiles[&cell].0.contains(&0));
        assert!(matches!(apply(&v2, &patch), Err(PatchError::Stale(2))));

        // A cluster move translates its unpinned windows by the same delta.
        let c = v2.bootstrap.clusters[1].clone();
        let patch = LayoutPatch {
            moves: vec![Move {
                key: format!("cluster:{}", c.id),
                x: c.rect.x + 1000.0,
                y: c.rect.y - 500.0,
                pinned: false,
            }],
            base_layout_version: 2,
        };
        let (v3, ids) = apply(&v2, &patch).ok().unwrap();
        assert!(!ids.is_empty());
        for (a, b) in v2.windows.iter().zip(&v3.windows) {
            if a.header.cluster == c.id && !a.header.flags.pinned {
                assert_eq!(b.header.rect.x - a.header.rect.x, 1000.0);
                assert_eq!(b.header.rect.y - a.header.rect.y, -500.0);
            }
        }
        assert_eq!(v3.bootstrap.clusters[1].rect.x, c.rect.x + 1000.0);
    }
}
