// c8: the realtime client resumes from its cursor, and resyncs when the server says its cursor is
// older than the event log keeps (hello.resync): it refetches (onResync), tells listeners with a
// synthetic op.changed, adopts the server's cursor and reconnects from it next time.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ServerEvent } from "../contracts/ws";
import { Live } from "./live";

class FakeSocket {
  static OPEN = 1;
  static all: FakeSocket[] = [];
  readyState = 0;
  sent: string[] = [];
  onopen?: () => void;
  onmessage?: (m: { data: string }) => void;
  onclose?: () => void;
  constructor(public url: string) {
    FakeSocket.all.push(this);
  }
  send(s: string) {
    this.sent.push(s);
  }
  close() {
    this.readyState = 3;
    this.onclose?.();
  }
  open() {
    this.readyState = 1;
    this.onopen?.();
  }
  frame(seq: string, event: unknown) {
    this.onmessage?.({ data: JSON.stringify({ seq, event }) });
  }
}

const hello = (cursor: string, resync?: boolean) => ({
  type: "hello",
  session: "s",
  op: "op1",
  serverVersion: "t",
  cursor,
  ...(resync ? { resync } : {}),
});

beforeEach(() => {
  FakeSocket.all = [];
  vi.useFakeTimers();
  vi.stubGlobal("WebSocket", FakeSocket);
  vi.stubGlobal("location", { href: "http://127.0.0.1:7100/" });
  vi.stubGlobal("history", { replaceState() {} });
  const store = new Map<string, string>();
  vi.stubGlobal("sessionStorage", { getItem: (k: string) => store.get(k) ?? null, setItem: (k: string, v: string) => store.set(k, v) });
});
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("Live", () => {
  it("resumes from its cursor and resyncs when the server cannot replay", () => {
    const live = new Live();
    const seen: ServerEvent[] = [];
    let resyncs = 0;
    live.on((e) => seen.push(e));
    live.onResync = () => resyncs++;
    live.start();
    const a = FakeSocket.all[0];
    expect(new URL(a.url).searchParams.get("cursor")).toBeNull();
    a.open();
    a.frame("0", hello("10"));
    a.frame("11", { type: "op.changed", op: "op2", changes: [], actor: { kind: "external" } });
    expect(seen.map((e) => e.type)).toEqual(["op.changed"]);

    // Disconnected: reconnects with the last cursor; a normal hello replays, no resync.
    a.close();
    vi.advanceTimersByTime(600);
    const b = FakeSocket.all[1];
    expect(new URL(b.url).searchParams.get("cursor")).toBe("11");
    b.open();
    b.frame("0", hello("12"));
    expect(resyncs).toBe(0);

    // Disconnected again; this time the log was pruned past our cursor: resync.
    b.close();
    vi.advanceTimersByTime(1100);
    const c = FakeSocket.all[2];
    expect(new URL(c.url).searchParams.get("cursor")).toBe("11");
    c.open();
    c.frame("0", hello("500", true));
    expect(resyncs).toBe(1);
    expect(seen.map((e) => e.type)).toEqual(["op.changed", "op.changed"]);
    expect(seen[1]).toMatchObject({ type: "op.changed", op: "op1", changes: [] });

    // The next reconnect starts from the server's cursor, not the expired one.
    c.close();
    vi.advanceTimersByTime(600);
    expect(new URL(FakeSocket.all[3].url).searchParams.get("cursor")).toBe("500");
    live.stop();
  });

  it("a first connect adopts the server's cursor without resyncing", () => {
    const live = new Live();
    let resyncs = 0;
    live.onResync = () => resyncs++;
    live.start();
    const a = FakeSocket.all[0];
    a.open();
    a.frame("0", hello("42", true));
    expect(resyncs).toBe(0);
    a.close();
    vi.advanceTimersByTime(600);
    expect(new URL(FakeSocket.all[1].url).searchParams.get("cursor")).toBe("42");
    live.stop();
  });
});
