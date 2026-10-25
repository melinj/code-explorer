import { describe, expect, it } from "vitest";
import type { StructureResponse } from "../contracts/model";
import { Structure, dirOf, isTreeLens, layoutFor } from "./structure";

// Ids are branded in model.ts; plain strings stand in for them here.
const data = {
  summary: { folders: 2, files: 3, links: 3, crossFolderLinks: 2, linkLocality: 0.333, heatTension: 0.2, pressure: 0.5 },
  folders: [
    { path: "app", depth: 1, files: 2, windows: 2, internal: 1, outgoing: 2, incoming: 0, pressure: 0.667, inside: 0.333, interface: 0, interfaceShare: 0, partners: 1, gateways: 1, spread: 0, chain: 2, mass: 0, tension: 0, hotFiles: [] },
    { path: "quiet", depth: 1, files: 1, windows: 1, internal: 0, outgoing: 0, incoming: 0, pressure: 0, inside: 0, interface: 0, interfaceShare: 0, partners: 0, gateways: 0, spread: 0, chain: 1, mass: 0.4, tension: 0.5, hotFiles: [] },
  ],
  files: [
    { path: "app/a.py", folder: "app", lane: "code", heat: 0, rate: 0, internal: 1, external: 2, exports: 0, tension: 0, pull: { folder: "core", links: 2 } },
    { path: "app/b.py", folder: "app", lane: "code", heat: 0, rate: 0, internal: 1, external: 0, exports: 0, tension: 0 },
  ],
  windows: [{ key: "app/a.py#f", path: "app/a.py", internal: 1, external: 2, incoming: 0, tension: 0 }],
} as unknown as StructureResponse;

describe("structure", () => {
  it("lenses pick the layout", () => {
    expect(layoutFor("none")).toBe("folders");
    expect(isTreeLens("depth") && isTreeLens("none") && isTreeLens("tension")).toBe(true);
    expect(isTreeLens("heat")).toBe(false);
    expect(layoutFor("depth")).toBe("flow");
    expect(layoutFor("tension")).toBe("folders");
    expect(layoutFor("heat")).toBe("districts");
    expect(layoutFor("cochange")).toBe("districts");
  });

  it("reads files and folders by path", () => {
    const s = new Structure(data);
    expect(s.pressure("app")).toBe(0.667);
    expect(s.pressure("quiet")).toBeUndefined(); // no link touches it
    expect(s.pressure(undefined)).toBeUndefined();
    expect(s.tension("quiet")).toBe(0.5);
    expect(s.tension("app")).toBe(0);
    expect(s.windows.get("app/a.py#f")?.external).toBe(2);
    expect(s.windowOutside("app/a.py#f")).toBeCloseTo(2 / 3);
    expect(s.windowOutside("nowhere#x")).toBeUndefined();
    expect(dirOf("app/a.py")).toBe("app");
    expect(dirOf("top.py")).toBe("");
  });
});

describe("depth colour", () => {
  it("hue follows the outside share, saturation the link volume (never below 20%)", async () => {
    const { DARK, depthColor } = await import("./theme");
    const rgb = (c: number) => [(c >> 16) & 255, (c >> 8) & 255, c & 255];
    const spread = (c: number) => Math.max(...rgb(c)) - Math.min(...rgb(c));
    const [cr, , cb] = rgb(depthColor(DARK, 0, 1));
    const [hr, , hb] = rgb(depthColor(DARK, 1, 1));
    expect(cb).toBeGreaterThan(cr); // sealed: blue
    expect(hr).toBeGreaterThan(hb); // leaking: red
    expect(spread(depthColor(DARK, 1, 1))).toBeGreaterThan(spread(depthColor(DARK, 1, 0)));
    expect(spread(depthColor(DARK, 1, 0))).toBeGreaterThan(8); // the 20% floor keeps some colour
  });

  it("measures link volume against the busiest on the view", () => {
    const s = new Structure(data);
    expect(s.folderVolume("app")).toBe(1); // the busiest directory
    expect(s.folderVolume("quiet")).toBe(0);
    expect(s.windowVolume("app/a.py#f")).toBe(1);
    expect(s.windowVolume("nowhere#x")).toBe(0);
  });
});
