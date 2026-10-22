"""DESIGN §9 / B2: tile query p95 < 10 ms on a real repo.

usage: python3 scripts/acceptance/tile_bench.py CLONE [--rounds 3] [--max-tiles 400]

Serves the clone, opens the snapshot view at @ and requests every non-empty tile of the levels the
web client asks for (z 0–3: one tile spans 512–1024 screen px from L1 up, web/src/data/client.ts
`tileLevel`), over one keep-alive HTTP connection, the way the browser does. Each tile is timed
from request to the last body byte, once uncompressed (the query) and once with
`Accept-Encoding: gzip` as the browser sends it (the response is compressed, not decoded here); the first round warms the server (view build, overlay and
range caches) and is not counted. Per level: median, p95 and max of the timed rounds; PASS when
p95 < 10 ms. Exits 1 when a level fails.
"""
import argparse
import http.client
import json
import os
import random
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import PORT, Server, Table, get, head_commit, http as call  # noqa: E402

TILE_BASE_WU = 4096
BUDGET_MS = 10.0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("clone")
    ap.add_argument("--rounds", type=int, default=3)
    ap.add_argument("--max-tiles", type=int, default=400, help="non-empty tiles per level (sampled)")
    a = ap.parse_args()
    repo = os.path.abspath(a.clone)
    t = Table("tile_bench", f"Tile query latency @ {head_commit(repo)} (budget p95 < {BUDGET_MS:g} ms)")
    with Server(repo) as srv:
        vid = srv.view()
        boot = get(f"/views/{vid}/bootstrap")
        w = boot["world"]
        conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=60)

        def fetch(z, x, y, enc="identity"):
            t0 = time.perf_counter()
            conn.request("GET", f"/api/v1/views/{vid}/tiles/{z}/{x}/{y}", headers={"Accept-Encoding": enc})
            r = conn.getresponse()
            body = r.read()
            return (time.perf_counter() - t0) * 1000, body

        for z in (0, 1, 2, 3):
            size = TILE_BASE_WU << z
            x0, y0 = int(w["x"] // size), int(w["y"] // size)
            x1, y1 = int((w["x"] + w["w"]) // size), int((w["y"] + w["h"]) // size)
            tiles = []
            for x in range(x0, x1 + 1):
                for y in range(y0, y1 + 1):
                    _, body = fetch(z, x, y)  # warm-up round, and find the non-empty tiles
                    d = json.loads(body)
                    if d.get("windows") or d.get("edges"):
                        tiles.append((x, y, len(d.get("windows", [])), len(d.get("edges", [])), len(body)))
            random.Random(z).shuffle(tiles)
            tiles = tiles[: a.max_tiles]
            for enc in ("identity", "gzip"):
                ms = []
                for _ in range(a.rounds):
                    for x, y, *_ in tiles:
                        ms.append(fetch(z, x, y, enc)[0])
                ms.sort()
                p95 = ms[min(len(ms) - 1, int(0.95 * len(ms)))]
                med = ms[len(ms) // 2]
                t.row(f"z{z} {enc}: {len(tiles)} non-empty tiles × {a.rounds}: median / p95 / max ms",
                      f"p95 < {BUDGET_MS:g}", f"{med:.2f} / {p95:.2f} / {ms[-1]:.1f}", "—", p95 < BUDGET_MS)
            big = max(tiles, key=lambda r: r[4])
            t.note(f"z{z}: largest tile {big[2]} windows, {big[3]} edges, {big[4] // 1024} KiB; "
                   f"mean {sum(r[2] for r in tiles) / max(len(tiles), 1):.0f} windows, "
                   f"{sum(r[3] for r in tiles) / max(len(tiles), 1):.0f} edges")
        conn.close()
        ref = call("GET", "/repo")
    t.note(f"view {vid}: {ref.get('name', '')} {boot['world']['w']:.0f}×{boot['world']['h']:.0f} wu; "
           "HTTP round trip on 127.0.0.1 from Python, keep-alive")
    t.write()
    sys.exit(1 if t.failed() else 0)


if __name__ == "__main__":
    main()
