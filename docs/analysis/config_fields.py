"""Read-only prototype: where are config fields (YAML/TOML/JSON keys) used in code?

Usage: python3 -I docs/analysis/config_fields.py <repo> [min_score]
Runs jj with --ignore-working-copy --at-op @ (no snapshot, no new operation) to list tracked files,
reads files from disk, never executes repo code. Prints a JSON summary plus a sample of links
for spot-checking. This is the reference heuristic for the config-fields channel pack (P5).
"""
import json
import math
import os
import random
import re
import subprocess
import sys
import tomllib
from collections import Counter, defaultdict

REPO = os.path.abspath(sys.argv[1])
MIN_SCORE = float(sys.argv[2]) if len(sys.argv) > 2 else 0.65
JJ = ["jj", "--ignore-working-copy", "--at-op", "@", "--color=never", "--no-pager"]

files = subprocess.run(JJ + ["file", "list", "-r", "@"], check=True, capture_output=True, text=True, cwd=REPO).stdout.split("\n")
files = [f for f in files if f]

LOCK = {"package-lock.json", "uv.lock", "pnpm-lock.yaml", "yarn.lock", "poetry.lock"}
SKIP_DIRS = {"node_modules", "docs", "tests", "test", "__tests__", "e2e"}


def is_config(f):
    name = f.rsplit("/", 1)[-1].lower()
    parts = f.lower().split("/")
    if name in LOCK or any(p in SKIP_DIRS for p in parts[:-1]) or ".github" in parts:
        return False
    return name.endswith((".yaml", ".yml", ".toml", ".json")) and not name.startswith(("tsconfig", "jsconfig"))


def is_code(f):
    name = f.rsplit("/", 1)[-1]
    parts = f.split("/")
    if any(p in SKIP_DIRS for p in parts[:-1]) or name.startswith("test_") or name.endswith("_test.py") or name == "conftest.py":
        return False
    return name.endswith((".py", ".ts", ".tsx", ".js", ".jsx")) and ".test." not in name and ".spec." not in name


def read(f):
    try:
        with open(os.path.join(REPO, f), encoding="utf-8", errors="replace") as fh:
            return fh.read()
    except OSError:
        return ""


# ── 1. Config keys: (file, key path, leaf name, line) ───────────────────────
YAML_KEY = re.compile(r"^(\s*)(-\s+)?([A-Za-z_][\w.-]*)\s*:(?:\s+(.*))?$")


def yaml_keys(text):
    out, stack, block = [], [], None
    for no, line in enumerate(text.split("\n"), 1):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        indent = len(line) - len(line.lstrip())
        if block is not None:
            if indent > block:
                continue  # inside a | or > block scalar
            block = None
        m = YAML_KEY.match(line)
        if not m:
            continue
        eff = indent + (len(m.group(2)) if m.group(2) else 0)
        while stack and stack[-1][0] >= eff:
            stack.pop()
        key = m.group(3)
        stack.append((eff, key))
        out.append((".".join(k for _, k in stack), key, no))
        value = (m.group(4) or "").strip()
        if value[:1] in ("|", ">"):
            block = eff
    return out


def tree_keys(obj, prefix=""):
    if isinstance(obj, dict):
        for k, v in obj.items():
            if isinstance(k, str) and re.fullmatch(r"[A-Za-z_][\w.-]*", k):
                path = f"{prefix}.{k}" if prefix else k
                yield path, k
                yield from tree_keys(v, path)
    elif isinstance(obj, list):
        for v in obj[:50]:
            yield from tree_keys(v, prefix)


def locate(text, key):
    m = re.search(r'(^|[\s{,"])' + re.escape(key) + r'"?\s*[:=]', text, re.M)
    return text.count("\n", 0, m.start()) + 1 if m else 1


config_keys = defaultdict(list)  # file -> [(path, leaf, line)]
for f in filter(is_config, files):
    text = read(f)
    if not text:
        continue
    if f.endswith((".yaml", ".yml")):
        config_keys[f] = yaml_keys(text)
    else:
        try:
            data = tomllib.loads(text) if f.endswith(".toml") else json.loads(text)
        except Exception:
            continue
        config_keys[f] = [(p, k, locate(text, k)) for p, k in tree_keys(data)]

# ── 2. Code index: where identifiers appear and how ──────────────────────────
ACCESS = re.compile(r"""(?:\[\s*|\bget\w*\(\s*|\w*[cC]onfig\w*\(\s*|\bparams?\w*\(\s*)["']([A-Za-z_][\w-]{2,})["']""")
STRING = re.compile(r"""["']([A-Za-z_][\w-]{2,})["']""")
ATTR = re.compile(r"\.([A-Za-z_]\w{2,})\b")
FIELD = re.compile(r"^\s+([a-z_]\w{2,})\s*:\s*[A-Za-z\"']", re.M)

code_files = [f for f in files if is_code(f)]
sites = defaultdict(list)  # token -> [(file, line, kind)]
doc_freq = Counter()
code_text = {}
for f in code_files:
    text = read(f)
    code_text[f] = text
    seen = set()
    for kind, rx in (("access", ACCESS), ("string", STRING), ("attr", ATTR), ("field", FIELD)):
        for m in rx.finditer(text):
            tok = m.group(1)
            line = text.count("\n", 0, m.start()) + 1
            sites[tok].append((f, line, kind))
            seen.add(tok)
    for tok in seen:
        doc_freq[tok] += 1

N = len(code_files)
STOP = {"name", "type", "id", "description", "version", "value", "data", "path", "key", "enabled", "default",
        "items", "title", "label", "kind", "mode", "model", "source", "target", "url", "port", "host", "text",
        "content", "config", "params", "inputs", "outputs", "optional", "required", "properties", "format"}
BASE = {"access": 0.55, "field": 0.35, "string": 0.35, "attr": 0.2}


def score(cfg, key, code, kind):
    s = BASE[kind]
    idf = math.log((N + 1) / (doc_freq[key] + 1))
    s += 0.3 * min(idf / math.log(N + 1), 1.0)
    cdir, kdir = os.path.dirname(code), os.path.dirname(cfg)
    if cdir == kdir:
        s += 0.3
    elif os.path.dirname(cdir) == os.path.dirname(kdir):
        s += 0.15
    text = code_text[code]
    if cfg in text:
        s += 0.3
    elif os.path.basename(cfg) in text:
        s += 0.25  # the code loads files of this family (e.g. every node.yaml)
    if key.lower() in STOP:
        s *= 0.5
    return min(s, 1.0)


links = []  # (score, cfg, path, cfg_line, code, code_line, kind)
for cfg, keys in config_keys.items():
    for path, key, line in keys:
        best = {}
        for code, cline, kind in sites.get(key, ()):
            sc = score(cfg, key, code, kind)
            if sc >= MIN_SCORE and sc > best.get((code, cline), (0,))[0]:
                best[(code, cline)] = (sc, kind)
        for (code, cline), (sc, kind) in sorted(best.items(), key=lambda kv: -kv[1][0])[:5]:
            links.append((sc, cfg, path, line, code, cline, kind))

total_keys = sum(len(v) for v in config_keys.values())
linked_keys = {(l[1], l[2]) for l in links}
linked_files = {l[1] for l in links}
family = Counter(os.path.basename(f) for f in config_keys)


def line_text(f, n):
    lines = (code_text.get(f) or read(f)).split("\n")
    return lines[n - 1].strip()[:110] if 0 < n <= len(lines) else ""


rng = random.Random(7)
sample = rng.sample(links, min(14, len(links)))
summary = {
    "repo": REPO,
    "min_score": MIN_SCORE,
    "config_files": len(config_keys),
    "config_keys": total_keys,
    "code_files": N,
    "links": len(links),
    "keys_with_a_reader": len(linked_keys),
    "keys_with_a_reader_pct": round(100 * len(linked_keys) / max(total_keys, 1), 1),
    "files_with_a_linked_key": len(linked_files),
    "link_kinds": dict(Counter(l[6] for l in links)),
    "largest_families": family.most_common(6),
    "strongest": [f"{l[0]:.2f} {l[1]}#{l[2]} ← {l[4]}:{l[5]} [{l[6]}] {line_text(l[4], l[5])}" for l in sorted(links, reverse=True)[:8]],
    "random_sample": [f"{l[0]:.2f} {l[1]}#{l[2]} ← {l[4]}:{l[5]} [{l[6]}] {line_text(l[4], l[5])}" for l in sample],
}
print(json.dumps(summary, indent=1))

# ── Family hubs: one hub per (file family, leaf key), as the canvas draws them ──
hubs = defaultdict(lambda: {"definers": set(), "readers": set()})
for sc, cfg, path, line, code, cline, kind in links:
    h = hubs[(os.path.basename(cfg), path.rsplit(".", 1)[-1])]
    h["definers"].add(cfg)
    h["readers"].add((code, cline))
biggest = sorted(hubs.items(), key=lambda kv: -len(kv[1]["definers"]))[:6]
print(json.dumps({
    "family_hubs": len(hubs),
    "direct_links": len(links),
    "edges_via_hubs": sum(len(h["definers"]) + len(h["readers"]) for h in hubs.values()),
    "biggest_hubs": [f"{base}#{leaf}: {len(h['definers'])} files → {len(h['readers'])} read sites" for (base, leaf), h in biggest],
}, indent=1))
