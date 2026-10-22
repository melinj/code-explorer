// End-to-end smoke test of the review flows in real Chrome: thread markers, thread popover (reply,
// resolve), composer from the gutter, and the editor (preview → apply → text reload).
// Usage: node scripts/review-flow.mjs <screenshot dir>
// Env: URL, LIGHT=1 (light theme), AGENT=<name> (an agent is attached: on cv serve,
// scripts/scripted-agent.mjs), NO_SIM_AGENT=1, REPO=<path> (the served repo: real jj ops for the
// agent-answer section), REQUIRE=<keys>|all (a check that cannot run prints "SKIP <key>: <reason>";
// listed keys print FAIL instead: conflicts, ask-agent, agent-resolution, agent-answer, fade).
import { chromium } from "playwright-core";
import { chrome } from "./chrome.mjs";
import { mkdirSync, writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { waits, ms, sleep } from "./e2e-wait.mjs";

const out = process.argv[2] ?? "shots-review";
mkdirSync(out, { recursive: true });
// CHROME_ARGS overrides the GPU flags (CI without a GPU: "--use-angle=swiftshader --enable-unsafe-swiftshader").
const browser = await chromium.launch({ ...chrome(), args: process.env.CHROME_ARGS ? process.env.CHROME_ARGS.split(" ") : ["--ignore-gpu-blocklist", "--use-angle=vulkan", "--enable-features=Vulkan"] });
const page = await browser.newPage({ viewport: { width: 1600, height: 1000 }, colorScheme: process.env.LIGHT ? "light" : "dark" });
const errors = [];
page.on("pageerror", (e) => errors.push(e.message + (e.stack ? " @ " + e.stack.split("\n").slice(1, 4).map((l) => l.trim()).join(" | ") : "")));
page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
await page.goto(process.env.URL ?? "http://127.0.0.1:5179");
await page.waitForFunction(() => !!window.__cv);
// No fixed sleeps: `idle` waits until the camera is at rest, its data update ran and nothing is
// loading or building (e2e-wait.mjs); `flown(v0)` until a flight that started after `camV()` ended.
const { settle, camV, flown, frames, pollNode, until } = waits(page);
const idle = () => settle();
const box = await (await page.$(".canvas canvas")).boundingBox();
const shot = async (n) => (await settle(), await frames(), page.screenshot({ path: `${out}/${n}.png` }));
const check = (ok, what) => {
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
  if (!ok) process.exitCode = 1;
};
// A check that cannot run here says so with its reason; under REQUIRE (CI) that is a failure.
const required = new Set((process.env.REQUIRE ?? "").split(",").map((k) => k.trim()).filter(Boolean));
const skip = (key, why) => {
  const hard = required.has(key) || required.has("all");
  console.log(`${hard ? "FAIL" : "SKIP"} ${key}: ${why}${hard ? " (required by REQUIRE)" : ""}`);
  if (hard) process.exitCode = 1;
};

// The theme follows the system colour scheme (LIGHT=1 runs the flow in the light theme): the canvas
// palette and the page's CSS variables both switch.
{
  const want = process.env.LIGHT ? "light" : "dark";
  const t = await page.evaluate(() => ({ canvas: window.__cv.engine["look"].theme.name, bg: getComputedStyle(document.documentElement).getPropertyValue("--bg").trim() }));
  const lum = (hex) => { const n = parseInt(hex.replace("#", ""), 16); return ((n >> 16) & 255) * 0.299 + ((n >> 8) & 255) * 0.587 + (n & 255) * 0.114; };
  check(t.canvas === want && (want === "light" ? lum(t.bg) > 160 : lum(t.bg) < 90), `${want} theme: canvas palette ${t.canvas}, page background ${t.bg}`);
}
// Load the view around review-first and wait for threads.
await page.evaluate(() => window.__cv.engine.home(0));
await idle();
// Real repos can frame review-first below the tile zoom; frame its largest cluster instead.
await page.evaluate(() => {
  const e = window.__cv.engine;
  if (e.cam.zoom >= 0.03) return;
  const b = e.client.boot;
  const code = b.clusters.filter((c) => c.lane === "code").sort((a, z) => z.heat - a.heat || z.conn - a.conn || z.windows - a.windows);
  e.frameRect(code[0].rect, 0);
  if (e.cam.zoom < 0.04) e.cam.animateTo(code[0].rect.x - 200, code[0].rect.y - 200, 0.05, 0);
});
await idle();
// A fresh repo has no threads yet: seed one through the API on a loaded code window.
await page.evaluate(async () => {
  const e = window.__cv.engine;
  await e.review.load();
  await e.review.loadConflicts();
  const ok = (w) => w.lane === "code" && !e.review.conflicts.has(w.path);
  if ([...e.client.windows.values()].some((w) => ok(w) && e.review.threadsIn(w).length)) return;
  const w = [...e.client.windows.values()].find((w) => ok(w) && w.lines.end - w.lines.start > 12);
  await e.review.create({ anchor: { path: w.path, rev: w.change, start: w.lines.start + 2, end: w.lines.start + 2 }, body: "Is this branch reachable?", kind: "question" });
});
await page.waitForFunction(() => window.__cv.engine.review.threads.size > 0);
// A loaded window with an open thread, opened at L3 (zoom 0.8, top of the window).
const w = await page.evaluate(() => {
  const e = window.__cv.engine;
  // Not in a conflicted file: amends there are refused until the conflict is resolved.
  const ws = [...e.client.windows.values()].filter((w) => e.review.threadsIn(w).length && w.lane === "code" && !e.review.conflicts.has(w.path));
  const w = ws[0];
  // Frame the thread's line, which can sit far down a tall window.
  const line = e.review.threadsIn(w)[0].anchor.start;
  const y = w.rect.y + 36 + (line - w.lines.start) * 18;
  e.cam.animateTo(w.rect.x - 300, Math.max(w.rect.y - 120, y - 300), 0.8, 0);
  return w;
});
await idle();
const marker = await page.evaluate((id) => {
  const e = window.__cv.engine;
  const h = e["threadMarks"].hits.find((h) => h.window.id === id);
  return h && e.cam.toScreen(h.x + h.w / 2, h.y + h.h / 2);
}, w.id);
check(!!marker, "thread marker drawn at its line");
await shot("r1-markers");

// Open the thread, reply, resolve.
await page.mouse.click(box.x + marker[0], box.y + marker[1]);
await page.waitForSelector(".pop", { timeout: ms(30000) }).catch(async (err) => {
  const dbg = await page.evaluate(([x, y, bx, by, id]) => {
    const e = window.__cv.engine;
    const el = document.elementFromPoint(bx + x, by + y);
    const t = e.pick(x, y);
    const h = e["threadMarks"].hits.find((h) => h.window.id === id);
    return { el: el.tagName + "." + el.className, pick: t?.type, thread: h?.thread.id, status: h?.thread.status, tier: e.cam.tier, x, y };
  }, [marker[0], marker[1], box.x, box.y, w.id]);
  console.log("     thread click:", JSON.stringify(dbg));
  throw err;
});
check(await page.$(".pop .comments li"), "thread popover opens with comments");
await shot("r2-thread");
await page.fill(".pop textarea", "Agreed, I'll take a look.");
await page.click(".pop button.primary");
await page.waitForFunction(() => document.querySelectorAll(".pop .comments li").length >= 2);
check(true, "reply posted");
await page.click(".pop .row button:has-text('Resolve')");
await page.waitForSelector(".pop .tstatus.resolved");
check(true, "thread resolved");
await page.keyboard.press("Escape");

// Composer from the gutter at a line without a thread (the mock keeps state across runs).
const k = await page.evaluate((w) => {
  const taken = new Set(window.__cv.engine.review.threadsIn(w).map((t) => t.anchor.start - w.lines.start));
  let k = 3;
  while (taken.has(k)) k++;
  return k;
}, w);
await page.evaluate((w) => window.__cv.engine.cam.animateTo(w.rect.x - 300, w.rect.y - 120, 0.8, 0), w);
await idle();
const gut = await page.evaluate(([w, k]) => window.__cv.engine.cam.toScreen(w.rect.x + 20, w.rect.y + 36 + k * 18 + 9), [w, k]);
await page.mouse.click(box.x + gut[0], box.y + gut[1]);
await page.waitForSelector(".pop textarea");
await page.click(".pop .seg button:has-text('suggestion')");
const prefilled = await page.inputValue(".pop textarea");
check(prefilled.includes("```suggestion"), "suggestion kind prefills the anchored line");
await page.fill(".pop textarea", "Rename for clarity:\n```suggestion\nrenamed_value = 1\n```");
await shot("r3-composer");
await page.click(".pop button.primary");
await page.waitForSelector(".pop .sugg-bar");
check(true, "thread created, suggestion offered");
await page.click(".pop .sugg-bar button");
await page.waitForSelector(".pop .sugg-bar button.primary");
await shot("r4-suggestion-preview");
await page.click(".pop .sugg-bar button.primary");
await page.waitForSelector(".pop .tstatus.resolved").catch(async (e) => {
  console.log("     popover:", (await page.innerText(".pop").catch(() => "")).replace(/\s+/g, " ").slice(0, 400));
  throw e;
});
check(true, "suggestion applied (amend) and thread resolved");
await page.keyboard.press("Escape");
await idle();
const applied = await page.evaluate(async ([w, k]) => {
  const r = await fetch(`/api/v1/blobs/${window.__cv.engine.client.windows.get(w.id).blob}/text?from=${w.lines.start + k}&to=${w.lines.start + k}`);
  return (await r.json()).lines[0];
}, [w, k]);
check(applied === "renamed_value = 1", `file text updated (${JSON.stringify(applied)})`);

// Range comment: drag in the gutter over 4 lines, then widen it by 2 with the bottom handle.
{
  await page.evaluate((w) => window.__cv.engine.cam.animateTo(w.rect.x - 300, w.rect.y - 120, 0.8, 0), w);
  await idle();
  const free = await page.evaluate((w) => {
    const taken = new Set(window.__cv.engine.review.threadsIn(w).map((t) => t.anchor.start - w.lines.start));
    for (let k = 6; k < 20; k++) if (![0, 1, 2, 3, 4, 5, 6].some((d) => taken.has(k + d))) return k;
    return 6;
  }, w);
  const at = (k) => page.evaluate(([w, k]) => window.__cv.engine.cam.toScreen(w.rect.x + 20, w.rect.y + 36 + k * 18 + 9), [w, k]);
  const [a, b] = [await at(free), await at(free + 3)];
  await page.mouse.move(box.x + a[0], box.y + a[1]);
  await page.mouse.down();
  await page.mouse.move(box.x + b[0], box.y + b[1], { steps: 6 });
  await page.mouse.up();
  await page.waitForSelector(".pop textarea");
  const lines = async () => [await page.inputValue(".pop input[aria-label='first line']"), await page.inputValue(".pop input[aria-label='last line']")].map(Number);
  const [s0, e0] = await lines();
  check(e0 - s0 === 3 && s0 === w.lines.start + free, `gutter drag selects a range (${s0}–${e0})`);
  const band = await page.evaluate(() => { const r = window.__cv.engine.commentRange; return r && [r.start, r.end, r.editable]; });
  check(band && band[0] === s0 && band[1] === e0 && band[2], "commented lines highlighted on the canvas with handles");
  // Measured now, at rest (the canvas box and the window can have moved since the flow started).
  await idle();
  const bx = await (await page.$(".canvas canvas")).boundingBox();
  const h = await page.evaluate(([w, k]) => window.__cv.engine.cam.toScreen(w.rect.x + 4 + 14 / window.__cv.engine.cam.zoom * 1, w.rect.y + 36 + k * 18), [w, free + 4]);
  const h2 = await page.evaluate(([w, k]) => window.__cv.engine.cam.toScreen(w.rect.x + 20, w.rect.y + 36 + k * 18 + 9), [w, free + 5]);
  await page.mouse.move(bx.x + h[0], bx.y + h[1]);
  await page.mouse.down();
  await page.mouse.move(bx.x + h2[0], bx.y + h2[1], { steps: 6 });
  // One more move inside the target line, so the last pointermove before the release lands there.
  await page.mouse.move(bx.x + h2[0], bx.y + h2[1] + 2);
  await page.mouse.up();
  // React has the new range in the inputs.
  const widened = await until((want) => Number(document.querySelector(".pop input[aria-label='last line']")?.value) === want, e0 + 2, 30000);
  if (!widened) console.log("     handle drag:", JSON.stringify(await page.evaluate(([w, id]) => { const e = window.__cv.engine; const x = e.client.windows.get(id); return { range: e.commentRange && [e.commentRange.start, e.commentRange.end], lines: x && [x.lines.start, x.lines.end], cam: [e.cam.x, e.cam.y, e.cam.zoom] }; }, [w, w.id])), JSON.stringify({ h, h2, bx }));
  const [s1, e1] = await lines();
  check(s1 === s0 && e1 === e0 + 2, `bottom handle widens the range (${s1}–${e1})`);
  await shot("r4b-range");
  await page.fill(".pop textarea", "These lines could be one helper.");
  await page.click(".pop button.primary");
  await page.waitForSelector(".pop .comments li");
  const anchor = await page.evaluate(() => { const r = window.__cv.engine.commentRange; return r && [r.start, r.end, r.editable]; });
  check(anchor && anchor[0] === s1 && anchor[1] === e1 && !anchor[2], `thread anchored to ${s1}–${e1}, its lines stay highlighted`);
  await page.keyboard.press("Escape");
  await page.waitForSelector(".pop", { state: "detached", timeout: ms(30000) }).catch(() => {});
  await until(() => !window.__cv.engine.commentRange, undefined, 10000);
  const left = await page.evaluate(() => ({ pop: !!document.querySelector(".pop"), r: window.__cv.engine.commentRange && [window.__cv.engine.commentRange.start, window.__cv.engine.commentRange.end] }));
  check(!left.pop, "Escape closes the thread popover");
  check(!left.r, `highlight cleared when the thread closes ${left.r ? JSON.stringify(left) : ""}`);
}

// Editor: double-click the code, change the first line, preview, apply.
const mid = await page.evaluate((w) => window.__cv.engine.cam.toScreen(w.rect.x + 300, w.rect.y + 36 + 9 * 18), w);
await page.mouse.dblclick(box.x + mid[0], box.y + mid[1]);
await page.waitForSelector(".cm-host .cm-content");
await page.waitForSelector(".cm-host .cm-editor .cm-line", { timeout: ms(30000) }).catch(() => {});
await shot("r5-editor");
await page.keyboard.press("Control+Home");
await page.keyboard.type("# reviewed\n");
await page.click(".savebar button:has-text('Preview')");
await page.waitForSelector(".savebar button.primary");
await shot("r6-editor-preview");
const blob0 = await page.evaluate((id) => window.__cv.engine.client.windows.get(id)?.blob, w.id);
await page.click(".savebar button.primary");
await page.waitForSelector(".cm-host", { state: "detached" });
// The window's header gets the new blob once the refresh after edit.updated lands (slower on a big map).
await page.waitForFunction(([id, b]) => window.__cv.engine.client.windows.get(id)?.blob !== b, [w.id, blob0], { timeout: ms(30000) }).catch(() => {});
await idle();
const first = await page.evaluate(async (w) => (await (await fetch(`/api/v1/blobs/${window.__cv.engine.client.windows.get(w.id).blob}/text?from=${w.lines.start}&to=${w.lines.start}`)).json()).lines[0], w);
check(first === "# reviewed", `edit applied (${JSON.stringify(first)})`);
await shot("r7-after-edit");
// Conflicts: open one from its marker and take a side; ask the agent to resolve another.
// Conflicts still on the server now (the client's list can lag behind a resolution).
const serverConflicts = () => page.evaluate(async () => (await (await fetch("/api/v1/conflicts")).json()).map((c) => c.path));
const openConflict = async (exclude = []) => {
  const live = await serverConflicts();
  const v0 = await camV();
  const c = await page.evaluate(async ([live, exclude]) => {
    const e = window.__cv.engine;
    await e.review.loadConflicts();
    for (const [path, d] of e.review.conflicts) {
      if (!live.includes(path) || exclude.includes(path)) continue;
      const ws = (await e.locate(path)).length;
      if (!ws) continue;
      const w = await e.goTo({ path, line: d.regions[0].start });
      if (w) return { path, start: d.regions[0].start };
    }
    return null;
  }, [live, exclude]);
  if (!c) return null;
  // goTo locates, then flies (1.5 s): wait for that flight, then for the conflict's marker.
  await flown(v0);
  await until((p) => window.__cv.engine["threadMarks"].conflictHits.some((h) => h.conflict.path === p), c.path, 30000);
  const hit = await page.evaluate((path) => {
    const e = window.__cv.engine;
    const h = e["threadMarks"].conflictHits.find((h) => h.conflict.path === path);
    return h && e.cam.toScreen(h.x + h.w / 2, h.y + h.h / 2);
  }, c.path);
  if (!hit) return null;
  const dbg = await page.evaluate(([x, y, bx, by]) => {
    const el = document.elementFromPoint(bx + x, by + y);
    const t = window.__cv.engine.pick(x, y);
    return { el: el.tagName + "." + el.className, pick: t?.type, tier: window.__cv.engine.cam.tier, x, y };
  }, [hit[0], hit[1], box.x, box.y]);
  console.log("     conflict click:", JSON.stringify(dbg));
  await page.mouse.click(box.x + hit[0], box.y + hit[1]);
  await page.waitForSelector(".pop .side-box", { timeout: ms(30000) });
  return c;
};
await page.waitForFunction(() => window.__cv.engine.review.conflicts.size > 0, null, { timeout: ms(30000) }).catch(() => {});
const anyConflicts = await page.evaluate(() => window.__cv.engine.review.conflicts.size > 0);
// A repo without conflicts (e.g. both seeded ones resolved by an earlier run): seed-conflicts.py.
if (!anyConflicts) skip("conflicts", "the repo has no conflicts (scripts/seed-conflicts.py adds some)");
const c1 = anyConflicts ? await openConflict() : null;
if (anyConflicts) check(!!c1, "conflict marker opens the conflict popover");
if (c1) {
  await shot("r8-conflict");
  // Every region gets a side (the popover steps to the next unsettled one); the file is resolved
  // once the last region has one.
  const regions = await page.evaluate((p) => window.__cv.engine.review.conflicts.get(p)?.regions.length ?? 1, c1.path);
  for (let i = 0; i < regions && (await page.$(".pop .side-box")); i++) {
    const sig = await page.evaluate(() => document.querySelector(".pop .side-box")?.textContent ?? "");
    await page.click(".pop .side-box button:has-text('Take this side')");
    // The popover moved on to the next unsettled region, or closed its side boxes.
    await until((sig) => (document.querySelector(".pop .side-box")?.textContent ?? "") !== sig, sig, 30000);
  }
  await page.waitForSelector(".pop .side-box", { state: "detached" });
  check(true, `took a side in each of ${regions} region(s)`);
  const left = await page.evaluate(async (p) => (await (await fetch("/api/v1/conflicts")).json()).some((c) => c.path === p), c1.path);
  check(!left, "taking a side resolves the conflict");
  const c2 = await openConflict([c1.path]);
  if (!c2) skip("ask-agent", "no second conflicted file left on the server");
  if (c2) {
    // The agent must resolve a conflict that is really there: it is on the server's list right now.
    const before = await serverConflicts();
    check(before.includes(c2.path) && c2.path !== c1.path, `a second, still unresolved conflict is open (${c2.path.split("/").pop()})`);
    // Resolutions announced from now on (conflicts.changed), to tell a real one from a listing blip.
    await page.evaluate(() => {
      const got = (window.__resolved = []);
      window.__cv.live.on((ev) => ev.type === "conflicts.changed" && got.push(...ev.resolved.map((c) => c.path)));
    });
    const submitted = page.waitForRequest((r) => r.url().endsWith("/reviews:submit") && r.method() === "POST", { timeout: ms(30000) }).then(() => true).catch(() => false);
    await page.click(".pop button:has-text('Ask agent to resolve')");
    check(await submitted, "“Ask agent” sends the task thread with reviews:submit");
    // cv-mock simulates the agent; on cv serve an agent must be attached to resolve it: AGENT=<name>
    // says one is (scripts/scripted-agent.mjs in CI, or agent-loop.mjs with a real model).
    if (process.env.NO_SIM_AGENT && !process.env.AGENT) skip("agent-resolution", "NO_SIM_AGENT is set and no AGENT is attached");
    else {
      // Resolved only if it was listed before the ask (checked above) and is gone now: without an
      // agent this fails instead of passing on a stale or already-resolved conflict.
      // Resolved: announced (conflicts.changed) and no longer listed in two polls a second apart.
      // Polled from here, not with page.waitForFunction: that does not await an async predicate (the
      // Promise is truthy, so it "passed" at once and an unresolved conflict read as announced, then
      // listed again).
      const listed = () => page.evaluate(async () => { const r = await fetch("/api/v1/conflicts"); const l = r.ok ? await r.json() : null; return Array.isArray(l) ? l.map((c) => c.path) : null; });
      let resolved = false;
      // While nothing announced it resolved, every listing must still have it (no blips).
      let polls = 0, unlisted = 0;
      // The agent's own pickup is bounded by its wait_for_events long-poll (up to 30 s) under load.
      for (const deadline = Date.now() + ms(120000); !resolved && Date.now() < deadline; ) {
        if (await page.evaluate((p) => window.__resolved.includes(p), c2.path)) {
          // Two listings at least a second apart (the server's op-watcher safety poll is 1 s).
          const a = await listed();
          await sleep(1000);
          const b = await listed();
          resolved = [a, b].every((l) => l && !l.includes(c2.path));
        } else {
          const l = await listed();
          polls++;
          if (!l?.includes(c2.path) && !(await page.evaluate((p) => window.__resolved.includes(p), c2.path))) unlisted++;
        }
        if (!resolved) await sleep(500); // poll interval
      }
      if (!resolved) check(unlisted === 0, `the unresolved conflict stayed listed and was never announced resolved (${polls - unlisted}/${polls} listings had it)`);
      let stays = false;
      if (resolved) {
        // It sticks: three listings in a row without it, each at least a second apart (counted, not
        // timed); bounded at 20 listings.
        let clean = 0;
        for (let i = 0; i < 20 && clean < 3; i++) {
          clean = (await serverConflicts()).includes(c2.path) ? 0 : clean + 1;
          if (clean < 3) await sleep(1000);
        }
        stays = clean >= 3;
      }
      check(resolved && stays && before.includes(c2.path), `agent resolved the conflict${resolved && !stays ? " (announced, but listed again: not resolved)" : ""}${resolved ? "" : " (not announced or still listed: is an agent attached? cv-mock simulates one; NO_SIM_AGENT=1 skips this on cv serve)"}`);
      const shown = await page.waitForFunction(() => document.querySelector(".pop .agent-res b.add")?.textContent, null, { timeout: ms(30000) }).then((h) => h.jsonValue()).catch(() => null);
      const text = await page.evaluate(() => document.querySelector(".pop .agent-res pre.resolved")?.textContent ?? "");
      check(!!shown && (!process.env.AGENT || shown.includes(process.env.AGENT)), `the agent's resolution is shown in the popover (${shown}${text ? `: ${JSON.stringify(text.split("\n")[0])}` : ""})`);
      if (process.env.AGENT) {
        // The task thread got the agent's reply (MCP reply with setStatus) and the jj op names the agent.
        const th = await page.evaluate(async (p) => (await (await fetch("/api/v1/threads")).json()).find((t) => t.anchor.path === p && t.kind === "task" && t.comments.some((c) => c.author.kind === "agent")), c2.path);
        check(!!th && th.status !== "open", `the agent replied in the task thread and moved it on (${th ? `${th.status}: ${JSON.stringify(th.comments.at(-1).body.slice(0, 60))}` : "no reply"})`);
      }
    }
    await shot("r9-agent-conflict");
    await page.keyboard.press("Escape");
  }
}
// The agent answers a comment with a code change (cv serve with an agent attached; in CI
// scripts/scripted-agent.mjs: one edit_code {thread, patches, reply} of several patches). The open
// popover shows the reply and the new status live, the window gets the new code, and the thread
// stays addressed on the new lines through the next jj operations (a1).
if (!process.env.AGENT) skip("agent-answer", "no agent attached (AGENT unset); on cv serve run scripts/scripted-agent.mjs and set AGENT");
else {
  await page.keyboard.press("Escape");
  const t = await page.evaluate(async (avoid) => {
    const e = window.__cv.engine;
    await e.review.loadConflicts();
    const ws = [...e.client.windows.values()].filter((x) => x.lane === "code" && !e.review.conflicts.has(x.path) && x.path !== avoid && x.lines.end - x.lines.start > 8 && !e.review.threadsIn(x).length);
    const x = ws[0];
    if (!x) return null;
    const start = x.lines.start + 3;
    const th = await e.review.create({ anchor: { path: x.path, rev: x.change, start, end: start + 1 }, body: "Explain these two lines in a comment.", kind: "comment" });
    // Frame the thread's line.
    e.cam.animateTo(x.rect.x - 300, x.rect.y + 36 + 3 * 18 - 300, 0.8, 0);
    return { id: th.id, window: x.id, path: x.path, blob: x.blob };
  }, w.path);
  check(!!t, `a comment thread for the agent on a code window (${t?.path})`);
  if (t) {
    await idle();
    const m = await page.evaluate((id) => {
      const e = window.__cv.engine;
      const h = e["threadMarks"].hits.find((h) => h.thread.id === id);
      return h && e.cam.toScreen(h.x + h.w / 2, h.y + h.h / 2);
    }, t.id);
    if (m) {
      await page.mouse.click(box.x + m[0], box.y + m[1]);
      await page.waitForSelector(".pop .comments li", { timeout: ms(30000) }).catch(() => {});
    }
    const popOpen = !!(await page.$(".pop .comments li"));
    check(popOpen, "the thread's popover is open while the agent works");
    // Send it to the agent, as "Send to agent" does.
    await page.evaluate((id) => window.__cv.engine.review.submit([id]), t.id);
    // Live: the reply and the status arrive in the open popover (thread.upserted over the WebSocket).
    const live = await page.waitForFunction(() => {
      const st = document.querySelector(".pop .tstatus");
      const li = [...document.querySelectorAll(".pop .comments li")].map((x) => x.textContent ?? "");
      return st?.classList.contains("addressed") && li.some((x) => x.includes("scripted agent")) ? li.at(-1) : false;
    }, null, { timeout: ms(30000) }).then((h) => h.jsonValue()).catch(() => null);
    check(!!live, `the agent's reply and "addressed" show in the open popover (${live ? JSON.stringify(live.slice(0, 70)) : "not shown"})`);
    await shot("r9b-agent-answer");
    // The new code: the window's blob moves on, and the thread's lines in it are the agent's new text.
    await page.waitForFunction(([id, b]) => window.__cv.engine.client.windows.get(id)?.blob !== b, [t.window, t.blob], { timeout: ms(30000) }).catch(() => {});
    const anchored = async () => page.evaluate(async ([id, wid]) => {
      const e = window.__cv.engine;
      const th = e.review.threads.get(id);
      const blob = e.client.windows.get(wid)?.blob;
      if (!th || !blob) return null;
      const r = await fetch(`/api/v1/blobs/${blob}/text?from=${th.anchor.start}&to=${th.anchor.end}`);
      return { status: th.status, outdated: !!th.outdated, lines: r.ok ? (await r.json()).lines : null, start: th.anchor.start, end: th.anchor.end, change: th.anchor.change, commit: th.anchor.commit };
    }, [t.id, t.window]);
    const a1 = await anchored();
    check(!!a1 && !a1.outdated && a1.lines?.[0]?.includes("# checked (scripted agent"), `the window shows the new code and the thread sits on it (${JSON.stringify(a1)})`);
    const served = async () => page.evaluate(async (id) => (await (await fetch(`/api/v1/threads/${id}`)).json()), t.id);
    // One more operation that rewrites the thread's change: with jj in REPO a working-copy snapshot
    // and a describe of that change; else (no REPO) an edit of another file through the API.
    const ops0 = await page.evaluate(() => {
      window.__ops = 0;
      window.__cv.live.on((ev) => ev.type === "op.changed" && window.__ops++);
      return 0;
    });
    if (process.env.REPO) {
      writeFileSync(`${process.env.REPO}/e2e-after-answer.txt`, "one more op\n");
      const jj = (...a) => execFileSync("jj", ["-R", process.env.REPO, "--no-pager", ...a], { stdio: "pipe" });
      jj("status");
      jj("describe", "-r", a1.change, "-m", "e2e: the change the agent answered in, described again");
    } else {
      await page.evaluate(async (wid) => {
        const e = window.__cv.engine;
        const x = e.client.windows.get(wid);
        await e.review.edit(x, x.lines.start, x.lines.start, "# one more op", "amend", false);
      }, w.id);
    }
    await page.waitForFunction((n) => window.__ops > n, ops0, { timeout: ms(30000) }).catch(() => {});
    // With REPO two jj ops (a snapshot, a describe) that the server may see as one or two syncs:
    // wait for their result, the thread's anchor on the rewritten commit.
    if (process.env.REPO) await pollNode(async () => (await served())?.anchor?.commit !== a1.commit, { timeout: 60000 });
    await idle();
    const after = await served();
    const ui = (await pollNode(async () => { const u = await anchored(); return u?.lines?.[0]?.includes("# checked (scripted agent") ? u : undefined; }, { timeout: 30000 })) ?? (await anchored());
    if (process.env.REPO) check(after.anchor.commit !== a1.commit, `the jj op rewrote the thread's change (anchor ${a1.commit.slice(0, 8)} → ${after.anchor.commit.slice(0, 8)})`);
    check(after.status === "addressed" && !after.outdated, `after one more jj op the thread is still addressed on the server (${after.status}${after.outdated ? `, outdated: ${after.outdated.reason}` : ""})`);
    check(!!ui && ui.status === "addressed" && !ui.outdated && ui.lines?.[0]?.includes("# checked (scripted agent"), `…and in the UI, on the agent's lines (${JSON.stringify(ui)})`);
    await page.keyboard.press("Escape");
  }
}
// Fades and expansion: a window that starts mid-file shows "↑ N lines above"; +20 widens it in place.
const ex = await page.evaluate(() => {
  const e = window.__cv.engine;
  const w = [...e.client.windows.values()].find((w) => w.lane === "code" && w.lines.start > 25 && !w.flags.minimized);
  if (!w) return null;
  e.select(undefined);
  e.cam.animateTo(w.rect.x - 300, w.rect.y - 150, 0.8, 0);
  return w;
});
if (ex) {
  await idle();
  await until((id) => !!window.__cv.engine["code"].get(id)?.hits.some((h) => h.req.before === 20), ex.id, 30000);
  const pill = await page.evaluate((id) => {
    const e = window.__cv.engine;
    const c = e["code"].get(id);
    const h = c?.hits.find((h) => h.req.before === 20);
    return h && e.cam.toScreen(h.x + 10, h.y + h.h / 2);
  }, ex.id);
  check(!!pill, "fade hint with expand pill drawn above a mid-file window");
  if (pill) {
    await shot("r10-fade");
    await page.mouse.click(box.x + pill[0], box.y + pill[1]);
    // By key: on cv serve the expand is a layout change, and the reloaded window has a new id.
    await page.waitForFunction(([key, s]) => [...window.__cv.engine.client.windows.values()].some((w) => w.key === key && w.lines.start === s - 20), [ex.key, ex.lines.start], { timeout: ms(30000) })
      .then(() => check(true, "expand widened the window by 20 lines"))
      .catch(() => check(false, "expand widened the window by 20 lines"));
    await idle();
    await shot("r11-expanded");
  }
} else skip("fade", "no loaded code window starts below line 25");
check(errors.length === 0, `no page errors ${errors.length ? JSON.stringify(errors.slice(0, 3)) : ""}`);
await browser.close();
