import { describe, expect, it } from "vitest";
import { invalidationAction } from "./invalidation";

const snap = { id: "v1", isRange: false };
const range = { id: "v1", isRange: true };

describe("invalidationAction (view.invalidated → what the tab refetches)", () => {
  it("reloads the whole view in place on an index invalidation of this view with no list", () => {
    expect(invalidationAction({ viewId: "v1", reason: "index" }, snap)).toEqual({ kind: "reload-layout" });
    // Another view's whole-view invalidation is not ours.
    expect(invalidationAction({ viewId: "v2", reason: "index" }, snap)).toEqual({ kind: "none" });
  });

  it("treats an empty window list as nothing, not everything", () => {
    expect(invalidationAction({ viewId: "v1", reason: "index", windows: [] }, snap)).toEqual({ kind: "none" });
    expect(invalidationAction({ viewId: "v1", reason: "op", windows: [] }, snap)).toEqual({ kind: "none" });
  });

  it("refetches listed windows (of any view: window ids are shared) and this view's tiles", () => {
    expect(invalidationAction({ viewId: "v2", reason: "index", windows: ["w1"] }, snap)).toEqual({ kind: "windows", windows: ["w1"] });
    expect(invalidationAction({ viewId: "v1", reason: "op", tiles: ["0/1/2"] }, snap)).toEqual({ kind: "tiles", tiles: ["0/1/2"] });
    expect(invalidationAction({ viewId: "v2", reason: "op", tiles: ["0/1/2"] }, snap)).toEqual({ kind: "none" });
    // Tiles alone are not "everything".
    expect(invalidationAction({ viewId: "v1", reason: "index", tiles: ["0/0/0"] }, snap)).toEqual({ kind: "tiles", tiles: ["0/0/0"] });
  });

  it("re-POSTs a stale range view, but not a snapshot", () => {
    expect(invalidationAction({ viewId: "v1", reason: "op" }, range)).toEqual({ kind: "reload-layout" });
    expect(invalidationAction({ viewId: "v1", reason: "op" }, snap)).toEqual({ kind: "none" });
    expect(invalidationAction({ viewId: "v1", reason: "op", windows: ["w"] }, range)).toEqual({ kind: "windows", windows: ["w"] });
  });

  it("reloads the bootstrap on a layout change without windows, and ignores expand", () => {
    expect(invalidationAction({ viewId: "v1", reason: "layout", windows: [] }, snap)).toEqual({ kind: "bootstrap" });
    expect(invalidationAction({ viewId: "v1", reason: "layout", windows: ["w"] }, snap)).toEqual({ kind: "windows", windows: ["w"] });
    expect(invalidationAction({ viewId: "v1", reason: "expand", windows: ["w"] }, snap)).toEqual({ kind: "none" });
  });
});
