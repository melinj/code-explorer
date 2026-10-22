import { describe, expect, it } from "vitest";
import type { LinkKind } from "../contracts/model";
import type { Edge } from "../data/client";
import { CONFIG_SOLID_SCORE, LOW_CONFIDENCE, isUncertain, quadEdges } from "./edges";
import type { LabelBatch } from "./labels";
import type { RectBatch } from "./rects";
import { DARK } from "./theme";

let next = 1;
const edge = (kind: LinkKind, confidence: number): Edge => ({ id: next++, kind, from: "a", to: "b", fromCluster: "c", toCluster: "c", linkId: "", ax: 0, ay: next * 100, bx: 2000, by: next * 100 + 300, confidence });

/** A RectBatch that records what quadEdges draws: polyline and quad alphas, and "?" labels. */
function recorder() {
  const lines: number[] = [];
  const quads: number[] = [];
  const rb = {
    polyline: (_p: Float32Array, _w: number, _c: number, a = 1) => void lines.push(a),
    quad: (...q: number[]) => void quads.push(q[9] ?? 1),
    round() {},
    add() {},
  } as unknown as RectBatch;
  const marks: string[] = [];
  const labels = { add: (t: string) => void marks.push(t) } as unknown as LabelBatch;
  return { rb, labels, lines, quads, marks };
}

describe("link confidence (DESIGN §4.6: config links 0.65–0.80 faded with ?)", () => {
  it("config-field links are uncertain below 0.80, code links below 0.6", () => {
    expect(CONFIG_SOLID_SCORE).toBe(0.8);
    for (const k of ["read", "config", "define"] as LinkKind[]) {
      expect(isUncertain({ kind: k, confidence: 0.65 })).toBe(true);
      expect(isUncertain({ kind: k, confidence: 0.79 })).toBe(true);
      expect(isUncertain({ kind: k, confidence: 0.8 })).toBe(false);
      expect(isUncertain({ kind: k, confidence: 1 })).toBe(false);
    }
    expect(isUncertain({ kind: "call", confidence: 0.7 })).toBe(false);
    expect(isUncertain({ kind: "call", confidence: LOW_CONFIDENCE - 0.01 })).toBe(true);
    expect(isUncertain({ kind: "subscribe", confidence: 0.7 })).toBe(false);
  });

  it("draws a 0.7 config read faded with a ?, a 0.9 one solid, and a 0.7 call solid", () => {
    const draw = (e: Edge) => {
      const r = recorder();
      const stats = { lowMarks: 0, maxWidthPx: 0 };
      quadEdges(r.rb, [e], { theme: DARK, wuPerPx: 1, detail: true, widthPx: 1.4, alpha: 0.8, hidden: new Set(), labels: r.labels, stats });
      return { ...r, stats };
    };
    const faded = draw(edge("read", 0.7));
    expect(faded.stats.lowMarks).toBe(1);
    expect(faded.marks).toEqual(["?"]);
    // Dashed (several pieces), each at the faded alpha.
    expect(faded.lines.length).toBeGreaterThan(1);
    for (const a of faded.lines) expect(a).toBeCloseTo(0.8 * 0.45);
    const solid = draw(edge("read", 0.9));
    expect(solid.stats.lowMarks).toBe(0);
    expect(solid.marks).toEqual([]);
    expect(solid.lines[0]).toBeCloseTo(0.8);
    // A solid link tapers from call site to definition (quads, not a polyline), at full alpha.
    const call = draw(edge("call", 0.7));
    expect(call.stats.lowMarks).toBe(0);
    expect(call.lines).toEqual([]);
    expect(call.quads.length).toBeGreaterThan(1);
    expect(new Set(call.quads)).toEqual(new Set([0.8]));
  });
});
