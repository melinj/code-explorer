/** What a `view.invalidated` event asks of this tab (contracts/ws.ts). */
export type InvalidationAction =
  | { kind: "none" }
  /** Re-open the view in place (re-POST its spec), keeping the camera and selection. */
  | { kind: "reload-layout" }
  /** Refetch these windows' headers (and their text where loaded). */
  | { kind: "windows"; windows: string[] }
  /** Refetch the loaded windows in these tiles ("z/x/y"). */
  | { kind: "tiles"; tiles: string[] }
  /** Re-read the bootstrap (cluster labels and summaries). */
  | { kind: "bootstrap" };

export interface InvalidationEvent {
  viewId: string;
  reason: "op" | "layout" | "index" | "expand";
  windows?: string[];
  tiles?: string[];
}

/**
 * The client's reading of `view.invalidated`:
 * - "expand": another client widened a window in place; nothing to refetch;
 * - "op" on this tab's range view without a window list: its revset resolves to other commits
 *   (stale), so its spec is re-POSTed;
 * - a window list (any view: windows are shared ids) or, for this view, a tile list: refetch those;
 * - "layout" without windows for this view: labels changed, reload the bootstrap;
 * - "index" for this view with no windows and no tiles at all means everything (e.g. hubs
 *   re-settled after an op): reload the view in place. An empty window list means nothing.
 */
export function invalidationAction(ev: InvalidationEvent, view: { id: string; isRange: boolean }): InvalidationAction {
  const mine = ev.viewId === view.id;
  if (ev.reason === "expand") return { kind: "none" };
  if (ev.reason === "op" && mine && view.isRange && ev.windows === undefined) return { kind: "reload-layout" };
  if (ev.windows?.length) return { kind: "windows", windows: ev.windows };
  if (ev.tiles?.length && mine) return { kind: "tiles", tiles: ev.tiles };
  if (ev.reason === "layout" && mine) return { kind: "bootstrap" };
  if (ev.reason === "index" && mine && ev.windows === undefined) return { kind: "reload-layout" };
  return { kind: "none" };
}
