/**
 * Strength 0..1 of a pair for drawing: the lower bound of the 95% Wilson interval on its jaccard
 * (support out of the commits touching either side), so three commits out of three draw weaker than
 * thirty out of forty; the server ranks pairs by the same bound. Cluster pairs run lower (a cluster
 * changes more often than any one of its files, which dilutes its jaccard), so they are stretched
 * over a lower range.
 */
export function coStrength(p: { jaccard: number; support: number }, level: "file" | "cluster") {
  const n = p.jaccard > 0 ? Math.round(p.support / p.jaccard) : p.support;
  const [lo, hi] = level === "file" ? [0.1, 0.65] : [0.04, 0.4];
  const k = (wilsonLower(p.support, n) - lo) / (hi - lo);
  return k < 0 ? 0 : k > 1 ? 1 : k;
}

/** Lower bound of the 95% Wilson interval of `k` successes in `n` trials. */
export function wilsonLower(k: number, n: number) {
  if (n <= 0) return 0;
  const z2 = 1.96 * 1.96;
  const p = k / n;
  return Math.max(0, (p + z2 / (2 * n) - 1.96 * Math.sqrt((p * (1 - p) + z2 / (4 * n)) / n)) / (1 + z2 / n));
}
