#!/usr/bin/env python3
"""Seed jj conflicts in a throwaway clone so review-flow can exercise conflict resolution on cv serve.

usage: seed-conflicts.py REPO [N]

For the first N files (default 2) that @ modifies, inserts an "upstream" change between @- and @ that
rewrites the first line @ changed, differently, then rebases @ onto it: @ ends up conflicted in each.
Never run this on a repo you care about.
"""
import re
import subprocess
import sys

repo = sys.argv[1]
n = int(sys.argv[2]) if len(sys.argv) > 2 else 2


def jj(*args):
    return subprocess.run(["jj", "-R", repo, *args], check=True, capture_output=True, text=True).stdout


me = jj("log", "-r", "@", "--no-graph", "-T", "change_id.short()").strip()
diff = jj("diff", "-r", "@", "--git")
# First removed line per file: the old line number in the parent and its text.
targets = []
path = None
old = 0
for line in diff.splitlines():
    if line.startswith("diff --git"):
        path = line.split(" b/", 1)[1]
        old = 0
    elif m := re.match(r"@@ -(\d+)", line):
        old = int(m.group(1))
    elif path and old and line.startswith("-") and not line.startswith("---"):
        targets.append((path, old))
        path = None
    elif path and old and not line.startswith("+"):
        old += 1
targets = targets[:n]
if not targets:
    sys.exit("@ removes no lines: nothing to conflict with")

jj("new", "@-", "-m", "upstream: concurrent edit (seeded conflict)")
for path, lineno in targets:
    p = f"{repo}/{path}"
    lines = open(p).read().split("\n")
    indent = re.match(r"\s*", lines[lineno - 1]).group(0)
    lines[lineno - 1] = f"{indent}# upstream rewrote this line"
    open(p, "w").write("\n".join(lines))
    print(f"seeded {path}:{lineno}")
up = jj("log", "-r", "@", "--no-graph", "-T", "change_id.short()").strip()
jj("rebase", "-r", me, "-d", up)
jj("edit", me)
print(jj("log", "-r", "conflicts()", "--no-graph", "-T", 'change_id.short() ++ " conflicted\\n"'), end="")
