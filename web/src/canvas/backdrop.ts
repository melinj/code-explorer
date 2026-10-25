import { Container, Graphics } from "pixi.js";
import type { ViewBootstrap } from "../contracts/http";
import type { Cluster, FolderFrame, Lens } from "../contracts/model";
import { MAX_ARCS, arc, centre, clusterPartners, coStrength, drawPartners, labelPartners, pct, type CoChange } from "./cochange";
import { dashedPolyline } from "./edges";
import { LabelBatch, fit, sansFont, textWidth } from "./labels";
import { CLUSTER_LABEL } from "./glyphSpot";
import { RectBatch, dispose } from "./rects";
import { structure } from "./structure";
import { clusterHeatLevel, depthColor, heatColor, mix, step, type Theme } from "./theme";

/** Folders layout: a directory frame's name (world units), in its title band (cv-layout FOLDER_TITLE). */
const FOLDER_LABEL = 180;
const FOLDER_TITLE = 360;

/**
 * World-space layers drawn once from the T0 bootstrap: lane bands, the district grid, clusters,
 * bundled edges and hubs. Tier changes only toggle visibility.
 */
export class Backdrop {
  readonly back = new Container({ isRenderGroup: true });
  readonly front = new Container({ isRenderGroup: true });
  /**
   * L1+: each cluster's box filled with the lane band beneath it, between the highways and the
   * links: a highway shows only in the gutters, never across another cluster's windows.
   */
  readonly cards = new Container({ isRenderGroup: true });
  private bands = new Graphics();
  /** Folders layout: directory frames over the lane bands. */
  private folderLines = new Graphics();
  /** Depth lens: each frame's title band (and a faint fill) by the share of its links that stay inside. */
  private folderBands = new Graphics();
  private clusterFill = new Graphics();
  private clusterLine = new Graphics();
  private hubs = new Graphics();
  private bigLabels = new Container();
  private smallLabels = new Container();
  /** L0 lens layers: glows and raised shadows beneath the clusters, fills and fades over them, arcs and labels on top. */
  private lensUnder = new Container();
  private lensOver = new Container();
  private lensTop = new Container();
  private lens: Lens = "none";
  private brush?: ReadonlySet<string>;
  /** What the L0 lens layers drew (e2e checks). */
  lensStats = { glows: 0, raised: 0, faded: 0, arcs: 0, partners: 0 };

  constructor() {
    this.back.addChild(this.bands, this.folderBands, this.folderLines);
    this.front.addChild(this.lensUnder, this.clusterFill, this.lensOver, this.clusterLine, this.hubs, this.bigLabels, this.smallLabels, this.lensTop);
  }

  /** Lens and Hotspots brush at L0 (§4.9): the cluster map gets the same emphasis as the windows. */
  setLens(lens: Lens, brush?: ReadonlySet<string>) {
    const depthChanged = (this.lens === "depth") !== (lens === "depth");
    this.lens = lens;
    this.brush = brush;
    this.buildLens();
    if (depthChanged) this.buildDepth();
  }

  /** The structure (structure.ts) arrived or changed: the Depth and Tension lenses redraw. */
  structureChanged() {
    if (this.lens === "tension" || this.lens === "depth") this.buildLens();
    if (this.lens === "depth") this.buildDepth();
  }

  /** A frame's title: under the Depth lens with the share of its links that stay inside it. */
  private frameText(f: FolderFrame) {
    const s = this.lens === "depth" ? structure?.folders.get(f.path) : undefined;
    return s && s.internal + s.outgoing + s.incoming > 0 ? `${f.label}  ·  ${Math.round(s.inside * 100)}% inside · ${s.interface}/${s.windows} exposed` : f.label;
  }

  /**
   * Depth lens: module depth by nesting level. Each directory's title band (and a faint wash over
   * its frame) takes its depth colour (theme.ts depthColor): hue by the share of its links that
   * leave it, saturation by how many links touch it against the busiest directory on the view. A
   * parent that stays cold while its children run hot is where a module closes. Frame titles carry the
   * share inside and how many of its windows are reached from outside.
   */
  private buildDepth() {
    const g = this.folderBands.clear();
    const boot = this.boot;
    const t = this.theme;
    if (boot && t) {
      this.buildSmallLabels(boot, t);
      this.buildGalaxyLabels(this.labelZoom || 0.01);
    }
    if (!boot || !t || this.lens !== "depth" || !structure) return;
    for (const f of boot.folders ?? []) {
      if (this.hiddenLanes?.has(f.lane)) continue;
      const k = structure.pressure(f.path);
      if (k === undefined) continue;
      const c = depthColor(t, k, structure.folderVolume(f.path));
      g.roundRect(f.rect.x, f.rect.y, f.rect.w, f.rect.h, 120).fill({ color: c, alpha: 0.07 });
      g.roundRect(f.rect.x, f.rect.y, f.rect.w, Math.min(FOLDER_TITLE, f.rect.h), 120).fill({ color: c, alpha: 0.85 });
    }
  }

  /** Co-change lens at L0: cluster partners (cochange.ts), and the cluster it is about. */
  private cochange?: CoChange;
  private cochangeFocus?: string;

  setCochange(co?: CoChange) {
    this.cochange = co;
    this.buildLens();
  }

  /** The cluster under the pointer (or selected) in the co-change lens; its partners light up. */
  setCochangeFocus(id?: string) {
    if (id === this.cochangeFocus) return;
    this.cochangeFocus = id;
    if (this.lens === "cochange") this.buildLens();
  }

  /** Zoom the lens layers were sized for (they are sized in screen px, like the galaxy labels). */
  private lensZoom = 0.01;

  /** Re-size the lens layers for the zoom the camera settled at (cheap: one mesh of a few quads per cluster). */
  setLensZoom(zoom: number) {
    this.lensZoom = zoom;
    if (this.lens !== "none" || this.brush) this.buildLens();
  }

  /** Lanes switched off in the legend: their clusters are left off the L0 map too. */
  private hiddenLanes?: ReadonlySet<string>;

  setHiddenLanes(lanes?: ReadonlySet<string>) {
    this.hiddenLanes = lanes;
    if (this.boot && this.theme) this.build(this.boot, this.theme);
  }

  /**
   * L0 lens layers. Each lens decides a fill per cluster that shows what position can't (the grid
   * already places clusters by heat and connectedness):
   * - heat: the continuous heat ramp, with a glow growing over warm and hot clusters;
   * - depends: whether a cluster is depended on (accent) or depends on others (consumer), by the
   *   solid highways in and out of it, stronger with more links;
   * - hotspots: the cluster's share of review-first windows (a double ring, as on the windows);
   * - co-change: how strongly it changes with another cluster, the strongest pairs with no static
   *   link drawn as arcs; pointed at, its partners light up by strength, with arcs and labels;
   * - depth (folder tree ordered by links): its directory's share of links leaving it as the hue,
   *   its link count against the busiest directory as the saturation (theme.ts depthColor);
   * - tension (folders layout): its directory's heat tension, busy files among quiet ones, glowing.
   * What a lens leaves out turns an even quiet grey, whatever its heat.
   */
  private buildLens() {
    const boot = this.boot;
    const t = this.theme;
    if (!boot || !t) return;
    for (const ch of this.lensTop.removeChildren()) dispose(ch);
    const z = this.lensZoom;
    const px = (v: number) => v / z;
    const dark = t.name === "dark";
    const shown = this.shownClusters(boot);
    const under = new RectBatch(shown.length * 10);
    const over = new RectBatch(shown.length * 8);
    const top = new RectBatch(256);
    const st = { glows: 0, raised: 0, faded: 0, arcs: 0, partners: 0 };
    const lens = this.lens;
    const co = lens === "cochange" ? this.cochange : undefined;
    const focus = co ? this.cochangeFocus : undefined;
    const byId = new Map<string, Cluster>(boot.clusters.map((c) => [c.id, c]));
    const partners = focus && co ? clusterPartners(co, focus, byId) : [];
    const partnerK = new Map(partners.map((p) => [p.c.id, p.k]));
    const flow = lens === "connectedness" ? this.flowOf(boot) : undefined;
    const quiet = mix(t.bg, t.ink, dark ? 0.085 : 0.07);
    const fades = (c: Cluster) => (focus ? c.id !== focus && !partnerK.has(c.id) : this.fadesCluster(c));
    for (const c of shown) {
      const r = c.rect;
      if (fades(c)) {
        over.round(r.x, r.y, r.w, r.h, 200, quiet, 1);
        st.faded++;
        continue;
      }
      if (lens === "heat") {
        // A thermal glow under warm and hot clusters, growing with heat: pre-blurred quads.
        const lv = clusterHeatLevel(c);
        if (lv >= 0.6) {
          const k = step((lv - 0.6) / 0.4, 3);
          under.soft(r.x, r.y, r.w, r.h, px(3 + 11 * k) + Math.min(r.w, r.h) * 0.05 * k, heatColor(t, lv), (dark ? 0.22 : 0.16) + (dark ? 0.33 : 0.24) * k, true);
          st.glows++;
        }
      } else if (lens === "connectedness" && flow) {
        // Diverging: depended on (accent) or depending (consumer), stronger with more solid links.
        const f = flow.get(c.id)!;
        const side = f.dir >= 0 ? t.accent : t.consumer;
        const k = step(Math.abs(f.dir) / 2.5);
        const fill = mix(mix(t.bg, t.ink, 0.25), side, 0.25 + 0.75 * k);
        over.round(r.x, r.y, r.w, r.h, 200, mix(t.bg, fill, 0.35 + 0.65 * step(f.mass)), 1);
        if (c.conn >= 2) {
          // Raised by connectedness: central clusters cast longer shadows.
          const o = px([0, 0, 2, 3.5, 5][c.conn]);
          under.soft(r.x + o * 0.4, r.y + o, r.w, r.h, px(3 + c.conn), t.shadow, dark ? 0.8 : 0.4, true);
          st.raised++;
        }
      } else if (lens === "hotspots") {
        // Review first: the double ring windows wear, stronger with the share of such windows.
        const k = c.review !== undefined ? step(c.review / 0.25) : 1;
        over.ring(r.x - px(3), r.y - px(3), r.w + px(6), r.h + px(6), px(1.5 + 1.5 * k), t.accent, 0.5 + 0.5 * k);
        over.ring(r.x - px(7), r.y - px(7), r.w + px(14), r.h + px(14), px(1), t.accent, 0.25 + 0.4 * k);
        st.glows++;
      } else if (lens === "depth") {
        // Hue: its directory's share of links leaving it (with what is below it); saturation: how
        // many links touch it against the busiest directory on the view.
        over.round(r.x, r.y, r.w, r.h, 200, depthColor(t, structure?.pressure(c.folder) ?? 0, structure?.folderVolume(c.folder) ?? 0), 1);
      } else if (lens === "tension") {
        const k = step(structure?.tension(c.folder) ?? 0);
        const hot = heatColor(t, 0.92);
        over.round(r.x, r.y, r.w, r.h, 200, mix(quiet, hot, 0.25 + 0.7 * k), 1);
        if (k > 0.3) {
          under.soft(r.x, r.y, r.w, r.h, px(3 + 11 * k), hot, (dark ? 0.25 : 0.18) + 0.3 * k, true);
          st.glows++;
        }
      } else if (lens === "cochange" && co) {
        if (focus) {
          const pk = partnerK.get(c.id);
          const k = pk === undefined ? undefined : step(pk, 5);
          if (c.id === focus) {
            over.round(r.x, r.y, r.w, r.h, 200, t.couple, 1);
            under.soft(r.x, r.y, r.w, r.h, px(10), t.couple, dark ? 0.7 : 0.5, true);
          } else if (k !== undefined) {
            over.round(r.x, r.y, r.w, r.h, 200, mix(quiet, t.couple, 0.3 + 0.7 * k), 1);
            under.soft(r.x, r.y, r.w, r.h, px(4 + 14 * k), t.couple, (dark ? 0.35 : 0.25) + 0.45 * k, true);
            st.glows++;
          }
        } else {
          const k = step(co.strongest(c.id), 5);
          over.round(r.x, r.y, r.w, r.h, 200, mix(quiet, t.couple, 0.18 + 0.55 * k), 1);
        }
      }
    }
    if (co && focus) {
      const f = byId.get(focus);
      if (f) {
        const near = partners.filter((p) => !this.hiddenLanes?.has(p.c.lane));
        drawPartners(top, t, px(1), f.rect, near);
        st.arcs = Math.min(near.length, MAX_ARCS);
        st.partners = near.length;
        const labels = new LabelBatch(sansFont());
        labelPartners(
          top,
          labels,
          t,
          px(1),
          [{ rect: f.rect, text: f.label, strong: true }, ...near.map((p) => ({ rect: p.rect, text: `${p.c.label}  ${pct(p.p.jaccard)}`, strong: false }))],
          "above",
        );
        const lc = new Container();
        const mc = new Container();
        top.build(mc);
        labels.build(lc);
        this.lensTop.addChild(mc, lc);
      }
    } else if (co) {
      // Nothing pointed at: the strongest pairs no static link explains, as faint arcs.
      const seen = new Set<string>();
      const hidden: Array<{ a: Cluster; b: Cluster; k: number }> = [];
      for (const [a, row] of co.clusters) {
        for (const [b, p] of row) {
          if (p.linked || a > b || seen.has(a + ">" + b)) continue;
          seen.add(a + ">" + b);
          const ca = byId.get(a);
          const cb = byId.get(b);
          if (ca && cb && !this.hiddenLanes?.has(ca.lane) && !this.hiddenLanes?.has(cb.lane)) hidden.push({ a: ca, b: cb, k: coStrength(p, "cluster") });
        }
      }
      hidden.sort((x, y) => y.k - x.k);
      for (const { a, b, k } of hidden.slice(0, 30)) {
        const [ax, ay] = centre(a.rect);
        const [bx, by] = centre(b.rect);
        dashedPolyline(top, arc(ax, ay, bx, by), px(8), px(5), px(1 + 1.5 * k), t.couple, 0.3 + 0.45 * k);
        st.arcs++;
      }
      const mc = new Container();
      top.build(mc);
      this.lensTop.addChild(mc);
    } else top.discard();
    // The pointed-at set brings its own labels; the galaxy's would collide with them.
    this.bigLabels.alpha = focus ? 0.25 : 1;
    under.build(this.lensUnder);
    over.build(this.lensOver);
    this.lensStats = st;
  }

  /**
   * Whether the lens (or the Hotspots brush) leaves cluster `c` out: heat below Mild; Depends, no
   * solid links in or out; Hotspots, no review-first window; co-change, no partner; Flow, no link
   * touching its directory; Tension, none in its directory.
   */
  fadesCluster(c: Cluster): boolean {
    const lens = this.lens;
    if (this.brush && !this.brush.has(c.id)) return true;
    switch (lens) {
      case "heat":
        return clusterHeatLevel(c) < 0.4;
      case "connectedness":
        return !!this.boot && !this.flowOf(this.boot).has(c.id);
      case "hotspots":
        return !(c.review !== undefined ? c.review > 0 : c.heat >= 3 && c.conn >= 3);
      case "cochange":
        return !!this.cochange && this.cochange.strongest(c.id) < 0;
      case "tension":
        return !!structure && !(structure.tension(c.folder) > 0);
      case "depth":
        return !!structure && structure.pressure(c.folder) === undefined;
      default:
        return false;
    }
  }

  private flow?: { boot: ViewBootstrap; map: ReturnType<typeof dependsFlow> };
  private flowOf(boot: ViewBootstrap) {
    if (this.flow?.boot !== boot) this.flow = { boot, map: dependsFlow(boot) };
    return this.flow.map;
  }

  /** Clusters the L0 map draws: range views only those the range touches; never hidden lanes. */
  private shownClusters(boot: ViewBootstrap) {
    const range = boot.view.spec.mode === "range";
    return boot.clusters.filter((c) => (!range || c.windows > 0) && !this.hiddenLanes?.has(c.lane));
  }

  build(boot: ViewBootstrap, t: Theme) {
    const g = this.bands.clear();
    const w = boot.world;
    // Lane bands, then alternate column shading, then row guides.
    boot.lanes.forEach((lane, i) => g.rect(lane.rect.x - 4000, lane.rect.y, lane.rect.w + 8000, lane.rect.h).fill(i % 2 ? t.bandAlt : t.band));
    boot.columns.forEach((c, i) => {
      if (i % 2) g.rect(c.x - 400, w.y, c.w + 800, w.h).fill({ color: t.line, alpha: 0.035 });
    });
    for (const lane of boot.lanes) {
      for (const r of lane.rows.slice(1)) g.moveTo(lane.rect.x - 4000, r.y - 250).lineTo(lane.rect.x + lane.rect.w + 4000, r.y - 250);
    }
    for (const c of boot.columns.slice(1)) g.moveTo(c.x - 800, w.y).lineTo(c.x - 800, w.y + w.h);
    g.stroke({ width: 1, pixelLine: true, color: t.grid, alpha: 0.9 });

    // L0 clusters: filled with heat, raised by connectedness; sized for L0 zoom (~0.01).
    const L0 = 0.01;
    // Range views draw only clusters the range touches; the rest stay as faint outlines (D16).
    const range = boot.view.spec.mode === "range";
    this.boot = boot;
    const shown = this.shownClusters(boot);
    const cf = this.clusterFill.clear();
    for (const c of shown) {
      const o = [0, 1, 2, 3.5, 5][c.conn] / L0;
      if (o) cf.roundRect(c.rect.x + o, c.rect.y + o, c.rect.w, c.rect.h, 200);
    }
    cf.fill({ color: t.shadow, alpha: t.name === "dark" ? 0.55 : 0.2 });
    if (range) {
      for (const c of boot.clusters) if (!c.windows) cf.roundRect(c.rect.x, c.rect.y, c.rect.w, c.rect.h, 200);
      cf.stroke({ width: 1, pixelLine: true, color: t.line, alpha: 0.35 });
    }
    for (const c of shown) {
      cf.roundRect(c.rect.x, c.rect.y, c.rect.w, c.rect.h, 200).fill({ color: heatColor(t, clusterHeatLevel(c)), alpha: 0.95 });
      if (c.conn >= 3) cf.roundRect(c.rect.x, c.rect.y, c.rect.w, c.rect.h, 200).stroke({ width: 1.5, pixelLine: false, color: t.ink, alpha: t.name === "dark" ? 0.75 : 0.55 });
    }
    // L1+: cluster outlines only.
    const cl = this.clusterLine.clear();
    for (const c of boot.clusters) cl.roundRect(c.rect.x - 30, c.rect.y - 30, c.rect.w + 60, c.rect.h + 60, 60);
    // Light hairlines so the cards carry the contrast. (A translucent fill here cost ~3 ms of GPU per frame.)
    cl.stroke({ width: 1, pixelLine: true, color: t.line, alpha: range ? 0.2 : 0.42 });
    // Folders layout: directory frames, the outermost ones a little stronger.
    const fl = this.folderLines.clear();
    const frames = (boot.folders ?? []).filter((f) => !this.hiddenLanes?.has(f.lane));
    for (const [deep, alpha] of [[false, 0.5], [true, 0.3]] as const) {
      for (const f of frames) if (f.depth > 0 === deep) fl.roundRect(f.rect.x, f.rect.y, f.rect.w, f.rect.h, 120);
      fl.stroke({ width: 1, pixelLine: true, color: t.ink, alpha: alpha * (t.name === "dark" ? 0.8 : 1) });
    }
    // Opaque quads in the band's own colour (with the odd-column shading), so the cards read as the
    // background: one mesh, no blending against the bands.
    for (const ch of this.cards.removeChildren()) dispose(ch);
    const cards = new RectBatch(boot.clusters.length * 9);
    const laneOf = new Map(boot.lanes.map((l, i) => [l.lane, i]));
    for (const c of boot.clusters) {
      const cx = c.rect.x + c.rect.w / 2;
      const col = boot.columns.findIndex((k) => cx >= k.x && cx < k.x + k.w);
      const band = (laneOf.get(c.lane) ?? 0) % 2 ? t.bandAlt : t.band;
      cards.round(c.rect.x - 30, c.rect.y - 30, c.rect.w + 60, c.rect.h + 60, 60, col % 2 === 1 ? mix(band, t.line, 0.035) : band);
    }
    cards.build(this.cards);

    const hg = this.hubs.clear();
    for (const h of boot.hubs) {
      if (!h.rect) continue;
      const cx = h.rect.x + h.rect.w / 2;
      const cy = h.rect.y + h.rect.h / 2;
      const r = h.rect.w / 2;
      hg.poly([cx, cy - r, cx + r, cy, cx, cy + r, cx - r, cy], true);
    }
    hg.fill({ color: t.bg }).stroke({ width: 1, pixelLine: true, color: t.edge.publish });

    this.boot = boot;
    this.theme = t;
    this.buildGalaxyLabels(0.01);
    this.buildSmallLabels(boot, t);
    this.buildLens();
    this.buildDepth();
  }

  /** L1+ labels: directory frames, clusters, hubs. */
  private buildSmallLabels(boot: ViewBootstrap, t: Theme) {
    const font = sansFont();
    const small = new LabelBatch(font);
    const ink = this.lens === "depth" && structure ? t.ink : mix(t.ink, t.bg, 0.2);
    for (const f of boot.folders ?? []) {
      if (!this.hiddenLanes?.has(f.lane)) small.add(fit(font, this.frameText(f), FOLDER_LABEL, f.rect.w - 240), f.rect.x + 120, f.rect.y + 100, FOLDER_LABEL, ink);
    }
    for (const c of boot.clusters) {
      const L = CLUSTER_LABEL;
      small.add(fit(font, c.label, L.size, c.rect.w - L.inset), c.rect.x + L.dx, c.rect.y + L.dy, L.size, t.ink);
    }
    for (const h of boot.hubs) {
      if (h.rect) small.add(h.display, h.rect.x + h.rect.w + 40, h.rect.y + h.rect.h / 2 - 50, 80, t.edge.publish);
    }
    small.build(this.smallLabels);
  }

  private boot?: ViewBootstrap;
  private theme?: Theme;
  /** Zoom the galaxy labels were laid out for. */
  labelZoom = 0;

  /**
   * L0 cluster labels at a constant ~13 px on screen for `zoom`: the biggest clusters first, and a
   * label only where it is not far wider than its cluster and overlaps no other label. Rebuilt when the camera
   * settles at a different galaxy zoom (one mesh, a few milliseconds).
   */
  buildGalaxyLabels(zoom: number) {
    const boot = this.boot;
    const t = this.theme;
    if (!boot || !t) return;
    this.labelZoom = zoom;
    const font = sansFont();
    const size = 13 / zoom;
    const big = new LabelBatch(font);
    const placed: Array<[number, number, number, number]> = [];
    // Folders layout: the outer directories' names first, top-left in their frames, where they fit.
    const frames = (boot.folders ?? []).filter((f) => f.depth <= 1 && !this.hiddenLanes?.has(f.lane));
    for (const f of frames.sort((a, b) => a.depth - b.depth || b.rect.w * b.rect.h - a.rect.w * a.rect.h)) {
      const s = size * (f.depth ? 1 : 1.15);
      const text = this.frameText(f);
      const w = textWidth(font, text, s);
      if (w + s > f.rect.w || s * 1.6 > f.rect.h) continue;
      const r: [number, number, number, number] = [f.rect.x, f.rect.y, w + s, s * 1.5];
      if (placed.some((p) => r[0] < p[0] + p[2] && p[0] < r[0] + r[2] && r[1] < p[1] + p[3] && p[1] < r[1] + r[3])) continue;
      placed.push(r);
      big.add(text, f.rect.x + s * 0.5, f.rect.y + s * 0.3, s, mix(t.ink, t.bg, 0.15));
    }
    const order = this.shownClusters(boot).sort((a, b) => b.windows - a.windows);
    for (const c of order) {
      const w = textWidth(font, c.label, size);
      if (w > c.rect.w * 4) continue;
      const x = c.rect.x + c.rect.w / 2 - w / 2;
      const y = c.rect.y + c.rect.h / 2 - size / 2;
      const r: [number, number, number, number] = [x - size * 0.3, y - size * 0.2, w + size * 0.6, size * 1.4];
      if (placed.some((p) => r[0] < p[0] + p[2] && p[0] < r[0] + r[2] && r[1] < p[1] + p[3] && p[1] < r[1] + r[3])) continue;
      placed.push(r);
      // Dark theme: white labels over a soft drop shadow, so they read on the brightest heat too.
      if (t.name === "dark") big.add(c.label, c.rect.x + c.rect.w / 2 + size * 0.06, y + size * 0.08, size, t.bg, "center");
      big.add(c.label, c.rect.x + c.rect.w / 2, y, size, t.name === "dark" ? 0xffffff : t.ink, "center");
    }
    big.build(this.bigLabels);
  }

  /** Show every layer at once (for a warm-up render that compiles shaders and uploads buffers). */
  showAll() {
    for (const c of this.front.children) c.visible = true;
    this.cards.visible = true;
  }

  setTier(tier: number) {
    const l0 = tier === 0;
    this.clusterFill.visible = l0;
    this.lensUnder.visible = this.lensOver.visible = this.lensTop.visible = l0;
    this.bigLabels.visible = l0;
    this.clusterLine.visible = !l0;
    this.cards.visible = !l0;
    this.smallLabels.visible = !l0;
  }
}

/**
 * The Depends lens at L0: per cluster, the solid links into and out of it (its highways; links from
 * the tests lane are left out, since a test depending on code says nothing of the code's role) as
 * `dir` = log2((in + 1) / (out + 1)) (positive: depended on more than depending) and `mass`, 0..1
 * by its total against the busiest cluster's (log scale). Clusters with no such links are absent.
 */
export function dependsFlow(boot: Pick<ViewBootstrap, "clusters" | "bundles">) {
  const lane = new Map<string, string>(boot.clusters.map((c) => [c.id, c.lane]));
  const io = new Map<string, [number, number]>();
  const at = (id: string) => {
    let v = io.get(id);
    if (!v) io.set(id, (v = [0, 0]));
    return v;
  };
  for (const b of boot.bundles) {
    if (lane.get(b.from) === "test") continue;
    at(b.to)[0] += b.weight;
    at(b.from)[1] += b.weight;
  }
  let max = 1;
  for (const [i, o] of io.values()) max = Math.max(max, i + o);
  const out = new Map<string, { dir: number; mass: number }>();
  for (const [id, [i, o]] of io) out.set(id, { dir: Math.log2((i + 1) / (o + 1)), mass: Math.log2(1 + i + o) / Math.log2(1 + max) });
  return out;
}
