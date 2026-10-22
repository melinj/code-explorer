#!/usr/bin/env bash
# Golden jj repos for tests and contract replay (WORKSTREAMS C0).
#
#   scripts/fixtures.sh            build fixtures/repos/* (deletes and rebuilds)
#   scripts/fixtures.sh --check    rebuild into a temp dir and compare with fixtures/repos.manifest
#   scripts/fixtures.sh --manifest rebuild and rewrite fixtures/repos.manifest
#   FIXTURES_DIR=/some/dir scripts/fixtures.sh   build somewhere else
#
# Deterministic: fixed user, timestamps and a per-command randomness seed, and a private jj config,
# so commit and change ids are identical on every machine with the same jj version (0.45.1).
#
# Repos:
#   linear      linear stack of 5 changes on `main`: a rename (C), a file split into 5 fragments
#               (src/shapes.py: 153 lines, 5 top-level 28-line functions), cross-file imports,
#               tests, config, infra and docs files for every lane; ranges: `B::D` (convex) and
#               `B | D` (non-convex) via the change descriptions.
#   conflict    base -> B -> C -> D, then an insert before C that edits the line C edits: C and D
#               become conflicted (the "insert conflicts with descendants" scenario).
#   workspaces  default, `ws2` and `agent` workspaces on a 2-change stack; default and ws2 hold
#               unsnapshotted edits (written after the last jj command, never snapshotted here).
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
MANIFEST="$HERE/fixtures/repos.manifest"
MODE="${1:-build}"

command -v jj >/dev/null || { echo "fixtures: jj not found" >&2; exit 1; }
JJ_VER="$(jj --version | awk '{print $2}')"

CONF_DIR="$(mktemp -d)"
trap 'rm -rf "$CONF_DIR"' EXIT
cat > "$CONF_DIR/config.toml" <<'TOML'
[user]
name = "Fixture Author"
email = "fixture@example.com"
[ui]
color = "never"
paginate = "never"
[git]
colocate = false
[snapshot]
auto-track = "all()"
TOML

export JJ_CONFIG="$CONF_DIR/config.toml"
export JJ_USER="Fixture Author" JJ_EMAIL="fixture@example.com"
export JJ_OP_HOSTNAME="fixtures" JJ_OP_USERNAME="fixtures"
export TZ=UTC
SEED=1000
TICK=0
# Every jj command gets its own seed and a timestamp one minute later than the previous one.
j() {
  SEED=$((SEED + 1)); TICK=$((TICK + 1))
  local ts; ts="$(date -u -d "@$((1767225600 + TICK * 60))" +%Y-%m-%dT%H:%M:%S+00:00)"
  JJ_RANDOMNESS_SEED=$SEED JJ_TIMESTAMP="$ts" JJ_OP_TIMESTAMP="$ts" jj "$@" >/dev/null 2>&1 \
    || { echo "fixtures: jj $* failed" >&2; JJ_RANDOMNESS_SEED=$SEED JJ_TIMESTAMP="$ts" JJ_OP_TIMESTAMP="$ts" jj "$@"; exit 1; }
}
desc() { j describe -m "$1"; }

shape_block() { # $1 name: a 28-line top-level function
  printf 'def %s(size: float) -> dict:\n' "$1"
  printf '    """Shape %s."""\n' "$1"
  for i in $(seq 1 24); do printf '    v%d = size * %d  # %s step %d\n' "$i" "$i" "$1" "$i"; done
  printf '    return {"name": "%s", "size": size, "area": v24}\n\n\n' "$1"
}

build_linear() {
  local r="$1/linear"; mkdir -p "$r"; cd "$r"
  j git init --no-colocate
  mkdir -p src tests docs config .github/workflows
  printf 'import os\n\nDEFAULT_TOPIC = "world.entity.moved"\nDB_URL = os.getenv("DB_URL", "sqlite://")\n\n\ndef load(path: str) -> str:\n    with open(path) as f:\n        return f.read()\n' > src/util.py
  printf '# Fixture\n\nA tiny repo for code-viewer tests.\n' > README.md
  printf 'name: fixture\nretries: 3\nfeatures:\n  fast: true\n' > config/app.yaml
  printf 'DB_URL=sqlite://\n' > .env.example
  desc "A: util and config"; j bookmark create main -r @
  j new
  { printf 'from src.util import load, DEFAULT_TOPIC\n\n\n'
    for n in circle square triangle hexagon star; do shape_block "$n"; done; } > src/shapes.py
  printf 'from src.shapes import circle\n\n\ndef test_circle():\n    assert circle(1.0)["name"] == "circle"\n' > tests/test_shapes.py
  desc "B: shapes split into five fragments"; j bookmark set main -r @
  j new
  mv src/util.py src/helpers.py
  sed -i 's/from src.util import/from src.helpers import/' src/shapes.py
  printf 'FROM python:3.12-slim AS base\nENV DB_URL=sqlite://\nCOPY src /app/src\nCMD ["python", "-m", "src.shapes"]\n' > Dockerfile
  desc "C: rename util.py to helpers.py"; j bookmark set main -r @
  j new
  sed -i 's/v8 = size \* 8  # square step 8/v8 = size * 80  # square step 8 (tuned)/' src/shapes.py
  printf 'name: ci\non: [push]\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - run: pytest tests\n' > .github/workflows/ci.yml
  printf '# Design\n\nShapes are dicts.\n' > docs/design.md
  desc "D: tune square, add CI and docs"; j bookmark set main -r @
  j new
  sed -i 's/retries: 3/retries: 5/' config/app.yaml
  printf '\n\ndef save(path: str, text: str) -> None:\n    with open(path, "w") as f:\n        f.write(text)\n' >> src/helpers.py
  desc "E: more retries, save helper"; j bookmark set main -r @
  j new
}

build_conflict() {
  local r="$1/conflict"; mkdir -p "$r"; cd "$r"
  j git init --no-colocate
  printf 'def speed():\n    return 1\n\n\ndef name():\n    return "base"\n' > engine.py
  desc "base"; j new
  printf 'NOTES = "b"\n' > notes.py
  desc "B: notes"; j new
  sed -i 's/return 1/return 2/' engine.py
  desc "C: speed 2"; j new
  sed -i 's/return "base"/return "d"/' engine.py
  desc "D: rename engine"; j bookmark create main -r @
  # Insert before C an edit of the same line C edits: C and D become conflicted.
  j new --insert-before 'description(substring:"C: speed 2")'
  sed -i 's/return 1/return 3/' engine.py
  desc "X: inserted speed 3"
  j new main
}

build_workspaces() {
  local r="$1/workspaces"; mkdir -p "$r/default"; cd "$r/default"
  j git init --no-colocate
  printf 'def greet(who):\n    return f"hello {who}"\n' > app.py
  printf 'mode: dev\n' > settings.yaml
  desc "base: app and settings"; j bookmark create main -r @
  j new
  printf '\n\ndef bye(who):\n    return f"bye {who}"\n' >> app.py
  desc "feature: bye"; j bookmark set main -r @
  j new
  j workspace add --name ws2 ../ws2 -r 'main'
  j workspace add --name agent ../agent -r 'main'
  # Unsnapshotted edits: written after the last jj command in each workspace.
  printf 'mode: prod\n' > "$r/default/settings.yaml"
  printf '\n\n# ws2 local edit\n' >> "$r/ws2/app.py"
}

ids() { # one line per commit, all workspaces' heads included; never snapshots
  local repo="$1"
  ( cd "$repo" && jj --ignore-working-copy --at-op @ log -r 'all()' --no-graph \
      -T 'change_id.short(12) ++ " " ++ commit_id.short(12) ++ " " ++ if(conflict, "conflict ", "") ++ working_copies ++ " " ++ description.first_line() ++ "\n"' )
}

build_all() {
  local out="$1"
  rm -rf "$out"; mkdir -p "$out"
  ( build_linear "$out" )
  ( SEED=2000; TICK=100; build_conflict "$out" )
  ( SEED=3000; TICK=200; build_workspaces "$out" )
}

manifest() {
  local out="$1"
  echo "# jj $JJ_VER — generated by scripts/fixtures.sh --manifest; change commit [conflict] workspaces description"
  for r in linear conflict workspaces/default; do
    echo "## $r"; ids "$out/$r"
  done
}

case "$MODE" in
  build|--build)
    OUT="${FIXTURES_DIR:-$HERE/fixtures/repos}"
    build_all "$OUT"; echo "fixtures: built $OUT (linear, conflict, workspaces/{default,ws2,agent})";;
  --manifest)
    OUT="${FIXTURES_DIR:-$HERE/fixtures/repos}"
    build_all "$OUT"; manifest "$OUT" > "$MANIFEST"; echo "fixtures: wrote $MANIFEST";;
  --check)
    TMP="$(mktemp -d)"; trap 'rm -rf "$CONF_DIR" "$TMP"' EXIT
    build_all "$TMP/repos"
    # The ids are what must match; the header only records which jj wrote the manifest.
    if diff -u <(grep -v '^# jj ' "$MANIFEST") <(manifest "$TMP/repos" | grep -v '^# jj '); then
      echo "fixtures: manifest matches (jj $JJ_VER)"; else
      echo "fixtures: ids differ from $MANIFEST (jj version or script changed? rerun with --manifest)" >&2; exit 1; fi;;
  *) sed -n 2,8p "$0"; exit 2;;
esac
