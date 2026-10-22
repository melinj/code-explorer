import { afterEach, describe, expect, it, vi } from "vitest";
import type { EditResult } from "../contracts/model";
import { ReviewClient, editWarnings, suggestionOf } from "./review";

const rec = { change: "c1", commit: "abc", path: "a.py", sides: 2, firstSeenOp: "op1" };
const detail = { ...rec, materialized: "<<<<<<<\n>>>>>>>\n", regions: [] };

function serve(routes: Record<string, () => Response>) {
  vi.stubGlobal("fetch", async (url: string) => (routes[url] ?? (() => new Response("{}", { status: 404 })))());
}
const json = (v: unknown, status = 200) => () => new Response(JSON.stringify(v), { status });

afterEach(() => vi.unstubAllGlobals());

describe("ReviewClient.loadConflicts", () => {
  it("keeps the previous list when GET /conflicts fails", async () => {
    const c = new ReviewClient();
    serve({ "/api/v1/conflicts": json([rec]), "/api/v1/conflicts/abc/a.py": json(detail) });
    await c.loadConflicts();
    expect([...c.conflicts.keys()]).toEqual(["a.py"]);
    serve({ "/api/v1/conflicts": json({ error: { code: "internal", message: "op heads mid-write" } }, 500) });
    await c.loadConflicts();
    expect([...c.conflicts.keys()]).toEqual(["a.py"]);
    // A successful empty list does clear it.
    serve({ "/api/v1/conflicts": json([]) });
    await c.loadConflicts();
    expect(c.conflicts.size).toBe(0);
  });

  it("keeps a conflict whose detail read failed", async () => {
    const c = new ReviewClient();
    serve({ "/api/v1/conflicts": json([rec]), "/api/v1/conflicts/abc/a.py": json(detail) });
    await c.loadConflicts();
    serve({ "/api/v1/conflicts": json([rec]), "/api/v1/conflicts/abc/a.py": json({}, 500) });
    await c.loadConflicts();
    expect(c.conflicts.get("a.py")?.commit).toBe("abc");
  });
});

describe("suggestionOf", () => {
  it("takes the first ```suggestion block, multi-line, without the trailing newline", () => {
    expect(suggestionOf("Try:\n```suggestion\na = 1\nb = 2\n```\nthanks")).toBe("a = 1\nb = 2");
    expect(suggestionOf("```suggestion\nfirst\n```\n```suggestion\nsecond\n```")).toBe("first");
  });
  it("keeps an empty suggestion (delete the lines) apart from none", () => {
    expect(suggestionOf("```suggestion\n```")).toBe("");
    expect(suggestionOf("no block here")).toBeUndefined();
    expect(suggestionOf("```python\nx\n```")).toBeUndefined();
  });
});

describe("editWarnings", () => {
  const base = { warnings: [] } as unknown as EditResult;
  it("folds `divergent` in once", () => {
    const r = { ...base, divergent: ["abc", "def"] } as EditResult;
    const w = editWarnings(r);
    expect(w).toHaveLength(1);
    expect(w[0]).toMatchObject({ code: "divergent", changes: ["abc", "def"] });
    expect(w[0].message).toContain("abc, def");
    // A server that already names it is not repeated.
    const named = { ...r, warnings: [{ code: "divergent", message: "x", changes: ["abc"] }] } as unknown as EditResult;
    expect(editWarnings(named)).toHaveLength(1);
  });
  it("copies, never mutates, the result's list", () => {
    const r = { ...base, divergent: ["abc"] } as EditResult;
    editWarnings(r);
    expect(r.warnings).toEqual([]);
    expect(editWarnings(undefined)).toEqual([]);
  });
});
