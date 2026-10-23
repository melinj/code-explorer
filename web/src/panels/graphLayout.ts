import type { GraphRow } from "../contracts/model";

/** Row height and lane pitch of the Graph panel, in CSS pixels. */
export const ROW = 34;
export const LANE = 16;
export const GUTTER_PAD = 12;

/** Lane colours (lane 0, usually trunk and @'s branch, takes the first). Mid-tone: legible on both themes. */
export const LANE_COLORS = ["#7d95ff", "#2bb38a", "#e8a33d", "#d9629b", "#4fb3d9", "#a77ce8", "#c9b13b", "#e5735a"];
export const laneColor = (lane: number) => LANE_COLORS[lane % LANE_COLORS.length];

export const laneX = (lane: number) => GUTTER_PAD + lane * LANE;
export const rowY = (i: number) => i * ROW + ROW / 2;

export interface EdgePath {
  d: string;
  color: string;
  /** An indirect edge (commits elided between) or one leaving the rows shown. */
  dashed: boolean;
  /** Commits hidden on the edge (indirect edges). */
  elided?: number;
  child: string;
  parent: string;
}

/** An S-bend from (x0, y0) down to (x1, y1). */
function bend(x0: number, y0: number, x1: number, y1: number) {
  if (x0 === x1) return `L${x1},${y1}`;
  const m = (y0 + y1) / 2;
  return `C${x0},${m} ${x1},${m} ${x1},${y1}`;
}

/**
 * Edges of the graph as SVG paths, from the server's lanes (readside/lanes.rs): an edge leaves its
 * row into the lane it leads to (`toLane`), runs down that lane, which stays reserved for the parent,
 * and bends into the parent's lane on the parent's row. A parent outside the rows shown runs to the
 * bottom.
 */
export function edgePaths(rows: GraphRow[]): EdgePath[] {
  const at = new Map(rows.map((r, i) => [r.rev.commit, i]));
  const bottom = rows.length * ROW;
  const out: EdgePath[] = [];
  rows.forEach((r, i) => {
    const x0 = laneX(r.lane);
    const y0 = rowY(i);
    for (const e of r.edges) {
      const xt = laneX(e.toLane);
      const j = at.get(e.parent);
      let d = `M${x0},${y0}`;
      if (j === undefined) {
        d += bend(x0, y0, xt, y0 + ROW) + `L${xt},${bottom}`;
      } else if (j === i + 1) {
        d += bend(x0, y0, laneX(rows[j].lane), rowY(j));
      } else {
        const y1 = y0 + ROW;
        const y2 = rowY(j) - ROW;
        d += bend(x0, y0, xt, y1);
        if (y2 > y1) d += `L${xt},${y2}`;
        d += bend(xt, Math.max(y1, y2), laneX(rows[j].lane), rowY(j));
      }
      out.push({ d, color: laneColor(e.toLane), dashed: !!e.elided || j === undefined, elided: e.elided, child: r.rev.commit, parent: e.parent });
    }
  });
  return out;
}

/** Width of the lane gutter for these rows. */
export function gutterWidth(rows: GraphRow[]) {
  const lanes = Math.max(1, ...rows.map((r) => Math.max(r.lane, ...r.edges.map((e) => e.toLane)) + 1));
  return GUTTER_PAD * 2 + (lanes - 1) * LANE;
}

/** What the map shows for a selection of changes (Graph panel, "Selected"). */
export interface SelectionView {
  revset: string;
  diff: "cumulative" | "per-commit";
  allowNonChain: boolean;
}

/**
 * The range view of the selected commits: one change by its id; a chain (each one the parent of the
 * next) as `oldest::newest` with a cumulative diff; anything else commit by commit.
 */
export function selectionView(rows: GraphRow[], picked: string[]): SelectionView | undefined {
  const sel = rows.filter((r) => picked.includes(r.rev.commit));
  if (!sel.length) return undefined;
  const id = (r: GraphRow) => r.rev.change.slice(0, 12);
  if (sel.length === 1) return { revset: id(sel[0]), diff: "cumulative", allowNonChain: false };
  // Rows are newest first: walk oldest → newest.
  const old = [...sel].reverse();
  const chain = old.every((r, k) => k === 0 || r.rev.parents.length === 1 && r.rev.parents[0] === old[k - 1].rev.commit);
  if (chain) return { revset: `${id(old[0])}::${id(old[old.length - 1])}`, diff: "cumulative", allowNonChain: false };
  return { revset: sel.map(id).join(" | "), diff: "per-commit", allowNonChain: true };
}

/** Shift-click: every row between the last pick and this one (inclusive), in row order. */
export function pickRange(rows: GraphRow[], from: string, to: string): string[] {
  const a = rows.findIndex((r) => r.rev.commit === from);
  const b = rows.findIndex((r) => r.rev.commit === to);
  if (a < 0 || b < 0) return [to];
  const [lo, hi] = a < b ? [a, b] : [b, a];
  return rows.slice(lo, hi + 1).map((r) => r.rev.commit);
}
