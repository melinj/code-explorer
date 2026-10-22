import { Container } from "pixi.js";
import type { Rect } from "../contracts/model";
import { TokenClass } from "../contracts/wire";
import type { CoPartner } from "./cochange";
import { pct } from "./cochange";
import { dashedPolyline } from "./edges";
import { LabelBatch, fit, fitStart, monoFont, sansFont } from "./labels";
import { RectBatch, TL, TR, dispose } from "./rects";
import { mix, step, type Theme } from "./theme";
import { GATHER, layoutTray, startRect } from "./gatherLayout";

export { GATHER, layoutTray, startRect };

/**
 * Co-change "gather": the partners of the file or cluster in focus fly into a tray beside it, so the
 * set that changes together can be looked at together. Nothing on the map moves or is rebuilt:
 * the tray holds lightweight proxy cards in one render group above everything, the originals'
 * spots are covered and outlined ("ghosts", with a thread to their card), and a scrim steps the
 * rest of the map back. Per frame only container transforms change while the cards fly.
 */

/** One partner in the tray. */
export interface GatherItem {
  /** "c:<cluster id>" or "f:<path>". */
  key: string;
  label: string;
  detail: string;
  p: CoPartner;
  /** Strength 0..1 (cochange.ts coStrength). */
  k: number;
  /** Where the original lives: a cluster box, or the file's loaded windows (may be empty). */
  origins: Rect[];
  /** Where its card flies from: the first origin, else the cluster the file lives in. */
  from: Rect;
  color: number;
  /** Symbols of a file's windows, shown on its card. */
  lines: string[];
}

const easeOut = (u: number) => 1 - (1 - u) ** 3;
const smooth = (u: number) => u * u * (3 - 2 * u);
const clamp01 = (u: number) => (u < 0 ? 0 : u > 1 ? 1 : u);

interface Slot {
  item: GatherItem;
  rect: Rect;
  start: Rect;
  c: Container;
  text: Container;
  delay: number;
}

export interface GatherStart {
  key: string;
  title: string;
  focus: Rect;
  items: GatherItem[];
  /** Partners past the cap (named in the tray header). */
  more: number;
  theme: Theme;
  /** World units per screen px, the view (world) and the camera's world → screen. */
  px: number;
  view: Rect;
  toScreen: (x: number, y: number) => [number, number];
  /** Screen area the tray may use (inside the rail and column header). */
  area: Rect;
  pinned: boolean;
}

export class Gather {
  readonly root = new Container({ isRenderGroup: true });
  private under = new Container();
  private cards = new Container();
  private hotLayer = new Container();
  state: "idle" | "in" | "held" | "out" = "idle";
  key?: string;
  pinned = false;
  private slots: Slot[] = [];
  private t0 = 0;
  private px = 1;
  private theme?: Theme;
  /** Focus and tray (world), grown: leaving it releases an unpinned gather. */
  region?: Rect;
  private focus?: Rect;
  private tray?: Rect;
  hot = -1;
  /** What the last gather drew (e2e checks and the bench). */
  stats = { started: 0, shown: 0, more: 0, covers: 0, buildMs: 0, side: "" };

  constructor() {
    this.root.addChild(this.under, this.cards, this.hotLayer);
    this.root.visible = false;
  }

  get active() {
    return this.state !== "idle";
  }

  /** The zoom the tray was laid out for. */
  get zoom() {
    return 1 / this.px;
  }

  start(now: number, o: GatherStart) {
    const t0 = performance.now();
    this.clear();
    const t = o.theme;
    const px = o.px;
    this.theme = t;
    this.px = px;
    this.key = o.key;
    this.pinned = o.pinned;
    this.t0 = now;
    this.hot = -1;
    const items = o.items.slice(0, GATHER.cap);
    const lines = Math.min(GATHER.maxLines, Math.max(0, ...items.map((i) => i.lines.length)));
    const slotPx = { w: GATHER.cardW, h: cardHeight(lines) };
    const [fx0, fy0] = o.toScreen(o.focus.x, o.focus.y);
    const [fx1, fy1] = o.toScreen(o.focus.x + o.focus.w, o.focus.y + o.focus.h);
    const lay = layoutTray({ x: fx0, y: fy0, w: fx1 - fx0, h: fy1 - fy0 }, items.length, slotPx, o.area);
    // Screen px → world, anchored where the camera is now: the tray pans and zooms with the map.
    const v = o.view;
    const toWorld = (r: Rect): Rect => {
      const [ax, ay] = [v.x + r.x * px, v.y + r.y * px];
      return { x: ax, y: ay, w: r.w * px, h: r.h * px };
    };
    this.tray = toWorld(lay.tray);
    this.focus = o.focus;
    const grow = (r: Rect, g: number): Rect => ({ x: r.x - g, y: r.y - g, w: r.w + 2 * g, h: r.h + 2 * g });
    this.region = bounds([grow(this.tray, 24 * px), grow(o.focus, 24 * px)]);

    const rb = new RectBatch(512);
    // Scrim over the map around the focus (a hole leaves it lit), well past the view so a pan
    // keeps it covered.
    const dark = t.name === "dark";
    const s = grow(v, Math.max(v.w, v.h) * 2);
    const h = grow(o.focus, 6 * px);
    const scrim = dark ? 0.66 : 0.58;
    rb.add(s.x, s.y, s.w, h.y - s.y, t.bg, scrim);
    rb.add(s.x, h.y + h.h, s.w, s.y + s.h - h.y - h.h, t.bg, scrim);
    rb.add(s.x, h.y, h.x - s.x, h.h, t.bg, scrim);
    rb.add(h.x + h.w, h.y, s.x + s.w - h.x - h.w, h.h, t.bg, scrim);
    // Ghosts: each original's spot covered and outlined, with a thread to its card.
    let covers = 0;
    const cover = mix(t.bg, t.band, 0.6);
    this.slots = items.map((item, i) => {
      const rect = toWorld(lay.slots[i]);
      for (const r of item.origins) {
        rb.round(r.x - 4 * px, r.y - 4 * px, r.w + 8 * px, r.h + 8 * px, 6 * px, cover);
        covers++;
      }
      return { item, rect, start: startRect(item.from, rect, v), c: new Container(), text: new Container(), delay: i * GATHER.staggerMs };
    });
    for (const sl of this.slots) {
      const k = sl.item.k;
      for (const r of sl.item.origins) ghost(rb, r, px, t, 0.4 + 0.4 * step(k));
      const [ax, ay] = centre(sl.item.from);
      const [bx, by] = centre(sl.rect);
      rb.polyline(new Float32Array([ax, ay, bx, by]), (1 + k) * px, t.couple, 0.18 + 0.4 * step(k));
    }
    // The tray: a raised plate with a header naming the focus.
    const tr = this.tray;
    rb.soft(tr.x, tr.y + 4 * px, tr.w, tr.h, 18 * px, t.shadow, dark ? 0.7 : 0.25);
    rb.round(tr.x, tr.y, tr.w, tr.h, 12 * px, mix(t.surface2, t.couple, 0.25));
    rb.round(tr.x + px, tr.y + px, tr.w - 2 * px, tr.h - 2 * px, 11 * px, t.surface);
    const plate = new Container();
    rb.build(plate);
    const head = new LabelBatch(sansFont());
    const more = o.more ? `  +${o.more} more` : "";
    head.add(fit(sansFont(), `Changes with ${o.title}${more}`, 12.5 * px, tr.w - 2 * GATHER.pad * px), tr.x + GATHER.pad * px, tr.y + 10 * px, 12.5 * px, t.ink);
    const hc = new Container();
    head.build(hc);
    this.under.addChild(plate, hc);
    for (const sl of this.slots) {
      buildCard(sl, px, t);
      this.cards.addChild(sl.c);
    }
    this.place(now);
    this.state = "in";
    this.root.visible = true;
    this.stats = { started: this.stats.started + 1, shown: items.length, more: o.more, covers, buildMs: performance.now() - t0, side: lay.side };
  }

  /** Advance the animation; true while something moved (the engine renders). */
  tick(now: number): boolean {
    if (this.state === "in" || this.state === "out") {
      const done = this.place(now);
      if (done) {
        if (this.state === "in") this.state = "held";
        else this.clear();
      }
      return true;
    }
    return false;
  }

  /** Fly the cards back to their spots (or drop everything at once). */
  release(now: number, instant = false) {
    if (this.state === "idle") return;
    this.hot = -1;
    for (const ch of this.hotLayer.removeChildren()) dispose(ch);
    if (instant) return this.clear();
    this.state = "out";
    this.t0 = now;
  }

  /** What a world point is over: a card (its index), the tray, the focus, or nothing. */
  hit(wx: number, wy: number): number | "tray" | "focus" | undefined {
    if (!this.active || this.state === "out") return undefined;
    const inside = (r: Rect) => wx >= r.x && wx <= r.x + r.w && wy >= r.y && wy <= r.y + r.h;
    const i = this.slots.findIndex((s) => inside(s.rect));
    if (i >= 0) return i;
    if (this.tray && inside(this.tray)) return "tray";
    if (this.focus && inside(this.focus)) return "focus";
    return undefined;
  }

  inRegion(wx: number, wy: number) {
    const r = this.region;
    return !!r && wx >= r.x && wx <= r.x + r.w && wy >= r.y && wy <= r.y + r.h;
  }

  item(i: number): GatherItem | undefined {
    return this.slots[i]?.item;
  }

  /** Where each card ended up (world), for tests. */
  slotRects() {
    return this.slots.map((s) => ({ key: s.item.key, ...s.rect }));
  }

  /** Light one card, its ghost and its thread (-1: none). */
  setHot(i: number) {
    if (i === this.hot || !this.theme) return;
    this.hot = i;
    for (const ch of this.hotLayer.removeChildren()) dispose(ch);
    const sl = this.slots[i];
    if (!sl) return;
    const t = this.theme;
    const px = this.px;
    const rb = new RectBatch(64);
    const r = sl.rect;
    rb.ring(r.x - 3 * px, r.y - 3 * px, r.w + 6 * px, r.h + 6 * px, 2 * px, t.couple, 1);
    for (const o of sl.item.origins) rb.ring(o.x - 6 * px, o.y - 6 * px, o.w + 12 * px, o.h + 12 * px, 2.5 * px, t.couple, 1);
    const [ax, ay] = centre(sl.item.from);
    const [bx, by] = centre(r);
    dashedPolyline(rb, new Float32Array([ax, ay, bx, by]), 8 * px, 5 * px, 2 * px, t.couple, 0.9);
    const c = new Container();
    rb.build(c);
    this.hotLayer.addChild(c);
  }

  /** Card transforms for `now`; true once the flight (or the release) is over. */
  private place(now: number): boolean {
    const out = this.state === "out";
    let done = true;
    for (const sl of this.slots) {
      const u = out ? clamp01((now - this.t0) / GATHER.releaseMs) : clamp01((now - this.t0 - sl.delay) / GATHER.flyMs);
      if (u < 1) done = false;
      const e = out ? 1 - smooth(u) : easeOut(u);
      const s = sl.start;
      const r = sl.rect;
      sl.c.position.set(s.x + (r.x - s.x) * e, s.y + (r.y - s.y) * e);
      sl.c.scale.set(s.w / r.w + (1 - s.w / r.w) * e, s.h / r.h + (1 - s.h / r.h) * e);
      // Captions fade in at the end of the flight and go first on release.
      const tail = GATHER.flyMs - 120;
      sl.text.alpha = out ? 0 : clamp01((now - this.t0 - sl.delay - tail) / 120);
    }
    this.under.alpha = out ? 1 - clamp01((now - this.t0) / GATHER.releaseMs) : clamp01((now - this.t0) / 250);
    return done;
  }

  private clear() {
    for (const c of [this.under, this.cards, this.hotLayer]) for (const ch of c.removeChildren()) dispose(ch);
    this.slots = [];
    this.state = "idle";
    this.key = undefined;
    this.region = this.tray = this.focus = undefined;
    this.root.visible = false;
    this.hot = -1;
  }
}

/** A card at the origin (0, 0) of its container, sized for its slot: heat cap, name, where, how strongly. */
function buildCard(sl: Slot, px: number, t: Theme) {
  const { item, rect } = sl;
  const w = rect.w;
  const h = rect.h;
  const dark = t.name === "dark";
  const rb = new RectBatch(48);
  rb.soft(0, 2 * px, w, h, 10 * px, t.shadow, dark ? 0.6 : 0.2);
  rb.round(0, 0, w, h, 8 * px, mix(t.line, t.couple, 0.35 + 0.65 * step(item.k)));
  rb.round(1.5 * px, 1.5 * px, w - 3 * px, h - 3 * px, 6.5 * px, t.body);
  rb.round(1.5 * px, 1.5 * px, w - 3 * px, 5 * px, 6.5 * px, item.color, 1, TL | TR);
  // How surely the two change together, as a bar along the bottom.
  rb.round(10 * px, h - 8 * px, w - 20 * px, 3 * px, 1.5 * px, mix(t.body, t.line, 0.4));
  rb.round(10 * px, h - 8 * px, Math.max(6 * px, (w - 20 * px) * Math.max(0.05, item.k)), 3 * px, 1.5 * px, t.couple);
  const mesh = new Container();
  rb.build(mesh);
  const sans = sansFont();
  const labels = new LabelBatch(sans);
  const inner = w - 20 * px;
  labels.add(fit(sans, item.label, 13 * px, inner), 10 * px, 12 * px, 13 * px, t.ink);
  labels.add(fitStart(sans, item.detail, 10.5 * px, inner), 10 * px, 31 * px, 10.5 * px, t.dim);
  const union = item.p.jaccard > 0 ? Math.round(item.p.support / item.p.jaccard) : item.p.support;
  const how = `${item.p.support} of ${union} commits · ${pct(item.p.jaccard)}${item.p.linked ? "" : " · no static link"}`;
  labels.add(fit(sans, how, 10.5 * px, inner), 10 * px, 46 * px, 10.5 * px, t.couple);
  const text = new Container();
  labels.build(text);
  if (item.lines.length) {
    const mono = monoFont();
    const code = new LabelBatch(mono);
    item.lines.slice(0, GATHER.maxLines).forEach((l, i) => code.add(fit(mono, l, 10.5 * px, inner), 10 * px, (64 + 15 * i) * px, 10.5 * px, t.token[TokenClass.Function] ?? t.ink));
    const cc = new Container();
    code.build(cc);
    text.addChild(cc);
  }
  sl.c.addChild(mesh, text);
  sl.text = text;
  sl.c.position.set(rect.x, rect.y);
}

/** Card height (screen px): name, where, how strongly; then up to GATHER.maxLines symbols; then the strength bar. */
function cardHeight(lines: number) {
  return (lines ? 62 + 15 * lines : 58) + 14;
}

/** A dashed outline around an emptied spot. */
function ghost(rb: RectBatch, r: Rect, px: number, t: Theme, alpha: number) {
  const g = 4 * px;
  const pts = new Float32Array([r.x - g, r.y - g, r.x + r.w + g, r.y - g, r.x + r.w + g, r.y + r.h + g, r.x - g, r.y + r.h + g, r.x - g, r.y - g]);
  dashedPolyline(rb, pts, 6 * px, 4 * px, 1.5 * px, t.couple, alpha);
}

function centre(r: Rect): [number, number] {
  return [r.x + r.w / 2, r.y + r.h / 2];
}

function bounds(rs: Rect[]): Rect {
  const x0 = Math.min(...rs.map((r) => r.x));
  const y0 = Math.min(...rs.map((r) => r.y));
  const x1 = Math.max(...rs.map((r) => r.x + r.w));
  const y1 = Math.max(...rs.map((r) => r.y + r.h));
  return { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
}
