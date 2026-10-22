#!/bin/sh
# Regenerate the TypeScript wire types from crates/cv-contracts (DESIGN D3) into
# docs/contracts/generated/ and web/src/contracts/generated/, then type-check them against the
# hand-written spec (docs/contracts/*.ts) through web/src/contracts/generated/check.ts; the MCP tool
# input schemas (crates/cv-mcp) into generated/mcp/ and compare them with mcp.ts McpTools[*].in;
# and check that the web app's hand copies web/src/contracts/*.ts equal docs/contracts/*.ts.
set -eu
cd "$(dirname "$0")/.."
CV_GEN_CONTRACTS=1 cargo test -q -p cv-contracts --test generated_ts generated_ts_is_current
CV_GEN_CONTRACTS=1 cargo test -q -p cv-mcp --test it mcp_inputs_ts_is_current
# The web app's hand copies of the spec must be identical to docs/contracts (also a cargo test).
for f in docs/contracts/*.ts; do
  cmp -s "$f" "web/src/contracts/$(basename "$f")" || { echo "contracts: web/src/contracts/$(basename "$f") differs from $f" >&2; exit 1; }
done
for f in web/src/contracts/*.ts; do
  [ -e "docs/contracts/$(basename "$f")" ] || { echo "contracts: $f has no docs/contracts counterpart" >&2; exit 1; }
done
if [ -d web/node_modules ]; then
  (cd web && npx tsc --noEmit -p tsconfig.json) && echo "contracts: generated TS matches the spec checks"
else
  echo "contracts: web/node_modules missing, skipped the tsc check" >&2
fi
