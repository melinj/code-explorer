import { describe, expect, it } from "vitest";
import type { GraphRow } from "../contracts/model";
import { ROW, edgePaths, gutterWidth, pickRange, rowY, selectionView } from "./graphLayout";

function row(commit: string, parents: string[], lane: number, toLanes: number[], extra: Partial<GraphRow["edges"][0]>[] = []): GraphRow {
  return {
    lane,
    edges: parents.map((p, k) => ({ parent: p, toLane: toLanes[k], ...(extra[k] ?? {}) })),
    rev: {
      change: `ch-${commit}-xxxxxxxx`,
      commit,
      parents,
      description: commit,
      author: { name: "a", email: "a@x", time: "2026-01-01T00:00:00Z" },
      committerTime: "2026-01-01T00:00:00Z",
      bookmarks: [],
      tags: [],
      workspaces: [],
      flags: { conflict: false, empty: false, immutable: false, divergent: false, hidden: false, workingCopy: false },
      shortest: { change: 4, commit: 4 },
    },
  } as unknown as GraphRow;
}

// m (merge of a and b), b, a, c — the lanes readside/lanes.rs assigns.
const rows = [row("m", ["a", "b"], 0, [0, 1]), row("b", ["c"], 1, [1]), row("a", ["c"], 0, [1]), row("c", [], 1, [])];

describe("edgePaths", () => {
  it("draws one path per edge, ending on the parent's node", () => {
    const p = edgePaths(rows);
    expect(p).toHaveLength(4);
    const ma = p.find((e) => e.child === "m" && e.parent === "a")!;
    // m (row 0) → a (row 2) runs down lane 0 and ends at a's node.
    expect(ma.d.startsWith(`M12,${rowY(0)}`)).toBe(true);
    expect(ma.d.endsWith(`12,${rowY(2)}`)).toBe(true);
    const ac = p.find((e) => e.child === "a")!;
    // a is in lane 0, c in lane 1: the adjacent edge bends across.
    expect(ac.d.endsWith(`28,${rowY(3)}`)).toBe(true);
    expect(p.every((e) => !e.dashed)).toBe(true);
  });
  it("runs edges to parents outside the rows to the bottom, dashed", () => {
    const p = edgePaths([row("x", ["gone"], 0, [0], [{ elided: 3 }])]);
    expect(p[0].dashed).toBe(true);
    expect(p[0].d.endsWith(`12,${ROW}`)).toBe(true);
  });
  it("sizes the gutter for the widest lane", () => {
    expect(gutterWidth(rows)).toBe(12 * 2 + 16);
  });
});

describe("selectionView", () => {
  const id = (c: string) => `ch-${c}-xxxxxxxx`.slice(0, 12);
  const chain = [row("c3", ["c2"], 0, [0]), row("c2", ["c1"], 0, [0]), row("c1", ["c0"], 0, [0]), row("x", ["c0"], 1, [0])];
  it("opens one change by its id", () => {
    expect(selectionView(chain, ["c2"])).toEqual({ revset: id("c2"), diff: "cumulative", allowNonChain: false });
  });
  it("opens a chain cumulatively, oldest::newest", () => {
    expect(selectionView(chain, ["c1", "c3", "c2"])!.revset).toBe(`${id("c1")}::${id("c3")}`);
  });
  it("opens anything else per commit", () => {
    const v = selectionView(chain, ["c3", "x"])!;
    expect(v).toEqual({ revset: `${id("c3")} | ${id("x")}`, diff: "per-commit", allowNonChain: true });
    expect(selectionView(chain, [])).toBeUndefined();
  });
  it("picks the rows between two clicks", () => {
    expect(pickRange(chain, "c3", "c1")).toEqual(["c3", "c2", "c1"]);
    expect(pickRange(chain, "c1", "c3")).toEqual(["c3", "c2", "c1"]);
  });
});
