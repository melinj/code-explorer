// DESIGN §9 "after a jj op the canvas updates < 2 s later" (B7: op.changed then view.invalidated
// within 2 s), measured end to end in the real UI.
//
// Usage: node scripts/acceptance/repaint-after-op.mjs <ui url with ?token=…> <repo dir> [trials=10] [out.json]
// The repo must be a THROWAWAY jj clone that `cv serve` is serving (this script edits files in it and
// runs `jj status`, which snapshots the working copy: one jj operation per trial).
//
// Method (per trial):
//   1. In the browser, frame a code window at L3, wait until its text is on screen and the page is
//      idle (no main-thread task > 50 ms for 1.5 s; dense clusters keep the UI busy for seconds
//      after arrival with edge tessellation and GC — recorded as longTasks).
//   2. From this script: append " # cvmark<N>" to the window's first line on disk, then run
//      `jj status` (snapshot → new operation). t_op = when `jj status` returned (the op exists).
//   3. A separate WebSocket listener records when op.changed and view.invalidated arrive.
//   4. In the page, a requestAnimationFrame probe records the first frame in which the window's
//      rebuilt text mesh (with the marker) is on screen. Repaint latency = that frame − t_op
//      (wall clock, same machine). The UI's own path runs unmodified: WS event → refetch headers
//      and text → mesh rebuild.
import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { chrome } from "../../web/scripts/chrome.mjs";

const { chromium } = await import(new URL("../../web/node_modules/playwright-core/index.mjs", import.meta.url));

const [url, repo] = process.argv.slice(2);
const trials = Number(process.argv[4] ?? 10);
const out = process.argv[5];
if (!url || !repo) throw new Error("usage: repaint-after-op.mjs <ui url> <repo dir> [trials] [out.json]");
const u = new URL(url);
const token = u.searchParams.get("token");

// WS listener (Node's global WebSocket).
const events = [];
const ws = new WebSocket(`ws://${u.host}/api/v1/ws${token ? `?token=${token}` : ""}`);
ws.onmessage = (m) => {
  const f = JSON.parse(m.data);
  if (f.event.type === "op.changed" || f.event.type === "view.invalidated") {
    events.push({ at: Date.now(), type: f.event.type, reason: f.event.reason, windows: f.event.windows?.length });
  }
};
await new Promise((r) => (ws.onopen = r));

const browser = await chromium.launch({
  ...chrome(),
  args: ["--ignore-gpu-blocklist", "--use-angle=vulkan", "--enable-features=Vulkan"],
});
const page = await browser.newPage({ viewport: { width: 1600, height: 1000 } });
// Network timeline (wall clock): when the UI's refetches after the op finish.
const net = [];
const started = new Map();
page.on("request", (r) => started.set(r, Date.now()));
page.on("requestfinished", (r) => {
  const p = new URL(r.url()).pathname;
  if (p.includes("/tiles/") || p.includes("/blobs/")) net.push({ at: Date.now(), start: started.get(r), kind: p.includes("/tiles/") ? "tiles" : "text" });
});
// Frames the page's own WebSocket receives (to separate delivery from client work).
const pageFrames = [];
page.on("websocket", (w) =>
  w.on("framereceived", (f) => {
    try {
      const ev = JSON.parse(f.payload).event;
      if (ev.type === "view.invalidated" || ev.type === "op.changed") pageFrames.push({ at: Date.now(), type: ev.type, reason: ev.reason, windows: ev.windows?.length });
    } catch {}
  }),
);
// In-page: when the UI's JS sees each WS frame, and long main-thread tasks (> 50 ms).
await page.addInitScript(() => {
  window.__wsSeen = [];
  window.__long = [];
  const Orig = window.WebSocket;
  window.WebSocket = class extends Orig {
    constructor(...a) {
      super(...a);
      this.addEventListener("message", (m) => {
        try {
          const ev = JSON.parse(m.data).event;
          if (ev.type === "view.invalidated" || ev.type === "op.changed") window.__wsSeen.push({ at: Date.now(), type: ev.type });
        } catch {}
      });
    }
  };
  try {
    new PerformanceObserver((l) => {
      for (const e of l.getEntries()) window.__long.push({ at: Math.round(performance.timeOrigin + e.startTime), ms: Math.round(e.duration) });
    }).observe({ entryTypes: ["longtask"] });
  } catch {}
});
await page.goto(url);
await page.waitForFunction(() => !!window.__cv?.engine?.client?.boot, null, { timeout: 60000 });
await page.waitForTimeout(1500);

await page.evaluate(() => {
  const e = window.__cv.engine;
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const centreOn = (x, y, z) => e.cam.animateTo(x - e.cam.width / 2 / z, y - e.cam.height / 2 / z, z, 0);
  const shownWith = (id, mark) => {
    const m = e.code.get(id);
    const w = e.client.windows.get(id);
    if (!m || m.c.destroyed || !m.chunk.parts.code.visible || m.c.alpha <= 0 || !w) return false;
    const t = e.client.texts.get(e.client.textKey(w));
    return !mark || !!t?.lines?.some((l) => l.includes(mark));
  };
  window.__rp = {
    clusters: e.client.boot.clusters.filter((c) => c.lane === "code" && c.windows >= 3).sort((a, b) => b.windows - a.windows).map((c) => c.rect),
    used: new Set(),
    /** Frame a .py window at L3 and wait for its text. */
    async pick(k) {
      const r = this.clusters[k % this.clusters.length];
      centreOn(r.x + Math.min(r.w, 1600) / 2, r.y + Math.min(r.h, 1000) / 2, 0.25);
      await sleep(1500);
      const v = e.cam.view();
      const inner = { x: v.x + v.w * 0.25, y: v.y + v.h * 0.25, w: v.w * 0.5, h: v.h * 0.5 };
      const w = e.client.windowsIn(inner).find((w) => !this.used.has(w.path) && w.path.endsWith(".py") && w.rect.h > 200);
      if (!w) return null;
      this.used.add(w.path);
      centreOn(w.rect.x + Math.min(w.rect.w, 900) / 2, w.rect.y + Math.min(w.rect.h, 500) / 2, 0.8);
      for (let i = 0; i < 100 && !shownWith(w.id); i++) await sleep(50);
      if (!shownWith(w.id)) return null;
      return { id: w.id, path: w.path, line: w.lines.start };
    },
    /** Resolve with Date.now() of the first frame showing the marker in window `id`. */
    watch(id, mark) {
      return new Promise((res) => {
        const t0 = Date.now();
        const tick = () => {
          if (shownWith(id, mark)) return res(Date.now());
          if (Date.now() - t0 > 15000) return res(-1);
          requestAnimationFrame(tick);
        };
        requestAnimationFrame(tick);
      });
    },
  };
});

const rows = [];
for (let k = 0; rows.length < trials && k < trials * 4; k++) {
  const w = await page.evaluate((k) => window.__rp.pick(k), k);
  if (!w) continue;
  // Let the page go idle first (the user is reading the window when the op lands): no main-thread
  // task > 50 ms for 1.5 s (at most 20 s).
  const settledAfter = await page.evaluate(async () => {
    const t0 = Date.now();
    while (Date.now() - t0 < 20000) {
      const last = window.__long.at(-1);
      if (!last || Date.now() - (last.at + last.ms) > 1500) break;
      await new Promise((r) => setTimeout(r, 100));
    }
    return Date.now() - t0;
  });
  const mark = `cvmark${Date.now() % 1e9}`;
  const watching = page.evaluate(([id, mark]) => window.__rp.watch(id, mark), [w.id, mark]);
  const file = join(repo, w.path);
  const lines = readFileSync(file, "utf8").split("\n");
  lines[w.line - 1] += `  # ${mark}`;
  const n0 = events.length;
  const m0 = net.length;
  const p0 = pageFrames.length;
  const tEdit = Date.now();
  writeFileSync(file, lines.join("\n"));
  execFileSync("jj", ["status", "--quiet"], { cwd: repo, stdio: "ignore" });
  const tOp = Date.now();
  const tPaint = await watching;
  const ev = events.slice(n0);
  const opChanged = ev.find((e) => e.type === "op.changed");
  const inval = ev.find((e) => e.type === "view.invalidated" && e.windows);
  const nv = net.slice(m0);
  const after = (kind, t) => nv.find((x) => x.kind === kind && x.start >= t);
  const tilesDone = inval ? after("tiles", inval.at) : undefined;
  const textDone = tilesDone ? after("text", tilesDone.at) : undefined;
  const js = await page.evaluate((t) => ({ seen: window.__wsSeen.filter((x) => x.at >= t - 1000), long: window.__long.filter((x) => x.at >= t - 3000) }), tEdit);
  const pf = pageFrames.slice(p0).map((f) => ({ ...f, at: f.at - tOp }));
  const row = {
    events: ev.map((e) => ({ ...e, at: e.at - tOp })),
    pageFrames: pf,
    pageJsSeen: js.seen.map((x) => ({ ...x, at: x.at - tOp })),
    longTasks: js.long.map((x) => ({ ...x, at: x.at - tOp })),
    settleWaitMs: settledAfter,
    path: w.path,
    line: w.line,
    jjStatusMs: tOp - tEdit,
    opChangedMs: opChanged ? opChanged.at - tOp : null,
    invalidatedMs: inval ? inval.at - tOp : null,
    headersRequestedMs: tilesDone ? tilesDone.start - tOp : null,
    headersRefetchedMs: tilesDone ? tilesDone.at - tOp : null,
    textRefetchedMs: textDone ? textDone.at - tOp : null,
    repaintMs: tPaint > 0 ? tPaint - tOp : -1,
  };
  rows.push(row);
  console.log(JSON.stringify(row));
  await page.waitForTimeout(500);
}
await browser.close();
ws.close();

const stat = (xs) => {
  const s = xs.filter((x) => x !== null && x >= 0).sort((a, b) => a - b);
  if (!s.length) return { n: 0, failed: xs.length };
  const q = (p) => s[Math.min(s.length - 1, Math.ceil(p * s.length) - 1)];
  return { n: s.length, failed: xs.length - s.length, median: q(0.5), p95: q(0.95), max: s.at(-1) };
};
const summary = {
  trials: rows.length,
  opChanged: stat(rows.map((r) => r.opChangedMs)),
  viewInvalidated: stat(rows.map((r) => r.invalidatedMs)),
  headersRefetched: stat(rows.map((r) => r.headersRefetchedMs)),
  textRefetched: stat(rows.map((r) => r.textRefetchedMs)),
  repaint: stat(rows.map((r) => r.repaintMs)),
};
summary.pass = summary.repaint.n > 0 && summary.repaint.failed === 0 && summary.repaint.max < 2000;
console.log(JSON.stringify(summary, null, 2));
if (out) writeFileSync(out, JSON.stringify({ summary, rows }, null, 2));
if (!summary.pass) process.exitCode = 1;
