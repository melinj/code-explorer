import { Container, Graphics, Sprite, Text, Texture, TilingSprite } from "pixi.js";
import type { ViewBootstrap } from "../contracts/http";
import type { LaneDef, Rect } from "../contracts/model";
import type { Camera } from "./camera";
import { clusterHeatLevel, heatColor, hex, type Theme } from "./theme";

export const RAIL_PX = 132;
export const COLHEAD_PX = 40;
const CONN_LABELS = ["Isolated", "Low", "Medium", "High", "Core"];

function patternTexture(kind: LaneDef["texture"], t: Theme): Texture | undefined {
  if (kind === "plain") return undefined;
  const size = { hatch: 8, dots: 10, lines: 6, cross: 18 }[kind];
  const c = document.createElement("canvas");
  c.width = c.height = size * 2;
  const g = c.getContext("2d")!;
  g.scale(2, 2);
  g.strokeStyle = g.fillStyle = hex(t.line);
  g.lineWidth = 1;
  if (kind === "hatch") {
    g.beginPath();
    g.moveTo(0, size);
    g.lineTo(size, 0);
    g.moveTo(-1, 1);
    g.lineTo(1, -1);
    g.moveTo(size - 1, size + 1);
    g.lineTo(size + 1, size - 1);
    g.stroke();
  } else if (kind === "dots") {
    g.beginPath();
    g.arc(3, 3, 1.2, 0, Math.PI * 2);
    g.fill();
  } else if (kind === "lines") {
    g.fillRect(0, 3, size, 1);
  } else {
    g.fillRect(6, 8.5, 6, 1);
    g.fillRect(8.5, 6, 1, 6);
  }
  return Texture.from({ resource: c, resolution: 2 });
}

/** A solid rectangle that moves and resizes without rebuilding anything (a tinted white texel). */
function bar(color: number, alpha = 1) {
  const s = new Sprite(Texture.WHITE);
  s.tint = color;
  s.alpha = alpha;
  return s;
}

function place(s: Sprite, x: number, y: number, w: number, h: number) {
  s.position.set(x, y);
  s.width = Math.max(0, w);
  s.height = Math.max(0, h);
}

const label = (size: number, weight: "400" | "500" | "600", fill: number, letterSpacing = 0) =>
  new Text({
    text: "",
    style: { fontFamily: "Inter, system-ui, sans-serif", fontSize: size, fontWeight: weight, fill, letterSpacing },
    resolution: Math.max(2, devicePixelRatio),
  });

/**
 * Screen-space parts that follow the camera every frame: lane textures (constant pixel size),
 * the sticky lane rail with row labels, the sticky heat-column header and the minimap.
 * Each frame only repositions a few dozen objects.
 */
export class Hud {
  readonly under = new Container();
  readonly over = new Container();
  private textures: Array<TilingSprite | undefined> = [];
  /**
   * Rail and column header: sprites positioned every frame. (Graphics cleared and redrawn per
   * camera frame tessellated anew each time: most of the garbage of a long pan.)
   */
  private rail = new Container();
  private colHead = new Container();
  private railBg = bar(0);
  private railEdge = bar(0);
  private railSeps: Sprite[] = [];
  private colBg = bar(0);
  private colEdge = bar(0);
  private colCorner = bar(0);
  private colDots: Graphics[] = [];
  private colSeps: Sprite[] = [];
  private viewFill = bar(0);
  private viewEdges = [bar(0), bar(0), bar(0), bar(0)];
  private laneLabels: Text[][] = [];
  /** Per heat column: full label ("Hot   89 clusters") and short label ("Hot"). */
  private colLabels: Array<[Text, Text]> = [];
  private mini = new Container();
  private miniBase = new Graphics();
  private miniView = new Container();
  /** Fragments of the selected file, marked on the minimap. */
  private miniMarks = new Graphics();
  /** What the minimap marks now: how many fragments, in which colour (e2e checks). */
  fragmentMarks = { n: 0, color: 0 };
  private miniScale = 1;
  /** Range views: clusters lit on the minimap (for tests and the status line). */
  litClusters = 0;
  private boot!: ViewBootstrap;

  constructor() {
    this.mini.addChild(this.miniBase, this.miniMarks, this.miniView);
    this.miniView.addChild(this.viewFill, ...this.viewEdges);
    this.over.addChild(this.colHead, this.rail, this.mini);
  }

  build(boot: ViewBootstrap, t: Theme) {
    this.boot = boot;
    for (const ch of this.under.removeChildren()) ch.destroy();
    const defs = new Map(boot.laneDefs.map((d) => [d.id, d]));
    this.textures = boot.lanes.map((lane) => {
      const tex = patternTexture(defs.get(lane.lane)?.texture ?? "plain", t);
      if (!tex) return undefined;
      const s = new TilingSprite({ texture: tex, width: 1, height: 1 });
      s.alpha = 0.3;
      this.under.addChild(s);
      return s;
    });
    for (const l of this.laneLabels.flat()) l.destroy();
    this.laneLabels = boot.lanes.map((lane) => {
      // Lane names as small caps headings, the file count beneath in the secondary ink.
      const name = label(11, "600", t.ink, 0.9);
      name.text = (defs.get(lane.lane)?.label ?? lane.lane).toUpperCase();
      const files = label(11.5, "400", t.dim);
      files.text = `${lane.files.toLocaleString()} files`;
      const rows = lane.rows.map((r) => {
        const l = label(11, "500", t.dim);
        l.text = CONN_LABELS[r.conn];
        l.anchor.set(1, 0.5);
        return l;
      });
      for (const l of [name, files, ...rows]) this.over.addChild(l);
      return [name, files, ...rows];
    });
    // Rail and header pieces (coloured for this theme; update() only moves them).
    for (const ch of this.rail.removeChildren()) if (!(ch === this.railBg || ch === this.railEdge)) ch.destroy();
    for (const ch of this.colHead.removeChildren()) if (!(ch === this.colBg || ch === this.colEdge || ch === this.colCorner)) ch.destroy();
    this.railBg.tint = t.rail;
    this.railEdge.tint = t.border;
    this.rail.addChild(this.railBg);
    this.railSeps = boot.lanes.map(() => this.rail.addChild(bar(t.border)));
    this.rail.addChild(this.railEdge);
    this.colBg.tint = t.rail;
    this.colEdge.tint = t.border;
    this.colCorner.tint = t.rail;
    this.colHead.addChild(this.colBg);
    this.colSeps = boot.columns.map(() => this.colHead.addChild(bar(t.border)));
    this.colDots = boot.columns.map((c) => this.colHead.addChild(new Graphics().roundRect(0, 0, 10, 10, 5).fill(t.heat[c.heat])));
    this.colHead.addChild(this.colEdge, this.colCorner);
    this.viewFill.tint = t.accent;
    this.viewFill.alpha = 0.12;
    for (const e of this.viewEdges) e.tint = t.accent;
    for (const l of this.colLabels.flat()) l.destroy();
    this.colLabels = boot.columns.map((c) => {
      const full = label(12, "600", t.ink);
      full.text = `${boot.heatLabels[c.heat]}  ·  ${c.clusters} clusters`;
      const short = label(12, "600", t.ink);
      short.text = boot.heatLabels[c.heat];
      this.over.addChild(full, short);
      return [full, short];
    });

    // Minimap: lanes and clusters, drawn once at minimap scale.
    const W = 200;
    const H = 150;
    this.miniScale = Math.min(W / boot.world.w, H / boot.world.h);
    const s = this.miniScale;
    const g = this.miniBase.clear();
    const mw = boot.world.w * s + 12;
    const mh = boot.world.h * s + 12;
    // A floating card: a few stacked translucent rounds stand in for a blurred shadow (drawn once).
    const dark = t.name === "dark";
    for (const [grow, dy, a] of [[6, 4, 0.05], [3, 2, 0.07], [1, 1, 0.08]] as const) {
      g.roundRect(-6 - grow, -6 - grow + dy, mw + 2 * grow, mh + 2 * grow, 10 + grow).fill({ color: t.shadow, alpha: dark ? a * 3 : a });
    }
    g.roundRect(-6, -6, mw, mh, 10).fill({ color: t.surface, alpha: 0.96 }).stroke({ width: 1, color: t.border, alpha: 1 });
    boot.lanes.forEach((l, i) => g.rect(0, l.rect.y * s, boot.world.w * s, l.rect.h * s).fill(i % 2 ? t.bandAlt : t.band));
    // Range views (D16): the whole grid stays for orientation, faint; the clusters the range changes
    // are lit in their heat colour with a ring, so even a 1 px cluster is findable.
    const range = boot.view.spec.mode === "range";
    for (const c of boot.clusters) {
      if (range && c.windows > 0) continue;
      g.rect(c.rect.x * s, c.rect.y * s, Math.max(1, c.rect.w * s), Math.max(1, c.rect.h * s)).fill({ color: range ? t.line : heatColor(t, clusterHeatLevel(c)), alpha: range ? 0.22 : 1 });
    }
    if (range) {
      for (const c of boot.clusters) {
        if (!c.windows) continue;
        g.rect(c.rect.x * s, c.rect.y * s, Math.max(1.5, c.rect.w * s), Math.max(1.5, c.rect.h * s)).fill(heatColor(t, clusterHeatLevel(c)));
      }
      for (const c of boot.clusters) if (c.windows) g.circle((c.rect.x + c.rect.w / 2) * s, (c.rect.y + c.rect.h / 2) * s, Math.max(3.5, (Math.max(c.rect.w, c.rect.h) * s) / 2 + 2));
      g.stroke({ width: 1.5, color: t.accent, alpha: 0.95 });
    }
    this.litClusters = range ? boot.clusters.filter((c) => c.windows > 0).length : 0;
  }

  update(cam: Camera) {
    const b = this.boot;
    const z = cam.zoom;
    const W = cam.width;
    const H = cam.height;
    const ox = -cam.x * z;
    const oy = -cam.y * z;

    // Lane textures: clipped to each lane's screen band, anchored to the world origin.
    b.lanes.forEach((lane, i) => {
      const s = this.textures[i];
      if (!s) return;
      const y0 = Math.max(COLHEAD_PX, (lane.rect.y - cam.y) * z);
      const y1 = Math.min(H, (lane.rect.y + lane.rect.h - cam.y) * z);
      const x0 = Math.max(RAIL_PX, (lane.rect.x - cam.x) * z);
      const x1 = Math.min(W, (lane.rect.x + lane.rect.w - cam.x) * z);
      s.visible = y1 > y0 && x1 > x0;
      if (!s.visible) return;
      s.position.set(x0, y0);
      s.width = x1 - x0;
      s.height = y1 - y0;
      s.tilePosition.set(ox - x0, oy - y0);
    });

    // Sticky lane rail.
    place(this.railBg, 0, COLHEAD_PX, RAIL_PX, H - COLHEAD_PX);
    b.lanes.forEach((lane, i) => {
      const [name, files, ...rows] = this.laneLabels[i];
      const top = (lane.rect.y - cam.y) * z;
      const bot = (lane.rect.y + lane.rect.h - cam.y) * z;
      const vis = bot > COLHEAD_PX && top < H;
      name.visible = files.visible = vis;
      const sep = this.railSeps[i];
      sep.visible = vis;
      if (vis) {
        place(sep, 0, top - 0.5, RAIL_PX, 1);
        // The lane name sticks to the top of the lane's visible part.
        const ly = Math.min(Math.max(top + 8, COLHEAD_PX + 8), bot - 40);
        name.position.set(12, ly + 2);
        files.position.set(12, ly + 18);
      }
      lane.rows.forEach((row, j) => {
        const l = rows[j];
        const cy = (row.y + row.h / 2 - cam.y) * z;
        const hpx = row.h * z;
        l.visible = vis && hpx > 16 && cy > COLHEAD_PX + 8 && cy < H - 8;
        if (l.visible) l.position.set(RAIL_PX - 8, Math.max(cy, COLHEAD_PX + 50));
      });
    });
    place(this.railEdge, RAIL_PX - 0.5, COLHEAD_PX, 1, H - COLHEAD_PX);

    // Sticky heat-column header.
    place(this.colBg, 0, 0, W, COLHEAD_PX);
    b.columns.forEach((c, i) => {
      const x0 = (c.x - cam.x) * z;
      const x1 = (c.x + c.w - cam.x) * z;
      const [full, short] = this.colLabels[i];
      const dot = this.colDots[i];
      const sep = this.colSeps[i];
      // The visible width of the column decides how much of its label fits.
      const vis = Math.min(x1, W) - Math.max(x0, RAIL_PX);
      full.visible = short.visible = false;
      dot.visible = sep.visible = false;
      if (x1 <= RAIL_PX || x0 >= W || vis < 22) return;
      const l = vis > full.width + 40 ? full : vis > short.width + 40 ? short : undefined;
      const lx = Math.min(Math.max(x0 + 8, RAIL_PX + 8), x1 - (l ? l.width + 30 : 20));
      dot.visible = true;
      dot.position.set(lx, 15);
      if (l) {
        l.visible = true;
        l.position.set(lx + 16, 12);
      }
      if (i > 0) {
        sep.visible = true;
        place(sep, x0 - 800 * z - 0.5, 6, 1, COLHEAD_PX - 12);
      }
    });
    place(this.colEdge, 0, COLHEAD_PX - 0.5, W, 1);
    place(this.colCorner, 0, 0, RAIL_PX, COLHEAD_PX);

    // Minimap, bottom-left, with the viewport rectangle.
    const s = this.miniScale;
    this.mini.position.set(RAIL_PX + 16, H - b.world.h * s - 16);
    const v = cam.view();
    const vx = Math.max(-4, v.x * s);
    const vy = Math.max(-4, v.y * s);
    const vw = Math.min(v.w * s, b.world.w * s + 8 - vx);
    const vh = Math.min(v.h * s, b.world.h * s + 8 - vy);
    const [top, right, bottom, left] = this.viewEdges;
    place(this.viewFill, vx, vy, vw, vh);
    place(top, vx - 0.75, vy - 0.75, vw + 1.5, 1.5);
    place(bottom, vx - 0.75, vy + vh - 0.75, vw + 1.5, 1.5);
    place(left, vx - 0.75, vy - 0.75, 1.5, vh + 1.5);
    place(right, vx + vw - 0.75, vy - 0.75, 1.5, vh + 1.5);
  }

  /** Mark world rects (a split file's fragments) on the minimap; none clears. */
  markFragments(rects: Rect[], color: number) {
    this.fragmentMarks = { n: rects.length, color };
    const s = this.miniScale;
    const g = this.miniMarks.clear();
    for (const r of rects) g.circle((r.x + r.w / 2) * s, (r.y + r.h / 2) * s, 3.5);
    if (rects.length) g.fill({ color }).stroke({ width: 1, color: 0x000000, alpha: 0.6 });
  }

  /** Screen rectangle of the minimap, including its frame. */
  miniRect() {
    const w = this.boot.world;
    const s = this.miniScale;
    return { x: this.mini.x - 6, y: this.mini.y - 6, w: w.w * s + 12, h: w.h * s + 12 };
  }

  /** World point for a click inside the minimap, or undefined. */
  miniHit(sx: number, sy: number): [number, number] | undefined {
    const s = this.miniScale;
    const x = (sx - this.mini.x) / s;
    const y = (sy - this.mini.y) / s;
    const w = this.boot.world;
    return x >= 0 && y >= 0 && x <= w.w && y <= w.h ? [x, y] : undefined;
  }
}
