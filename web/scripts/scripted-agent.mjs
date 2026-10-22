// A scripted coding agent for end-to-end tests: no LLM, just the cv MCP tools over Streamable HTTP
// (POST /mcp), the way a real agent drives them. It binds a workspace, waits for review work
// (wait_for_events), and for every open thread assigned to the agent:
//   - in a conflicted file: get_conflict, resolve_conflict with the first side of every region
//     (the whole file, no markers), then a reply that marks the thread addressed;
//   - otherwise (a comment on code): answers it with a code change, the way a real agent does: one
//     edit_code {thread, patches, reply} of two patches (a comment line above the anchored lines and
//     a header line at the top of the file), which applies and marks the thread addressed.
// Usage: node scripts/scripted-agent.mjs --server http://127.0.0.1:8086 [--token T] [--workspace default]
//          [--name scripted] [--seconds 600] [--once]
// CV_TOKEN works for --token. Logs one line per action to stdout; exits after --seconds (or after the
// first handled thread with --once). Never point it at a repo you care about: it resolves conflicts.
const args = process.argv.slice(2);
const opt = (n, d) => (args.includes(n) ? args[args.indexOf(n) + 1] : d);
const server = opt("--server", "http://127.0.0.1:8086").replace(/\/$/, "");
const token = opt("--token", process.env.CV_TOKEN);
const workspace = opt("--workspace", "default");
const name = opt("--name", "scripted");
const seconds = Number(opt("--seconds", "600"));
const once = args.includes("--once");
const log = (...a) => console.log(`[scripted-agent ${new Date().toISOString().slice(11, 19)}]`, ...a);

/** A minimal MCP client over Streamable HTTP: JSON or SSE answers, the session id in a header. */
class Mcp {
  constructor(url) {
    this.url = url;
    this.session = undefined;
    this.id = 0;
  }
  headers() {
    return {
      accept: "application/json, text/event-stream",
      "content-type": "application/json",
      "mcp-protocol-version": "2025-06-18",
      "x-cv-agent": name,
      "x-cv-workspace": workspace,
      ...(token ? { authorization: `Bearer ${token}` } : {}),
      ...(this.session ? { "mcp-session-id": this.session } : {}),
    };
  }
  async post(body) {
    // Bounded: wait_for_events long-polls at most 30 s; a hung server must not hang the agent.
    const r = await fetch(this.url, { method: "POST", headers: this.headers(), body: JSON.stringify(body), signal: AbortSignal.timeout(120_000) });
    const sid = r.headers.get("mcp-session-id");
    if (sid) this.session = sid;
    if (!r.ok) throw new Error(`${body.method}: HTTP ${r.status} ${(await r.text()).slice(0, 200)}`);
    const text = await r.text();
    if (!text.trim()) return undefined;
    if (!(r.headers.get("content-type") ?? "").includes("text/event-stream")) return JSON.parse(text);
    // SSE: the answer is the data line carrying our id.
    for (const line of text.split("\n")) {
      const data = line.startsWith("data:") ? line.slice(5).trim() : "";
      if (!data) continue; // priming events carry no data
      const v = JSON.parse(data);
      if (v.id === body.id) return v;
    }
    return undefined;
  }
  async rpc(method, params) {
    const id = ++this.id;
    const v = await this.post({ jsonrpc: "2.0", id, method, params });
    if (v?.error) throw new Error(`${method}: ${JSON.stringify(v.error)}`);
    return v?.result;
  }
  async init() {
    await this.rpc("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "scripted-agent", version: "1" } });
    await this.post({ jsonrpc: "2.0", method: "notifications/initialized" });
  }
  /** A tool call's text blocks. Throws on a tool error. */
  async texts(tool, a = {}) {
    const r = await this.rpc("tools/call", { name: tool, arguments: a });
    const text = (r?.content ?? []).filter((c) => c.type === "text").map((c) => c.text);
    if (r?.isError) throw new Error(`${tool}: ${text.join(" ").slice(0, 300)}`);
    return text;
  }
  /** A tool call; the first text block parsed as JSON when it is JSON. Throws on a tool error. */
  async tool(tool, a = {}) {
    const text = await this.texts(tool, a);
    try {
      return JSON.parse(text[0]);
    } catch {
      return text.join("\n");
    }
  }
}

/** The file with every conflicted region replaced by its first side (regions: 1-based, markers included). */
export function takeFirstSides(d) {
  const lines = d.materialized.split("\n");
  for (const r of [...d.regions].sort((a, b) => b.start - a.start)) {
    const side = (r.sides[0]?.text ?? "").replace(/\n$/, "");
    lines.splice(r.start - 1, r.end - r.start + 1, ...(side === "" ? [] : side.split("\n")));
  }
  return lines.join("\n");
}

/** Numbered code lines of an MCP code block ("  12│ text") → Map(line → text). */
export function codeLines(block) {
  const m = new Map();
  for (const l of block.split("\n")) {
    const x = /^\s*(\d+)│ ?(.*)$/.exec(l);
    if (x) m.set(Number(x[1]), x[2]);
  }
  return m;
}

/** The patches answering a thread on line `start` (whose text is `line`): a comment line inserted
 *  above the anchored lines, indented like them, and, when the thread is not on line 1, a header
 *  line inserted at the top of the file (it shifts the anchor). Inserted lines only, so port notes
 *  that MCP appends to code lines never end up in the file. */
export function answerPatches(start, line, id) {
  const indent = /^\s*/.exec(line)[0];
  const ps = [{ start, end: start - 1, text: `${indent}# checked (scripted agent, ${id})` }];
  if (start > 1) ps.push({ start: 1, end: 0, text: "# reviewed by the scripted agent" });
  return ps;
}

const mcp = new Mcp(`${server}/mcp`);
await mcp.init();
// bind_workspace is in the full profile only; the other profiles take the workspace from the
// X-CV-Workspace header sent with every request.
const bound = await mcp.tool("bind_workspace", { workspace }).catch(() => undefined);
log(`${bound ? "bound to" : "working in"} ${bound?.bound ?? workspace} as ${name}`);

const done = new Set();
/** Thread id → failed attempts: a thread is done only once handled; failures are retried. */
const tries = new Map();
const MAX_TRIES = 8;
const deadline = Date.now() + seconds * 1000;
let cursor;

/** "Ask agent to resolve" (Review.tsx) files a task asking to resolve the conflict in its file. */
const asksToResolve = (t) => /resolve (this|the \d+) conflict/i.test(t.preview ?? t.body ?? t.comments?.[0]?.body ?? "");

async function handle(t) {
  const path = t.path ?? t.anchor?.path;
  const conflicts = await mcp.tool("list_conflicts", {}).catch(() => []);
  const c = Array.isArray(conflicts) ? conflicts.find((x) => x.path === path) : undefined;
  // The thread can be listed before the conflict is (the server syncs the op first): retry rather
  // than treating a conflict request as a code comment.
  if (!c && asksToResolve(t)) throw new Error(`${path} is not listed as conflicted yet`);
  if (c) {
    const d = await mcp.tool("get_conflict", { rev: c.change ?? c.commit, path });
    const content = takeFirstSides(d);
    if (/^(<<<<<<<|>>>>>>>)/m.test(content)) throw new Error(`markers left in ${path}`);
    const r = await mcp.tool("resolve_conflict", { rev: c.change ?? c.commit, path, content });
    log(`resolved ${path} (${d.regions.length} region(s)): ${typeof r === "string" ? r.slice(0, 80) : (r?.status ?? "ok")}`);
    await mcp.tool("reply", { thread: t.id, body: `Resolved the conflict in ${path}: kept the first side of ${d.regions.length} region(s). (scripted agent)`, setStatus: "addressed" });
  } else {
    // A comment on code: read it with its code (this also makes that code the edit's baseBlob), then
    // change the code and answer in one call.
    const [meta, code] = await mcp.texts("get_thread", { thread: t.id, contextLines: 2 });
    const m = JSON.parse(meta);
    const line = codeLines(code).get(m.start);
    if (line === undefined) throw new Error(`line ${m.start} of ${path} not in the code shown`);
    const patches = answerPatches(m.start, line, t.id);
    const r = await mcp.tool("edit_code", {
      thread: t.id,
      patches,
      reply: `Done: commented line ${m.start} and added a file header (${patches.length} patches, one edit). (scripted agent)`,
    });
    log(`answered ${t.id} (${path}:${m.start}) with edit ${r?.id} ${r?.status}: ${patches.length} patches, thread ${r?.thread?.status ?? r?.threadError}`);
  }
}

while (Date.now() < deadline) {
  // Work first (threads assigned before we started count too), then wait for more.
  const threads = await mcp.tool("list_threads", { assignee: "agent", status: ["open", "in-progress"] }).catch((e) => (log(String(e)), []));
  const list = Array.isArray(threads) ? threads : (threads?.threads ?? []);
  let retry = false;
  for (const t of list) {
    if (done.has(t.id)) continue;
    try {
      await handle(t);
      done.add(t.id);
    } catch (e) {
      const n = (tries.get(t.id) ?? 0) + 1;
      tries.set(t.id, n);
      if (n >= MAX_TRIES) done.add(t.id);
      else retry = true;
      log(`failed on ${t.id} (attempt ${n}/${MAX_TRIES}${n >= MAX_TRIES ? ", giving up" : ", will retry"}): ${e.message}`);
    }
    if (once && done.has(t.id)) process.exit(0);
  }
  // A thread to retry: look again soon (an event may never come for it).
  const left = retry ? 2 : Math.max(1, Math.min(30, Math.round((deadline - Date.now()) / 1000)));
  const ev = await mcp.tool("wait_for_events", { cursor, kinds: ["review.submitted", "thread.upserted"], timeoutS: left }).catch((e) => (log(String(e)), undefined));
  if (ev?.cursor) cursor = ev.cursor;
}
log("time is up");
