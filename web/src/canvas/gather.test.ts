import { describe, expect, it } from "vitest";
import type { Rect } from "../contracts/model";
import { intersects } from "../data/loader";
import { layoutTray, startRect } from "./gatherLayout";
import { coStrength, wilsonLower } from "./coStrength";

const view: Rect = { x: 0, y: 0, w: 1300, h: 900 };
const slot = { w: 210, h: 66 };
const inside = (r: Rect, v: Rect) => r.x >= v.x && r.y >= v.y && r.x + r.w <= v.x + v.w && r.y + r.h <= v.y + v.h;

describe("layoutTray (co-change gather)", () => {
  it("goes right of the focus when it fits, strongest card nearest", () => {
    const focus = { x: 200, y: 400, w: 120, h: 80 };
    const l = layoutTray(focus, 6, slot, view);
    expect(l.side).toBe("right");
    expect(inside(l.tray, view)).toBe(true);
    expect(intersects(l.tray, focus)).toBe(false);
    // Two columns of three: the first three ranks in the column next to the focus.
    expect(l.slots[0].x).toBeLessThan(l.slots[3].x);
    expect(l.slots[0].x).toBe(l.slots[2].x);
  });

  it("falls back to the left near the right edge, nearest column still next to the focus", () => {
    const focus = { x: 1100, y: 400, w: 120, h: 80 };
    const l = layoutTray(focus, 6, slot, view);
    expect(l.side).toBe("left");
    expect(inside(l.tray, view)).toBe(true);
    expect(l.slots[0].x).toBeGreaterThan(l.slots[3].x);
  });

  it("goes below a focus too wide for either side", () => {
    const focus = { x: 100, y: 100, w: 1100, h: 200 };
    const l = layoutTray(focus, 5, slot, view);
    expect(l.side).toBe("below");
    expect(l.tray.y).toBeGreaterThan(focus.y + focus.h);
    expect(inside(l.tray, view)).toBe(true);
  });

  it("narrows the tray to keep it beside a focus in a crowded view, never over it", () => {
    // Three columns would not fit right of this focus; two do.
    const focus = { x: 578, y: 381, w: 144, h: 150 };
    const v = { x: 144, y: 136, w: 1144, h: 742 };
    const l = layoutTray(focus, 12, { w: 210, h: 91 }, v);
    expect(l.side).toBe("right");
    expect(intersects(l.tray, focus)).toBe(false);
    expect(inside(l.tray, v)).toBe(true);
  });

  it("covers as little of the focus as it can when nothing fits", () => {
    const focus = { x: 100, y: 100, w: 1100, h: 700 };
    const l = layoutTray(focus, 12, slot, view);
    expect(inside(l.tray, view)).toBe(true);
  });

  it("never lays a slot over another", () => {
    const l = layoutTray({ x: 300, y: 300, w: 50, h: 50 }, 12, slot, view);
    for (let i = 0; i < l.slots.length; i++) for (let j = i + 1; j < l.slots.length; j++) expect(intersects(l.slots[i], l.slots[j])).toBe(false);
  });

  it("starts an off-screen partner at the view's edge, card-sized", () => {
    const s = startRect({ x: 5000, y: 300, w: 400, h: 300 }, { x: 600, y: 300, ...slot }, view);
    expect(s.w).toBe(slot.w);
    expect(s.x + s.w / 2).toBe(view.x + view.w);
    const on = { x: 100, y: 100, w: 300, h: 200 };
    expect(startRect(on, { x: 600, y: 300, ...slot }, view)).toBe(on);
  });
});

describe("coStrength", () => {
  it("ranks a long history above a tiny perfect one", () => {
    expect(wilsonLower(3, 3)).toBeLessThan(wilsonLower(30, 40));
    expect(coStrength({ jaccard: 1, support: 3 }, "file")).toBeLessThan(coStrength({ jaccard: 0.75, support: 30 }, "file"));
    expect(coStrength({ jaccard: 0.1, support: 3 }, "cluster")).toBe(0);
  });
});
