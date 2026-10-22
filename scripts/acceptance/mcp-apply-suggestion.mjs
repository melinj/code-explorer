// B9 acceptance on a real repo, without a real Claude: drive cv's MCP endpoint over Streamable HTTP
// as an agent would, while a WebSocket client (standing in for the UI) asserts the events.
//
// Usage: node scripts/acceptance/mcp-apply-suggestion.mjs <server base http://127.0.0.1:PORT> <token>
//          <repo dir> [--workspace default] [--path P --line N] [--out result.json]
// The repo must be a THROWAWAY jj clone served by `cv serve` (this script amends a change in it).
//
// Steps:
//   1. UI side: open /api/v1/ws and remember the cursor from hello.
//   2. Human: POST /api/v1/threads — a "suggestion" thread assigned to the agent, anchored on one
//      line of a Python file in the workspace's @ change, with a ```suggestion block.
//   3. Agent (MCP /mcp/full): initialize → bind_workspace → wait_for_events (thread.upserted) →
//      get_thread → apply_suggestion { mode: "amend" } → reply { setStatus: "addressed" }.
//   4. Assert: the UI socket received thread.upserted (create and reply), edit.updated (applied),
//      op.changed and workspace.updated; and `jj file show` of the change has the suggested line.
import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";

const args = process.argv.slice(2);
const opt = (n, d) => (args.includes(n) ? args[args.indexOf(n) + 1] : d);
const [base, token, repo] = args;
if (!base || !token || !repo) throw new Error("usage: mcp-apply-suggestion.mjs <base> <token> <repo> [--workspace W] [--path P --line N] [--out F]");
const workspace = opt("--workspace", "default");
const out = opt("--out");
const auth = { authorization: `Bearer ${token}` };
const checks = [];
const check = (ok, what) => {
  checks.push({ ok: !!ok, what });
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
};
const log = [];

// ── 1. UI socket ──
const events = [];
const ws = new WebSocket(`${base.replace(/^http/, "ws")}/api/v1/ws?token=${token}`);
let hello;
ws.onmessage = (m) => {
  const f = JSON.parse(m.data);
  if (f.event.type === "hello") hello = f.event;
  else events.push({ at: Date.now(), seq: f.seq, ev: f.event });
};
await new Promise((r, j) => ((ws.onopen = r), (ws.onerror = j)));
for (let i = 0; i < 50 && !hello; i++) await new Promise((r) => setTimeout(r, 50));
const waitEvent = async (pred, ms = 10000) => {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    const e = events.find((e) => pred(e.ev));
    if (e) return e;
    await new Promise((r) => setTimeout(r, 25));
  }
  return undefined;
};

const api = async (method, path, body) => {
  const r = await fetch(`${base}/api/v1${path}`, {
    method,
    headers: { ...auth, ...(body ? { "content-type": "application/json" } : {}) },
    body: body ? JSON.stringify(body) : undefined,
  });
  const text = await r.text();
  if (!r.ok) throw new Error(`${method} ${path}: ${r.status} ${text}`);
  return text ? JSON.parse(text) : null;
};

// ── 2. The human's suggestion thread ──
const wss = await api("GET", "/workspaces");
const wsInfo = (Array.isArray(wss) ? wss : wss.workspaces).find((w) => w.name === workspace);
if (!wsInfo) throw new Error(`no workspace ${workspace}`);
const change = wsInfo.wc.change;
const jj = (...a) => execFileSync("jj", ["--ignore-working-copy", ...a], { cwd: repo, encoding: "utf8" });
let path = opt("--path");
let line = Number(opt("--line", 0));
if (!path) {
  // A Python file in the change's tree with a simple local assignment, not conflicted.
  const files = jj("file", "list", "-r", change).split("\n").filter((f) => /^src\/.*\.py$/.test(f)).sort();
  for (const f of files) {
    let text;
    try {
      text = jj("file", "show", "-r", change, f);
    } catch {
      continue;
    }
    if (text.includes("<<<<<<<")) continue;
    const i = text.split("\n").findIndex((l) => /^\s+[a-z_]{3,} = \d+$/.test(l));
    if (i >= 0) {
      path = f;
      line = i + 1;
      break;
    }
  }
}
const before = jj("file", "show", "-r", change, path).split("\n");
const old = before[line - 1];
const [, indent, name, value] = old.match(/^(\s*)(\w+) = (\d+)$/) ?? [null, "", "x", "0"];
const replacement = `${indent}${name} = ${Number(value) + 1}  # cv acceptance`;
const thread = await api("POST", "/threads", {
  anchor: { path, rev: change, start: line, end: line },
  body: `Bump this.\n\n\`\`\`suggestion\n${replacement}\n\`\`\``,
  kind: "suggestion",
  assignee: "agent",
});
log.push({ step: "POST /threads", thread: thread.id, path, line, old, replacement, change });
const created = await waitEvent((e) => e.type === "thread.upserted" && e.thread?.id === thread.id);
check(created, `UI got thread.upserted for the new thread ${thread.id}`);

// ── 3. The agent over MCP (Streamable HTTP) ──
let session;
let rpcId = 0;
const rpc = async (method, params, notify = false) => {
  const body = notify ? { jsonrpc: "2.0", method, params } : { jsonrpc: "2.0", id: ++rpcId, method, params };
  const r = await fetch(`${base}/mcp/full`, {
    method: "POST",
    headers: {
      ...auth,
      "content-type": "application/json",
      accept: "application/json, text/event-stream",
      ...(session ? { "mcp-session-id": session } : {}),
    },
    body: JSON.stringify(body),
  });
  session ??= r.headers.get("mcp-session-id") ?? undefined;
  if (notify) return null;
  const text = await r.text();
  if (!r.ok) throw new Error(`${method}: ${r.status} ${text}`);
  const msgs = (r.headers.get("content-type") ?? "").includes("event-stream")
    ? text
        .split("\n")
        .filter((l) => l.startsWith("data:"))
        .map((l) => l.slice(5).trim())
        .filter(Boolean)
        .map((d) => JSON.parse(d))
    : [JSON.parse(text)];
  const m = msgs.find((m) => m.id === body.id);
  if (!m) throw new Error(`${method}: no response in ${text.slice(0, 300)}`);
  if (m.error) throw new Error(`${method}: ${JSON.stringify(m.error)}`);
  return m.result;
};
const tool = async (name, a) => {
  const t0 = Date.now();
  const res = await rpc("tools/call", { name, arguments: a });
  const text = (res.content ?? []).filter((c) => c.type === "text").map((c) => c.text).join("\n");
  let json;
  try {
    json = JSON.parse((res.content ?? [])[0]?.text ?? "");
  } catch {}
  log.push({ tool: name, args: a, ms: Date.now() - t0, isError: !!res.isError, text: text.slice(0, 600) });
  return { res, text, json };
};

const init = await rpc("initialize", {
  protocolVersion: "2025-06-18",
  capabilities: {},
  clientInfo: { name: "cv-acceptance", version: "1" },
});
await rpc("notifications/initialized", {}, true);
check(init?.serverInfo, `initialize: ${init?.serverInfo?.name} ${init?.serverInfo?.version ?? ""}`);

const bound = await tool("bind_workspace", { workspace });
check(!bound.res.isError && bound.text.includes(workspace), `bind_workspace ${workspace}: ${bound.text.slice(0, 120)}`);

const waited = await tool("wait_for_events", { cursor: hello?.cursor, kinds: ["thread.upserted"], timeoutS: 5 });
check(!waited.res.isError && waited.text.includes(thread.id), "wait_for_events returned the thread.upserted for the new thread");

const got = await tool("get_thread", { thread: thread.id });
check(!got.res.isError && got.text.includes(path), `get_thread shows ${path}`);

const nEdit = events.length;
const applied = await tool("apply_suggestion", { thread: thread.id, mode: "amend" });
check(!applied.res.isError && /applied/.test(applied.text), `apply_suggestion amend: ${applied.text.slice(0, 160).replace(/\n/g, " ")}`);

const replied = await tool("reply", { thread: thread.id, body: "Applied the suggestion.", setStatus: "addressed" });
check(!replied.res.isError, `reply: ${replied.text.slice(0, 120).replace(/\n/g, " ")}`);

// ── 4. What the UI saw ──
const after = (pred) => waitEvent((e) => pred(e) && events.indexOf(events.find((x) => x.ev === e)) >= nEdit);
const editEv = await after((e) => e.type === "edit.updated" && e.edit?.status === "applied");
check(editEv, `UI got edit.updated (applied)${editEv ? ` ${editEv.ev.edit.id}` : ""}`);
const opEv = await after((e) => e.type === "op.changed");
check(opEv, "UI got op.changed for the amend");
const wsEv = await after((e) => e.type === "workspace.updated" && e.workspace?.name === workspace);
check(wsEv, `UI got workspace.updated for ${workspace}`);
const replyEv = await after((e) => e.type === "thread.upserted" && e.thread?.id === thread.id && e.thread.status === "addressed");
check(replyEv, "UI got thread.upserted with status addressed and the agent's reply");
const attn = events.some((e) => e.ev.type === "agent.attention");
check(attn, "UI got agent.attention while the agent worked");

const now = jj("file", "show", "-r", change, path).split("\n");
check(now[line - 1] === replacement, `change ${change} now has the suggested line ${line}: ${JSON.stringify(now[line - 1])}`);
check(now.length === before.length, "only that line changed (same line count)");

ws.close();
const kinds = [...new Set(events.map((e) => e.ev.type))];
const summary = {
  passed: checks.every((c) => c.ok),
  checks,
  eventsSeen: kinds,
  eventOrder: events.map((e) => e.ev.type + (e.ev.edit ? `(${e.ev.edit.status})` : e.ev.thread ? `(${e.ev.thread.status})` : "")),
  log,
};
if (out) writeFileSync(out, JSON.stringify(summary, null, 2));
console.log(`\n${summary.passed ? "PASS" : "FAIL"} — events: ${summary.eventOrder.join(", ")}`);
if (!summary.passed) process.exitCode = 1;
