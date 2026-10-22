// IndexScheduler acceptance (WORKSTREAMS B7): after a jj op that changes N files, how long until the
// view.invalidated naming windows in view arrives, with a focus (view.state on the view, viewport
// over one changed file) vs without (view.state on another view). Runs against `cv serve --no-auth`.
// usage: node scripts/acceptance/viewport-first.mjs BASE REPO [N_FILES=300] [ROUNDS=6]  (REPO: a throwaway clone; files get appended to)
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

const [base, repo, nFiles = "300", rounds = "3"] = process.argv.slice(2);
const api = (p, init) => fetch(base + "/api/v1" + p, { ...init, headers: { "content-type": "application/json" } }).then((r) => r.json());
const view = await api("/views", { method: "POST", body: JSON.stringify({ mode: "snapshot", rev: "@" }) });
const boot = await api(`/views/${view.id}/bootstrap`);
const ws = new WebSocket(base.replace("http", "ws") + "/api/v1/ws");
const events = [];
ws.onmessage = (m) => events.push({ t: performance.now(), ev: JSON.parse(m.data).event });
await new Promise((r) => (ws.onopen = r));
const sendState = (viewId) => ws.send(JSON.stringify({ type: "view.state", state: { viewId, viewport: { rect, zoom: 1, tier: "L3" }, lens: "none", followAgent: false } }));

// Files of the view whose windows intersect the viewport (from tiles is overkill: use windows:batch over fragments of the cluster).
const pyFiles = execFileSync("jj", ["file", "list", "-r", "@", "--ignore-working-copy"], { cwd: repo, encoding: "utf8" })
  .split("\n").filter((f) => f.endsWith(".py"));
pyFiles.sort();
const pick = pyFiles.filter((_, i) => i % Math.max(1, Math.floor(pyFiles.length / +nFiles)) === 0).slice(0, +nFiles);
// Viewport: 900 × 560 wu around the first window of a changed file in the middle of the list.
const frag = await api(`/views/${view.id}/fragments?path=${encodeURIComponent(pick[Math.floor(pick.length / 2)])}`);
const fr = frag.fragments[0].rect;
const rect = { x: fr.x + fr.w / 2 - 450, y: fr.y + fr.h / 2 - 280, w: 900, h: 560 };
let gen = 0;
const results = [];
for (let r = 0; r < +rounds * 2; r++) {
  const focused = r % 2 === 0;
  sendState(focused ? view.id : "v_elsewhere");
  // The previous op has fully settled: no events for 2 s.
  for (;;) {
    const last = events.at(-1)?.t ?? 0;
    if (performance.now() - last > 2000) break;
    await new Promise((res) => setTimeout(res, 200));
  }
  events.length = 0;
  gen++;
  for (const f of pick) fs.appendFileSync(path.join(repo, f), `# bench ${gen}\n`);
  execFileSync("jj", ["util", "snapshot"], { cwd: repo });
  const t0 = performance.now();
  const want = focused ? 2 : 1;
  let inv = [];
  for (let i = 0; i < 1200; i++) {
    inv = events.filter((e) => e.ev.type === "view.invalidated" && e.ev.viewId === view.id && e.ev.windows?.length);
    if (inv.length >= want) break;
    await new Promise((res) => setTimeout(res, 10));
  }
  const op = events.find((e) => e.ev.type === "op.changed");
  const first = inv[0], last = inv[inv.length - 1];
  results.push({
    focused,
    stages: inv.length,
    firstWindows: first?.ev.windows?.length,
    msOpChanged: op ? Math.round(op.t - t0) : null,
    msToFirst: first ? Math.round(first.t - t0) : null,
    msToLast: last ? Math.round(last.t - t0) : null,
    progress: events.filter((e) => e.ev.type === "index.progress").map((e) => `${e.ev.done}/${e.ev.total}`),
  });
  console.log(JSON.stringify(results.at(-1)));
}
const med = (xs) => xs.sort((a, b) => a - b)[Math.floor(xs.length / 2)];
for (const f of [true, false]) {
  const rs = results.filter((x) => x.focused === f);
  console.log(`${f ? "focused" : "unfocused"}: first view.invalidated ${med(rs.map((x) => x.msToFirst))} ms, last ${med(rs.map((x) => x.msToLast))} ms (median of ${rs.length})`);
}
ws.close();
