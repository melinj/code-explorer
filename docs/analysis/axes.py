"""Read-only analysis of a jj repo: change frequency vs connectedness, and file-role lanes.

Usage: python3 -I axes.py <repo> [commits]
Runs jj with --ignore-working-copy --at-op @ (no snapshot, no new operation) and parses Python
imports with ast (never executes repo code). Prints a JSON summary.
"""
import ast
import json
import math
import os
import re
import subprocess
import sys
from collections import Counter, defaultdict
from datetime import datetime, timezone

REPO = os.path.abspath(sys.argv[1])
N_COMMITS = int(sys.argv[2]) if len(sys.argv) > 2 else 400
JJ = ["jj", "--ignore-working-copy", "--at-op", "@", "--color=never", "--no-pager", "-R", REPO]


def jj(*args):
    # cwd=REPO: jj prints paths relative to the current directory.
    return subprocess.run(JJ + list(args), check=True, capture_output=True, text=True, cwd=REPO).stdout


# ── Lanes ────────────────────────────────────────────────────────────────────
OTHER_EXT = (".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".svg", ".pt", ".bin", ".npz", ".parquet",
             ".zip", ".gz", ".tgz", ".woff", ".woff2", ".ttf", ".mp4", ".wav", ".pdf", ".db", ".sqlite")
LOCKFILES = {"uv.lock", "package-lock.json", "yarn.lock", "pnpm-lock.yaml", "poetry.lock", "cargo.lock"}
INFRA_NAMES = {".dockerignore", "docker-compose.yml", "docker-compose.yaml", "compose.yml", "compose.yaml",
               "makefile", "justfile", "procfile", "jenkinsfile", ".gitlab-ci.yml"}
CONFIG_EXT = (".yaml", ".yml", ".toml", ".json", ".ini", ".cfg", ".conf", ".properties")
CODE_EXT = (".py", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".rs", ".go", ".java", ".rb", ".c", ".cc",
            ".cpp", ".h", ".hpp", ".css", ".scss", ".html", ".jinja", ".j2", ".vue", ".svelte", ".sql")


def lane(path):
    p = path.lower()
    name = p.rsplit("/", 1)[-1]
    parts = p.split("/")
    if name in LOCKFILES or "node_modules" in parts or name.endswith(OTHER_EXT) or ".min." in name:
        return "other"
    if (name.startswith("dockerfile") or name.endswith(".dockerfile") or name in INFRA_NAMES
            or p.startswith(".github/workflows/") or name.endswith((".tf", ".tfvars", ".hcl", ".service"))
            or name.endswith(".sh")):
        return "infra"
    if (any(d in ("tests", "test", "__tests__", "e2e") for d in parts[:-1])
            or name.startswith("test_") or name.endswith("_test.py") or name == "conftest.py"
            or re.search(r"\.(test|spec)\.[jt]sx?$", name)):
        return "test"
    if name.endswith((".md", ".rst")) or parts[0] in ("docs", "doc"):
        return "docs"
    if name.startswith(".env") or name.endswith(CONFIG_EXT) or name in (".python-version", ".gitignore"):
        if name.endswith((".yaml", ".yml")):
            try:
                head = open(os.path.join(REPO, path), encoding="utf-8", errors="replace").read(4000)
                if re.search(r"^apiVersion:", head, re.M) and re.search(r"^kind:", head, re.M):
                    return "infra"
            except OSError:
                pass
        return "config"
    if name.endswith(CODE_EXT):
        return "code"
    return "other"


# ── Files at @ and history ───────────────────────────────────────────────────
files = [l for l in jj("file", "list", "-r", "@").splitlines() if l]
fileset = set(files)
lanes = {f: lane(f) for f in files}

tmpl = 'json(self) ++ "\\t" ++ self.diff().files().map(|f| f.path()).join("\\t") ++ "\\n"'
raw = jj("log", "-r", f"ancestors(@, {N_COMMITS}) ~ root()", "--no-graph", "-T", tmpl)
now = datetime.now(timezone.utc)
touch = Counter()
weighted = defaultdict(float)
W = W2 = 0.0
commits = 0
for line in raw.splitlines():
    if not line.strip():
        continue
    meta, *paths = line.split("\t")
    m = json.loads(meta)
    when = datetime.fromisoformat(m["committer"]["timestamp"])
    age = max((now - when).total_seconds() / 86400, 0)
    paths = [p for p in paths if p]
    if not paths:
        continue
    commits += 1
    w = 2 ** (-age / 90) * (1 if len(paths) <= 50 else 1 / math.log2(1 + len(paths)))
    W += w
    W2 += w * w
    for p in paths:
        if p in fileset:
            touch[p] += 1
            weighted[p] += w
rate = {f: weighted.get(f, 0.0) / W if W else 0.0 for f in files}


def mass_grades(values):
    """Change-mass grades: hot = top files holding 50% of mass, warm next 25%, mild next 15%, cool rest > 0."""
    total = sum(v for v in values.values() if v > 0)
    acc, out = 0.0, {}
    for f, v in sorted(values.items(), key=lambda kv: -kv[1]):
        if v <= 0:
            out[f] = 0
            continue
        share = acc / total
        out[f] = 4 if share < 0.5 else 3 if share < 0.75 else 2 if share < 0.9 else 1
        acc += v
    return out


heat = mass_grades(rate)

# ── Import graph (Python via ast, TS/JS via relative import specifiers) ─────────
code_files = [f for f in files if lanes[f] in ("code", "test") and f.endswith((".py", ".ts", ".tsx", ".js", ".jsx"))]
mod2file = {}
for f in files:
    if f.endswith(".py"):
        mod = f[:-3].replace("/", ".")
        if mod.endswith(".__init__"):
            mod = mod[: -len(".__init__")]
        mod2file[mod] = f


def py_targets(f, tree):
    pkg = f[:-3].replace("/", ".").rsplit(".", 1)[0] if "/" in f else ""
    if f.endswith("__init__.py"):
        pkg = f[: -len("/__init__.py")].replace("/", ".")
    out = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for a in node.names:
                parts = a.name.split(".")
                for i in range(len(parts), 0, -1):
                    if ".".join(parts[:i]) in mod2file:
                        out.add(mod2file[".".join(parts[:i])])
                        break
        elif isinstance(node, ast.ImportFrom):
            base = node.module or ""
            if node.level:
                up = pkg.split(".") if pkg else []
                up = up[: len(up) - (node.level - 1)] if node.level > 1 else up
                base = ".".join([*up, base] if base else up)
            hit = False
            for a in node.names:
                cand = f"{base}.{a.name}" if base else a.name
                if cand in mod2file:
                    out.add(mod2file[cand])
                    hit = True
            if not hit and base in mod2file:
                out.add(mod2file[base])
    out.discard(f)
    return out


JS_IMPORT = re.compile(r"""(?:import\s[^'"]*?from\s*|import\s*\(\s*|require\s*\(\s*|export\s[^'"]*?from\s*)['"](\.{1,2}/[^'"]+)['"]""")


def js_targets(f, text):
    out = set()
    base = os.path.dirname(f)
    for spec in JS_IMPORT.findall(text):
        target = os.path.normpath(os.path.join(base, spec))
        for cand in (target, *(target + e for e in (".ts", ".tsx", ".js", ".jsx")),
                     *(f"{target}/index{e}" for e in (".ts", ".tsx", ".js", ".jsx"))):
            if cand in fileset:
                out.add(cand)
                break
    out.discard(f)
    return out


deps = {}
parse_fail = 0
for f in code_files:
    try:
        text = open(os.path.join(REPO, f), encoding="utf-8", errors="replace").read()
    except OSError:
        continue
    if f.endswith(".py"):
        try:
            deps[f] = py_targets(f, ast.parse(text))
        except SyntaxError:
            parse_fail += 1
            deps[f] = set()
    else:
        deps[f] = js_targets(f, text)

nodes = sorted(deps)
idx = {f: i for i, f in enumerate(nodes)}
fan_out = {f: len(deps[f]) for f in nodes}
fan_in = Counter(t for f in nodes for t in deps[f] if t in idx)

# Reach = transitive dependents: SCC condensation (iterative Kosaraju) + bitset propagation.
adj = [[idx[t] for t in deps[f] if t in idx] for f in nodes]        # A -> B : A depends on B
radj = [[] for _ in nodes]
for a, outs in enumerate(adj):
    for b in outs:
        radj[b].append(a)
n = len(nodes)
seen, order = [False] * n, []
for s in range(n):
    if seen[s]:
        continue
    stack = [(s, 0)]
    seen[s] = True
    while stack:
        v, i = stack.pop()
        if i < len(adj[v]):
            stack.append((v, i + 1))
            w = adj[v][i]
            if not seen[w]:
                seen[w] = True
                stack.append((w, 0))
        else:
            order.append(v)
comp = [-1] * n
ncomp = 0
for s in reversed(order):
    if comp[s] != -1:
        continue
    stack = [s]
    comp[s] = ncomp
    while stack:
        v = stack.pop()
        for w in radj[v]:
            if comp[w] == -1:
                comp[w] = ncomp
                stack.append(w)
    ncomp += 1
# Kosaraju numbers components in topological order of the A->B graph (dependents first).
members = defaultdict(int)
for v in range(n):
    members[comp[v]] |= 1 << v
cdeps = defaultdict(set)  # component -> components it depends on
for a in range(n):
    for b in adj[a]:
        if comp[a] != comp[b]:
            cdeps[comp[a]].add(comp[b])
cdependents = defaultdict(set)
for a, bs in cdeps.items():
    for b in bs:
        cdependents[b].add(a)
reach_bits = [0] * ncomp
for c in range(ncomp):  # dependents (lower numbers) are already done
    bits = 0
    for a in cdependents[c]:
        bits |= members[a] | reach_bits[a]
    reach_bits[c] = bits
reach = {f: bin(reach_bits[comp[idx[f]]] | members[comp[idx[f]]]).count("1") - 1 for f in nodes}


def pct_rank(values):
    items = sorted(values.items(), key=lambda kv: kv[1])
    out, i = {}, 0
    while i < len(items):
        j = i
        while j + 1 < len(items) and items[j + 1][1] == items[i][1]:
            j += 1
        r = (i + j) / 2 / max(len(items) - 1, 1)
        for k in range(i, j + 1):
            out[items[k][0]] = r
        i = j + 1
    return out


def spearman(xs, ys):
    keys = list(xs)
    rx, ry = pct_rank({k: xs[k] for k in keys}), pct_rank({k: ys[k] for k in keys})
    mx, my = sum(rx.values()) / len(keys), sum(ry.values()) / len(keys)
    cov = sum((rx[k] - mx) * (ry[k] - my) for k in keys)
    vx = math.sqrt(sum((rx[k] - mx) ** 2 for k in keys))
    vy = math.sqrt(sum((ry[k] - my) ** 2 for k in keys))
    return cov / (vx * vy) if vx and vy else float("nan")


def reach_grades(values):
    nz = sorted(v for v in values.values() if v > 0)
    def q(p):
        return nz[min(int(p * len(nz)), len(nz) - 1)] if nz else 0
    t50, t75, t90 = q(0.5), q(0.75), q(0.9)
    return {f: 0 if v == 0 else 4 if v >= t90 else 3 if v >= t75 else 2 if v >= t50 else 1 for f, v in values.items()}


code_nodes = [f for f in nodes if lanes[f] == "code"]
rg = reach_grades({f: reach[f] for f in code_nodes})
hp = pct_rank({f: rate[f] for f in code_nodes})
rp = pct_rank({f: reach[f] for f in code_nodes})
risk = {f: math.sqrt(hp[f] * rp[f]) for f in code_nodes}
top = max(1, round(0.05 * len(code_nodes)))
top_risk = sorted(code_nodes, key=lambda f: -risk[f])[:top]
top_heat = sorted(code_nodes, key=lambda f: -rate[f])[:top]
top_reach = sorted(code_nodes, key=lambda f: -reach[f])[:top]

hot = lambda f: heat[f] >= 3
central = lambda f: rg[f] >= 3
quad = Counter()
examples = defaultdict(list)
for f in sorted(code_nodes, key=lambda f: -risk[f]):
    key = ("hot" if hot(f) else "cold") + "+" + ("central" if central(f) else "peripheral")
    quad[key] += 1
    if len(examples[key]) < 4:
        examples[key].append(f"{f} (heat {heat[f]}, reach {reach[f]}, touches {touch[f]})")

# ── Cross-lane: environment variables defined in config/infra and read in code ──
ENV_READ = re.compile(r"""(?:os\.environ(?:\.get)?\s*[\[(]\s*|os\.getenv\s*\(\s*|getenv\s*\(\s*|process\.env\.)['"]?([A-Z][A-Z0-9_]{2,})""")
ENV_DEF = re.compile(r"""^\s*(?:export\s+|ENV\s+|ARG\s+|-\s*)?([A-Z][A-Z0-9_]{2,})\s*[=:]""", re.M)
reads, defs = defaultdict(set), defaultdict(set)
for f in files:
    if lanes[f] in ("code", "test") and f.endswith((".py", ".ts", ".tsx", ".js", ".jsx")):
        try:
            for v in ENV_READ.findall(open(os.path.join(REPO, f), encoding="utf-8", errors="replace").read()):
                reads[v].add(f)
        except OSError:
            pass
    elif lanes[f] in ("config", "infra") and (f.rsplit("/", 1)[-1].startswith((".env", "dockerfile", "Dockerfile", "docker-compose", "compose")) or f.endswith(".sh")):
        try:
            for v in ENV_DEF.findall(open(os.path.join(REPO, f), encoding="utf-8", errors="replace").read()):
                defs[v].add(f)
        except OSError:
            pass
shared_env = sorted(set(reads) & set(defs))

lane_stats = {}
total_w = sum(weighted.values()) or 1
for ln in ("infra", "config", "code", "test", "docs", "other"):
    fs = [f for f in files if lanes[f] == ln]
    lane_stats[ln] = {
        "files": len(fs),
        "change_mass_pct": round(100 * sum(weighted.get(f, 0) for f in fs) / total_w, 1),
        "hot_or_warm_files": sum(1 for f in fs if heat[f] >= 3),
        "hottest": [f"{f} ({touch[f]} touches)" for f in sorted(fs, key=lambda f: -rate[f])[:3] if touch[f]],
    }

out = {
    "repo": REPO,
    "commits_scanned": commits,
    "effective_sample": round(W * W / W2, 1) if W2 else 0,
    "files_at_head": len(files),
    "lanes": lane_stats,
    "graph": {"nodes": n, "edges": sum(len(a) for a in adj), "sccs": ncomp, "python_parse_failures": parse_fail,
              "code_nodes": len(code_nodes), "max_reach": max(reach.values()) if reach else 0,
              "median_reach_code": sorted(reach[f] for f in code_nodes)[len(code_nodes) // 2] if code_nodes else 0},
    "correlation_code": {
        "spearman_heat_vs_reach": round(spearman({f: rate[f] for f in code_nodes}, {f: reach[f] for f in code_nodes}), 3),
        "spearman_heat_vs_fan_in": round(spearman({f: rate[f] for f in code_nodes}, {f: fan_in.get(f, 0) for f in code_nodes}), 3),
        "spearman_heat_vs_fan_out": round(spearman({f: rate[f] for f in code_nodes}, {f: fan_out[f] for f in code_nodes}), 3),
    },
    "quadrants_code": dict(quad),
    "quadrant_examples": dict(examples),
    "top5pct": {
        "k": top,
        "review_first_also_in_top_heat": len(set(top_risk) & set(top_heat)),
        "review_first_also_in_top_reach": len(set(top_risk) & set(top_reach)),
        "top_heat_also_in_top_reach": len(set(top_heat) & set(top_reach)),
        "review_first": [f"{f} (heat {heat[f]}, reach {reach[f]})" for f in top_risk[:8]],
    },
    "env_vars": {"read_in_code": len(reads), "defined_in_config_or_infra": len(defs), "both": len(shared_env),
                 "examples": shared_env[:8]},
}
print(json.dumps(out, indent=1))

# ── Second pass: does transitive reach saturate, and which variant discriminates best? ──
scc_size = Counter(comp[v] for v in range(n))
big = scc_size.most_common(1)[0]
code_mask = 0
for f in code_nodes:
    code_mask |= 1 << idx[f]
reach_code = {f: bin((reach_bits[comp[idx[f]]] | members[comp[idx[f]]]) & code_mask).count("1") - 1 for f in code_nodes}
direct_dependents = defaultdict(set)
for a, outs in enumerate(adj):
    for b in outs:
        direct_dependents[b].add(a)
is_code = [lanes[f] == "code" for f in nodes]
two_hop = {}
for f in code_nodes:
    i = idx[f]
    s1 = {a for a in direct_dependents[i] if is_code[a]}
    s2 = set(s1)
    for a in s1:
        s2 |= {x for x in direct_dependents[a] if is_code[x]}
    s2.discard(i)
    two_hop[f] = len(s2)
tests_direct = {f: sum(1 for a in direct_dependents[idx[f]] if lanes[nodes[a]] == "test") for f in code_nodes}


def profile(name, metric):
    vals = [metric[f] for f in code_nodes]
    top_k = sorted(code_nodes, key=lambda f: -metric[f])[:top]
    hp_ = pct_rank({f: rate[f] for f in code_nodes})
    mp_ = pct_rank(metric)
    risk_ = {f: math.sqrt(hp_[f] * mp_[f]) for f in code_nodes}
    rf = sorted(code_nodes, key=lambda f: -risk_[f])[:top]
    return {
        "metric": name,
        "max": max(vals),
        "median": sorted(vals)[len(vals) // 2],
        "distinct_values": len(set(vals)),
        "share_at_or_above_half_of_code": round(sum(v >= 0.5 * len(code_nodes) for v in vals) / len(vals), 3),
        "spearman_with_heat": round(spearman({f: rate[f] for f in code_nodes}, metric), 3),
        "top5_overlap_with_top_heat": len(set(top_k) & set(top_heat)),
        "review_first_examples": [f"{f} (heat {heat[f]}, {name} {metric[f]})" for f in rf[:5]],
    }


second = {
    "largest_scc": {"files": big[1], "sample": sorted(nodes[v] for v in range(n) if comp[v] == big[0])[:6]},
    "variants": [
        profile("reach_all", {f: reach[f] for f in code_nodes}),
        profile("reach_code_only", reach_code),
        profile("dependents_2hop_code", two_hop),
        profile("fan_in", {f: fan_in.get(f, 0) for f in code_nodes}),
    ],
    "tests_direct": {"code_files_with_any_test": sum(1 for v in tests_direct.values() if v),
                     "max": max(tests_direct.values())},
}
print(json.dumps(second, indent=1))

# ── Third pass: quadrants with the chosen metric (2-hop code dependents, quantile grades) ──
nz2 = sorted(v for v in two_hop.values() if v > 0)
def q2(p):
    return nz2[min(int(p * len(nz2)), len(nz2) - 1)]
c_t = {"core": q2(0.90), "high": q2(0.75), "medium": q2(0.50)}
def conn_grade(v):
    return 0 if v == 0 else 4 if v >= c_t["core"] else 3 if v >= c_t["high"] else 2 if v >= c_t["medium"] else 1
cg = {f: conn_grade(two_hop[f]) for f in code_nodes}
quad2 = Counter(("hot" if heat[f] >= 3 else "cold") + "+" + ("central" if cg[f] >= 3 else "peripheral") for f in code_nodes)
third = {
    "conn_grade_thresholds_2hop": c_t,
    "conn_grade_counts": dict(Counter(cg.values())),
    "heat_grade_counts_code": dict(Counter(heat[f] for f in code_nodes)),
    "quadrants_code_2hop": dict(quad2),
}
print(json.dumps(third, indent=1))
