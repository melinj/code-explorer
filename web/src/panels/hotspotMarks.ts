/**
 * Hotspots scatter marks (DESIGN §4.9: "dots are shaped by lane"): one shape per built-in lane, so
 * lanes read without colour (colour is heat). User lanes get a circle.
 */
export type MarkShape = "circle" | "square" | "diamond" | "triangle" | "cross";

export const LANE_SHAPE: Record<string, MarkShape> = { code: "circle", test: "square", config: "diamond", infra: "triangle", docs: "cross" };

export const markShape = (lane: string): MarkShape => LANE_SHAPE[lane] ?? "circle";

/**
 * SVG path data of a mark centred on (x, y); `stroke` marks (the cross) are drawn with a stroke and
 * no fill, the rest filled. The circle is drawn as an SVG circle (no path).
 */
export function markPath(shape: MarkShape, x: number, y: number): { d: string; stroke: boolean } | undefined {
  switch (shape) {
    case "square":
      return { d: `M${x - 2.5},${y - 2.5}h5v5h-5Z`, stroke: false };
    case "diamond":
      return { d: `M${x},${y - 3.5}L${x + 3.5},${y}L${x},${y + 3.5}L${x - 3.5},${y}Z`, stroke: false };
    case "triangle":
      return { d: `M${x},${y - 3.5}L${x + 3.5},${y + 3}L${x - 3.5},${y + 3}Z`, stroke: false };
    case "cross":
      return { d: `M${x - 3},${y}H${x + 3}M${x},${y - 3}V${y + 3}`, stroke: true };
    case "circle":
      return undefined;
  }
}
