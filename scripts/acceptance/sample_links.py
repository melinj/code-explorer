"""B4 / P5 acceptance samples: draw links for hand-labelling, and score labelled samples.

  python3 scripts/acceptance/sample_links.py code CLONE OUT.tsv [--n 50 --seed 7]
          [--conf-min 0 --conf-max 1] [--pool POOL.json]
      50 code links (reference resolution: imports, calls, refs) from cv serve, uniformly from
      the out-links of every code file (GET /links?target=<path>&dir=out), lexical candidates
      (confidence ≤ 0.5, DESIGN §4.5) included; --conf-min/--conf-max restrict the confidence band
      (e.g. --conf-max 0.5 for candidates only). --pool caches the fetched pool between draws.
  python3 scripts/acceptance/sample_links.py config CLONE OUT.tsv --dump CHANNEL_LINKS.tsv
      50 config-field links (P5) from the `cv index` channel dump (config_fields.py writes it with
      the default context scoring; pass a new --seed for a fresh sample).
  python3 scripts/acceptance/sample_links.py score LABELLED.tsv [--min 0.9]
      precision = correct / labelled (labels: correct|incorrect or plausible|implausible).

The TSV has the link, both ends with their source line, then `label` and `reason`, which a person
fills in after reading the code at both ends.
"""
import argparse
import json
import os
import random
import sys
import urllib.parse

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import Server, get  # noqa: E402

CHANNEL_KINDS = {"http", "subscribe", "publish", "env", "deploy", "read", "config"}
COLS = ["link", "kind", "confidence", "source", "source_text", "target", "target_text", "label", "reason"]


def line_of(repo, path, line):
    try:
        with open(os.path.join(repo, path), encoding="utf-8", errors="replace") as f:
            for i, l in enumerate(f, 1):
                if i == line:
                    return l.strip().replace("\t", " ")[:160]
    except OSError:
        pass
    return ""


def write(rows, out):
    if os.path.exists(out) and not os.environ.get("FORCE"):
        sys.exit(f"{out} exists (it may hold hand labels); set FORCE=1 to overwrite")
    with open(out, "w") as f:
        f.write("\t".join(COLS) + "\n")
        for r in rows:
            f.write("\t".join(str(r.get(c, "")) for c in COLS) + "\n")
    print(f"wrote {len(rows)} links to {out}")


def code_sample(repo, out, n, seed, lo=0.0, hi=1.0, pool_cache=None):
    if pool_cache and os.path.exists(pool_cache):
        uniq = json.load(open(pool_cache))
        print(f"{len(uniq)} code links from {pool_cache}")
    else:
        uniq = fetch_pool(repo)
        if pool_cache:
            json.dump(uniq, open(pool_cache, "w"))
    band = [l for l in uniq if lo <= l["confidence"] <= hi]
    print(f"{len(band)} links with confidence in [{lo}, {hi}]")
    rows = []
    for l in random.Random(seed).sample(band, min(n, len(band))):
        f, t = l["from"], l["to"]
        rows.append({"link": f"{l['label']}", "kind": l["kind"], "confidence": l["confidence"],
                     "source": f"{f['path']}:{f['line']}", "source_text": line_of(repo, f["path"], f["line"]),
                     "target": f"{t['path']}:{t['line']}", "target_text": line_of(repo, t["path"], t["line"])})
    write(rows, out)


def fetch_pool(repo):
    pool = []
    with Server(repo) as srv:
        vid = srv.view()
        pts = get(f"/views/{vid}/metrics?level=file&limit=100000")["points"]
        files = sorted(p["label"] for p in pts if p["lane"] in ("code", "test"))
        for path in files:
            q = urllib.parse.urlencode({"viewId": vid, "target": path, "dir": "out"})
            try:
                res = get(f"/links?{q}")
            except Exception:
                continue
            for l in res.get("links", []):
                if l["kind"] in CHANNEL_KINDS or l["from"]["path"] != path:
                    continue
                to = l["to"]
                if "path" not in to:
                    continue
                pool.append(l)
    seen, uniq = set(), []
    for l in pool:
        if l["id"] not in seen:
            seen.add(l["id"])
            uniq.append(l)
    uniq.sort(key=lambda l: l["id"])
    kinds = {}
    for l in uniq:
        kinds[l["kind"]] = kinds.get(l["kind"], 0) + 1
    cand = sum(1 for l in uniq if l["confidence"] <= 0.5)
    print(f"{len(uniq)} code links in the pool ({len(files)} code/test files); by kind: {kinds}; "
          f"{cand} lexical candidates (≤ 0.5)")
    return uniq


def config_sample(repo, out, dump, n, seed):
    pool = []
    for line in open(dump, encoding="utf-8", errors="replace"):
        p = line.rstrip("\n").split("\t")
        if len(p) >= 7 and p[0] == "config":
            pool.append(p)
    pool.sort()
    print(f"{len(pool)} config links in the dump")
    rows = []
    for p in random.Random(seed).sample(pool, min(n, len(pool))):
        proto, kind, conf, src, dst, label = p[:6]
        sp, sl = src.rsplit(":", 1)
        dp, dl = dst.rsplit(":", 1)
        rows.append({"link": label, "kind": kind, "confidence": conf, "source": src,
                     "source_text": line_of(repo, sp, int(sl)), "target": dst,
                     "target_text": line_of(repo, dp, int(dl))})
    write(rows, out)


def score(path, minimum, floor=0.0):
    """Precision over the labelled rows with confidence ≥ floor (all rows when floor is 0)."""
    rows = [line.rstrip("\n").split("\t") for line in open(path)][1:]
    good = [r for r in rows if r[7].strip().lower() in ("correct", "plausible")]
    bad = [r for r in rows if r[7].strip().lower() in ("incorrect", "implausible")]
    total = len(good) + len(bad)
    p_all = len(good) / total if total else 0.0
    g = [r for r in good if float(r[2]) >= floor]
    n = g + [r for r in bad if float(r[2]) >= floor]
    p = len(g) / len(n) if n else 0.0
    if floor > 0:
        print(f"{os.path.basename(path)}: links ≥ {floor} (dependencies): {len(g)}/{len(n)} = {p:.2f} "
              f"(need ≥ {minimum}) {'PASS' if p >= minimum else 'FAIL'}; all drawn links incl. lexical "
              f"candidates: {len(good)}/{total} = {p_all:.2f}")
    else:
        print(f"{os.path.basename(path)}: {len(good)}/{total} = {p:.2f} (need ≥ {minimum}) "
              f"{'PASS' if p >= minimum else 'FAIL'}")
    for lo, hi in ((0.8, 1.01), (0.6, 0.8), (0.0, 0.6)):
        band = [r for r in good + bad if lo <= float(r[2]) < hi]
        ok = sum(1 for r in band if r in good)
        print(f"  confidence [{lo:.2f}, {min(hi, 1):.2f}{']' if hi > 1 else ')'}: {ok}/{len(band)}")
    cands = [r for r in good + bad if float(r[2]) <= 0.5]
    if cands:
        print(f"  lexical candidates (≤ 0.5, drawn faded, not dependencies): "
              f"{sum(1 for r in cands if r in good)}/{len(cands)} plausible")
        for c in sorted({round(float(r[2]), 2) for r in cands}, reverse=True):
            at = [r for r in cands if round(float(r[2]), 2) == c]
            print(f"    at {c:.2f}: {sum(1 for r in at if r in good)}/{len(at)}")
    kinds = {}
    for r in rows:
        kinds[r[1]] = kinds.get(r[1], 0) + 1
    print(f"  kinds: {kinds}")
    for r in bad:
        print(f"  ✗ {r[0]} {r[3]} → {r[5]}: {r[8]}")
    return p >= minimum


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("mode", choices=["code", "config", "score"])
    ap.add_argument("args", nargs="+")
    ap.add_argument("--n", type=int, default=50)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--dump")
    ap.add_argument("--min", type=float, default=0.9)
    ap.add_argument("--floor", type=float, default=0.0, help="score: precision over links ≥ this confidence")
    ap.add_argument("--conf-min", type=float, default=0.0)
    ap.add_argument("--conf-max", type=float, default=1.0)
    ap.add_argument("--pool")
    a = ap.parse_args()
    if a.mode == "code":
        code_sample(os.path.abspath(a.args[0]), a.args[1], a.n, a.seed, a.conf_min, a.conf_max, a.pool)
    elif a.mode == "config":
        config_sample(os.path.abspath(a.args[0]), a.args[1], a.dump, a.n, a.seed)
    else:
        sys.exit(0 if score(a.args[0], a.min, a.floor) else 1)


if __name__ == "__main__":
    main()
