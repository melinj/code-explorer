#!/usr/bin/env node
// Contract conformance of recorded request/response pairs (WORKSTREAMS C0 contract replay).
//
//   node scripts/replay/check.mjs [serve|mock|all]
//
// For every recording in fixtures/replay/<target>/*.json this generates a TypeScript module that
// assigns the recorded JSON (as an object literal, so excess properties are errors) to the type
// docs/contracts/http.ts gives that route: `Routes[route]["res"]` for 2xx answers, `Routes[route]["body"]`
// for the request, and `{ error: ApiError }` for error answers. Ids are branded in the spec but plain
// strings on the wire, so the check unbrands types first (Wire<T>). A non-2xx status is a failure too.
//
// Every failure must be listed in fixtures/replay/known-divergences.txt to pass:
//   <target>/<recording slug>  <substring of the message>  # reason
// The slug is the file name without its "NN-" prefix and ".json". Unused entries are reported as stale.

import { createRequire } from "node:module";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const require = createRequire(path.join(ROOT, "web/package.json"));
const ts = require("typescript");

const which = process.argv[2] || "all";
const GROUPS = { serve: ["serve", "serve-conflict", "serve-workspaces", "serve-resync"], mock: ["mock"] };
const targets = which === "all" ? [...GROUPS.serve, ...GROUPS.mock] : GROUPS[which] || [which];
const REPLAY = path.join(ROOT, "fixtures/replay");
// Generated modules go to a temp dir outside the repo (REPLAY_KEEP_TMP=1 keeps it for debugging).
const TMP = fs.mkdtempSync(path.join(os.tmpdir(), "cv-replay-"));
process.on("exit", () => {
  if (process.env.REPLAY_KEEP_TMP) console.log(`contract replay: kept ${TMP}`);
  else fs.rmSync(TMP, { recursive: true, force: true });
});

const rel = path.relative(TMP, path.join(ROOT, "docs/contracts")).split(path.sep).join("/");
fs.writeFileSync(
  path.join(TMP, "wire.ts"),
  `import type { Routes } from "${rel}/http";
import type { ApiError } from "${rel}/ids";
import type { ServerFrame } from "${rel}/ws";
/** The wire form of a spec type: branded ids become plain strings, everything else is kept. */
export type Wire<T> = T extends string
  ? [Exclude<keyof T, keyof string>] extends [never] ? T : string
  : T extends number
    ? [Exclude<keyof T, keyof number>] extends [never] ? T : number
    : T extends readonly unknown[]
      ? { [K in keyof T]: Wire<T[K]> }
      : T extends object
        ? { [K in keyof T]: Wire<T[K]> }
        : T;
export type Res<K extends keyof Routes> = Wire<Routes[K] extends { res: infer R } ? R : never>;
export type Body<K extends keyof Routes> = Wire<Routes[K] extends { body: infer B } ? B : never>;
export type Err = { error: Wire<ApiError>; code?: string; message?: string };
export type RouteKey = keyof Routes;
/** WS /api/v1/ws recordings: the first frame a client gets (the hello). */
export type Frame = Wire<ServerFrame>;
`,
);

const failures = []; // {key, message}; duplicates (same key and message) are dropped below
const files = [];
const fileToKey = new Map();
let recordings = 0;
for (const t of targets) {
  const dir = path.join(REPLAY, t);
  if (!fs.existsSync(dir)) {
    failures.push({ key: `${t}/*`, message: `no recordings in fixtures/replay/${t}` });
    continue;
  }
  for (const f of fs.readdirSync(dir).filter((f) => f.endsWith(".json")).sort()) {
    recordings++;
    const rec = JSON.parse(fs.readFileSync(path.join(dir, f), "utf8"));
    const slug = f.replace(/^\d+-/, "").replace(/\.json$/, "");
    const key = `${t}/${slug}`;
    const ws = rec.route.startsWith("WS "); // a WebSocket recording: its first frame
    const ok = ws ? rec.status === 101 : rec.status >= 200 && rec.status < 300;
    if (rec.expectStatus !== undefined && rec.expectStatus !== rec.status) {
      failures.push({ key, message: `status ${rec.status}, contract says ${rec.expectStatus} for ${rec.route} ${rec.path}` });
    } else if (!ok && rec.expectStatus !== rec.status) {
      const msg = rec.response && typeof rec.response === "object" ? JSON.stringify(rec.response).slice(0, 160) : "";
      failures.push({ key, message: `status ${rec.status} for ${rec.route} ${rec.path} ${msg}` });
    }
    const walk = (v, p) => {
      if (v === "<non-iso-time>") failures.push({ key, message: `${p}: time is not ISO-8601 (ids.ts wire convention)` });
      else if (Array.isArray(v)) v.forEach((x) => walk(x, p + "[]"));
      else if (v && typeof v === "object") for (const [k, x] of Object.entries(v)) walk(x, p ? `${p}.${k}` : k);
    };
    walk(rec.response, "res");
    const r = JSON.stringify(rec.route);
    let src = ws
      ? `import type { Frame } from "./wire";\n`
      : `import type { Res, Body, Err, RouteKey } from "./wire";\nconst route = ${r} satisfies RouteKey;\nvoid route;\n`;
    if (rec.body !== undefined) src += `export const body: Body<${r}> = ${JSON.stringify(rec.body)};\n`;
    if (rec.status === 0) {
      // transport failure: nothing to type-check
    } else if (ws) src += `export const res: Frame = ${JSON.stringify(rec.response)};\n`;
    else if (ok) src += `export const res: Res<${r}> = ${JSON.stringify(rec.response)};\n`;
    else src += `export const err: Err = ${JSON.stringify(rec.response)};\n`;
    const out = path.join(TMP, `${t}__${f.replace(/\.json$/, "")}.ts`);
    fs.writeFileSync(out, src);
    files.push(out);
    fileToKey.set(path.resolve(out), key);
  }
}

const program = ts.createProgram(files, {
  strict: true,
  noEmit: true,
  target: ts.ScriptTarget.ES2022,
  module: ts.ModuleKind.ESNext,
  moduleResolution: ts.ModuleResolutionKind.Bundler,
  skipLibCheck: true,
  types: [],
});
for (const d of ts.getPreEmitDiagnostics(program)) {
  const file = d.file ? path.resolve(d.file.fileName) : "";
  const key = fileToKey.get(file) || (d.file ? path.relative(ROOT, d.file.fileName) : "global");
  let where = "";
  if (d.file && d.start !== undefined) {
    // Name the JSON path: the chain of property names enclosing the error position.
    where = jsonPath(d.file, d.start);
  }
  const msg = ts.flattenDiagnosticMessageText(d.messageText, " / ").replace(/\s+/g, " ");
  failures.push({ key, message: `${where ? where + ": " : ""}${msg}`.slice(0, 600) });
}

function jsonPath(sf, pos) {
  const names = [];
  const visit = (node) => {
    if (pos < node.getStart(sf) || pos >= node.getEnd()) return;
    if (ts.isPropertyAssignment(node)) names.push(node.name.getText(sf).replace(/^"|"$/g, ""));
    if (ts.isVariableDeclaration(node)) names.push(node.name.getText(sf));
    ts.forEachChild(node, visit);
  };
  visit(sf);
  return names.join(".");
}

{
  const seen = new Set();
  for (let i = failures.length - 1; i >= 0; i--) {
    const k = failures[i].key + "\u0000" + failures[i].message;
    if (seen.has(k)) failures.splice(i, 1);
    seen.add(k);
  }
}

// ── allow-list ──
const allowFile = path.join(REPLAY, "known-divergences.txt");
const allow = fs.existsSync(allowFile)
  ? fs
      .readFileSync(allowFile, "utf8")
      .split("\n")
      .map((l, i) => ({ line: i + 1, text: l }))
      .filter((l) => l.text.trim() && !l.text.trim().startsWith("#"))
      .map((l) => {
        const [spec, reason = ""] = l.text.split(/\s+#\s+/);
        const m = spec.trim().match(/^(\S+)\s+(.*)$/);
        return { line: l.line, key: m ? m[1] : spec.trim(), needle: m ? m[2].trim() : "", reason, used: false };
      })
      .filter((a) => targets.some((t) => a.key.startsWith(t + "/")))
  : [];

const unexpected = [];
for (const f of failures) {
  const a = allow.find((a) => (a.key === f.key || (a.key.endsWith("/*") && f.key.startsWith(a.key.slice(0, -1)))) && f.message.includes(a.needle));
  if (a) a.used = true;
  else unexpected.push(f);
}
const allowed = failures.length - unexpected.length;
for (const a of allow.filter((a) => !a.used)) {
  console.log(`stale allow-list entry (line ${a.line}): ${a.key}  ${a.needle}`);
}
for (const f of unexpected) console.log(`FAIL ${f.key}: ${f.message}`);
console.log(
  `contract replay [${targets.join(", ")}]: ${recordings} recordings, ${failures.length} divergences (${allowed} known), ${unexpected.length} unexpected`,
);
if (process.env.REPLAY_LIST_ALL) for (const f of failures) console.log(`  ${f.key}  ${f.message}`);
process.exit(unexpected.length ? 1 : 0);
