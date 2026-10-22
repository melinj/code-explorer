"""B4/B3/B5 acceptance: reproduce DESIGN appendix C on any repo with cv's own index.

usage: python3 scripts/acceptance/appendix_c.py CLONE [--axes-json CACHE]

Runs docs/analysis/axes.py (the reference) and `cv serve` on the same clone at the same commit,
then compares: lane file counts, code-lane heat grade counts, connectedness (2-hop code dependents,
quantile grades), fan-in, test dependents, quadrants. cv's per-file numbers come from
GET /views/:id/metrics?level=file (&basis=imports for the file import graph). Exits 1 when a row
fails. Tolerances: lane counts ±1%; graph metrics ±5%; grade counts
±10% or ±10 files (cv's churn stops early when the grades settle, axes.py always reads 400 commits,
and cv resolves imports with its language packs instead of ast + relative specifiers).
Appendix C's own 2026-10-18 numbers (measured on the reference repo) are shown for comparison.

Lane rows: axes.py's lane rule is appendix C's (test and docs directories before config, no
vendored/generated rule). The product's rule changed on purpose (D15: config files are config
wherever they are, so the config rule goes before test and docs; vendored and generated files are
other; DESIGN §4.3, packs/lanes/defaults.toml). The PASS/FAIL lane rows therefore compare cv with
`d15_lane` below: axes.py's own rules, unchanged, tried in the D15 order with the vendored/generated
rule added. axes.py's counts as written stay as info rows, and the note counts the files the two
rules place differently, so the gap to appendix C is accounted for file by file.
"""
import argparse
import math
import os
import re
import subprocess
import sys
from collections import Counter

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import Server, Table, get, head_commit, reference, within  # noqa: E402

GRAPH_EXT = (".py", ".ts", ".tsx", ".js", ".jsx")
APPENDIX_C = {"lanes": {"infra": 11, "config": 955, "code": 1488, "test": 2061, "docs": 1093},
              "heat": [95, 104, 132, 131, 1019], "conn": [101, 152, 272, 484, 472],
              "quad": {"hot+central": 73, "hot+peripheral": 126, "cold+central": 180, "cold+peripheral": 1102},
              "max2hop": 562, "median2hop": 2, "fanin_max": 735, "tested": 1050, "tests_max": 507}


# axes.py's lane rules (docs/analysis/axes.py `lane`), tried in the D15 order (other, infra, config,
# test, docs, code) with packs/lanes/defaults.toml's vendored/generated rule in `other`.
OTHER_EXT = (".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".svg", ".pt", ".bin", ".npz", ".parquet",
             ".zip", ".gz", ".tgz", ".woff", ".woff2", ".ttf", ".mp4", ".wav", ".pdf", ".db", ".sqlite")
GENERATED_EXT = ("_pb2.py", "_pb2.pyi", "_pb2_grpc.py", ".pb.go", ".pb.h", ".pb.cc")
VENDOR_DIRS = {"node_modules", "vendor", "vendored", "third_party", "third-party", "thirdparty", "generated",
               "__generated__"}
LOCKFILES = {"uv.lock", "package-lock.json", "yarn.lock", "pnpm-lock.yaml", "poetry.lock", "cargo.lock"}
INFRA_NAMES = {".dockerignore", "docker-compose.yml", "docker-compose.yaml", "compose.yml", "compose.yaml",
               "makefile", "justfile", "procfile", "jenkinsfile", ".gitlab-ci.yml"}
CONFIG_EXT = (".yaml", ".yml", ".toml", ".json", ".ini", ".cfg", ".conf", ".properties")
CODE_EXT = (".py", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".rs", ".go", ".java", ".rb", ".c", ".cc",
            ".cpp", ".h", ".hpp", ".css", ".scss", ".html", ".jinja", ".j2", ".vue", ".svelte", ".sql")


def d15_lane(repo, path):
    p = path.lower()
    name = p.rsplit("/", 1)[-1]
    parts = p.split("/")
    if (name in LOCKFILES or any(d in VENDOR_DIRS for d in parts[:-1]) or name.endswith(OTHER_EXT + GENERATED_EXT)
            or ".min." in name or ".generated." in name):
        return "other"
    if (name.startswith("dockerfile") or name.endswith(".dockerfile") or name in INFRA_NAMES
            or p.startswith(".github/workflows/") or name.endswith((".tf", ".tfvars", ".hcl", ".service", ".sh"))):
        return "infra"
    if name.startswith(".env") or name.endswith(CONFIG_EXT) or name in (".python-version", ".gitignore"):
        if name.endswith((".yaml", ".yml")):
            try:
                head = open(os.path.join(repo, path), encoding="utf-8", errors="replace").read(4000)
                if re.search(r"^apiVersion:", head, re.M) and re.search(r"^kind:", head, re.M):
                    return "infra"
            except OSError:
                pass
        return "config"
    if (any(d in ("tests", "test", "__tests__", "e2e") for d in parts[:-1])
            or name.startswith("test_") or name.endswith("_test.py") or name == "conftest.py"
            or re.search(r"\.(test|spec)\.[jt]sx?$", name)):
        return "test"
    if name.endswith((".md", ".rst")) or (parts[0] in ("docs", "doc") and len(parts) > 1):
        return "docs"
    if name.endswith(CODE_EXT):
        return "code"
    return "other"


def appendix_lane(repo, path):
    """axes.py's `lane` as written (appendix C's order: test and docs before config, no
    vendored/generated rule); used only to count which files the D15 order moves."""
    p = path.lower()
    name = p.rsplit("/", 1)[-1]
    parts = p.split("/")
    if name in LOCKFILES or "node_modules" in parts or name.endswith(OTHER_EXT) or ".min." in name:
        return "other"
    if (name.startswith("dockerfile") or name.endswith(".dockerfile") or name in INFRA_NAMES
            or p.startswith(".github/workflows/") or name.endswith((".tf", ".tfvars", ".hcl", ".service", ".sh"))):
        return "infra"
    if (any(d in ("tests", "test", "__tests__", "e2e") for d in parts[:-1])
            or name.startswith("test_") or name.endswith("_test.py") or name == "conftest.py"
            or re.search(r"\.(test|spec)\.[jt]sx?$", name)):
        return "test"
    if name.endswith((".md", ".rst")) or parts[0] in ("docs", "doc"):
        return "docs"
    if name.startswith(".env") or name.endswith(CONFIG_EXT) or name in (".python-version", ".gitignore"):
        return d15_lane(repo, path) if name.endswith((".yaml", ".yml")) else "config"  # the k8s sniff
    if name.endswith(CODE_EXT):
        return "code"
    return "other"


def pct_rank(values):
    items = sorted(values.items(), key=lambda kv: kv[1])
    out, i = {}, 0
    while i < len(items):
        j = i
        while j + 1 < len(items) and items[j + 1][1] == items[i][1]:
            j += 1
        for k in range(i, j + 1):
            out[items[k][0]] = (i + j) / 2 / max(len(items) - 1, 1)
        i = j + 1
    return out


def spearman(xs, ys):
    keys = list(xs)
    rx, ry = pct_rank(xs), pct_rank(ys)
    mx, my = sum(rx.values()) / len(keys), sum(ry.values()) / len(keys)
    cov = sum((rx[k] - mx) * (ry[k] - my) for k in keys)
    vx = math.sqrt(sum((rx[k] - mx) ** 2 for k in keys))
    vy = math.sqrt(sum((ry[k] - my) ** 2 for k in keys))
    return cov / (vx * vy) if vx and vy else float("nan")


def conn_rows(t, code, second, third, tag, strict):
    """Connectedness rows for one basis; `strict` rows are PASS/FAIL, others are info."""
    ok = (lambda b: b) if strict else (lambda b: None)
    v2 = {k: p["conn"]["dependents2"] for k, p in code.items()}
    prof = {v["metric"]: v for v in second["variants"]}
    d2 = prof["dependents_2hop_code"]
    vals = sorted(v2.values())
    t.row(f"{tag} 2-hop dependents max (appendix C {APPENDIX_C['max2hop']})", d2["max"], vals[-1], "±5%",
          ok(within(d2["max"], vals[-1], 0.05, 1)))
    t.row(f"{tag} 2-hop dependents median (appendix C {APPENDIX_C['median2hop']})", d2["median"],
          vals[len(vals) // 2], "±1", ok(within(d2["median"], vals[len(vals) // 2], 0, 1)))
    t.row(f"{tag} 2-hop dependents distinct values", d2["distinct_values"], len(set(vals)), "±10%",
          ok(within(d2["distinct_values"], len(set(vals)), 0.10, 2)))
    ref_c = third["conn_grade_counts"]
    got_c = Counter(p["conn"]["grade"] for p in code.values())
    cnames = ["Core", "High", "Medium", "Low", "Isolated"]
    for i, g in enumerate((4, 3, 2, 1, 0)):
        ref, got = ref_c.get(str(g), 0), got_c.get(g, 0)
        t.row(f"{tag} conn grade {cnames[i]} (appendix C {APPENDIX_C['conn'][i]})", ref, got, "±10% / ±10",
              ok(within(ref, got, 0.10, 10)))
    fanin = max(p["conn"]["fanIn"] for p in code.values())
    t.row(f"{tag} fan-in max (appendix C {APPENDIX_C['fanin_max']})", prof["fan_in"]["max"], fanin, "±5%",
          ok(within(prof["fan_in"]["max"], fanin, 0.05, 1)))
    tested = sum(1 for p in code.values() if p["conn"]["tests"] > 0)
    tmax = max(p["conn"]["tests"] for p in code.values())
    td = second["tests_direct"]
    t.row(f"{tag} code files with a direct test (appendix C {APPENDIX_C['tested']})",
          td["code_files_with_any_test"], tested, "±5%", ok(within(td["code_files_with_any_test"], tested, 0.05, 1)))
    t.row(f"{tag} most-tested file's tests (appendix C {APPENDIX_C['tests_max']})", td["max"], tmax, "±5%",
          ok(within(td["max"], tmax, 0.05, 1)))
    # Quadrants with cv's own grades.
    quad = Counter(("hot" if p["heat"]["grade"] >= 3 else "cold") + "+" +
                   ("central" if p["conn"]["grade"] >= 3 else "peripheral") for p in code.values())
    for k in ("hot+central", "hot+peripheral", "cold+central", "cold+peripheral"):
        ref, got = third["quadrants_code_2hop"].get(k, 0), quad.get(k, 0)
        t.row(f"{tag} quadrant {k} (appendix C {APPENDIX_C['quad'][k]})", ref, got, "±10% / ±10",
              ok(within(ref, got, 0.10, 10)))
    rate = {k: p["heat"]["rate"] for k, p in code.items()}
    rho = spearman(rate, v2)
    t.row(f"{tag} ρ(heat, 2-hop dependents) (appendix C 0.30)", d2["spearman_with_heat"], f"{rho:.3f}", "±0.05",
          ok(abs(rho - d2["spearman_with_heat"]) <= 0.05))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("clone")
    ap.add_argument("--axes-json", default=None, help="cache of axes.py output (reused if present)")
    a = ap.parse_args()
    repo = os.path.abspath(a.clone)
    first, second, third = reference("axes.py", repo, a.axes_json, 400)
    # Appendix C read ancestors(@, 400); cv reads as many commits as it is allowed (and stops early
    # once the grades settle), so give it the same budget.
    with Server(repo, args=["--max-commits", "400"]) as srv:
        vid = srv.view()
        boot = get(f"/views/{vid}/bootstrap")
        pts = get(f"/views/{vid}/metrics?level=file&limit=100000")["points"]
        imp = get(f"/views/{vid}/metrics?level=file&basis=imports&limit=100000")["points"]
    t = Table("appendix_c", f"Appendix C reproduction @ {head_commit(repo)} (axes.py vs cv)")

    # Lanes: PASS/FAIL against axes.py's rules in the D15 order; axes.py as written is info.
    cv_lane_files = {band["lane"]: band["files"] for band in boot["lanes"]}
    files = [f for f in subprocess.run(["jj", "--ignore-working-copy", "file", "list", "-r", "@"], cwd=repo,
                                       capture_output=True, text=True, check=True).stdout.splitlines() if f]
    d15 = {f: d15_lane(repo, f) for f in files}
    old = {f: appendix_lane(repo, f) for f in files}
    d15_count = Counter(d15.values())
    for ln in ("infra", "config", "code", "test", "docs"):
        ref = d15_count.get(ln, 0)
        got = cv_lane_files.get(ln, 0)
        t.row(f"lane files: {ln} (axes.py rules, D15 order)", ref, got, "±1%", within(ref, got, 0.01, 1))
    for ln in ("infra", "config", "code", "test", "docs"):
        t.row(f"[axes.py as written, info] lane files: {ln} (appendix C {APPENDIX_C['lanes'][ln]})",
              first["lanes"][ln]["files"], cv_lane_files.get(ln, 0), "±1%", None)
    agree = sum(1 for ln in ("infra", "config", "code", "test", "docs")
                if Counter(old.values()).get(ln, 0) == first["lanes"][ln]["files"])
    moves = Counter(f"{old[f]}→{d15[f]}" for f in files if old[f] != d15[f])
    t.note(f"Lane rule: {sum(moves.values())} files move between axes.py's order and the D15 order "
           f"({', '.join(f'{k} {v}' for k, v in sorted(moves.items()))}); this script's copy of axes.py's "
           f"order reproduces axes.py's own counts on {agree}/5 lanes.")

    code = {p["label"]: p for p in pts if p["lane"] == "code" and p["label"].endswith(GRAPH_EXT)}
    code_imp = {p["label"]: p for p in imp if p["lane"] == "code" and p["label"].endswith(GRAPH_EXT)}
    t.row("code files with graph data", first["graph"]["code_nodes"], len(code), "±1%",
          within(first["graph"]["code_nodes"], len(code), 0.01, 1))

    # Heat grade counts (code lane), Hot..Frozen = 4..0.
    ref_h = third["heat_grade_counts_code"]
    got_h = Counter(p["heat"]["grade"] for p in code.values())
    names = ["Hot", "Warm", "Mild", "Cool", "Frozen"]
    for i, g in enumerate((4, 3, 2, 1, 0)):
        ref, got = ref_h.get(str(g), 0), got_h.get(g, 0)
        t.row(f"heat grade {names[i]} (appendix C {APPENDIX_C['heat'][i]})", ref, got, "±10% / ±10",
              within(ref, got, 0.10, 10))

    # Connectedness. Appendix C defines it per file on the import graph; that definition is
    # reproduced exactly by cv's file import graph (basis=imports) and must match. The product
    # grades windows over typed links (DESIGN §4.8), a different metric: its per-file aggregate
    # (max over the file's windows) is reported as info rows and documented in §4.8.
    conn_rows(t, code_imp, second, third, "[imports]", True)
    conn_rows(t, code, second, third, "[typed links, info]", False)

    settled = sum(1 for p in code.values() if p["heat"]["settled"])
    t.note(f"cv: {len(pts)} file points; {settled}/{len(code)} code files settled. "
           f"axes.py: {first['commits_scanned']} commits, effective sample {first['effective_sample']}, "
           f"graph {first['graph']['nodes']} nodes / {first['graph']['edges']} edges, largest SCC "
           f"{second['largest_scc']['files']} files.")
    t.note("[imports]: GET /views/:id/metrics?level=file&basis=imports (cv's file import graph, appendix C's "
           "definition; PASS/FAIL). [typed links, info]: level=file, the max over each file's windows of the "
           "window-level typed-link connectedness the product uses (DESIGN §4.8); not comparable by design.")
    t.write()
    sys.exit(1 if t.failed() else 0)


if __name__ == "__main__":
    main()
