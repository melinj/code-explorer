// DESIGN §9 text latency: text appears ≤ 150 ms after crossing into L3 when cached, ≤ 400 ms cold.
//
// Usage: node scripts/acceptance/text-latency.mjs <ui url with ?token=…> [trials=12] [out.json]
// Needs: a running `cv serve <repo> --ui web/dist`, and web/node_modules (playwright-core) plus
// Chrome at $CHROME (default: a usual install, see web/scripts/chrome.mjs).
//
// Method (per trial, on a different code window each time):
//   1. Frame a code cluster at L2 (zoom 0.25) and wait until tiles and outlines have settled, so the
//      window header is known and only text is missing.
//   2. "cold": the window's text is not in the client cache (never fetched by this page). Jump the
//      camera onto it at L3 (zoom 0.8, no animation) and time until the frame in which its text mesh
//      is on screen (built, layer visible, alpha > 0). The text request, decode and mesh build are
//      all inside this interval; tier-crossing logic runs as it does for a user.
//   3. "cached": zoom back out to L2, then cross into L3 again over the same window:
//        - cached/mesh-kept: the engine still holds the built mesh (the common case);
//        - cached/mesh-evicted: the mesh is destroyed first (as after eviction of > 300 meshes), so
//          the text comes from the client LRU and the mesh is rebuilt. This is the budgeted figure.
// Each row also records the longest main-thread task and a frame trace (frames, longest gap between
// frames, when the tier became L3 and the text was in the client cache), so stalls are attributed.
// Timing is measured inside the page (performance.now at the camera jump → requestAnimationFrame in
// which the mesh is visible), so the resolution is one frame (~16 ms at 60 Hz).
import { writeFileSync } from "node:fs";
import { chrome } from "../../web/scripts/chrome.mjs";

const { chromium } = await import(new URL("../../web/node_modules/playwright-core/index.mjs", import.meta.url));

const url = process.argv[2];
const trials = Number(process.argv[3] ?? 12);
const out = process.argv[4];
if (!url) throw new Error("usage: text-latency.mjs <ui url> [trials] [out.json]");

const browser = await chromium.launch({
  ...chrome(),
  args: ["--ignore-gpu-blocklist", "--use-angle=vulkan", "--enable-features=Vulkan"],
});
const page = await browser.newPage({ viewport: { width: 1600, height: 1000 } });
// Long main-thread tasks (> 50 ms), to attribute outliers.
await page.addInitScript(() => {
  window.__long = [];
  try {
    new PerformanceObserver((l) => {
      for (const e of l.getEntries()) window.__long.push({ start: e.startTime, ms: e.duration });
    }).observe({ entryTypes: ["longtask"] });
  } catch {}
});
await page.goto(url);
await page.waitForFunction(() => !!window.__cv?.engine?.client?.boot, null, { timeout: 60000 });
await page.waitForTimeout(1500);

// Runs in the page: helpers on window.__lat.
await page.evaluate(() => {
  const e = window.__cv.engine;
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const centreOn = (x, y, z) => e.cam.animateTo(x - e.cam.width / 2 / z, y - e.cam.height / 2 / z, z, 0);
  const shown = (id) => {
    const m = e.code.get(id);
    return !!m && !m.c.destroyed && m.chunk.parts.code.visible && m.c.alpha > 0;
  };
  // Resolve with ms from now until `id`'s text is on screen (or -1 after 5 s).
  // Longest main-thread task overlapping [t0, t1] (ms), for attribution.
  const blocked = (t0, t1) => Math.round(Math.max(0, ...window.__long.filter((l) => l.start < t1 && l.start + l.ms > t0).map((l) => l.ms)));
  // Per measurement: frames rendered, the longest gap between frames, and when the camera reported
  // L3 and the window was known/queued — to attribute slow cases.
  const until = (id, t0) =>
    new Promise((res) => {
      const tr = { frames: 0, maxGap: 0, l3At: -1, queuedAt: -1, textAt: -1 };
      let last = t0;
      const tick = () => {
        const now = performance.now();
        tr.frames++;
        tr.maxGap = Math.max(tr.maxGap, Math.round(now - last));
        last = now;
        const w = e.client.windows.get(id);
        if (tr.l3At < 0 && e.cam.tier === "L3") tr.l3At = Math.round(now - t0);
        if (tr.queuedAt < 0 && e.codeQueue.has(id)) tr.queuedAt = Math.round(now - t0);
        if (tr.textAt < 0 && w && e.client.texts.has(e.client.textKey(w))) tr.textAt = Math.round(now - t0);
        if (shown(id)) {
          this_.traces.push(tr);
          return res(now - t0);
        }
        if (now - t0 > 5000) {
          this_.traces.push(tr);
          return res(-1);
        }
        requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
    }).then((ms) => {
      this_.longest.push(ms < 0 ? -1 : blocked(t0, t0 + ms));
      return ms;
    });
  const this_ = { longest: [], traces: [] };
  window.__lat = {
    clusters: e.client.boot.clusters
      .filter((c) => c.lane === "code" && c.windows >= 3)
      .sort((a, b) => b.windows - a.windows)
      .map((c) => c.rect),
    used: new Set(),
    async trial(k) {
      const r = this.clusters[k % this.clusters.length];
      // 1. L2 over the cluster; let tiles and outlines settle.
      centreOn(r.x + Math.min(r.w, 1600) / 2, r.y + Math.min(r.h, 1000) / 2, 0.25);
      await sleep(1500);
      const v = e.cam.view();
      const inner = { x: v.x + v.w * 0.25, y: v.y + v.h * 0.25, w: v.w * 0.5, h: v.h * 0.5 };
      const cands = e.client
        .windowsIn(inner)
        .filter((w) => !this.used.has(w.id) && !e.code.has(w.id) && !e.client.texts.has(e.client.textKey(w)) && w.rect.h > 200);
      if (!cands.length) return null;
      const w = cands[0];
      this_.longest.length = 0;
      this_.traces.length = 0;
      this.used.add(w.id);
      const cx = w.rect.x + Math.min(w.rect.w, 900) / 2;
      const cy = w.rect.y + Math.min(w.rect.h, 500) / 2;
      // 2. cold
      let t0 = performance.now();
      centreOn(cx, cy, 0.8);
      const cold = await until(w.id, t0);
      await sleep(400);
      // 3a. cached, mesh kept
      centreOn(cx, cy, 0.25);
      await sleep(800);
      t0 = performance.now();
      centreOn(cx, cy, 0.8);
      const kept = await until(w.id, t0);
      await sleep(300);
      // 3b. cached, mesh evicted (text still in the client LRU)
      centreOn(cx, cy, 0.25);
      await sleep(800);
      const m = e.code.get(w.id);
      if (m) {
        m.c.destroy({ children: true });
        e.code.delete(w.id);
      }
      const cachedText = e.client.texts.has(e.client.textKey(w));
      t0 = performance.now();
      centreOn(cx, cy, 0.8);
      const evicted = await until(w.id, t0);
      await sleep(300);
      const [lc, lk, le] = this_.longest.splice(0);
      const [tc, tk, te] = this_.traces.splice(0);
      return { longTaskMs: { cold: lc, cachedKept: lk, cachedEvicted: le }, trace: { cold: tc, cachedKept: tk, cachedEvicted: te }, window: w.id, path: w.path, lines: w.lines, cold, cachedKept: kept, cachedEvicted: evicted, cachedText };
    },
  };
});

const rows = [];
for (let k = 0; rows.length < trials && k < trials * 4; k++) {
  const r = await page.evaluate((k) => window.__lat.trial(k), k);
  if (r) {
    rows.push(r);
    console.log(JSON.stringify(r));
  }
}
const heapMB = await page.evaluate(() => Math.round((performance.memory?.usedJSHeapSize ?? 0) / 1e6));
await browser.close();

const stat = (xs) => {
  const s = xs.filter((x) => x >= 0).sort((a, b) => a - b);
  const q = (p) => s[Math.min(s.length - 1, Math.ceil(p * s.length) - 1)];
  return { n: s.length, failed: xs.length - s.length, median: +q(0.5).toFixed(1), p95: +q(0.95).toFixed(1), max: +s.at(-1).toFixed(1) };
};
const summary = {
  url: url.replace(/token=[^&]+/, "token=…"),
  trials: rows.length,
  cold: stat(rows.map((r) => r.cold)),
  cachedMeshKept: stat(rows.map((r) => r.cachedKept)),
  cachedMeshEvicted: stat(rows.map((r) => r.cachedEvicted)),
};
// Measurements during which the page rendered no frame for > 250 ms (main-thread or GPU stall).
summary.stalls = rows
  .flatMap((r) => ["cold", "cachedKept", "cachedEvicted"].map((k) => ({ path: r.path, case: k, ms: Math.round(r[k]), longTaskMs: r.longTaskMs[k], ...r.trace[k] })))
  .filter((x) => x.maxGap > 250);
summary.heapMB = heapMB;
summary.pass = {
  cold400: summary.cold.failed === 0 && summary.cold.p95 <= 400,
  cached150: summary.cachedMeshEvicted.failed === 0 && summary.cachedMeshEvicted.p95 <= 150,
};
console.log(JSON.stringify(summary, null, 2));
if (out) writeFileSync(out, JSON.stringify({ summary, rows }, null, 2));
if (!summary.pass.cold400 || !summary.pass.cached150) process.exitCode = 1;
