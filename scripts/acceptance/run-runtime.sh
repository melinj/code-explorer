#!/usr/bin/env bash
# Runtime acceptance on a throwaway clone of a repo: DESIGN §9 text latency (L3 cached/cold),
# canvas repaint after a jj op, and the B9 MCP apply_suggestion run with its WS events.
#
# Usage: scripts/acceptance/run-runtime.sh <source repo> <work dir> [port=7993]
#   The source repo is only read (cloned into <work dir>/repo).
#   CV=path/to/cv (default target/release/cv; built when missing), UI=web/dist (built when missing)
#   CHROME=/usr/bin/google-chrome-stable; LAT_TRIALS=15, REPAINT_TRIALS=10
# Results: scripts/acceptance/results/{text-latency,repaint-after-op,mcp-apply-suggestion}.json
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
src="${1:?usage: run-runtime.sh <source repo> <work dir> [port]}"
work="${2:?usage: run-runtime.sh <source repo> <work dir> [port]}"
port="${3:-7993}"
cv="${CV:-$root/target/release/cv}"
ui="${UI:-$root/web/dist}"
res="$here/results"
mkdir -p "$work" "$res"

[ -x "$cv" ] || (cd "$root" && timeout 1800 cargo build --release -p cv-server)
[ -f "$ui/index.html" ] || (cd "$root/web" && timeout 600 npm run build)

repo="$work/repo"
rm -rf "$repo"
timeout 300 jj git clone "$src" "$repo" >/dev/null 2>&1
(cd "$repo" && jj new 'trunk()' >/dev/null 2>&1 && jj describe -m "acceptance: wip" >/dev/null 2>&1)

token="acc$(date +%s)"
log="$work/serve.log"
CV_TOKEN="$token" "$cv" serve "$repo" --port "$port" --ui "$ui" >"$log" 2>&1 &
pid=$!
trap 'kill $pid 2>/dev/null || true; wait $pid 2>/dev/null || true' EXIT
for _ in $(seq 1 300); do grep -q 'UI:' "$log" && break; sleep 1; done
grep -q 'UI:' "$log" || { cat "$log"; exit 1; }
url="http://127.0.0.1:$port/?token=$token"

status=0
echo "== text latency (L3; fresh server: first trials are server-cold too)"
timeout 900 node "$here/text-latency.mjs" "$url" "${LAT_TRIALS:-15}" "$res/text-latency.json" | tail -40 || status=1
echo "== repaint after a jj op"
timeout 900 node "$here/repaint-after-op.mjs" "$url" "$repo" "${REPAINT_TRIALS:-10}" "$res/repaint-after-op.json" | tail -40 || status=1
echo "== MCP apply_suggestion (B9)"
timeout 300 node "$here/mcp-apply-suggestion.mjs" "http://127.0.0.1:$port" "$token" "$repo" --out "$res/mcp-apply-suggestion.json" || status=1
exit $status
