import type { Engine } from "./engine";

/**
 * Scripted camera runs for performance checks. Each scenario drives the camera from the engine's
 * frame hook (so it measures exactly what a user sees) and records every frame interval plus long
 * tasks. Run from the console or the bench script: `await __cv.bench("pan-l1")`.
 */
export interface BenchResult {
  scenario: string;
  frames: number;
  seconds: number;
  fps: number;
  p50: number;
  p95: number;
  p99: number;
  max: number;
  over20ms: number;
  over33ms: number;
  /** Main-thread work per frame (ms): what is left of the budget even when frames hold 60 fps. */
  workMean: number;
  workP50: number;
  workP95: number;
  workP99: number;
  longTasks: number;
  longTaskMs: number;
  windowsLoaded: number;
  requests: number;
}

type Step = (t: number, e: Engine) => void; // t in 0..1

function panAt(zoom: number, dxScreensPerSec: number, dyScreensPerSec: number): (e: Engine) => Step {
  return (e) => {
    const cam = e.cam;
    const cx = cam.x + cam.width / 2 / cam.zoom;
    const cy = cam.y + cam.height / 2 / cam.zoom;
    cam.animateTo(cx - cam.width / 2 / zoom, cy - cam.height / 2 / zoom, zoom, 0);
    const [x0, y0] = [cam.x, cam.y];
    return (t) => {
      // Back and forth, so the run covers new ground and then returns over cached ground.
      const s = Math.sin(t * Math.PI * 2) * 4;
      cam.animateTo(x0 + (s * dxScreensPerSec * cam.width) / zoom, y0 + (s * dyScreensPerSec * cam.height) / zoom, zoom, 0);
    };
  };
}

function zoomSweep(zMin: number, zMax: number, cycles: number): (e: Engine) => Step {
  return (e) => {
    const cam = e.cam;
    const cx = cam.x + cam.width / 2 / cam.zoom;
    const cy = cam.y + cam.height / 2 / cam.zoom;
    return (t) => {
      const u = (1 - Math.cos(t * Math.PI * 2 * cycles)) / 2;
      const z = Math.exp(Math.log(zMax) + (Math.log(zMin) - Math.log(zMax)) * u);
      cam.animateTo(cx - cam.width / 2 / z, cy - cam.height / 2 / z, z, 0);
    };
  };
}

/** Continuous pan in one direction: every frame reaches ground that was never loaded or built. */
function fling(zoom: number, screensPerSec: number, ms: number): (e: Engine) => Step {
  return (e) => {
    const cam = e.cam;
    // Sweep back and forth across the code lane (the densest part of the map), drifting down it.
    const lane = e.client.boot.lanes.find((l) => l.lane === "code")!.rect;
    const dist = ((screensPerSec * ms) / 1000) * (cam.width / zoom);
    return (t) => {
      const s = (dist * t) / lane.w;
      const tri = s % 2 < 1 ? s % 2 : 2 - (s % 2);
      const cx = lane.x + lane.w * (0.05 + 0.9 * tri);
      const cy = lane.y + lane.h * (0.1 + 0.8 * t);
      cam.animateTo(cx - cam.width / 2 / zoom, cy - cam.height / 2 / zoom, zoom, 0);
    };
  };
}

/** Zoom from L0 into code at several spots and back out, as when exploring the map. */
function dives(n: number): (e: Engine) => Step {
  return (e) => {
    const cam = e.cam;
    const b = e.client.boot;
    const code = b.clusters.filter((c) => c.lane === "code");
    const targets = Array.from({ length: n }, (_, i) => code[Math.floor(((i + 0.5) / n) * code.length)].rect);
    return (t) => {
      const k = Math.min(n - 1, Math.floor(t * n));
      const u = t * n - k;
      // In over 70% of the segment, out over 30%.
      const d = u < 0.7 ? u / 0.7 : 1 - (u - 0.7) / 0.3;
      const e1 = (1 - Math.cos(d * Math.PI)) / 2;
      const z = Math.exp(Math.log(0.006) + (Math.log(0.9) - Math.log(0.006)) * e1);
      const r = targets[k];
      const cx = r.x + 400;
      const cy = r.y + 600;
      cam.animateTo(cx - cam.width / 2 / z, cy - cam.height / 2 / z, z, 0);
    };
  };
}

export const SCENARIOS: Record<string, { ms: number; setup: (e: Engine) => Step }> = {
  "pan-l1": { ms: 6000, setup: panAt(0.05, 0.25, 0.08) },
  "pan-l2": { ms: 6000, setup: panAt(0.2, 0.25, 0.1) },
  "pan-l3": { ms: 6000, setup: panAt(0.8, 0.25, 0.15) },
  "zoom-l0-l3": { ms: 8000, setup: zoomSweep(0.006, 1.0, 2) },
  "zoom-l1-l2": { ms: 6000, setup: zoomSweep(0.03, 0.3, 3) },
  "fling-l1": { ms: 6000, setup: fling(0.04, 2, 6000) },
  "fling-l2": { ms: 6000, setup: fling(0.15, 2, 6000) },
  "fling-l3": { ms: 6000, setup: fling(0.7, 1.5, 6000) },
  dives: { ms: 12000, setup: dives(4) },
};

export async function bench(engine: Engine, name: string): Promise<BenchResult> {
  const sc = SCENARIOS[name];
  if (!sc) throw new Error(`unknown scenario ${name}; have ${Object.keys(SCENARIOS).join(", ")}`);
  engine.home(0);
  await new Promise((r) => setTimeout(r, 300));
  const step = sc.setup(engine);
  const frames: number[] = [];
  const work: number[] = [];
  let longTasks = 0;
  let longTaskMs = 0;
  const obs = new PerformanceObserver((l) => {
    for (const e of l.getEntries()) {
      longTasks++;
      longTaskMs += e.duration;
    }
  });
  try {
    obs.observe({ entryTypes: ["longtask"] });
  } catch {
    /* not supported */
  }
  const req0 = engine.client.loader.stats.requests;
  await new Promise<void>((resolve) => {
    let t0 = -1;
    let last = -1;
    engine.beforeFrame = (now) => {
      if (t0 < 0) t0 = now;
      else {
        frames.push(now - last);
        work.push(engine.lastFrameWork);
      }
      last = now;
      const t = (now - t0) / sc.ms;
      if (t >= 1) {
        engine.beforeFrame = undefined;
        resolve();
        return;
      }
      step(t, engine);
    };
  });
  obs.disconnect();
  const s = [...frames].sort((a, b) => a - b);
  const q = (p: number) => s[Math.min(s.length - 1, Math.floor(p * s.length))];
  const total = frames.reduce((a, b) => a + b, 0);
  const ws = [...work].sort((a, b) => a - b);
  const wq = (p: number) => ws[Math.min(ws.length - 1, Math.floor(p * ws.length))];
  return {
    scenario: name,
    frames: frames.length,
    seconds: total / 1000,
    fps: frames.length / (total / 1000),
    p50: q(0.5),
    p95: q(0.95),
    p99: q(0.99),
    max: s[s.length - 1],
    over20ms: frames.filter((f) => f > 20).length,
    over33ms: frames.filter((f) => f > 33.4).length,
    workMean: work.reduce((a, b) => a + b, 0) / Math.max(1, work.length),
    workP50: wq(0.5),
    workP95: wq(0.95),
    workP99: wq(0.99),
    longTasks,
    longTaskMs,
    windowsLoaded: engine.client.windows.size,
    requests: engine.client.loader.stats.requests - req0,
  };
}
