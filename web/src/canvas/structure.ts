import type { FileStructure, FolderStructure, Lens, StructureResponse, ViewLayout, WindowStructure } from "../contracts/model";

/**
 * The layout a lens shows a snapshot in (model.ts Lens): the folder tree for "none" and "tension",
 * the folder tree ordered by links ("flow") for "depth", the district grid for the lenses whose
 * subject the grid's axes carry.
 */
export function layoutFor(lens: Lens): ViewLayout {
  if (lens === "depth") return "flow";
  return lens === "none" || lens === "tension" ? "folders" : "districts";
}

/** Lenses showing a folder tree (either order): the ones the structure is loaded for. */
export function isTreeLens(lens: Lens) {
  return layoutFor(lens) !== "districts";
}

/** The directory of a repo path ("" at the root). */
export function dirOf(path: string) {
  const i = path.lastIndexOf("/");
  return i < 0 ? "" : path.slice(0, i);
}

/**
 * GET /views/:id/structure, indexed by path and window key (DESIGN §4.11): the Depth lens (links
 * staying in a directory or below against those leaving) and the Tension lens (busy files among
 * quiet siblings), and the hover cards.
 */
export class Structure {
  readonly folders = new Map<string, FolderStructure>();
  readonly files = new Map<string, FileStructure>();
  readonly windows = new Map<string, WindowStructure>();

  /** The most links touching one directory, and one window, on the view (Depth saturation). */
  private readonly maxFolderLinks: number;
  private readonly maxWindowLinks: number;

  constructor(readonly data: StructureResponse) {
    for (const f of data.folders) this.folders.set(f.path, f);
    for (const f of data.files) this.files.set(f.path, f);
    for (const w of data.windows) this.windows.set(w.key, w);
    this.maxFolderLinks = Math.max(1, ...data.folders.map((f) => f.internal + f.outgoing + f.incoming));
    this.maxWindowLinks = Math.max(1, ...data.windows.map((w) => w.internal + w.external));
  }

  /** A directory's links against the busiest directory on the view, 0..1 on a log scale. */
  folderVolume(folder: string | undefined): number {
    const f = folder === undefined ? undefined : this.folders.get(folder);
    return f ? Math.log1p(f.internal + f.outgoing + f.incoming) / Math.log1p(this.maxFolderLinks) : 0;
  }

  /** A window's links against the busiest window on the view, 0..1 on a log scale. */
  windowVolume(key: string): number {
    const w = this.windows.get(key);
    return w ? Math.log1p(w.internal + w.external) / Math.log1p(this.maxWindowLinks) : 0;
  }

  /** A directory's flow pressure; undefined when no link touches it. */
  pressure(folder: string | undefined): number | undefined {
    const f = folder === undefined ? undefined : this.folders.get(folder);
    return f && f.internal + f.outgoing + f.incoming > 0 ? f.pressure : undefined;
  }

  /** Share of a window's links that leave its directory and what is below it, 0..1; undefined without links. */
  windowOutside(key: string): number | undefined {
    const w = this.windows.get(key);
    const n = w ? w.internal + w.external : 0;
    return w && n > 0 ? w.external / n : undefined;
  }

  tension(folder: string | undefined): number {
    return (folder !== undefined && this.folders.get(folder)?.tension) || 0;
  }
}

/** The structure of the view on screen (set by the engine while a folder-tree lens is on). */
export let structure: Structure | undefined;

export function setStructure(s?: Structure) {
  structure = s;
}
