# Code Viewer: base design

Draft v0.3, 2026-10-18.

- v0.2 folded in the answers to the v0.1 open questions, plus a measured study of how to show change frequency and connectedness.
- v0.3 adds file-chip colours, config-field usage links (prototyped on the reference repo: a private Python/TypeScript project of about 5.6k files that the numbers in this document were measured on; its code is not part of this repository), diff-focused range reviews on the full view's map, and heuristic C++ includes.

Companion files:

- `docs/contracts/*.ts`: the external API contracts (shared model, HTTP, WebSocket, MCP, wire formats). They are type-only and strict-typechecked. The Rust backend implements them in `cv-contracts`, and `ts-rs` must regenerate the same shapes.
- `docs/contracts/rust/`: the trait sketches for the boundaries between backend crates. They compile.
- `docs/schema/index.sql`, `docs/schema/review.sql`: the SQLite schema.
- `docs/WORKSTREAMS.md`: how the work splits across implementer agents.
- [Code Viewer Blueprint](https://claude.ai/artifact/M8kXC79WAw5WeW7s2Rk7Bp): wireframes plus a visual tour of the contracts. If they disagree, the repo files win.

## 1. What we are building

A local web app that lays a jj repository out on one large canvas, so a human and a coding agent can review and edit it together.

- **Canvas.** Code sits in windows of at most about 500 lines. Windows group into clusters by dependencies.
- **Lanes.** Each file role gets its own horizontal lane: infrastructure, config, code, tests, docs.
- **Two axes.** Inside each lane, clusters sit on a grid. Columns are change frequency, shared across all lanes. Rows are connectedness, per lane. "Hot and central", the code to review first, is always top-left.
- **Links.** Calls, inheritance, event-bus topics, HTTP endpoints, config keys and deploy wiring are drawn as edges. Each edge starts at a labelled port on the exact callsite line. When a link's target can only be known at runtime, it ends in a visible open stub.
- **Two modes.** *Snapshot* shows the whole codebase at one revision, with heat per file. *Range* shows a chain of commits as a diff, with heat per hunk.
- **Workspaces are first-class.** Every jj workspace is visible, including each agent's: its stack, its unsaved files, the agent attached to it, and any overlap with other workspaces.
- **Review.** GitHub-style threads with suggestions, plus inline editing at any mutable commit. The backend amends the right commit, rebases descendants and reports conflicts.
- **Agent interface.** An MCP server exposes the same view, threads, edits, conflicts, workspaces and hotspots. The human always sees what the agent is looking at.

Non-goals for v1: native git support (we colocate jj instead: `jj git init --colocate`), runtime tracing of links, multi-user hosting, and PR-platform sync.

## 2. Decisions

| # | Decision | Choice | Why |
|---|---|---|---|
| D1 | Backend language | **Rust**, from day one *(user, 2026-10-18)* | jj-lib gives native reads and writes, exact edit previews in a discarded transaction, and new files at any commit without a temporary workspace. tree-sitter, SQLite and MCP all have mature crates (§3). |
| D2 | Frontend | TypeScript: React for chrome, PixiJS (WebGL2) for the world including L3 code text (glyph-atlas meshes), CodeMirror 6 (DOM) for the activated window | The GPU draws thousands of frames cheaply. DOM text was measured and rejected for L3 (§5 "Rendering performance"): it costs style, layout and raster work whenever windows enter the view. Selection and editing happen in the activated window. |
| D3 | Contracts | `docs/contracts/*.ts` is the spec. The Rust `cv-contracts` crate (serde + ts-rs) is the implementation of the HTTP and WS types and generates TS for the web. MCP tool inputs are the exception: they are serde + schemars structs in `cv-mcp` (`crates/cv-mcp/src/params.rs`, next to the tools that take them; shared types such as `ViewSpec` come from `cv-contracts` via `#[schemars(with = …)]`), and their JSON Schemas, as the agent sees them, are rendered to TS and checked against `mcp.ts` `McpTools[*].in` (§3 *Contracts check*). | One source of truth in the code that runs. The checked-in TS spec is the golden file the generated TS must match. |
| D4 | jj access | jj-lib pinned to the installed CLI's exact version (0.45.1). The verified CLI recipes are kept as the test oracle and a fallback. | Same on-disk formats as the user's jj, so both can work on one repo. Upgrade the two together. |
| D5 | Storage | `index.db` (derived, deletable) + `review.db` (human and agent data). Threads stay in the database; export to the repo may come later. *(user)* | Re-indexing can never destroy comments. |
| D6 | Grades | Quantiles on both axes, never fixed limits *(user)*. Heat uses change-mass quantiles; connectedness uses rank quantiles. The cut points are configurable per repo in `.cv/grades.toml` (§4.7). | Adapts to any repo size and history length. |
| D7 | Connectedness metric | Dependents within 2 hops, code lane only, with tests counted separately | Measured on the reference repo (§4.8): transitive reach saturates because of a 270-file import cycle, and tests inflate it. |
| D8 | Default layout | District grid: lanes × heat columns × connectedness rows, dependency clusters kept whole inside cells (§4.9) | Makes both axes explicit and keeps related code together. The layout stays stable because clusters move only when a grade settles. |
| D9 | Big files | Windows are the layout unit. Fragments of one file attract each other and carry a coloured file chip; clicking the chip highlights every fragment. *(user)* | Dependencies decide placement, and file identity stays one click away. |
| D10 | Edits | Amend when the target is mutable *(user)*. Immutable targets get a new branch. Range edits are absorbed into the commits that own the lines. | Review fixes land in the change under review. |
| D11 | Dynamic links | No runtime tracing *(user)*. A key that can't be resolved statically ends in an open stub labelled with what is known. | Honest about what static analysis can see, without the cost of tracing. |
| D12 | Workspaces | First-class *(user)*: a panel, live overlays, agent binding, stack overlap warnings | The reference repo runs a dozen agent workspaces in parallel. |
| D13 | Languages and IPC | Plugin packs behind traits *(user)*: Python and TypeScript first, then Rust and C++. IPC methods are separate channel packs. No language detail leaks into the core. | New languages and protocols are added as data plus a small crate. |
| D14 | File chips | Each split file gets its own chip colour, muted at rest. Selecting a file makes its chips and fragment outlines more saturated and brighter, and marks its fragments on the minimap. *(user)* | File identity is visible everywhere, without competing with heat until you ask for it. |
| D15 | Config files | Their own lane, plus a heuristic for where their fields are used, scored and drawn with its confidence *(user)*. One hub per file family and key. | "Who reads this setting" is the main question about config. Exact answers aren't needed (§4.6). |
| D16 | Range review | The full view's map: same lanes, heat columns, connectedness rows and positions. Windows are individual diffs; unchanged connected functions start minimized. *(user)* | Switching between snapshot and range keeps every cluster where you last saw it. |
| D17 | C++ includes | Heuristic resolution only, no `compile_commands.json` *(user)* | Works on any checkout without a build. |

## 3. Architecture

```
 Browser (TypeScript)                               Coding agents (Claude Code, opencode, …)
 ┌──────────────────────────────────┐               ┌────────────────────────────────┐
 │ F4 panels · F3 windows/editor    │               │ MCP client, bound to a workspace│
 │ F1 canvas engine (PixiJS + DOM)  │               └───────────────┬────────────────┘
 │ F2 data client (tiles, caches)   │                               │ MCP /mcp (Streamable HTTP) or `cv mcp` stdio
 └──────┬──────────────▲────────────┘                               │
        │ HTTP /api/v1 │ WS /api/v1/ws                              │
 ┌──────▼──────────────┴────────────────────────────────────────────▼──────┐
 │ cv (one Rust binary · tokio · axum · 127.0.0.1 · token auth)            │
 │  cv-server  routes, WS, event log, scheduler     cv-mcp   rmcp tools     │
 │  cv-review  threads, anchoring, mutation queue, conflicts                │
 │  cv-lang + language packs ──► cv-link + channel packs ──► cv-churn ──►   │
 │  cv-layout (windows, clusters, district grid)                            │
 │  cv-store   rusqlite (bundled: FTS5, R*Tree) · index.db + review.db      │
 │  cv-vcs     jj-lib 0.45.1 (= the user's jj) · CLI recipes as oracle      │
 └──────┬───────────────────────────────────────────────────────────────────┘
 ┌──────▼──────────────────────────────┐
 │ jj repo + workspaces                 │ ◄── the human, agents, editors
 └──────────────────────────────────────┘
```

**Cargo workspace.** Each crate maps to one workstream (`docs/WORKSTREAMS.md`).

| Crate | Purpose |
|---|---|
| `cv-contracts` | serde types for HTTP and WS with ts-rs derives (MCP tool inputs live in `cv-mcp`, see below) |
| `cv-vcs` | the `Vcs` trait and its jj-lib implementation |
| `cv-store` | rusqlite with the bundled build (FTS5 and R*Tree verified) |
| `cv-lang` | the language-neutral parse driver, chunking and lane classifier |
| `cv-lang-python`, `cv-lang-typescript`, `cv-lang-rust`, `cv-lang-cpp`, `cv-lang-config` | language packs; config covers YAML, TOML, JSON, Dockerfile, HCL and shell |
| `cv-channel` | key algebra and the hub matcher |
| `cv-channel-bus`, `cv-channel-http`, `cv-channel-env`, `cv-channel-deploy` | channel packs |
| `cv-link` | reference resolution, hubs and connectedness |
| `cv-churn` | history mining and heat grades |
| `cv-layout` | windows, clusters and the district grid; Sugiyama layering via `rust-sugiyama` or in-house |
| `cv-review` | threads, anchoring, the mutation queue and conflicts |
| `cv-server` | axum routes, WebSocket, the event log, the scheduler and the `cv` CLI |
| `cv-mcp` | the `rmcp` 3.x server |

Crate versions checked on 2026-10-18:

| Crate | Version |
|---|---|
| jj-lib | 0.45.1 |
| rusqlite | 0.40.2 |
| tree-sitter | 0.27.1 |
| tree-sitter-python | 0.25 |
| tree-sitter-typescript | 0.23 |
| tree-sitter-rust | 0.24 |
| tree-sitter-cpp | 0.23 |
| tree-sitter-yaml | 0.7 |
| tree-sitter-toml-ng | 0.7 |
| tree-sitter-hcl | 1.1 |
| tree-sitter-bash | 0.25 |
| tree-sitter-dockerfile-updated | 0.2 |
| tree-sitter-json | 0.24 |
| axum | 0.8 |
| rmcp | 3.5 |
| ts-rs | 12 |
| schemars | 1.2 |
| petgraph | 0.8 |
| rust-sugiyama | 0.4 |
| notify | 9 (RC) |

**Contracts check (D3).** `scripts/gen-contracts.sh` regenerates; two cargo tests fail on stale output, so `cargo test --workspace` (CI step `test`) enforces both:

- `cv-contracts --test generated_ts`: the ts-rs output in `docs/contracts/generated/` and `web/src/contracts/generated/`, whose `check.ts` asserts structural equality with the hand-written spec;
- `cv-mcp --test it mcp_inputs_ts`: renders every tool's input schema (schemars, slimmed as `tools/list` sends it) as TS into `generated/mcp/inputs.ts`, whose `check.ts` requires mutual assignability with `McpTools[tool]["in"]` of `mcp.ts` (one listed one-way exception: `set_view`, which fills `RangeOptions` defaults); it also checks the Rust profiles against `MCP_PROFILES`.

The `check.ts` files compile under `tsc --noEmit` in `web/` (CI step `web`). The MCP inputs stay in `cv-mcp` rather than `cv-contracts` because their doc comments are the tool descriptions the agent reads and their enums are the tools' argument validation; moving them would not remove a check.

**Stack spike, 2026-10-18** (`docs/spikes/rust-stack/`). It built in 55 s on rustc 1.98.1 and confirmed three things:

- bundled SQLite 3.53.2 with R*Tree and FTS5;
- the tree-sitter 0.27 grammars for Python, TypeScript, Rust and C++ parse sample callsites without errors;
- jj-lib 0.45.1 loads a repo created by the jj 0.45.1 CLI and reads the same head operation id the CLI reports.

`load_at_head` is async in this version, so `cv-vcs` drives jj-lib from the blocking pool through a minimal executor.

**Running it.**

- `cv serve [path]` serves the UI, the API and `/mcp` on 127.0.0.1:7878. `cv mcp` is the stdio shim and `cv index` runs headless indexing.
- Data lives in `$XDG_CACHE_HOME/code-viewer/<repo-id>/index.db` and `$XDG_DATA_HOME/code-viewer/<repo-id>/review.db`. The repo-id is the real path of `.jj/repo`, so all workspaces of one repo share the same data.

**Storage (D5), as built** (`docs/schema/*.sql`, `cv-store`, `cv-server/src/view_store.rs`):

- **index.db** holds the parse cache and the saved snapshot views. Every fresh snapshot build is written in the background, in two transactions. The first (`IndexDb::put_view`) writes files (`canvas_file`), windows (`canvas_window` plus the `canvas_window_rtree` R*Tree over their rects), clusters, links (`link_edge`), the rest of the bootstrap and the layout placement (`canvas_view`). Only when it commits does the second (`put_search`) write the view's symbols and files for search (`search_fts`, `search_scope`). A crash between the two leaves a saved view without search rows; search then falls back to the in-memory index once and writes them (see *Search* below). `cv serve` loads a saved view instead of indexing when spec, operation, head commit and build inputs match (inputs: the `cv` executable, pack versions, `--max-commits`, window pins, env switches; only builds that started without a previous placement, i.e. what a fresh process computes; at most 24 h old, since heat decays). Loading re-reads blob text by id, takes facts from the parse cache and rebuilds the grid index. `cv index` always indexes and writes the result. The newest 6 views are kept. `CV_VIEW_CACHE=off` disables it.
- **Cold start on the reference repo** (5,635 files, 28,713 windows, 72,638 links; parse cache warm): `cv serve` ready (POST /views + bootstrap answered) in 4.1–5.0 s before, 0.88–0.94 s after; loading the saved view takes 0.8 s (rows 0.3 s, blobs 0.2 s, facts 0.06 s, files and grid 0.2 s) against a 3.5–4.0 s index. Saving takes 1.6 s in the background after a build (`cv index`: in the foreground); a server stopped before that simply indexes again next time.
- **Tiles: the in-memory grid stays the hot path.** Measured on the reference repo (2,000 random z0–3 tiles, release): in-memory `tile_response` p50 0.02 ms / p95 2.1 ms; a whole tile from index.db (R*Tree windows, their headers, edges with a port in the tile) p50 0.18 ms / p95 11.8 ms; the bare R*Tree `windows_in_rect` p95 0.3 ms. Every tile also needs the live overlay, link feedback and workspace decorations, which work on the in-memory view. The R*Tree serves the disk path: tiles of a saved view that is not loaded (a view id from before a restart) come from index.db instead of a 404.
- **Search** reads `search_fts` (trigram tokenizer: any substring of 3+ characters, case-insensitive) for symbols and files, then ranks the candidates with the same scores as before (an exact symbol name first). Hubs are searched in memory (channel hubs change in the background). A revision without rows (a view refreshed by an op) falls back to the in-memory index once and writes its rows. On the reference repo, 67 queries × 3 kind filters: FTS p50 0.9 ms / p95 9.4 ms against 11–14 ms / 19–28 ms in memory, and the first search no longer re-analyses every file (several seconds).
- **review.db** keeps declared links and verdicts (`declared_link`, `link_feedback`), saved ranges (`saved_range`), window pins (`layout_pin`), cluster pins (`cluster_pin`) and cluster labels (`cluster_label`) in tables (schema version 2). A version-1 file is upgraded on open and the JSON records earlier builds kept in `meta` move into the tables.

## 4. Indexing pipeline

### 4.1 Sync with jj (cv-vcs)

- **Reads** go through jj-lib, pinned to one operation (`load_at_operation`). A read never snapshots a working copy.
- **Watcher.** `notify` watches `.jj/repo/op_heads/heads`; a check every second still runs in case an event is lost, and if `notify` cannot watch the directory the watcher polls every 250 ms instead. Each new op is diffed against the last one, so only changed changes are re-indexed.
- **Incremental updates.** An open snapshot view whose revision moved is updated in place: changed files are re-parsed and keep their windows' places; deleted files lose their windows and links; new files get windows next to their directory's cluster (or a new cluster at the lane's right edge).
  - In the same step, every code ref is re-resolved over the whole view (facts come from memory or the parse cache), so links into new files and from changed files are current; unchanged links keep their ids. Connectedness is recomputed from the updated graph.
  - Channel links and hubs (the packs match producers and consumers project-wide) and heat (a fresh churn walk) follow in the background, then a second `view.invalidated` names the windows that changed. A window's heat grade changes only when the new grade is settled, or when its file had no change in history before.
  - Positions don't move: cluster cells and the full layout follow at the next full index or on request.
- **Viewport-first scheduling** (`IndexScheduler`, `cv-server/src/scheduler.rs`). The latest `view.state` is the focus: a view id and the world rectangle on screen. After an op, the focused view is refreshed before other views. Within it, changed files with a window in the focus region (the viewport grown by one viewport on each side, i.e. the client's predicted viewport) are re-parsed first, nearest the viewport centre first. When at least 64 changed files (or 1 MiB) lie outside that region, the refresh is staged: the files in view are published with `index.progress { job: "parse", viewId }` and a `view.invalidated` naming their windows, then everything else follows with a second `view.invalidated`. Both stages run inside the op hook, so a read after a write still sees the whole refresh. The first full index of a spec is not staged: its layout is global and the client has no viewport before the bootstrap.
- **Content** is read through the store and is content-addressed by blob id. Conflicted trees are rendered with jj's own marker formatting, in the user's `ui.conflict-marker-style`: a conflicted path is listed once, under a synthetic blob id, and reads as the materialized text with `<<<<<<<` markers. Range diffs name both sides the same way, so a conflicted side shows its markers too.
- **Parse cache.** Parsed facts are cached in index.db (D5) per path × blob × pack version (`LanguagePack::version()`), so a re-index parses only what changed.
- **Live working copies.** For every workspace, files changed on disk but not yet snapshotted are overlaid from disk and marked `unsnapshotted`; new files are drawn as new windows, placed as incremental updates place them; tiles, window batches, fragments and the bootstrap all serve this overlay. We never snapshot on someone's behalf just to display something.
- **Stale and divergent workspaces.** When an operation run elsewhere rewrites a workspace's `@` under it, the card carries `stale` ("working-copy": run `jj workspace update-stale` there). Its files on disk then belong to the old `@`, so they aren't overlaid, and the card counts none of them as unsnapshotted (its stale note says how many there are). A workspace `jj workspace add` is still checking out (its tree state is still the empty tree) is neither stale nor full of edits; it reads as "unknown" until the checkout ends. When `@` turns divergent, the card says so and `workspace.recovered` names the sibling that may hold the edits. Recovering another workspace is its owner's job (§6).

### 4.2 Language packs (cv-lang + one crate per language)

The core never names a language. A pack implements `LanguagePack` (`docs/contracts/rust/lang.rs`):

- it claims files by extension, shebang or sniffing;
- it parses with its tree-sitter grammar, and keeps tree-sitter types inside the pack;
- it turns parse trees into language-neutral `BlobFacts`: symbols, refs, imports, silhouette and tokens. Its `.scm` queries drive this;
- it **folds key expressions**: literals, f-strings, template literals, `format!`, concatenation, constants;
- it resolves imports with project awareness:

| Language | Resolution source |
|---|---|
| Python | `pyproject.toml`, package roots |
| TypeScript | `tsconfig.json` paths |
| Rust | `Cargo.toml` and the `mod` tree |
| C++ | heuristics only (D17): relative to the including file, then conventional include dirs (`include/`, `src/`), then `include_directories()` scraped from `CMakeLists.txt` |

Packs ship in this order: Python and TypeScript (with TSX and JS), then Rust, then C++. `cv-lang-config` covers YAML, TOML, JSON, Dockerfile, HCL and shell for the config and infra lanes. Its "symbols" are top-level keys, compose services, CI jobs, Dockerfile stages and Terraform resources.

- **Config grammars** (2026-10-19). `cv-lang-config` parses YAML (tree-sitter-yaml), TOML (tree-sitter-toml-ng), JSON (tree-sitter-json), Dockerfiles (tree-sitter-dockerfile-updated), shell scripts (tree-sitter-bash) and HCL (tree-sitter-hcl) with `.scm` queries (`crates/cv-lang-config/queries/<dialect>/{symbols,keys,highlights}.scm`). Key paths keep parity with the earlier line scanners and with `config_fields.py` (P5 numbers unchanged on the reference repo except one key the reference read inside a multi-line quoted string).
- **Deviations, kept on the line scanner:**
  - INI/.env, Makefiles/justfiles and Markdown headings: no grammar crate is used for them here (the `sh` lines of a Makefile are not bash).
  - A YAML document, Dockerfile, script or HCL file whose grammar parse has errors falls back to the scanner, so it keeps its keys. YAML: templated files (Jinja `{% %}` templates) are not YAML, yet `config_fields.py` reads their keys line by line. Dockerfile: the grammar (the only maintained Rust crate) lacks legacy `ENV NAME value with spaces`, newer flags such as `COPY --parents` and `ENV NAME=` with no value, and on them recovers by dropping the rest of the file; about 14% of 226 local Dockerfiles hit this. Shell: about 2% of scripts.
- **Code pack queries** (2026-10-19). The Python, TypeScript/TSX/JS, Rust and C/C++ packs read their trees through `crates/cv-lang-<x>/queries/highlights.scm` and `tags.scm` (TSX splices in `jsx.scm`), compiled together into one query so the tree is walked once (`cv_lang_sitter::query::PackQuery`):
  - `highlights.scm` captures map to token classes. Tokens are painted in the order a depth-first walk meets the captured nodes (outer node first), the first capture covering a span wins, and on one node the earlier pattern wins. Each file ends with a generated block that classifies every anonymous token of the grammar (keywords, operators, punctuation); a test per pack fails when the grammar has tokens the block does not list.
  - `tags.scm` captures the nodes facts come from (definitions, calls, imports, heritage). The pack's code turns them into facts. Code decides what a query cannot: which definitions are symbols (module level and direct class/impl/namespace members, not nested functions), the innermost symbol a ref belongs to (by nesting), which parts of a definition count (a class's heritage and body; a function's body and initializers), qualnames and scopes (impl self types, namespaces, `super` depth), Rust `use`-tree flattening and the calls inside macro token trees, and key folding.
  - **Parity:** byte-identical `BlobFacts` before and after (symbols, refs, imports, every token, syntax errors), checked with `cargo run --release -p cv-server --example facts_dump -- DIRS` and a diff, on 10,122 code files: the reference repo (3,015 Python, 494 TS/JS), this repo (266), Python stdlib packages (122), node_modules pixi.js/react-dom/vite (2,237 TS/JS), jj-lib/tokio/rmcp/axum/regex-automata/serde (1,027 Rust), libstdc++ `bits`/`ext` (441 C++) and fmt/gtest/nlohmann/Python/LLVM ADT/Support headers and Arduino libraries (2,360 C/C++, plus 154 Python and JS files in them). The existing pack tests and hand-checked goldens pass unchanged.
  - **Timing** (`parse_speed`, median ms per 1k lines; old and new binaries alternated three times on the same files, load 7–12 on 24 cores; the agreeing runs are shown). The grammar parse, the §9 acceptance figure, is unchanged. The whole `analyze` costs 2–16% more, because a query cursor visits more state per node than the hand-written walks did:

    | Pack | Files | Grammar | Full before → after |
    |---|---|---|---|
    | Python (reference repo) | 2,745 | 4.0 ✓ | 6.5 → 7.2 |
    | JavaScript (reference repo) | 465 | 3.2 ✓ | 5.6 → 6.1 |
    | TypeScript (this repo, the reference repo's web app) | 60 | 4.6–4.8 ✓ | 8.4–8.5 → 9.2 |
    | TSX (this repo's web/src) | 7 | 6.2 (over 5 ms before and after; a property of the grammar on these dense component files) | 10.5 → 11.7 |
    | Rust (this repo) | 147 | 4.1–4.2 ✓ | 7.2 → 8.3 |
    | C++ (libstdc++ `bits`, `ext`) | 446 | 6.8 (accepted exception) | 8.5 → 8.6–8.7 |
  - **Deviations, measured:**
    - Module-level TypeScript declarations are found by listing the program's children in code, not by a `(program …)` pattern: that pattern cost 14.2 ms per 1k lines on this repo's TSX against 5.2 ms without it (grammar parse 6.3 ms).
    - C/C++ highlights colour a name as a function or a macro constant from what reading the tags found (called names through `a::b<T>`, `x.f`, `~X`, `operator==`; declarators of functions in the scopes that are read; `#define` names), via a hook on the `@name` captures: a query cannot follow declarators through pointer/reference wrappers or skip `friend` declarations and unread parts of definitions without losing parity.
- **Conventions as pack data (D13).** What the core needs to know about a language comes from `LanguagePack::conventions()`: self receivers (`self`/`cls`, `this`), module entry stems (`__init__`, `index`, `mod`), entry stems for cluster labels (`main`, `lib`), reserved name prefixes (`__`), and builtin methods, receivers and functions that lexical candidates never guess (§4.5). `scripts/check-core-deps.sh` fails when cv-lang, cv-link, cv-channel, cv-layout or cv-churn name a language, an extension, a self receiver or an entry stem.

### 4.3 Lanes

Lanes come from ordered rules; the first match wins. Defaults live in the binary, and the repo can override them in `.cv/lanes.toml`.

Config files (YAML, TOML, JSON, `.env`) always get their own lane (D15), with field-usage links into code (§4.6). Lanes are user-definable, so a repo can split further. For example, the reference repo could give its prompt YAML (`src/**/prompt*.yaml`), several of its hottest config files, a `prompts` lane of their own.

| Lane | Default rules | Texture |
|---|---|---|
| infra | `Dockerfile*`, `*.dockerfile`, compose files, `.dockerignore`, `.github/workflows/**`, `.gitlab-ci.yml`, `Jenkinsfile`, `Makefile`, `justfile`, `*.tf`, `*.hcl`, `*.service`, `*.sh`, YAML with top-level `apiVersion` + `kind` | diagonal hatch |
| config | `.env*`, YAML, TOML, JSON, INI, CFG, conf, properties not matched above | dots |
| code | source extensions of the installed language packs | plain |
| test | `tests/`, `test/`, `__tests__/`, `e2e/`, `test_*.py`, `*_test.py`, `conftest.py`, `*.test.*`, `*.spec.*` | horizontal lines |
| docs | `*.md`, `*.rst`, `docs/**` (collapsed by default) | sparse crosses |
| other | lockfiles, binaries, vendored (`vendor/`, `third_party/`, `node_modules/`) and generated files (`generated/`, `*_pb2.py`, `*.pb.go`, `*.generated.*`, `*.min.*`) (hidden by default) | — |

The rules are tried in the order other, infra, config, test, docs, code, so a config file under `tests/` or `docs/` is config (D15).

**`.cv/lanes.toml`.** It is read from the indexed revision's tree (never the working directory), so a change to it takes effect on the next full index of a revision that has it; edits applied incrementally to an open view keep that view's lanes. One `[[lane]]` table per lane; every key but `id` is optional:

```toml
[[lane]]
id = "prompts"                    # a new lane, or a built-in id (infra config code test docs other)
label = "Prompts"                 # default: the built-in label, else the id
texture = "dots"                  # plain | hatch | dots | lines | cross; anything else → plain
order = 2                         # 0-based final position among all lanes (hidden ones count)
globs = ["src/**/prompt*.yaml"]   # repo-relative, case-sensitive; `*` stays within a directory
hidden = false                    # true: not indexed or drawn, like `other`
collapsed = false
```

- **Rules.** Every table's globs, in file order, go before the built-in rules; the first match wins. A table with a built-in id adds files to that lane (`id = "code"`, `globs = ["scripts/*.yaml"]`) and may relabel, retexture, reorder or hide it. User rules beat D15 and the content sniffs.
- **Order.** Lanes without `order` keep their place; a new lane without one goes before `other`. Lanes with `order` are then placed at that index, lowest first.
- **Errors never stop indexing.** Bad TOML keeps the defaults; an entry without a valid `id` (1–32 of `a-z 0-9 - _`), a repeated id, a bad glob, a wrong value type or an unknown key is skipped. Each problem is logged as a warning. At most 32 new lanes are read.
- The bootstrap's `laneDefs` and `lanes` (bands) and `GET /lanes` (which also lists hidden lanes) carry the config's lanes in order; range views share the lanes of their tip's snapshot.

Measured on the reference repo (appendix C; files re-counted on a later trunk with the D15 rule order and the vendored/generated rule, which moved 44 fixture files from test to config and 4 generated files from code to other; shares as in appendix C):

| Lane | Files | Share of change mass |
|---|---|---|
| infra | 11 | 0% |
| config | 1,000 | 18.7% |
| code | 1,490 | 42.3% |
| test | 2,029 | 30.3% |
| docs | 1,101 | 7.6% |

Config is the second-most-changed role after code once tests are set aside. Mixing it into the code lane would bury that.

### 4.4 Windows and fragments

**Snapshot chunking** (cv-layout):

- A file of ≤ 120 lines becomes one window.
- A top-level declaration of ≥ 12 lines gets its own window.
- Small declarations are merged into chunks of ≤ 500 lines.
- A declaration over 500 lines splits into a header window plus member windows, or into parts.

**Range hunk windows** are diff-focused: one window per hunk cluster, merging hunks closer than 2 × context and widening to the enclosing symbol when the result fits in 500 lines. Deleted lines travel in `WindowDiff`.

Unchanged functions connected to the change (callers and callees, `contextHops` = 1 by default) appear as `context` windows. They start **minimized** (D16): a header-only pill with name, ↧ dependents and "unchanged". Clicking expands one in place as an overlay, without relayout. `contextDisplay` can also hide them or show them expanded.

**Fragments.** A file split into several windows has fragments 1…n.

- **Layout:** the clustering graph adds a same-file cohesion weight, and placement adds an attraction between same-file windows. Fragments therefore stay together unless dependencies pull hard the other way.
- **File chip:** every fragment carries a chip, e.g. "builder.py · 2/5". Each split file gets its own colour (D14):
  - 10 slots, hue = slot × 36° in OKLCH;
  - stable per path by hash; the client shifts a slot when two split files in view collide;
  - muted at rest (L 0.78, C 0.06), so it reads as identity, not heat.
- **Selecting a file:** clicking the chip calls `GET …/fragments?path=`.
  - Every fragment's chip and outline go more saturated and brighter (L 0.88, C 0.17), and everything else dims.
  - Dotted fragment links join fragments that ended up apart.
  - The minimap marks every fragment in the bright colour, including off-screen ones.
  - `]f` / `[f` jump between fragments.

### 4.5 Code links (cv-link)

Resolution happens in tiers:

1. **Lexical, import-aware** (language packs). Ambiguous matches produce up to 3 candidates at confidence ≤ 0.5.
   - A ref is ambiguous when it does not resolve through its file, its imports or its class and its name is not bound by an import. A bare name looks for top-level definitions elsewhere; `x.name` and an unresolved `self.name` look for members.
   - Only same-language definitions count, and only when there are 1–6 of them (a name defined more often is not guessed); names under 3 characters, reserved names (dunders) and builtin calls are never guessed. Builtins come from the pack (`Conventions`): methods of builtin and standard-library types called on a receiver that is not `self`/`this` (`items.append`, `path.open`, `arr.push`, `v.unwrap`), calls through builtin globals or modules (`console`, `Math`, `std`) and builtin functions (`len`, `parseInt`). On the reference repo this cut candidates from 23,023 to 12,436. Two candidate samples were labelled, at different revisions of cv:
     - *Sample A, before builtin filtering* (cv change `ztywmomo`, pool of 22,976 candidates): 17 candidates out of the seed-2026 50-link sample plus a 30-candidate sample (seed 2027), 17/47 plausible (36%; 11/30 in the candidates-only part); 13 of its 30 misses were builtin or library methods. The labelled samples are not published (they quote private code); the breakdown is in the bullet *Measured* below.
     - *Sample B, with builtin filtering* (cv change `wspqmmkq` while it was being written, pool of 13,059 candidates, before `open` and `is_alive` joined the builtin list): 50 candidates, 20/50 plausible (40%; 0.50: 9/10, 0.45: 6/14, 0.40: 2/13, 0.35: 2/8, 0.25: 1/5). Most remaining misses are inherited methods matched to a sibling subclass.
     - Both were drawn from clones of the reference repo on 2026-10-19.
   - The nearest 3 by shared directory become candidates at 0.5 for a unique definition, 0.05 less per extra definition (at least 0.25). The canvas draws links under 0.6 faded and dashed.
   - Candidates are drawn but neither pull windows together in the layout nor count as dependencies (§4.8).
   - Measured on the reference repo (2026-10-19, sample A above: 47 hand-labelled candidates, before builtin filtering): 17 plausible (36%); by confidence 0.50: 5/7, 0.45: 6/17, 0.40: 3/10, 0.35: 2/3, 0.30: 1/6, 0.25: 0/4. The typical miss is a method of a builtin container or library object (`list.append`, `set.add`, `dict.update`, `Path.open`, `Pattern.match`, `Condition.wait`) guessed as a same-named project method; the rest are another class's override or a test double. Links ≥ 0.6 were 33/33 correct in the same 50-link sample, and 50/50 in a separate sample of links ≥ 0.6 only (a later trunk; WORKSTREAMS B4). With builtin filtering (sample B) 40% are plausible. Either way the faded, dashed drawing is the right weight for them.
2. **SCIP ingestion** (optional, precise, confidence 1.0). An `index.scip`, else `.cv/index.scip`, in the indexed revision's tree is decoded; each reference to a symbol defined in another window becomes a link with provenance `scip`, `is_implementation` relationships become `implement` links, and lexical candidates on the lines it covers give way. `GET /repo` reports `capabilities.scip` when the shown index ingested one.
3. **Human and agent feedback.** `rate_link` confirms or rejects a link and `declare_link` adds one. Both persist in review.db. A confirmed link that a later index no longer produces (its score fell under the draw threshold, or it is no longer among a ref's candidates) is re-added as it was rated.

Ports go on callsite lines: outgoing on the right edge, incoming on the left at the definition line.

### 4.6 Channel packs: IPC, config keys, deploy wiring (cv-channel + packs)

A channel pack implements `ChannelPack` (`docs/contracts/rust/channel.rs`). It owns three things:

- **Rules:** where endpoints appear, as data. There is one rules file per protocol and language, e.g. `packs/channels/http/python-fastapi.toml`, `…/typescript-fetch.toml`, `…/rust-axum.toml`, `…/cpp-httplib.toml`.
- **Normalization:** how keys are normalized.
- **Matching:** how producer and consumer keys match.

Language packs fold the captured key expressions, so a channel pack never parses code itself. Producers meet consumers at hubs. The Rust pack's sites (2026-10-19) fold literals, `format!`, `concat!` and constants (file constants in place, imported ones through `use`); an axum route is scoped by where it is built (inside `.nest(prefix, …)` the nest call, else its `let` binding, else its function), so `.nest("/p", api())` prefixes the routes of `fn api()` in the same file. Indexing this repository links the web client's `fetch("/api/v1/…")` calls to cv-server's axum routes (18 hubs, 60 links) and `CV_TOKEN` / `JJ_CONFIG` definers to their `std::env::var` readers. Shell `NAME=value command` prefixes define env variables too.

| Pack | Producers → hub → consumers | Matching |
|---|---|---|
| bus | `EventEnvelope(topic=…)` + `emit`, `publish(...)` → topic → `bus.on(pattern, handler)`, YAML `topic:` | mqtt (`*` = one segment, `#` = tail) |
| http | route decorators and routers (FastAPI, Express, axum) → method + path → clients (`fetch`, axios, `request()` wrappers with prefix, reqwest) | path template, `{}` holes, prefixes composed |
| env | `.env*`, Dockerfile `ENV`/`ARG`, compose `environment`, k8s `env`, CI `env` → `env:NAME` → `os.getenv`, `process.env`, `std::env::var`, `getenv`, **pydantic-settings `BaseSettings` fields** | exact; the prefix is honoured |
| deploy | Dockerfile `CMD`/`ENTRYPOINT`, compose `command`, CI `run:`, shell scripts → the module, script or test path they run | direct link, no hub |
| config fields | config keys (YAML, TOML, JSON) → `family#key` hub → code that reads them | heuristic score (below) |

**Config field usage** (pack P5, D15). The goal is "who reads this setting", answered by a sensible heuristic rather than an exact one.

- **Inputs.** `cv-lang-config` emits every key path with its line (`ConfigKeyFact`). The YAML reader is indent-aware and skips `|`/`>` block text. Language packs capture *read sites* of identifiers:

  | Read site | Base score |
  |---|---|
  | subscript or `get("key")`-style access with a string literal | 0.55 |
  | a bare string literal | 0.35 |
  | a model field such as a dataclass, pydantic or serde field of that name | 0.35 |
  | attribute access | 0.20 |

- **Score.** The base score above, plus:
  - up to 0.30 for key rarity (IDF over code files);
  - 0.30 when the code is in the same directory as the config file, 0.15 when it is in a sibling directory;
  - 0.30 when the code mentions the config's path, 0.25 when it mentions only its file name (a loader for that file family).

  Generic keys (`name`, `type`, `id`, `description`, `path`, …) score half. Links ≥ 0.80 are solid, 0.65–0.80 are faded with "?", and below 0.65 they are dropped. `rate_link` feedback overrides scores.

- **Context** (the default scoring; `CV_CONFIG_SCORING=reference` gives the reference score alone). Most wrong links in a hand-labelled sample were structural keys (`from`, `qty`, `words`, `size`) or same-named keys of a different node, read in unrelated code. So:
  - *node context:* a parent key of the config key (`entries` and `rate_tables` for `rate_tables.*.entries.weight`), or its singular/plural, named within 10 lines of the read site adds 0.15 (within 2 lines when the word is in > 30% of code files);
  - *generic keys* are derived from the config side: a key defined in ≥ 10% of config families (and ≥ 3) scores ×0.75. For this count a file name shared by several files is one family and a one-off name counts as its directory (`workflows/*.yaml`);
  - *no context, no link:* a link needs a parent key near the read site or, for a non-generic key, a same or sibling directory or a loader mention of a top-level key.

  On the reference repo (2026-10-18) this kept 20,869 of 22,691 keys with a reader (92%) and 113,115 of 143,368 links, and raised plausibility from 64% to 82% (WORKSTREAMS P5). Re-measured on a trunk 12 commits later (2026-10-19): 123,814 links, 21,190 keys with a reader (40.9% of 51,816 key paths), 3,175 hubs; a fresh hand-labelled sample is 40/50 plausible (80%). The same build gives the old numbers exactly on the old trunk, so the change is the repo's, not the scoring's.

- **Hubs.** Config files come in families: the reference repo has config families of 237, 91 and 53 files. There is one hub per (family, key), e.g. `config:node.yaml#capacity`. Each file defines into the hub, and each read site reads from it, so a generic loader appears once instead of once per file.
- **Measured on the reference repo** (appendix D):
  - 44% of 51,709 key paths get at least one reader.
  - Grouping by family turns 143,012 direct links into 27,168 hub edges through 3,475 hubs (5.3× fewer).
  - A 14-link random sample had 12 plausible readers. The two misses were generic keys (`trigger`, `from`) read in unrelated code.
  - Strong examples: a nested key read with `.get("…")` in the module named after its config file, and a per-node key read as `entry["node_id"]` in the node's own module.

**Dynamic stubs** (D11). If folding leaves no literal part, the port ends in an open dashed stub with a "?" and the hint, e.g. `⇢ ? topic built at runtime`. If folding leaves a partial pattern, e.g. `jobs.run.requested.{instance}`, it still matches hubs as a wildcard.

Findings on the reference repo shaped these packs:

- Topics are folded from constants (including imported ones), `{instance}` templates and the `scoped_topic()` transform (appendix B).
- Settings are read through `Settings(BaseSettings)` in `src/config.py`. A regex for `os.environ` alone links only 4 of the 53 variables defined in config and infra files, so the env pack must understand settings classes.

### 4.7 Change frequency (cv-churn)

History streams newest-first from jj-lib. Each commit is weighted by recency with a 90-day half-life, and mass refactors (> 50 files) are down-weighted by 1/log₂(1 + files). Each file's rate gets a Wilson 95% interval computed on the effective sample size.

**Grades are quantiles of change mass.** Sort files by rate, highest first, and accumulate their share of the total:

| Grade | Files |
|---|---|
| Hot | the files holding the first 50% of change mass |
| Warm | the next 25% |
| Mild | the next 15% |
| Cool | the rest with any change |
| Frozen | no change in the scanned history |

The cut points are configurable per repo in `.cv/grades.toml`, read from the indexed revision's tree (like `.cv/lanes.toml`); every key is optional:

```toml
[heat]            # shares of change mass, hottest first
hot = 0.50
warm = 0.25
mild = 0.15       # Cool is the rest with any change

[connectedness]   # shares of the windows with dependents, most connected first (§4.8)
core = 0.10
high = 0.15
medium = 0.25     # Low is the rest
```

A table whose shares are not each in (0, 1] or sum to more than 1, that has an unknown key (likely a typo, e.g. `hott`) or a wrong type, is ignored as a whole with a warning (that table's defaults stay; the other table still applies). Bad TOML or an unknown table keeps every default. The same cut points grade clusters (§4.10).

**Stopping rule.** Check every 25 commits. Stop when all three hold:

- at least 50 commits have been scanned;
- ≥ 90% of the change mass sits in settled files, where a file is settled when its interval lies inside one band;
- every band edge moved < 10% in each of the last two batches.

Also stop at 2,000 commits, at 730 days, or when history runs out. The UI shows the result, e.g. "graded over 340 commits · 93% settled".

**Where the rule cannot stop: the reference repo** (measured 2026-10-19). Its last 2,000 commits span 8.7 days, so every weight is 0.94–1.0 and the effective sample is the commit count; 5,635 files share the change mass and most have few touches (1,873 files have 1, 746 have 2). The walk stops at the 2,000 cap with 36% of the mass settled: the Hot files (51% of the mass) are 73% settled, Warm, Mild and Cool none. The limit is statistical, not the cap: a Warm file at 5 touches in 2,000 (rate 0.0025) has a Wilson interval of [0.001, 0.005], wider than the Warm band [0.0016, 0.0041). At today's rates the settled mass would reach 41% at 4k commits, 56% at 16k, 72% at 32k and level off near 80% (files sitting on a band edge never settle), so 90% is out of reach at any history length, and the whole history (3,922 commits back to 2026-03-24) settles 38%. A half-life relative to the observed span would not help: it can only lower the effective sample. The cap and the rule stay as designed; the consequence is that on such a repo only Hot files move their clusters when heat changes (§4.9 "a cluster moves only when one of its grades settles"), and the UI says so ("graded over 2000 commits · 36% settled"). Churn took 1.1–3.0 s for the 2,000 commits and 1.8–2.0 s for the whole history (§9: < 10 s).

**Range mode.** Placement keeps the full view's historical heat columns (D16). In-range activity is shown on the windows instead:

- a "2 of 3 commits" badge;
- owner stripes per line;
- in the Hotspots panel, the option to rank by in-range touches.

### 4.8 Connectedness (cv-link)

**Choice:** *dependents within 2 hops*, i.e. the windows that depend on this one directly or through one intermediary.

- Dependencies are typed: calls, inheritance, imports, bus and HTTP hubs, config keys. Consumers depend on producers.
  - Code links count from confidence 0.6 (lexical candidates do not; SCIP links do); channel links from 0.8.
  - An import makes the window holding the import line depend on the imported member's window, or on the imported file's first window for a module or star import.
- It is computed per window. A code-lane window counts code-lane dependents only; a test-lane window counts test-lane dependents; config and infra windows count dependents from every lane but tests.
- Test dependents are shown separately ("covered by 12 tests": test files with a window depending on it directly) so a heavily tested module doesn't look central.
- Details: `reach` counts the files depending on the window's file transitively (tests excluded), and `cycle` is the size of the import cycle (strongly connected component of the file import graph) the file sits in.

**Grades** are rank quantiles among windows with at least one dependent:

| Grade | Rank among windows with dependents |
|---|---|
| Core | top 10% |
| High | next 15% |
| Medium | next 25% |
| Low | the rest |
| Isolated | no dependents |

The cut points are configurable (D6, `.cv/grades.toml`, §4.7). Fan-in and fan-out appear as badges.

**Measured on the reference repo** (2026-10-18, `cv index`, code lane, 9,553 windows), before and after moving from the file import graph to typed window links:

| | Core | High | Medium | Low | Isolated | 2-hop max · median · distinct | windows reaching ≥ ½ of the lane | with direct tests |
|---|---|---|---|---|---|---|---|---|
| file import graph, the file's grade on each window | 1,903 | 1,729 | 2,341 | 2,416 | 1,164 | 570 · 14 · 153 | 0 | 8,853 |
| typed links per window | 784 | 1,161 | 2,197 | 3,472 | 1,939 | 979 · 3 · 180 | 0 | 3,641 |

Grades now follow the windows' own dependents, so the rank quantiles hold (Core 10.3% and High 15.2% of the 7,614 windows with dependents) instead of every window inheriting its file's grade. The 2-hop count still does not saturate. The most depended-on windows are the settings module `src/config.py` (979), an event-bus error-reporting helper (716) and three core domain modules (494, 493, 450). The largest import cycle has 278 files (appendix C counted 270 from Python and relative TS imports only). Links plus connectedness took 0.25–0.5 s (1.3 s on 2026-10-19 with the config-field links, still inside §9's 2 s). Re-measured 2026-10-19 (the reference repo's trunk at the time, 9,562 code windows): Core 785 · High 1,161 · Medium 2,201 · Low 3,474 · Isolated 1,941; 2-hop max 979, median 3, 178 distinct; 3,644 with direct tests; 22,984 lexical candidates (not dependencies).

**Appendix C stays reproducible.** Appendix C measured files on the import graph; the product measures windows over typed links. These are different metrics, so the study is reproduced on its own definition: `GET /views/:id/metrics?level=file&basis=imports` computes the appendix's file-level numbers from cv's own resolved imports (`cv_link::conn::file_import_connectedness`, the same 2-hop/test/fan-in/grade code run with files as nodes). On the reference repo (2026-10-19) it matches `axes.py` on all 16 connectedness rows, most exactly: 2-hop max 569 / 569, Core 102 / 102, High 155 / 155, Medium 274 / 274, Low 484 / 483, Isolated 472 / 473, fan-in max 742 / 742, files with a direct test 1,056 / 1,056, quadrants 73 · 134 · 184 · 1,096 exact, ρ 0.308 / 0.309 (`scripts/acceptance/appendix_c.py`, 27/27 PASS); re-run on a later trunk after the D15 lane order, 27/27 again (WORKSTREAMS B4). Lane counts are compared with `axes.py`'s rules tried in the D15 order: appendix C's own order (test and docs directories before config) is the study's, and the product departs from it on purpose (§4.3).

The product's per-file view (`level=file`, the maximum over a file's windows of the typed-link metric) is reported next to it as information, not compared: Core 267, High 214, Medium 256, Low 352, Isolated 398; 2-hop max 979; 891 files with a direct test; quadrants hot+central 134, hot+peripheral 73, cold+central 347, cold+peripheral 933; ρ with heat 0.39. Two things move these numbers, both by design:

- *Typed links add dependents the import graph lacks:* calls into a module, bus/HTTP/env/config hubs (consumers depend on producers). `src/config.py` gains every settings reader (979 vs 569), and 74 fewer files are Isolated.
- *A file takes its most connected window's value:* one central window lifts a large file into Core, so the file-level aggregate is no longer a rank quantile over files (267 Core files instead of 10%). Window grades themselves keep the quantiles (above). Tests count only windows that depend on the window directly (calls and member imports), not a module import anywhere in the test file, hence 891 instead of 1,056.

**Why not transitive reach.** Measured on the reference repo (appendix C):

- one import cycle of 270 files makes transitive reach saturate: 26% of code files reach at least half the code lane;
- tests inflate it further;
- the top-5% list by transitive reach shares only 1 of 74 files with the top-5% by heat, because every top-reach file sits in that cycle.

The 2-hop measure has none of these problems: 0% of files saturate, 154 distinct values. It still tells the axes apart (Spearman ρ with heat = 0.30) and finds the files you'd expect at the top: the settings module `src/config.py`, the event payload and event type modules, and two core domain modules. Transitive reach stays available as a detail ("in an import cycle of 270 files").

### 4.9 Emphasizing the two axes on the canvas

**The data says both axes are needed.** Across the reference repo's 1,481 code files, heat and connectedness correlate only weakly (ρ = 0.30). Among the 199 hot files, 126 are not central. Among the 253 central files, 180 are cold. Only 73 are both. Sorting by either axis alone would hide most of the other.

**Channel allocation.** Each kind of information gets exactly one visual channel, so they never compete:

| Information | Channel |
|---|---|
| change frequency | **hue** (one continuous heat ramp, slate → sand → amber → muted coral, on the frame strip, silhouette and L0 fill; a window sits within its grade's fifth of the ramp by its rate between the grade's bands, a cluster by `Cluster.heatLevel`) |
| connectedness | **elevation** (drop shadow and frame weight; core code looks raised) |
| file role | **lane texture** and a label rail |
| file identity | a small chip with a pastel stripe |
| both axes together | **position** in the district grid |

**District grid** (default layout, D8):

- Lanes stack top to bottom: infra, config, code, tests, docs.
- **Columns** are heat grades, Hot · Warm · Mild · Cool · Frozen from left to right. They are shared and aligned across every lane, so a hot Dockerfile sits above hot code.
- **Rows** are connectedness grades, Core · High · Medium · Low · Isolated from top to bottom, per lane.
- Each dependency cluster goes into the cell of its aggregate grades (quantiles over clusters, §4.10). Inside the cell, its windows keep the layered dependency layout (callers left of callees).
- Empty cells collapse: an empty row has no height, and a heat column empty in every lane has no width (the columns stay aligned across lanes).
- The camera starts at code × Hot × Core: "review first".
- A cluster moves only when one of its grades *settles* in a new band, so the map does not jitter while statistics refine.
- On the reference repo the code lane's Frozen column holds 69% of files, which keeps the active part of the grid compact at the top-left.

**Always-on encodings.**

- Heat: header strip colour and silhouette saturation.
- Connectedness: elevation, plus "↧ 85" (dependents within 2 hops) and "↥ 12" (fan-out) badges.
- A tiny 5×5 district glyph in each header marks its cell.
- The top 5% by risk get a double accent outline. Risk is the geometric mean of the heat and connectedness percentiles.

**Lenses** change emphasis only, never layout. The grid already places clusters by heat and connectedness, so a lens shows what position cannot.
- **Colour:** each lens recolours what it is about, cards from L1 and cluster fills at L0.
- **Fading:** what a lens leaves out turns an even quiet grey, whatever its heat.
- **Links:** links and highways whose ends the lens fades step back with them, so the emphasis is not drawn over.

| Lens | Effect |
|---|---|
| Heat | the continuous heat ramp, with a glow growing over warm and hot code; below Mild fades |
| Depends | direction of dependence: accent for code depended on more than it depends (fan-in > fan-out; at L0 the solid highways in and out, test sources left out), teal for the reverse, stronger the more lopsided; isolated windows (and clusters with no solid links) fade; elevation exaggerated. Link width by coupling was dropped: links are keyed per window pair and kind, so the "coupling" was constant. |
| Hotspots | review-first windows at full strength (L0: the share of a cluster's windows that are review first, `Cluster.review`, as a double ring) |
| Co-change | files and clusters that change together in history (`cochange` file pairs without a static link, `cochangeClusters` for every cluster pair, ranked by the Wilson lower bound on jaccard so 3 of 3 commits rank below 30 of 40). Pointing at a cluster or file lights its partners by strength, with an arc to each (dashed where no static link explains the coupling) and labels; with nothing pointed at, L0 shows each cluster's strongest coupling and the strongest unexplained pairs as faint arcs. Resting on a focus (600 ms, a cue ring fills) or pressing G **gathers** its partners into a tray beside it: proxy cards fly from their places, which stay covered with a dashed outline and a thread to their card, while a scrim steps the rest of the map back. Nothing on the map is rebuilt or moved. A click on a card flies there; Esc, G or leaving the tray releases it. |

**Risk** (review first) uses heat and connectedness percentiles **within the window's lane**. A window that never changes, or that nothing depends on, has risk 0. Before this, tied zeros took a mid rank, which filled quiet lanes, docs especially, with untouched or isolated "review first" windows.

**Hotspots panel.** A scatter of clusters or files: x = connectedness percentile, y = heat percentile. Quadrants are named Review first, Active leaves, Foundations and Quiet periphery. Dots are shaped by lane, a brush filters the canvas, and a ranked list sits below. MCP `get_hotspots` returns the same list to agents.

**Layouts considered and rejected:**

| Layout | Why not |
|---|---|
| v0.1 radial heat map | connectedness only implicit |
| continuous quadrant scatter | both axes exact, but scatters dependency clusters, stretches edges and jitters as metrics refine |
| terrain overlay (contours for connectedness, thermal colour for heat) | layout unchanged and pretty, but hard to read at a glance |
| "dependency" layout (clusters only, no axes) | an early contract value (`ViewLayout`) that was never specified or built: it gives up both axes, which is the point of the map, and the dependency structure already lives inside each cell. Dropped from the contract; POST /views answers 400 `invalid` for any `options.layout` but "districts". |

The grid gives the scatter's reading with the map's stability. The scatter survives as the Hotspots panel.

### 4.10 Clusters and placement (cv-layout)

1. **Clustering graph over windows.** Within a lane, the edge weight is
   1.0·log₂(1+calls) + 2.0·inherits + 1.5·channel links + 0.5·imports + 1.0·co-change Jaccard + cohesion,
   where cohesion links each pair of consecutive fragments of the same file.
2. **Communities.** Leiden, run in-house over petgraph, deterministic, two levels. A label from a human or agent carries over to the new cluster with the highest member Jaccard (≥ 0.6).
3. **Cell placement.** Each cluster's aggregate grades are quantiles *over clusters*, per lane, so D6 (quantiles, never fixed limits) holds at the cluster level too:
   - **heat:** clusters sorted by change mass per line; Hot holds the clusters with the first 50% of the lane's change mass, Warm the next 25%, Mild 15%, Cool the rest with any change, Frozen none;
   - **connectedness:** rank quantile of each cluster's most-depended-on window (Core top 10%, High 15%, Medium 25%, Low the rest, Isolated none).

   The earlier rule, change-mass-weighted heat and the *maximum* connectedness grade of the windows, was measured and dropped: with clusters of up to 36 windows, one hot or one central file lifts its whole cluster into Hot or Core, so most clusters crowd the first column and row and the grid stops separating anything. Window and file grades are unchanged and still drive the frame colour, elevation and badges.

   Within a cell, clusters are packed by mutual link weight, and same-file attraction pulls clusters that share a file toward each other.
4. **Inside a cluster.** Sugiyama layering, callers left of callees, with ports at line offsets and hotter windows first within a layer.
5. **Range views reuse the map** (D16). A range view takes the placement of the snapshot view of its tip (`ViewRef.sharedPlacement`): same lanes, columns, rows and cluster positions.
   - Hunk windows sit where their symbol's window sits in the full view, and minimized context pills sit at their own full-view spots.
   - Unchanged clusters are not drawn; faint outlines at L0/L1 keep orientation.
   - "Fit" frames only what changed, and the minimap shows the whole grid with the changed clusters lit.
6. **Stability.**
   - Positions are reused by `WindowKey` and pins are honoured.
   - Stored cluster labels and cluster pins (review.db) follow drift: after every full index each matched record takes the members and lane of the cluster it matched (Jaccard ≥ 0.6, own lane first, then any lane), so a record survives any cumulative drift that each index sees ≥ 0.6 of. An unmatched record is kept and dropped after 20 consecutive full indexes without a match.
   - Window pins follow file renames that the churn walk saw (old path → path at the head). A renamed symbol changes the `WindowKey`; such a pin is not carried and is dropped after 20 misses.
   - Moves happen only on settled grade changes.
   - Inside a continuing cluster, surviving windows keep their offsets. New windows take free space next to their file's other fragments or a linked window; a resized window shifts only when it would overlap, to the nearest free spot. A cluster that no longer fits its spot takes the nearest free spot of its cell, inside the row's height when there is room, so rows below do not shift. An in-place refresh (no re-layout) keeps every window where it is, except that a window that grew or is new and now overlaps another takes the nearest free spot in its cluster.
   - A full re-layout happens only on request.
   - Acceptance check: one changed file moves < 5% of windows. Measured on the reference repo (2026-10-19, 28,710 windows, a function appended to one file and indexed with the previous placement): the edit itself moved no window; 389 windows (1.4%) moved, all in the 15 clusters that changed cell (a settled heat grade or a connectedness grade) as the commit window advanced.

## 5. Front-end

**Zoom tiers.** Tiers are chosen by on-screen line height h = 18·zoom px, with ±10% hysteresis. The data-tier column refers to the T0–T3 tiers listed after this table.

| Tier | Zoom | Drawn | Data tier |
|---|---|---|---|
| L0 Galaxy | < 0.03 | lane bands, column and row guides, clusters with heat and elevation, bundled edges, hubs | T0 |
| L1 Map | 0.03–0.09 | window tiles and titles, file chips, edges without labels | T1 |
| L2 Outline | 0.09–0.5 | silhouettes, headings, port pills, dynamic stubs, badges, district glyphs | T2 |
| L3 Code | ≥ 0.5 | text, tokens, inline threads, fade hints | T3 |
| Active | — | CodeMirror and the save bar | — |

**Loading.**

- **T0:** the bootstrap, which now carries lane bands, column guides and row guides.
- **T1:** tiles of the district grid, served from the in-memory view (its grid index); only a saved view that is not loaded (an id from before a restart) is answered from index.db's R*Tree (§3 *Storage*).
- **T2:** batches of at most 200 windows.
- **T3:** immutable blob text.

Requests are prioritized by distance to the predicted viewport, at most 6 are in flight, and those that leave the predicted region are cancelled. Caches are LRU and decoding happens in a worker. The render loop never waits on the network.

**Rendering.**

- PixiJS draws bands, textures, clusters, frames, elevation, silhouettes, edges, hubs and L3 code text.
- CodeMirror 6 (DOM) takes over the one activated window, for selection and editing.
- Lane labels and row labels stick to the left screen edge while panning; heat-column labels stick to the top.

**Rendering performance** (F1, measured with `web/scripts/bench.mjs`, 9 scenarios, headless Chrome on an RTX 3090 Ti). Without throttling these rules keep pan and zoom at 60 fps: 59.9–60 fps on the 20k-window mock and on cv serve with the reference repo (28,713 windows). With 4× CPU throttling (`--cpu 4`) they come close but do not hold 60 fps everywhere. Measured 2026-10-19 with other agents' builds on the machine (load average 39 → 14 over the runs):

- cv-mock 20k view: 58.3–60 fps (pan 60, zoom 59.5–59.8, fling 59.3–59.7, dives 58.3), 0–7 frames over 33 ms per scenario.
- cv serve on a clone of the reference repo: 55.3–60 fps (fling-l1 55.3, dives 56.1, zoom-l0-l3 57.1, the rest 58.3–60), 0–10 frames over 33 ms per scenario.
- The audit's run on cv serve measured 55.6–59.3 fps.

The rules:

- *Camera moves never touch the scene graph.* Two world containers get one transform per frame. Everything else is static geometry in per-chunk render groups (16,384 wu cells), and frames render only when something changed.
- *All rebuilds are budgeted, within a build too.* Chunk parts (frames, short edges, titles, header labels, silhouettes, port pills) and L3 text rebuild nearest the view first under a frame budget: 10 ms when idle and 4 ms (3 ms for text) while moving. A part build is resumable (`Chunk.step`, a generator per part): it works in units of one window (or 24 links, or 512 silhouette lines), stops before a unit that would not fit before the frame's deadline, and resumes next frame; marking the part dirty again restarts it. Highway pages also end at the deadline. Old geometry stays on screen until its replacement is ready, so nothing flickers.
  - *Measured at 4× CPU before resumable builds* (every part build during the 9 scenarios on cv serve with the reference repo, 2026-10-19, load average 110 → 41): median 0.7–2.0 ms per part. Over 4 ms: `ports` 26% (p95 9.3 ms, max 14.8 ms), `frames` 11% (p95 5.7, max 23.3), `edges` 10% (p95 5.0, max 19.0), `heads` 14%, `sil` 3%, `titles` 2%. On cv-mock at load ≈ 35, `ports` reached p95 5.2 ms and max 7.5 ms (14% over 4 ms). The audit saw a 25 ms `ports` build on cv serve. Under heavy load (load average 100+), single builds reached 30–75 ms.
  - *Why `ports` is the worst:* a chunk's pills are rebuilt whenever an outline batch for its cell arrives (each batch marks `sil` and `ports` dirty), so one dense chunk is rebuilt 4–6 times while its outlines stream in. Warm and unthrottled, one build of a 107-window chunk with 284 pills takes 1.0 ms; the slow ones are those repeats at 4× CPU on a loaded machine.
  - Fixed by resumable builds (above) and by coalescing: outline batches mark a cell's `sil` and `ports` dirty once none of its windows' outlines are in flight, or 120 ms after the first unapplied batch (`OUTLINE_COALESCE_MS`), not once per batch. Not yet re-measured on a quiet machine.
- *No per-shape objects.* Rectangles (frames, shadows, chips, badges, silhouettes) are typed-array quads coloured through a palette texture. The palette is an atlas of 16×16 cells per colour holding a quarter disc, ring or soft falloff, so rounded cards, pills and soft shadows are a few extra quads in the same mesh (no filters, no per-card textures). Labels and code text are typed-array glyph quads over a mipmapped glyph atlas, one mesh per colour. Pixi `Graphics` is used only for hairlines.
- *Edges are 1 px hairlines at every zoom* (`pixelLine`), so tier changes never rebuild them, and curves are pre-flattened. Edges longer than a chunk cell live in global append-only pages; otherwise every chunk they leave would count as visible and culling would fail.
- *Cost is paid where it is measured.* Graphics are tessellated inside the build budget, not lazily at first render. Shaders, glyph atlases and the palette warm up during startup and after a theme change.
- *Decoding stays off the main thread.* The worker parses tiles, streams windows in slices of 250 (one task each) and ships edges as a transferable `Float64Array` with interned ids.
- Tiles carry port-to-port edge geometry (`TileResponse.edges`, additive to v1), so edges draw before T2 arrives and stay consistent across tile boundaries.

**Edges.** Styles by kind:

| Kind | Style |
|---|---|
| call | solid |
| inherit | dashed with a hollow triangle |
| event | teal, through a topic diamond |
| HTTP | amber, through an endpoint pill |
| env | through a key hub on the lane boundary |
| deploy | dotted, crossing lanes |
| config | dotted |
| dynamic | open stub with "?" |
| low confidence | faded with "?" |

Cross-lane edges are bundled per cluster pair, and hubs for cross-lane channels sit on the lane boundary.

## 6. Review and editing (cv-review)

**Threads.**

- Lifecycle: open → in-progress → addressed → resolved, plus outdated and wontfix.
- Anchors point at change ids. After each op the thread is re-anchored through the evolog and diff, or marked `outdated` with its last good commit.
  - Each rewrite on the evolog path from the anchor's commit is diffed in turn. A divergent change follows the commit whose evolog holds the anchor's commit; ties go to the newest commit, then the lowest id.
  - An edit that answers a thread (`origin.thread`, with any number of patches or whole content) moves the anchor onto the lines that replaced or now hold the anchored ones, in the commit that holds the edit. The answered thread stays current through later ops.
  - Setting an outdated thread back to another status re-anchors it on its change's current commit.
- Suggestions are ```suggestion blocks that replace the anchored lines.
- Threads stay in review.db (D5).

**Where an edit goes** (D10: amend whenever possible).

| Editing in | Default | Alternatives |
|---|---|---|
| a workspace's `@` | write the file in that workspace | — |
| mutable commit C | **amend C** | insert after C · branch from C · amend but keep descendants' content |
| immutable commit C | **branch from C** (nothing is rewritten) | — |

*Immutable C, as built:* the server does not pick the mode. `edit` rejects any mode but `branch` on an immutable target with `immutable` (`cv-review` `prepare`; `cv-vcs` `validate` refuses it again). The clients choose branch: the save bar reads the change's `immutable` flag from `/revisions` and offers only "branch", and if a preview still comes back `immutable` it switches to branch and previews again (`Review.tsx`); MCP `edit_code` without an explicit `mode` retries the rejected request once with `mode: "branch"` (`cv-mcp` `server.rs`).
| range view (tip) | **absorb** into the commits that own the lines; leftovers amend the tip | amend a chosen commit · new change on top |
| applying a suggestion | **amend** the change the thread anchors to | insert after it |

**Mutation protocol.**

1. **Validate.** The target must be mutable (else `immutable`, see above). If a file's `baseBlob` is no longer its content at the target, the edit is rebased onto the current content with a three-way merge (base = `baseBlob`, ours = the edit, theirs = now); it fails with `stale-base` only when the merge overlaps, the file is now absent or conflicted, or `baseBlob` is unknown. Work out the *affected workspaces*: those whose `@` descends from the target. If an affected workspace has an attached agent, warn.
2. **Preview, exactly.** Open a jj-lib transaction, rewrite the commit, rebase its descendants and collect new conflicts, then **drop the transaction**. Nothing is written.
3. **Apply,** one mutation at a time:
   - re-check: if the operation moved on since the preview, the plan is previewed again and must rewrite the same changes, create the same changes, leave the same new conflicts and touch the same workspaces, else `stale-base` ("preview it again");
   - dry-run the plan in a throwaway transaction to find the workspaces whose `@` it rewrites, and snapshot those;
   - run the plan again in a new transaction on the repo as the snapshots left it, and commit that (the preview's transaction was dropped; nothing from it is reused);
   - update those workspaces' checkouts. If jj-lib fails before the commit, the plan is retried through the jj CLI.

   If a workspace's `@` still turns divergent, its edits are not lost: they sit in the divergent sibling. Emit `workspace.recovered`.
4. **Report** new conflicts (`conflict_seen`, `edit.updated`, `conflicts.changed`). Undo = revert our op, never the last op blindly. Undo is refused (409 `undo-blocked`) when the revert would leave a change divergent or create a conflict that was not there at the edit's `opBefore`; undoing a conflict resolution brings that conflict back (reopened with its first sighting) and is allowed.

The CLI recipes in appendix A are the oracle: B1's tests run each mutation both through jj-lib and through the CLI on golden repos and compare the resulting trees.

**Conflicts** get × in the history panel and a conflict window on the canvas. Actions: take a side, edit, or ask the agent. Asking the agent files a task thread and sends it.

**Event log.** Every server event is appended to review.db's `event_log` (seq = cursor) before it is sent; re-anchoring after an op reports itself as `index.progress` job `reanchor`. Retention: the newest 100,000 events, and nothing older than 14 days beyond the newest 1,000; pruned at startup and every 1,000 appends. Seqs are never reused. A client whose cursor is older than the oldest kept event cannot replay without a hole, so the WS `hello` (on `?cursor=`) and GET /events answer `resync: true`; the UI then reloads its view, threads and conflicts, and MCP `wait_for_events` passes the flag on.

## 7. History, ranges and workspaces

**History panel.**

- Revset: the user's `revsets.log` plus `working_copies()`.
- Lanes are computed on the server.
- Each row shows: glyph, change id, description, bookmarks, workspace chips, thread count and "changed since review".
- "Changed since review" opens an interdiff: `jj interdiff --from <reviewed commit> --to <change>`.

**Graph panel.** The same graph as a side panel docked left of the map (top bar "Graph", `g`, except in the co-change lens, where `g` gathers), for working with the repository's shape while looking at the code.

- Edges are drawn from the server's lanes: an edge leaves its row into the lane it leads to, runs down that lane (reserved for the parent until its row) and bends into the parent's lane. Indirect edges and parents outside the rows are dashed.
- **What the map shows.** Three things, named the same in the Graph panel and the History tab: **State at commit** (a snapshot at that change), **Diff of commit** (a range view of that one change: its diff against its parent) and **Diff of range** (a chain as `oldest::newest`, cumulative) or **Diff of commits** (unrelated changes, per commit, `allowNonChain`). The panel's **Map: Repo | Diff** switch shows which is on: Diff follows the selection (double-click a change = Diff of commit), Repo goes back to the snapshot; State at commit sets Repo without following. The History tab's **Open in Graph** opens this panel with its picked changes selected (or says they are outside the graph's revset). The right-hand side panel can be hidden (top bar "Side panel", » in its tabs; remembered per browser) for a wider map; narrow screens keep it as a drawer. One view is opened at a time and the latest selection wins; a view already built is cached by the server, so going back is cheap. The first view at a new tip costs one snapshot index of that revision (parse cache shared; about 1 s on a 600-file repo).
- **Operations** (`POST /graph:op`, model.ts `GraphOp`), each one jj operation through the mutation queue, previewed exactly first (`dryRun`) and undone with `jj op revert` (`POST /graph:undo`): set (create or move; backwards or sideways only when allowed) and delete a bookmark, land, rebase `-r | -s | -b`, describe, abandon, `new --no-edit` (one or several parents), commit and squash. When the plan rewrites a workspace's `@`, that workspace is snapshotted first and the commits the plan names follow their rewrite (a squash of `@` takes the edits on disk). Only commit gives a workspace a new `@`, and that one sits on the old `@` with the same files, so nothing on disk changes; no operation checks another commit out.
- **Commit** is `jj commit` in a workspace (strip button when its `@` has changes, a message or unsnapshotted files; action bar on a selected `@`): the workspace is snapshotted first, `@` gets the message, a new empty `@` starts on top (every workspace on that `@` moves, as in jj), and an optional bookmark is created or moved forward to the committed change (a sideways move is refused: use Bookmark…). The dialog suggests the bookmark on `@`'s parent, the trunk first; a bookmark move onto a working copy offers "Commit … and move … to it" instead.
- **Land** brings a stack onto the trunk bookmark (`GET /refs` suggests it: the local bookmark at `trunk()`, else main, master or trunk). Source: a workspace (its `@`, or `@-` when `@` is an empty undescribed scratch commit, which is then carried along), a bookmark (optionally deleted after) or a commit. Strategies: fast-forward (refused unless the trunk is an ancestor), rebase (`jj rebase -b head -d trunk`, then move the bookmark) and merge (`jj new --no-edit trunk head`, the carried `@` rebased onto the merge, then move the bookmark). Any strategy fast-forwards when it can. Workspaces whose `@` is rewritten get their files checked out, as for edits.
- **Bookmarks.** A strip lists every local bookmark (trunk first). Clicking one, there or on a row, opens its moves: to each workspace's head (its `@`, or `@-` for an empty undescribed `@`, as Land picks it; when that row is outside the revset, a fast-forward land the server resolves), to the selected change, to a change clicked next, land on the trunk, delete. Each opens the previewed dialog; a move onto a working copy with changes warns that the bookmark will follow its edits. Dragging a bookmark onto a change does the same; dragging a change onto another rebases it (`-s` by default).
- **Squash** is `jj squash --from <source> --into <dest>` (default: its only parent; "Pick another change…" takes the next click): the source is abandoned when emptied (a working copy gets a new empty `@` on the destination), descendants rebase. Without a message the descriptions are combined as jj's editor would leave them (destination, blank line, source); the dialog prefills that text. The CLI fallback always passes `-m`.
- **Operations** (`GET /ops`, `GET /ops/:op`, `POST /ops:restore`): the operation log, newest first along all parents, snapshots hidden by default. An operation's detail is the change set against its first parent (changed and abandoned changes, local bookmark moves). Undo is `jj op revert` (`/graph:undo`, refused when it would leave divergence or conflicts); Restore is `jj op restore` (the repo and remote-tracking portions; git's own refs stay current), previewed, humans only, and itself undoable.
- **Fetch and push** (`POST /git:fetch`, `POST /git:push`) go through the jj CLI rather than jj-lib, so the user's git setup applies as in a terminal (remotes, credential helpers, the ssh agent, push safety checks), inside the mutation queue: no controlling terminal (`setsid`), `GIT_TERMINAL_PROMPT=0`, killed with its process group after 180 s. Humans only; bookmark and remote names are checked before jj runs. A push is previewed with `--dry-run`; it is not undoable from the UI (the remote keeps it). `GET /refs` lists the remotes (asked of the CLI: jj-lib keeps the git config it read at open) and, per tracked remote bookmark with one target on each side, `ahead`/`behind` counts (capped at 1000), shown as `↑n`/`↓n`.
- **Expand** (header button, `G`) lays the panel over the map and side bar, which keep running underneath, with full dates and commit ids per row; State at commit, a diff or a range docks it again. Git mode keeps the commit details beside it.
- Git-colocated repos: bookmark changes are exported to git branches, as the jj CLI does.

**Git mode.** `cv serve` on a plain git repository (no `.jj` at or above it) does not exit: it serves the history read-only from the `git` CLI (argv only; no pager, prompts, signature checks or fsmonitor; client revisions may not be git options except `--all`, `--branches`, `--tags`, `--remotes`, and come after `--end-of-options`). `GET /setup` says `vcs: "git"`; `/revisions` (lanes from the same assigner; commit ids stand in for change ids; branches as bookmarks, remote ones as `name@remote`; worktrees as workspaces, the main one "HEAD"), `/refs`, `/workspaces` (`[]`) and `/git/commits/:commit` answer; every other route is 409 `jj-required`. The UI shows a setup page beside the read-only graph. **Initialise jj** (`POST /setup:init-jj`, humans only, `confirm: true`) runs `jj git init --colocate` at the root; the server then replaces itself (`exec`, same arguments; `CV_RESTARTED` keeps start.sh from opening a second tab) with the full server, and the page polls `/setup` and reloads. A `.jj` that appears from a terminal triggers the same restart once the repository opens.

**Range dialog.**

- A revset field with live validation.
- Presets: my stack, mine, one per agent workspace, last 10, single change.
- A commit picker, with the gap check `(roots(R)::heads(R)) ~ R`.
- A fileset filter.
- Options: cumulative or per-commit diff, context hops, context display (minimized by default), ignore whitespace.
- Saved ranges.

**Range review** (D16).

- **Same map as the full view.** The view reuses the full view's map at the range tip: same lanes, heat columns, connectedness rows and cluster positions.
- **Diff-focused windows.** Each window is one hunk cluster.
- **Connected code stays reachable.** Unchanged connected functions are minimized pills until clicked.
- **Activity shows as badges, not position.** In-range activity appears as "2 of 3 commits" badges and owner stripes.

A range must be a chain: one root and one head. The base is `roots(R)-`, and the view shows `diff(base, head)`.

**Workspaces** (D12). A Workspaces tab lists one card per jj workspace, with:

- path and its `@` change;
- stack (`trunk()..<name>@`), with changes, files and conflicts;
- unsnapshotted files;
- attached agent and its activity;
- the last operation;
- **overlap warnings** when two stacks change the same files, counted on each stack's own commits (history both share is no overlap, even with no remote, where `trunk()` is `root()`);
- a **stale** flag when an operation run elsewhere left the workspace stale or its `@` divergent (§4.1).

Card actions:

- **Open @** shows a snapshot of that workspace with its live files (`rev: "<name>@"`, `options.workspace`).
- **Review stack** opens a range view.
- **Compare**, on an overlap warning, opens a per-commit range view of both stacks, filtered to the shared files.

**Other revisions.** The top bar's revision chip (and "State at commit" in the History tab and the Graph panel) opens a snapshot at any revision that resolves to one commit, or at another workspace's `@`. The editor there targets that commit, so §6's "mutable commit C" rows (amend, insert after, branch, keep descendants) and "immutable commit C → branch" apply. A snapshot spec's `fileset`, `liveWorkingCopy: false` and `workspace` each make a view of their own; bad values are a 400.

Windows touched by another workspace's stack carry an "also changed in …" chip (`WindowHeader.flags.alsoChangedIn`), in snapshot views and on range views' hunk windows. The workspace `cv serve` runs in is marked "you" (`WorkspaceSummary.serving`).

**Agent binding.** An MCP session works in a workspace without binding one: the served workspace, or for the stdio shim the jj workspace holding its cwd (`cv mcp --workspace tax`, the `X-CV-Workspace` header over HTTP, or `bind_workspace` choose another). Attention markers then read `claude@tax`, edits are attributed per workspace, and the agent's default revsets point at its own stack.

## 8. Agent interface (cv-mcp)

The tools are listed in `docs/contracts/mcp.ts`, grouped into reader, reviewer, editor and full profiles. The editor profile is kept small (15 tools) for an agent that addresses review threads; graph, metrics, tours and binding are in reviewer and full. Outputs are slimmed for a model's context (compact JSON, short ids, code as numbered text) while the HTTP API stays as the UI uses it. New in v0.2:

- `get_hotspots`: a ranked review-first list per lane;
- `get_metrics`: heat, connectedness and co-change for a window or file;
- `list_workspaces` and `bind_workspace`.

The principles are unchanged:

- the same services and mutation queue as the UI;
- shared attention: view state, focus and tours;
- `AnnotatedCode` with ports, threads and dynamic stubs inline;
- the `wait_for_events` long-poll.

Typical loops:

| Loop | Steps |
|---|---|
| Address review | `list_threads {withCode}` → per thread one `edit_code {thread, patches, reply}` or `apply_suggestion {thread, reply}` (edit + answer + addressed); `wait_for_events` when there are none |
| Agent as reviewer | `get_hotspots` → `read_window` / `get_links` → `create_thread(finding)` → `create_tour` |
| Conflicts | `list_conflicts` → `get_conflict` → `resolve_conflict` |

## 9. Budgets

- **Pan and zoom:** 60 fps on a 20k-window view. The client holds at most 16,000 windows at once (`windowCap`, `web/src/data/client.ts`); past that, cells far from the camera are evicted and reload when the camera returns, so "20k" is the view's size, not what is loaded at one time. Measured on cv-mock's `v_synth20000` (`web/scripts/bench.mjs`, 9 scenarios) and in the 600 s soak with the cache at the cap (peak 15,992 of 16,000), WORKSTREAMS F1/F2. Not met with 4× CPU throttling: 55.3–60 fps (§5 *Rendering performance*).
- **Server:** a tile query p95 < 10 ms (Rust, from the in-memory grid; the R*Tree only answers tiles of saved views that are not loaded, §3). Measured on the reference repo (2026-10-19, `scripts/acceptance/tile_bench.py`, every non-empty tile of the levels the client requests, z0–z3, 3 rounds over keep-alive HTTP): p95 0.3–3.9 ms, worst z3 (535 windows, 3,721 edges, 1.5 MiB). The route serializes window headers by reference (no clone, no `serde_json::Value` tree) and sends tiles uncompressed: the server only listens on 127.0.0.1 (§10), where gzip of a z3 tile cost ≈ 20 ms and saved nothing (before: z3 p95 14.9 ms plain, 24 ms gzip).
- **Text latency:** text appears ≤ 150 ms after crossing into L3 when cached, ≤ 400 ms cold.
- **Initial index** of a repo the size of the reference repo on 8 cores:
  - parse < 20 s
  - churn < 10 s
  - connectedness < 2 s (in Python, the whole analysis in appendix C took 13 s)
- **After a jj op:** the canvas updates < 2 s later.

## 10. Safety

- The server binds to 127.0.0.1 and requires a bearer token.
- jj runs in-process through jj-lib; CLI fallbacks use argv only and a server-owned `--config-file`.
- Immutable commits are never rewritten.
- One mutation queue; exact previews; per-actor attribution; targeted undo.
- Per-session toggles control whether the agent may edit and whether it may move the human's camera. A session's toggles are in force while it is present: a WebSocket open with its `?session=` id, or seen (a socket, a permissions call) or changed less than `CV_SESSION_TTL_SECS` ago (default 600 s; the stored change time counts across restarts). A forgotten tab that switched editing off stops blocking the agent after that; its row is kept and applies again when the tab comes back.

## 11. Open questions

All questions raised so far are answered (D1–D17). These defaults were chosen along the way; any of them can be changed:

- Range views draw unchanged clusters as faint outlines at L0/L1 only.
- Context windows reach 1 hop.
- Config-field links appear at score ≥ 0.65 and are solid from 0.80.
- Generic config keys score half.
- The reference repo keeps its prompt YAML in the config lane; a `prompts` lane is one rule away.

## Appendix A: jj 0.45.1 behaviour verified for this design (2026-10-18)

All checked in scratch repos with the CLI. These remain the oracle for the jj-lib implementation.

| What | Result |
|---|---|
| `jj log -T 'json(self)'` | commit_id, parents, change_id, description, author, committer as JSON |
| `self.diff().files()`, `self.diff().git(0)`, `self.diff().stat()` in `jj log` | per-commit files, zero-context diffs for many commits in one process, line counts |
| `jj new --insert-after C --no-edit` from the user's workspace | inserts between C and its children, leaves `@` in place, snapshots pending edits first |
| `jj diffedit --from 'root()' --to X --tool cv-write -- <path>` | writes an existing path into any commit; jj reports "New conflicts appeared in …" |
| same for a path absent at X | "No matching entries" → temporary sparse workspace + `jj squash --from T --into C` |
| rewriting under another workspace's `@` without snapshotting it first | that workspace goes stale. `update-stale` then splits `@` into divergent commits; unsnapshotted files vanish from disk but survive in the divergent sibling |
| `jj resolve -r K --tool cv-resolve` | resolves at a non-`@` commit; descendants rebase clean |
| `jj absorb`, `jj evolog`, `jj interdiff` | absorbs into owning commits; per-change history with ops; interdiff including the description |
| `working_copies()`, `(roots(R)::heads(R)) ~ R`, `jj op revert` | all workspace `@`s; exact gap commits; targeted undo |

## Appendix B: Example rule pack (sketch)

```toml
# .cv/links.toml: project rules on top of the built-in channel packs
[[bus.endpoints]]
id = "bus.envelope"
lang = "python"
role = "producer"
query = '''
(call function: (identifier) @_c (#eq? @_c "EventEnvelope")
  arguments: (argument_list (keyword_argument name: (identifier) @_k (#eq? @_k "topic") value: (_) @key)))
'''

[[bus.endpoints]]
id = "bus.on"
lang = "python"
role = "consumer"
query = '''(call function: (attribute attribute: (identifier) @_m (#eq? @_m "on"))
  arguments: (argument_list . (_) @key . (_) @handler)) @call'''

[[bus.transforms]]
fn = "scoped_topic"
result = "{0}.*?"

[[bus.config]]
id = "workflow.topic"
glob = "config/workflows/**/*.yaml"
path = "$..topic"
role = "consumer"

[[http.endpoints]]
id = "web.request"
lang = "typescript"
role = "client"
method = "GET"
prefix = "/api"
query = '''(call_expression function: (identifier) @_f (#eq? @_f "request") arguments: (arguments . (_) @key)) @call'''

[[env.settings]]
id = "pydantic-settings"
lang = "python"
base_class = "BaseSettings"   # every field of a BaseSettings subclass reads env var FIELD (honours env_prefix)
```

## Appendix C: two-axes study on the reference repo (2026-10-18)

**Method.**

- **Scope:** `ancestors(@, 400)` in the reference repo, read-only. jj ran with `--ignore-working-copy --at-op @`; the op log was unchanged before and after.
- **Commits:** 399 scanned, effective sample 391 after recency weighting; 5,670 files at `@`.
- **Import graph:** built from Python `ast` and relative TS/JS imports. It has 3,490 nodes (code + tests) and 18,313 edges. The largest strongly connected component is 270 files under one core package.
- **Script:** `docs/analysis/axes.py` (`python3 -I docs/analysis/axes.py <repo> [commits]`, read-only). It is the reference implementation for `cv-link` tests.

**Results, code lane (1,481 files with graph data).**

| Connectedness metric | max | median | distinct values | files reaching ≥ ½ of code | ρ with heat | top-5% overlap with top heat |
|---|---|---|---|---|---|---|
| transitive reach, incl. tests | 2,400 | 16 | 192 | 26% | 0.33 | 1 / 74 |
| transitive reach, code only | 790 | 4 | 71 | 26% | 0.27 | 2 / 74 |
| **dependents within 2 hops, code only** | 562 | 2 | 154 | 0% | 0.30 | 11 / 74 |
| fan-in | 735 | 3 | 94 | 0% | 0.42 | 13 / 74 |

| | Hot | Warm | Mild | Cool | Frozen |
|---|---|---|---|---|---|
| heat grade counts | 95 | 104 | 132 | 131 | 1,019 |

| | Core (≥ 85) | High (≥ 23) | Medium (≥ 6) | Low | Isolated |
|---|---|---|---|---|---|
| connectedness grade counts (2-hop quantiles) | 101 | 152 | 272 | 484 | 472 |

| Quadrant | Files |
|---|---|
| hot (Hot/Warm) and central (Core/High) | 73 |
| hot and peripheral | 126 |
| cold and central | 180 |
| cold and peripheral | 1,102 |

1,050 code files have at least one test importing them directly; the most-tested has 507.

## Appendix D: config field usage on the reference repo (2026-10-18)

**Method.** `python3 -I docs/analysis/config_fields.py <repo> [min_score]`, read-only:

- jj ran with `--ignore-working-copy --at-op @`, and the op log was unchanged afterwards;
- files are read from disk and never executed.

| | |
|---|---|
| Config files (YAML, TOML, JSON outside tests and docs) | 952 |
| Key paths | 51,709 |
| Code files scanned | 1,481 |
| Key paths with at least one reader at score ≥ 0.65 | 22,616 (43.7%) |
| Config files with at least one linked key | 906 |
| Direct links (up to 5 readers per key) | 143,012 |
| Links by read kind | access 90,965 · string 33,305 · field 15,580 · attribute 3,162 |
| Largest families (files each) | 237 · 91 · 53 · 28 · 27 · 27 |
| Family hubs (file family, key) | 3,475 |
| Edges through hubs (definers + read sites) | 27,168, 5.3× fewer than direct links |
| Largest hubs | keys of the 91-file family: 91 files → 5 read sites each |

**Spot check.** A 14-link random sample had 12 plausible readers. Typical hits:

- the generic workflow loader `item.get("capacity")` for `inputs.*.capacity`;
- key-path tuples (section, `"*"`, key, field) in a settings module;
- dataclass-style model fields named after the key (`<key>: str`);
- `entry["node_id"]` in the node's own module.

Both misses were generic keys (`trigger`, `from`) read in unrelated code, which the generic-key penalty is meant to fade.

The prototype runs in about 8 minutes because it scans every site for every key. `cv-link` should index sites by token once and score per family hub.
