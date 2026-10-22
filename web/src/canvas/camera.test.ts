import { describe, expect, it } from "vitest";
import { Camera } from "./camera";

function cam(zoom: number) {
  const c = new Camera();
  c.resize(1600, 1000);
  c.animateTo(0, 0, zoom, 0);
  return c;
}

describe("Camera", () => {
  it("picks tiers by zoom (DESIGN §5)", () => {
    expect(cam(0.01).tier).toBe("L0");
    expect(cam(0.05).tier).toBe("L1");
    expect(cam(0.2).tier).toBe("L2");
    expect(cam(1).tier).toBe("L3");
  });

  it("needs a 10% overshoot to change tier, in both directions", () => {
    const c = cam(0.05);
    c.animateTo(0, 0, 0.0951, 0);
    expect(c.tier).toBe("L1"); // above the 0.09 edge, but not by 10%
    c.animateTo(0, 0, 0.1, 0);
    expect(c.tier).toBe("L2");
    c.animateTo(0, 0, 0.085, 0);
    expect(c.tier).toBe("L2"); // below the edge, but not by 10%
    c.animateTo(0, 0, 0.08, 0);
    expect(c.tier).toBe("L1");
  });

  it("keeps the world point under the cursor fixed while zooming", () => {
    const c = cam(0.1);
    const before = c.toWorld(400, 300);
    c.zoomAt(400, 300, 3.7);
    const after = c.toWorld(400, 300);
    expect(after[0]).toBeCloseTo(before[0], 6);
    expect(after[1]).toBeCloseTo(before[1], 6);
  });

  it("bumps its version on every change, so the engine knows when to render", () => {
    const c = cam(0.1);
    const v = c.version;
    c.panBy(10, 0);
    expect(c.version).toBeGreaterThan(v);
  });
});
