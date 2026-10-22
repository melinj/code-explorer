/// <reference lib="webworker" />
// Fetches and decodes off the main thread. Tiles come back in slices of windows (each slice is its
// own main-thread task, so deserializing a big tile never blocks a frame) and edges as one
// transferable Float64Array. Requests can be cancelled by id.

import type { TileResponse, WindowBatchResponse } from "../contracts/http";
import type { LinkKind } from "../contracts/model";

export const LINK_KINDS: LinkKind[] = [
  "call",
  "inherit",
  "implement",
  "import",
  "reference",
  "publish",
  "subscribe",
  "dispatch",
  "http-request",
  "http-route",
  "config",
  "define",
  "read",
  "deploy",
  "ipc",
];

/** Fields per edge in the packed array: id, kind, ax, ay, bx, by, confidence. */
export const EDGE_STRIDE = 7;
const WINDOW_SLICE = 250;

export type WorkerRequest =
  | { type: "fetch"; id: number; url: string; method?: string; body?: unknown; kind: "json" | "batch" | "tile" }
  | { type: "cancel"; id: number }
  | { type: "auth"; token?: string; session?: string }
  /** The main thread holds these (skip them in later tiles), dropped these, or switched views. */
  | { type: "have"; windows: string[]; edges: number[] }
  | { type: "forget"; windows: string[]; edges: number[] }
  | { type: "reset" };

export type DecodedOutline = Omit<WindowBatchResponse["outlines"][number], "silhouette"> & { silhouette: Uint8Array };
export interface PackedTile {
  layoutVersion: number;
  edges: Float64Array;
  /** Per edge: window and cluster ids of both ends, and the link id (strings clone cheaply in flat arrays). */
  ends: string[];
}

export type WorkerResponse =
  | { type: "part"; id: number; data: unknown }
  | { type: "done"; id: number; data: unknown }
  | { type: "error"; id: number; status: number; message: string; aborted: boolean };

const inflight = new Map<number, AbortController>();
// What the main thread confirmed it holds: tiles that overlap loaded ones (zooming out, coming back
// after an eviction) send only what is new, instead of cloning every header again.
const haveWindows = new Set<string>();
const haveEdges = new Set<number>();
let authToken: string | undefined;
let sessionId: string | undefined;
// Link ids are interned to dense integers so the main thread de-duplicates with numbers.
const linkIds = new Map<string, number>();
const intern = (s: string) => {
  let n = linkIds.get(s);
  if (n === undefined) linkIds.set(s, (n = linkIds.size));
  return n;
};

function decodeBase64(s: string): Uint8Array {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

const post = (m: WorkerResponse, transfer: Transferable[] = []) => (self as unknown as Worker).postMessage(m, transfer);

self.onmessage = async (e: MessageEvent<WorkerRequest>) => {
  const msg = e.data;
  if (msg.type === "cancel") {
    inflight.get(msg.id)?.abort();
    return;
  }
  if (msg.type === "have" || msg.type === "forget") {
    const add = msg.type === "have";
    for (const id of msg.windows) add ? haveWindows.add(id) : haveWindows.delete(id);
    for (const id of msg.edges) add ? haveEdges.add(id) : haveEdges.delete(id);
    return;
  }
  if (msg.type === "reset") {
    haveWindows.clear();
    haveEdges.clear();
    return;
  }
  if (msg.type === "auth") {
    authToken = msg.token;
    sessionId = msg.session;
    return;
  }
  const ctl = new AbortController();
  inflight.set(msg.id, ctl);
  try {
    const res = await fetch(msg.url, {
      method: msg.method ?? "GET",
      body: msg.body === undefined ? undefined : JSON.stringify(msg.body),
      headers: {
        ...(msg.body === undefined ? {} : { "content-type": "application/json" }),
        ...(authToken ? { Authorization: `Bearer ${authToken}` } : {}),
        ...(sessionId ? { "X-CV-Session": sessionId } : {}),
      },
      signal: ctl.signal,
    });
    if (!res.ok) throw Object.assign(new Error(await res.text()), { status: res.status });
    const data = await res.json();
    if (msg.kind === "tile") {
      const t = data as TileResponse;
      const ws = t.windows.filter((w) => !haveWindows.has(w.id));
      for (let i = 0; i < ws.length; i += WINDOW_SLICE) post({ type: "part", id: msg.id, data: ws.slice(i, i + WINDOW_SLICE) });
      const fresh = t.edges.filter((ed) => !haveEdges.has(intern(ed.id)));
      const edges = new Float64Array(fresh.length * EDGE_STRIDE);
      fresh.forEach((ed, i) => {
        const o = i * EDGE_STRIDE;
        edges[o] = intern(ed.id);
        edges[o + 1] = Math.max(0, LINK_KINDS.indexOf(ed.kind));
        edges[o + 2] = ed.a[0];
        edges[o + 3] = ed.a[1];
        edges[o + 4] = ed.b[0];
        edges[o + 5] = ed.b[1];
        edges[o + 6] = ed.confidence;
      });
      const ends: string[] = [];
      for (const ed of fresh) ends.push(ed.from, ed.to, ed.fromCluster, ed.toCluster, ed.id);
      post({ type: "done", id: msg.id, data: { layoutVersion: t.layoutVersion, edges, ends } satisfies PackedTile }, [edges.buffer]);
      return;
    }
    const transfer: Transferable[] = [];
    if (msg.kind === "batch") {
      for (const o of (data as WindowBatchResponse).outlines) {
        const bytes = decodeBase64(o.silhouette);
        (o as unknown as DecodedOutline).silhouette = bytes;
        transfer.push(bytes.buffer);
      }
    }
    post({ type: "done", id: msg.id, data }, transfer);
  } catch (err) {
    post({
      type: "error",
      id: msg.id,
      status: (err as { status?: number }).status ?? 0,
      message: String((err as Error).message),
      aborted: (err as Error).name === "AbortError",
    });
  } finally {
    inflight.delete(msg.id);
  }
};
