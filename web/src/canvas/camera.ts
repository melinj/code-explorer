import type { Rect, ZoomTier } from "../contracts/model";
import { LINE_WU } from "../contracts/wire";

export const MIN_ZOOM = 0.002;
export const MAX_ZOOM = 4;

/** Tier boundaries on zoom (DESIGN §5): on-screen line height h = 18·zoom px. */
const TIER_EDGES: Array<[ZoomTier, number]> = [
  ["L1", 0.03],
  ["L2", 0.09],
  ["L3", 0.5],
];
const HYSTERESIS = 0.1;

/**
 * Screen = (world - pos) · zoom. The camera only holds numbers; the engine applies them to one
 * world transform per frame, so panning and zooming never touch the scene graph.
 */
export class Camera {
  x = 0;
  y = 0;
  zoom = 0.01;
  width = 1;
  height = 1;
  tier: ZoomTier = "L0";
  /** Bumped on every change; the engine compares it to decide whether to render. */
  version = 0;

  private anim?: { from: [number, number, number]; to: [number, number, number]; t0: number; ms: number; arc: number };
  private vel: [number, number] = [0, 0];
  lastMotion = 0;

  resize(w: number, h: number) {
    this.width = w;
    this.height = h;
    this.touch();
  }

  touch() {
    this.version++;
    this.lastMotion = performance.now();
    this.updateTier();
  }

  get lineHeightPx() {
    return LINE_WU * this.zoom;
  }

  /** Visible world rectangle. */
  view(): Rect {
    return { x: this.x, y: this.y, w: this.width / this.zoom, h: this.height / this.zoom };
  }

  toWorld(sx: number, sy: number): [number, number] {
    return [this.x + sx / this.zoom, this.y + sy / this.zoom];
  }

  toScreen(wx: number, wy: number): [number, number] {
    return [(wx - this.x) * this.zoom, (wy - this.y) * this.zoom];
  }

  panBy(dxScreen: number, dyScreen: number) {
    this.anim = undefined;
    this.x -= dxScreen / this.zoom;
    this.y -= dyScreen / this.zoom;
    this.touch();
  }

  /** Zoom by `factor` keeping the world point under (sx, sy) fixed. */
  zoomAt(sx: number, sy: number, factor: number) {
    this.anim = undefined;
    const z = clamp(this.zoom * factor, MIN_ZOOM, MAX_ZOOM);
    const [wx, wy] = this.toWorld(sx, sy);
    this.zoom = z;
    this.x = wx - sx / z;
    this.y = wy - sy / z;
    this.touch();
  }

  setVelocity(vx: number, vy: number) {
    this.vel = [vx, vy];
  }

  /** Inertia velocity in screen px per ms. */
  velocityPx(): [number, number] {
    return [this.vel[0] * 1000, this.vel[1] * 1000];
  }

  stop() {
    this.vel = [0, 0];
    this.anim = undefined;
  }

  /** Animate to frame `r` (world) with `pad` screen px of margin. */
  flyTo(r: Rect, ms = 450, pad = 40) {
    const z = clamp(Math.min((this.width - 2 * pad) / r.w, (this.height - 2 * pad) / r.h), MIN_ZOOM, MAX_ZOOM);
    const x = r.x + r.w / 2 - this.width / 2 / z;
    const y = r.y + r.h / 2 - this.height / 2 / z;
    this.animateTo(x, y, z, ms);
  }

  /**
   * Fly to a far target: zoom out mid-flight so both ends are briefly in view, then in. The default
   * 1.5 s gives the eye time to follow where the camera is going.
   */
  flyAlong(x: number, y: number, zoom: number, ms = 1500) {
    const z0 = this.zoom;
    const cx0 = this.x + this.width / 2 / z0;
    const cy0 = this.y + this.height / 2 / z0;
    const cx1 = x + this.width / 2 / zoom;
    const cy1 = y + this.height / 2 / zoom;
    // Distance in screens at the smaller zoom; the arc zooms out by about that factor.
    const screens = (Math.hypot(cx1 - cx0, cy1 - cy0) * Math.min(z0, zoom)) / Math.min(this.width, this.height);
    this.animateTo(x, y, zoom, ms, Math.log(1 + Math.max(0, screens - 0.5)));
  }

  animateTo(x: number, y: number, zoom: number, ms: number, arc = 0) {
    this.vel = [0, 0];
    if (ms <= 0) {
      // A jump also cancels any flight in progress, or that flight would carry on from here.
      this.anim = undefined;
      this.x = x;
      this.y = y;
      this.zoom = clamp(zoom, MIN_ZOOM, MAX_ZOOM);
      this.touch();
      return;
    }
    this.anim = { from: [this.x, this.y, this.zoom], to: [x, y, zoom], t0: performance.now(), ms, arc };
  }

  get moving() {
    return !!this.anim || Math.hypot(this.vel[0], this.vel[1]) > 0.01;
  }

  /** Advance animation and inertia; returns true if the camera changed. */
  step(now: number, dt: number): boolean {
    if (this.anim) {
      const a = this.anim;
      const t = Math.min(1, (now - a.t0) / a.ms);
      // Arcs ease in and out (the camera visibly sets off); plain moves ease out.
      const e = a.arc ? t * t * (3 - 2 * t) : 1 - Math.pow(1 - t, 3);
      // Interpolate zoom in log space (dipping out by `arc` mid-flight) and keep the screen centre
      // on a straight world path.
      // Never below MIN_ZOOM: a deeper dip would have the data layer walk a vast off-world area.
      const z = clamp(Math.exp(Math.log(a.from[2]) + (Math.log(a.to[2]) - Math.log(a.from[2])) * e - a.arc * 4 * e * (1 - e)), MIN_ZOOM, MAX_ZOOM);
      const cx0 = a.from[0] + this.width / 2 / a.from[2];
      const cy0 = a.from[1] + this.height / 2 / a.from[2];
      const cx1 = a.to[0] + this.width / 2 / a.to[2];
      const cy1 = a.to[1] + this.height / 2 / a.to[2];
      const cx = cx0 + (cx1 - cx0) * e;
      const cy = cy0 + (cy1 - cy0) * e;
      this.zoom = z;
      this.x = cx - this.width / 2 / z;
      this.y = cy - this.height / 2 / z;
      if (t >= 1) this.anim = undefined;
      this.touch();
      return true;
    }
    if (this.vel[0] || this.vel[1]) {
      this.x -= (this.vel[0] * dt) / this.zoom;
      this.y -= (this.vel[1] * dt) / this.zoom;
      const decay = Math.pow(0.004, dt / 1000);
      this.vel = [this.vel[0] * decay, this.vel[1] * decay];
      if (Math.hypot(this.vel[0], this.vel[1]) < 0.02) this.vel = [0, 0];
      this.touch();
      return true;
    }
    return false;
  }

  private updateTier() {
    // Hysteresis: leaving a tier needs the zoom to pass its edge by 10%.
    const z = this.zoom;
    let tier: ZoomTier = "L0";
    for (const [t, edge] of TIER_EDGES) {
      const current = tierIndex(this.tier) >= tierIndex(t);
      if (z >= edge * (current ? 1 - HYSTERESIS : 1 + HYSTERESIS)) tier = t;
    }
    this.tier = tier;
  }
}

export function tierIndex(t: ZoomTier) {
  return t === "L0" ? 0 : t === "L1" ? 1 : t === "L2" ? 2 : 3;
}

export function clamp(v: number, lo: number, hi: number) {
  return v < lo ? lo : v > hi ? hi : v;
}
