/**
 * Compact encodings for the two payloads that are fetched in bulk while panning.
 * Producer: B3 (parse) computes them per blob; B7 serves them. Consumer: F2 decodes in a worker.
 */

/** Dominant token class of a span or line. Shared by silhouettes, highlighting and themes. */
export enum TokenClass {
  Plain = 0,
  Keyword = 1,
  String = 2,
  Comment = 3,
  Number = 4,
  Function = 5,
  Type = 6,
  Variable = 7,
  Operator = 8,
  Punctuation = 9,
  Decorator = 10,
  Constant = 11,
  Property = 12,
  Tag = 13,
  Invalid = 14,
  Blank = 15,
}

/**
 * OutlineEncoding: 3 bytes per displayed line, concatenated, base64 in JSON.
 *   byte 0  indent in columns (clamped 0–255)
 *   byte 1  visible length after the indent in columns (clamped 0–255)
 *   byte 2  bits 0–3 dominant TokenClass · bits 4–5 diff (0 none, 1 add, 2 del)
 *           · bit 6 line has a port · bit 7 line has a thread
 * A 500-line window costs 1.5 KB. It is the placeholder drawn until text arrives.
 */
export const OUTLINE_RECORD_BYTES = 3;

export interface OutlineRecord {
  indent: number;
  length: number;
  cls: TokenClass;
  diff: 0 | 1 | 2;
  hasPort: boolean;
  hasThread: boolean;
}

/**
 * TokenEncoding: per line, a flat array of triples [startCol, length, TokenClass, …].
 * Columns are UTF-16 code units. Gaps between triples are Plain.
 */
export type LineTokens = number[];

/**
 * Tile addressing for T1 viewport queries. A tile at level z covers TILE_BASE_WU · 2^z world units
 * square; x and y are floor(world / size). The client picks z so one tile spans 512–1024 screen px,
 * requests the tiles intersecting viewport + one-screen margin, and de-duplicates windows by id.
 */
export const TILE_BASE_WU = 4096;

/**
 * Every file split into several windows gets a chip colour: FileFragment.chip is a slot in
 * 0..FILE_CHIP_SLOTS-1, stable per path (hash), with the client shifting a slot when two split files
 * in view collide. Slot i uses OKLCH hue i·36°. At rest the chip is muted (L 0.78, C 0.06) so it reads
 * as identity, not heat; when its file is selected, every fragment's chip and outline go more
 * saturated and brighter (L 0.88, C 0.17) and the fragments are marked on the minimap.
 */
export const FILE_CHIP_SLOTS = 10;

/** Per-window cap on displayed lines unless the user expands on purpose. */
export const WINDOW_MAX_LINES = 500;
export const LINE_WU = 18;
export const COL_WU = 9;
export const HEADER_WU = 36;
