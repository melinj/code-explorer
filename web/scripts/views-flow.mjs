// End-to-end check of views, sessions and navigation (DESIGN §6, §7, §10), against cv-mock or `cv serve`:
// - per-session permissions: camera-follow switched on in one tab stays off in another, and a tab
//   shows the toggle stored for its own session after a reload;
// - the revision chip opens a snapshot at another revision (and refuses a bad one in place); there,
//   the editor offers the §6 "commit C" rows (amend, insert after, branch, keep descendants) aimed at
//   that commit, and a preview targets it;
// - Workspaces "Open @" opens a snapshot of that workspace's @ with its live files (REPO: a real
//   second workspace with an unsnapshotted edit), and "@" comes back to the same view;
// - History "Review range" gives `older::newer` whichever row was clicked first;
// - a 404 "view-unknown" (the server restarted) re-opens the view instead of leaving windows empty;
// - search lists review threads (REPO: a thread made here).
// Usage: URL=… [REPO=<repo cv serve serves>] node scripts/views-flow.mjs <screenshot dir>
import { chromium } from "playwright-core";
import { chrome } from "./chrome.mjs";
import { mkdirSync, writeFileSync, rmSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { dirname, basename, join } from "node:path";
import { waits, ms } from "./e2e-wait.mjs";

const out = process.argv[2] ?? "shots-views";
mkdirSync(out, { recursive: true });
const URL_ = process.env.URL ?? "http://127.0.0.1:5179";
const REPO = process.env.REPO;
const RUN = Date.now().toString(36).slice(-5);
const jj = (...args) => execFileSync("jj", ["-R", REPO, "--no-pager", "--color=never", ...args], { cwd: REPO, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], env: { ...process.env, JJ_EDITOR: "true" } }).trim();
const skip = (what) => console.log(`skip ${what}`);
const browser = await chromium.launch({ ...chrome(), args: process.env.CHROME_ARGS ? process.env.CHROME_ARGS.split(" ") : ["--ignore-gpu-blocklist", "--use-angle=vulkan", "--enable-features=Vulkan"] });
const errors = [];
const check = (ok, what) => {
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
  if (!ok) process.exitCode = 1;
};
/** A tab of its own (own context: own sessionStorage, so its own UI session id). */
async function openTab(name) {
  const ctx = await browser.newContext({ viewport: { width: 1500, height: 950 }, colorScheme: "dark" });
  const p = await ctx.newPage();
  p.on("pageerror", (e) => errors.push(`${name}: ${e.message}`));
  // 400s and 404s are provoked on purpose below (a bad revision, a lost view).
  p.on("console", (m) => m.type() === "error" && !/ (400|404) /.test(m.text()) && errors.push(`${name}: ${m.text()}`));
  await p.goto(URL_);
  await p.waitForFunction(() => !!window.__cv, null, { timeout: ms(60000) });
  return p;
}
const page = await openTab("A");
const until = (p, fn, arg, t = 30000) => p.waitForFunction(fn, arg, { timeout: ms(t), polling: 200 }).then((h) => h.jsonValue()).catch(() => null);
// No fixed sleeps: `idle(p)` waits until p's camera is at rest, its data update ran and nothing is
// loading, building or switching views (e2e-wait.mjs).
const { settle, frames, pollNode } = waits(page);
const idle = (p = page) => settle(p);
const shot = async (n, p = page) => (await settle(p), await frames(p), p.screenshot({ path: `${out}/${n}.png` }));
const tab = (name, p = page) => p.click(`.tabs button:has-text('${name}')`, { timeout: ms(60000) });
const api = (p, method, path, body) => p.evaluate(async ([m, u, b]) => {
  const r = await fetch(u, { method: m, headers: b === undefined ? {} : { "content-type": "application/json" }, body: b === undefined ? undefined : JSON.stringify(b) });
  let json = null;
  try { json = await r.json(); } catch {}
  return { status: r.status, json };
}, [method, path, body]);
const spec = (p = page) => p.evaluate(() => window.__cv.engine.client.view.spec);
const ensureCode = async (p = page) => {
  if (await p.evaluate(() => [...window.__cv.engine.client.windows.values()].some((w) => w.lane === "code" && !w.flags.minimized && w.lines.end - w.lines.start > 4))) return;
  await p.evaluate(() => {
    const e = window.__cv.engine;
    const c = e.client.boot.clusters.filter((c) => c.lane === "code").sort((a, z) => z.windows - a.windows)[0];
    if (c) e.cam.animateTo(c.rect.x + c.rect.w / 2 - e.cam.width / 2 / 0.08, c.rect.y + c.rect.h / 2 - e.cam.height / 2 / 0.08, 0.08, 0);
  });
  await idle(p);
};
await idle();
const view0 = await page.evaluate(() => window.__cv.engine.client.view.id);

// ── §10: per-session permissions across two tabs ────────────────────────────────────────────────
{
  const pageB = await openTab("B");
  await idle(pageB);
  for (const p of [page, pageB]) {
    await tab("Agent", p);
    await p.waitForSelector(".perms label:has-text('move my camera') input:not([disabled])", { timeout: ms(30000) }).catch(() => {});
    // Record the agent.permissions events this tab receives.
    await p.evaluate(() => { window.__perm = []; window.__cv.live.on((e) => e.type === "agent.permissions" && window.__perm.push(e.permissions)); });
  }
  const camBox = "label:has-text('move my camera') input";
  const before = await pageB.isChecked(camBox);
  await page.check(camBox);
  const sawA = await until(page, () => window.__perm.length > 0 && window.__perm.at(-1).moveCamera === true);
  // B gets the event too (one server, one event stream), but it names A's session.
  const sawB = await until(pageB, () => window.__perm.length > 0 && window.__perm.at(-1));
  // B's handler has run on that event (a synchronous React update): its checkbox must not follow.
  await frames(pageB, 3);
  const bChecked = await pageB.isChecked(camBox);
  const bStored = await api(pageB, "GET", `/api/v1/agent/permissions?session=${encodeURIComponent(await pageB.evaluate(() => sessionStorage.getItem("cv-session") ?? ""))}`);
  const bFollow = await pageB.evaluate(() => {
    // view.state carries followAgent; the Agent panel's checkbox is the visible half.
    return document.querySelector(".perms label:nth-of-type(2) input")?.checked ?? null;
  });
  check(!!sawA && sawB?.session && !before && !bChecked && bFollow === false, `camera-follow on in tab A stays off in tab B (A ${sawA ? "on" : "no event"}; B saw an event for session ${sawB?.session ? "A's" : "?"} and kept ${bChecked ? "ON" : "off"}; B's own row moveCamera=${bStored.json?.moveCamera})`);
  await shot("v-perms-tab-b", pageB);
  // A reload shows what is stored for this tab's session, not a local default.
  await page.reload();
  await page.waitForFunction(() => !!window.__cv, null, { timeout: ms(60000) });
  await idle();
  await tab("Agent");
  const stored = await until(page, () => { const i = document.querySelector(".perms label:nth-of-type(2) input"); return i && !i.disabled && i.checked ? true : null; }, null, 30000);
  check(!!stored, "after a reload, tab A's camera checkbox shows the stored permission (on)");
  await page.uncheck(camBox);
  await until(page, () => !document.querySelector(".perms label:nth-of-type(2) input")?.checked);
  await pageB.context().close();
}

// ── §7: the revision chip opens a snapshot at another revision ────────────────────────────────
await tab("Inspector");
let atRev;
{
  await page.click(".rev-chip");
  const shown = await page.waitForSelector(".revpick", { timeout: ms(30000) }).then(() => true).catch(() => false);
  check(shown, "the @ chip opens the revision picker");
  await shot("v-revpick");
  if (REPO) {
    // A revision that resolves to nothing is refused in place; the view stays.
    await page.fill(".revpick input[aria-label=Revision]", `nope_${RUN}(`);
    await page.click(".revpick button:has-text('Open')");
    const err = await until(page, () => document.querySelector(".revpick .err")?.textContent ?? null, null, 30000);
    const still = await page.evaluate(() => window.__cv.engine.client.view.id);
    check(!!err && still === view0, `a bad revision is refused in the picker (“${(err ?? "").slice(0, 80)}”), the view stays`);
  }
  await page.fill(".revpick input[aria-label=Revision]", "@-");
  await page.click(".revpick button:has-text('Open')");
  atRev = await until(page, () => { const v = window.__cv.engine.client.view; return v.spec.mode === "snapshot" && v.spec.rev === "@-" ? { id: v.id, head: v.resolved.head } : null; }, null, 60000);
  await idle();
  const chip = (await until(page, () => (document.querySelector(".rev-chip")?.textContent?.trim() === "@-" ? "@-" : null), null, 30000)) ?? (await page.textContent(".rev-chip"))?.trim();
  const head = REPO ? jj("log", "-r", "@-", "--no-graph", "-T", "commit_id") : undefined;
  check(!!atRev && chip === "@-" && (!REPO || atRev.head === head), `the chip opened a snapshot of @- (chip “${chip}”${REPO ? `, head ${atRev?.head?.slice(0, 8)} = jj @- ${head?.slice(0, 8)}` : ""})`);
  await shot("v-snapshot-at-parent");
}

// §6 "commit C": in that snapshot the editor offers amend / insert after / branch / keep descendants,
// aimed at the window's change, and a preview targets it.
{
  await ensureCode();
  const w = await page.evaluate(() => {
    const e = window.__cv.engine;
    const w = [...e.client.windows.values()].find((w) => w.lane === "code" && !w.flags.minimized && w.lines.end - w.lines.start > 4 && !e.review.conflicts.has(w.path));
    if (!w) return null;
    e.select({ type: "window", w });
    e.cam.animateTo(w.rect.x - 300, w.rect.y - 120, 0.8, 0);
    return { id: w.id, change: w.change, path: w.path };
  });
  if (!w) skip("§6 rows: no code window at @-");
  else {
    await idle();
    await page.evaluate((id) => window.__cv.engine.onAction({ type: "edit", window: window.__cv.engine.client.windows.get(id) }), w.id);
    await page.waitForSelector(".cm-host .cm-content", { timeout: ms(30000) }).catch(() => {});
    const modes = await until(page, () => { const o = [...document.querySelectorAll(".savebar select[aria-label='Where the edit goes'] option")].map((o) => o.value); return o.length ? o : null; }, null, 30000);
    const want = ["amend", "insert-after", "branch", "keep-descendants"];
    const immutable = modes?.length === 1 && modes[0] === "branch";
    check(!!modes && (want.every((m) => modes.includes(m)) || immutable), `the editor at @- offers the §6 rows for commit C (${modes?.join(", ")})`);
    if (modes && !immutable) {
      await page.selectOption(".savebar select[aria-label='Where the edit goes']", "insert-after");
      await page.click(".cm-host .cm-content");
      await page.keyboard.press("Control+Home");
      await page.keyboard.type(`# views5 ${RUN}\n`);
      const req = page.waitForRequest((r) => r.url().endsWith("/api/v1/edits") && r.method() === "POST", { timeout: ms(30000) }).catch(() => null);
      const res = page.waitForResponse((r) => r.url().endsWith("/api/v1/edits") && r.request().method() === "POST", { timeout: ms(30000) }).catch(() => null);
      await page.click(".savebar button:has-text('Preview')");
      const body = (await req)?.postDataJSON();
      const answer = await (await res)?.json().catch(() => null);
      check(body?.target === w.change && body?.mode === "insert-after" && body?.dryRun === true && answer?.status === "previewed", `the preview goes to that commit (target ${body?.target?.slice(0, 8)} = window change ${w.change.slice(0, 8)}, mode ${body?.mode}, answer ${answer?.status ?? answer?.error?.code})`);
      if (REPO) check((answer?.rewritten ?? []).some((c) => w.change.startsWith(c) || c.startsWith(w.change.slice(0, 8))) || (answer?.created && true), `the server previewed it at @- (rewritten ${(answer?.rewritten ?? []).map((c) => c.slice(0, 8)).join(", ")}${answer?.created ? `, creates ${answer.created.slice(0, 8)}` : ""})`);
      await shot("v-edit-at-parent");
    }
    await page.click(".savebar button:has-text('Cancel')").catch(() => {});
    await page.keyboard.press("Escape").catch(() => {});
  }
}

// ── §7: Workspaces "Open @" ─────────────────────────────────────────────────────────────────────
{
  let name;
  let dir;
  let livePath;
  if (REPO) {
    // From the @ map: its files are the new workspace's (a child of @).
    await page.evaluate(() => window.__cv.engine.openSnapshot("@"));
    await idle();
    await ensureCode();
    name = `vw${RUN}`;
    dir = join(dirname(REPO), `${basename(REPO)}.${name}`);
    jj("workspace", "add", "--name", name, "-r", "@", dir);
    // An unsnapshotted edit in that workspace: it shows on its own snapshot as a live file.
    // A code file the @ map shows (the new workspace's @ is a child of @: it has the same files).
    // (Not a file that is itself only live in the served workspace: @'s tree must have it.)
    livePath = await page.evaluate(() => [...window.__cv.engine.client.windows.values()].find((w) => w.lane === "code" && !w.flags.minimized && !w.flags.unsnapshotted && !w.blob.startsWith("live") && !window.__cv.engine.review.conflicts.has(w.path))?.path);
    if (livePath) {
      const p = join(dir, livePath);
      const text = execFileSync("cat", [p], { encoding: "utf8" });
      writeFileSync(p, `# live edit ${RUN}\n${text}`);
    }
    // The server's workspace poll (1 s) and the cards' cache (2 s) have picked the workspace up.
    await pollNode(async () => (await api(page, "GET", "/api/v1/workspaces")).json?.some?.((c) => c.name === name), { timeout: ms(60000) });
  } else {
    const cards = (await api(page, "GET", "/api/v1/workspaces")).json ?? [];
    name = cards.find((c) => !c.serving)?.name;
  }
  if (!name) skip("Open @: no second workspace");
  else {
    await tab("Workspaces");
    await page.waitForSelector(`.ws-card[data-ws='${name}'] .open-at`, { timeout: ms(30000) }).catch(() => {});
    await page.click(`.ws-card[data-ws='${name}'] .open-at`);
    const opened = await until(page, (n) => { const s = window.__cv.engine.client.view.spec; return s.mode === "snapshot" && s.rev === `${n}@` && s.options?.workspace === n ? window.__cv.engine.client.view.id : null; }, name, 240000);
    await idle();
    const chip = (await until(page, (n) => (document.querySelector(".rev-chip")?.textContent?.trim() === `${n}@` ? `${n}@` : null), name, 30000)) ?? (await page.textContent(".rev-chip"))?.trim();
    check(!!opened && chip === `${name}@`, `Workspaces "Open @" opens a snapshot of ${name}@ with its live files (chip “${chip}”)`);
    if (REPO && livePath) {
      await page.evaluate((p) => window.__cv.engine.goTo({ path: p }), livePath);
      await idle();
      const live = await until(page, (p) => [...window.__cv.engine.client.windows.values()].some((w) => w.path === p && w.flags.unsnapshotted && w.blob.startsWith("live")) || null, livePath, 30000);
      check(!!live, `${name}@'s unsnapshotted edit of ${livePath} shows on its snapshot (live blob)`);
    }
    await shot("v-open-at");
    // Back to @ from the picker: the same view as before (nothing indexed again).
    await page.click(".rev-chip");
    await page.click(".revpick button.chip:has-text('@')");
    const back = await until(page, () => { const v = window.__cv.engine.client.view; return v.spec.rev === "@" ? v.id : null; }, null, 60000);
    check(back === view0, `the picker's "@" returns to the @ view (${back === view0 ? "same id" : `${back} ≠ ${view0}`})`);
    await idle();
  }
  if (REPO && dir) {
    try { jj("workspace", "forget", name); } catch {}
    rmSync(dir, { recursive: true, force: true });
  }
}

// ── History "Review range": order of the clicks does not matter ───────────────────────────────
{
  await tab("History");
  await page.waitForSelector(".hist li[data-change]", { timeout: ms(30000) }).catch(() => {});
  const rows = await page.$$eval(".hist li[data-change]", (ls) => ls.map((l) => l.getAttribute("data-change")));
  if (rows.length < 3) skip(`Review range order: ${rows.length} history rows`);
  else {
    const range = async (first, second) => {
      await page.click(`.hist li[data-change='${first}']`);
      await page.click(`.hist li[data-change='${second}']`, { modifiers: ["Shift"] });
      await page.click(".hist-bar button:has-text('Review range')");
      const v = await page.waitForSelector("[aria-label=Revset]", { timeout: ms(30000) }).then((h) => h.inputValue()).catch(() => null);
      await page.keyboard.press("Escape");
      await page.waitForSelector("[aria-label=Revset]", { state: "detached", timeout: ms(30000) }).catch(() => {});
      return v;
    };
    const [newer, older] = [rows[0], rows[2]];
    const a = await range(newer, older);
    const b = await range(older, newer);
    const want = `${older.slice(0, 8)}::${newer.slice(0, 8)}`;
    check(a === want && b === want, `History "Review range" is older::newer either way (${a} / ${b})`);
  }
}

// ── A lost view (404 view-unknown, e.g. after a restart) is re-opened ──────────────────────────
{
  await tab("Inspector");
  await ensureCode();
  let posted = 0;
  const onReq = (r) => r.method() === "POST" && r.url().endsWith("/api/v1/views") && posted++;
  page.on("request", onReq);
  let failed = 0;
  const batch = (u) => u.pathname.endsWith("/windows:batch");
  await page.route(batch, (route) => {
    if (failed++ > 0) return route.continue();
    return route.fulfill({ status: 404, contentType: "application/json", body: JSON.stringify({ error: { code: "view-unknown", message: "unknown view: not loaded" }, code: "view-unknown", message: "unknown view: not loaded" }) });
  });
  // Zoom into code so outlines are asked for (and the first batch is answered "view-unknown").
  await page.evaluate(() => {
    const e = window.__cv.engine;
    const w = [...e.client.windows.values()].find((w) => w.lane === "code" && !w.flags.minimized);
    e.client.outlines = e.client.newOutlines();
    e.cam.animateTo(w.rect.x - 200, w.rect.y - 100, 0.5, 0);
  });
  // Re-opened: a new view is in place and its switch is over.
  const reopened = await until(page, () => (window.__cv.engine.stats().windowsLoaded > 0 && !window.__cv.engine.stats().pendingSwitch) || null, null, 120000);
  await idle();
  // Outlines are asked for as the camera settles at L2 and closer: nudge it once the view is back.
  const outlines = await until(page, () => {
    const e = window.__cv.engine;
    if (e.client.outlines.map.size > 0) return true;
    const w = [...e.client.windows.values()].find((w) => w.lane === "code" && !w.flags.minimized);
    if (w && Math.random() < 0.2) e.cam.animateTo(w.rect.x - 150 - Math.random() * 50, w.rect.y - 80, 0.9, 0);
    return null;
  }, null, 60000);
  check(failed >= 1 && posted >= 1 && !!reopened && !!outlines, `a 404 "view-unknown" re-opens the view (${failed} batch(es) intercepted, ${posted} POST /views, outlines ${outlines ? "back" : "missing"})`);
  await page.unroute(batch);
  page.off("request", onReq);
}

// ── Search lists review threads ───────────────────────────────────────────────────────────────
{
  let word = "";
  if (REPO) {
    const w = await page.evaluate(() => [...window.__cv.engine.client.windows.values()].find((w) => w.lane === "code"));
    word = `zebra${RUN}`;
    const t = await api(page, "POST", "/api/v1/threads", { anchor: { path: w.path, rev: w.change, start: w.lines.start, end: w.lines.start }, body: `Check this ${word} please` });
    if (t.status >= 300) word = "";
  } else {
    const ts = (await api(page, "GET", "/api/v1/threads")).json ?? [];
    word = ts[0]?.comments?.[0]?.body?.split(/\s+/).find((x) => x.length > 4) ?? "";
  }
  if (!word) skip("search thread hits: no thread to find");
  else {
    await page.fill("input.search", word);
    const hit = await until(page, () => [...document.querySelectorAll(".hits li .kind")].some((k) => k.textContent === "thread") || null, null, 30000);
    check(!!hit, `search lists review threads (“${word}”)`);
    await page.fill("input.search", "");
  }
}

check(errors.length === 0, `no page errors${errors.length ? `: ${errors.slice(0, 5).join(" | ")}` : ""}`);
await browser.close();
