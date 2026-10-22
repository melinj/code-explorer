import { Container } from "pixi.js";
import type { ConflictDetail, Thread, WindowHeader } from "../contracts/model";
import { HEADER_WU, LINE_WU } from "../contracts/wire";
import type { ReviewClient } from "../data/review";
import { LabelBatch, sansFont } from "./labels";
import { RectBatch, dispose } from "./rects";
import type { Theme } from "./theme";

export interface ConflictHit {
  x: number;
  y: number;
  w: number;
  h: number;
  conflict: ConflictDetail;
  window: WindowHeader;
}

export interface ThreadHit {
  x: number;
  y: number;
  w: number;
  h: number;
  thread: Thread;
  window: WindowHeader;
}

/**
 * Review threads on the canvas: a marker in the window's gutter at the anchored line (pink for
 * yours, accent for the agent's, dim once resolved) plus a bar spanning the anchored range. Few
 * enough to rebuild whole whenever threads, loaded windows or the tier change.
 */
export class ThreadMarks {
  readonly root = new Container();
  hits: ThreadHit[] = [];
  conflictHits: ConflictHit[] = [];

  build(review: ReviewClient, windowsByPath: Map<string, WindowHeader[]>, theme: Theme, wuPerPx: number) {
    for (const ch of this.root.removeChildren()) dispose(ch);
    this.hits = [];
    this.conflictHits = [];
    const rb = new RectBatch(256);
    const labels = new LabelBatch(sansFont());
    // Readable from outline zoom: at least ~12 px, never smaller than a code line.
    const size = Math.max(LINE_WU, 12 * wuPerPx);
    for (const [path, threads] of review.byPath) {
      const ws = windowsByPath.get(path);
      if (!ws) continue;
      const perLine = new Map<string, Thread[]>();
      for (const t of threads) {
        const w = ws.find((w) => t.anchor.start >= w.lines.start && t.anchor.start <= w.lines.end);
        if (!w) continue;
        const k = w.id + ":" + t.anchor.start;
        const l = perLine.get(k);
        if (l) l.push(t);
        else perLine.set(k, [t]);
      }
      for (const [k, list] of perLine) {
        const w = ws.find((x) => x.id === k.slice(0, k.lastIndexOf(":")))!;
        const t = list.find((x) => x.status !== "resolved" && x.status !== "wontfix") ?? list[0];
        const done = t.status === "resolved" || t.status === "wontfix";
        const agent = t.createdBy.kind === "agent" || t.assignee === "agent";
        const color = done ? theme.dim : agent ? theme.accent : theme.thread;
        const y0 = w.rect.y + HEADER_WU + (t.anchor.start - w.lines.start) * LINE_WU;
        const span = (Math.min(t.anchor.end, w.lines.end) - t.anchor.start + 1) * LINE_WU;
        // Range bar along the gutter edge, then the marker itself.
        rb.add(w.rect.x + 2, y0, Math.max(3, 2 * wuPerPx), span, color, done ? 0.4 : 0.9);
        const x = w.rect.x + 6;
        const y = y0 + LINE_WU / 2 - size / 2;
        rb.add(x, y, size * 1.25, size, color, done ? 0.5 : 1);
        if (list.length > 1 || !done) labels.add(String(list.length), x + size * 0.35, y + size * 0.08, size * 0.8, theme.bg);
        this.hits.push({ x, y, w: size * 1.25, h: size, thread: t, window: w });
      }
    }
    // Conflicted regions: a red band over the lines and a marker in the gutter.
    for (const [path, d] of review.conflicts) {
      const ws = windowsByPath.get(path);
      if (!ws) continue;
      for (const r of d.regions) {
        const w = ws.find((w) => r.start >= w.lines.start && r.start <= w.lines.end);
        if (!w) continue;
        const y0 = w.rect.y + HEADER_WU + (r.start - w.lines.start) * LINE_WU;
        const span = (Math.min(r.end, w.lines.end) - r.start + 1) * LINE_WU;
        rb.add(w.rect.x + 2, y0, w.rect.w - 4, span, theme.conflict, 0.16);
        rb.add(w.rect.x + 2, y0, Math.max(4, 3 * wuPerPx), span, theme.conflict, 0.95);
        const x = w.rect.x + w.rect.w - size * 4.2;
        const y = y0 - size * 1.1;
        rb.add(x, y, size * 4, size, theme.conflict, 1);
        labels.add("conflict", x + size * 0.3, y + size * 0.08, size * 0.75, theme.bg);
        this.conflictHits.push({ x, y, w: size * 4, h: size, conflict: d, window: w });
      }
    }
    const a = new Container();
    rb.build(a);
    const b = new Container();
    labels.build(b);
    this.root.addChild(a, b);
  }
}
