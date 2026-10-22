import { describe, expect, it } from "vitest";
import { LANE_SHAPE, markPath, markShape } from "./hotspotMarks";

describe("hotspots marks (shaped by lane)", () => {
  it("gives every built-in lane its own shape, docs a cross", () => {
    const lanes = ["infra", "config", "code", "test", "docs"];
    expect(new Set(lanes.map(markShape)).size).toBe(lanes.length);
    expect(markShape("docs")).toBe("cross");
    expect(markShape("prompts")).toBe("circle");
  });

  it("draws each shape (the docs cross as a stroked plus, not a fallback circle)", () => {
    for (const shape of Object.values(LANE_SHAPE)) {
      const m = markPath(shape, 10, 20);
      if (shape === "circle") expect(m).toBeUndefined();
      else expect(m?.d).toMatch(/^M/);
    }
    const cross = markPath("cross", 10, 20)!;
    expect(cross.stroke).toBe(true);
    expect(cross.d).toBe("M7,20H13M10,17V23");
  });
});
