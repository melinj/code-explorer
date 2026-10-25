// End-to-end check of the panels, the agent loop (simulated by cv-mock), range views, lenses and the
// data client (F2), against cv-mock or `cv serve`.
// Usage: URL=… [REPO=<repo cv serve serves>] [NO_SIM_AGENT=1] node scripts/panels-flow.mjs <screenshot dir>
// With REPO set (cv serve), states the mock can only fake are created for real with jj in that repo:
// a stale workspace, an agent attached to a workspace, a blocked undo, a range that is not a chain,
// overlapping stacks for Compare, a dynamic link, a conflict, a divergent workspace.
import { chromium } from "playwright-core";
import { chrome } from "./chrome.mjs";
import { mkdirSync, writeFileSync, readFileSync, existsSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { dirname, basename, join } from "node:path";
import { waits, ms } from "./e2e-wait.mjs";

const out = process.argv[2] ?? "shots-panels";
mkdirSync(out, { recursive: true });
const URL_ = process.env.URL ?? "http://127.0.0.1:5179";
const REPO = process.env.REPO;
const RUN = Date.now().toString(36).slice(-5);
/** jj in the served repo (or another workspace dir), never interactive. */
const jjIn = (dir, ...args) => execFileSync("jj", ["-R", dir, "--no-pager", "--color=never", ...args], { cwd: dir, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], env: { ...process.env, JJ_EDITOR: "true" } }).trim();
const jj = (...args) => jjIn(REPO, ...args);
/** A sibling directory of the repo for an extra workspace. */
const wsDir = (name) => join(dirname(REPO), `${basename(REPO)}.${name}`);
const skip = (what) => console.log(`skip ${what}`);
// CHROME_ARGS overrides the GPU flags (CI without a GPU: "--use-angle=swiftshader --enable-unsafe-swiftshader").
const browser = await chromium.launch({ ...chrome(), args: process.env.CHROME_ARGS ? process.env.CHROME_ARGS.split(" ") : ["--ignore-gpu-blocklist", "--use-angle=vulkan", "--enable-features=Vulkan"] });
const page = await browser.newPage({ viewport: { width: 1600, height: 1000 }, colorScheme: process.env.LIGHT ? "light" : "dark" });
const errors = [];
// With a few stack lines, so an error names where it came from.
page.on("pageerror", (e) => errors.push(e.message + (e.stack ? " @ " + e.stack.split("\n").slice(1, 4).map((l) => l.trim()).join(" | ") : "")));
// The permission check provokes a 403, the not-a-chain check a 400 and the blocked-undo check a 409
// on purpose. 404s are judged by URL below: the UI degrades
// around routes a server may not offer yet (cv-mock: undo, range metrics, interdiff, review marks).
page.on("console", (m) => m.type() === "error" && !/ (400|403|404|409) /.test(m.text()) && errors.push(m.text()));
const OPTIONAL = [/\/edits\/[^/]+\/undo$/, /\/views\/[^/]+\/metrics/, /\/interdiff/, /\/reviews\/marks\//, /\/workspaces:compare$/];
// A conflict's detail can 404 "… is not conflicted at …" when a jj op (this script's, with REPO)
// resolves it between the UI's list and detail requests; the UI drops it. Judged on the body.
const pending404 = [];
page.on("response", (r) => {
  if (r.status() !== 404 || !r.url().includes("/api/") || OPTIONAL.some((re) => re.test(r.url()))) return;
  if (/\/api\/v1\/conflicts\/[^/]+\//.test(r.url())) pending404.push(r.text().then((t) => /is not conflicted at/.test(t) || errors.push(`404 ${r.url()}`), () => errors.push(`404 ${r.url()}`)));
  else errors.push(`404 ${r.url()}`);
});
// Client WS frames (view.subscribe, view.state, ping), to check what this tab subscribes to.
const sentFrames = [];
page.on("response", (r) => r.request().method() === "PATCH" && !r.ok() && r.text().then((t) => console.log(`     PATCH ${r.status()} ${t.slice(0, 200)}`)).catch(() => {}));
page.on("websocket", (ws) => ws.on("framesent", (f) => typeof f.payload === "string" && sentFrames.push(f.payload)));
await page.goto(URL_);
await page.waitForFunction(() => !!window.__cv);
// op.changed events seen by this tab: after a jj command, `opSeen(n0)` waits until the server has
// synced it (instead of a fixed delay).
await page.evaluate(() => { window.__ops = 0; window.__cv.live.on((ev) => { if (ev.type === "op.changed") window.__ops++; }); });
const opCount = () => page.evaluate(() => window.__ops);
const opSeen = (n0, t = 60000) => page.waitForFunction((n0) => window.__ops > n0, n0, { timeout: ms(t), polling: 100 }).then(() => true, () => (console.log(`     no op.changed after ${t} ms`), false));
// Requests as another client would send them (not through this tab's fetch, session or socket).
const token = new URL(URL_).searchParams.get("token");
const origin = new URL(URL_).origin;
const other = async (method, path, body) => {
  const r = await page.request.fetch(origin + path, { method, data: body, headers: { ...(token ? { Authorization: `Bearer ${token}` } : {}), "X-CV-Session": `e2e-other-${RUN}` } });
  let json = null;
  try { json = await r.json(); } catch {}
  return { status: r.status(), json };
};
const api = (method, path, body, headers = {}) => page.evaluate(async ([m, p, b, h]) => {
  const r = await fetch(p, { method: m, headers: { ...(b === undefined ? {} : { "content-type": "application/json" }), ...h }, body: b === undefined ? undefined : JSON.stringify(b) });
  let json = null;
  try { json = await r.json(); } catch {}
  return { status: r.status, json };
}, [method, path, body, headers]);
/** Wait for `fn` (a page function) to return something truthy; resolves to it, or null on timeout. */
const until = (fn, arg, t = 30000) => page.waitForFunction(fn, arg, { timeout: ms(t), polling: 200 }).then((h) => h.jsonValue()).catch(() => null);
// No fixed sleeps: `idle` waits until the camera is at rest, its data update ran and nothing is
// loading or building (e2e-wait.mjs); `flown(v0)` until a flight that started after `camV()` ended.
const { settle: steady, camV, flown, frames, pollNode } = waits(page);
const idle = () => steady();
const shot = async (n) => (await steady(), await frames(), page.screenshot({ path: `${out}/${n}.png` }));
const check = (ok, what) => {
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
  if (!ok) process.exitCode = 1;
};
const tab = (name) => page.click(`.tabs button:has-text('${name}')`, { timeout: ms(60000) });
/** Leave a range: the snapshot view is re-created and rebuilt (seconds on a 20k-window map under
 *  load); wait until that switch is over, not just until isRange flips. */
const backToSnapshot = async () => {
  await page.click(".chip.range .x");
  await until(() => !window.__cv.engine.isRange && !window.__cv.engine.stats().pendingSwitch, undefined, 120000);
  await idle();
};
const box0 = await (await page.$(".canvas canvas")).boundingBox();
// Real repos open review-first below tile zoom: frame the biggest hot code cluster at map zoom so
// code windows (and split files) are loaded.
const ensureCode = async () => {
  if (await page.evaluate(() => { const e = window.__cv.engine; return [...e.client.windows.values()].some((w) => w.lane === "code" && !w.flags.minimized && w.lines.end - w.lines.start > 8 && !e.review.conflicts.has(w.path)); })) return;
  await page.evaluate(() => {
    const e = window.__cv.engine;
    const code = e.client.boot.clusters.filter((c) => c.lane === "code").sort((a, z) => z.heat - a.heat || z.windows - a.windows);
    const c = code[0];
    e.cam.animateTo(c.rect.x + c.rect.w / 2 - e.cam.width / 2 / 0.08, c.rect.y + c.rect.h / 2 - e.cam.height / 2 / 0.08, 0.08, 0);
  });
  await idle();
};
const sessionHeader = page.waitForRequest((r) => r.url().includes("/api/v1/") && !!r.headers()["x-cv-session"], { timeout: ms(30000) }).then((r) => r.headers()["x-cv-session"]).catch(() => undefined);
await idle();
await page.waitForSelector(".live.live", { timeout: ms(30000) }).then(() => check(true, "event stream connected")).catch(() => check(false, "event stream connected"));

for (const t of ["History", "Workspaces", "Hotspots", "Review"]) {
  await tab(t);
  // The tab is on and its panel has its data.
  await page.waitForSelector(`.tabs button.on:has-text('${t}')`, { timeout: ms(10000) }).catch(() => {});
  await until(() => !document.querySelector(".side")?.textContent?.includes("loading…"));
  await shot(`p-${t.toLowerCase()}`);
}
{
  const sid = await sessionHeader;
  const ws = await page.evaluate(() => window.__cv.live["ws"]?.url ?? "");
  check(!!sid && ws.includes(`session=${sid}`), `this tab's session id goes on API calls and the WS URL (${sid?.slice(0, 8)}…)`);
}
{
  const viewId = await page.evaluate(() => window.__cv.engine.client.view.id);
  await pollNode(async () => sentFrames.some((f) => f.includes('"view.subscribe"') && f.includes(viewId)), { timeout: ms(30000) });
  check(sentFrames.some((f) => f.includes('"view.subscribe"') && f.includes(viewId)), `the tab subscribes to its view over WS (${viewId})`);
}
// Lanes (§4.3): a lane the server marks collapsed (docs, by default) starts closed: its pill is off
// and none of its windows is drawn. Flying to one of its files opens it.
{
  const st = await page.evaluate(() => {
    const e = window.__cv.engine;
    const defs = e.client.boot.laneDefs.filter((l) => l.collapsed && !l.hidden);
    const pills = Object.fromEntries([...document.querySelectorAll("button.pill")].map((b) => [b.textContent, b.getAttribute("aria-pressed")]));
    return { collapsed: defs.map((l) => l.id), labels: defs.map((l) => l.label), pills, hidden: [...(e.look.hiddenLanes ?? [])], docsFile: e.client.boot.lanes.find((l) => l.lane === "docs")?.files ?? 0 };
  });
  if (st.collapsed.length || REPO) {
    check(st.collapsed.includes("docs") && st.collapsed.every((l) => st.hidden.includes(l)) && st.labels.every((l) => st.pills[l] === "false"), `collapsed lanes start closed (${st.collapsed.join(", ") || "none"}; pills ${st.labels.map((l) => `${l}=${st.pills[l]}`).join(", ")})`);
    if (REPO && st.docsFile) {
      const opened = await page.evaluate(async () => {
        const e = window.__cv.engine;
        const w = await e.goTo({ path: "README.md" });
        // Until the lane is open (bounded; the check below reports what it saw).
        for (let t0 = performance.now(); e.look.hiddenLanes?.has("docs") && performance.now() - t0 < 30000; ) await new Promise((r) => setTimeout(r, 50));
        await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
        const pill = [...document.querySelectorAll("button.pill")].find((b) => b.textContent === e.client.boot.laneDefs.find((l) => l.id === "docs")?.label);
        return { lane: w?.lane, open: !e.look.hiddenLanes?.has("docs"), pressed: pill?.getAttribute("aria-pressed") };
      });
      check(opened.lane === "docs" && opened.open && opened.pressed === "true", `flying to README.md opens the docs lane (${JSON.stringify(opened)})`);
    }
  } else skip("collapsed lanes: the server marks none (cv-mock)");
}
// History (§7): rows render; the revset field narrows them; conflicts get ×; "changed since review"
// opens the interdiff.
{
  await tab("History");
  const n = await page.waitForFunction(() => document.querySelectorAll(".hist li").length, null, { timeout: ms(30000) }).then((h) => h.jsonValue()).catch(() => 0);
  check(n > 0, `history rendered (${n} rows)`);
  const conflicted = await page.evaluate(() => [document.querySelectorAll(".hist li.conflicted").length, document.querySelectorAll(".hist li .conflict-glyph").length]);
  // Not vacuous: with REPO the repo has conflicted changes (seeded); a server that flags none is skipped.
  const serverConflicted = (await api("GET", "/api/v1/revisions?limit=200")).json?.rows?.filter((r) => r.rev.flags.conflict).length ?? 0;
  if (conflicted[0] || serverConflicted || REPO) check(conflicted[0] > 0 && conflicted[0] === serverConflicted && conflicted[0] === conflicted[1], `conflicted changes carry × (${conflicted[0]} rows of ${serverConflicted} conflicted, ${conflicted[1]} marks)`);
  else skip("conflict × check: the server flags no conflicted change in the history");
  // A review mark set by another client shows up live (review.marked; the panel's slow re-read is 15 s,
  // so 4 s proves the event): "changed since review" on a change marked reviewed at its parent.
  {
    const rows = (await api("GET", "/api/v1/revisions?limit=50")).json?.rows ?? [];
    // A change with files of its own, so its interdiff against the parent shows them.
    const row = rows.find((r) => r.rev.parents.length && !r.review?.changedSinceReview && !r.rev.flags.immutable && !r.rev.flags.empty);
    const res = row ? await other("PUT", `/api/v1/reviews/marks/${row.rev.change}`, { commit: row.rev.parents[0] }) : { status: 0 };
    if (!row) skip("remote mark check: no mutable change with a parent in the history");
    else if (res.status === 404 && !REPO) skip("remote mark check: this server has no review marks (PUT /reviews/marks 404)");
    else {
      const shown = await page.waitForSelector(`.hist li[data-change="${row.rev.change}"] .chip.csr`, { timeout: ms(30000) }).then(() => true).catch(() => false);
      const served = (await api("GET", "/api/v1/revisions?limit=50")).json?.rows?.find((r) => r.rev.change === row.rev.change)?.review?.changedSinceReview;
      if (!served && !REPO) skip("remote mark check: this server's /revisions does not reflect review marks");
      else check(res.status === 200 && shown, `History refreshes live on a review mark from another client (“changed since review” on ${row.rev.change.slice(0, 8)}; PUT ${res.status})`);
    }
  }
  // The answer, not just the request: the rows re-render from it.
  const asked = page.waitForResponse((r) => r.url().includes("/revisions") && r.url().includes("revset=%40"), { timeout: ms(30000) }).then(() => true).catch(() => false);
  await page.fill(".revset-bar input", "@");
  await page.click(".revset-bar button[type=submit]");
  check(await asked, "the history revset field queries /revisions?revset=@");
  await frames();
  const narrowed = await page.evaluate(() => document.querySelectorAll(".hist li").length);
  check(narrowed >= 1 && narrowed <= n, `revset @ narrows the history (${narrowed} of ${n})`);
  await shot("p-history-revset");
  await page.click(".revset-bar button:has-text('Reset')");
  await page.waitForFunction((n) => document.querySelectorAll(".hist li").length === n, n, { timeout: ms(30000) }).catch(() => {});
  // The interdiff: from a "changed since review" chip when the server marks one, else opened directly
  // for the working copy against its parent (the server's review hook may not be wired yet).
  const chip = await page.$(".hist .chip.csr");
  if (chip) await chip.click();
  else
    // No chip: a change with files of its own against its parent, so the interdiff has something to show.
    await page.evaluate(async () => {
      const r = await (await fetch("/api/v1/revisions?limit=20")).json();
      const row = r.rows.find((x) => x.rev.parents.length && !x.rev.flags.immutable && !x.rev.flags.empty) ?? r.rows.find((x) => x.rev.parents.length) ?? r.rows[0];
      window.__cv.openInterdiff({ change: row.rev.change, from: row.rev.parents[0], title: "e2e" });
    });
  await page.waitForSelector(".modal.interdiff", { timeout: ms(30000) });
  // Answered: file sections, the "no file changes" note, or the "unavailable" warning.
  await page.waitForFunction(() => document.querySelector(".modal.interdiff .idiff section, .modal.interdiff .warn-box, .modal.interdiff .muted.small:not(:empty)") && !document.querySelector(".modal.interdiff")?.textContent?.includes("loading…"), null, { timeout: ms(30000) }).catch(() => {});
  const idiff = await page.evaluate(() => ({
    files: document.querySelectorAll(".modal.interdiff .idiff section").length,
    hunks: document.querySelectorAll(".modal.interdiff .idiff-hunk").length,
    unavailable: document.querySelector(".modal.interdiff .warn-box")?.textContent?.slice(0, 120),
  }));
  // Both servers serve GET /interdiff: "unavailable" is a failure, and so is an empty diff (the change
  // was marked reviewed at its parent, or picked for having files).
  check(!idiff.unavailable && idiff.files > 0 && idiff.hunks > 0, `${chip ? "“changed since review” opens" : "opening"} the interdiff viewer with the changes (${idiff.unavailable ?? `${idiff.files} file(s), ${idiff.hunks} hunk(s)`})`);
  await shot("p-interdiff");
  await page.keyboard.press("Escape");
  check(!(await page.$(".modal.interdiff")), "Escape closes the interdiff");
}

// A whole-view invalidation (view.invalidated, reason "index", no windows and no tiles: e.g. channel
// hubs re-settled after an op) reloads the view in place: the spec is re-POSTed, the camera stays,
// and what the server now serves (the new hub) is on the map. An empty window list reloads nothing.
// With REPO the event is real: a new bus topic with a producer and a consumer, snapshotted by jj, so
// the background settle finds a new hub. On cv-mock (which never sends one) it is delivered to the
// tab's event handlers as the socket would.
await ensureCode(); // windows loaded around the camera, to see them come back after the reload
{
  const posts = [];
  const onReq = (r) => r.method() === "POST" && /\/api\/v1\/views$/.test(r.url()) && posts.push(Date.now());
  page.on("request", onReq);
  await page.evaluate(() => {
    const got = (window.__inval = []);
    window.__cv.live.on((ev) => ev.type === "view.invalidated" && got.push({ ...ev, at: Date.now() }));
  });
  const cam0 = await page.evaluate(() => { const c = window.__cv.engine.cam; return [c.x, c.y, c.zoom]; });
  const viewId = await page.evaluate(() => window.__cv.engine.client.view.id);
  // What a reload replaces: the client's view generation and the loaded window objects.
  const before = await page.evaluate(() => { const c = window.__cv.engine.client; window.__preReload = { gen: c.gen, wins: new Set(c.windows.values()) }; return { gen: c.gen, windows: c.windows.size }; });
  const inject = (ev) => page.evaluate((ev) => { for (const h of window.__cv.live["handlers"]) h(ev); }, ev);
  // An empty list means nothing: no reload.
  await inject({ type: "view.invalidated", viewId, layoutVersion: 0, reason: "index", windows: [] });
  // The handler is synchronous (App.tsx → invalidationAction): a reload would have started by now.
  await frames(page, 3);
  await idle();
  const genAfterEmpty = await page.evaluate(() => window.__cv.engine.client.gen);
  check(posts.length === 0 && genAfterEmpty === before.gen, `an "index" invalidation with an empty window list reloads nothing (${posts.length} POST /views, generation ${before.gen} → ${genAfterEmpty})`);
  let topic = "";
  if (REPO) {
    topic = `world.e2e${RUN}.ping`;
    const file = join(REPO, `e2e_hub_${RUN}.py`);
    writeFileSync(file, `def start(bus):\n    bus.on("${topic}", on_ping)\n\n\ndef ping(bus):\n    bus.emit("${topic}", {"n": 1})\n\n\ndef on_ping(payload):\n    return payload\n`);
    jj("status"); // snapshots it: one op that changes @
  } else {
    await inject({ type: "view.invalidated", viewId, layoutVersion: 0, reason: "index" });
  }
  const ev = await until(() => window.__inval.find((e) => e.reason === "index" && !e.windows && !e.tiles?.length) ?? null, undefined, REPO ? 60000 : 30000);
  // Reloaded: the client switched to a new generation, the windows were dropped with the old layout
  // (none of the old window objects is left) and the tiles around the camera came back.
  const reloaded = await until(() => {
    const c = window.__cv.engine.client, pre = window.__preReload;
    return c.gen > pre.gen && c.windows.size > 0 && window.__cv.engine.stats().pendingRequests === 0 && ![...c.windows.values()].some((w) => pre.wins.has(w));
  }, undefined, 60000);
  await idle();
  const after = await page.evaluate((topic) => {
    const e = window.__cv.engine;
    return { cam: [e.cam.x, e.cam.y, e.cam.zoom], windows: e.client.windows.size, view: e.client.view.id, hub: !topic || e.client.boot.hubs.some((h) => [h.key, h.display, h.id].some((x) => (x ?? "").includes(topic))) };
  }, topic);
  const posted = ev ? posts.filter((t) => t >= ev.at - 50).length : 0;
  const kept = Math.abs(after.cam[0] - cam0[0]) < 1 && Math.abs(after.cam[1] - cam0[1]) < 1 && Math.abs(after.cam[2] - cam0[2]) < 1e-6;
  check(!!ev && posted >= 1 && !!reloaded && kept && after.windows > 0 && before.windows > 0, `a whole-view "index" invalidation reloads the view in place (${ev ? `event ${REPO ? "from the server" : "delivered"}` : "no event"}, ${posted} POST /views, ${reloaded ? "windows dropped and re-fetched" : "windows NOT re-fetched"}: ${before.windows} → ${after.windows}, camera ${kept ? "kept" : `moved ${JSON.stringify(cam0)} → ${JSON.stringify(after.cam)}`})`);
  if (REPO) check(after.hub, `…and the reloaded bootstrap has the new hub (${topic})`);
  page.off("request", onReq);
}

// Lenses (§4.9) change emphasis only, never layout: heat glows under hot windows and fades cold ones;
// connectedness fades only Isolated windows and raises central windows; hotspots keeps review-first
// windows; co-change lights a file's partners and gathers them into a tray. Links whose ends a lens
// fades step back with them. Checked on what the chunk builds drew (glow quads, shadow offsets,
// faded links), at map zoom and at L0.
await ensureCode();
{
  const before = await page.evaluate(() => Object.fromEntries([...window.__cv.engine.client.windows.values()].map((w) => [w.id, `${w.rect.x},${w.rect.y}`])));
  // Frame a hot code cluster at map zoom with central windows in it.
  await page.evaluate(() => {
    const e = window.__cv.engine;
    const code = e.client.boot.clusters.filter((c) => c.lane === "code" && c.windows > 2).sort((a, z) => z.heat - a.heat || z.conn - a.conn || z.windows - a.windows);
    const c = code[0];
    e.cam.animateTo(c.rect.x + c.rect.w / 2 - e.cam.width / 2 / 0.06, c.rect.y + c.rect.h / 2 - e.cam.height / 2 / 0.06, 0.06, 0);
  });
  await idle();
  const lensState = () => page.evaluate(() => {
    const e = window.__cv.engine;
    const vis = [...e["visible"]];
    const sum = (k) => vis.reduce((a, c) => a + c.stats[k], 0);
    const max = (k) => vis.reduce((a, c) => Math.max(a, c.stats[k]), 0);
    const ws = vis.flatMap((c) => c.windows).filter((w) => !w.flags.minimized);
    const look = e["look"];
    return {
      lens: look.lens, tier: e.cam.tier,
      glows: sum("glows"), faded: sum("faded"), raised: sum("raised"), elev: max("maxElevPx"), edgePx: max("maxEdgePx"), fadedLinks: sum("fadedLinks"),
      n: ws.length, hot: ws.filter((w) => w.heat.grade >= 3).length, cold: ws.filter((w) => w.heat.grade <= 1).length,
      iso: ws.filter((w) => w.conn.grade === 0).length, low: ws.filter((w) => w.conn.grade === 1).length, central: ws.filter((w) => w.conn.grade >= 2).length,
      notFirst: ws.filter((w) => !w.reviewFirst).length,
      highwayPx: e["bundles"].maxWidthPx, highwaysFaded: e["bundles"].fadedCount, highwaysBuilding: e["bundles"].building, l0: e["backdrop"].lensStats,
    };
  });
  // The lens is applied and every chunk and highway rebuilt for it (idle covers bundles.building).
  const LENS = { None: "none", Depth: "depth", Tension: "tension", Heat: "heat", Depends: "connectedness", "Review priority": "hotspots", "Co-change": "cochange" };
  // None and Tension show the folder tree, Depth the tree ordered by links, the rest the district
  // grid (structure.ts layoutFor).
  const layoutOf = (lens) => (lens === "depth" ? "flow" : lens === "none" || lens === "tension" ? "folders" : "districts");
  const layoutNow = () => page.evaluate(() => window.__cv.engine.client.view.spec.options?.layout ?? "districts");
  const settle = async (want) => {
    if (want) {
      await until((l) => window.__cv.engine["look"].lens === l, want);
      // The snapshot re-opens in the lens's layout: wait for that switch, not just the lens. (The
      // mock has one synthetic map, whatever the layout: cv serve only.)
      const layout = layoutOf(want);
      if (REPO) await until((l) => { const e = window.__cv.engine; return (e.client.view.spec.options?.layout ?? "districts") === l && !e.stats().pendingSwitch; }, layout, 120000).catch(() => {});
    }
    await idle();
  };
  const pick = async (label) => {
    await page.click(`.seg[aria-label=Lens] button:has-text('${label}')`);
    await settle(LENS[label]);
    return lensState();
  };
  const none = await pick("None");
  check(none.lens === "none" && none.faded === 0 && none.glows === 0 && none.n > 0, `no lens: nothing faded or glowing (${none.n} windows at ${none.tier})`);
  const tree = await page.evaluate(() => {
    const b = window.__cv.engine.client.boot;
    return { frames: b.folders?.length ?? 0, columns: b.columns.length, folder: b.clusters.every((c) => c.folder !== undefined) };
  });
  if (REPO) check((await layoutNow()) === "folders" && tree.frames > 0 && tree.columns === 0 && tree.folder, `None shows the folder tree (${tree.frames} folder frames, every cluster a directory)`);
  else skip("folder tree: the mock draws one synthetic map");
  await until(() => !!document.querySelector(".structure-chip"), undefined, 60000).catch(() => {});
  const chip = await page.evaluate(() => document.querySelector(".structure-chip")?.textContent ?? "");
  check(/outside \d+% · tension \d+%/.test(chip), `the top bar shows the folder tree's structure indices (${chip})`);
  for (const label of ["Depth", "Tension"]) {
    const st = await pick(label);
    await until(() => !!window.__cv.engine["structureView"], undefined, 60000).catch(() => {});
    const loaded = await page.evaluate(() => window.__cv.engine["structureView"] === window.__cv.engine.client.view.id);
    await shot(`p-lens-${label.toLowerCase()}`);
    // Per-window structure: windows of the loaded tiles with an entry (links or tension).
    const perWindow = await page.evaluate(async () => {
      const e = window.__cv.engine;
      const s = await (await fetch(`/api/v1/views/${e.client.view.id}/structure`)).json();
      const keys = new Set(s.windows.map((w) => w.key));
      return [...e.client.windows.values()].filter((w) => keys.has(w.key)).length;
    });
    check(st.lens === LENS[label] && (!REPO || (await layoutNow()) === layoutOf(LENS[label])) && loaded && st.faded <= st.n && perWindow > 0, `lens ${label} shows its folder tree (${layoutOf(LENS[label])}) and reads its structure (${st.faded} of ${st.n} windows faded, ${perWindow} loaded windows with per-window structure)`);
  }
  const heat = await pick("Heat");
  await shot("p-lens-heat");
  check(heat.lens === "heat" && (!REPO || (await layoutNow()) === "districts"), `lens Heat switches the canvas lens and the snapshot to the district grid (${heat.lens})`);
  if (heat.hot) check(heat.glows === heat.hot, `heat lens: a thermal glow under each of the ${heat.hot} hot windows (${heat.glows} glows)`);
  else skip("heat glow check: no hot window in view");
  check(heat.faded === heat.cold, `heat lens fades the ${heat.cold} cold windows (${heat.faded} faded)`);
  // (A small repo's cold windows can have no drawn link at all: then there is nothing to step back.)
  if (heat.cold > 0 && heat.fadedLinks === 0) skip(`heat lens link fade: no drawn link touches the ${heat.cold} cold windows`);
  else check(none.fadedLinks === 0 && (heat.cold === 0 || heat.fadedLinks > 0), `heat lens steps back the links of faded windows (${heat.fadedLinks} links, none without a lens)`);
  const conn = await pick("Depends");
  await shot("p-lens-connectedness");
  check(conn.lens === "connectedness", `lens Depends switches the canvas lens (${conn.lens})`);
  check(conn.faded === conn.iso, `connectedness lens fades only the ${conn.iso} Isolated windows, not the ${conn.low} Low ones (${conn.faded} faded)`);
  if (conn.central) check(conn.raised === conn.central && conn.elev > none.elev * 1.5, `connectedness lens raises the ${conn.central} central windows (shadow offset ${none.elev.toFixed(1)} → ${conn.elev.toFixed(1)} px)`);
  else skip("elevation check: no central window in view");
  // Coupling widths were dropped: every window pair has one link per kind, so they were constant.
  // (Against Heat, on the same district grid: None shows the folder tree, other windows in view.)
  check(Math.abs(conn.edgePx - heat.edgePx) < 1e-6 && Math.abs(conn.highwayPx - heat.highwayPx) < 1e-6, `connectedness lens keeps link and highway widths (links ${conn.edgePx.toFixed(1)} px, highways ${conn.highwayPx.toFixed(1)} px)`);
  const hot = await pick("Review priority");
  check(hot.lens === "hotspots" && hot.faded === hot.notFirst, `hotspots lens keeps only review-first windows at full strength (${hot.n - hot.notFirst} kept, ${hot.faded} faded)`);
  const co = await pick("Co-change");
  await until(() => !!window.__cv.engine["cochange"], undefined, 60000);
  await settle();
  // The pairs, the first with both files' windows loaded, and what selecting its file draws.
  const firstPair = () => page.evaluate(() => {
    const e = window.__cv.engine;
    const co = e["cochange"];
    const pairs = [];
    for (const [a, row] of co?.files ?? []) for (const b of row.keys()) if (a < b) pairs.push([a, b]);
    const loaded = pairs.find(([a, b]) => e.client.windowsByPath.get(a)?.length && e.client.windowsByPath.get(b)?.length);
    return { pairs: pairs.length, loaded, first: pairs[0] };
  });
  let coState = await firstPair();
  // Not vacuous: the server must name pairs (the CI repo has files committed together on purpose),
  // and with none loaded around the view, fly to the first pair's files and look again.
  if (coState.pairs > 0 && !coState.loaded) {
    await page.evaluate(async ([a, b]) => {
      const e = window.__cv.engine;
      const ra = (await e.locate(a))[0], rb = (await e.locate(b))[0];
      if (!ra || !rb) return;
      const x0 = Math.min(ra.x, rb.x), y0 = Math.min(ra.y, rb.y), x1 = Math.max(ra.x + ra.w, rb.x + rb.w), y1 = Math.max(ra.y + ra.h, rb.y + rb.h);
      // Both files on screen, centred (frameRect reads a tall rect from its top instead), at a
      // zoom where tiles load (≥ 0.03).
      const c = e.cam, cx = (x0 + x1) / 2, cy = (y0 + y1) / 2;
      const z = Math.max(0.03, Math.min(0.6, (0.9 * c.width) / (x1 - x0), (0.9 * c.height) / (y1 - y0)));
      c.animateTo(cx - c.width / 2 / z, cy - c.height / 2 / z, z, 0);
    }, coState.first);
    // Both files' windows loaded (a condition, not a delay).
    await until(() => {
      const e = window.__cv.engine;
      for (const [a, row] of e["cochange"]?.files ?? []) for (const b of row.keys()) if (e.client.windowsByPath.get(a)?.length && e.client.windowsByPath.get(b)?.length) return true;
      return false;
    }, undefined, 60000);
    await settle();
    coState = await firstPair();
  }
  // Selecting one file of the pair lights up its partners (glows, rings, an arc each).
  await page.evaluate(([a]) => {
    const e = window.__cv.engine;
    e.select({ type: "window", w: e.client.windowsByPath.get(a)[0] });
  }, coState.loaded ?? coState.first);
  await until(() => window.__cv.engine.cochangeStats.partners > 0, undefined, 10000).catch(() => {});
  await settle();
  const lit = await page.evaluate(() => ({ ...window.__cv.engine.cochangeStats, drawn: window.__cv.engine["cochangeLayer"].children.length }));
  await shot("p-lens-cochange");
  check(co.lens === "cochange" && coState.pairs > 0 && !!coState.loaded && lit.partners > 0 && lit.drawn > 0, `co-change lens lights a selected file's partners (${coState.pairs} pairs; ${lit.partners} partners, ${lit.arcs} arcs, ${lit.offscreen} off-screen)`);
  // Gather (G): the partners fly into a tray beside it; Esc puts them back. The layout never moves.
  await page.keyboard.press("g");
  await until(() => window.__cv.engine.gatherStats.state === "held", undefined, 5000).catch(() => {});
  const g = await page.evaluate(() => window.__cv.engine.gatherStats);
  await shot("p-cochange-gather");
  await page.keyboard.press("Escape");
  await until(() => window.__cv.engine.gatherStats.state === "idle", undefined, 5000).catch(() => {});
  const gEnd = await page.evaluate(() => window.__cv.engine.gatherStats.state);
  check(g.state === "held" && g.pinned && g.shown > 0 && g.shown <= 12 && g.covers > 0 && gEnd === "idle", `G gathers the ${g.shown} partners into a tray (${g.side}, ${g.covers} spots covered, +${g.more} more, built in ${g.buildMs.toFixed(1)} ms); Esc releases (${gEnd})`);
  await page.evaluate(() => window.__cv.engine.select(undefined));
  // L0: the cluster map follows the lens (glows under hot clusters, long shadows under central ones).
  await page.evaluate(() => { const e = window.__cv.engine; e.cam.animateTo(e.cam.x, e.cam.y, 0.012, 0); });
  const l0heat = await pick("Heat");
  await shot("p-lens-heat-l0");
  const l0conn = await pick("Depends");
  const cl = await page.evaluate(() => {
    const e = window.__cv.engine;
    const b = e.client.boot;
    const hidden = e["look"].hiddenLanes;
    const cs = b.clusters.filter((c) => !hidden?.has(c.lane) && (b.view.spec.mode !== "range" || c.windows > 0));
    const level = (c) => c.heatLevel ?? (c.heat + 0.5) / 5;
    // Depends at L0: clusters with solid links in or out (links from the tests lane left out).
    const lane = new Map(b.clusters.map((c) => [c.id, c.lane]));
    const flow = new Set();
    for (const x of b.bundles) if (lane.get(x.from) !== "test") flow.add(x.from).add(x.to);
    return {
      hot: cs.filter((c) => level(c) >= 0.6).length,
      cold: cs.filter((c) => level(c) < 0.4).length,
      central: cs.filter((c) => c.conn >= 2 && flow.has(c.id)).length,
      noFlow: cs.filter((c) => !flow.has(c.id)).length,
    };
  });
  check(l0heat.tier === "L0" && l0heat.l0.glows === cl.hot && l0heat.l0.faded === cl.cold && cl.hot > 0, `L0 heat lens: glows under the ${l0heat.l0.glows} warm and hot clusters, the ${l0heat.l0.faded} cold ones faded`);
  check(l0conn.l0.raised === cl.central && l0conn.l0.faded === cl.noFlow && cl.central > 0, `L0 Depends lens: the ${l0conn.l0.raised} central clusters raised, the ${l0conn.l0.faded} with no solid links in or out faded`);
  await pick("None");
  // Every window loaded before and after the lens switches sits where it was.
  const moved = await page.evaluate((before) => {
    let same = 0, moved = 0;
    for (const w of window.__cv.engine.client.windows.values()) {
      const b = before[w.id];
      if (b === undefined) continue;
      if (b === `${w.rect.x},${w.rect.y}`) same++;
      else moved++;
    }
    return { same, moved };
  }, before);
  check(moved.same > 0 && moved.moved === 0, `lenses never move a window (${moved.same} windows compared)`);
}

// F4: the Hotspots brush filters the canvas: windows of clusters outside the brush fade (and their
// clusters at L0); Clear brings them back.
{
  await page.evaluate(() => {
    const e = window.__cv.engine;
    const code = e.client.boot.clusters.filter((c) => c.lane === "code" && c.windows > 2).sort((a, z) => z.heat - a.heat || z.conn - a.conn || z.windows - a.windows);
    const c = code[0];
    e.cam.animateTo(c.rect.x + c.rect.w / 2 - e.cam.width / 2 / 0.05, c.rect.y + c.rect.h / 2 - e.cam.height / 2 / 0.05, 0.05, 0);
  });
  await tab("Hotspots");
  const dots = await page.waitForFunction(() => document.querySelectorAll(".scatter circle, .scatter rect, .scatter path").length > 4, null, { timeout: ms(30000) }).then(() => true).catch(() => false);
  if (!dots) skip("brush check: the Hotspots scatter has no points");
  else {
    const sb = await (await page.$(".scatter")).boundingBox();
    // Brush the hot, central quadrant (review first).
    await page.mouse.move(sb.x + sb.width * 0.45, sb.y + 2);
    await page.mouse.down();
    await page.mouse.move(sb.x + sb.width - 2, sb.y + sb.height * 0.55, { steps: 6 });
    await page.mouse.up();
    await page.waitForFunction(() => window.__cv.engine["look"].brush?.size > 0, null, { timeout: ms(30000) }).catch(() => {});
    await idle();
    const brushed = () => page.evaluate(() => {
      const e = window.__cv.engine;
      const b = e["look"].brush;
      const vis = [...e["visible"]];
      const ws = vis.flatMap((c) => c.windows).filter((w) => !w.flags.minimized);
      return { size: b?.size ?? 0, faded: vis.reduce((a, c) => a + c.stats.faded, 0), outside: b ? ws.filter((w) => !b.has(w.cluster)).length : 0, n: ws.length, l0: e["backdrop"].lensStats.faded };
    });
    const on = await brushed();
    await shot("p-hotspots-brush");
    check(on.size > 0 && on.faded === on.outside && on.outside > 0 && on.l0 > 0, `the Hotspots brush filters the canvas: ${on.size} clusters brushed, the ${on.outside} windows outside fade (${on.faded} faded; ${on.l0} clusters faded at L0)`);
    await page.click(".side button:has-text('Clear')");
    await idle();
    const off = await brushed();
    check(off.size === 0 && off.faded === 0, `clearing the brush brings every window back (${off.faded} faded)`);
  }
  await tab("Inspector");
}

// Port pills never overlap at L2/L3 (N14): pills on the same or neighbouring lines are stacked (with a
// leader to their line) or merged into "×N". District glyphs read at L2 (sized in screen pixels).
// Low-confidence links carry "?" (§5 Edges).
{
  const portCheck = async (z) => {
    await page.evaluate((z) => {
      const e = window.__cv.engine;
      const code = e.client.boot.clusters.filter((c) => c.lane === "code" && c.windows > 2).sort((a, b) => b.heat - a.heat || b.conn - a.conn || b.windows - a.windows);
      const c = code[0];
      e.cam.animateTo(c.rect.x + 200 - 300 / z, c.rect.y + 300 - 200 / z, z, 0);
    }, z);
    await idle();
    return page.evaluate(() => {
      const e = window.__cv.engine;
      let overlaps = 0, pills = 0, stacked = 0, merged = 0, low = 0, lowMarks = 0;
      const bad = [];
      for (const c of e["visible"]) {
        pills += c.stats.pills;
        stacked += c.stats.stacked;
        merged += c.stats.merged;
        const by = new Map();
        for (const p of c.portHits) {
          const k = p.window.id + p.side;
          if (!by.has(k)) by.set(k, []);
          by.get(k).push(p);
        }
        for (const l of by.values()) {
          l.sort((a, b) => a.y - b.y);
          for (let i = 1; i < l.length; i++) if (l[i].y < l[i - 1].y + l[i - 1].h - 0.01) (overlaps++, bad.push(`${l[i - 1].label}/${l[i].label}`));
        }
      }
      for (const c of e["visibleEdges"]) {
        if (c.dirty.has("edges")) continue;
        lowMarks += c.stats.lowMarks;
        low += c.edges.filter((x) => x.confidence < 0.6).length;
      }
      return { tier: e.cam.tier, overlaps, pills, stacked, merged, bad: bad.slice(0, 3), low, lowMarks, glyphs: e.glyphStats };
    });
  };
  const l2 = await portCheck(0.16);
  await shot("p-ports-l2");
  check(l2.tier === "L2" && l2.pills > 0 && l2.overlaps === 0, `L2: ${l2.pills} port pills, none overlapping (${l2.stacked} stacked, ${l2.merged} merged into ×N)${l2.bad.length ? ` ${l2.bad.join(", ")}` : ""}`);
  check(l2.glyphs.count > 0 && l2.glyphs.px >= 12, `L2: district glyphs at ${l2.glyphs.px.toFixed(0)} px on screen (${l2.glyphs.count} windows)`);
  // …and they cover neither another window nor a cluster label (as the backdrop draws it).
  {
    const g = await page.evaluate(() => {
      const e = window.__cv.engine;
      const hit = (a, b) => a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
      const bands = e.clusterLabelRects(e.cam.view()).map((r) => ({ label: r.label, r }));
      const ws = [...e.client.windows.values()].filter((w) => !w.flags.minimized);
      const bad = [];
      for (const r of e.glyphRects) {
        const l = bands.find((b) => hit(b.r, r));
        const w = ws.find((w) => w.id !== r.id && hit(w.rect, r));
        if (l || w) bad.push(l ? `label "${l.label}"` : `window ${w.symbol?.name ?? w.path}`);
      }
      return { n: e.glyphRects.length, bad, moved: e.glyphStats.moved };
    });
    check(g.n > 0 && g.bad.length === 0, `L2: district glyphs cover no cluster label or other window (${g.n} glyphs, ${g.moved} moved off the corner spot)${g.bad.length ? `: ${g.bad.slice(0, 3).join(", ")}` : ""}`);
  }
  if (l2.low) check(l2.lowMarks === l2.low, `low-confidence links carry “?” (${l2.lowMarks} of ${l2.low})`);
  else skip("“?” check: no low-confidence link in view");
  const l3 = await portCheck(0.7);
  check(l3.tier === "L3" && l3.overlaps === 0, `L3: ${l3.pills} port pills, none overlapping (${l3.stacked} stacked, ${l3.merged} merged)${l3.bad.length ? ` ${l3.bad.join(", ")}` : ""}`);
  await shot("p-ports-l3");
}

// F3: text crossfades over the silhouette (it fades in while the silhouette under it stays drawn).
{
  const id = await page.evaluate(() => {
    const e = window.__cv.engine;
    const v = e.cam.view();
    const w = [...e.client.windows.values()].find((w) => w.lane === "code" && !w.flags.minimized && w.lines.end - w.lines.start > 8 && w.rect.x > v.x && w.rect.y > v.y && w.rect.x < v.x + v.w && w.rect.y < v.y + v.h) ?? [...e.client.windows.values()].find((w) => w.lane === "code" && !w.flags.minimized && w.lines.end - w.lines.start > 8);
    e.cam.animateTo(w.rect.x - 300, w.rect.y - 100, 0.8, 0);
    return w.id;
  });
  await idle();
  const fade = await page.evaluate(async (id) => {
    const e = window.__cv.engine;
    const w = e.client.windows.get(id);
    // Forget the window's text mesh and cached text, so it loads and fades in again.
    const old = e["code"].get(id);
    if (old) {
      e["code"].delete(id);
      old.c.parent?.removeChild(old.c);
      old.c.destroy({ children: true });
    }
    e.client.texts.delete(e.client.textKey(w));
    e.cam.animateTo(e.cam.x + 0.01, e.cam.y, e.cam.zoom, 0);
    const seen = [];
    const at = [];
    let silUnder = true;
    await new Promise((res) => {
      const t0 = performance.now();
      const tick = () => {
        const x = e["code"].get(id);
        if (x) {
          seen.push(x.c.alpha);
          at.push(performance.now());
          if (x.c.alpha < 1) silUnder = silUnder && x.chunk.parts.sil.visible && x.chunk.parts.sil.children.length > 0;
        }
        if ((x && x.c.alpha >= 1) || performance.now() - t0 > 30000) res();
        else requestAnimationFrame(tick);
      };
      tick();
    });
    const dt = at.slice(1).map((t, i) => t - at[i]).sort((a, b) => a - b);
    return { frames: seen.length, mid: seen.filter((a) => a > 0 && a < 1).length, first: seen[0], last: seen[seen.length - 1], silUnder, dt: dt[dt.length >> 1] ?? 0 };
  }, id);
  // The fade lasts 150 ms: a renderer slower than ~50 ms a frame (SwiftShader on a busy CI runner) can
  // step over it, so the frames in between are only required when frames come fast enough to land there.
  const slow = fade.dt > 50;
  check(fade.frames > 0 && fade.first < 0.5 && (fade.mid >= 2 || slow) && fade.last === 1 && fade.silUnder, `L3 text crossfades over the silhouette (${fade.mid} frames between 0 and 1, from ${fade.first?.toFixed(2)} to ${fade.last}${slow ? `; ${fade.dt.toFixed(0)} ms frames, too slow to require frames mid-fade` : ""})`);
}

// F2: the data client asks only for what is near the predicted view, aborts what leaves it, and an
// invalidation refetches only what changed.
{
  await page.evaluate(() => {
    const e = window.__cv.engine;
    const L = e.client.loader;
    const f2 = (window.__f2 = { log: [], cancels: 0, aborted: 0 });
    const orig = L.request.bind(L);
    L.request = (job) => {
      const v = e.cam.view();
      f2.log.push({ at: performance.now(), r: job.region, v, kind: job.kind, key: job.key });
      return orig(job);
    };
    const w = L["worker"];
    const post = w.postMessage.bind(w);
    w.postMessage = (m, t) => {
      if (m?.type === "cancel") f2.cancels++;
      return post(m, t);
    };
    const onmsg = w.onmessage;
    w.onmessage = (ev) => {
      if (ev.data?.type === "error" && ev.data.aborted) f2.aborted++;
      return onmsg(ev);
    };
  });
  // Fast flings at map and outline zoom over a slow network: requests stay near the view, and the
  // ones the camera leaves behind are aborted in flight.
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Network.enable");
  await cdp.send("Network.emulateNetworkConditions", { offline: false, latency: 250, downloadThroughput: 4e6, uploadThroughput: 4e6 });
  for (const sc of ["fling-l1", "fling-l2"]) await page.evaluate((sc) => window.__cv.bench(sc), sc);
  // Then jumps across the map while tiles are in flight (slower network): what was asked for the
  // place just left must be aborted, not completed.
  await cdp.send("Network.emulateNetworkConditions", { offline: false, latency: 900, downloadThroughput: 2e6, uploadThroughput: 2e6 });
  await page.evaluate(async () => {
    const e = window.__cv.engine;
    const cs = e.client.boot.clusters.filter((c) => c.windows > 3);
    for (let i = 0; i < 8; i++) {
      const c = cs[(i * 7919 + 13) % cs.length];
      e.cam.animateTo(c.rect.x + c.rect.w / 2 - e.cam.width / 2 / 0.06, c.rect.y + c.rect.h / 2 - e.cam.height / 2 / 0.06, 0.06, 0);
      await new Promise((r) => setTimeout(r, 400));
    }
  });
  // Everything asked for has finished or been aborted (still on the slow network).
  await until(() => window.__cv.engine.client.loader.pending === 0, undefined, 60000);
  await cdp.send("Network.emulateNetworkConditions", { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1 });
  await idle();
  const f2 = await page.evaluate(() => {
    const { log, cancels, aborted } = window.__f2;
    const hit = (a, b) => a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
    // Viewport + margin: the loader's keep region, 1.5 screens around the view.
    const out = log.filter((x) => !hit(x.r, { x: x.v.x - 1.5 * x.v.w, y: x.v.y - 1.5 * x.v.h, w: 4 * x.v.w, h: 4 * x.v.h }));
    return { n: log.length, out: out.length, sample: out.slice(0, 2).map((x) => x.key), cancels, aborted, queuedCancels: window.__cv.engine.client.loader.stats.cancelled };
  });
  const big = await page.evaluate(() => window.__cv.engine.client.view.stats.windows >= 2000);
  // A small map (the cv serve fixture) is fully loaded before the flings: they need no tile at all.
  if (!big && f2.n === 0) skip("F2 request check: the map is small and was fully loaded before the flings (0 requests)");
  else check(f2.n > 0 && f2.out === 0, `F2: no request outside the viewport + margin (${f2.n} requests during flings${f2.out ? `; ${f2.out} outside: ${f2.sample.join(", ")}` : ""})`);
  if (!big && !f2.cancels && !f2.queuedCancels) skip(`abort check: the map is small enough that nothing was left in flight (${f2.n} requests)`);
  else check(f2.cancels > 0 && f2.aborted > 0 && f2.aborted <= f2.cancels, `F2: requests the camera leaves are aborted in flight (${f2.cancels} cancelled while running, ${f2.aborted} fetches aborted; ${f2.queuedCancels} dropped from the queue)`);
}

// Live refreshes: an edit by another client (edit.updated + view.invalidated) reloads only the edited
// file's windows; review.submitted from another client reloads the threads.
await ensureCode();
{
  const w = await page.evaluate(() => {
    const e = window.__cv.engine;
    const w = [...e.client.windows.values()].find((w) => w.lane === "code" && !w.flags.minimized && w.lines.end - w.lines.start > 8 && !e.review.conflicts.has(w.path));
    e.select(undefined);
    e.cam.animateTo(w.rect.x - 300, w.rect.y - 100, 0.8, 0);
    return { id: w.id, path: w.path, change: w.change, blob: w.blob, start: w.lines.start, rect: w.rect };
  });
  await idle();
  await page.evaluate(() => {
    const inv = (window.__inv = { events: [], fetches: [] });
    window.__cv.live.on((ev) => (ev.type === "view.invalidated" || ev.type === "edit.updated") && inv.events.push({ at: performance.now(), type: ev.type, windows: ev.windows, reason: ev.reason }));
    const f = window.fetch;
    window.fetch = (input, init) => {
      inv.fetches.push({ at: performance.now(), url: String(input instanceof Request ? input.url : input) });
      return f(input, init);
    };
  });
  const line = (await api("GET", `/api/v1/blobs/${w.blob}/text?from=${w.start}&to=${w.start}`)).json?.lines?.[0] ?? "";
  const body = { target: w.change, mode: "amend", files: [{ path: w.path, patches: [{ start: w.start, end: w.start, text: `${line}\n# e2e: edited by another client ${RUN}` }] }] };
  const made = await other("POST", "/api/v1/edits", body);
  const applied = made.json?.id && made.json.status !== "applied" ? await other("POST", `/api/v1/edits/${made.json.id}/apply`) : made;
  const ok = applied.status === 200 && applied.json?.status === "applied";
  check(ok, `an edit by another client applied (${applied.status} ${applied.json?.status ?? JSON.stringify(applied.json)?.slice(0, 120)})`);
  if (ok) {
    // edit.updated: the edited window gets its new blob and text.
    const fresh = await until(([id, run]) => {
      const e = window.__cv.engine;
      const x = e.client.windows.get(id);
      const t = x && e.client.texts.get(e.client.textKey(x));
      return !!t && t.lines.some((l) => l.includes(`edited by another client ${run}`)) && !!e["code"].get(id);
    }, [w.id, RUN], 60000);
    check(!!fresh, "edit.updated from another client reloads the edited window's text and redraws it");
    // The invalidation arrived and its refetches are over: nothing pending and no fetch for a
    // while, several polls in a row (instead of a fixed 2.5 s window).
    await until(() => window.__inv.events.some((x) => x.type === "view.invalidated"), undefined, 60000);
    await until(() => {
      const i = window.__inv, s = window.__cv.engine.stats();
      const last = Math.max(0, ...i.fetches.map((f) => f.at));
      const q = s.pendingRequests === 0 && performance.now() - last > 500;
      window.__invQ = q ? (window.__invQ | 0) + 1 : 0;
      return window.__invQ >= 3;
    }, undefined, 60000);
    const r = await page.evaluate(([path, rect]) => {
      const e = window.__cv.engine;
      const inv = window.__inv;
      const evs = inv.events.filter((x) => x.type === "view.invalidated");
      const t0 = Math.min(...inv.events.map((x) => x.at));
      const ids = new Set(evs.flatMap((x) => x.windows ?? []));
      for (const x of e.client.windowsByPath.get(path) ?? []) ids.add(x.id);
      // What may be refetched: base tiles under the invalidated windows near the view (the rest wait
      // until the camera comes near) and the edited file's outlines and text.
      const v = e.cam.view();
      const near = { x: v.x - v.w, y: v.y - v.h, w: 3 * v.w, h: 3 * v.h };
      const hit = (a, b) => a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
      const all = [...ids].map((id) => e.client.windows.get(id)?.rect).filter(Boolean);
      const rects = [rect, ...all.filter((r) => hit(r, near))];
      const TILE = 4096;
      const okTile = (z, x, y) => z === 0 && rects.some((r) => x >= Math.floor(r.x / TILE) && x <= Math.floor((r.x + r.w) / TILE) && y >= Math.floor(r.y / TILE) - 1 && y <= Math.floor((r.y + r.h) / TILE) + 1);
      const tiles = [];
      const bad = [];
      for (const f of inv.fetches.filter((f) => f.at >= t0)) {
        const m = f.url.match(/\/tiles\/(\d+)\/(-?\d+)\/(-?\d+)/);
        if (m) (tiles.push(m.slice(1).join("/")), okTile(+m[1], +m[2], +m[3]) || bad.push(m.slice(1).join("/")));
        if (/\/bootstrap/.test(f.url)) bad.push("bootstrap");
      }
      for (const j of window.__f2.log.filter((j) => j.at >= t0 && j.kind === "tile")) {
        const m = j.key.match(/^t(\d+)\/(-?\d+)\/(-?\d+)/);
        tiles.push(j.key.slice(1));
        if (m && !okTile(+m[1], +m[2], +m[3])) bad.push(j.key);
      }
      const batches = window.__f2.log.filter((j) => j.at >= t0 && j.kind === "batch").length;
      const textJobs = window.__f2.log.filter((j) => j.at >= t0 && j.key.startsWith("x"));
      const texts = textJobs.length;
      const nearPaths = new Set([path, ...[...ids].map((id) => e.client.windows.get(id)).filter((x) => x && hit(x.rect, near)).map((x) => x.path)]);
      let pathWindows = 0;
      // Text loads are for windows of those files (matched by place: a text fetched between the
      // edit and the reindex has an intermediate blob id).
      const pathRects = [];
      for (const p of nearPaths) for (const x of e.client.windowsByPath.get(p) ?? []) (pathWindows++, pathRects.push(x.rect));
      const okText = (j) => pathRects.some((r) => hit(r, j.r));
      const textBad = textJobs.filter((j) => !okText(j)).length;
      bad.push(...textJobs.filter((j) => !okText(j)).slice(0, 2).map((j) => j.key));
      // A whole-view "index" invalidation (no windows, no tiles: the edit moved a bus or HTTP site, so
      // the hubs re-settled) reloads everything by design (see the reload check above).
      const whole = evs.some((x) => x.reason === "index" && x.windows === undefined);
      return { whole, pathWindows, textBad, events: evs.length, reasons: [...new Set(evs.map((x) => x.reason))].join("+"), windows: ids.size, loaded: all.length, nearN: rects.length - 1, tiles: tiles.length, unique: new Set(tiles).size, bad: bad.slice(0, 4), nbad: bad.length, batches, texts, fileWindows: (e.client.windowsByPath.get(path) ?? []).length };
    }, [w.path, w.rect]);
    if (!r.events && !REPO) skip("invalidation check: this server sends no view.invalidated for edits");
    else if (r.whole) check(r.events > 0 && r.textBad === 0, `view.invalidated after an edit that moved a channel site: a whole-view "index" invalidation, so the view reloads (${r.reasons}: ${r.windows} windows; ${r.nbad ? `refetched ${r.bad.join(", ")}` : "nothing else"})`);
    else check(r.events > 0 && r.nbad === 0 && r.textBad === 0, `view.invalidated (${r.reasons}: ${r.windows} windows, ${r.loaded} loaded, ${r.nearN} near the view) refetches only what changed: ${r.unique} base tile(s) under them (${r.tiles} fetches for edit.updated + view.invalidated), ${r.batches} outline batch(es), ${r.texts} text(s)${r.nbad ? `; also ${r.bad.join(", ")}` : ""}`);
  }
  // review.submitted from another client: the threads reload (markers, Review tab) and a notice shows.
  const th = await api("POST", "/api/v1/threads", { anchor: { path: w.path, rev: w.change, start: w.start + 1, end: w.start + 1 }, body: `e2e: please look (${RUN})`, kind: "question" });
  const reload = page.waitForRequest((q) => /\/api\/v1\/threads(\?|$)/.test(q.url()) && q.method() === "GET", { timeout: ms(30000) }).then(() => true).catch(() => false);
  const sub = await other("POST", "/api/v1/reviews:submit", { threads: [th.json?.id], note: `e2e ${RUN}` });
  const toast = await page.waitForSelector(`.toast:has-text("e2e ${RUN}")`, { timeout: ms(30000) }).then(() => true).catch(() => false);
  check(sub.status === 200 && (await reload) && toast, `review.submitted from another client reloads the threads and says so (submit ${sub.status})`);
}

await ensureCode();
// File chips (D14): selecting a split file marks its fragments; ]f / [f move between them; slots shift
// so split files in one cell don't share a colour; everything else dims.
{
  await tab("Inspector");
  const f = await page.evaluate(async () => {
    const e = window.__cv.engine;
    // Prefer a split file whose fragments ended up apart (they get dotted links, D14).
    const gapOf = (a, b) => Math.max(b.x - (a.x + a.w), a.x - (b.x + b.w), b.y - (a.y + a.h), a.y - (b.y + b.h));
    const seen = new Set();
    let w, apart = 0;
    for (const x of e.client.windows.values()) {
      if (!x.fragment || x.fragment.of < 2 || x.flags.minimized || seen.has(x.path) || seen.size >= 20) continue;
      seen.add(x.path);
      const fr = await e.fragmentsOf(x.path);
      const list = [...(fr?.fragments ?? [])].sort((a, b) => a.index - b.index);
      let n = 0;
      for (let i = 1; i < list.length; i++) if (gapOf(list[i - 1].rect, list[i].rect) >= 120) n++;
      if (!w || n > apart) (w = x), (apart = n);
      if (n) break;
    }
    if (!w) return null;
    e.cam.animateTo(w.rect.x - 300, w.rect.y - 200, 0.25, 0);
    e.select({ type: "window", w });
    return { id: w.id, path: w.path, of: w.fragment.of, apart };
  });
  if (!f) console.log("skip file-chip checks: no split file loaded");
  else {
    await page.waitForFunction((p) => window.__cv.engine["fragments"]?.path === p, f.path, { timeout: ms(30000) }).catch(() => {});
    await idle();
    const st = await page.evaluate(() => {
      const e = window.__cv.engine;
      const dim = e["overlay"]["dimLayer"];
      const quads = (c) => (c?.children ?? []).reduce((a, m) => a + (m.geometry ? m.geometry.positions.length / 8 : quads(m)), 0);
      // Expected: the windows around the view that are neither linked to the selection nor the file's.
      const sel = e.selected;
      const keep = new Set([sel.w.id]);
      for (const x of e.edgesOf(sel)) keep.add(x.from).add(x.to);
      const region = e["overlayRegion"];
      const others = region ? e.client.windowsIn(region).filter((w) => !keep.has(w.id) && w.path !== sel.w.path).length : 0;
      return { sel: e["look"].selectedPath, frags: e["fragments"]?.fragments.length ?? 0, links: e.fragLinks, dimmed: quads(dim), others, tier: e.cam.tier };
    });
    check(st.sel === f.path && st.frags >= 2, `selecting a split file loads its ${st.frags} fragments (${f.path.split("/").pop()})`);
    if (f.apart) check(st.links === f.apart, `dotted links join the ${f.apart} fragment pair(s) that ended up apart (${st.links} drawn)`);
    else skip(`dotted fragment links: ${f.path.split("/").pop()}'s fragments sit together`);
    if (st.others) check(st.dimmed === st.others, `selecting a file dims the ${st.others} other windows around it (${st.dimmed} dimmed)`);
    else skip("dimming check: no other window loaded around the selected file");
    // F1: each split file's chip is a colour of the 10-slot palette (its slot); the selected file's
    // chips switch to the brighter palette, and the minimap marks every fragment in that colour.
    const chips = await page.evaluate((path) => {
      const e = window.__cv.engine;
      const pal = e.chipPalette;
      let drawn = 0, selected = 0;
      const bad = [];
      for (const c of e["visible"]) {
        for (const [id, color] of c.stats.chips) {
          const w = e.client.windows.get(id);
          if (!w?.fragment) continue;
          drawn++;
          const sel = w.path === path;
          if (sel) selected++;
          const want = (sel ? pal.hi : pal.rest)[w.fragment.chip];
          if (w.fragment.chip < 0 || w.fragment.chip >= pal.rest.length || color !== want) bad.push(`${w.path.split("/").pop()}#${w.fragment.index}: slot ${w.fragment.chip} drew ${color.toString(16)}`);
        }
      }
      const w = e.client.windowsByPath.get(path)?.[0];
      const marks = e["hud"].fragmentMarks;
      return { drawn, selected, bad: bad.slice(0, 3), slots: pal.rest.length, distinct: new Set(pal.rest).size, distinctHi: new Set(pal.hi).size, hiOff: pal.rest.every((c, i) => c !== pal.hi[i]), marks, want: w?.fragment ? pal.hi[w.fragment.chip] : -1 };
    }, f.path);
    check(chips.slots === 10 && chips.distinct === 10 && chips.distinctHi === 10 && chips.hiOff, `the chip palette has 10 distinct slots, and 10 brighter ones for the selected file`);
    check(chips.drawn > 0 && chips.selected > 0 && chips.bad.length === 0, `file chips wear their slot's colour (${chips.drawn} drawn, ${chips.selected} of the selected file in the bright palette)${chips.bad.length ? `: ${chips.bad.join(", ")}` : ""}`);
    check(chips.marks.n === st.frags && chips.marks.color === chips.want, `the minimap marks all ${st.frags} fragments of the selected file in its chip colour (${chips.marks.n} marks, #${chips.marks.color.toString(16)})`);
    await shot("p-file-chip");
    await page.mouse.move(box0.x + 700, box0.y + 500);
    await page.keyboard.press("]");
    await page.keyboard.press("f");
    const moved = await page.waitForFunction((id) => { const s = window.__cv.engine.selected; return s?.type === "window" && s.w.id !== id && s.w; }, f.id, { timeout: ms(30000) }).then((h) => h.jsonValue()).catch(() => null);
    check(moved && moved.path === f.path, `]f jumps to the next fragment (${moved ? `${moved.fragment.index}/${moved.fragment.of}` : "none"})`);
    await page.keyboard.press("[");
    await page.keyboard.press("f");
    const back = await page.waitForFunction((id) => window.__cv.engine.selected?.w?.id === id, f.id, { timeout: ms(30000) }).then(() => true).catch(() => false);
    check(back, "[f jumps back to the previous fragment");
    const collide = await page.evaluate(() => {
      // Split files near each other (one 4096 wu cell, about a screen at outline zoom) and their slots,
      // as shifted by the client and as the server hashed them.
      const c = window.__cv.engine.client;
      const cells = new Map();
      for (const w of c.windows.values()) {
        if (!w.fragment) continue;
        const k = Math.floor(w.rect.x / 4096) + "," + Math.floor(w.rect.y / 4096);
        if (!cells.has(k)) cells.set(k, new Map());
        cells.get(k).set(w.path, w.fragment.chip);
      }
      let client = 0, server = 0, checked = 0;
      for (const m of cells.values()) {
        if (m.size < 2 || m.size > 10) continue;
        checked++;
        if (new Set(m.values()).size !== m.size) client++;
        if (new Set([...m.keys()].map((p) => c.serverChip.get(p))).size !== m.size) server++;
      }
      return { client, server, checked };
    });
    if (!collide.checked) skip("chip slot check: no cell holds two split files");
    else check(collide.client === 0 || collide.client < collide.server, `chip slots shift apart: ${collide.client} colliding cells of ${collide.checked} (server hash alone: ${collide.server})`);
    await page.evaluate(() => window.__cv.engine.select(undefined));
    await page.waitForFunction(() => window.__cv.engine["hud"].fragmentMarks.n === 0, null, { timeout: ms(30000) }).catch(() => {});
    check(await page.evaluate(() => window.__cv.engine["hud"].fragmentMarks.n === 0), "clearing the selection clears the minimap marks");
  }
}

// WS resume: drop the socket, write while offline, and get the missed event on reconnect.
await ensureCode();
{
  const live = await page.evaluate(() => window.__cv.live.status);
  await page.evaluate(() => window.__cv.live["ws"].close());
  await page.waitForFunction(() => window.__cv.live.status !== "live", null, { timeout: ms(30000) }).catch(() => {});
  const id = await page.evaluate(async () => {
    const e = window.__cv.engine;
    const w = [...e.client.windows.values()].find((w) => w.lane === "code" && !w.flags.minimized);
    const r = await fetch("/api/v1/threads", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ anchor: { path: w.path, rev: w.change, start: w.lines.start, end: w.lines.start }, body: "written while the socket was down", kind: "comment" }) });
    return (await r.json()).id;
  });
  await page.waitForFunction(() => window.__cv.live.status === "live", null, { timeout: ms(30000) }).catch(() => {});
  const got = await page.waitForFunction((id) => window.__cv.engine.review.threads.has(id), id, { timeout: ms(30000) }).then(() => true).catch(() => false);
  check(live === "live" && got, "after a reconnect the event stream resumes from its cursor (missed thread.upserted arrives)");
}

// focus.request while not following the agent: a suggestion with "Go" instead of a camera jump.
// The target is the loaded code window farthest from the screen, so "Go" has to fly: the first window
// of the map can be the one an earlier step already framed (then the camera has nowhere to go, as seen
// on a large repo). Arrived: the flight is over, the target is selected and its middle is on screen.
await ensureCode();
{
  await idle();
  const target = await page.evaluate(() => {
    const e = window.__cv.engine;
    const v = e.cam.view();
    // The loaded code window farthest from the middle of the screen.
    const d = (r) => Math.hypot(r.x + r.w / 2 - (v.x + v.w / 2), r.y + r.h / 2 - (v.y + v.h / 2));
    const w = [...e.client.windows.values()].filter((w) => w.lane === "code" && !w.flags.minimized).sort((a, b) => d(b.rect) - d(a.rect))[0];
    return w && { id: w.id, path: w.path, line: w.lines.start, away: Math.round(d(w.rect) / Math.max(v.w, v.h) * 10) / 10 };
  });
  if (!target) skip("focus.request suggestion: no code window loaded");
  else {
    const cam0 = await page.evaluate(() => [window.__cv.engine.cam.x, window.__cv.engine.cam.y]);
    await page.evaluate((t) => fetch("/api/v1/agent/focus", { method: "POST", headers: { "content-type": "application/json", "X-CV-Agent": "claude" }, body: JSON.stringify({ target: { path: t.path, line: t.line }, note: "look here" }) }), target);
    const sug = await page.waitForSelector(".suggest", { timeout: ms(30000) }).then(() => true).catch(() => false);
    const cam1 = await page.evaluate(() => [window.__cv.engine.cam.x, window.__cv.engine.cam.y]);
    check(sug && cam0[0] === cam1[0] && cam0[1] === cam1[1], "focus.request while not following shows a suggestion and leaves the camera");
    await shot("p-suggestion");
    if (sug) {
      await page.click(".suggest button:has-text('Go')");
      // The flight lasts 1.5 s; then the target window is selected and on screen.
      const arrived = await page
        .waitForFunction((id) => {
          const e = window.__cv.engine;
          const w = e.client.windows.get(id);
          if (e.cam.moving || !w || e.selected?.w?.id !== id) return false;
          const v = e.cam.view();
          const cx = w.rect.x + w.rect.w / 2;
          const cy = w.rect.y + Math.min(w.rect.h, v.h) / 2;
          return cx > v.x && cx < v.x + v.w && cy > v.y && cy < v.y + v.h;
        }, target.id, { timeout: ms(60000) })
        .then(() => true)
        .catch(() => false);
      const after = await page.evaluate(() => ({ cam: [Math.round(window.__cv.engine.cam.x), Math.round(window.__cv.engine.cam.y)], sel: window.__cv.engine.selected?.w?.id }));
      const moved = after.cam[0] !== Math.round(cam1[0]) || after.cam[1] !== Math.round(cam1[1]);
      check(arrived && (moved || target.away < 1), `Go flies to the suggested place (${target.path.split("/").pop()}:${target.line}, ${target.away} screens away: camera ${cam1.map(Math.round)} → ${after.cam}, selected ${after.sel === target.id ? "the target" : after.sel})`);
    }
  }
}

// Search: type, pick the first hit, fly there.
// Search for a symbol the view has (real repos and fixtures alike).
const q = await page.evaluate(() => { const w = [...window.__cv.engine.client.windows.values()].find((w) => w.lane === "code" && w.symbol?.name?.length > 3); return w?.symbol.name ?? "camera"; });
await page.click(".search");
await page.keyboard.type(q);
await page.waitForSelector(".hits li");
await shot("p-search");
const vSearch = await camV();
await page.keyboard.press("Enter");
await flown(vSearch, { selected: "window" });
check(await page.evaluate(() => window.__cv.engine.selected?.type === "window"), "search flew to and selected a window");

// Linked code: the Inspector's section lists the selected window's linked code with previews; a
// card flies there.
{
  await tab("Inspector");
  await page.waitForSelector(".linked .lk-card, .linked .pad", { timeout: ms(30000) }).catch(() => {});
  const groups = await page.evaluate(() => [...document.querySelectorAll(".linked h3")].map((h) => h.textContent));
  check(await page.$(".side .linked"), `linked-code section in the side bar on selection (${groups.join(", ") || "no links"})`);
  if (await page.$(".linked .lk-card")) {
    await page.waitForSelector(".linked .lk-code > div", { timeout: ms(30000) }).catch(() => {});
    check(await page.$(".linked .lk-code > div"), "linked cards show a code preview");
    await page.hover(".linked .lk-card");
    await until(() => !!window.__cv.engine["peekMark"].range, undefined, 10000);
    const peek = await page.evaluate(() => { const r = window.__cv.engine["peekMark"].range; return r && r.start; });
    const local = await page.evaluate(() => document.querySelector(".linked .lk-card .lk-local .ln")?.textContent);
    check(peek && local === `L${peek}`, `hovering a linked card marks its line on the canvas (${local})`);
    const before = await page.evaluate(() => { const c = window.__cv.engine.cam; return [c.x, c.y]; });
    const vCard = await camV();
    await page.click(".linked .lk-card");
    await flown(vCard);
    const after = await page.evaluate(() => { const c = window.__cv.engine.cam; return [c.x, c.y]; });
    check(Math.hypot(after[0] - before[0], after[1] - before[1]) > 1, "clicking a linked card flies there");
  }
  await shot("p-linked");
}

// Hub spokes: a hub's producers and consumers are linked to it and listed in the Inspector.
{
  await tab("Inspector");
  const hub = await page.evaluate(() => {
    const e = window.__cv.engine;
    const h = e.client.boot.hubs.find((h) => h.rect && h.anchors?.length);
    if (!h) return null;
    e.select({ type: "hub", h });
    return { id: h.id, spokes: e.edgesOf({ type: "hub", h }).length, sites: h.producers.length + h.consumers.length };
  });
  check(hub && hub.spokes > 0, `hub spokes connect ${hub?.id} to ${hub?.spokes} sites`);
  if (hub) await until((n) => document.querySelectorAll(".side .ins .links li").length >= n, Math.min(hub.sites, 200), 10000);
  const listed = await page.evaluate(() => document.querySelectorAll(".side .ins .links li").length);
  await shot("p-hub");
  check(hub && listed >= Math.min(hub.sites, 200), `hub Inspector lists its sites (${listed})`);
}

// Tests ↔ code at cluster level: a code cluster lists the test clusters linking into it.
{
  const r = await page.evaluate(() => {
    const e = window.__cv.engine;
    const b = e.client.boot;
    const lane = new Map(b.clusters.map((c) => [c.id, c.lane]));
    const bu = b.bundles.find((x) => lane.get(x.from) === "test" && lane.get(x.to) === "code");
    if (!bu) return null;
    const c = b.clusters.find((c) => c.id === bu.to);
    e.select({ type: "cluster", c });
    return c.label;
  });
  if (r) {
    const has = !!(await until(() => [...document.querySelectorAll(".side .ins h3")].some((h) => h.textContent.startsWith("Tests")), undefined, 10000));
    check(has, `code cluster "${r}" lists its test clusters`);
  } else console.log("skip cluster test links: no test → code bundles");
  await page.evaluate(() => window.__cv.engine.select(undefined));
}

// Agent loop: ask the agent (task) on a loaded code window and watch it work.
await tab("Agent");
const w = await page.evaluate(() => {
  const e = window.__cv.engine;
  const w = [...e.client.windows.values()].find((w) => w.lane === "code" && w.lines.end - w.lines.start > 10);
  e.cam.animateTo(w.rect.x - 300, w.rect.y - 120, 0.8, 0);
  return w;
});
await idle();
const line = w.lines.start + 4;
const before = await page.evaluate(async ([w, l]) => (await (await fetch(`/api/v1/blobs/${window.__cv.engine.client.windows.get(w.id).blob}/text?from=${l}&to=${l}`)).json()).lines[0], [w, line]);
await page.evaluate(([w, l]) => window.__cv.engine.onAction({ type: "compose", window: w, line: l }), [w, line]);
await page.waitForSelector(".pop textarea");
await page.fill(".pop textarea", "Please rename this local for clarity.");
await page.click(".pop button:has-text('Ask agent')");
// cv-mock answers with a simulated agent; cv serve has none (agent-loop.mjs drives a real one).
if (process.env.NO_SIM_AGENT) console.log("skip simulated-agent checks (NO_SIM_AGENT)");
else {
  await page.waitForSelector(".agent-chip", { timeout: ms(30000) }).then(() => check(true, "agent presence chip appears")).catch(() => check(false, "agent presence chip appears"));
  await shot("p-agent-working");
  await page.waitForFunction(() => [...document.querySelectorAll(".pop .comments li")].some((li) => li.textContent.includes("(agent)")), null, { timeout: ms(30000) })
    .then(() => check(true, "agent replied in the thread"))
    .catch(() => check(false, "agent replied in the thread"));
  // The amend reached the server: the window's blob changes, then its line reads differently.
  await until(([id, b]) => window.__cv.engine.client.windows.get(id)?.blob !== b, [w.id, w.blob], 60000);
  await idle();
  const lineNow = () => page.evaluate(async ([w, l]) => (await (await fetch(`/api/v1/blobs/${window.__cv.engine.client.windows.get(w.id).blob}/text?from=${l}&to=${l}`)).json()).lines[0], [w, line]);
  const after = (await pollNode(async () => { const t = await lineNow(); return t !== before ? { t } : undefined; }, { timeout: ms(30000) }))?.t ?? (await lineNow());
  check(after !== before, `agent's amend landed (${JSON.stringify(before)} → ${JSON.stringify(after)})`);
}
await shot("p-agent-done");
await page.keyboard.press("Escape");

// Tours and permissions: an agent's tour shows in the Agent panel and plays stop by stop.
const tourStops = await page.evaluate(() => [...window.__cv.engine.client.windows.values()].filter((w) => w.lane === "code").slice(0, 3).map((w) => ({ target: { path: w.path, line: w.lines.start + 1 }, note: `Look at ${w.symbol?.name ?? w.path}` })));
await page.evaluate((steps) => fetch("/api/v1/agent/tours", { method: "POST", headers: { "content-type": "application/json", "X-CV-Agent": "claude" }, body: JSON.stringify({ title: "Where retries happen", steps }) }), tourStops);
await page.waitForSelector(".links button:has-text('Where retries happen')", { timeout: ms(30000) }).then(() => check(true, "agent tour listed in the Agent panel")).catch(() => check(false, "agent tour listed in the Agent panel"));
await page.click(".links button:has-text('Where retries happen')");
await page.waitForSelector(".tour");
const cam0 = await page.evaluate(() => [window.__cv.engine.cam.x, window.__cv.engine.cam.y]);
const vTour = await camV();
await page.click(".tour button:has-text('Next')");
await flown(vTour);
const cam1 = await page.evaluate(() => [window.__cv.engine.cam.x, window.__cv.engine.cam.y]);
check(cam0[0] !== cam1[0] || cam0[1] !== cam1[1], "tour Next flies the camera to the next stop");
await shot("p-tour");
await page.click(".tour .x");
// The toggle is stored before the agent's edit is tried.
const permPut = () => page.waitForResponse((r) => r.url().includes("/agent/permissions") && r.request().method() === "PUT", { timeout: ms(30000) }).catch(() => null);
const offStored = permPut();
await page.uncheck(".perms label:has-text('Agent may edit') input");
await offStored;
const denied = await page.evaluate(async () => {
  const w = [...window.__cv.engine.client.windows.values()].find((w) => w.lane === "code");
  const r = await fetch("/api/v1/edits", { method: "POST", headers: { "content-type": "application/json", "X-CV-Agent": "claude" }, body: JSON.stringify({ target: w.change, mode: "amend", dryRun: true, files: [{ path: w.path, patches: [{ start: w.lines.start, end: w.lines.start, text: "x" }] }] }) });
  return r.status;
});
check(denied === 403, `agent edit refused while editing is off (${denied})`);
const onStored = permPut();
await page.check(".perms label:has-text('Agent may edit') input");
await onStored;

// With REPO, make sure the range below has connected unchanged code to show as context: a one-line
// change inside a code window that other windows depend on.
if (REPO) {
  const t = await page.evaluate(() => {
    const e = window.__cv.engine;
    const w = [...e.client.windows.values()].filter((w) => w.lane === "code" && !w.flags.minimized && w.conn.dependents2 > 0 && /\.(py|ts|js|rs|go)$/.test(w.path) && !e.review.conflicts.has(w.path)).sort((a, b) => b.conn.dependents2 - a.conn.dependents2)[0];
    return w && { path: w.path, line: w.lines.start + 1 };
  });
  if (t && existsSync(join(REPO, t.path))) {
    const f = join(REPO, t.path);
    const lines = readFileSync(f, "utf8").split("\n");
    lines.splice(t.line - 1, 0, `${t.path.endsWith(".py") ? "#" : "//"} e2e: context check (${RUN})`);
    writeFileSync(f, lines.join("\n"));
    const n0 = await opCount();
    jj("status");
    await opSeen(n0);
  } else skip("context seeding: no depended-on code window loaded");
}
// Range view from the dialog.
await page.click(".seg button:has-text('Range')");
await page.waitForSelector(".modal input");
await page.fill(".modal input", "trnk()..@");
{
  // The server's error and its hint, both shown; opening is refused while the revset is invalid.
  const err = await page.waitForSelector(".modal .err", { timeout: ms(30000) }).then(() => page.evaluate(() => {
    const e = document.querySelector(".modal .err");
    return { text: e?.textContent ?? "", hint: e?.querySelector(".muted")?.textContent ?? "", bad: !!document.querySelector(".modal input.bad-in") };
  })).catch(() => null);
  const served = (await api("POST", "/api/v1/revsets:validate", { revset: "trnk()..@" })).json;
  check(!!err && served?.ok === false && err.text.includes(served.error) && err.bad, `invalid revset shows the server's error (${JSON.stringify(err?.text?.slice(0, 80))})`);
  if (served?.hint) check(err?.hint.includes(served.hint), `…with its hint (${JSON.stringify(err?.hint)})`);
  else check(false, `the server gives a hint for an unknown function (${JSON.stringify(served)})`);
  // Open must be disabled (the click does nothing): no switch starts, the dialog stays.
  const disabled = await page.evaluate(() => !!document.querySelector(".modal button.primary")?.disabled);
  await page.click(".modal button.primary", { timeout: 2000 }).catch(() => {});
  await frames(page, 3);
  check(disabled && !(await page.evaluate(() => window.__cv.engine.isRange || window.__cv.engine.stats().pendingSwitch)) && !!(await page.$(".modal input[aria-label=Revset]")), "an invalid revset does not open a range");
}
// Presets (§7): my stack, one per agent workspace, last 10, single change; a commit picker; a fileset.
{
  const presets = await page.evaluate(() => [...document.querySelectorAll(".modal .presets button")].map((b) => b.textContent));
  const has = (t) => presets.some((p) => p.startsWith(t));
  check(has("My stack") && has("Last 10") && has("Single change"), `range presets: ${presets.map((p) => p.split(" ").slice(0, 2).join(" ")).join(", ")}`);
  const agents = await page.evaluate(async () => (await (await fetch("/api/v1/workspaces")).json()).filter((w) => w.agent && !w.serving).map((w) => `${w.agent.name}@${w.name}`));
  check(agents.every((a) => presets.some((p) => p.startsWith(a))), `one preset per agent workspace (${agents.join(", ") || "none bound"})`);
  await page.click(".modal .presets button:has-text('Last 10')");
  check((await page.inputValue(".modal input[aria-label=Revset]")).startsWith("ancestors(@, 10)"), "“Last 10” fills ancestors(@, 10)");
  await page.click(".modal .presets button:has-text('Pick commits')");
  await page.waitForSelector(".modal .picker li input", { timeout: ms(30000) }).catch(() => {});
  const boxes = await page.$$(".modal .picker li input");
  if (boxes.length >= 3) {
    // Two commits with one between them: not contiguous, so the dialog warns and offers to fill the gap.
    await boxes[0].check();
    await boxes[2].check();
    const rv = await page.inputValue(".modal input[aria-label=Revset]");
    check(rv.split("|").length === 2, `picking two commits builds the revset (${rv})`);
    const gap = await page.waitForSelector(".modal .warn-box:has-text('Not contiguous')", { timeout: ms(30000) }).then((h) => h.textContent()).catch(() => null);
    const served = (await api("POST", "/api/v1/revsets:validate", { revset: rv })).json;
    const sw = served?.warnings?.find((w) => w.kind === "gap");
    check(!!gap && !!sw && gap.includes(`${sw.missing.length} change(s)`), `a revset with a gap warns “Not contiguous” (${gap?.trim().slice(0, 60) ?? "no warning"}; server: ${sw ? `${sw.missing.length} missing` : "no gap warning"})`);
    await shot("p-range-gap");
    if (gap) {
      await page.click(".modal .warn-box button:has-text('Fill gap')");
      const filled = await page.inputValue(".modal input[aria-label=Revset]");
      check(filled === `(${rv})::`, `“Fill gap” extends the revset to its descendants (${filled})`);
      if (REPO) {
        // A real server: the filled range has no gap left.
        const left = await page.waitForFunction(() => document.querySelector(".modal .ok-sum") && !document.querySelector(".modal .warn-box")?.textContent?.includes("Not contiguous"), null, { timeout: ms(30000) }).then(() => true).catch(() => false);
        check(left, "…and the gap warning goes away");
      }
    }
    for (const b of await page.$$(".modal .picker li input:checked")) await b.uncheck();
  } else check(false, `commit picker lists changes (${boxes.length})`);
  await shot("p-range-picker");
  const filesetAsked = page.waitForRequest((r) => r.url().includes("revsets:validate") && (r.postData() ?? "").includes("\"fileset\""), { timeout: ms(30000) }).then(() => true).catch(() => false);
  await page.fill(".modal input[aria-label=Fileset]", "src/");
  check(await filesetAsked, "the fileset filter is validated with the revset");
  await page.fill(".modal input[aria-label=Fileset]", "");
  await page.click(".modal .presets button:has-text('Single change')");
  check((await page.inputValue(".modal input[aria-label=Revset]")).length > 0, "“Single change” fills one change");
}
await page.fill(".modal input[aria-label=Revset]", "trunk()..@");
await page.waitForSelector(".modal .ok-sum");
await shot("p-range-dialog");
await page.click(".modal button.primary");
await page.waitForSelector(".chip.range", { timeout: ms(60000) });
// The switch is over (the range view is built) and the camera's first frame of it has settled.
await until(() => window.__cv.engine.isRange && !window.__cv.engine.stats().pendingSwitch, undefined, 120000);
await idle();
await shot("p-range-map");
const stats = await page.evaluate(() => ({ range: window.__cv.engine.isRange, windows: window.__cv.engine.client.windows.size }));
check(stats.range && stats.windows > 0 && stats.windows < 500, `range view shows only changed + context windows (${stats.windows})`);
// D16: Fit and f frame what changed; the minimap lights the changed clusters; badges and owners.
{
  const framed = async () => page.evaluate(() => {
    const e = window.__cv.engine;
    const ch = e.changedClusters();
    const v = e.cam.view();
    const inView = ch.filter((c) => c.rect.x + c.rect.w / 2 >= v.x && c.rect.x + c.rect.w / 2 <= v.x + v.w && c.rect.y + c.rect.h / 2 >= v.y && c.rect.y + c.rect.h / 2 <= v.y + v.h).length;
    const w = e.client.boot.world;
    const worldZoom = Math.min((e.cam.width - 192) / w.w, (e.cam.height - 100) / w.h);
    return { changed: ch.length, inView, zoom: e.cam.zoom, worldZoom, lit: e["hud"].litClusters };
  });
  await page.evaluate(() => window.__cv.engine.cam.animateTo(0, 0, 0.5, 0));
  await page.mouse.move(box0.x + 700, box0.y + 500);
  const vF = await camV();
  await page.keyboard.press("f");
  await flown(vF);
  const a = await framed();
  check(a.changed > 0 && a.inView === a.changed, `f frames the ${a.changed} changed clusters (${a.inView} in view, zoom ${a.zoom.toFixed(4)} vs whole map ${a.worldZoom.toFixed(4)})`);
  await page.evaluate(() => window.__cv.engine.cam.animateTo(0, 0, 0.5, 0));
  const vFit = await camV();
  await page.click(".zoomctl button:has-text('Fit')");
  await flown(vFit);
  const b = await framed();
  check(b.inView === b.changed && Math.abs(b.zoom - a.zoom) < 1e-6, "the Fit button frames the same changed clusters");
  check(b.lit === b.changed, `the minimap lights the ${b.lit} changed clusters`);
  await shot("p-range-fit");
  const badge = await page.evaluate(() => {
    const e = window.__cv.engine;
    const w = [...e.client.windows.values()].find((w) => w.kind === "hunk" && w.heat.inRange);
    if (!w) return null;
    e.select({ type: "window", w });
    // Close enough for the T2 diff (owners) to load.
    e.cam.animateTo(w.rect.x - 300, w.rect.y - 100, 0.5, 0);
    return `${w.heat.inRange.commits} of ${w.heat.inRange.of}`;
  });
  await idle();
  if (badge) {
    await tab("Inspector");
    await page.waitForSelector(".side .ins dd.accent", { timeout: ms(10000) }).catch(() => {});
    // The hunk's T2 diff (owners) is loaded.
    await until(() => { const e = window.__cv.engine; return !!e.selected?.w && e.client.diffs.has(e.selected.w.id); }, undefined, 30000);
    const dd = await page.evaluate(() => [...document.querySelectorAll(".side .ins dd.accent")].map((d) => d.textContent).join(" | "));
    check(dd.includes(badge + " commit"), `in-range badge “${badge} commits” (Inspector: ${dd})`);
    const owners = await page.evaluate(() => { const e = window.__cv.engine; const w = e.selected.w; return { lines: (e.client.diffs.get(w.id)?.lines ?? []).filter((l) => l.owner && l.kind !== "ctx").length, shown: document.querySelectorAll(".side .owners .owner").length }; });
    if (owners.lines) {
      check(owners.shown > 0, `owner stripes: ${owners.shown} owning commit(s) listed for the hunk`);
      // …and drawn on the canvas, at the gutter edge of the lines each commit last touched (D16).
      const stripes = await page.evaluate(() => {
        const e = window.__cv.engine;
        const w = e.selected.w;
        const c = e["chunks"].get(Math.floor(w.rect.x / 16384) + "," + Math.floor(w.rect.y / 16384));
        return c ? c.stats.ownerStripes : -1;
      });
      check(stripes > 0, `owner stripes drawn on the canvas (${stripes} line stripes in the hunk's chunk)`);
    }
    else console.log("     (hunk diff has no owners yet)");
  } else check(false, "range hunk windows carry heat.inRange (“N of M commits”)");
  // Hotspots can rank by in-range touches.
  await tab("Hotspots");
  await page.waitForSelector(".seg[aria-label='Rank by']", { timeout: ms(30000) }).catch(() => {});
  const ranked = await page.evaluate(() => ({ seg: !!document.querySelector(".seg[aria-label='Rank by'] .on"), title: document.querySelector(".side .ins h3.grow")?.textContent, rows: [...document.querySelectorAll(".links.ranked .kind")].slice(0, 3).map((k) => k.textContent) }));
  check(ranked.seg && ranked.title === "Ranked by in-range touches" && ranked.rows.length > 0 && ranked.rows.every((r) => r.endsWith("×")), `Hotspots ranks by in-range touches (${ranked.rows.join(", ")})`);
  await shot("p-range-hotspots");
  await tab("Inspector");
  await page.evaluate(() => window.__cv.engine.select(undefined));
}
// Zoom to a hunk with its diff, and expand a context pill.
const hunk = await page.evaluate(() => {
  const e = window.__cv.engine;
  const h = [...e.client.windows.values()].find((w) => w.kind === "hunk");
  e.cam.animateTo(h.rect.x - 300, h.rect.y - 100, 0.7, 0);
  return h;
});
await idle();
check(await page.evaluate((id) => !!window.__cv.engine.client.diffs.get(id), hunk.id), "hunk diff loaded");
await shot("p-range-hunk");
const pill = await page.evaluate(() => {
  const e = window.__cv.engine;
  const c = [...e.client.windows.values()].find((w) => w.flags.minimized);
  if (!c) return null;
  e.cam.animateTo(c.rect.x - 300, c.rect.y - 100, 0.5, 0);
  return c;
});
if (pill) {
  await idle();
  const box = await (await page.$(".canvas canvas")).boundingBox();
  const p = await page.evaluate((c) => window.__cv.engine.cam.toScreen(c.rect.x + 100, c.rect.y + 18), pill);
  const under = await page.evaluate(([x, y]) => { const e = document.elementFromPoint(x, y); return e.tagName + "." + e.className; }, [box.x + p[0], box.y + p[1]]);
  await page.mouse.click(box.x + p[0], box.y + p[1]);
  await until((id) => window.__cv.engine["expanded"].has(id), pill.id, 15000);
  if (!under.startsWith("CANVAS")) console.log("     (click landed on", under + ")", JSON.stringify(p), JSON.stringify(pill.rect), await page.evaluate(() => JSON.stringify({ x: window.__cv.engine.cam.x, y: window.__cv.engine.cam.y, z: window.__cv.engine.cam.zoom })));
  check(await page.evaluate((id) => window.__cv.engine["expanded"].has(id), pill.id), "context pill expands in place");
  await shot("p-range-expanded");
}
// Context display passes through (D16): "expanded" opens every context window, "hidden" drops them.
// With REPO the range is @ alone, which holds the one-line change made above: its callers are
// unchanged in it, so they come as context windows.
for (const display of ["expanded", "hidden"]) {
  await page.click(".seg button:has-text('Range')");
  await page.waitForSelector(".modal input[aria-label=Revset]");
  await page.fill(".modal input[aria-label=Revset]", REPO ? "@" : "trunk()..@");
  await page.selectOption(".modal select[aria-label='Context display']", display);
  await page.waitForSelector(".modal .ok-sum");
  const posted = page.waitForRequest((r) => r.url().endsWith("/views") && r.method() === "POST", { timeout: ms(30000) }).then((r) => JSON.parse(r.postData()).options?.contextDisplay).catch(() => undefined);
  await page.click(".modal button.primary");
  const sent = await posted;
  await until((d) => window.__cv.engine["look"].contextDisplay === d && !window.__cv.engine.stats().pendingSwitch, display, 120000);
  await idle();
  // Look at a context window up close (where it would be drawn and picked).
  await page.evaluate(() => {
    const e = window.__cv.engine;
    const w = [...e.client.windows.values()].find((w) => w.kind === "context");
    if (w) e.cam.animateTo(w.rect.x - 200, w.rect.y - 100, 0.3, 0);
  });
  await idle();
  const ctx = await page.evaluate(() => {
    const e = window.__cv.engine;
    const ws = [...e.client.windows.values()].filter((w) => w.kind === "context");
    const isPill = (w) => w.flags.minimized && e["look"].contextDisplay !== "expanded" && !e["expanded"].has(w.id);
    // Drawn: context windows on screen that are there to pick (hidden context is neither drawn nor picked).
    const drawn = ws.filter((w) => {
      const [sx, sy] = e.cam.toScreen(w.rect.x + w.rect.w / 2, w.rect.y + 18);
      if (sx < 140 || sy < 50 || sx > e.cam.width || sy > e.cam.height) return false;
      const t = e.pick(sx, sy);
      return t?.type === "window" && t.w.id === w.id;
    }).length;
    return { n: ws.length, pills: ws.filter(isPill).length, drawn, look: e["look"].contextDisplay };
  });
  if (display === "expanded") check(sent === display && ctx.look === display && ctx.n > 0 && ctx.pills === 0, `context display “expanded” is sent and opens every context window (${ctx.n} context windows, ${ctx.pills} still pills)`);
  else check(sent === display && ctx.look === display && ctx.drawn === 0, `context display “hidden” is sent and draws no context window (${ctx.n} sent by the server, ${ctx.drawn} drawn)`);
  if (display === "hidden") await shot("p-range-hidden");
  else await shot("p-range-expanded-all");
}
// A range that is not a chain (400 range-not-chain) offers to open it per commit with allowNonChain.
// With REPO the range is real: @ and a second head made beside it. On cv-mock the 400 is shaped here.
{
  let revset = "trunk()..@";
  let refused = false;
  if (REPO) {
    jj("new", "--no-edit", "-r", "@-", "-m", `e2e: a second head (${RUN})`);
    const side = jj("log", "--no-graph", "-r", `description(substring:"a second head (${RUN})")`, "-T", "change_id.short(12)");
    revset = `@ | ${side}`;
  } else {
    await page.route("**/api/v1/views", async (route) => {
      const body = JSON.parse(route.request().postData() ?? "{}");
      if (!refused && body.mode === "range" && !body.options?.allowNonChain) {
        refused = true;
        return route.fulfill({ status: 400, json: { error: { code: "range-not-chain", message: "`a | b` has 2 heads (aaaa, bbbb): a range view needs a chain with one root and one head (D16)", details: { hint: "review one stack, or set options.allowNonChain" } } } });
      }
      return route.continue();
    });
  }
  const answer = page.waitForResponse((r) => r.url().endsWith("/views") && r.request().method() === "POST", { timeout: ms(30000) }).then(async (r) => [r.status(), (await r.json().catch(() => ({})))?.error?.code]).catch(() => [0]);
  await page.click(".seg button:has-text('Range')");
  await page.waitForSelector(".modal input[aria-label=Revset]");
  await page.fill(".modal input[aria-label=Revset]", revset);
  await page.waitForSelector(".modal .ok-sum, .modal .gap-warn, .modal .warn", { timeout: ms(30000) }).catch(() => {});
  await page.click(".modal button.primary");
  const [status, code] = await answer;
  const asked = await page.waitForSelector(".modal[aria-label='Not a chain']", { timeout: ms(30000) }).then(() => true).catch(() => false);
  await shot("p-not-a-chain");
  const retried = page.waitForRequest((r) => r.url().endsWith("/views") && r.method() === "POST" && JSON.parse(r.postData()).options?.allowNonChain === true, { timeout: ms(30000) }).then(() => true).catch(() => false);
  if (asked) await page.click(".modal button:has-text('Open per commit anyway')");
  check(status === 400 && code === "range-not-chain" && asked && (await retried), `a range that is not a chain offers “Open per commit anyway” (allowNonChain)${REPO ? ` (real: ${revset} → ${status} ${code})` : " (shaped 400)"}`);
  await until(() => window.__cv.engine.isRange && !window.__cv.engine.stats().pendingSwitch, undefined, 120000);
  if (!REPO) await page.unroute("**/api/v1/views");
}
await backToSnapshot();
check(true, "back to the snapshot");

// Workspace cards warn when a checkout is stale (WorkspaceSummary.stale). With REPO the state is real:
// a workspace whose @ another workspace rewrote. On cv-mock the response is shaped here.
{
  let name = "";
  if (REPO) {
    // Rebasing its @ onto another tree from here leaves its files on disk behind: stale.
    name = `e2e-stale-${RUN}`;
    // On root() (never on a conflicted commit: rewriting that away races the UI's conflict
    // requests), then rewritten from here: its checkout is stale.
    jj("workspace", "add", "--name", name, wsDir(name), "-r", "root()");
    jj("describe", "-r", `${name}@`, "-m", `e2e: rewritten elsewhere (${RUN})`);
  } else {
    await page.route("**/api/v1/workspaces", async (route) => {
      const res = await route.fetch();
      const list = await res.json();
      if (Array.isArray(list) && list[0]) list[0].stale = { reason: "working-copy", note: "an edit rewrote its @ while it was checked out", commits: ["087a4c3740eefb81"] };
      await route.fulfill({ response: res, json: list });
    });
  }
  await tab("Review");
  await tab("Workspaces");
  const sel = name ? `.ws-card[data-ws="${name}"] .stale` : ".ws-card .stale";
  // The server notices other workspaces' state by polling; the card updates live (workspace.updated).
  const stale = await page.waitForSelector(sel, { timeout: ms(30000) }).then((h) => h.textContent()).catch(() => "");
  check(stale.includes("Stale working copy") && stale.includes("jj workspace update-stale") && (REPO || stale.includes("rewrote its @")), `a stale workspace card says to run jj workspace update-stale${REPO ? ` (real: ${name})` : " (shaped response)"}`);
  await shot("p-ws-stale");
  if (!REPO) await page.unroute("**/api/v1/workspaces");
  await tab("Review");
}

// Workspaces Compare (D12): POST /workspaces:compare, or the same range spelled out on older servers.
// With REPO, two overlapping stacks are made for real: a workspace whose stack changes a file the
// served workspace's stack changes.
{
  if (REPO) {
    const files = jj("diff", "--name-only", "-r", "trunk()..@").split("\n").filter((f) => f && existsSync(join(REPO, f)));
    if (files.length) {
      const name = `e2e-cmp-${RUN}`;
      const dir = wsDir(name);
      jj("workspace", "add", "--name", name, dir, "-r", "trunk()");
      const f = join(dir, files[0]);
      writeFileSync(f, (existsSync(f) ? readFileSync(f, "utf8") : "") + `\n# e2e: compare (${RUN})\n`);
      jjIn(dir, "describe", "-m", `e2e: overlapping stack (${RUN})`);
    } else skip("seeding overlapping stacks: the served stack changes no file");
  }
  await tab("Workspaces");
  const btn = await page.waitForSelector(".ws-card .compare", { timeout: ms(30000) }).catch(() => null);
  if (!btn) (REPO ? check(false, "Compare: overlapping stacks show a Compare button") : skip("Compare: no overlapping workspaces"));
  else {
    const req = page.waitForRequest((r) => (r.url().endsWith("/workspaces:compare") || r.url().endsWith("/views")) && r.method() === "POST", { timeout: ms(30000) }).then((r) => r.url().split("/").pop()).catch(() => null);
    await btn.click();
    const via = await req;
    const opened = !!(await until(() => window.__cv.engine.isRange && !window.__cv.engine.stats().pendingSwitch, undefined, 120000));
    const toast = await page.evaluate(() => document.querySelector(".toast")?.textContent ?? "");
    check(opened || /no files in common/.test(toast), `Compare opens both stacks per commit (via ${via}${opened ? "" : `; ${toast}`})`);
    await idle();
    await shot("p-compare");
    if (opened) await backToSnapshot();
  }
  await tab("Inspector");
}

// Drag and pin: drag the selected window's title bar; it stays pinned there across a reload; Unpin.
// Moves belong to the district grid (the folder tree places windows by path): a grid lens first.
await page.click(`.seg[aria-label=Lens] button:has-text('Heat')`);
await until(() => { const e = window.__cv.engine; return (e.client.view.spec.options?.layout ?? "districts") === "districts" && !e.stats().pendingSwitch; }, undefined, 120000).catch(() => {});
await idle();
const box = await (await page.$(".canvas canvas")).boundingBox();
await ensureCode();
{
  await tab("Inspector");
  await idle();
  await page.waitForFunction(() => [...window.__cv.engine.client.windows.values()].some((w) => w.lane === "code" && !w.flags.minimized && w.lines.end - w.lines.start > 6), null, { timeout: ms(30000) }).catch(() => {});
  const w0 = await page.evaluate(async () => {
    const e = window.__cv.engine;
    const w = [...e.client.windows.values()].find((w) => w.lane === "code" && !w.flags.minimized && w.lines.end - w.lines.start > 6);
    e.select({ type: "window", w });
    e.cam.animateTo(w.rect.x - 200, w.rect.y - 200, 0.6, 0);
    return { key: w.key, x: w.rect.x, y: w.rect.y, w: w.rect.w };
  });
  await idle();
  const [hx, hy] = await page.evaluate((w) => window.__cv.engine.cam.toScreen(w.x + w.w / 2, w.y + 15), w0);
  const pre = await page.evaluate(([x, y]) => { const e = window.__cv.engine; return { pick: e.pick(x, y)?.type, sel: e.selected?.type, range: e.isRange, tier: e.cam.tier }; }, [hx, hy]);
  await page.mouse.move(box.x + hx, box.y + hy);
  await page.mouse.down();
  await page.mouse.move(box.x + hx + 150, box.y + hy + 90, { steps: 8 });
  await page.mouse.up();
  const moved = await page.waitForFunction((key) => {
    const w = [...window.__cv.engine.client.windows.values()].find((w) => w.key === key);
    return w && w.flags.pinned && w;
  }, w0.key, { timeout: ms(30000) }).then((h) => h.jsonValue()).catch(() => null);
  const dx = moved ? Math.round(moved.rect.x - w0.x) : NaN;
  const dy = moved ? Math.round(moved.rect.y - w0.y) : NaN;
  check(moved && Math.abs(dx - 250) < 8 && Math.abs(dy - 150) < 8, `dragging the title bar moves and pins the window (Δ ${dx}, ${dy})${moved ? "" : ` ${JSON.stringify(pre)}`}`);
  await shot("p-pinned");
  check(await page.$(".side .row.pin button:has-text('Unpin')"), "Inspector shows it pinned with Unpin");
  await page.click(".side .row.pin button:has-text('Unpin')").catch(() => {});
  const unpinned = await page.waitForFunction((key) => {
    const w = [...window.__cv.engine.client.windows.values()].find((w) => w.key === key);
    return w && !w.flags.pinned;
  }, w0.key, { timeout: ms(30000) }).then(() => true).catch(() => false);
  check(unpinned, "Unpin releases it");
}

// Clusters move zoomed out: select one at L1 and drag it.
{
  const c0 = await page.evaluate(() => {
    const e = window.__cv.engine;
    const c = e.client.boot.clusters.find((c) => c.lane === "code" && c.windows > 3);
    e.select({ type: "cluster", c });
    e.cam.animateTo(c.rect.x - 4000, c.rect.y - 3000, 0.05, 0);
    return { id: c.id, x: c.rect.x, y: c.rect.y, w: c.rect.w, h: c.rect.h };
  });
  await idle();
  const [cx, cy] = await page.evaluate((c) => window.__cv.engine.cam.toScreen(c.x + c.w / 2, c.y + c.h / 2), c0);
  await page.mouse.move(box.x + cx, box.y + cy);
  await page.mouse.down();
  await page.mouse.move(box.x + cx + 60, box.y + cy + 40, { steps: 8 });
  await page.mouse.up();
  const c1 = await page.waitForFunction((c0) => {
    const c = window.__cv.engine.client.boot.clusters.find((c) => c.id === c0.id);
    return c && c.pinned && Math.abs(c.rect.x - c0.x) > 1 && c;
  }, c0, { timeout: ms(30000) }).then((h) => h.jsonValue()).catch(() => null);
  await idle();
  check(c1 && Math.abs(c1.rect.x - c0.x - 1200) < 40 && Math.abs(c1.rect.y - c0.y - 800) < 40, `dragging a selected cluster moves and pins it (Δ ${c1 ? Math.round(c1.rect.x - c0.x) : "–"}, ${c1 ? Math.round(c1.rect.y - c0.y) : "–"})`);
  await page.evaluate((c) => window.__cv.engine.moveTo(`cluster:${c.id}`, c.x, c.y, false), c0);
  await idle();
  await page.evaluate(() => window.__cv.engine.select(undefined));
}

// Saved ranges: save the current revset under a name, pick it, delete it.
{
  await page.click(".seg button:has-text('Range')");
  await page.waitForSelector(".modal");
  await page.fill(".modal input.mono", "@");
  await page.waitForSelector(".modal .ok-sum", { timeout: ms(30000) }).catch(() => {});
  await page.fill(".modal .save-name", "e2e-mine");
  await page.click(".modal button:has-text('Save')");
  const listed = await page.waitForSelector(".modal .saved-ranges button:has-text('e2e-mine')", { timeout: ms(30000) }).then(() => true).catch(() => false);
  check(listed, "a range saved under a name is listed");
  await page.fill(".modal input.mono", "trunk()");
  await page.click(".modal .saved-ranges button:has-text('e2e-mine')").catch(() => {});
  check((await page.inputValue(".modal input.mono").catch(() => "")) === "@", "picking a saved range fills the form");
  await page.click(".modal .saved-ranges button[aria-label='Delete saved range e2e-mine']").catch(() => {});
  const gone = await page.waitForFunction(() => !document.querySelector(".modal .saved-ranges button[aria-label='Delete saved range e2e-mine']"), null, { timeout: ms(30000) }).then(() => true).catch(() => false);
  check(gone, "deleting a saved range removes it");
  await page.keyboard.press("Escape");
}
await ensureCode();
// F3: an immutable target offers "branch" as the only mode (D10): nothing is rewritten there. With a
// real server, a loaded window whose change is immutable (trunk); else (cv-mock, or a repo with no
// immutable change on screen) the /revisions answer for that change is shaped immutable here.
{
  await tab("Inspector");
  const pick = await page.evaluate(async () => {
    const e = window.__cv.engine;
    const ws = [...e.client.windows.values()].filter((w) => w.lane === "code" && !w.flags.minimized && w.lines.end - w.lines.start > 4 && !e.review.conflicts.has(w.path));
    const seen = new Set();
    for (const w of ws) {
      if (seen.has(w.change) || seen.size >= 25) continue;
      seen.add(w.change);
      const r = await (await fetch(`/api/v1/revisions?revset=${encodeURIComponent(w.change)}&limit=1`)).json().catch(() => null);
      if (r?.rows?.[0]?.rev.flags.immutable) return { id: w.id, change: w.change, real: true };
    }
    const w = ws[0];
    return w && { id: w.id, change: w.change, real: false };
  });
  if (!pick) skip("branch-only save bar: no code window loaded");
  else {
    const shape = !pick.real;
    const revRoute = (u) => u.pathname === "/api/v1/revisions";
    if (shape)
      await page.route(revRoute, async (route) => {
        const u = new URL(route.request().url());
        if (!(u.searchParams.get("revset") ?? "").startsWith(pick.change.slice(0, 8))) return route.continue();
        const j = await (await route.fetch()).json().catch(() => ({ rows: [] }));
        // The change's own row (cv-mock answers any revset with its whole history), immutable.
        const own = j.rows?.find((x) => x.rev.change.startsWith(pick.change)) ?? j.rows?.[0] ?? { rev: { commit: "0".repeat(40), parents: [], description: "", flags: {} }, lane: 0, edges: [] };
        return route.fulfill({ json: { ...j, rows: [{ ...own, rev: { ...own.rev, change: own.rev.change?.startsWith(pick.change) ? own.rev.change : pick.change, flags: { ...own.rev.flags, immutable: true } } }] } });
      });
    await page.evaluate((id) => {
      const e = window.__cv.engine;
      const w = e.client.windows.get(id);
      e.select({ type: "window", w });
      e.cam.animateTo(w.rect.x - 300, w.rect.y - 120, 0.8, 0);
    }, pick.id);
    await idle();
    await page.evaluate((id) => window.__cv.engine.onAction({ type: "edit", window: window.__cv.engine.client.windows.get(id) }), pick.id);
    await page.waitForSelector(".cm-host .cm-content", { timeout: ms(30000) }).catch(() => {});
    const bar = await page.waitForFunction(() => {
      const opts = [...document.querySelectorAll(".savebar select[aria-label='Where the edit goes'] option")].map((o) => o.value);
      const warn = document.querySelector(".savebar .warn-inline")?.textContent ?? "";
      return opts.length === 1 && warn ? { opts, warn, value: document.querySelector(".savebar select[aria-label='Where the edit goes']").value, desc: !!document.querySelector(".savebar input[placeholder=Description]") } : null;
    }, null, { timeout: ms(30000) }).then((h) => h.jsonValue()).catch(() => null);
    await shot("p-savebar-immutable");
    check(!!bar && bar.opts[0] === "branch" && bar.value === "branch" && /immutable/.test(bar.warn) && bar.desc, `an immutable target's save bar offers only "branch" (${bar ? `${bar.opts.join(", ")}; “${bar.warn}”; description field` : "not shown"})${shape ? " (immutable shaped)" : ` (real: ${pick.change.slice(0, 8)})`}`);
    await page.click(".savebar button:has-text('Cancel')").catch(() => {});
    if (shape) await page.unroute(revRoute);
  }
}
// Undo (D10): apply an edit from the editor; the undo bar offers POST /edits/:id/undo and shows the
// server's answer (reverted, or why it refused).
{
  await tab("Inspector");
  const w = await page.evaluate(() => {
    const e = window.__cv.engine;
    const w = [...e.client.windows.values()].find((w) => w.lane === "code" && !w.flags.minimized && w.lines.end - w.lines.start > 8 && !e.review.conflicts.has(w.path));
    e.select({ type: "window", w });
    e.cam.animateTo(w.rect.x - 300, w.rect.y - 120, 0.8, 0);
    return w;
  });
  await idle();
  const first = async () => page.evaluate(async (w) => (await (await fetch(`/api/v1/blobs/${window.__cv.engine.client.windows.get(w.id).blob}/text?from=${w.lines.start}&to=${w.lines.start}`)).json()).lines[0], w);
  const before = await first();
  await page.evaluate((w) => window.__cv.engine.onAction({ type: "edit", window: window.__cv.engine.client.windows.get(w.id) }), w);
  await page.waitForSelector(".cm-host .cm-content");
  const modes = await page.evaluate(() => [...document.querySelectorAll(".savebar select[aria-label='Where the edit goes'] option")].map((o) => o.value));
  check(modes.includes("keep-descendants") || modes.length === 1, `editor offers keep-descendants (${modes.join(", ")})`);
  await page.keyboard.press("Control+Home");
  await page.keyboard.type("# undo me\n");
  await page.click(".savebar button:has-text('Preview')");
  await page.waitForSelector(".savebar button.primary", { timeout: ms(30000) });
  await page.click(".savebar button.primary");
  await page.waitForSelector(".undobar", { timeout: ms(30000) });
  await idle();
  check((await first()) === "# undo me", "the edit applied and the undo bar offers to revert it");
  await shot("p-undo-offered");
  await page.click(".undobar button:has-text('Undo')");
  const res = await page.waitForFunction(() => document.querySelector(".undobar .add, .undobar .err")?.textContent, null, { timeout: ms(30000) }).then((h) => h.jsonValue()).catch(() => null);
  await idle();
  const after = await first();
  check(res && (res.startsWith("Undone") ? after === before : res.startsWith("Undo refused")), `Undo: ${res} (${JSON.stringify(after)})`);
  await shot("p-undo-result");
  await page.click(".undobar .x").catch(() => {});
}

await ensureCode();
// Safety additions (§6, §10): edit warnings in the save bar, and a blocked undo showing the later
// operations and the server's suggestion. With REPO both are real: an agent bound to a workspace
// whose @ sits on the edited change (agent-attached), and a later jj operation rewriting the edited
// change before Undo (undo-blocked). On cv-mock the responses are shaped here.
{
  const w = await page.evaluate(() => {
    const e = window.__cv.engine;
    const w = [...e.client.windows.values()].find((w) => w.lane === "code" && !w.flags.minimized && w.lines.end - w.lines.start > 8 && !e.review.conflicts.has(w.path));
    e.cam.animateTo(w.rect.x - 300, w.rect.y - 120, 0.8, 0);
    return w;
  });
  await idle();
  let agentWs = "";
  if (REPO) {
    agentWs = `e2e-agent-${RUN}`;
    jj("workspace", "add", "--name", agentWs, wsDir(agentWs), "-r", w.change);
    const bound = await other("POST", "/api/v1/agent/bind", { agent: "e2e-bot", client: "e2e", workspace: agentWs });
    check(bound.status === 200, `an agent binds to workspace ${agentWs} (${bound.status})`);
  } else {
    await page.route("**/api/v1/edits", async (route) => {
      const res = await route.fetch();
      const body = await res.json();
      body.warnings = [{ code: "agent-attached", message: "claude is editing in gym@, whose @ descends from this change", workspace: "gym", agent: { kind: "agent", name: "claude", workspace: "gym" } }];
      await route.fulfill({ response: res, json: body });
    });
    await page.route("**/api/v1/edits/*/undo", (route) =>
      route.fulfill({ status: 409, json: { error: { code: "undo-blocked", message: "later operations rewrote this change", details: { laterOps: [{ op: "a1b2c3d4e5f6", description: "describe commit 087a4c37", time: "2026-10-18T21:00:00Z", user: "you", changes: ["pmqsykqz"] }], divergent: false, conflicted: false, suggestion: "Edit the change again instead of undoing." } } } }),
    );
  }
  await page.evaluate((w) => window.__cv.engine.onAction({ type: "edit", window: window.__cv.engine.client.windows.get(w.id) ?? w }), w);
  await page.waitForSelector(".cm-host .cm-content");
  await page.keyboard.press("Control+Home");
  await page.keyboard.type("# warned\n");
  await page.click(".savebar button:has-text('Preview')");
  const warned = await page.waitForSelector(".savebar .edit-warnings .w-agent-attached", { timeout: ms(30000) }).then((h) => h.textContent()).catch(() => "");
  check(!!warned && (!REPO || warned.includes(agentWs)), `an agent-attached warning shows in the save bar${REPO ? ` (real: ${warned.slice(0, 90)})` : " (shaped response)"}`);
  await shot("p-edit-warning");
  await page.click(".savebar button.primary");
  await page.waitForSelector(".undobar", { timeout: ms(30000) });
  if (REPO) {
    // A later operation rewrites the edited change: undoing the edit's operation would lose it.
    await idle();
    jj("describe", "-r", w.change, "-m", `e2e: a later operation (${RUN})`);
  }
  await page.click(".undobar button:has-text('Undo')");
  const blocked = await page.waitForSelector(".undobar .undo-blocked li", { timeout: ms(30000) }).then(() => page.evaluate(() => document.querySelector(".undobar .undo-blocked").textContent)).catch(() => "");
  check(REPO ? blocked.length > 0 && /describe/i.test(blocked) : blocked.includes("describe commit 087a4c37") && blocked.includes("Edit the change again"), `a blocked undo lists the later operations and the suggestion${REPO ? ` (real: ${blocked.replace(/\s+/g, " ").slice(0, 120)})` : " (shaped 409)"}`);
  await shot("p-undo-blocked");
  if (!REPO) {
    await page.unroute("**/api/v1/edits");
    await page.unroute("**/api/v1/edits/*/undo");
  }
  await page.click(".undobar .x").catch(() => {});
}

// Dynamic links (D11) end in an open stub labelled "⇢ ? …". Real data first: the code lane's windows
// are scanned through the API (tiles, then outline batches) for a link whose target is only known at
// runtime. With REPO and none found, a case is seeded: a module whose bus topic is a runtime value.
{
  const scan = () => page.evaluate(async () => {
    const e = window.__cv.engine;
    const v = e.client.view;
    const code = e.client.boot.clusters.filter((c) => c.lane === "code" && c.windows > 0).sort((a, b) => b.windows - a.windows).slice(0, 60);
    const ids = new Set();
    const T = 4096;
    for (const c of code) {
      const z = Math.max(0, Math.ceil(Math.log2(Math.max(c.rect.w, c.rect.h) / T)));
      const size = T * 2 ** z;
      for (let x = Math.floor(c.rect.x / size); x <= Math.floor((c.rect.x + c.rect.w) / size); x++) {
        for (let y = Math.floor(c.rect.y / size); y <= Math.floor((c.rect.y + c.rect.h) / size); y++) {
          const t = await (await fetch(`/api/v1/views/${v.id}/tiles/${z}/${x}/${y}`)).json();
          for (const w of t.windows ?? []) if (w.lane === "code") ids.add(w.id);
        }
      }
      if (ids.size >= 3000) break;
    }
    const list = [...ids];
    for (let i = 0; i < list.length; i += 200) {
      const r = await fetch(`/api/v1/views/${v.id}/windows:batch`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ ids: list.slice(i, i + 200), parts: ["outline"], layoutVersion: v.layoutVersion }) });
      const b = await r.json();
      const l = (b.links ?? []).find((l) => "dynamic" in l.to && l.from.path);
      if (l) return { path: l.from.path, line: l.from.line, hint: l.to.dynamic.hint, scanned: Math.min(list.length, i + 200) };
    }
    return { scanned: list.length };
  });
  const show = (t) => page.evaluate(async (t) => {
    const e = window.__cv.engine;
    const w = await e.goTo({ path: t.path, line: t.line });
    if (!w) return false;
    e.select(undefined);
    const y = w.rect.y + 36 + ((t.line ?? w.lines.start) - w.lines.start) * 18;
    e.cam.animateTo(w.rect.x - 300, y - 400, 0.35, 0);
    for (let t0 = performance.now(); performance.now() - t0 < 60000; ) {
      if (window.__cv.stubs().length && [...e.client.links.values()].some((l) => "dynamic" in l.to && l.from.path === t.path)) return true;
      await new Promise((r) => setTimeout(r, 250));
    }
    return false;
  }, t);
  let found = await scan();
  let seeded = false;
  if (!found.path && REPO) {
    const path = `e2e_dynamic_${RUN}.py`;
    writeFileSync(join(REPO, path), `def announce(bus, kind, payload):\n    topic = make_topic(kind)\n    bus.emit(topic, payload)\n\n\ndef make_topic(kind):\n    return "e2e." + kind\n`);
    const n0 = await opCount();
    jj("status");
    // A file that did not exist when the view opened: reopen the snapshot at the new operation
    // (once the server has synced it).
    await opSeen(n0);
    await page.evaluate(() => window.__cv.engine.openSnapshot());
    await idle();
    found = await scan();
    seeded = true;
  }
  if (!found.path) (REPO ? check(false, `dynamic stub: a dynamic link (real or seeded) exists (${found.scanned} code windows scanned)`) : skip(`stub check: no dynamic link in ${found.scanned} code windows`));
  else {
    const drawn = await show(found);
    await idle();
    const stubs = await page.evaluate(() => window.__cv.stubs());
    check(drawn && stubs.length > 0 && stubs.every((t) => t.startsWith("⇢ ? ")), `dynamic stubs read “⇢ ? …” (${stubs.slice(0, 2).join(" ; ")}; ${seeded ? "seeded" : "real"}: ${found.path.split("/").pop()}:${found.line})`);
    await shot("p-dynamic-stub");
  }
}

// F4: workspace cards update from workspace.updated (an agent binding elsewhere; with REPO also a new
// workspace appearing), and workspace.recovered (REPO: a workspace whose @ became divergent through
// operations run elsewhere) is announced on its card. Last: on cv-mock a bind stands the simulated
// agent down.
{
  await tab("Workspaces");
  await page.waitForSelector(".ws-card", { timeout: ms(30000) }).catch(() => {});
  const seen = await page.evaluate(() => {
    const got = (window.__wsEvents = []);
    window.__cv.live.on((ev) => (ev.type === "workspace.updated" || ev.type === "workspace.recovered") && got.push(ev));
    return [...document.querySelectorAll(".ws-card")].map((c) => c.dataset.ws);
  });
  if (REPO) {
    const name = `e2e-live-${RUN}`;
    jj("workspace", "add", "--name", name, wsDir(name), "-r", "@-");
    const card = await page.waitForSelector(`.ws-card[data-ws="${name}"]`, { timeout: ms(30000) }).then(() => true).catch(() => false);
    check(card, `a new workspace's card appears live (${name})`);
  }
  const target = REPO ? seen.find((n) => n?.startsWith("e2e-stale-")) ?? seen[0] : seen[seen.length - 1];
  const bot = `e2e-watcher-${RUN}`;
  const bound = await other("POST", "/api/v1/agent/bind", { agent: bot, client: "e2e", workspace: target });
  const chip = await page.waitForSelector(`.ws-card[data-ws="${target}"] .chip.ag:has-text("${bot}")`, { timeout: ms(30000) }).then(() => true).catch(() => false);
  const evs = await page.evaluate(() => window.__wsEvents.filter((e) => e.type === "workspace.updated").length);
  check(bound.status === 200 && chip && evs > 0, `workspace cards update from workspace.updated (${bot}@${target} bound by another client; ${evs} event(s))`);
  await shot("p-ws-updated");
  if (REPO) {
    // Two operations from the same starting point rewrite the same change: its commits diverge.
    const name = `e2e-div-${RUN}`;
    jj("workspace", "add", "--name", name, wsDir(name), "-r", "@-");
    // The UI reloads the view on workspace.recovered (layout and conflicts may have moved).
    const reloaded = page.waitForRequest((r) => r.url().endsWith("/api/v1/views") && r.method() === "POST", { timeout: ms(45000) }).then(() => true).catch(() => false);
    const op0 = jj("op", "log", "-n", "1", "--no-graph", "-T", "id.short(16)");
    jj("describe", "-r", `${name}@`, "-m", `e2e: one side (${RUN})`);
    jj("--at-op", op0, "describe", "-r", `${name}@`, "-m", `e2e: other side (${RUN})`);
    jj("log", "-r", "@", "--no-graph", "-T", "change_id");
    const rec = await page.waitForSelector(`.ws-card[data-ws="${name}"] .warn-box:has-text("Recovered")`, { timeout: ms(40000) }).then((h) => h.textContent()).catch(() => "");
    const evs = await page.evaluate((n) => window.__wsEvents.filter((e) => e.type === "workspace.recovered" && e.workspace === n).length, name);
    check(rec.length > 0 && evs > 0 && (await reloaded), `workspace.recovered for a workspace whose @ diverged elsewhere shows on its card and reloads the view (${rec.slice(0, 90)})`);
    await shot("p-ws-recovered");
  } else skip("workspace.recovered: cv-mock has no way to make a workspace diverge");
  await tab("Inspector");
}

// Memory stays bounded (§5): far cells are evicted past the window cap and reload on return.
{
  const r = await page.evaluate(async () => {
    const e = window.__cv.engine;
    const c = e.client;
    const cap = c.windowCap;
    c.windowCap = 400;
    // Settled (as e2e-wait.mjs's settle): the camera's last data update ran and nothing is pending,
    // three polls in a row; bounded at 60 s.
    const idleP = () => new Promise((r) => {
      let q = 0;
      const t0 = performance.now();
      const t = setInterval(() => {
        const s = e.stats(), cam = e.cam;
        const ok = !cam.moving && performance.now() - cam.lastMotion > 200 && e["camVersion"] === cam.version && e["lastData"] >= cam.lastMotion && !s.pendingSwitch && !s.pendingRequests && !s.pendingBuilds;
        q = ok ? q + 1 : 0;
        if (q >= 3 || performance.now() - t0 > 60000) (clearInterval(t), r());
      }, 100);
    });
    // Six code clusters far apart, visited at map zoom; the first is where we come back to.
    const cs = c.boot.clusters.filter((k) => k.windows > 2).sort((a, b) => a.rect.x + a.rect.y * 0.5 - (b.rect.x + b.rect.y * 0.5));
    const stops = [0, 0.2, 0.4, 0.6, 0.8, 0.99].map((f) => cs[Math.floor(f * (cs.length - 1))]);
    const at = (k) => e.cam.animateTo(k.rect.x + k.rect.w / 2 - e.cam.width / 2 / 0.05, k.rect.y + k.rect.h / 2 - e.cam.height / 2 / 0.05, 0.05, 0);
    let peak = 0;
    for (const k of stops) {
      at(k);
      await idleP();
      peak = Math.max(peak, c.windows.size);
      // Eviction runs a few cells per pass, every 400 ms while the camera moves: nudge it along.
      for (let k = 0; k < 60 && (c.windows.size > c.windowCap || c.edgeIds?.size > c.edgeCap); k++) {
        await new Promise((r) => setTimeout(r, 420));
        e.cam.panBy(k % 2 ? 1 : -1, 0);
      }
      await idleP();
    }
    const settled = c.windows.size;
    const chunks = e["chunks"].size;
    // Windows left in cells farther than a screen from the view (eviction keeps everything nearer).
    const v = e.cam.view();
    const keep = { x: v.x - v.w, y: v.y - v.h, w: v.w * 3, h: v.h * 3 };
    let far = 0;
    for (const w of c.windows.values()) {
      const cx = Math.floor(w.rect.x / 16384) * 16384, cy = Math.floor(w.rect.y / 16384) * 16384;
      if (cx > keep.x + keep.w || cx + 16384 < keep.x || cy > keep.y + keep.h || cy + 16384 < keep.y) far++;
    }
    at(stops[0]);
    await idleP();
    const back = c.windowsIn(e.cam.view()).length;
    c.windowCap = cap;
    return { settled, peak, chunks, back, far, total: c.view.stats.windows };
  });
  if (r.peak <= 400 && r.total < 800) skip(`eviction check: the map has only ${r.total} windows`);
  else check(r.peak > 400 && (r.settled <= 400 || r.far === 0) && r.back > 0, `window cache stays bounded past its cap (cap 400: ${r.settled} loaded, ${r.far} far from the view, peak ${r.peak}, ${r.chunks} chunks; ${r.back} reloaded on return)`);
}

await Promise.all(pending404);
check(errors.length === 0, `no page errors ${errors.length ? JSON.stringify(errors.slice(0, 3)) : ""}`);

// Narrow screens: at 400 px the side panel is a drawer over the canvas; nothing scrolls sideways.
{
  const narrow = await browser.newPage({ viewport: { width: 400, height: 800 }, colorScheme: process.env.LIGHT ? "light" : "dark" });
  narrow.on("pageerror", (e) => errors.push(e.message + (e.stack ? " @ " + e.stack.split("\n").slice(1, 4).map((l) => l.trim()).join(" | ") : "")));
  await narrow.goto(process.env.URL ?? "http://127.0.0.1:5179");
  await narrow.waitForFunction(() => !!window.__cv, null, { timeout: ms(30000) });
  await steady(narrow);
  await narrow.waitForSelector(".drawer-btn", { timeout: ms(30000) });
  // The drawer's CSS transition (0.2 s) is over: no running animation on it.
  const drawer = (open) => narrow.waitForFunction((open) => { const el = document.querySelector(".side"); return !!el && el.classList.contains("open") === open && el.getAnimations().length === 0; }, open, { timeout: ms(10000), polling: 50 }).catch(() => {});
  const closed = await narrow.evaluate(() => getComputedStyle(document.querySelector(".side")).transform);
  await narrow.click(".drawer-btn");
  await drawer(true);
  const lay = await narrow.evaluate(() => {
    const side = document.querySelector(".side").getBoundingClientRect();
    const tabs = [...document.querySelectorAll(".tabs button")].map((b) => b.getBoundingClientRect());
    return { open: document.querySelector(".side").classList.contains("open"), side: [side.left, side.right, side.top, side.bottom], tabsIn: tabs.every((t) => t.left >= 0 && t.right <= 400.5 && t.bottom <= innerHeight), scrollW: document.documentElement.scrollWidth, canvasW: document.querySelector(".canvas").getBoundingClientRect().width };
  });
  check(closed !== "none" && lay.open && lay.side[0] >= 0 && lay.side[1] <= 400.5 && lay.side[3] <= 800.5 && lay.tabsIn && lay.scrollW <= 400 && lay.canvasW <= 400.5, `400 px: drawer opens over the canvas within the viewport (${JSON.stringify(lay)})`);
  await narrow.click(".tabs button:has-text('History')");
  await narrow.waitForSelector(".hist li", { timeout: ms(30000) }).catch(() => {});
  await drawer(true);
  await narrow.screenshot({ path: `${out}/p-400-drawer.png` });
  await narrow.click(".drawer-btn");
  await drawer(false);
  check(!(await narrow.evaluate(() => document.querySelector(".side").classList.contains("open"))), "400 px: the drawer closes again");
  await narrow.close();
}
await Promise.all(pending404);
check(errors.length === 0, `no page errors at 400 px ${errors.length ? JSON.stringify(errors.slice(0, 3)) : ""}`);
await browser.close();
