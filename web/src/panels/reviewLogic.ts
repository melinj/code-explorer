import type { ConflictDetail, Thread } from "../contracts/model";

/**
 * "Open" in the review panel: the thread still wants someone's attention. Outdated threads count
 * (their lines changed under them and nobody has looked again), as do threads an agent picked up.
 * The "open" filter and the "Send N open to agent" button both use this, so their counts agree.
 */
export const isOpenThread = (t: Pick<Thread, "status">) => t.status === "open" || t.status === "in-progress" || t.status === "outdated";

/** Open threads not yet with the agent: what "Send N open to agent" sends. */
export const sendable = (t: Pick<Thread, "status" | "assignee">) => isOpenThread(t) && t.assignee !== "agent";

/** Outdated, said plainly: why, and the last commit the anchor was good at. */
export function outdatedNote(t: Pick<Thread, "status" | "outdated">): string | undefined {
  if (t.status !== "outdated") return undefined;
  const why = t.outdated?.reason === "file-removed" ? "the file was removed" : t.outdated?.reason === "change-abandoned" ? "the change was abandoned" : "the commented lines changed";
  return t.outdated?.lastGood ? `Outdated: ${why}; last good at ${t.outdated.lastGood.slice(0, 8)}` : `Outdated: ${why}`;
}

/**
 * The marker blocks of a materialized conflict (0-based line indexes of `<<<<<<<` and `>>>>>>>`), in
 * file order. Nested-looking markers inside a block are not expected (jj lengthens its markers when
 * the content has marker-like lines, and those still start with 7 of the character).
 */
export function markerBlocks(materialized: string): Array<[number, number]> {
  const lines = materialized.split("\n");
  const out: Array<[number, number]> = [];
  for (let i = 0; i < lines.length; i++) {
    if (!lines[i].startsWith("<<<<<<<")) continue;
    const j = lines.findIndex((l, k) => k > i && l.startsWith(">>>>>>>"));
    if (j < 0) break;
    out.push([i, j]);
    i = j;
  }
  return out;
}

/**
 * The whole resolved file: each conflict block replaced by its chosen text (`choices[k]` for region
 * k). Blocks are found by their markers and paired with regions in order; when the counts disagree,
 * the regions' own 1-based `start`/`end` (marker lines included) are used. A region without a
 * choice keeps its markers (the caller resolves only when every region has one).
 */
export function resolveRegions(materialized: string, regions: ConflictDetail["regions"], choices: Array<string | undefined>): string {
  const lines = materialized.split("\n");
  let blocks = markerBlocks(materialized);
  if (blocks.length !== regions.length) blocks = regions.map((r) => [r.start - 1, r.end - 1]);
  // Bottom-up, so earlier indexes stay valid.
  const order = blocks.map((b, k) => [b, k] as const).sort((a, b) => b[0][0] - a[0][0]);
  for (const [[a, b], k] of order) {
    const c = choices[k];
    if (c === undefined || a < 0 || b < a || b >= lines.length) continue;
    lines.splice(a, b - a + 1, ...c.split("\n"));
  }
  return lines.join("\n");
}

