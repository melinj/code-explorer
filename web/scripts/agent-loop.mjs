// The review loop with a real coding agent, watched through the UI:
//   1. a human asks the agent for a change through the canvas composer ("Ask agent"),
//   2. Claude Code runs headless with only the cv MCP tools (cv-mcp stdio → the server),
//   3. the UI shows the agent working (presence, feed) and receives its reply and code change live.
// Usage: node scripts/agent-loop.mjs <out dir> [--server http://127.0.0.1:7878] [--url http://127.0.0.1:5179]
//          [--mcp <cv-mcp binary> | --mcp-http] [--budget USD] [--repo <served repo>]
//          (CV_TOKEN: the server's bearer token). With --repo, one more jj operation follows the agent's
//          answer (a working-copy snapshot and a describe of the answered change) and the thread must
//          stay addressed, on the agent's lines (a1).
// Costs one real `claude -p` run (capped by --max-budget-usd).
import { chromium } from "playwright-core";
import { chrome } from "./chrome.mjs";
import { execFileSync, spawn } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const args = process.argv.slice(2);
const opt = (n, d) => (args.includes(n) ? args[args.indexOf(n) + 1] : d);
const out = resolve(args[0] ?? "agent-loop");
const server = opt("--server", "http://127.0.0.1:7878");
const url = opt("--url", "http://127.0.0.1:5179");
const mcpBin = opt("--mcp", resolve(import.meta.dirname, "../../target/release/cv-mcp"));
mkdirSync(out, { recursive: true });
const check = (ok, what) => {
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
  if (!ok) process.exitCode = 1;
};

const browser = await chromium.launch({ ...chrome(), args: ["--ignore-gpu-blocklist", "--use-angle=vulkan", "--enable-features=Vulkan"] });
const page = await browser.newPage({ viewport: { width: 1600, height: 1000 }, colorScheme: "dark" });
await page.goto(url);
await page.waitForFunction(() => !!window.__cv);
const shot = (n) => page.screenshot({ path: `${out}/${n}.png` });

// 1. The human's request, through the composer, on a code window with a local assignment.
// Load code around the hottest code clusters in turn (real repos can frame review-first below
// tile zoom) until one has a suitable local.
const frame = (k) =>
  page.evaluate((k) => {
    const e = window.__cv.engine;
    const c = e.client.boot.clusters.filter((c) => c.lane === "code").sort((a, z) => z.heat - a.heat || z.windows - a.windows)[k];
    if (!c) return false;
    e.frameRect(c.rect, 0);
    if (e.cam.zoom < 0.04) e.cam.animateTo(c.rect.x - 200, c.rect.y - 200, 0.05, 0);
    return true;
  }, k);
const pick = () => page.evaluate(async () => {
  const e = window.__cv.engine;
  await e.review.loadConflicts();
  for (const w of e.client.windows.values()) {
    if (w.lane !== "code" || !w.path.endsWith(".py") || w.lines.end - w.lines.start < 12 || e.review.conflicts.has(w.path)) continue;
    const t = await (await fetch(`/api/v1/blobs/${window.__cv.engine.client.windows.get(w.id).blob}/text?from=${w.lines.start}&to=${w.lines.end}`)).json();
    const cands = t.lines.map((l, i) => i).filter((i) => /^\s+[a-z][a-z_]{2,} = \d+$/.test(t.lines[i]));
    if (!cands.length) continue;
    // A local whose every use in its function (not attributes such as self.<name>) is listed in the
    // request, so renaming exactly those lines is a complete, safe change a careful agent will make.
    const all = (await (await fetch(`/api/v1/blobs/${window.__cv.engine.client.windows.get(w.id).blob}/text`)).json()).lines;
    const indent = (l) => l.match(/^\s*/)[0].length;
    for (const i of cands) {
      const at = t.from + i - 1; // 0-based index into `all`
      const name = t.lines[i].trim().split(" ")[0];
      let def = at;
      while (def >= 0 && !(/^\s*(async\s+)?def\s/.test(all[def]) && indent(all[def]) < indent(all[at]))) def--;
      if (def < 0) continue;
      let end = def + 1;
      while (end < all.length && (all[end].trim() === "" || indent(all[end]) > indent(all[def]))) end++;
      const re = new RegExp(`(?<![.\\w])${name}\\b`);
      const hits = [];
      for (let k = def; k < end; k++) if (re.test(all[k])) hits.push([k + 1, all[k]]);
      if (hits.length < 2 || hits.length > 6 || hits[0][0] !== t.from + i || hits.at(-1)[0] - hits[0][0] > 40) continue;
      return { w, line: t.from + i, name, text: t.lines[i], uses: hits.slice(1) };
    }
  }
  return null;
});
let target = null;
for (let k = 0; k < 8 && !target; k++) {
  if (!(await frame(k))) break;
  await page.waitForFunction(() => window.__cv.engine.stats().windowsLoaded > 50, null, { timeout: 20000 }).catch(() => {});
  target = await pick();
}
if (!target) throw new Error("no suitable window loaded");
if (args.includes("--pick-only")) {
  console.log(JSON.stringify({ path: target.w.path, line: target.line, name: target.name, text: target.text, uses: target.uses }));
  await browser.close();
  process.exit(0);
}
await page.evaluate((t) => {
  const e = window.__cv.engine;
  e.cam.animateTo(t.w.rect.x - 300, t.w.rect.y + 36 + (t.line - t.w.lines.start) * 18 - 300, 0.8, 0);
  e.onAction({ type: "compose", window: t.w, line: t.line });
}, target);
await page.waitForSelector(".pop textarea");
const useLines = target.uses.map(([n]) => n);
const ask = `Rename the local \`${target.name}\` to \`${target.name}_value\` throughout its function: it is assigned on line ${target.line} and used on line${useLines.length > 1 ? "s" : ""} ${useLines.join(", ")}. Also add a short trailing comment on line ${target.line} saying what the number is for.`;
await page.fill(".pop textarea", ask);
const before = await page.evaluate(() => [...window.__cv.engine.review.threads.keys()]);
await page.click(".pop button:has-text('Ask agent')");
await page.waitForSelector(".pop .comments li");
// The new thread is the one that wasn't there before.
const threadId = await page.evaluate((b) => [...window.__cv.engine.review.threads.keys()].find((id) => !b.includes(id)), before);
check(!!threadId, `thread ${threadId} created through the UI and assigned to the agent`);
await page.click(".tabs button:has-text('Agent')");
await shot("a1-asked");

// 2. A real coding agent, with only the cv MCP tools.
const cfg = `${out}/mcp.json`;
// --mcp-http: cv serve's own /mcp (Streamable HTTP, tools in-process); else the stdio shim.
const viaHttp = args.includes("--mcp-http");
const auth = process.env.CV_TOKEN ? { Authorization: `Bearer ${process.env.CV_TOKEN}` } : {};
const cvServer = viaHttp
  ? { type: "http", url: `${server}/mcp/editor`, headers: auth }
  : { type: "stdio", command: mcpBin, args: ["stdio", "--server", server, "--profile", "editor"], env: process.env.CV_TOKEN ? { CV_TOKEN: process.env.CV_TOKEN } : {} };
writeFileSync(cfg, JSON.stringify({ mcpServers: { cv: cvServer } }));
const prompt = [
  "You are a coding agent working through the cv MCP server on a jj repository.",
  "A human reviewer assigned you a review thread. Find the open threads assigned to you together with their code (list_threads with withCode),",
  "then make exactly the change the reviewer asked for with one edit_code call that names the thread (it amends the change that owns the lines) and carries your reply saying what you changed (the reply sets the thread to addressed).",
  "Do not change anything else.",
].join(" ");
const t0 = Date.now();
const claude = spawn("claude", ["-p", prompt, "--mcp-config", cfg, "--strict-mcp-config", "--allowedTools", "mcp__cv__*", "--tools", "", "--max-budget-usd", opt("--budget", "2"), "--output-format", "stream-json", "--verbose"], { stdio: ["ignore", "pipe", "pipe"] });
const log = [];
claude.stdout.on("data", (d) => log.push(d.toString()));
claude.stderr.on("data", (d) => log.push(d.toString()));
let shotWorking = false;
const watcher = setInterval(async () => {
  if (shotWorking) return;
  const working = await page.evaluate(() => !!document.querySelector(".agent-chip")).catch(() => false);
  if (working) {
    shotWorking = true;
    await shot("a2-agent-working").catch(() => {});
  }
}, 500);
const code = await new Promise((r) => claude.on("close", r));
clearInterval(watcher);
writeFileSync(`${out}/claude.jsonl`, log.join(""));
check(code === 0, `claude exited ${code} after ${((Date.now() - t0) / 1000).toFixed(0)} s`);

// 3. What the human sees.
await page.waitForFunction((id) => window.__cv.engine.review.threads.get(id)?.status === "addressed", threadId, { timeout: 15000 })
  .then(() => check(true, "the thread turned 'addressed' in the UI (live event)"))
  .catch(() => check(false, "the thread turned 'addressed' in the UI (live event)"));
const reply = await page.evaluate((id) => {
  const t = window.__cv.engine.review.threads.get(id);
  return t.comments.filter((c) => c.author.kind === "agent").map((c) => c.body).join("\n---\n");
}, threadId);
check(reply.length > 0, "the agent's reply shows with an agent author");
// Read through the UI's own (live-refreshed) header for the window: its blob is the new text's.
const [l1, ...after] = await page.evaluate(async ([t, lines]) => {
  const get = async (n) => (await (await fetch(`/api/v1/blobs/${window.__cv.engine.client.windows.get(t.w.id).blob}/text?from=${n}&to=${n}`)).json()).lines[0];
  return Promise.all([t.line, ...lines].map(get));
}, [target, useLines]);
const renamed = (l) => new RegExp(`(?<![.\\w])${target.name}_value\\b`).test(l) && !new RegExp(`(?<![.\\w])${target.name}\\b`).test(l);
check(renamed(l1) && l1.includes("#"), `line ${target.line}: ${JSON.stringify(target.text)} → ${JSON.stringify(l1)}`);
target.uses.forEach(([n, text], k) => check(renamed(after[k]), `line ${n}: ${JSON.stringify(text)} → ${JSON.stringify(after[k])}`));
await page.evaluate((t) => window.__cv.engine.onAction({ type: "thread", thread: window.__cv.engine.review.threads.get(t), window: window.__cv.engine.client.windows.get(window.__cv.engine.selected?.w?.id ?? "") ?? [...window.__cv.engine.client.windows.values()][0] }), threadId).catch(() => {});
await page.waitForTimeout(1500);
await shot("a3-done");
// 4. One more jj operation after the answer: the thread stays addressed on its new lines.
let afterOp;
if (opt("--repo")) {
  const repo = opt("--repo");
  const th0 = await page.evaluate(async (id) => (await (await fetch(`/api/v1/threads/${id}`)).json()), threadId);
  writeFileSync(`${repo}/.agent-loop-after-answer`, "one more op\n");
  const jj = (...a) => execFileSync("jj", ["-R", repo, "--no-pager", ...a], { stdio: "pipe" }).toString();
  jj("status");
  jj("describe", "-r", th0.anchor.change, "-m", `${jj("log", "--no-graph", "-r", th0.anchor.change, "-T", "description").trim()} (described again)`);
  await page.waitForTimeout(4000);
  const th1 = await page.evaluate(async (id) => (await (await fetch(`/api/v1/threads/${id}`)).json()), threadId);
  const ui = await page.evaluate((id) => window.__cv.engine.review.threads.get(id)?.status, threadId);
  const quoted = th1.anchor.path.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
  const lines = jj("file", "show", "--ignore-working-copy", "-r", th1.anchor.commit, `root-file:"${quoted}"`).split("\n").slice(th1.anchor.start - 1, th1.anchor.end);
  afterOp = { before: th0.anchor, after: th1.anchor, status: th1.status, outdated: th1.outdated, ui, lines };
  check(th1.anchor.commit !== th0.anchor.commit, `the jj op rewrote the answered change (${th0.anchor.commit.slice(0, 8)} → ${th1.anchor.commit.slice(0, 8)})`);
  check(th1.status === "addressed" && !th1.outdated && ui === "addressed", `after one more jj op the thread is still addressed (server ${th1.status}${th1.outdated ? `, outdated: ${th1.outdated.reason}` : ""}; UI ${ui}); anchored lines ${th1.anchor.start}–${th1.anchor.end}: ${JSON.stringify(lines)}`);
}
writeFileSync(`${out}/result.json`, JSON.stringify({ threadId, ask, reply, before: [target.text, ...target.uses.map(([, x]) => x)], after: [l1, ...after], afterOp }, null, 2));
await browser.close();
