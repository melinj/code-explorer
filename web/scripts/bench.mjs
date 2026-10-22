// Pan/zoom benchmark in real Chrome (GPU), driven through window.__cv.
// Usage: node scripts/bench.mjs [--url http://127.0.0.1:5178] [--shots dir] [--headed] [--lens heat] [scenario…]
import { chromium } from "playwright-core";
import { chrome } from "./chrome.mjs";

const args = process.argv.slice(2);
const opt = (name, dflt) => {
  const i = args.indexOf(name);
  if (i < 0) return dflt;
  const v = args[i + 1];
  args.splice(i, 2);
  return v;
};
const flag = (name) => {
  const i = args.indexOf(name);
  if (i >= 0) args.splice(i, 1);
  return i >= 0;
};
const url = opt("--url", "http://127.0.0.1:5179");
const shots = opt("--shots", undefined);
const headed = flag("--headed");
const profile = flag("--profile");
const callers = opt("--callers", undefined);
const width = Number(opt("--width", "1600"));
const height = Number(opt("--height", "1000"));
const dpr = Number(opt("--dpr", "1"));
const cpuThrottle = Number(opt("--cpu", "1"));
const dark = flag("--dark");
/** Run every scenario under this lens (none, heat, connectedness, hotspots, cochange). */
const lens = opt("--lens", undefined);

const browser = await chromium.launch({
  ...chrome(),
  headless: !headed,
  args: ["--ignore-gpu-blocklist", "--enable-gpu-rasterization", "--use-angle=vulkan", "--enable-features=Vulkan"],
});
const page = await browser.newPage({ viewport: { width, height }, deviceScaleFactor: dpr, colorScheme: dark ? "dark" : "light" });
page.on("console", (m) => m.type() === "error" && console.error("[page]", m.text()));
page.on("pageerror", (e) => console.error("[page error]", e.message));
await page.goto(url);
await page.waitForFunction(() => !!window.__cv, null, { timeout: 30000 });
const gpu = await page.evaluate(() => {
  const gl = document.createElement("canvas").getContext("webgl2");
  const ext = gl?.getExtension("WEBGL_debug_renderer_info");
  return ext ? gl.getParameter(ext.UNMASKED_RENDERER_WEBGL) : "unknown";
});
console.error("GPU:", gpu);

if (shots) {
  const { mkdirSync } = await import("node:fs");
  mkdirSync(shots, { recursive: true });
  const at = [
    ["l0", 0.008],
    ["l1", 0.05],
    ["l2", 0.18],
    ["l2b", 0.32],
    ["l3", 0.9],
  ];
  for (const [name, z] of at) {
    await page.evaluate((z) => {
      const e = window.__cv.engine;
      // Centre on the middle cluster of the review-first cell.
      const b = e.client.boot;
      const cell = b.clusters.filter((c) => c.lane === "code" && c.heat === 4 && c.conn === 4);
      const t = cell[Math.floor(cell.length / 2)].rect;
      const c = e.cam;
      const cx = t.x + 700;
      const cy = t.y + 900;
      c.animateTo(cx - c.width / 2 / z, cy - c.height / 2 / z, z, 0);
    }, z);
    // Let data arrive and chunk builds finish.
    await page.waitForFunction(() => {
      const s = window.__cv.engine.stats();
      return s.pendingRequests === 0 && s.pendingBuilds === 0;
    }, null, { timeout: 15000 }).catch(() => console.error("timeout waiting for idle at", name));
    await page.waitForTimeout(400);
    await page.screenshot({ path: `${shots}/${name}.png` });
  }
}

const scenarios = args.length ? args : await page.evaluate(() => window.__cv.scenarios);
const results = [];
for (const s of scenarios) {
  // Fresh page per scenario so caches don't flatter later runs.
  await page.reload();
  await page.waitForFunction(() => !!window.__cv, null, { timeout: 30000 });
  if (lens) await page.evaluate((l) => window.__cv.engine.setLens(l), lens);
  const cdp = await page.context().newCDPSession(page);
  if (cpuThrottle > 1) await cdp.send("Emulation.setCPUThrottlingRate", { rate: cpuThrottle });
  if (profile) {
    await cdp.send("Profiler.enable");
    await cdp.send("Profiler.setSamplingInterval", { interval: 200 });
    await cdp.send("Profiler.start");
  }
  results.push(await page.evaluate((s) => window.__cv.bench(s), s));
  const slow = await page.evaluate(() => window.__cv.engine.slowFrames.filter((f) => f.inBench));
  for (const f of slow) {
    const ph = Object.entries(f.phases).map(([k, v]) => `${k} ${v.toFixed(1)}`).join(", ");
    console.error(`slow frame ${f.total.toFixed(1)} ms [${ph}] ${f.builds.slice(0, 4).join("; ")}`);
  }
  if (profile) {
    const { profile: prof } = await cdp.send("Profiler.stop");
    printProfile(s, prof);
  }
}

// Self time per function, from a CDP CPU profile.
function printProfile(name, prof) {
  const byId = new Map(prof.nodes.map((n) => [n.id, n]));
  const self = new Map();
  const dts = prof.timeDeltas;
  prof.samples.forEach((id, i) => {
    const n = byId.get(id);
    const f = n.callFrame;
    const key = `${f.functionName || "(anon)"} ${f.url.split("/").slice(-2).join("/")}:${f.lineNumber + 1}`;
    self.set(key, (self.get(key) ?? 0) + (dts[i] ?? 0) / 1000);
  });
  const total = [...self.values()].reduce((a, b) => a + b, 0);
  console.error(`\n== ${name}: top self time (ms) of ${total.toFixed(0)} ms sampled`);
  for (const [k, v] of [...self].sort((a, b) => b[1] - a[1]).slice(0, 28)) console.error(v.toFixed(1).padStart(8), k);
  if (!callers) return;
  // Stacks (inclusive time) leading to samples in functions matching --callers.
  const parent = new Map();
  for (const n of prof.nodes) for (const c of n.children ?? []) parent.set(c, n.id);
  const stacks = new Map();
  prof.samples.forEach((id, i) => {
    const n = byId.get(id);
    if (!n.callFrame.functionName.includes(callers)) return;
    const chain = [];
    for (let p = parent.get(id); p && chain.length < 9; p = parent.get(p)) {
      const f = byId.get(p).callFrame;
      chain.push(`${f.functionName || "(anon)"}:${f.url.split("/").pop().split("?")[0]}:${f.lineNumber + 1}`);
    }
    const k = chain.join(" < ");
    stacks.set(k, (stacks.get(k) ?? 0) + (dts[i] ?? 0) / 1000);
  });
  console.error(`-- callers of ${callers}`);
  for (const [k, v] of [...stacks].sort((a, b) => b[1] - a[1]).slice(0, 6)) console.error(v.toFixed(1).padStart(8), k);
}
const r = (v) => (typeof v === "number" ? Math.round(v * 10) / 10 : v);
console.table(results.map((x) => Object.fromEntries(Object.entries(x).map(([k, v]) => [k, r(v)]))));
console.log(JSON.stringify(results));
await browser.close();
