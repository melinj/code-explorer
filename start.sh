#!/usr/bin/env bash
# Start Code Viewer on a repository: builds what is missing or stale, checks the folder is a jj repo,
# picks a free port, serves the API + UI + MCP, and opens the browser.
#
#   ./start.sh [REPO] [options] [-- extra `cv serve` args]
#   ./start.sh --demo
#
#   REPO              folder to review (default: current directory); any folder inside the repo works
#   --demo            no repository needed: a synthetic repository, with a simulated agent
#   --port N          port (default 7878, or the next free one)
#   --no-open         don't open the browser
#   --rebuild         rebuild the server and the UI even if they look up to date
#   --mcp             also register the MCP server with Claude Code for REPO (claude mcp add, local scope)
#   --yes             answer yes to prompts (e.g. making a plain git repo a colocated jj repo)
#   -h, --help        this help
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$here/scripts/env.sh" # the shared compile cache (sccache) when installed
repo="."
port=""
open_browser=1
rebuild=0
register_mcp=0
yes=0
demo=0
extra=()

usage() { awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "${BASH_SOURCE[0]}"; }
die() { echo "start.sh: $*" >&2; exit 1; }
say() { echo "▸ $*" >&2; }

repo_set=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --port) port="${2:?--port needs a number}"; shift 2 ;;
    --no-open) open_browser=0; shift ;;
    --rebuild) rebuild=1; shift ;;
    --mcp) register_mcp=1; shift ;;
    --yes|-y) yes=1; shift ;;
    --demo) demo=1; shift ;;
    -h|--help) usage; exit 0 ;;
    --) shift; extra+=("$@"); break ;;
    # Other flags go to `cv serve`, with their value when one follows (`--max-commits 500`); a
    # folder after a flag is the repository (`--no-auth ~/src/x`).
    -*)
      if [[ $# -gt 1 && "$2" != -* && ! -d "$2" ]]; then extra+=("$1" "$2"); shift 2; else extra+=("$1"); shift; fi ;;
    *) [[ $repo_set -eq 0 ]] || die "one repository at a time (got '$repo' and '$1')"; repo="$1"; repo_set=1; shift ;;
  esac
done

# ── Prerequisites ────────────────────────────────────────────────────────────────────────────────
need() { command -v "$1" >/dev/null 2>&1 || die "$1 not found — $2"; }
need cargo "install Rust from https://rustup.rs"
need npm "install Node.js 22.12+ from https://nodejs.org"
node_v="$(node -p 'process.versions.node' 2>/dev/null || echo 0)"
IFS=. read -r node_major node_minor _ <<<"$node_v"
if (( node_major < 22 || (node_major == 22 && node_minor < 12) )); then die "Node.js $node_v is too old for the UI build; install 22.12 or newer"; fi
if [[ $demo -eq 1 ]]; then
  [[ $repo_set -eq 0 && $register_mcp -eq 0 ]] || die "--demo shows a synthetic repository; drop REPO and --mcp"
else
  need jj "install jj from https://jj-vcs.github.io/jj/latest/install-and-setup/"
fi

# ── The repository ───────────────────────────────────────────────────────────────────────────────
if [[ $demo -eq 0 ]]; then
[[ -d "$repo" ]] || die "no such folder: $repo"
repo="$(cd "$repo" && pwd)"
# `jj root` from inside the folder finds the repository above it (`jj root -R` would not).
if ! root="$(cd "$repo" && jj root 2>/dev/null)"; then
  if top="$(git -C "$repo" rev-parse --show-toplevel 2>/dev/null)"; then
    # A jj repository jj can't open (a jj version or config problem): show jj's own error.
    [[ -d "$top/.jj" ]] && { (cd "$repo" && jj root) || true; die "jj could not open $top"; }
    echo "$top is a git repository but not a jj one."
    echo "Code Viewer needs jj; 'jj git init --colocate' adds it alongside git (git keeps working)."
    if [[ $yes -eq 1 ]]; then answer=y
    elif [[ -t 0 ]]; then read -r -p "Run it now? [y/N] " answer || answer=n
    else answer=n
    fi
    [[ "$answer" =~ ^[Yy] ]] || die "not a jj repository: $repo (run 'jj git init --colocate' in $top, or pass --yes)"
    (cd "$top" && jj git init --colocate)
    root="$top"
  else
    die "$repo is not inside a jj (or git) repository (try ./start.sh --demo to look around first)"
  fi
fi
fi

# ── Build what is missing or stale ───────────────────────────────────────────────────────────────
if [[ $demo -eq 1 ]]; then pkg=cv-mock; else pkg=cv-server; fi
cv="$here/target/release/$([[ $demo -eq 1 ]] && echo cv-mock || echo cv)"
# Newer than $1: any file under the given paths (sources, manifests, lockfiles).
stale() {
  local out="$1"; shift
  [[ ! -e "$out" ]] && return 0
  [[ -n "$(find "$@" -newer "$out" -type f -print -quit 2>/dev/null)" ]]
}
if [[ $rebuild -eq 1 ]] || stale "$cv" "$here/crates" "$here/Cargo.toml" "$here/Cargo.lock"; then
  say "building the server (cargo build --release -p $pkg; the first build takes several minutes)…"
  (cd "$here" && cargo build --release -p "$pkg")
fi
web="$here/web"
if [[ ! -d "$web/node_modules" ]] || stale "$web/node_modules/.package-lock.json" "$web/package.json" "$web/package-lock.json"; then
  say "installing UI dependencies (npm ci)…"
  (cd "$web" && npm ci --no-fund --no-audit)
fi
if [[ $rebuild -eq 1 ]] || stale "$web/dist/index.html" "$web/src" "$web/index.html" "$web/package.json" "$web/vite.config.ts"; then
  say "building the UI (npm run build)…"
  (cd "$web" && npm run build --silent -- --logLevel warn)
fi

# ── A free port ──────────────────────────────────────────────────────────────────────────────────
in_use() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
if [[ -n "$port" ]]; then
  in_use "$port" && die "port $port is in use (pick another with --port)"
else
  port=7878
  while in_use "$port"; do port=$((port + 1)); done
fi

# ── Serve, and open the UI once it is up ─────────────────────────────────────────────────────────
open_url() {
  if command -v xdg-open >/dev/null 2>&1; then xdg-open "$1" >/dev/null 2>&1 &
  elif command -v open >/dev/null 2>&1; then open "$1" &
  fi
}
if [[ $demo -eq 1 ]]; then
  say "serving the demo on port $port (Ctrl-C stops it)"
  serve=("$cv" --port "$port" --ui "$web/dist")
else
  say "serving $root on port $port (Ctrl-C stops it)"
  # (The odd expansion keeps bash 3.2, macOS's default, happy with an empty array under `set -u`.)
  serve=("$cv" serve "$root" --port "$port" ${extra[@]+"${extra[@]}"})
fi
set +e
# Only the server's own two lines are acted on (they start with two spaces); anything else it logs
# (which can quote file names from the repository) is just printed.
"${serve[@]}" 2>&1 | while IFS= read -r line; do
  printf '%s\n' "$line"
  case "$line" in
    "  UI:  http://127.0.0.1:"*)
      [[ $open_browser -eq 1 ]] && open_url "${line#  UI:  }"
      ;;
    "  MCP: claude mcp add "*)
      if [[ $register_mcp -eq 1 ]]; then
        # The URL and token are parsed out and passed as plain arguments: nothing from the server's
        # output is run as shell code.
        mcp_url="" mcp_token=""
        [[ "$line" =~ (http://127\.0\.0\.1:[0-9]+/mcp/[a-z]+) ]] && mcp_url="${BASH_REMATCH[1]}"
        [[ "$line" =~ Bearer\ ([^\"]+)\" ]] && mcp_token="${BASH_REMATCH[1]}"
        mcp_header=()
        [[ -n "$mcp_token" ]] && mcp_header=(--header "Authorization: Bearer $mcp_token")
        if [[ -z "$mcp_url" ]]; then
          echo "▸ could not read the MCP line above; run it yourself"
        elif command -v claude >/dev/null 2>&1; then
          # Replace an earlier registration (the port or token may differ), then add for this repo.
          (cd "$root" && { claude mcp remove cv -s local >/dev/null 2>&1
            claude mcp add -s local --transport http cv "$mcp_url" ${mcp_header[@]+"${mcp_header[@]}"} >/dev/null 2>&1; }) \
            && echo "▸ registered MCP server 'cv' with Claude Code for $root (run claude there; try /mcp__cv__address_review)" \
            || echo "▸ could not register with Claude Code; run the MCP line above yourself"
        else
          echo "▸ claude not found; run the MCP line above once Claude Code is installed"
        fi
      fi
      ;;
  esac
done
exit "${PIPESTATUS[0]}"
