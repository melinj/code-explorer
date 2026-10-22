import type { ClientMessage, ServerEvent, ServerFrame } from "../contracts/ws";
import { token } from "./auth";

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
  onStatus?: (s: Live["status"]) => void;

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
    const ws = new WebSocket(url);
    this.ws = ws;
    ws.onopen = () => {
      this.retry = 500;
      this.setStatus("live");
    };
    ws.onmessage = (m) => {
      const frame = JSON.parse(m.data) as ServerFrame;
      // hello carries the server's cursor; only adopt it on the first connect.
      if (frame.event.type === "hello") {
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

  /** Send a client message (view.state, ping) if connected; dropped otherwise. */
  send(msg: ClientMessage) {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify(msg));
  }

  stop() {
    this.closed = true;
    this.ws?.close();
  }

  private setStatus(s: Live["status"]) {
    this.status = s;
    this.onStatus?.(s);
  }
}
