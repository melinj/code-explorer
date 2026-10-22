import { describe, expect, it } from "vitest";
import { grow, intersects } from "../data/loader";
import { curve, distSq } from "./edges";
import { DARK, LIGHT, hex, mix, oklch } from "./theme";

describe("rect helpers", () => {
  it("intersects is strict on touching edges", () => {
    const a = { x: 0, y: 0, w: 10, h: 10 };
    expect(intersects(a, { x: 5, y: 5, w: 10, h: 10 })).toBe(true);
    expect(intersects(a, { x: 10, y: 0, w: 5, h: 5 })).toBe(false);
    expect(intersects(a, { x: -5, y: 11, w: 30, h: 1 })).toBe(false);
  });
  it("grow widens on both sides", () => {
    expect(grow({ x: 10, y: 20, w: 30, h: 40 }, 5)).toEqual({ x: 5, y: 15, w: 40, h: 50 });
    expect(grow({ x: 10, y: 20, w: 30, h: 40 }, 5, 1)).toEqual({ x: 5, y: 19, w: 40, h: 42 });
  });
});

describe("link curves", () => {
  it("start at the callsite and end at the definition", () => {
    for (const [ax, ay, bx, by] of [[0, 0, 1000, 300], [1000, 300, 0, 0], [0, 0, 0, 5000]]) {
      const p = curve(ax, ay, bx, by);
      expect([p[0], p[1]]).toEqual([ax, ay]);
      expect(p[p.length - 2]).toBeCloseTo(bx, 3);
      expect(p[p.length - 1]).toBeCloseTo(by, 3);
    }
  });
  it("get more points as they get longer, within the cap", () => {
    const short = curve(0, 0, 200, 0).length / 2 - 1;
    const long = curve(0, 0, 20000, 4000).length / 2 - 1;
    expect(short).toBeGreaterThanOrEqual(12);
    expect(long).toBeGreaterThan(short);
    expect(long).toBeLessThanOrEqual(72);
  });
  it("distSq is zero on the curve and grows off it", () => {
    const p = curve(0, 0, 1000, 0);
    expect(distSq(p, 500, 0)).toBeCloseTo(0, 3);
    expect(distSq(p, 500, 30)).toBeCloseTo(900, 0);
  });
});

describe("theme colours", () => {
  it("mix interpolates per channel", () => {
    expect(mix(0x000000, 0xffffff, 0)).toBe(0x000000);
    expect(mix(0x000000, 0xffffff, 1)).toBe(0xffffff);
    expect(mix(0x102030, 0x304050, 0.5)).toBe(0x203040);
    expect(hex(0xab)).toBe("#0000ab");
  });
  it("oklch maps white and black, and clamps out-of-gamut colours", () => {
    expect(oklch(1, 0, 0)).toBe(0xffffff);
    expect(oklch(0, 0, 0)).toBe(0x000000);
    const c = oklch(0.7, 0.4, 140);
    expect(c).toBeGreaterThanOrEqual(0);
    expect(c).toBeLessThanOrEqual(0xffffff);
  });
  it("light and dark themes keep every key and differ in background and ink", () => {
    expect(Object.keys(LIGHT).sort()).toEqual(Object.keys(DARK).sort());
    expect(LIGHT.bg).not.toBe(DARK.bg);
    expect(LIGHT.ink).not.toBe(DARK.ink);
    expect(LIGHT.heat).toHaveLength(5);
  });
});
