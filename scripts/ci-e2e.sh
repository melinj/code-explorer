#!/usr/bin/env bash
# End-to-end UI gate, runnable locally: `scripts/ci-e2e.sh`. GitHub Actions runs it as the e2e job.
#
# 1. builds cv and cv-mock (release) and the web UI (vite build into web/dist);
# 2. builds the golden fixture repos (scripts/fixtures.sh) into a temp dir and turns a copy of
#    `linear` into the e2e repo: a few more modules (bus topics, calls, a runtime topic) and one
#    conflict, so every check has something real to look at (no private repo needed);
# 3. cv-mock + `vite preview`: panels-flow, then (on a fresh mock) review-flow in the light theme,
#    then (fresh again) a soak that fails when the retained heap grows by ≥ 9 MB/min after its warm-up
#    (a 20 MB/min leak fails it; E2E_SOAK_SECONDS, default 390);
# 4. `cv serve <e2e repo> --ui web/dist`: panels-flow (light theme) with REPO set (stale workspace, agent
#    attached, blocked undo, not-a-chain range, overlapping stacks, a whole-view reindex… made for
#    real with jj), then review-flow on two fresh conflicts with scripts/scripted-agent.mjs attached
#    (a scripted MCP agent, no model) to answer "Ask agent to resolve" and a comment (with a
#    multi-patch code change; the thread must stay addressed through later jj ops); NO_SIM_AGENT=1
#    for both. review-flow runs with REQUIRE: a check it has to skip fails the gate. Then
#    views-flow (also run on cv-mock after panels-flow).
#
# Env: CHROME (browser binary), CHROME_ARGS (GPU flags; default SwiftShader when CI is set),
#      E2E_MOCK_PORT (8085), E2E_SERVE_PORT (8086), E2E_PREVIEW_PORT (5225), E2E_OUT (screenshots and
#      logs; default a temp dir, kept when something fails), E2E_SKIP_BUILD=1 (reuse the binaries
#      and web/dist), E2E_BIN (where cv and cv-mock are; default target/release),
#      E2E_ONLY=mock|serve (run one half), CI_SOAK=0 (skip the soak while iterating; default on),
#      E2E_FLOW_TIMEOUT (seconds one flow may take before it is killed and fails; default 1500).
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/env.sh # the shared compile cache (sccache) when installed
ROOT="$PWD"
MOCK_PORT="${E2E_MOCK_PORT:-8085}"
SERVE_PORT="${E2E_SERVE_PORT:-8086}"
PREVIEW_PORT="${E2E_PREVIEW_PORT:-5225}"
TMP="$(mktemp -d)"
OUT="${E2E_OUT:-$TMP/out}"
mkdir -p "$OUT"
pids=()
KEEP=0
cleanup() {
  for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
  if ((KEEP)) && [[ -z "${E2E_OUT:-}" ]]; then echo "ci-e2e: kept $TMP"; else rm -rf "$TMP"; fi
}
trap cleanup EXIT

if [[ -z "${CHROME:-}" ]]; then
  CHROME="$(command -v google-chrome-stable || command -v google-chrome || command -v chromium || command -v chromium-browser || true)"
fi
[[ -n "$CHROME" ]] || { echo "ci-e2e: no Chrome found (set CHROME)" >&2; exit 1; }
export CHROME
if [[ -z "${CHROME_ARGS:-}" && -n "${CI:-}" ]]; then
  export CHROME_ARGS="--use-angle=swiftshader --enable-unsafe-swiftshader --ignore-gpu-blocklist"
fi

if [[ "${E2E_SKIP_BUILD:-0}" != "1" ]]; then
  echo "== build: cv, cv-mock, web"
  cargo build --release -p cv-server -p cv-mock
  ( cd web && { [[ -e node_modules ]] || npm ci --no-fund --no-audit; } && npx tsc --noEmit && npx vite build --logLevel warn )
fi
BIN="${E2E_BIN:-$ROOT/target/release}"

wait_http() { # url [seconds]
  local t=$((SECONDS + ${2:-120}))
  until curl -fsS -o /dev/null "$1" 2>/dev/null; do
    ((SECONDS < t)) || { echo "ci-e2e: $1 did not come up" >&2; return 1; }
    sleep 1
  done
}

failed=()
run() { # name cmd…: runs cmd in web/, records a failure (exit code or a FAIL line), keeps going
  local name="$1"; shift
  echo "== $name"
  local t0=$SECONDS
  # A hung flow fails (and is killed) instead of holding the job until its own timeout.
  if (cd "$ROOT/web" && timeout -k 10 "${E2E_FLOW_TIMEOUT:-1500}" "$@") 2>&1 | tee "$OUT/$name.log"; then
    if grep -q '^FAIL' "$OUT/$name.log"; then failed+=("$name"); fi
  else failed+=("$name"); fi
  echo "== $name: $((SECONDS - t0)) s (load $(cut -d' ' -f1-3 /proc/loadavg))"
}

start_mock() {
  "$BIN/cv-mock" --port "$MOCK_PORT" > "$OUT/cv-mock.log" 2>&1 &
  MOCK_PID=$!
  pids+=("$MOCK_PID")
  wait_http "http://127.0.0.1:$MOCK_PORT/api/v1/repo" 60
}

if [[ "${E2E_ONLY:-}" != "serve" ]]; then
  echo "== cv-mock on :$MOCK_PORT, vite preview on :$PREVIEW_PORT"
  start_mock
  ( cd web && CV_API="http://127.0.0.1:$MOCK_PORT" exec npx vite preview --host 127.0.0.1 --port "$PREVIEW_PORT" --strictPort ) > "$OUT/preview.log" 2>&1 &
  pids+=("$!")
  wait_http "http://127.0.0.1:$PREVIEW_PORT/" 60
  run mock-panels-flow env URL="http://127.0.0.1:$PREVIEW_PORT" node scripts/panels-flow.mjs "$OUT/mock-panels"
  # Views, sessions and navigation (two tabs, revision chip, Open @, a lost view).
  run mock-views-flow env URL="http://127.0.0.1:$PREVIEW_PORT" node scripts/views-flow.mjs "$OUT/mock-views"
  # panels-flow binds an agent at the end, which stands cv-mock's simulated agent down: fresh mock.
  kill "$MOCK_PID"; wait "$MOCK_PID" 2>/dev/null || true
  start_mock
  # The light theme runs here and in serve-panels-flow (the other flows run dark).
  # REQUIRE: these checks must run (a skip is a failure). cv-mock answers conflicts with its simulated
  # agent; comments answered with code need a real MCP agent (the cv serve half).
  run mock-review-flow env URL="http://127.0.0.1:$PREVIEW_PORT" LIGHT=1 REQUIRE="${E2E_MOCK_REQUIRE:-conflicts,ask-agent,agent-resolution,fade}" node scripts/review-flow.mjs "$OUT/mock-review"
  # A short soak on a fresh mock: the heap stays bounded while panning across the whole map
  # (soak.mjs fails on a rising retained heap or a peak over its bound). CI_SOAK=0 skips it.
  if [[ "${CI_SOAK:-1}" != "0" ]]; then
    kill "$MOCK_PID"; wait "$MOCK_PID" 2>/dev/null || true
    start_mock
    # The retained heap fills its caches for the first two minutes or so on cv-mock's 20k windows (~230 →
    # ~280 MB), then swings ±15 MB with the walk (period ~4 min) around 250-280 MB. So the slope is fitted
    # only after a 120 s warm-up, over 270 s, from a sample every zoom round (--gc-every 5: ~33 samples).
    # Measured 2026-10-19: with no leak the slope of any ≥ 240 s window starting at ≥ 90 s was
    # ≤ 4.7 MB/min (2 runs of 600 s; 3 runs of this exact config: -3.9, -3.5, -2.6); with an injected
    # 20 MB/min leak (soak.mjs --leak 20, 20.0 MB/min on a blank page) 15.2-22 MB/min, and 15.9 in this
    # config (a 10 MB/min leak gave 7.0: below the bound). --max-slope 9 sits between the two.
    run mock-soak node scripts/soak.mjs --url "http://127.0.0.1:$PREVIEW_PORT" --seconds "${E2E_SOAK_SECONDS:-390}" --warmup 120 --gc-every 5 --max-slope 9 --max-retained 380
  else echo "== mock-soak: skipped (CI_SOAK=0)"; fi
fi

if [[ "${E2E_ONLY:-}" != "mock" ]]; then
  echo "== fixture repo"
  FIXTURES_DIR="$TMP/fixtures" scripts/fixtures.sh
  REPO="$TMP/e2e"
  cp -r "$TMP/fixtures/linear" "$REPO"
  export JJ_USER="E2E" JJ_EMAIL="e2e@example.invalid"
  jj() { command jj -R "$REPO" --no-pager --color=never "$@" >/dev/null 2>&1; }
  # More code to look at: a bus with producers and consumers, calls between modules, and a topic
  # only known at runtime (a dynamic link, "⇢ ?").
  mkdir -p "$REPO/src/bus"
  cat > "$REPO/src/bus/events.py" <<'PY'
ENTITY_MOVED = "world.entity.moved"
ENTITY_SPAWNED = "world.entity.spawned"


class Bus:
    def __init__(self):
        self.handlers = {}

    def on(self, topic, handler):
        self.handlers.setdefault(topic, []).append(handler)

    def emit(self, topic, payload):
        for h in self.handlers.get(topic, []):
            h(payload)
PY
  cat > "$REPO/src/bus/world.py" <<'PY'
from src.bus.events import ENTITY_MOVED, ENTITY_SPAWNED, Bus
from src.shapes import circle, square


def move(bus: Bus, entity: str, to: tuple) -> None:
    bus.emit(ENTITY_MOVED, {"entity": entity, "to": to})


def spawn(bus: Bus, entity: str) -> dict:
    shape = circle(1.0) if entity.startswith("c") else square(1.0)
    bus.emit(ENTITY_SPAWNED, {"entity": entity, "shape": shape})
    return shape


def announce(bus: Bus, kind: str, payload: dict) -> None:
    topic = make_topic(kind)
    bus.emit(topic, payload)


def make_topic(kind: str) -> str:
    return "world." + kind
PY
  cat > "$REPO/src/bus/watch.py" <<'PY'
from src.bus.events import ENTITY_MOVED, ENTITY_SPAWNED, Bus
from src.bus.world import move, spawn


def watch(bus: Bus, log: list) -> None:
    bus.on(ENTITY_MOVED, lambda p: log.append(("moved", p["entity"])))
    bus.on(ENTITY_SPAWNED, lambda p: log.append(("spawned", p["entity"])))


def demo() -> list:
    bus = Bus()
    log: list = []
    watch(bus, log)
    spawn(bus, "c1")
    move(bus, "c1", (1, 2))
    return log
PY
  cat > "$REPO/tests/test_bus.py" <<'PY'
from src.bus.watch import demo


def test_demo():
    assert demo() == [("spawned", "c1"), ("moved", "c1")]
PY
  jj describe -m "F: bus, world events and watchers"
  # Files that change together without a static link (the co-change lens has pairs to draw): with
  # A and E, README.md and config/app.yaml share 3 commits (CoChangeLimits: support ≥ 3).
  for i in 1 2; do
    jj new
    printf '\nrelease %s: retries tuned\n' "$i" >> "$REPO/README.md"
    sed -i "s/^name: fixture.*/name: fixture  # release $i/" "$REPO/config/app.yaml"
    jj describe -m "G$i: release notes and config"
  done
  top="$(command jj -R "$REPO" --no-pager log -r @ --no-graph -T 'change_id')"
  # One conflict: insert before D an edit of the line D edits (as fixtures.sh builds `conflict`);
  # D and everything above it become conflicted. Then back to the top of the stack.
  jj new --insert-before 'description(substring:"D: tune square")'
  sed -i 's/v8 = size \* 8  # square step 8/v8 = size * 81  # square step 8 (e2e)/' "$REPO/src/shapes.py"
  jj describe -m "X: inserted square step"
  jj new "$top"
  command jj -R "$REPO" --no-pager log -r 'conflicts()' --no-graph -T 'change_id.short() ++ " " ++ description.first_line() ++ "\n"' | sed 's/^/   conflicted: /'

  echo "== cv serve on :$SERVE_PORT"
  "$BIN/cv" serve "$REPO" --port "$SERVE_PORT" --ui "$ROOT/web/dist" > "$OUT/cv-serve.log" 2>&1 &
  pids+=("$!")
  t=$((SECONDS + 180))
  until grep -q 'UI:' "$OUT/cv-serve.log" 2>/dev/null; do
    ((SECONDS < t)) || { echo "ci-e2e: cv serve did not start" >&2; tail -n 30 "$OUT/cv-serve.log" >&2; exit 1; }
    sleep 1
  done
  URL="$(grep -o 'http://127.0.0.1:[0-9]*/?token=[0-9a-f]*' "$OUT/cv-serve.log" | head -1)"
  # In the light theme (with mock-review-flow): the canvas, panels and checks of a second flow.
  run serve-panels-flow env URL="$URL" REPO="$REPO" NO_SIM_AGENT=1 LIGHT=1 node scripts/panels-flow.mjs "$OUT/serve-panels"
  # Two fresh conflicts for review-flow (one resolved by taking a side, one by the agent): @ edits two
  # files, then seed-conflicts.py puts a conflicting upstream change under it.
  command jj -R "$REPO" --no-pager new >/dev/null 2>&1
  sed -i 's/    bus.emit(ENTITY_MOVED, {"entity": entity, "to": to})/    bus.emit(ENTITY_MOVED, {"entity": entity, "to": to, "e2e": 1})/' "$REPO/src/bus/world.py"
  sed -i 's/    bus = Bus()/    bus = Bus()  # e2e/' "$REPO/src/bus/watch.py"
  command jj -R "$REPO" --no-pager describe -m "e2e: two edits to conflict with" >/dev/null 2>&1
  python3 web/scripts/seed-conflicts.py "$REPO" 2 | sed 's/^/   /'
  # A scripted agent (scripts/scripted-agent.mjs: no model, the MCP tools over HTTP) is attached, so
  # "Ask agent to resolve" is answered for real, and so is a comment sent to the agent: it answers
  # with one multi-patch edit_code {thread, patches, reply}; review-flow checks the reply, the new
  # code and the status show live, then makes jj ops in REPO and checks the thread stays addressed.
  TOKEN="${URL##*token=}"
  ( cd web && exec node scripts/scripted-agent.mjs --server "http://127.0.0.1:$SERVE_PORT" --token "$TOKEN" --seconds 900 ) > "$OUT/scripted-agent.log" 2>&1 &
  AGENT_PID=$!
  pids+=("$AGENT_PID")
  run serve-review-flow env URL="$URL" NO_SIM_AGENT=1 AGENT=scripted REPO="$REPO" REQUIRE="${E2E_SERVE_REQUIRE:-conflicts,ask-agent,agent-resolution,agent-answer,fade}" node scripts/review-flow.mjs "$OUT/serve-review"
  kill "$AGENT_PID" 2>/dev/null || true
  sed 's/^/   /' "$OUT/scripted-agent.log"
  # Last (it files a thread of its own): snapshots at other revisions and workspaces, made with jj.
  run serve-views-flow env URL="$URL" REPO="$REPO" node scripts/views-flow.mjs "$OUT/serve-views"
fi

if ((${#failed[@]})); then
  echo "E2E FAILED: ${failed[*]} (logs and screenshots in $OUT)"
  KEEP=1
  exit 1
fi
echo "E2E OK ($SECONDS s)"
