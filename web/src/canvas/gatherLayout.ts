import type { Rect } from "../contracts/model";

/** Pure layout of the co-change gather (gather.ts): where the tray and its cards go. */

/** Timings (ms), sizes (screen px) and caps of the gather. */
export const GATHER = {
  dwellMs: 600,
  cueMs: 200,
  stillPx: 4,
  leaveMs: 250,
  flyMs: 420,
  staggerMs: 18,
  releaseMs: 260,
  cap: 12,
  maxLines: 4,
  cardW: 210,
  gap: 12,
  pad: 14,
  head: 30,
  focusGap: 28,
} as const;

export interface TrayLayout {
  side: "right" | "left" | "below" | "above";
  tray: Rect;
  slots: Rect[];
}

/**
 * Where the tray goes (screen px): beside the focus on the first side of right, left, below and
 * above where it fits the view, trying narrower (beside) or wider (above and below) arrangements
 * before the next side; if none fits, the placement covering least of the focus, clamped inside the
 * view. Slots fill from the focus side outward, strongest first: column by column beside it, row by
 * row above or below it.
 */
export function layoutTray(focus: Rect, n: number, slot: { w: number; h: number }, view: Rect): TrayLayout {
  const { gap, pad, head, focusGap } = GATHER;
  const dims = (cols: number) => {
    const rows = Math.ceil(n / cols);
    return { cols, rows, w: cols * slot.w + (cols - 1) * gap + 2 * pad, h: head + rows * slot.h + (rows - 1) * gap + 2 * pad };
  };
  const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(Math.max(lo, hi), v));
  const fx = focus.x + focus.w / 2;
  const fy = focus.y + focus.h / 2;
  const [x0, y0, x1, y1] = [view.x, view.y, view.x + view.w, view.y + view.h];
  const beside = n <= 4 ? 1 : n <= 8 ? 2 : 3;
  const flat = Math.min(n, 4);
  type Try = { side: TrayLayout["side"]; d: ReturnType<typeof dims>; x: number; y: number; fits: boolean };
  const tries: Try[] = [];
  for (const side of ["right", "left", "below", "above"] as const) {
    const colsList = side === "right" || side === "left" ? range(beside, 1) : range(flat, Math.min(n, 6));
    for (const cols of colsList) {
      const d = dims(cols);
      let x: number;
      let y: number;
      let fits: boolean;
      if (side === "right" || side === "left") {
        x = side === "right" ? focus.x + focus.w + focusGap : focus.x - focusGap - d.w;
        y = clamp(fy - d.h / 2, y0, y1 - d.h);
        fits = x >= x0 && x + d.w <= x1 && d.h <= y1 - y0;
      } else {
        y = side === "below" ? focus.y + focus.h + focusGap : focus.y - focusGap - d.h;
        x = clamp(fx - d.w / 2, x0, x1 - d.w);
        fits = y >= y0 && y + d.h <= y1 && d.w <= x1 - x0;
      }
      tries.push({ side, d, x, y, fits });
    }
  }
  let pick = tries.find((t) => t.fits);
  if (!pick) {
    // Nothing fits: clamp each into the view and keep the one hiding least of the focus.
    const overlap = (t: Try) => {
      const x = clamp(t.x, x0, x1 - t.d.w);
      const y = clamp(t.y, y0, y1 - t.d.h);
      const w = Math.max(0, Math.min(x + t.d.w, focus.x + focus.w) - Math.max(x, focus.x));
      const h = Math.max(0, Math.min(y + t.d.h, focus.y + focus.h) - Math.max(y, focus.y));
      return { t: { ...t, x, y }, area: w * h };
    };
    const inView = tries.filter((t) => t.d.w <= x1 - x0 && t.d.h <= y1 - y0);
    pick = (inView.length ? inView : tries).map(overlap).reduce((a, b) => (b.area < a.area ? b : a)).t;
  }
  const { side: s, d, x: tx, y: ty } = pick;
  const slots: Rect[] = [];
  for (let i = 0; i < n; i++) {
    // Beside the focus: columns, nearest first; above or below it: rows, nearest first.
    let c = s === "right" || s === "left" ? Math.floor(i / d.rows) : i % d.cols;
    let r = s === "right" || s === "left" ? i % d.rows : Math.floor(i / d.cols);
    if (s === "left") c = d.cols - 1 - c;
    if (s === "above") r = d.rows - 1 - r;
    slots.push({ x: tx + pad + c * (slot.w + gap), y: ty + head + pad + r * (slot.h + gap), w: slot.w, h: slot.h });
  }
  return { side: s, tray: { x: tx, y: ty, w: d.w, h: d.h }, slots };
}

/** from, from ± 1, …, to (either direction). */
function range(from: number, to: number) {
  const out: number[] = [];
  for (let v = from; from <= to ? v <= to : v >= to; v += from <= to ? 1 : -1) out.push(v);
  return out;
}

/** Where a card starts (world): its origin when on screen, else a card-sized box at the view's edge toward it. */
export function startRect(from: Rect, slot: Rect, view: Rect): Rect {
  const cx = from.x + from.w / 2;
  const cy = from.y + from.h / 2;
  const onScreen = cx >= view.x && cx <= view.x + view.w && cy >= view.y && cy <= view.y + view.h;
  if (onScreen && from.w < view.w * 1.5 && from.h < view.h * 1.5) return from;
  const x = Math.max(view.x, Math.min(view.x + view.w, cx));
  const y = Math.max(view.y, Math.min(view.y + view.h, cy));
  return { x: x - slot.w / 2, y: y - slot.h / 2, w: slot.w, h: slot.h };
}
