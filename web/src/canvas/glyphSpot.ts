import type { Rect } from "../contracts/model";

/** Side of an L2 district glyph on screen (px); the engine draws it at this size at any zoom. */
export const GLYPH_PX = 14;
/** The glyph card's border (px) around the 5×5 cells, and its gap from the window (px). */
export const GLYPH_BORDER_PX = 2;
export const GLYPH_GAP_PX = 6;

/**
 * A cluster's small label as the backdrop draws it (world units): font size, offset from the
 * cluster's top-left corner, and the frame width it may not use (fit to `rect.w - inset`).
 */
export const CLUSTER_LABEL = { size: 84, dx: 40, dy: 12, inset: 80 } as const;

/**
 * Where an L2 district glyph (a `g`-sized square, world units) goes for window `r`, with `px` world
 * units per screen pixel and `blocked` saying whether a spot overlaps something already drawn
 * (another window, a cluster label). At L2 a window's header is a few pixels tall, so the glyph sits
 * outside the window, at the first free spot of:
 *   1. diagonally off its top-left corner, in the crossing of the gutters: clear of the window's
 *      port pills (they sit on its sides, below its top edge) and of stacked neighbours;
 *   2. above its top-left corner;
 *   3. above its top-right corner (the first row of a cluster has the cluster label above its
 *      left part), then diagonally off that corner;
 *   4. left of its top-left corner, top-aligned with the window (columns have wide gutters);
 *   5. inside the window's top-left corner, over its header (always free of other things).
 */
export type GlyphWhere = "corner" | "above" | "above-right" | "corner-right" | "left" | "inside";
export function glyphSpot(r: Rect, g: number, px: number, blocked: (s: Rect) => boolean): { x: number; y: number; where: GlyphWhere } {
  const gap = GLYPH_GAP_PX * px;
  const pad = GLYPH_BORDER_PX * px; // the glyph's border
  const spots: Array<{ x: number; y: number; where: GlyphWhere }> = [
    { x: r.x - g - gap, y: r.y - g - gap, where: "corner" },
    { x: r.x, y: r.y - g - gap, where: "above" },
    { x: r.x + r.w - g, y: r.y - g - gap, where: "above-right" },
    { x: r.x + r.w + gap, y: r.y - g - gap, where: "corner-right" },
    { x: r.x - g - gap, y: r.y, where: "left" },
  ];
  for (const s of spots) if (!blocked({ x: s.x - pad, y: s.y - pad, w: g + 2 * pad, h: g + 2 * pad })) return s;
  return { x: r.x + 3 * px, y: r.y + 3 * px, where: "inside" };
}

/** The world rect of a cluster's small label as the backdrop draws it ([CLUSTER_LABEL]). */
export function clusterLabelRect(c: { rect: Rect }, width: number): Rect {
  return { x: c.rect.x + CLUSTER_LABEL.dx, y: c.rect.y + CLUSTER_LABEL.dy, w: width, h: CLUSTER_LABEL.size * 1.25 };
}
