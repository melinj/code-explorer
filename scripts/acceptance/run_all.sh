#!/usr/bin/env bash
# Run every acceptance harness on a throwaway clone of a repo and record the results under
# scripts/acceptance/results/ (git-ignored: results quote the code they measured).
# usage: scripts/acceptance/run_all.sh SOURCE CLONE_DIR   (CLONE_DIR is created by prepare.sh if missing)
# Needs target/release/cv (cargo build --release -p cv-server) or CV_BIN; port 7997 (CV_ACCEPT_PORT).
# The hand-labelled samples (results/*_sample.tsv) are only re-scored here; to draw new ones, delete
# them and run sample_links.py (then label them by reading the code at both ends).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
src=${1:?usage: run_all.sh SOURCE CLONE_DIR}
clone=${2:?usage: run_all.sh SOURCE CLONE_DIR}
"$here/prepare.sh" "$src" "$clone"
cache="$here/results/.cache"; mkdir -p "$cache"
failed=()
py() { timeout 3600 python3 "$here/$1" "${@:2}" || failed+=("$1"); }
py appendix_c.py "$clone" --axes-json "$cache/axes.json"
py config_fields.py "$clone" --ref-json "$cache/config_fields.json" --dump "$cache/channel-links.tsv"
py tile_bench.py "$clone"
# name:min precision:confidence floor (link precision counts links ≥ 0.6, the dependencies; lexical
# candidates ≤ 0.5 are scored as info in link_candidates and in link_precision's own breakdown).
# A result file's leading `#` lines (seed, revisions, labeller, criteria) are kept when it is re-scored.
for s in link_precision:0.9:0.6 link_precision_ge06:0.9:0.6 link_candidates:0:0 config_plausibility:0.8:0; do
  IFS=: read -r name min floor <<<"$s"
  f="$here/results/${name}_sample.tsv"
  [ -f "$f" ] || continue
  out="$here/results/${name}.txt"
  hdr=$(grep '^#' "$out" 2>/dev/null || true)
  { [ -n "$hdr" ] && printf '%s\n' "$hdr"
    timeout 60 python3 "$here/sample_links.py" score "$f" --min "$min" --floor "$floor"; } >"$out.new" \
    || failed+=("$name")
  mv "$out.new" "$out"
  cat "$here/results/${name}.txt"
done
if ((${#failed[@]})); then echo "ACCEPTANCE FAILED: ${failed[*]}"; exit 1; fi
echo "ACCEPTANCE OK"
