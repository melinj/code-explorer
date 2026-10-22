# code-viewer

A canvas code viewer for reviewing jj repositories with coding agents. The whole repository is
one zoomable map:
- **Lanes:** code, tests, config, infra and docs each get a band.
- **Placement:** modules sit in a grid by how often they change (heat) and how much depends on
  them.
- **Zoom:** zooming in goes from modules to outlines to real, highlighted code with its links.
- **Review:** you comment on lines, and a coding agent (Claude Code over MCP) answers, edits and
  resolves conflicts while you watch.

The design is in `docs/DESIGN.md`, the contracts in `docs/contracts/`, and the work breakdown in
`docs/WORKSTREAMS.md`.

## Quick start

You need Rust with a C compiler, Node.js 22.12 or newer, and [jj](https://jj-vcs.github.io/jj/)
for your own repositories (details under [Requirements](#requirements)). Then:

```sh
git clone https://github.com/melinj/code-explorer.git && cd code-explorer
./start.sh --demo                 # a synthetic repository to look around in first; no jj needed
./start.sh ~/src/my-project       # your own jj (or git) repository
```

The first run builds the server and the UI, which takes several minutes; later runs start in
seconds. `start.sh` opens the UI in your browser, and Ctrl-C stops it. Before pointing it at a
repository you care about, read [what it does to your repository](#before-you-start-what-it-does-to-your-repository).

## What's here

- `crates/cv-server` (binary `cv`): indexes a jj repository (parsing, links, channels, churn, layout) and
  serves the canvas routes, review threads, edits, conflicts, live events and MCP for agents.
- `crates/cv-mock`: a synthetic 20k-window backend for UI work and benchmarks (no repo needed).
- `web/`: the React + PixiJS front end.
- Design: `docs/DESIGN.md`, contracts: `docs/contracts/`, work breakdown: `docs/WORKSTREAMS.md`.

## Requirements

- **Rust:** stable Rust 1.90 or newer (the dependencies need it; developed and tested on 1.99), from
  [rustup](https://rustup.rs), plus a C compiler: the tree-sitter grammars and SQLite are built
  from C. Install Xcode Command Line Tools on macOS, or `build-essential` (or your distro's
  equivalent) on Linux.
- **Node:** 22.12 or newer (`npm`).
- **jj:** [jj](https://jj-vcs.github.io/jj/) for the repo you view. It is tested with jj 0.45–0.46;
  the server links jj-lib 0.45.
- **Platforms:** Linux and macOS. Windows is untested (try WSL).
- **Languages:** Python, TypeScript/JavaScript, Rust and C/C++, plus config files (YAML, TOML,
  JSON, Dockerfile, shell, HCL). Files in other languages show up, but without symbols or links.
- **First build:** compiles a few hundred crates, which takes several minutes.
- The folder you point `cv` at must be a **jj workspace**. Any folder inside it works; `cv` finds
  the root. A plain git repo can be made one in place with `jj git init --colocate`, run inside it.

### Before you start: what it does to your repository

- Reading never modifies the repository.
- Changes happen only when you, or an agent you allowed, apply an edit, accept a suggestion or
  resolve a conflict.
  - Each change is one jj operation that rewrites the commit holding the code, so its descendants
    are rebased.
  - In a colocated git repo git sees the rewritten commits: like any jj rewrite, git's HEAD becomes
    detached.
  - `jj op undo` takes any change back.
- An agent connected over MCP may edit by default. Turn that off in the **Agent** panel.
- To look around first, try it on a clone: `jj git clone --colocate ~/src/my-project /tmp/my-project`.
- Local data:
  - Threads and edits live in `~/.local/share/code-viewer/<repo id>/`.
  - The index lives in `~/.cache/code-viewer/<repo id>/`. It can reach a few hundred MB for a
    large repository, and is safe to delete.

### Security notes

Code Viewer is a local tool, and it trusts whoever holds its token.
- **Network:** the server listens on 127.0.0.1 only. It refuses requests addressed to any other
  host name (DNS rebinding), and browser requests from other sites or other local ports.
- **The token:** every API and MCP request needs it. It lives in `~/.local/share/code-viewer/token`
  (owner-only) and is printed at startup, so keep it out of screenshots. Anyone holding it can read
  and edit every repository you serve. Delete the file to get a new one, then register MCP again.
  `--no-auth` turns the check off; use it only for experiments.
- **Agents:** the MCP profiles (`reader`, `editor`, …) and the Agent panel's switches steer a
  cooperating agent. They are not a sandbox: an agent holding the token can call any endpoint. Give
  it the same trust you would give it in your shell.
- **Untrusted repositories:** files are read as data; nothing in the repository is run. Opening a
  repository still means jj loads its `.jj` config, as running `jj` there would.

## Run it on your repo

```sh
./start.sh ~/src/my-project            # add --mcp to also register it with Claude Code
```

`start.sh` builds the server and the UI when they are missing or out of date, checks the folder is a jj
repository (and offers `jj git init --colocate` for a plain git one), picks a free port starting at
7878, starts the server and opens the UI in your browser. Ctrl-C stops it. Options: `--port N`,
`--no-open`, `--rebuild`, `--mcp` (runs `claude mcp add` for that repo, local scope), `--yes`,
`--demo` (the synthetic repository instead of a real one), and anything after `--` goes to
`cv serve`. `./start.sh --help` lists them.

The same steps by hand:

1. Build once (the server, and the UI it serves):

   ```sh
   cargo build --release -p cv-server
   (cd web && npm install && npm run build)          # → web/dist
   ```

2. Serve the folder you want to review. The folder is the positional argument (default: the current
   directory):

   ```sh
   ./target/release/cv serve ~/src/my-project
   ```

   It indexes the snapshot at `@` (a few seconds for ~5k files), then prints what to open and how to
   connect Claude:

   ```text
     UI:  http://127.0.0.1:7878/?token=3f9c…
     MCP: claude mcp add --transport http cv http://127.0.0.1:7878/mcp/editor --header "Authorization: Bearer 3f9c…"
   ```

3. Open the UI link. That's it: the API, live events, MCP and the UI all come from this one process.

Options:

| Flag | Meaning |
|---|---|
| `--port N` | listen on 127.0.0.1:N (default 7878) |
| `--token T` | use this bearer token for this run. Default: `CV_TOKEN`, else the stable token in `~/.local/share/code-viewer/token`: generated on the first run, then the same for every run and every repo (`--token` and `CV_TOKEN` never overwrite it). Delete the file to get a new one. |
| `--no-auth` | no token check (local experiments only) |
| `--ui DIR` / `--no-ui` | serve the UI from DIR (default: `CV_UI_DIR`, else this checkout's `web/dist`) / don't serve it |
| `--max-commits N` | how much history to read for heat (default 2000) |

Reading never modifies the repo. Writes happen only when you (or an agent you allowed) apply an
edit, accept a suggestion or resolve a conflict: each is one jj operation, attributed in the op log,
undoable with `jj op undo`. Immutable commits are never rewritten. Threads, edits and events live in
`~/.local/share/code-viewer/<repo id>/review.db`.

For UI development, run the Vite dev server against a backend instead (hot reload; it proxies `/api`
and adds the token): `cd web && CV_API=http://127.0.0.1:7878 CV_TOKEN=<token> npm run dev`, then open
http://127.0.0.1:5178.

To review a different folder, start `cv serve` on that path (one repo per server). To have several open
side by side, give each its own `--port`; each serves its own UI.

### Let Claude Code work on the repo (MCP)

`cv serve` also serves MCP (Streamable HTTP) at `/mcp`, with tool profiles at `/mcp/reader` (read
only), `/mcp/reviewer` (read + threads, tours, links), `/mcp/editor` (threads with their code, edit +
reply in one call, conflicts; the default for `/mcp`) and `/mcp/full` (every tool). Use `editor` for an
agent that addresses review comments. No workspace binding is needed: the agent works in the served
workspace (over stdio, in the jj workspace of its cwd).

Register it with the `MCP:` line `cv serve` prints. The token stays the same across restarts (it is
reused from the last run), so you only do this once per repo. Over stdio instead, through the shim in
the same binary:

```sh
claude mcp add cv -e CV_TOKEN=<token> -- /path/to/code-viewer/target/release/cv mcp --server http://127.0.0.1:7878 --profile editor
```

Edits go through the server to the repo it serves, whatever folder Claude runs in. Then, in Claude Code,
run the server's prompt `/mcp__cv__address_review`, or ask e.g. "Use the cv tools to address the open
threads assigned to the agent; edit only with edit_code." Comments you mark **Ask agent** (or send with
**Send N open to agent** in the Review panel) show up for it; its replies, edits and where it is
looking appear live in the UI. The Agent panel controls whether it may edit and whether it may move your
camera. If the tools fail with `token rejected`, the token Claude has is not the one the server uses
(it is in `~/.local/share/code-viewer/token`); if they fail with `not reachable`, `cv serve`
is not running on that port.

### Try it without a repo

`./start.sh --demo` serves a synthetic repository of 20k windows (`crates/cv-mock`) with the built UI. It
includes a simulated agent that answers **Ask agent** comments. For UI work with hot reload, run the
mock and the Vite dev server instead:

```sh
cargo run --release -p cv-mock            # 127.0.0.1:7878 (--windows N, --latency-ms L)
cd web && npm install && npm run dev      # http://127.0.0.1:5178
```

### Use

- Navigate: drag to pan, wheel to zoom (Shift+wheel pans), `f` fits the map, `h` returns to the
  review-first cell, arrows pan, `+`/`-` zoom, `⌘K` searches symbols, files, topics and threads.
- Select a window to see its links, tests, callers and callees in the side bar (hover an entry to see
  its line on the canvas); click a port pill or link to fly to the other end.
- Review: click the gutter to comment on a line, drag in the gutter for a range (drag the band's
  handles to adjust); double-click code to edit it; open conflicts from their red markers.
- Range review (`Range` in the top bar) shows what changed in a revset, in place.
- **Lenses** (top bar) change emphasis, never the layout. What a lens leaves out turns grey, along
  with the links between such windows.
  - **Heat:** how often code changes, on one continuous scale (legend: Frozen → Hot), with a glow
    over the hottest.
  - **Depends:** blue for code others depend on, teal for code that depends on others.
  - **Hotspots:** review-first code, which is both hot and central.
  - **Co-change:** files and modules that change together in history.
    - Point at a module or a file to light up its partners, by how surely they change together.
      Dashed arcs mark coupling that no static link explains.
    - Rest the pointer on it (a ring fills), or press `G`, to **gather** the partners into a tray
      beside it. Click a card to fly there; `Esc` (or moving away) puts them back.

To check indexing speed on a repo without serving it: `./target/release/cv index ~/src/my-project`.

## Performance checks

`web/scripts/bench.mjs` drives scripted pans, zoom sweeps, flings and dives in real Chrome (GPU) through
`window.__cv.bench(name)` and reports frame-interval percentiles, frames over 20/33 ms and long tasks.
It also prints a phase breakdown (camera, data, builds, text, render) for every frame whose work took
more than 20 ms.

```sh
cd web && npx vite build && npx vite preview &      # benchmarks run against the production build (:5179)
node scripts/bench.mjs                              # all scenarios, 1600×1000
node scripts/bench.mjs --cpu 4 fling-l1 dives       # 4× CPU throttling, selected scenarios
node scripts/bench.mjs --width 2560 --height 1440 --dpr 2
node scripts/bench.mjs --profile --callers someFn   # CPU profile: top self time, and the stacks into someFn
node scripts/bench.mjs --shots /tmp/shots --dark    # screenshots at each tier
```

The rules that keep navigation at 60 fps are in DESIGN §5, "Rendering performance".

## Tests

```sh
cargo test && cargo clippy -- -D warnings
cd web && npm run typecheck && npm test
```

### Shared build cache

Every jj workspace has its own `target/`, so each one would cold-compile all dependencies. With
[sccache](https://github.com/mozilla/sccache) installed (on `PATH` or in `~/.cargo/bin`), `scripts/ci.sh`,
`scripts/ci-e2e.sh` and `start.sh` compile through it (`scripts/env.sh`): workspaces share compiled
dependencies in `~/.cache/sccache` (cap 30 GB; `SCCACHE_DIR`, `SCCACHE_CACHE_SIZE`). Without sccache
nothing changes; `CV_SCCACHE=0` turns it off. For plain `cargo` in your shell: `source scripts/env.sh`.
Install without root: `cargo install sccache --locked`, or the release binary into `~/.cargo/bin`.
A fresh workspace with a warm cache still compiles the workspace crates, proc-macros and a few
dependencies whose keys include the workspace path (about 70% of dependency compiles hit), and
links every test binary: it saves about a quarter of the CPU time of a cold build, more when
several agents build at once. Incremental compilation stays on locally (sccache skips only the
workspace's own crates); on CI (`CI` set) `CARGO_INCREMENTAL=0`. `sccache --show-stats` shows hits.
Dependencies are built with `debug = "line-tables-only"` (backtraces keep file:line): a debug
test build's `target/` is about 17.5 GB instead of 27 GB.

End-to-end flows in real Chrome (`web/scripts`), against the mock (`URL` defaults to the :5179 preview)
or against `cv serve` on a throwaway clone (never a repo you care about):

```sh
python3 web/scripts/seed-conflicts.py /tmp/clone                 # optional: two jj conflicts for review-flow
CV_TOKEN=t cargo run --release -p cv-server -- serve /tmp/clone --port 7880
cd web && CV_TOKEN=t CV_API=http://127.0.0.1:7880 npx vite --port 5190 &
URL=http://127.0.0.1:5190 node scripts/review-flow.mjs           # threads, suggestions, editor, conflicts, expand
NO_SIM_AGENT=1 URL=http://127.0.0.1:5190 node scripts/panels-flow.mjs   # panels, search, tours, permissions, ranges
CV_TOKEN=t node scripts/agent-loop.mjs out --server http://127.0.0.1:7880 --url http://127.0.0.1:5190 --mcp-http
```

`agent-loop.mjs` runs a real `claude -p` (capped at $2) on cv serve's MCP: a comment assigned to the agent
in the UI is answered, the code is refactored, and the UI shows the reply and the new text live.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
