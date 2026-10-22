# Build environment shared by scripts/ci.sh, scripts/ci-e2e.sh and start.sh (sourced, not run).
#
# Shared compile cache: when sccache is installed (on PATH, or ~/.cargo/bin/sccache), cargo runs
# rustc (and cc-rs runs the C compiler for the tree-sitter grammars and SQLite) through it, so every
# jj workspace and agent reuses the same compiled dependencies instead of cold-compiling its own.
# Without sccache nothing changes. Opt out with CV_SCCACHE=0; a RUSTC_WRAPPER you set yourself wins.
#
#   SCCACHE_DIR         cache location (default ~/.cache/sccache)
#   SCCACHE_CACHE_SIZE  size cap, LRU-evicted (default 30G; read when the sccache server starts)
#
# What is shared: registry crates and the C/asm of the tree-sitter grammars and SQLite. Crates
# whose build script output is included via OUT_DIR (serde, anyhow, thiserror, …) and their
# dependents still miss across workspaces, because sccache hashes the absolute OUT_DIR path (about
# 70% of the Rust dependency compiles hit; SCCACHE_BASEDIRS does not apply to rustc in 0.18).
# Rebuilding at the same path (a wiped target/) hits everything. Proc-macro, bin and build-script
# crates are never cached (they link).
#
# Incremental compilation stays on locally: sccache skips incremental crates, but those are only
# the workspace's own crates, and their key includes the workspace path anyway, so turning it off
# would lose fast edit-compile rebuilds for no cross-workspace hits. On CI (CI set)
# CARGO_INCREMENTAL defaults to 0: nothing is rebuilt there and incremental state only costs time
# and disk.
# sccache refuses to run at all when CARGO_INCREMENTAL=1 is set explicitly: then it stays off.
if [[ "${CV_SCCACHE:-1}" != "0" && -z "${RUSTC_WRAPPER:-}" && "${CARGO_INCREMENTAL:-}" != "1" ]]; then
  _cv_sccache="$(command -v sccache 2>/dev/null || true)"
  [[ -z "$_cv_sccache" && -x "$HOME/.cargo/bin/sccache" ]] && _cv_sccache="$HOME/.cargo/bin/sccache"
  if [[ -n "$_cv_sccache" ]]; then
    export SCCACHE_DIR="${SCCACHE_DIR:-$HOME/.cache/sccache}"
    export SCCACHE_CACHE_SIZE="${SCCACHE_CACHE_SIZE:-30G}"
    # Start the server now (or reach the one already running); if sccache is broken, build
    # without it rather than fail.
    if "$_cv_sccache" --start-server >/dev/null 2>&1 || "$_cv_sccache" --show-stats >/dev/null 2>&1; then
      export RUSTC_WRAPPER="$_cv_sccache"
    else
      echo "env.sh: sccache at $_cv_sccache does not start; building without the cache" >&2
    fi
  fi
  unset _cv_sccache
fi
if [[ -n "${CI:-}" ]]; then export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"; fi
