#!/usr/bin/env bash
# The CI gate, runnable locally: `scripts/ci.sh`. GitHub Actions runs the same script.
#
# Steps (each can be skipped with an env var set to 1):
#   fmt       cargo fmt --all --check                               CI_SKIP_FMT
#   clippy    cargo clippy --workspace --all-targets -D warnings    CI_SKIP_CLIPPY
#   test      cargo test --workspace                                CI_SKIP_TEST
#             (includes the cv-contracts golden test: generated TS == committed TS;
#             scripts/gen-contracts.sh regenerates it)
#   core-deps scripts/check-core-deps.sh (D13)                      CI_SKIP_DEPS
#   web       npm ci (if node_modules is missing), tsc --noEmit,    CI_SKIP_WEB
#             vitest, vite build
#   fixtures  scripts/fixtures.sh --check (golden jj repos rebuild  CI_SKIP_FIXTURES
#             to the ids in fixtures/repos.manifest; needs jj)
#   replay    scripts/contract-replay.sh: recorded serve/mock       CI_SKIP_REPLAY
#             responses type-check against docs/contracts (needs web/node_modules)
# CI_TIMEOUT (seconds, default 1800) bounds each step.
# The UI end-to-end gate (headless Chrome, cv-mock and cv serve on a fixture repo) is a separate
# script and CI job: scripts/ci-e2e.sh.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/env.sh # the shared compile cache (sccache) when installed
T="${CI_TIMEOUT:-1800}"
failed=()
# The jj-backed tests skip without jj; here they must run (have_jj() fails under CV_REQUIRE_JJ).
command -v jj >/dev/null || { echo "CI FAILED: jj is not on PATH (the repo tests need it)"; exit 1; }
export CV_REQUIRE_JJ=1

step() {
  local name="$1" skip="$2"; shift 2
  if [[ "${!skip:-0}" == "1" ]]; then echo "== $name: skipped ($skip=1)"; return; fi
  echo "== $name: $*"
  local t0=$SECONDS
  if timeout "$T" "$@"; then
    echo "== $name: ok ($((SECONDS - t0)) s)"
  else
    echo "== $name: FAILED ($((SECONDS - t0)) s)"
    failed+=("$name")
  fi
}

step fmt CI_SKIP_FMT cargo fmt --all --check
step clippy CI_SKIP_CLIPPY cargo clippy --workspace --all-targets -- -D warnings
step test CI_SKIP_TEST cargo test --workspace
step core-deps CI_SKIP_DEPS scripts/check-core-deps.sh

web() {
  cd web
  if [[ ! -e node_modules ]]; then npm ci --no-fund --no-audit; fi
  npx tsc --noEmit
  npx vitest run --passWithNoTests
  npx vite build --logLevel warn
}
step web CI_SKIP_WEB bash -euo pipefail -c "$(declare -f web); web"
step fixtures CI_SKIP_FIXTURES scripts/fixtures.sh --check
step replay CI_SKIP_REPLAY scripts/contract-replay.sh

if ((${#failed[@]})); then
  echo "CI FAILED: ${failed[*]}"
  exit 1
fi
echo "CI OK"
