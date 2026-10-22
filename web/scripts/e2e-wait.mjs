// Condition waits shared by the e2e flows (panels-flow, review-flow, views-flow, soak): no fixed
// sleeps. Every predicate handed to page.waitForFunction is synchronous — Playwright does not await
// a predicate's Promise (a Promise is truthy) — so anything that needs fetch() polls from node.
//
// Bounds are generous on purpose (CI boxes run at load 100+): a bound only stops a hung run; the
// check after the wait is what passes or fails.

/** Slow machines: E2E_SLOW=3 triples every bound below. */
export const SLOW = Number(process.env.E2E_SLOW ?? 1) || 1;
export const ms = (n) => Math.round(n * SLOW);
export const sleep = (n) => new Promise((r) => setTimeout(r, n));

/**
 * Settled: the camera is at rest and its last position's data update ran, no view switch is in
 * flight, nothing is requested or queued, no visible chunk has a dirty part, the bundles, spokes,
 * code text queue and fades are done — for `q` polls in a row. Runs in the page.
 */
function settledInPage(q) {
  const e = window.__cv?.engine;
  if (!e) return false;
  const c = e.cam;
  const s = e.stats();
  const ok =
    !c.moving &&
    !e["wheel"] &&
    performance.now() - c.lastMotion > 200 &&
    e["camVersion"] === c.version &&
    e["lastData"] >= c.lastMotion &&
    !s.pendingSwitch &&
    s.pendingRequests === 0 &&
    s.pendingBuilds === 0 &&
    !e["bundles"]?.building &&
    !e["spokes"]?.building &&
    (e.tier !== "L3" || (e["codeQueue"]?.size ?? 0) === 0) && // drained at L3 only
    (e["fading"]?.length ?? 0) === 0;
  window.__cvSettleQ = ok ? (window.__cvSettleQ ?? 0) + 1 : 0;
  return window.__cvSettleQ >= q;
}

export function waits(page, log = console.log) {
  /** Data and drawing caught up with the camera (see settledInPage). Logs, never throws, on timeout. */
  const settle = async (p = page, { timeout = 60000, q = 3 } = {}) => {
    await p.evaluate(() => (window.__cvSettleQ = 0)).catch(() => {});
    const t0 = Date.now();
    await p.waitForFunction(settledInPage, q, { timeout: ms(timeout), polling: 100 }).catch(async () => {
      const st = await p.evaluate(() => {
        const e = window.__cv?.engine;
        if (!e) return "no engine";
        const s = e.stats();
        return { moving: e.cam.moving, req: s.pendingRequests, builds: s.pendingBuilds, sw: s.pendingSwitch, bundles: e["bundles"]?.building, spokes: e["spokes"]?.building, code: e["codeQueue"]?.size, fading: e["fading"]?.length };
      }).catch((err) => String(err));
      log(`     settle: not settled after ${Math.round((Date.now() - t0) / 1000)} s: ${JSON.stringify(st)}`);
    });
  };
  /** The camera's version (take it before an action that flies; then `flown(v0)`). */
  const camV = (p = page) => p.evaluate(() => window.__cv.engine.cam.version);
  /** A flight started after `v0` (goTo locates before it flies) and ended, then settled. */
  const flown = async (v0, { p = page, timeout = 30000, selected } = {}) => {
    const ok = await p
      .waitForFunction(([v0, sel]) => { const e = window.__cv.engine; return e.cam.version !== v0 && !e.cam.moving && (!sel || e.selected?.type === sel); }, [v0, selected], { timeout: ms(timeout), polling: 50 })
      .then(() => true, () => false);
    if (!ok) log(`     flown: no finished flight${selected ? ` selecting a ${selected}` : ""} after ${timeout} ms`);
    await settle(p);
    return ok;
  };
  /** `n` animation frames: a synchronous handler / React commit has run and painted. */
  const frames = (p = page, n = 2) => p.evaluate((n) => new Promise((r) => { const f = (k) => (k ? requestAnimationFrame(() => f(k - 1)) : r()); f(n); }), n);
  /** Poll `fn` (node side; may use fetch / page.evaluate) until it returns something truthy. Returns it, or undefined at the bound. */
  const pollNode = async (fn, { timeout = 30000, every = 200 } = {}) => {
    const until = Date.now() + ms(timeout);
    for (;;) {
      const v = await fn().catch(() => undefined);
      if (v) return v;
      if (Date.now() > until) return undefined;
      await sleep(every);
    }
  };
  /** A synchronous page predicate holds (null at the bound). */
  const until = (fn, arg, timeout = 30000) => page.waitForFunction(fn, arg, { timeout: ms(timeout), polling: 100 }).then((h) => h.jsonValue()).catch(() => null);
  return { settle, camV, flown, frames, pollNode, until };
}
