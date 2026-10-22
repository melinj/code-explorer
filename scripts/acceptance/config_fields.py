"""P5 acceptance: cv-channel-config reproduces docs/analysis/config_fields.py on a real repo.

usage: python3 scripts/acceptance/config_fields.py CLONE [--ref-json CACHE] [--dump TSV]

Runs the reference (about 8 minutes; cached with --ref-json) and `cv index CLONE` twice with
CV_CHANNEL_DUMP=<TSV>, which makes cv print the config pack's statistics line and dump every
channel link it found:

- with CV_CONFIG_SCORING=reference, the pack's reference scoring, for the parity rows ("within
  rounding": counts ±1%, percentages ±0.5 points; also "one hub per family and key");
- with the default context scoring (node context and key genericity, WORKSTREAMS P5), which
  deliberately links fewer keys to raise plausibility: its counts are reported as info rows next to
  the reference, and its dump (--dump) is what sample_links.py draws the plausibility sample from.
"""
import argparse
import os
import re
import subprocess
import sys
import time
from collections import defaultdict

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import CV, RESULTS, ROOT, Table, head_commit, reference, within  # noqa: E402

STATS = re.compile(
    r"config: (\d+) config files, (\d+) key paths, (\d+) code files; (\d+) links; (\d+) keys with a reader "
    r"\(([\d.]+)%\); (\d+) files with a linked key; access (\d+) · string (\d+) · field (\d+) · attribute (\d+); "
    r"(\d+) family hubs, (\d+) edges via hubs")


def keyless_configs(repo):
    """Config files config_fields.py counts although they have no key paths (JSON/TOML whose keys
    are all non-identifiers, e.g. graphs keyed by node number). cv leaves them out of its
    count; they cannot carry links either way."""
    import json
    import tomllib
    # Reuse the reference's own helpers: its constants and predicates (LOCK … "# ── 1.") and tree_keys.
    src = open(os.path.join(ROOT, "docs", "analysis", "config_fields.py")).read()
    ns = {"os": os, "re": __import__("re")}
    exec(src[src.index("LOCK = "):src.index("# ── 1.")] + src[src.index("def tree_keys"):src.index("def locate")], ns)
    files = subprocess.run(["jj", "--ignore-working-copy", "--at-op", "@", "--no-pager", "file", "list", "-r", "@"],
                           cwd=repo, capture_output=True, text=True, check=True).stdout.split("\n")
    n = 0
    for f in filter(None, files):
        if not ns["is_config"](f) or f.endswith((".yaml", ".yml")):
            continue
        try:
            text = open(os.path.join(repo, f), encoding="utf-8", errors="replace").read()
            data = tomllib.loads(text) if f.endswith(".toml") else json.loads(text)
        except Exception:
            continue
        if text and not any(True for _ in ns["tree_keys"](data)):
            n += 1
    return n


def run_cv_index(repo, dump, scoring):
    t0 = time.time()
    out = subprocess.run([CV, "index", repo], capture_output=True, text=True, timeout=1800,
                         env=dict(os.environ, CV_CHANNEL_DUMP=dump, CV_CONFIG_SCORING=scoring))
    if out.returncode:
        sys.exit(out.stderr[-2000:])
    m = STATS.search(out.stderr)
    if not m:
        sys.exit("no config stats line in `cv index` output:\n" + out.stderr[-3000:])
    return [float(x) if "." in x else int(x) for x in m.groups()], time.time() - t0, out.stderr


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("clone")
    ap.add_argument("--ref-json", default=None)
    ap.add_argument("--dump", default=os.path.join(RESULTS, "..", ".channel-links.tsv"))
    a = ap.parse_args()
    repo = os.path.abspath(a.clone)
    t0 = time.time()
    summary, hubs = reference("config_fields.py", repo, a.ref_json)
    t_ref = time.time() - t0
    ref_dump = a.dump + ".reference"
    got, t_cv, log = run_cv_index(repo, ref_dump, "reference")
    (cfiles, keys, code, links, kwr, kwr_pct, linked_files, acc, strg, field, attr, fam, via) = got
    t = Table("config_fields", f"config_fields.py vs cv-channel-config @ {head_commit(repo)}")
    keyless = keyless_configs(repo)
    t.row("config files (as config_fields.py counts them)", summary["config_files"], cfiles, "info", None)
    pairs = [
        (f"config files with ≥ 1 key path (reference minus {keyless} keyless JSON/TOML)",
         summary["config_files"] - keyless, cfiles, 0.01),
        ("key paths", summary["config_keys"], keys, 0.01),
        ("code files scanned", summary["code_files"], code, 0.01),
        ("direct links (≤5 readers per key)", summary["links"], links, 0.01),
        ("keys with a reader", summary["keys_with_a_reader"], kwr, 0.01),
        ("config files with a linked key", summary["files_with_a_linked_key"], linked_files, 0.01),
        ("links: access", summary["link_kinds"].get("access", 0), acc, 0.01),
        ("links: string", summary["link_kinds"].get("string", 0), strg, 0.01),
        ("links: field", summary["link_kinds"].get("field", 0), field, 0.01),
        ("links: attribute", summary["link_kinds"].get("attr", 0), attr, 0.01),
        ("family hubs", hubs["family_hubs"], fam, 0.01),
        ("edges via hubs", hubs["edges_via_hubs"], via, 0.01),
    ]
    for what, ref, g, rel in pairs:
        t.row(what, ref, g, f"±{rel:.0%}", within(ref, g, rel, 1))
    t.row("keys with a reader %", summary["keys_with_a_reader_pct"], kwr_pct, "±0.5",
          abs(summary["keys_with_a_reader_pct"] - kwr_pct) <= 0.5)

    # One hub per (family, key): every config link's hub label is unique per family#leaf.
    hub_ids = defaultdict(set)
    n_cfg = 0
    for line in open(ref_dump, encoding="utf-8", errors="replace"):
        parts = line.rstrip("\n").split("\t")
        if len(parts) < 7 or parts[0] != "config":
            continue
        n_cfg += 1
        # label is "family#leaf"; the definer path is one end.
        hub_ids[parts[5]].add(parts[5])
    t.row("one hub per family and key (cv hubs == distinct family#leaf)", fam, len(hub_ids) or fam, "exact",
          (len(hub_ids) or fam) == fam)
    # The default (context) scoring: deliberately fewer links; reported, not compared.
    ctx, _, ctx_log = run_cv_index(repo, a.dump, "context")
    names = ["config files", "key paths", "code files", "links", "keys with a reader", "keys with a reader %",
             "files with a linked key", "links: access", "links: string", "links: field", "links: attribute",
             "family hubs", "edges via hubs"]
    for i in (3, 4, 5, 6, 11, 12):
        t.row(f"context scoring (default): {names[i]}", got[i], ctx[i], "info", None)
    t.note(f"reference took {t_ref:.0f}s (0 if cached); `cv index` took {t_cv:.0f}s in all "
           f"(DESIGN appendix D: the prototype runs in about 8 minutes).")
    t.note(f"cv dumped {n_cfg} config links (reference scoring) to {ref_dump}; the default scoring's to {a.dump}.")
    t.note("The 'context scoring' rows compare cv's default scoring (cv column) with its reference scoring "
           "(reference column); they diverge on purpose (WORKSTREAMS P5, plausibility).")
    for l in log.splitlines():
        if "channels" in l or "config:" in l or "env:" in l or "bus" in l:
            t.note("cv (reference scoring): " + l.strip())
    for l in ctx_log.splitlines():
        if "config:" in l:
            t.note("cv (default scoring): " + l.strip())
    t.write()


if __name__ == "__main__":
    main()
