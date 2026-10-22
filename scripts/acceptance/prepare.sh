#!/usr/bin/env bash
# Make a throwaway jj clone of a repo at its trunk, for the acceptance harnesses.
# usage: scripts/acceptance/prepare.sh SOURCE DIR
# The source repo is only read (jj git clone); everything happens in DIR.
set -euo pipefail
src=${1:?usage: prepare.sh SOURCE DIR}
dir=${2:?usage: prepare.sh SOURCE DIR}
if [ -e "$dir/.jj" ]; then echo "$dir exists; reusing it" >&2; exit 0; fi
timeout 900 jj git clone "$src" "$dir" >/dev/null 2>&1
(cd "$dir" && timeout 300 jj new 'trunk()' >/dev/null 2>&1)
echo "clone at $dir: @- = $(cd "$dir" && jj log --no-graph -r @- -T 'commit_id.short(12)')"
