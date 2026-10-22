import type { Rect } from "../contracts/model";
import type { WorkerRequest, WorkerResponse } from "./fetch.worker";
import { session, token } from "./auth";

export interface Job {
  key: string;
  url: string;
  method?: string;
  body?: unknown;
  kind: "json" | "batch" | "tile";
  /** World region this request serves; drives priority and cancellation. */
  region: Rect;
  /** Lower runs first among jobs at equal distance (e.g. tiles before outlines). */
  rank: number;
  /** Streamed pieces before completion (tile window slices). */
  onPart?: (data: unknown) => void;
  resolve: (data: unknown) => void;
  reject: (err: { status: number; message: string; aborted: boolean }) => void;
}

const MAX_IN_FLIGHT = 6;

/**
 * Priority request scheduler (DESIGN §5 "Loading"): at most 6 requests in flight, ordered by
 * distance to the predicted viewport; queued or running requests whose region leaves the
 * predicted viewport plus margin are cancelled. The render loop never waits on it.
 */
export class Loader {
  private worker = new Worker(new URL("./fetch.worker.ts", import.meta.url), { type: "module" });
  private queue: Job[] = [];
  private running = new Map<number, Job>();
  private keys = new Set<string>();
  private nextId = 1;
  private focus: Rect = { x: 0, y: 0, w: 1, h: 1 };
  stats = { requests: 0, cancelled: 0, bytesHint: 0 };

  constructor() {
    const t = token();
    this.worker.postMessage({ type: "auth", token: t, session: session() } satisfies WorkerRequest);
    this.worker.onmessage = (e: MessageEvent<WorkerResponse>) => {
      const m = e.data;
      const job = this.running.get(m.id);
      if (!job) return;
      if (m.type === "part") {
        job.onPart?.(m.data);
        return;
      }
      this.running.delete(m.id);
      this.keys.delete(job.key);
      if (m.type === "done") job.resolve(m.data);
      else job.reject(m);
      this.pump();
    };
  }

  /** Tell the worker what the main thread holds (see WorkerRequest "have" / "forget" / "reset"). */
  tell(m: Extract<WorkerRequest, { type: "have" | "forget" | "reset" }>) {
    this.worker.postMessage(m);
  }

  get pending() {
    return this.queue.length + this.running.size;
  }

  has(key: string) {
    return this.keys.has(key);
  }

  request(job: Omit<Job, "resolve" | "reject">): Promise<unknown> {
    return new Promise((resolve, reject) => {
      this.keys.add(job.key);
      this.queue.push({ ...job, resolve, reject });
      this.pump();
    });
  }

  /** Cancel everything queued and in flight (view switch). */
  cancelAll() {
    for (const j of this.queue) {
      this.keys.delete(j.key);
      j.reject({ status: 0, message: "cancelled", aborted: true });
    }
    this.queue = [];
    for (const id of this.running.keys()) this.worker.postMessage({ type: "cancel", id } satisfies WorkerRequest);
  }

  /** Set the predicted viewport (`focus`) and the region outside which work is cancelled (`keep`). */
  setFocus(focus: Rect, keep: Rect) {
    this.focus = focus;
    const drop = (j: Job) => !intersects(j.region, keep);
    for (const j of this.queue.filter(drop)) {
      this.keys.delete(j.key);
      this.stats.cancelled++;
      j.reject({ status: 0, message: "cancelled", aborted: true });
    }
    this.queue = this.queue.filter((j) => !drop(j));
    for (const [id, j] of this.running) {
      if (drop(j)) this.worker.postMessage({ type: "cancel", id } satisfies WorkerRequest);
    }
    this.pump();
  }

  private priority(j: Job) {
    const fx = this.focus.x + this.focus.w / 2;
    const fy = this.focus.y + this.focus.h / 2;
    const dx = Math.max(0, Math.abs(j.region.x + j.region.w / 2 - fx) - (j.region.w + this.focus.w) / 2);
    const dy = Math.max(0, Math.abs(j.region.y + j.region.h / 2 - fy) - (j.region.h + this.focus.h) / 2);
    return Math.hypot(dx, dy) / Math.max(this.focus.w, 1) + j.rank * 0.25;
  }

  private pump() {
    if (this.running.size >= MAX_IN_FLIGHT || this.queue.length === 0) return;
    this.queue.sort((a, b) => this.priority(a) - this.priority(b));
    while (this.running.size < MAX_IN_FLIGHT && this.queue.length) {
      const j = this.queue.shift()!;
      const id = this.nextId++;
      this.running.set(id, j);
      this.stats.requests++;
      this.worker.postMessage({ type: "fetch", id, url: j.url, method: j.method, body: j.body, kind: j.kind } satisfies WorkerRequest);
    }
  }
}

export function intersects(a: Rect, b: Rect) {
  return a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
}

export function grow(r: Rect, mx: number, my = mx): Rect {
  return { x: r.x - mx, y: r.y - my, w: r.w + 2 * mx, h: r.h + 2 * my };
}
