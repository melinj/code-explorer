import { FILE_CHIP_SLOTS, TokenClass } from "../contracts/wire";

/**
 * The one palette, for the canvas and (as CSS variables, see `applyCssVars`) for the DOM chrome.
 * Hue is reserved for heat; lanes differ by texture, files by chip.
 */
export interface Theme {
  name: "dark" | "light";
  bg: number;
  band: number;
  bandAlt: number;
  rail: number;
  grid: number;
  line: number;
  dim: number;
  ink: number;
  body: number;
  header: number;
  shadow: number;
  accent: number;
  /** One colour per heat grade (legend, minimap, column dots): the ramp at each grade's middle. */
  heat: [number, number, number, number, number];
  /** The continuous heat ramp, frozen → hot, in HEAT_STEPS steps (see `heatColor`). */
  heatRamp: number[];
  edge: Record<string, number>;
  token: Record<number, number>;
  conflict: number;
  thread: number;
  /** The co-change lens: files and clusters that change together (apart from accent and heat). */
  couple: number;
  /** The Depends lens: code that depends on others more than others depend on it (accent is the reverse). */
  consumer: number;
  /** Positive (added lines, live) and warning colours, shared by canvas marks and the DOM. */
  add: number;
  warn: number;
  /** DOM chrome surfaces: panels, raised controls, hairlines. */
  surface: number;
  surface2: number;
  border: number;
}

/**
 * Heat is one continuous ramp (a sliding scale, not five unrelated hues): quiet slate for frozen
 * code, through a neutral middle, to warm amber and a muted coral for the hottest. Lightness and
 * chroma both rise with heat, so hot code stands out while the cold majority recedes. Stops are
 * [position, OKLCH L, C, h], interpolated in OKLab.
 */
type Stop = [number, number, number, number];
const HEAT_DARK: Stop[] = [
  [0, 0.47, 0.028, 255],
  [0.25, 0.555, 0.06, 235],
  [0.5, 0.655, 0.06, 88],
  [0.74, 0.71, 0.11, 62],
  [1, 0.7, 0.155, 33],
];
const HEAT_LIGHT: Stop[] = [
  [0, 0.84, 0.02, 255],
  [0.25, 0.77, 0.05, 235],
  [0.5, 0.78, 0.065, 85],
  [0.74, 0.73, 0.11, 60],
  [1, 0.63, 0.16, 32],
];
/** Colours on the ramp: few enough that every tint of them fits the rect palette atlas. */
export const HEAT_STEPS = 24;

function heatRamp(stops: Stop[]): number[] {
  const lab = stops.map(([, L, C, h]) => [L, C * Math.cos((h * Math.PI) / 180), C * Math.sin((h * Math.PI) / 180)]);
  return Array.from({ length: HEAT_STEPS }, (_, i) => {
    const t = i / (HEAT_STEPS - 1);
    let k = 0;
    while (k < stops.length - 2 && t > stops[k + 1][0]) k++;
    const u = (t - stops[k][0]) / (stops[k + 1][0] - stops[k][0]);
    const [a, b] = [lab[k], lab[k + 1]];
    return oklab(a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u, a[2] + (b[2] - a[2]) * u);
  });
}

function gradeColors(ramp: number[]): Theme["heat"] {
  return [0, 1, 2, 3, 4].map((g) => ramp[Math.round(((g + 0.5) / 5) * (HEAT_STEPS - 1))]) as Theme["heat"];
}

/** The ramp colour at `level` (0..1, see `heatLevel`). */
export function heatColor(t: Theme, level: number) {
  const i = Math.round(level * (HEAT_STEPS - 1));
  return t.heatRamp[i < 0 ? 0 : i >= HEAT_STEPS ? HEAT_STEPS - 1 : i];
}

/**
 * A window's place on the heat ramp (0..1): its grade's fifth of the scale, and inside it where its
 * recency-weighted rate sits between the grade's band thresholds (log scale; `churn` is the view's
 * ChurnSummary). Without the bands (older servers) a window sits mid-grade.
 */
export function heatLevel(grade: number, rate: number, churn?: { bands: readonly number[]; topRate?: number }): number {
  const bands = churn?.bands;
  if (!bands || bands.length < 4) return (grade + 0.5) / 5;
  if (grade <= 0) return rate > 0 && bands[0] > 0 ? (clamp01(Math.log((rate * 16) / bands[0]) / Math.log(16)) * 0.999) / 5 : 0;
  const lo = bands[grade - 1];
  // Hot has no upper threshold: it runs to the hottest file (often 20× its floor).
  const hi = grade < 4 ? bands[grade] : Math.max(churn?.topRate ?? 0, bands[3] * 4);
  const f = lo > 0 && hi > lo && rate > 0 ? Math.log(rate / lo) / Math.log(hi / lo) : 0.5;
  return (grade + clamp01(f) * 0.999) / 5;
}

/**
 * `k` (0..1) rounded to `n` steps. Every colour a build mixes from a continuous value must go
 * through this (or the heat ramp's steps): each distinct colour × alpha takes a cell of the rect
 * palette atlas (rects.ts), cells are never freed, and each new one re-uploads the atlas.
 */
export function step(k: number, n = 4) {
  return Math.round((k < 0 ? 0 : k > 1 ? 1 : k) * n) / n;
}

/** A cluster's place on the heat ramp: the server's level, else the middle of its grade. */
export function clusterHeatLevel(c: { heat: number; heatLevel?: number }) {
  return c.heatLevel ?? (c.heat + 0.5) / 5;
}

function clamp01(v: number) {
  return v < 0 ? 0 : v > 1 ? 1 : v;
}

const RAMP_DARK = heatRamp(HEAT_DARK);
const RAMP_LIGHT = heatRamp(HEAT_LIGHT);

export const DARK: Theme = {
  name: "dark",
  bg: 0x0e1218,
  band: 0x11161e,
  bandAlt: 0x141a23,
  rail: 0x10151c,
  grid: 0x222b38,
  line: 0x8c9bb2,
  dim: 0x7f8ba0,
  ink: 0xe7ecf3,
  body: 0x171d27,
  header: 0x1e2531,
  shadow: 0x000000,
  accent: 0x7d95ff,
  heat: gradeColors(RAMP_DARK),
  heatRamp: RAMP_DARK,
  edge: {
    call: 0xc3ccd9,
    inherit: 0xb39cf0,
    implement: 0xb39cf0,
    publish: 0x4cc6aa,
    subscribe: 0x4cc6aa,
    dispatch: 0x4cc6aa,
    "http-request": 0xe7b05c,
    "http-route": 0xe7b05c,
    read: 0x8a97ab,
    define: 0x8a97ab,
    config: 0x8a97ab,
    deploy: 0x7f9cc4,
  },
  token: {
    [TokenClass.Keyword]: 0xd193ec,
    [TokenClass.String]: 0x8bd49c,
    [TokenClass.Comment]: 0x667389,
    [TokenClass.Number]: 0xeab26a,
    [TokenClass.Function]: 0x8aa2ff,
    [TokenClass.Type]: 0x6cc0cf,
    [TokenClass.Decorator]: 0xe79a62,
    [TokenClass.Property]: 0xb8c4d6,
  },
  conflict: 0xf0505f,
  thread: 0xec6cc0,
  couple: 0xc77dff,
  consumer: 0x4fbfae,
  add: 0x45c08a,
  warn: 0xe8b84a,
  surface: 0x141920,
  surface2: 0x1b212b,
  border: 0x29313d,
};

export const LIGHT: Theme = {
  ...DARK,
  name: "light",
  bg: 0xf3f4f7,
  band: 0xf0f2f5,
  bandAlt: 0xebeef3,
  rail: 0xfafbfc,
  grid: 0xdbe0e8,
  line: 0x5d6b80,
  dim: 0x6a7488,
  ink: 0x1b2230,
  body: 0xffffff,
  header: 0xf5f6f9,
  shadow: 0x1c2638,
  accent: 0x4361ee,
  heat: gradeColors(RAMP_LIGHT),
  heatRamp: RAMP_LIGHT,
  edge: {
    ...DARK.edge,
    call: 0x566274,
    inherit: 0x8360cf,
    implement: 0x8360cf,
    publish: 0x14917a,
    subscribe: 0x14917a,
    dispatch: 0x14917a,
    "http-request": 0xbf7a12,
    "http-route": 0xbf7a12,
    read: 0x8892a3,
    define: 0x8892a3,
    config: 0x8892a3,
    deploy: 0x4f6b94,
  },
  token: {
    [TokenClass.Keyword]: 0x8a2c9e,
    [TokenClass.String]: 0x1a7d43,
    [TokenClass.Comment]: 0x8e98a8,
    [TokenClass.Number]: 0x9a5b00,
    [TokenClass.Function]: 0x3550d4,
    [TokenClass.Type]: 0x0f7a72,
    [TokenClass.Decorator]: 0xa2471c,
    [TokenClass.Property]: 0x4a5a70,
  },
  conflict: 0xe03e4e,
  thread: 0xd4459f,
  couple: 0x8f3fd0,
  consumer: 0x13887a,
  add: 0x1f9d63,
  warn: 0xb8860b,
  surface: 0xffffff,
  surface2: 0xf1f3f6,
  border: 0xdfe3ea,
};

/** Mirror the palette into CSS custom properties, so the DOM chrome and the canvas never drift. */
export function applyCssVars(t: Theme, el: HTMLElement = document.documentElement) {
  const v: Record<string, number> = {
    bg: t.bg,
    surface: t.surface,
    "surface-2": t.surface2,
    ink: t.ink,
    "ink-2": t.dim,
    line: t.border,
    accent: t.accent,
    thread: t.thread,
    bad: t.conflict,
    add: t.add,
    warn: t.warn,
    couple: t.couple,
    "accent-ink": t.name === "dark" ? t.bg : 0xffffff,
  };
  for (const [k, c] of Object.entries(v)) el.style.setProperty(`--${k}`, hex(c));
}

export function hex(c: number) {
  return "#" + c.toString(16).padStart(6, "0");
}

export function mix(a: number, b: number, t: number) {
  // Inline channel math: no closures, since builds call this for every card.
  const ar = (a >> 16) & 255, ag = (a >> 8) & 255, ab = a & 255;
  const r = Math.round(ar + (((b >> 16) & 255) - ar) * t);
  const g = Math.round(ag + (((b >> 8) & 255) - ag) * t);
  const bl = Math.round(ab + ((b & 255) - ab) * t);
  return (r << 16) | (g << 8) | bl;
}

/** OKLCH → sRGB, for the file-chip palette (wire.ts FILE_CHIP_SLOTS: hue = slot · 36°). */
export function oklch(L: number, C: number, hDeg: number) {
  const h = (hDeg * Math.PI) / 180;
  return oklab(L, C * Math.cos(h), C * Math.sin(h));
}

/** OKLab → sRGB (clipped to the gamut). */
export function oklab(L: number, a: number, b: number) {
  const l_ = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m_ = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s_ = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
  const lin = [
    4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_,
    -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_,
    -0.0041960863 * l_ - 0.7034186147 * m_ + 1.707614701 * s_,
  ];
  const enc = (x: number) => {
    const v = x <= 0.0031308 ? 12.92 * x : 1.055 * Math.pow(x, 1 / 2.4) - 0.055;
    return Math.round(Math.min(1, Math.max(0, v)) * 255);
  };
  return (enc(lin[0]) << 16) | (enc(lin[1]) << 8) | enc(lin[2]);
}

/**
 * Depth lens: hue from the share of links leaving (cold blue → teal → yellow → orange → red when
 * every link leaves), saturation from how many links there are against the busiest on the view
 * (`volume`, 0..1; never below DEPTH_MIN_SAT). Lightness stays even so the hue reads alone.
 * Quantised (DEPTH_HUE_STEPS × DEPTH_SAT_STEPS colours), as the rect palette atlas is bounded.
 */
const DEPTH_HUES = [255, 195, 95, 55, 25];
export const DEPTH_MIN_SAT = 0.2;
const DEPTH_HUE_STEPS = 13;
const DEPTH_SAT_STEPS = 6;
const depthCache = new Map<string, number>();
export function depthColor(t: Theme, outside: number, volume: number) {
  const o = Math.round(clamp01(outside) * (DEPTH_HUE_STEPS - 1)) / (DEPTH_HUE_STEPS - 1);
  const v = Math.round(clamp01(volume) * (DEPTH_SAT_STEPS - 1)) / (DEPTH_SAT_STEPS - 1);
  const key = `${t.name}|${o}|${v}`;
  let c = depthCache.get(key);
  if (c === undefined) {
    const x = o * (DEPTH_HUES.length - 1);
    const i = Math.min(DEPTH_HUES.length - 2, Math.floor(x));
    const hue = DEPTH_HUES[i] + (DEPTH_HUES[i + 1] - DEPTH_HUES[i]) * (x - i);
    const sat = DEPTH_MIN_SAT + (1 - DEPTH_MIN_SAT) * v;
    c = oklch(t.name === "dark" ? 0.68 : 0.74, 0.16 * sat, hue);
    depthCache.set(key, c);
  }
  return c;
}

/** Owner stripes of range views (in-range blame): eight hues at even lightness, apart from heat's ramp. */
export const OWNER = [250, 160, 300, 200, 330, 120, 270, 185].map((h) => oklch(0.7, 0.13, h));

export const CHIP_REST = Array.from({ length: FILE_CHIP_SLOTS }, (_, i) => oklch(0.78, 0.06, i * 36));
export const CHIP_HI = Array.from({ length: FILE_CHIP_SLOTS }, (_, i) => oklch(0.88, 0.17, i * 36));
