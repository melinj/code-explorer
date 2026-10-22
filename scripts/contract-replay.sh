#!/usr/bin/env bash
# Contract replay (WORKSTREAMS C0): recorded request/response pairs against cv serve and cv-mock,
# type-checked against docs/contracts/http.ts.
#
#   scripts/contract-replay.sh                 check the committed recordings (offline, no servers)
#   scripts/contract-replay.sh --record [serve|mock|all]
#                                              rebuild fixtures, start the server(s) on 7991/7992,
#                                              re-record fixtures/replay/<target>/, then check
#   (serve also records serve-resync: the linear repo's event log aged past retention, so an old
#   cursor gets `resync: true` from GET /events and the WS hello)
# Known contract drift is listed in fixtures/replay/known-divergences.txt (one line each, with a reason).
set -euo pipefail
HERE="$(cd "$(dirname "$0")/.." && pwd)"
RECORD=0
if [[ "${1:-}" == "--record" ]]; then RECORD=1; shift; fi
WHAT="${1:-all}"
SERVE_PORT="${REPLAY_SERVE_PORT:-7991}"
MOCK_PORT="${REPLAY_MOCK_PORT:-7992}"

if [[ ! -d "$HERE/web/node_modules/typescript" ]]; then
  echo "contract-replay: web/node_modules missing (run npm ci in web/)" >&2; exit 1
fi

TMP="$(mktemp -d)"
PIDS=()
cleanup() { for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null || true; done; rm -rf "$TMP"; }
trap cleanup EXIT

wait_up() { # url
  for _ in $(seq 1 600); do curl -fsS -o /dev/null "$1" 2>/dev/null && return 0; sleep 0.2; done
  echo "contract-replay: $1 did not come up" >&2; return 1
}

record_serve() {
  local tmp="$TMP"
  FIXTURES_DIR="$tmp/repos" "$HERE/scripts/fixtures.sh" >/dev/null
  ( cd "$HERE" && timeout 900 cargo build -q -p cv-server --bin cv )
  for scen in linear conflict workspaces; do
    local dir="serve" repo="$tmp/repos/$scen"
    [[ $scen != linear ]] && dir="serve-$scen"
    [[ $scen == workspaces ]] && repo="$tmp/repos/workspaces/default"
    XDG_DATA_HOME="$tmp/data-$scen" XDG_CACHE_HOME="$tmp/cache-$scen" \
      timeout 600 "$HERE/target/debug/cv" serve "$repo" --port "$SERVE_PORT" --no-auth --no-ui \
      >"$tmp/serve-$scen.log" 2>&1 &
    PIDS+=($!)
    wait_up "http://127.0.0.1:$SERVE_PORT/api/v1/repo" || { cat "$tmp/serve-$scen.log" >&2; return 1; }
    timeout 300 python3 -I "$HERE/scripts/replay/record.py" "http://127.0.0.1:$SERVE_PORT" \
      "$HERE/fixtures/replay/$dir" --scenario "$scen"
    # Recordings must not show where the temp fixture lived.
    sed -i "s#$tmp/repos#<FIXTURES>#g" "$HERE"/fixtures/replay/$dir/*.json
    kill "${PIDS[-1]}"; wait "${PIDS[-1]}" 2>/dev/null || true
  done
  record_resync
}

# Ages the linear repo's event log past retention (DESIGN §6: beyond the newest 1,000 events, nothing
# older than 14 days is kept), restarts cv serve (it prunes at startup) and records the resync answers.
record_resync() {
  local tmp="$TMP" db
  db="$(find "$tmp/data-linear" -name review.db | head -1)"
  [[ -n "$db" ]] || { echo "contract-replay: no review.db under $tmp/data-linear" >&2; return 1; }
  python3 -I - "$db" <<'PY'
import sqlite3, sys
c = sqlite3.connect(sys.argv[1])
c.execute("UPDATE event_log SET at = '2000-01-01T00:00:00Z'")
row = c.execute("SELECT type, payload_json FROM event_log ORDER BY seq DESC LIMIT 1").fetchone()
c.executemany("INSERT INTO event_log (at, type, payload_json) VALUES (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), ?, ?)",
              [row] * 1000)
c.commit()
PY
  XDG_DATA_HOME="$tmp/data-linear" XDG_CACHE_HOME="$tmp/cache-linear" \
    timeout 600 "$HERE/target/debug/cv" serve "$tmp/repos/linear" --port "$SERVE_PORT" --no-auth --no-ui \
    >"$tmp/serve-resync.log" 2>&1 &
  PIDS+=($!)
  wait_up "http://127.0.0.1:$SERVE_PORT/api/v1/repo" || { cat "$tmp/serve-resync.log" >&2; return 1; }
  timeout 300 python3 -I "$HERE/scripts/replay/record.py" "http://127.0.0.1:$SERVE_PORT" \
    "$HERE/fixtures/replay/serve-resync" --scenario resync
  sed -i "s#$tmp/repos#<FIXTURES>#g" "$HERE"/fixtures/replay/serve-resync/*.json
  kill "${PIDS[-1]}"; wait "${PIDS[-1]}" 2>/dev/null || true
}

record_mock() {
  local tmp="$TMP"
  ( cd "$HERE" && timeout 900 cargo build -q -p cv-mock )
  timeout 600 "$HERE/target/debug/cv-mock" --port "$MOCK_PORT" --windows 2000 --latency-ms 0 --no-sim-agent \
    >"$tmp/mock.log" 2>&1 &
  PIDS+=($!)
  wait_up "http://127.0.0.1:$MOCK_PORT/api/v1/repo" || { cat "$tmp/mock.log" >&2; return 1; }
  timeout 300 python3 -I "$HERE/scripts/replay/record.py" "http://127.0.0.1:$MOCK_PORT" \
    "$HERE/fixtures/replay/mock" --scenario mock
  kill "${PIDS[-1]}"; wait "${PIDS[-1]}" 2>/dev/null || true
}

if (( RECORD )); then
  case "$WHAT" in
    serve) record_serve ;;
    mock) record_mock ;;
    all) record_serve; record_mock ;;
    *) echo "usage: $0 [--record] [serve|mock|all]" >&2; exit 2 ;;
  esac
fi
timeout 300 node "$HERE/scripts/replay/check.mjs" "$WHAT"
