// Long pan session (§5 "memory stays bounded"): flies between clusters at every tier for a while and
// checks that memory stays bounded, reporting the JS heap over time, frame intervals and
// garbage-collection pauses (from a Chrome trace).
// Usage: node scripts/soak.mjs [--url http://127.0.0.1:5179] [--seconds 180] [--out trace.json]
//          [--max-slope 20] [--max-retained 400] [--max-heap 900] [--warmup 40] [--no-retained]
//          [--gc-every 15] [--leak MB/min]
// The retained heap is measured after a forced full GC (CDP HeapProfiler.collectGarbage) every 15
// stops (~25 s; --gc-every) once the warm-up is over: those are the true low points, not the GC-timing-dependent
// ones a sampler sees. Fails (exit 1, "FAIL" lines) when the retained heap rises faster than --max-slope
// MB/min (least squares over the samples), when it ends above --max-retained MB, or when the heap
// ever exceeds --max-heap MB. Frames during a forced GC are left out of the frame statistics.
import { chromium } from "playwright-core";
import { chrome } from "./chrome.mjs";
import { waits, sleep } from "./e2e-wait.mjs";
import { writeFileSync } from "node:fs";

const args = process.argv.slice(2);
const opt = (name, dflt) => {
  const i = args.indexOf(name);
  return i < 0 ? dflt : args[i + 1];
};
const url = opt("--url", "http://127.0.0.1:5179");
const seconds = Number(opt("--seconds", "180"));
const out = opt("--out", undefined);
const maxSlope = Number(opt("--max-slope", "20"));
const maxRetained = Number(opt("--max-retained", "400"));
const maxHeap = Number(opt("--max-heap", "900"));
const warmup = Number(opt("--warmup", "40"));
// --no-retained: no forced GCs (frame and GC-pause statistics exactly as a user would see them; the
// retained-heap checks are skipped).
const probe = !args.includes("--no-retained");
// --leak N: the page retains N MB more every minute (a self-test: the slope check must fail it).
const leak = Number(opt("--leak", "0"));
// One retained-heap sample per 3 rounds of the 5 zoom levels (~25 s); --gc-every 5 samples every
// round (a multiple of 5 keeps every sample at the same point of the zoom cycle).
const gcEveryStops = Number(opt("--gc-every", "15"));

// CHROME_ARGS overrides the GPU flags (CI without a GPU: SwiftShader).
const gpu = process.env.CHROME_ARGS ? process.env.CHROME_ARGS.split(" ") : ["--ignore-gpu-blocklist", "--enable-gpu-rasterization", "--use-angle=vulkan", "--enable-features=Vulkan"];
const browser = await chromium.launch({
  ...chrome(),
  args: [...gpu, "--enable-precise-memory-info"],
});
const page = await browser.newPage({ viewport: { width: 1600, height: 1000 } });
const errors = [];
page.on("pageerror", (e) => (errors.push(e.message), console.error("[page error]", e.message)));
await page.goto(url);
await page.waitForFunction(() => !!window.__cv, null, { timeout: 60000 });
await waits(page).settle(); // the boot data is in and drawn
const cdp = await page.context().newCDPSession(page);
await cdp.send("HeapProfiler.enable");

// Frame intervals from rAF, independent of the engine's own stats; the heap on every frame (its
// peak); nothing counted while a forced GC runs.
await page.evaluate(() => {
  const s = (window.__soak = { frames: 0, over33: 0, over100: 0, max: 0, last: performance.now(), heapMax: 0, paused: false });
  const tick = (t) => {
    const dt = t - s.last;
    s.last = t;
    const m = performance.memory;
    if (m && m.usedJSHeapSize > s.heapMax) s.heapMax = m.usedJSHeapSize;
    if (!s.paused) {
      s.frames++;
      if (dt > 33.4) s.over33++;
      if (dt > 100) s.over100++;
      if (dt > s.max) s.max = dt;
    }
    requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
});

/** Full GC, then the heap that is really retained (MB). */
async function retained() {
  await page.evaluate(() => (window.__soak.paused = true));
  await cdp.send("HeapProfiler.collectGarbage");
  return page.evaluate(async () => {
    const v = Math.round(performance.memory.usedJSHeapSize / 1e6);
    // Let the frame after the pause go by uncounted.
    await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    window.__soak.last = performance.now();
    window.__soak.paused = false;
    return v;
  });
}

if (leak > 0)
  await page.evaluate((mbPerMin) => {
    // Doubles in plain arrays live on the JS heap (ArrayBuffers would not count): 8 bytes each.
    const kept = (window.__leak = []);
    const perTick = Math.round((mbPerMin * 1e6) / 60 / 4 / 8);
    setInterval(() => kept.push(new Array(perTick).fill(0.5)), 250);
  }, leak);
await browser.startTracing(page, { categories: ["v8", "devtools.timeline", "disabled-by-default-v8.gc"] });
const t0 = Date.now();
const samples = []; // [seconds, retained MB]
let stop = 0;
const zooms = [0.035, 0.07, 0.15, 0.3, 0.7];
while (Date.now() - t0 < seconds * 1000) {
  const el = (Date.now() - t0) / 1000;
  // Sampled at the same point of the zoom cycle every time (after the L3 stop): what is built
  // depends on the tier, so samples taken at random phases scatter by ±20 MB.
  if (probe && el >= warmup && stop % gcEveryStops === 0) samples.push([Math.round(el), await retained()]);
  await page.evaluate(
    ([i, z]) => {
      const e = window.__cv.engine;
      const cs = e.client.boot.clusters.filter((c) => c.windows > 0);
      // A deterministic walk over the whole map, so later stops are far from earlier ones.
      const c = cs[(i * 7919) % cs.length];
      e.cam.flyAlong(c.rect.x + c.rect.w / 2 - e.cam.width / 2 / z, c.rect.y + c.rect.h / 2 - e.cam.height / 2 / z, z);
      window.__soak.maxWindows = Math.max(window.__soak.maxWindows ?? 0, e.client.windows.size);
    },
    [stop, zooms[stop % zooms.length]],
  );
  stop++;
  // The walk's cadence (a 1.5 s flight + 100 ms) is deliberate: the soak measures over wall time.
  // On a slow machine a flight can run late: the next one starts only after it ends (bounded).
  await sleep(1600);
  await page.waitForFunction(() => !window.__cv.engine.cam.moving, null, { timeout: 10000, polling: 50 }).catch(() => {});
}
if (probe && samples.length < 3) samples.push([Math.round((Date.now() - t0) / 1000), await retained()]);
const trace = JSON.parse((await browser.stopTracing()).toString());
if (out) writeFileSync(out, JSON.stringify(trace));
// (engine.slowFrames keeps the last 50 frames whose work took > 20 ms)
const soak = await page.evaluate(() => {
  const e = window.__cv.engine;
  return { ...window.__soak, windows: e.client.windows.size, windowCap: e.client.windowCap, peakWindows: Math.max(e.client.peakWindows ?? 0, window.__soak.maxWindows ?? 0), edges: e.client["edgeIds"].size, chunks: e["chunks"].size, built: [...e["chunks"].values()].filter((c) => c.built).length, longEdges: e.client.longEdges.length, links: e.client.links.size, outlines: e.client.outlines["map"].size, texts: e.client.texts["map"].size, code: e["code"].size, evict: e.client.lastEvictStats };
});
// GC pauses on the page's main thread (the forced collections included: they show in gcMaxMs).
const events = trace.traceEvents ?? trace;
const gcs = events.filter((ev) => ev.ph === "X" && /^(MajorGC|MinorGC|V8\.GC_MARK_COMPACTOR|V8\.GCScavenger|V8\.GCFinalizeMC)$/.test(ev.name)).map((ev) => [ev.name, ev.dur / 1000]);
const gcMax = gcs.reduce((m, [, d]) => Math.max(m, d), 0);
const gcTotal = gcs.reduce((a, [, d]) => a + d, 0);
// Least-squares slope of the retained heap, MB per minute.
const slope = (() => {
  if (samples.length < 3) return 0;
  const n = samples.length;
  const mx = samples.reduce((a, [t]) => a + t, 0) / n;
  const my = samples.reduce((a, [, v]) => a + v, 0) / n;
  const num = samples.reduce((a, [t, v]) => a + (t - mx) * (v - my), 0);
  const den = samples.reduce((a, [t]) => a + (t - mx) ** 2, 0);
  return den ? (num / den) * 60 : 0;
})();
const heapMax = Math.round(soak.heapMax / 1e6);
const last = samples.length ? samples[samples.length - 1][1] : NaN;
const result = {
  seconds,
  stops: stop,
  frames: soak.frames,
  over33ms: soak.over33,
  over100ms: soak.over100,
  maxFrameMs: Math.round(soak.max),
  gcEvents: gcs.length,
  gcMaxMs: Math.round(gcMax * 10) / 10,
  gcOver33ms: gcs.filter(([, d]) => d > 33).length,
  gcTotalMs: Math.round(gcTotal),
  retainedMb: samples.map(([, v]) => v),
  retainedSlopeMbPerMin: Math.round(slope * 10) / 10,
  heapMbMax: heapMax,
  windows: soak.windows,
  peakWindows: soak.peakWindows,
  windowCap: soak.windowCap,
  edges: soak.edges,
  chunks: soak.chunks,
  builtChunks: soak.built,
  longEdges: soak.longEdges,
  links: soak.links,
  outlines: soak.outlines,
  texts: soak.texts,
  codeMeshes: soak.code,
  lastEvict: soak.evict,
};
const slow = await page.evaluate(() => window.__cv.engine.slowFrames.sort((a, b) => b.total - a.total).slice(0, 5).map((f) => `${f.total.toFixed(0)} ms [${Object.entries(f.phases).filter(([, v]) => v > 1).map(([k, v]) => `${k} ${v.toFixed(0)}`).join(", ")}] ${f.builds.slice(0, 3).join("; ")}`));
for (const f of slow) console.log("slow frame:", f);
console.log("retained heap MB (after a full GC):", samples.map(([t, mb]) => `${t}s:${mb}`).join(" "));
console.log(JSON.stringify(result));
const check = (ok, what) => {
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
  if (!ok) process.exitCode = 1;
};
if (probe) {
check(samples.length >= 3, `${samples.length} retained-heap samples (≥ 3 needed: run longer than ${warmup + 75} s)`);
check(slope <= maxSlope, `retained heap slope ${slope.toFixed(1)} MB/min ≤ ${maxSlope} (${samples[0][1]} → ${last} MB over ${samples[samples.length - 1][0] - samples[0][0]} s)`);
check(last <= maxRetained, `retained heap at the end ${last} MB ≤ ${maxRetained}`);
}
check(heapMax <= maxHeap, `heap peak ${heapMax} MB ≤ ${maxHeap}`);
check(soak.peakWindows <= soak.windowCap, `windows held stay under the cap (peak ${soak.peakWindows} of ${soak.windowCap})`);
check(errors.length === 0, `no page errors ${errors.length ? JSON.stringify(errors.slice(0, 3)) : ""}`);
await browser.close();
