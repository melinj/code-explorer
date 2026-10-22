import type { ClientMessage, ServerEvent, ServerFrame } from "../contracts/ws";
import { session, token } from "./auth";

type Handler = (e: ServerEvent) => void;

/**
 * F2 realtime client for /api/v1/ws: reconnects with backoff and resumes from the last cursor, so
 * events missed while disconnected are replayed by the server's event log.
 */
export class Live {
  private ws?: WebSocket;
  private cursor?: string;
  private handlers = new Set<Handler>();
  private retry = 500;
  private closed = false;
  status: "connecting" | "live" | "offline" = "connecting";
  /** The view this tab shows: subscribed on every (re)connect (view.subscribe). */
  private viewId?: string;
  private pinger?: ReturnType<typeof setInterval>;
  onStatus?: (s: Live["status"]) => void;
  /**
   * The server no longer has the events since our cursor (event-log retention): what we show may
   * be stale in ways no event will tell. Listeners also get a synthetic op.changed.
   */
  onResync?: () => void;

  constructor(private path = "/api/v1/ws") {}

  on(h: Handler): () => void {
    this.handlers.add(h);
    return () => void this.handlers.delete(h);
  }

  start() {
    this.closed = false;
    const url = new URL(this.path, location.href);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    if (this.cursor) url.searchParams.set("cursor", this.cursor);
    // Browsers can't set headers on a WebSocket: the token goes in the query.
    const t = token();
    if (t) url.searchParams.set("token", t);
    url.searchParams.set("session", session());
    const ws = new WebSocket(url);
    this.ws = ws;
    ws.onopen = () => {
      this.retry = 500;
      this.setStatus("live");
      if (this.viewId) this.send({ type: "view.subscribe", viewId: this.viewId as never });
      clearInterval(this.pinger);
      this.pinger = setInterval(() => this.send({ type: "ping" }), 30_000);
    };
    ws.onmessage = (m) => {
      const frame = JSON.parse(m.data) as ServerFrame | { type: "pong" };
      // Answers to ping, and anything else that is not an event frame.
      if (!("event" in frame) || !frame.event || frame.event.type === "pong") return;
      // hello carries the server's cursor; only adopt it on the first connect, or when the server
      // cannot replay from ours (resync: refetch everything, then continue from its cursor).
      if (frame.event.type === "hello") {
        if (frame.event.resync && this.cursor) {
          this.cursor = frame.event.cursor;
          this.onResync?.();
          const ev: ServerEvent = { type: "op.changed", op: frame.event.op, changes: [], actor: { kind: "external" } };
          for (const h of this.handlers) h(ev);
          return;
        }
        this.cursor ??= frame.event.cursor;
        return;
      }
      this.cursor = frame.seq;
      for (const h of this.handlers) h(frame.event);
    };
    ws.onclose = () => {
      if (this.closed) return;
      this.setStatus("offline");
      setTimeout(() => this.start(), this.retry);
      this.retry = Math.min(this.retry * 2, 8000);
    };
  }

  /** Follow events of `viewId` (and stop following the previous view). Re-sent on reconnect. */
  subscribe(viewId: string) {
    if (viewId === this.viewId) return;
    if (this.viewId) this.send({ type: "view.unsubscribe", viewId: this.viewId as never });
    this.viewId = viewId;
    this.send({ type: "view.subscribe", viewId: viewId as never });
  }

  /** Send a client message (view.state, ping) if connected; dropped otherwise. */
  send(msg: ClientMessage) {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify(msg));
  }

  stop() {
    this.closed = true;
    clearInterval(this.pinger);
    this.ws?.close();
  }

  private setStatus(s: Live["status"]) {
    this.status = s;
    this.onStatus?.(s);
  }
}
