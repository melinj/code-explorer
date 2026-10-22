import { describe, expect, it } from "vitest";
import type { Rect } from "../contracts/model";
import { intersects } from "../data/loader";
import { GLYPH_PX, clusterLabelRect, glyphSpot } from "./glyphSpot";

const px = 5; // zoom 0.2
const G = GLYPH_PX * px;
const win = { x: 1000, y: 1000, w: 700, h: 900 };
const box = (s: { x: number; y: number }): Rect => ({ x: s.x, y: s.y, w: G, h: G });
const blockedBy = (things: Rect[]) => (s: Rect) => things.some((t) => intersects(t, s));

describe("glyphSpot (L2 district glyph placement)", () => {
  it("takes the corner off the window's top-left, outside the window", () => {
    const s = glyphSpot(win, G, px, () => false);
    expect(s.where).toBe("corner");
    expect(intersects(box(s), win)).toBe(false);
    expect(s.x + G).toBeLessThanOrEqual(win.x);
    expect(s.y + G).toBeLessThanOrEqual(win.y);
  });

  it("moves off a cluster label above the window, and never covers it", () => {
    // The first row of a cluster: its label runs above the window's left part.
    const label = clusterLabelRect({ rect: { x: win.x - 200, y: win.y - 130, w: 3000, h: 4000 } }, 420);
    const s = glyphSpot(win, G, px, blockedBy([label]));
    expect(s.where).toBe("above-right");
    expect(intersects(box(s), label)).toBe(false);
    expect(intersects(box(s), win)).toBe(false);
  });

  it("stays clear of a window stacked right above with a narrow gap", () => {
    const above = { x: win.x, y: win.y - 40 - 600, w: 700, h: 600 };
    const left = { x: win.x - 400, y: win.y - 700, w: 380, h: 650 };
    const s = glyphSpot(win, G, px, blockedBy([above, left]));
    expect(s.where).toBe("corner-right");
    expect(intersects(box(s), above)).toBe(false);
    expect(intersects(box(s), left)).toBe(false);
  });

  it("falls back to inside the window's corner when every outside spot is taken", () => {
    const s = glyphSpot(win, G, px, () => true);
    expect(s.where).toBe("inside");
    const b = box(s);
    expect(b.x >= win.x && b.y >= win.y && b.x + G <= win.x + win.w && b.y + G <= win.y + win.h).toBe(true);
  });
});
