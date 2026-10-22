#!/usr/bin/env bash
# D13 / WORKSTREAMS rule 2: the core stays language- and protocol-neutral.
#
# Fails when a core crate depends, directly or transitively through normal (non-dev, non-build)
# dependencies, on
#   - a tree-sitter crate (the `tree-sitter` runtime or any `tree-sitter-*` grammar), or
#   - a pack crate: `cv-lang-<x>` (language packs and their shared support crate cv-lang-sitter)
#     or `cv-channel-<x>` (channel packs).
#
# Core = every workspace member that is not a pack crate and not a composition root. The only
# composition root is `cv-server` (the `cv` binary), which wires the packs into the registries.
# Dev-dependencies are allowed (tests may use real packs).
#
# It also fails when the analysis core (cv-lang, cv-link, cv-channel, cv-layout, cv-churn: the crates
# that consume code facts) holds language knowledge in its non-test sources: string literals naming
# a language, a source-file extension, a self receiver (`self`, `this`, `cls`), a module entry stem
# (`__init__`, `index`, `mod`, `lib`, `main`) or the dunder prefix. Packs provide these as data
# (`LanguagePack::conventions`, lane hints, channel rules). `#[cfg(test)]` modules and `tests.rs`
# files are not scanned.
#
# Usage: scripts/check-core-deps.sh [--metadata FILE]   (FILE: saved `cargo metadata` JSON)
set -euo pipefail
cd "$(dirname "$0")/.."
META=""
if [[ "${1:-}" == "--metadata" ]]; then META="$2"; fi
if [[ -z "$META" ]]; then
  META="$(mktemp)"; trap 'rm -f "$META"' EXIT
  timeout 300 cargo metadata --format-version 1 --locked > "$META"
fi
python3 -I - "$META" <<'PY'
import json, re, sys
m = json.load(open(sys.argv[1]))
COMPOSITION_ROOTS = {"cv-server"}
PACK = re.compile(r"^cv-(lang|channel)-")
FORBIDDEN = lambda n: n == "tree-sitter" or n.startswith("tree-sitter-") or PACK.match(n)
pkgs = {p["id"]: p for p in m["packages"]}
name = lambda i: pkgs[i]["name"]
# Normal dependency edges from the resolve graph (what is actually built).
edges = {}
for n in m["resolve"]["nodes"]:
    edges[n["id"]] = [d["pkg"] for d in n["deps"]
                      if any(k["kind"] is None for k in d.get("dep_kinds", [{"kind": None}]))]
core = [i for i in m["workspace_members"]
        if not PACK.match(name(i)) and name(i) not in COMPOSITION_ROOTS]
bad = []
for root in sorted(core, key=name):
    seen, stack = {root: None}, [root]
    while stack:
        cur = stack.pop()
        for d in edges.get(cur, []):
            if d in seen:
                continue
            seen[d] = cur
            if FORBIDDEN(name(d)):
                path, x = [name(d)], cur
                while x is not None:
                    path.append(name(x)); x = seen[x]
                bad.append(" -> ".join(reversed(path)))
            else:
                stack.append(d)
print("core crates:", " ".join(sorted(name(i) for i in core)))
if bad:
    print("FAIL: core crates depend on language/channel packs or tree-sitter (D13):")
    for b in bad:
        print("  " + b)
    sys.exit(1)
print("ok: no core crate depends on a grammar, tree-sitter or a pack crate")
PY
python3 -I - <<'PY'
import pathlib, re, sys
NEUTRAL = ["cv-lang", "cv-link", "cv-channel", "cv-layout", "cv-churn"]
WORDS = r"self|this|cls|super|__init__|__main__|__|index|mod|lib|main|python|typescript|javascript|rust|cpp|c\+\+|java|go"
EXT = r"py|pyi|ts|tsx|js|jsx|mjs|cjs|rs|c|cc|cpp|cxx|h|hh|hpp|hxx|go|java|rb"
LIT = re.compile(r'"(?:' + WORDS + r')"|"[^"\s]*\.(?:' + EXT + r')"')
bad = []
for c in NEUTRAL:
    for f in sorted(pathlib.Path("crates", c, "src").rglob("*.rs")):
        if f.name == "tests.rs":
            continue
        for no, line in enumerate(f.read_text().splitlines(), 1):
            if line.startswith("#[cfg(test)]"):
                break
            code = line.split("//", 1)[0] if not line.lstrip().startswith("//") else ""
            for m in LIT.finditer(code):
                bad.append(f"{f}:{no}: {m.group(0)}")
if bad:
    print("FAIL: language knowledge in the language-neutral core (D13); move it to the packs:")
    for b in bad:
        print("  " + b)
    sys.exit(1)
print("ok: no language identifiers in", " ".join(NEUTRAL))
PY
