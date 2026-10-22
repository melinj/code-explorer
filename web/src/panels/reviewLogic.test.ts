import { describe, expect, it } from "vitest";
import type { ConflictDetail, Thread } from "../contracts/model";
import { isOpenThread, markerBlocks, outdatedNote, resolveRegions, sendable } from "./reviewLogic";
import { conflictUrl } from "../data/review";

const mat = ["a", "<<<<<<< Conflict 1 of 2", "+++++++ side x", "x1", "%%%%%%% side y", "y1", ">>>>>>> Conflict 1 of 2 ends", "b", "<<<<<<< Conflict 2 of 2", "+++++++ side x", "x2", "%%%%%%% side y", "y2", ">>>>>>> Conflict 2 of 2 ends", "c"].join("\n");
const regions: ConflictDetail["regions"] = [
  { start: 2, end: 7, sides: [{ label: "x", text: "x1" }, { label: "y", text: "y1" }] },
  { start: 9, end: 14, sides: [{ label: "x", text: "x2" }, { label: "y", text: "y2" }] },
];

describe("resolveRegions", () => {
  it("finds every marker block", () => {
    expect(markerBlocks(mat)).toEqual([
      [1, 6],
      [8, 13],
    ]);
  });
  it("replaces each region with its own choice", () => {
    expect(resolveRegions(mat, regions, ["x1", "y2"])).toBe("a\nx1\nb\ny2\nc");
    expect(resolveRegions(mat, regions, ["merged\nlines", ""])).toBe("a\nmerged\nlines\nb\n\nc");
  });
  it("keeps the markers of a region without a choice", () => {
    expect(resolveRegions(mat, regions, [undefined, "x2"])).toBe(mat.split("\n").slice(0, 8).join("\n") + "\nx2\nc");
  });
  it("falls back to the regions' own lines when the marker count disagrees", () => {
    // One region reported, two blocks in the text (e.g. a server that merged them): start/end win.
    expect(resolveRegions(mat, [regions[1]], ["z"])).toBe(mat.split("\n").slice(0, 8).join("\n") + "\nz\nc");
  });
});

describe("thread predicates", () => {
  const t = (status: Thread["status"], assignee?: Thread["assignee"]) => ({ status, assignee });
  it("the open filter and Send N open agree (outdated is open)", () => {
    const all = [t("open"), t("outdated"), t("in-progress", "agent"), t("open", "agent"), t("addressed"), t("resolved"), t("wontfix")];
    const open = all.filter(isOpenThread);
    expect(open).toHaveLength(4);
    // Every sendable thread is an open one; the rest of the open ones are already with the agent.
    expect(all.filter(sendable).every(isOpenThread)).toBe(true);
    expect(all.filter(sendable)).toHaveLength(2);
    expect(open.filter((x) => !sendable(x)).every((x) => x.assignee === "agent")).toBe(true);
  });
  it("outdated notes name the reason and the last good commit", () => {
    expect(outdatedNote({ status: "open" })).toBeUndefined();
    expect(outdatedNote({ status: "outdated", outdated: { reason: "lines-changed", lastGood: "0123456789abcdef" as never } })).toBe("Outdated: the commented lines changed; last good at 01234567");
    expect(outdatedNote({ status: "outdated", outdated: { reason: "file-removed", lastGood: "ff" as never } })).toContain("the file was removed");
  });
});

describe("conflictUrl", () => {
  it("encodes each path segment, keeps the slashes", () => {
    expect(conflictUrl("abc", "src/a b/c#1?.py")).toBe("/conflicts/abc/src/a%20b/c%231%3F.py");
    expect(conflictUrl("abc", "a.py")).toBe("/conflicts/abc/a.py");
  });
});
