// Drives real pointer interactions and screenshots each state (visual check of hover, selection,
// unbundling). Usage: node scripts/ui-shots.mjs <outdir> [--light]
import { chromium } from "playwright-core";
import { chrome } from "./chrome.mjs";
import { mkdirSync } from "node:fs";

const out = process.argv[2] ?? "shots-ui";
const light = process.argv.includes("--light");
mkdirSync(out, { recursive: true });
const browser = await chromium.launch({ ...chrome(), args: ["--ignore-gpu-blocklist", "--use-angle=vulkan", "--enable-features=Vulkan"] });
const page = await browser.newPage({ viewport: { width: 1600, height: 1000 }, colorScheme: light ? "light" : "dark" });
page.on("pageerror", (e) => console.error("[page error]", e.message));
await page.goto(process.env.URL ?? "http://127.0.0.1:5179");
await page.waitForFunction(() => !!window.__cv);
const idle = () =>
  page.waitForFunction(() => {
    const s = window.__cv.engine.stats();
    return s.pendingRequests === 0 && s.pendingBuilds === 0;
  }, null, { timeout: 15000 }).catch(() => console.error("not idle"));
const canvas = await page.$(".canvas canvas");
const box = await canvas.boundingBox();
// Screen position of a world point.
const toScreen = (x, y) => page.evaluate(([x, y]) => window.__cv.engine.cam.toScreen(x, y), [x, y]);
const shot = async (name) => {
  await page.waitForTimeout(300);
  await page.screenshot({ path: `${out}/${name}.png` });
};

// 1. Map view of the review-first cell.
await page.evaluate(() => window.__cv.engine.home(0));
await idle();
await shot("1-map");

// 2. Zoom into a cluster in that cell at outline zoom, hover a window.
const target = await page.evaluate(() => {
  const e = window.__cv.engine;
  const cell = e.client.boot.clusters.filter((c) => c.lane === "code" && c.heat === 4 && c.conn === 4);
  const c = cell.sort((a, b) => b.windows - a.windows)[0];
  e.frameRect(c.rect, 0);
  return c.rect;
});
await idle();
await page.waitForTimeout(500);
await idle();
await shot("2-cluster");
const w = await page.evaluate(([x, y, w, h]) => window.__cv.engine.client.windowsIn({ x, y, w, h }).sort((a, b) => (b.counts.ports - a.counts.ports))[0], [target.x, target.y, target.w, target.h]);
const [hx, hy] = await toScreen(w.rect.x + w.rect.w / 2, w.rect.y + 60);
await page.mouse.move(box.x + hx, box.y + hy);
await page.waitForTimeout(200);
await page.mouse.move(box.x + hx + 2, box.y + hy + 2);
await shot("3-hover-window");

// 3. Click it: only its links, unbundled.
await page.mouse.click(box.x + hx, box.y + hy);
await idle();
await shot("4-selected-window");

// 4. Zoom into it (L3) with the selection kept.
await page.evaluate((r) => window.__cv.engine.frameRect({ x: r.x - 300, y: r.y - 100, w: r.w + 600, h: Math.min(r.h, 1400) + 200 }, 0), w.rect);
await idle();
await page.waitForTimeout(600);
await idle();
await shot("5-selected-code");

// 5. Back to the map, click empty space inside a cluster box: unbundle the cluster.
await page.evaluate(() => window.__cv.engine.home(0));
await idle();
const sel = await page.evaluate(() => {
  const e = window.__cv.engine;
  const cell = e.client.boot.clusters.filter((c) => c.lane === "code" && c.heat === 4 && c.conn === 4);
  const c = cell.sort((a, b) => b.windows - a.windows)[0];
  e.select({ type: "cluster", c });
  return c.label;
});
await idle();
await shot("6-selected-cluster");

// 6. Hover a highway at L0.
await page.evaluate(() => {
  const e = window.__cv.engine;
  e.select(undefined);
  e.fit(0);
});
await idle();
await page.waitForTimeout(500);
const hw = await page.evaluate(() => {
  const e = window.__cv.engine;
  const h = e.highwaysOf(e.client.boot.clusters.filter((c) => c.lane === "code")[0].id)[0] ?? e["bundles"].highways.find((x) => x.count > 3);
  // A point of it in a gutter: at L0 highways run beneath the clusters, which win the pick there.
  const p = h.pts;
  const inCluster = (x, y) => e.client.boot.clusters.some((c) => x >= c.rect.x && x <= c.rect.x + c.rect.w && y >= c.rect.y && y <= c.rect.y + c.rect.h);
  const mid = p.length / 4;
  let m = mid | 0;
  let best = Infinity;
  for (let i = 0; i < p.length; i += 2) {
    if (inCluster(p[i], p[i + 1]) || Math.abs(i / 2 - mid) >= best) continue;
    best = Math.abs(i / 2 - mid);
    m = i / 2;
  }
  return e.cam.toScreen(p[m * 2], p[m * 2 + 1]);
});
await page.mouse.move(box.x + hw[0], box.y + hw[1]);
await page.waitForTimeout(150);
await page.mouse.move(box.x + hw[0] + 1, box.y + hw[1]);
await shot("7-hover-highway-l0");
console.log("selected cluster:", sel);
await browser.close();
