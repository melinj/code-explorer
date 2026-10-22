import type { CoChangeClusterPair, CoChangePair, Cluster, Rect } from "../contracts/model";
import { dashedPolyline } from "./edges";
import { LabelBatch, sansFont, textWidth } from "./labels";
import type { RectBatch } from "./rects";
import { mix, step, type Theme } from "./theme";
import { coStrength } from "./coStrength";

export { coStrength, wilsonLower } from "./coStrength";

/** One co-change partner: how strongly the two change together, and whether a static link joins them. */
export interface CoPartner {
  jaccard: number;
  support: number;
  linked: boolean;
}

export type CoMap = Map<string, Map<string, CoPartner>>;

/**
 * The co-change lens at two levels of the map: file partners (pairs without a static link) and
 * cluster partners. Cluster pairs come from the server (commits touching both clusters); an older
 * server sends only file pairs, which are then folded onto their clusters.
 */
export class CoChange {
  readonly files: CoMap = new Map();
  readonly clusters: CoMap = new Map();
  /** Files in some pair (windows of other files fade under the lens). */
  readonly paths = new Set<string>();
  /** The cluster of each paired file's first window: where to point at a file that is not loaded. */
  readonly fileCluster = new Map<string, string>();

  constructor(pairs: readonly CoChangePair[], clusterPairs?: readonly CoChangeClusterPair[]) {
    for (const p of pairs) {
      put(this.files, p.a, p.b, { jaccard: p.jaccard, support: p.support, linked: false });
      this.paths.add(p.a).add(p.b);
      if (p.clusterA) this.fileCluster.set(p.a, p.clusterA);
      if (p.clusterB) this.fileCluster.set(p.b, p.clusterB);
    }
    if (clusterPairs?.length) {
      for (const p of clusterPairs) put(this.clusters, p.a, p.b, { jaccard: p.jaccard, support: p.support, linked: p.linked });
    } else {
      for (const p of pairs) {
        if (p.clusterA && p.clusterB && p.clusterA !== p.clusterB) put(this.clusters, p.clusterA, p.clusterB, { jaccard: p.jaccard, support: p.support, linked: false });
      }
    }
  }

  /** Strength (coStrength) of a cluster's strongest partner; -1 when it has none. */
  strongest(cluster: string) {
    let best = this.best.get(cluster);
    if (best === undefined) {
      best = -1;
      for (const p of this.clusters.get(cluster)?.values() ?? []) best = Math.max(best, coStrength(p, "cluster"));
      this.best.set(cluster, best);
    }
    return best;
  }
  private best = new Map<string, number>();
}

function put(m: CoMap, a: string, b: string, p: CoPartner) {
  if (a === b) return;
  for (const [x, y] of [
    [a, b],
    [b, a],
  ]) {
    let row = m.get(x);
    if (!row) m.set(x, (row = new Map()));
    const old = row.get(y);
    if (!old || old.jaccard < p.jaccard) row.set(y, p);
  }
}

/** Most partners drawn with an arc and a label; the rest still glow. */
export const MAX_ARCS = 24;

/**
 * A focus and its partners: an arc from the focus to each partner (dashed when no static link joins
 * them: the coupling the code does not show), width and strength by jaccard, over a background
 * casing so it reads across fills; then a ring around each partner and the focus. Sizes are in
 * screen px (`px` = world units per px).
 */
export function drawPartners(
  rb: RectBatch,
  t: Theme,
  px: number,
  focus: Rect,
  partners: Array<{ rect: Rect; p: CoPartner; k: number }>,
  ringGap = 0,
) {
  const c = t.couple;
  const [fx, fy] = centre(focus);
  for (const { rect, p, k: k0 } of partners.slice(0, MAX_ARCS)) {
    const k = step(k0);
    const [x, y] = centre(rect);
    const pts = arc(fx, fy, x, y);
    const w = (1.4 + 2.6 * k) * px;
    rb.polyline(pts, w + 3 * px, t.bg, 0.55);
    if (p.linked) rb.polyline(pts, w, c, 0.45 + 0.35 * k);
    else dashedPolyline(rb, pts, 9 * px, 5 * px, w, c, 0.6 + 0.4 * k);
  }
  for (const { rect, k: k0 } of partners) {
    const k = step(k0);
    const g = ringGap + 2 * px;
    rb.ring(rect.x - g, rect.y - g, rect.w + 2 * g, rect.h + 2 * g, (1.5 + 1.5 * k) * px, mix(c, 0xffffff, t.name === "dark" ? 0.25 * k : 0), 0.65 + 0.35 * k);
  }
  const g = ringGap + 3 * px;
  rb.ring(focus.x - g, focus.y - g, focus.w + 2 * g, focus.h + 2 * g, 3 * px, t.name === "dark" ? 0xffffff : t.ink, 0.95);
}

/**
 * Labels of a co-change set: the focus, then partners by strength, each "label  NN%" on a pill,
 * skipping any that would overlap one already placed. `size` and `px` are world units.
 */
export function labelPartners(
  rb: RectBatch,
  labels: LabelBatch,
  t: Theme,
  px: number,
  items: Array<{ rect: Rect; text: string; strong: boolean }>,
  at: "centre" | "above" = "centre",
) {
  const font = sansFont();
  const size = 12 * px;
  const placed: Rect[] = [];
  for (const it of items.slice(0, MAX_ARCS + 1)) {
    const w = textWidth(font, it.text, size);
    const cx = it.rect.x + it.rect.w / 2;
    const cy = at === "above" ? it.rect.y - size * 0.75 - 8 * px : it.rect.y + it.rect.h / 2;
    const box = { x: cx - w / 2 - 6 * px, y: cy - size * 0.75 - 3 * px, w: w + 12 * px, h: size * 1.5 + 6 * px };
    if (placed.some((p) => box.x < p.x + p.w && p.x < box.x + box.w && box.y < p.y + p.h && p.y < box.y + box.h)) continue;
    placed.push(box);
    rb.round(box.x, box.y, box.w, box.h, box.h / 2, it.strong ? t.couple : t.bg, it.strong ? 1 : 0.88);
    labels.add(it.text, cx, cy - size * 0.55, size, it.strong ? t.bg : t.ink, "center");
  }
}

/** Percent text of a jaccard ("42%"). */
export function pct(j: number) {
  return `${Math.round(j * 100)}%`;
}

export function centre(r: Rect): [number, number] {
  return [r.x + r.w / 2, r.y + r.h / 2];
}

/** A gentle arc from (ax, ay) to (bx, by), bowing to one side by a fifth of its length. */
export function arc(ax: number, ay: number, bx: number, by: number, n = 20): Float32Array {
  const dx = bx - ax;
  const dy = by - ay;
  const mx = (ax + bx) / 2 - dy * 0.2;
  const my = (ay + by) / 2 + dx * 0.2;
  const out = new Float32Array((n + 1) * 2);
  for (let i = 0; i <= n; i++) {
    const s = i / n;
    const u = 1 - s;
    out[i * 2] = u * u * ax + 2 * u * s * mx + s * s * bx;
    out[i * 2 + 1] = u * u * ay + 2 * u * s * my + s * s * by;
  }
  return out;
}

/** Partners of a cluster as drawable items, strongest first. */
export function clusterPartners(co: CoChange, focus: string, byId: ReadonlyMap<string, Cluster>) {
  const out: Array<{ c: Cluster; rect: Rect; p: CoPartner; k: number }> = [];
  for (const [id, p] of co.clusters.get(focus) ?? []) {
    const c = byId.get(id);
    if (c) out.push({ c, rect: c.rect, p, k: coStrength(p, "cluster") });
  }
  return out.sort((a, b) => b.k - a.k || b.p.jaccard - a.p.jaccard);
}
