# Workstreams

The build splits across implementer agents as follows. Each workstream:

- owns one crate or one web directory;
- provides and consumes the contracts in `docs/contracts/`: TypeScript for the wire, Rust traits for the crate boundaries;
- can start against mocks before its neighbours exist.

The rationale behind every requirement is in `docs/DESIGN.md`; section numbers below point there.

Measurements below were taken on *the reference repo*: a private Python/TypeScript project of about 5.6k files. Its code is not part of this repository.

## Rules for every implementer agent

1. **Contracts are the boundary.** Wire types change only through C0, additively within `/api/v1`. Crate traits change only by agreement with the crates on both sides. If a contract is wrong, raise it; do not route around it.
2. **The core stays language- and protocol-neutral.** No crate except `cv-lang-*` and `cv-channel-*` may depend on a tree-sitter grammar or match on a language or protocol name. C0 enforces this with a `cargo metadata` check in CI, and `scripts/check-core-deps.sh` also scans the analysis core (cv-lang, cv-link, cv-channel, cv-layout, cv-churn) for language names, file extensions, self receivers and entry stems; packs supply those as `LanguagePack::conventions()`.
3. **One agent, one jj workspace.** For example: `jj workspace add ../cv-b1 --name b1`, then bind your MCP session to it. Describe your change as soon as you start.
4. **Done means green.** Backend: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`. Web: typecheck plus tests. A workstream is done when its acceptance list passes against C0's fixtures.
5. **Pin versions.** jj-lib stays at the installed jj CLI's exact version (0.45.1 today).

## Layout

```
crates/cv-contracts        C0   wire types (serde + ts-rs + schemars); generated TS → web/src/contracts/
crates/cv-mock             C0   axum server answering every route from fixtures
fixtures/                  C0   golden jj repos (scripted), mock views, 20k-window synthetic view
crates/cv-vcs              B1
crates/cv-store            B2   + migrations from docs/schema/*.sql
crates/cv-lang             B3   pack registry, parse driver, chunking + fragments, lane classifier
crates/cv-lang-python      L1   ┐
crates/cv-lang-typescript  L2   │ language packs (LanguagePack)
crates/cv-lang-config      L3   │ YAML, TOML, JSON, Dockerfile, HCL, shell
crates/cv-lang-rust        L4   │
crates/cv-lang-cpp         L5   ┘
crates/cv-channel          B4   key algebra, hub matching, pack registry
crates/cv-link             B4   reference resolution, hubs, connectedness
crates/cv-channel-bus      P1   ┐
crates/cv-channel-http     P2   │ channel packs (ChannelPack)
crates/cv-channel-env      P3   │
crates/cv-channel-deploy   P4   │
crates/cv-channel-config   P5   ┘ config keys → code that reads them (heuristic)
crates/cv-churn            B5
crates/cv-layout           B6
crates/cv-server           B7   axum, WS, event log, scheduler, `cv` CLI
crates/cv-review           B8
crates/cv-mcp              B9   rmcp
web/src/canvas             F1
web/src/data               F2
web/src/windows            F3
web/src/panels             F4   + app shell
```

## Overview

| ID | Workstream | Phase | Needs (real) | Starts against |
|---|---|---|---|---|
| C0 | Contracts & mocks | 0 | — | — |
| B1 | jj adapter (`cv-vcs`) | 1 | — | golden repos |
| B2 | Store (`cv-store`) | 1 | — | schema files |
| B3 | Language core (`cv-lang`) | 1 | — | a stub pack |
| L1 | Python pack | 1 | B3 trait | reference repo files |
| L2 | TypeScript pack | 1 | B3 trait | the reference repo's web app |
| B7 | Server (`cv-server`) | 1 | B2 | fake services |
| F1 | Canvas engine | 1 | — | cv-mock, 20k view |
| F2 | Data client | 1 | — | cv-mock |
| F4 | Panels & dialogs | 1 | — | cv-mock |
| L3 | Config pack | 2 | B3 | reference repo YAML, Dockerfile |
| B4 | Channels core + links | 2 | B3 L1 L2 | BlobFacts fixtures |
| P1 | Bus channel pack | 2 | B4 | reference repo topics |
| P2 | HTTP channel pack | 2 | B4 | FastAPI + the web app |
| P3 | Env channel pack | 2 | B4 L3 | reference repo settings |
| P4 | Deploy channel pack | 2 | B4 L3 | Dockerfile, CI, scripts |
| P5 | Config-fields channel pack | 2 | B4 L3 | reference repo YAML + `docs/analysis/config_fields.py` |
| B5 | Churn (`cv-churn`) | 2 | B1 | recorded history |
| B6 | Layout (`cv-layout`) | 2 | B3 B4 B5 | fixture facts, links, heat |
| B8 | Review & edits (`cv-review`) | 2 | B1 B2 | golden repos |
| F3 | Code windows & editor | 2 | F1 F2 | cv-mock |
| B9 | MCP (`cv-mcp`) | 3 | B7 B8 | — |
| I1 | Integration & performance | 3 | all | the reference repo |
| L4 | Rust pack | 4 | B3 B4 | this repo's own crates |
| L5 | C++ pack | 4 | B3 B4 | a sample C++ project (no build needed) |

## C0: Contracts & mocks (phase 0, one agent, short)

- **Owns:** `cv-contracts`, `cv-mock`, `fixtures/`, the Cargo workspace and web skeletons, CI.
- **Provides:**
  - Rust wire types for everything in `docs/contracts/*.ts`;
  - ts-rs output in `web/src/contracts/`;
  - schemars JSON Schemas for MCP tools (as built, the input structs live in `cv-mcp/src/params.rs` and are checked against `mcp.ts` by `cargo test -p cv-mcp --test mcp_inputs_ts`; DESIGN D3, §3 *Contracts check*);
  - the boundary traits from `docs/contracts/rust/`, moved into their crates.
- **Fixtures:**
  - a script that builds golden jj repos covering: a linear stack, an insert that conflicts with descendants, two workspaces with unsnapshotted edits, a non-convex selection, an agent-bound workspace, and a file split into 5 fragments;
  - mock views with lanes, district grid, fragments, dynamic links and workspaces;
  - a 20k-window synthetic view.
- **Acceptance:**
  - the generated TS is structurally equal to `docs/contracts/*.ts` (golden test);
  - every fixture validates;
  - cv-mock answers every route;
  - the CI check rejects a core crate that depends on a grammar or channel pack.

Evidence (2026-10-18):
- Generated TS: `scripts/gen-contracts.sh` writes `docs/contracts/generated/` and `web/src/contracts/generated/` from
  cv-contracts (ts-rs 12, 84 types). The golden test `cargo test -p cv-contracts --test generated_ts` fails when either copy
  is stale. The generated `check.ts` asserts that every type shared with the hand-written spec is structurally equal to it
  (mutual assignability plus the same top-level keys; ids unbranded, literal unions widened); `tsc --noEmit` in `web/`
  compiles it. Known differences are allow-listed in `crates/cv-contracts/ts-divergences.txt`.
- Fixtures: `scripts/fixtures.sh` builds the golden repos `linear` (5-change stack, a rename, a file in 5 fragments,
  convex `B::D` and non-convex `B | D` ranges), `conflict` (an insert that conflicts with 2 descendants) and `workspaces`
  (`default`, `ws2` with unsnapshotted edits, `agent`). They are deterministic: `--check` rebuilds them and matches
  `fixtures/repos.manifest`.
- Replay: `scripts/contract-replay.sh [--record]` has 177 recorded calls (re-recorded 2026-10-19: cv serve on the 3
  repos 118 = linear 54, conflict 23, workspaces 41; `serve-resync` 3; cv-mock 56), type-checked against
  `docs/contracts/http.ts` (WebSocket frames against `ws.ts` `ServerFrame`) with excess-property checks: **0
  divergences**. Covered: every `http.ts` route except long-poll waiting, now including `PATCH /threads/:threadId`
  (status and assignee; serve and mock), `GET /diff` (serve: a revset pair and a `fileset`; mock: one pair) and the
  WS `hello` (mock and serve). `serve-resync`: the recorder ages the linear repo's event log past retention
  (every event dated 2000-01-01, then 1,000 fresh copies), restarts cv serve, which prunes at startup, and records
  `GET /events?cursor=1` and WS `?cursor=1` answering `resync: true`, plus a plain hello. cv-mock has no event
  retention, so it has no resync recording. Before that, re-recorded 2026-10-18 (conform): 164 calls, 0 divergences
  (was 48 allow-listed). Of the 48, 5 serve entries (ISO times,
  bind 404, `agent.client: null`) were already fixed by later server changes; `POST /agent/bind` now answers the bound
  workspace's `WorkspaceSummary` (the card `GET /workspaces` shows; MCP `bind_workspace` reads its `stack.revset`); cv-mock
  sends ISO-8601 times, omits `assignee`, `EditResult.created` and `WorkspaceSummary.agent` instead of sending null, and
  drops the extra `MetricPoint.n` and `ConflictRecord.start`. `fixtures/replay/known-divergences.txt` is empty.
- `ts-divergences.txt`: the Rust types now carry the spec's optional `SiteRef.col`/`symbol`, `ViewRef.sharedPlacement`,
  `ViewBootstrap.churn` (`ChurnSummary`), `Heat.inRange` and `WindowHeader.diffStat`, so those 5 entries are gone. Four
  one-directional entries stay on purpose: `Cluster.pinned` and `WorkspaceSummary.files` are always written (`out`; ts-rs
  can only mark `Option` fields optional), `ExpandRequest`/`NewThread` bodies accept more than the spec (`in`), and
  `ViewBootstrap` contains `Cluster`.
- cv-mock answers every route in `http.ts` (`cargo test -p cv-mock`, route tests via tower `oneshot`).
- `scripts/check-core-deps.sh` (`cargo metadata`) reports OK today. On a scratch copy of the manifests it rejected
  `cv-churn -> tree-sitter-python` and `cv-layout -> cv-channel-http`. CI: `.github/workflows/ci.yml` runs `scripts/ci.sh`.

## B1: jj adapter (`cv-vcs`)

- **Provides:** the `Vcs` trait (`docs/contracts/rust/src/vcs.rs`), implemented with jj-lib 0.45.1.
- **Key behaviour:** DESIGN §4.1, §6, §7; appendix A.
- **Acceptance:**
  - Reads are correct on golden repos, and never create a jj operation.
  - `history()` is lazy.
  - `preview()` never writes: the op log is unchanged after it.
  - Every `MutationPlan` produces the same trees as the CLI recipe oracle, including a new file at a non-`@` commit.
  - An unsnapshotted edit in a second workspace survives an ancestor amend, or is reported in `recovered_into`.
  - Revert restores trees.

## B2: Store (`cv-store`)

- **Provides:** the `Store` trait over `index.db` and `review.db` (rusqlite, bundled: FTS5 and R*Tree).
- **Acceptance:**
  - Migrations equal `docs/schema/*.sql`.
  - `windows_in_rect` p95 < 10 ms on the 20k view.
  - FTS ranks an exact symbol match first.
  - Deleting `index.db` never touches `review.db`.
- **Status (2026-10-19):** saved snapshot views (windows + R*Tree, links, clusters, files, FTS) and review records in tables; see DESIGN §3 "Storage (D5), as built". The in-memory grid stays the tile hot path (measured, DESIGN §3); the R*Tree serves tiles of unloaded saved views (p95 0.3 ms for `windows_in_rect`). `vcs_*`, `blob`/`fact_*`, `link_hub`, `link_metric` and `churn_*` stay empty: facts live in the parse cache, hubs and churn in `canvas_view`. `canvas_port`, `canvas_window_diff` and `canvas_hub_place` also stay empty (nothing in the code reads or writes them, checked 2026-10-19): ports are derived from `link_edge` at the callsite line (`tile_edges`) with only their count kept in `canvas_window.ports`, hub placement is part of the bootstrap JSON in `canvas_view`, and range views (the only views with diff lines) are not saved. They are kept in `docs/schema/index.sql` as reserved; dropping them needs an index.db `schema_version` bump (a rebuild of derived data).

## B3: Language core (`cv-lang`)

- **Provides:**
  - the `LanguagePack` and `LaneClassifier` traits and registry;
  - the parse driver (rayon; content-addressed by blob × pack version);
  - snapshot chunking and hunk windows;
  - file fragments;
  - the lane classifier (defaults plus `.cv/lanes.toml`).
- **Key behaviour:** DESIGN §4.2–§4.4.
- **Acceptance:**
  - Windows ≤ 500 lines unless expanded; files ≤ 120 lines get one window.
  - Fragments are numbered with a stable chip slot per path.
  - Lane defaults classify the reference repo as in DESIGN §4.3 (infra 11, config 1,000, code 1,490, test 2,029, docs 1,101 ± rename drift; appendix C's
    955 / 1,488 / 2,061 / 1,093 predate the D15 rule order, see the layout5 evidence below).
  - The core has no language names in it.

Evidence (2026-10-18):
- The lane rules (extensions, lockfiles, infra names, test dirs, the k8s sniff) moved from `lanes.rs` to the data file
  `packs/lanes/defaults.toml`. The test `data_rules_classify_like_the_old_hard_coded_rules` checks them against the old
  classifier's output on 1,050 committed paths (`crates/cv-lang/testdata/lane_sample.tsv`; all 6,103 captured paths
  matched), and `core_names_no_file_types` keeps them out of the code.
- Lane counts on the reference repo (before layout5): infra 11, config 955, code 1,494, test 2,072, docs 1,096
  (other 62), checked by a test run against a local clone (not published).

Evidence (2026-10-19, layout5 lane rules):
- D15 rule order (other, infra, config, test, docs, code: a config file under `tests/` or `docs/` is config) and the
  vendored/generated rule (vendor, third-party and `generated` directories, protobuf/gRPC stubs, `*.generated.*`,
  minified bundles → `other`) in `packs/lanes/defaults.toml`; tests `config_files_under_tests_and_docs_are_config`,
  `vendored_and_generated_files_are_other`, `docs_start_collapsed` (cv-lang); 23 rows of `lane_sample.tsv` re-labelled on purpose
  (its header says so).
- On the reference repo the new order moves 44 config files under `tests/` from test to config and 4 generated files from code to other.
  At the trunk of the time: infra 11, config 1,000, code 1,490, test 2,029, docs 1,101 (other 66), now §4.3's numbers.
  On a later trunk: infra 11, config 1,000, code 1,492, test 2,032, docs 1,101 (other 66), the same per-lane
  counts as `appendix_c.py`'s `d15_lane` reference ✓ (within ±2% of §4.3).
- The docs lane starts collapsed: the server marks it (`laneDefs[].collapsed`), the UI starts its pill off and opening a
  docs target opens the lane (panels-flow on cv serve: "collapsed lanes start closed", "flying to README.md opens the
  docs lane"; cv-mock marks no lane collapsed on purpose, see `crates/cv-mock/src/synth.rs`).

## L1–L5: Language packs

- **Each provides:** one `LanguagePack` with:
  - its grammar and `.scm` queries for symbols, refs, imports and highlights;
  - an import resolver;
  - key folding.
- **Each acceptance (on a hand-checked sample of ≥ 20 files):**
  - symbols, refs and imports are correct;
  - key folding handles the language's string forms;
  - columns are UTF-16;
  - median parse < 5 ms per 1k lines, where "parse" is the tree-sitter grammar parse (decided
    2026-10-18). Exception: C++, whose grammar (tree-sitter-cpp 0.23.4, the newest) parses at 7–9 ms
    per 1k lines on real code; it is accepted as a limit of the chosen parser. TypeScript passes but
    with little margin on dense app code (4.9 ms on web/src).

Hand-checked samples (2026-10-18), alongside the earlier Rust and C++ ones:
- **L1 Python:** 23 files from the reference repo, 1,155 lines (`crates/cv-lang-python/tests/sample.rs`). Every symbol and import was
  checked by reading the source, plus 29 refs, UTF-16 columns, and folding of f-strings, `.format`, `%` and `+`. ✓
  Replaced on 2026-10-22 by 24 synthetic files (862 lines, a small bookshop service) written to cover the same cases; 38 refs.
- **L2 TypeScript:** 23 files, 1,425 lines (13 from web/src, 10 from the reference repo's web app, TS and JS)
  (`crates/cv-lang-typescript/tests/sample.rs`). Symbols, imports, 21 refs, UTF-16 columns and template literals. ✓
  On 2026-10-22 the 10 reference-repo files became 12 synthetic ones (an invoicing front end); now 25 files, 1,292 lines, 23 refs.
- **L3:** `crates/cv-lang-config/tests/l3.rs` covers Dockerfile stages, shell functions (`f() {`, `function f {`,
  `function f() {`), CI jobs, and YAML `>`/`|` blocks. ✓
- **Fixed on the way:**
  - shell `function f {` produced no symbol;
  - Python f-string `{{`/`}}` and `.format` `}}` were not unescaped.
- **L3 grammars (2026-10-19):** YAML, TOML, JSON, Dockerfile, shell and HCL parse with tree-sitter and `.scm` queries
  (DESIGN §4.2; INI/.env, Makefiles and Markdown stay on the line scanner, which is also the fallback for files a
  grammar cannot parse). Parity with the line scanners (`crates/cv-lang-config/tests/dump.rs` dumps every key and
  symbol; diffed before/after): the reference repo 71,942 → 71,854 key facts, all differences explained (87 `sh` facts for
  shell lines that run no command dropped; 2 `env.*` command prefixes added; 3 YAML facts the line reader took from
  inside multi-line quoted strings dropped; a few YAML values now unquoted when a comment follows them; TOML
  array-of-tables keys now carry their own line instead of the first element's), symbols identical. On 7,000+ local
  files: JSON 749,863 keys identical; TOML keys identical (lines as above); YAML 99% identical, the rest keys the
  line reader took from multi-line strings; Dockerfile 3,062 of 3,063 keys. P5 on the reference repo is unchanged (123,814
  links, 21,190 keys with a reader, 3,175 hubs). JSON is 26× faster (the scanner's tokenizer was quadratic on
  minified files).

Measured (2026-10-18; `cargo run --release -p cv-server --example parse_speed -- DIRS`: median over real
files of ≥ 20 lines, best of 3, quiet machine). "Grammar" is the acceptance figure; "full" is the pack's
whole `analyze` (symbols, refs, imports, highlight tokens), reported for tracking:

| Pack | Sample | Grammar | Full |
|---|---|---|---|
| TypeScript | this repo's web/src (41 files) | 4.9 ms ✓ | 8.7 ms |
| JavaScript | reference repo (459 files) | 3.5 ms ✓ | 5.9 ms |
| Python | reference repo (2,721 files) | 4.1 ms ✓ | 6.7 ms |
| Rust | this repo (92 files) | 4.3 ms ✓ | 7.3 ms |
| C++ | libstdc++ headers (174–441 files) | 7.0–8.7 ms (accepted exception) | 8.8–10.5 ms |

The repo-level budget (§9: parse < 20 s for the reference repo) is met at 1.6 s.

- **L1, L2, L4, L5 on `.scm` queries (2026-10-19):** the four code packs now read their trees through
  `crates/cv-lang-<x>/queries/{highlights,tags}.scm`, compiled as one query per grammar
  (`cv_lang_sitter::query::PackQuery`), in place of the hand-written walks. Code still decides scopes, nesting and
  qualnames from the tagged nodes, and C++ colours function and macro names from what reading the tags found
  (DESIGN §4.2 lists the deviations with measurements). Evidence:
  - parity: `cargo run --release -p cv-server --example facts_dump -- DIRS`, run before and after, gives byte-identical
    symbols, refs, imports, tokens and error counts on 10,122 code files: the reference repo (a clone: 3,015
    Python, 494 TS/JS), this repo (266), Python stdlib packages (122), node_modules TS/JS (2,237), six crates.io crates (1,027
    Rust), libstdc++ and other C/C++ headers (2,801);
  - every existing pack test and hand-checked golden (`tests/sample.rs` in L1/L2, the Rust and C++ unit samples)
    passes unchanged; a new test per pack fails when the grammar has an anonymous token the generated block of
    `highlights.scm` does not classify;
  - timing (same machine, old and new binaries alternated): grammar parse unchanged (Python 4.0, JS 3.2, TS 4.6–4.8,
    Rust 4.1–4.2 ms ✓; C++ 6.8 ms, accepted exception; this repo's 7 TSX files 6.2 ms, over 5 ms before and after);
    full `analyze` 2–16% slower (Python 6.5 → 7.2, JS 5.6 → 6.1, TS 8.4 → 9.2, TSX 10.5 → 11.7, Rust 7.2 → 8.3,
    C++ 8.5 → 8.6 ms per 1k lines).

| Pack | Covers |
|---|---|
| L1 Python | f-strings, `.format`, `%`, module constants, relative imports, pyproject roots, decorators |
| L2 TypeScript | TS, TSX and JS; template literals; `tsconfig` paths; `export … from` |
| L3 Config | YAML, TOML, JSON, Dockerfile (stages), HCL (resources), shell (functions). The config "symbols" give windows: top-level keys, compose services, CI jobs. Emits every key path with its line (`ConfigKeyFact`), indent-aware, skipping `\|`/`>` block text |
| L4 Rust | `mod` tree, `use` paths, Cargo workspace members, `format!`, `concat!`; channel sites (axum routes and `nest` prefixes, reqwest, `std::env::var` / `env!`) |
| L5 C++ | `#include` by heuristics only, no `compile_commands.json`: relative to the including file, then conventional include dirs, then `include_directories()` scraped from `CMakeLists.txt`; namespaces and classes as containers |

## B4: Channels core and links (`cv-channel`, `cv-link`)

- **Provides:**
  - the `ChannelPack` trait and registry;
  - `KeyExpr` normalization and the matchers' common parts;
  - hubs;
  - reference resolution;
  - dynamic stubs;
  - **connectedness** (2-hop code dependents, fan-in/out, test dependents, cycle size, quantile grades);
  - the `Linker` trait.
- **Key behaviour:** DESIGN §4.5, §4.6, §4.8.
- **Acceptance:**
  - The appendix C numbers reproduce on the reference repo within rounding (`docs/analysis/axes.py` is the reference implementation;
    lane counts use its rules in the D15 order, see the evidence).
  - A fully dynamic key yields a dynamic stub, never a guessed link.
  - Precision ≥ 0.9 on 50 hand-labelled links in the reference repo.
  - Confirmed or rejected feedback overrides heuristics.

Evidence (2026-10-19; the reference repo's trunk at the time unless noted; `scripts/acceptance/run_all.sh <clone>`). The
headers of these records first named the wrong commit as the trunk: the empty working-copy commit that `prepare.sh`
(`jj new 'trunk()'`) made in the throwaway clone, because `common.head_commit` printed `@`. Its parent was the trunk
measured, and axes.py's lane counts on that trunk (956 / 1,494 / 2,073 / 1,101) equal the recorded ones. `head_commit`
now prints the trunk commit and names the clone's `@` separately.
- **Appendix C** reproduces (**27/27 rows** within rounding, `--max-commits 400` like the
  study; `appendix_c.py` exits 1 when a row fails). First run, before layout5:
  - lanes match axes.py exactly (infra 11, config 956, code 1,494, test 2,073, docs 1,101);
  - heat grades: Hot 96 / 95, Warm 111 / 112, Mild 148 / 148, Cool 123 / 122, Frozen 1,009 / 1,010;
  - connectedness on the appendix's own definition, the file import graph
    (`GET /views/:id/metrics?level=file&basis=imports`, axes.py vs cv): 2-hop max 569 / 569, median 2 / 2,
    151 / 151 distinct; Core 102 / 102, High 155 / 155, Medium 274 / 274, Low 484 / 483, Isolated 472 / 473;
    fan-in max 742 / 742; files with a direct test 1,056 / 1,056 (most-tested 514 / 514); quadrants
    73 · 134 · 184 · 1,096, all exact; ρ 0.308 / 0.309.
  - Why a second basis: connectedness became per window over typed links (DESIGN §4.8), while appendix C is
    file-level over imports. The earlier harness compared the max over each file's windows with axes.py and
    regressed to 15 PASS / 12 FAIL once the metric changed (and still exited 0). The window-level per-file
    aggregate is now printed as `[typed links, info]` rows (Core 267, 2-hop max 979, quadrants 134 · 73 · 347 · 933,
    ρ 0.39), with the reasons in DESIGN §4.8.
  - **Lane parity after layout5 (2026-10-19, a later trunk, docs6).** Against axes.py as written, the lane rows now
    fail: config 956 / 1,000 (4.6%) and test 2,076 / 2,032 (2.1%) are outside ±1% (code 1,496 / 1,492 and the rest
    pass). The difference is exactly the layout5 rule change: 44 files move test → config (config under `tests/`
    and `docs/`, D15) and 4 code → other (generated `.d.ts` files), 48 in all. The reference changed, not the
    threshold: D15 says config files always get their own lane, so appendix C's order (test and docs directories
    first) is the study's rule, not the product's, and widening ±1% to ~5% would hide real classifier regressions
    of that size. `appendix_c.py` now compares cv with `d15_lane` (axes.py's own rules, unchanged, tried in the D15
    order with the vendored/generated rule) and keeps axes.py's counts as `[axes.py as written, info]` rows, with
    a note that counts the moved files and checks that its copy of axes.py's order reproduces axes.py's own counts
    (5/5). axes.py itself stays as appendix C ran it. Result: **27/27 PASS**: lanes 11 / 1,000 / 1,492 / 2,032 /
    1,101 exact; heat Hot 98 / 97, Warm 113 / 112, Mild 149 / 151, Cool 123 / 120, Frozen 1,006 / 1,005; imports
    basis 2-hop max 571 / 571, Core 102 / 102, High 156 / 155, Medium 275 / 275, Low 484 / 482, Isolated 472 / 471,
    fan-in max 744 / 744, tested 1,058 / 1,058, quadrants 73 / 72 · 138 / 137 · 185 / 185 · 1,093 / 1,091,
    ρ 0.310 / 0.306. "Code files with graph data" is 1,489 / 1,485: axes.py still counts the 4 generated files.
- **Precision on 50 hand-labelled links ≥ 0.6** ✓ (2026-10-19, docs6, on that later trunk; the labelled sample is not
  published, it quotes private code): **50/50 = 1.00** (95% Wilson lower bound 0.929). Seed 1019, drawn with `sample_links.py code CLONE
  OUT --conf-min 0.6 --seed 1019` from the 45,996 links ≥ 0.6 among the 58,427 code/test out-links (all at 0.85,
  0.9 or 1.0: no code link scores between 0.6 and 0.8); 37 at 0.85, 13 at 0.9; 47 calls, 3 `inherit`. Labelled by
  an agent (Claude), not a person, by reading both ends and checking every binding of the called name in the source
  file (criteria in the sample's header). All 50 are import-resolved, same-file or `self.` calls, the kinds the
  resolver scores ≥ 0.85; the sample says nothing about lexical candidates.
  This closes the gap the earlier sample left. That one (seed 2026, uniform over 68,779 code/test out-links
  including 22,976 candidates, on the earlier trunk) gave
  33/33 for links ≥ 0.6 but only 39/50 = 0.78 over every drawn link, because 17 of its links were candidates
  (≤ 0.5, drawn faded and dashed, not dependencies). **So, honestly:** the ≥ 0.9 bar on 50 labelled links is met
  for dependency links (≥ 0.6); it is not met for a uniform sample of every drawn link, candidates included (0.78),
  and is not meant to be, since candidates are drawn as guesses (DESIGN §4.5).
  The seed-7 sample from before candidates existed scored 50/50.
- **Lexical candidates (≤ 0.5), plausibility**: the 17 above plus a candidates-only sample of 30 (seed 2027),
  each labelled by reading the call site's receiver: **17/47 = 36%**
  plausible. By confidence: 0.50 5/7, 0.45 6/17, 0.40 3/10, 0.35 2/3, 0.30 1/6, 0.25 0/4. 13 of the 30 misses are
  builtin or library methods (`list.append`, `set.add`, `dict.update`, `Path.open`, `Pattern.match`,
  `Condition.wait`) matched to a same-named project method; the others pick another class's override, a test
  double or the protocol stub instead of the implementation. A label is "plausible" only when the target is the
  definition the call dispatches to (an inherited base method counts; a base the receiver's class overrides
  does not). Candidates are drawn faded and dashed and are not dependencies, which these numbers support.

## P1–P4: Channel packs

- **Each provides:** one `ChannelPack` with rule files per language pack (`packs/channels/<protocol>/<lang>-<framework>.toml`), normalization and matching.

| Pack | Covers | Acceptance |
|---|---|---|
| P1 Bus | MQTT `*` and `#` matching; `scoped_topic` transform; YAML `topic:` config rules | the reference repo's topic hubs, exact and wildcard, link producers to consumers |
| P2 HTTP | FastAPI, Express, axum routes; fetch, axios, wrapper prefixes, reqwest clients; router prefixes | `/api` + `/item/${id}` matches `/api/item/{item_id}` |
| P3 Env | dotenv, Dockerfile `ENV`/`ARG`, compose `environment`, k8s, CI env; `os.getenv`, `process.env`, `std::env::var`, `getenv`; **pydantic-settings `BaseSettings` fields** (honouring `env_prefix`) | the reference repo's settings link to `.env.example`, raising the 4 regex-only matches toward the 53 defined variables |
| P4 Deploy | Dockerfile `CMD`/`ENTRYPOINT`, compose `command`, CI `run:`, scripts → module, script or test path | the reference repo's `Dockerfile` → `main.py`; `pre-commit.sh` and CI → tests |
| P5 Config fields | config keys → `family#key` hubs → read sites. Scored by read kind, key rarity, directory proximity and loader mentions (DESIGN §4.6); `affinity()` returns the score; generic keys score half; by default also node context and key genericity (DESIGN §4.6 "Context") | the reference scoring (`CV_CONFIG_SCORING=reference`) reproduces `docs/analysis/config_fields.py` on the reference repo within rounding; ≥ 80% plausible on 50 hand-checked links of the default scoring; one hub per family and key |

Evidence (2026-10-18; the reference repo's trunk at the time):
- **P1** ✓:
  - an exact topic: a Python producer → a consumer declared in a workflow YAML file;
  - a wildcard topic family: 3 hubs, 10 producer files → 3 consumer files;
  - all 89 bus hubs have both ends.
- **P2** ✓: a TypeScript client call with a template-literal path, made through a `request()` wrapper with
  `API_BASE = '/api'`, meets the FastAPI route it targets at a `GET /api/…/{}` hub. All 51
  HTTP hubs have a client and a route.
- **P4** ✓: the `Dockerfile`'s `ENTRYPOINT [… "python", "main.py" …]` → `main.py`; `pre-commit.sh`'s
  `uv run pytest … tests/` → `tests/conftest.py`; an e2e orchestration script → the e2e suite's entry module.
  8 deploy links in all (also the `start_*.sh` scripts → `main.py`). The reference repo has no CI workflow file
  (`.github/workflows`, `.gitlab-ci.yml`), so "CI → tests" is checked by `cargo test -p cv-channel-deploy --test ci_run`.
- **P2/P3 for Rust** (2026-10-19, this repository indexed read-only): `http: 147 endpoints, 18 hubs, 60 links` (was 42
  endpoints, 0 hubs): the web client's `fetch("/api/v1/…")` calls (engine.ts, App.tsx, Panels.tsx, Review.tsx, the e2e
  scripts) reach cv-server's axum routes and cv-mock's. `env: 77 endpoints, 2 hubs, 8 links`: `CV_TOKEN` (defined by
  `CV_TOKEN=… cv serve` in `run-runtime.sh`) → cv-server, cv-mcp, vite.config.ts and agent-loop.mjs; `JJ_CONFIG`
  (`export` in `fixtures.sh`) → cv-vcs. `CV_UI_DIR` is read but defined nowhere in the repo, so it has no hub.
- **P3** ✓: 25 env variables link a definition to a read (the regex-only baseline was 4; ceiling 53). 17 of them are defined
  in `.env.example`.
- **P5 reproduces config_fields.py** ✓ (14/14 rows exact, reference scoring):
  - 51,811 key paths;
  - 143,368 links;
  - 22,691 keys with a reader (43.8%);
  - 3,499 hubs, 27,254 hub edges;
  - one hub per family and key (3,499 distinct).

  938 config files vs the reference's 952: the 14 JSON files without identifier keys cannot carry links.
- **P5 plausibility ✓: 41/50 = 82%** with the default context scoring (fresh sample, seed 2026, hand-labelled by
  reading both ends, not published as it quotes private code; 2026-10-18; re-measured 2026-10-19 on a trunk 12 commits
  later at 40/50 = 80%, below).
  - Before (reference scoring): 32/50 = 64% (seed 7).
    Misses were generic structural keys (`from`, `qty`, `words`, `size`, `height`, `topic`) and same-named keys
    of a different node, matched across unrelated structures.
  - The fix (cv-channel-config `Scoring::Context`, DESIGN §4.6): a parent key named within a few lines of the
    read site adds 0.15; keys defined in ≥ 10% of config families score ×0.75; and a link needs context (a
    parent key nearby, or for non-generic keys a same/sibling directory or a loader mention of a top-level key).
  - Recall (same trunk): links 143,368 → 113,115; keys with a reader 22,691 → 20,869 (43.8% → 40.3% of key paths, 92% kept);
    files with a linked key 906 → 885; hubs 3,499 → 3,079.
  - On the old seed-7 sample, 32 of the 50 links survive the new scoring and 27 of those are plausible (84%):
    27 of its 32 plausible links stay, 13 of its 18 misses go.
  - Remaining misses (9): nested same-named keys read at another level (a list item's key vs a top-level key of
    the same name, ×2), `qty` of a different structure (×2), an attribute match inside a string literal, a key in a
    prompt string, a subscription-list `topic` vs a node's `topic` param, a request payload `method` vs
    an endpoint's configured `method`, and another node's `temperature`.
  - Confidence now separates better: 35/41 plausible at ≥ 0.80, 6/9 below.
  - **Re-measured 2026-10-19**: the reference repo's trunk had moved 12 commits. Default scoring on a clone of the new
    trunk: 939 config files, 51,816 key paths, **123,814 links, 21,190 keys with a reader (40.9%), 886 files with a
    linked key, 3,175 hubs** (25,513 hub edges). The same build on a clone of the old trunk reproduces 113,115 / 20,869 /
    885 / 3,079 exactly, so the two sets of numbers are two revisions of the reference repo, not a scoring change; the
    123,814 / 21,190 / 3,175 quoted under L3 are the newer trunk's. Fresh sample (seed 1019, 50 of 123,814, labelled by
    reading both ends): **40/50 = 80% plausible ✓** (at the threshold); ≥ 0.80: 36/43, 0.60–0.80: 4/7. The 10 misses
    are the known kinds: `qty`, `words`, `height` and similar keys of a different structure (7), docstring mentions (2),
    an input key matched to the output parser (1).
  - Parity: `config_fields.py` runs `cv index` twice; the reference scoring must match the reference script,
    the default scoring's counts are reported as info rows. The two diverge on purpose.

## B5: Churn (`cv-churn`)

- **Provides:** `ChurnService`: heat with change-mass quantile grades, Wilson intervals on the effective sample size, the stopping rule, co-change and range heat.
- **Key behaviour:** DESIGN §4.7.
- **Acceptance:**
  - Stable synthetic history stops as "settled" before 500 commits; drifting history does not stop early.
  - Mass refactors are down-weighted.
  - Renames carry history.
  - The reference repo grades in < 10 s.

Evidence (2026-10-19; the reference repo's trunk at the time, see B4):
- **< 10 s ✓**: churn over 2,000 commits took 1.1–3.0 s (`cv serve` timings, runs of the same day, in parallel with
  parsing); the whole history (3,922 commits) 1.8–2.0 s (`cv index --max-commits 20000`).
- **Settling on the reference repo**: the walk stops at the 2,000 cap with 36% of the change mass settled (Hot 73%
  settled; Warm, Mild, Cool none). The 2,000 commits span 8.7 days, so recency weights are 0.94–1.0 and the
  effective sample equals the commit count; the limit is statistical (Warm files have ~3–8 touches, whose Wilson
  intervals are wider than the Warm band). Replaying today's rates, settled mass would be 41% at 4k commits, 56% at
  16k, 72% at 32k and levels off near 80%, so the 90% rule cannot stop on this repo at any length (the whole history
  settles 38%). A span-relative half-life would only lower the effective sample. The rule and the cap stay as
  designed (DESIGN §4.7 documents the limit and its consequence: only Hot files move clusters on heat changes).

## B6: Layout (`cv-layout`)

- **Provides:** `LayoutService`:
  - window clustering with file cohesion;
  - the district grid (lanes × heat columns × connectedness rows, columns aligned across lanes, empty cells collapsed);
  - Sugiyama layering inside clusters;
  - same-file attraction;
  - stability;
  - metrics for the Hotspots panel.
- **Key behaviour:** DESIGN §4.9, §4.10.
- **Acceptance:**
  - Deterministic for identical input.
  - A cluster moves only when a grade settles.
  - One changed file moves < 5% of windows.
  - Pins never move.
  - The review-first cell of the code lane is at the camera's start.
  - The fragments of the 5-fragment fixture file end up in ≤ 2 clusters.
  - A range view reuses the snapshot placement of its tip: every changed window sits where its symbol sits in the full view.
  - Context windows start minimized.

Evidence (2026-10-19, `cargo test --release -p cv-layout`, 19/19 pass; and `-p cv-server --test readside`):
- deterministic for identical input: `deterministic`, `leiden::deterministic_and_warm_start_keeps_a_good_partition`;
- a cluster moves only when a grade settles: `a_cluster_moves_only_when_its_heat_grade_settles`;
- one changed file moves < 5% of windows: `one_changed_file_moves_under_5_percent_of_windows`;
- pins never move: `pins_never_move` (and window/cluster pins over HTTP in `cv-server --test layout`);
- review-first cell at the start: `review_first_cell_is_top_left_of_the_code_lane`;
- the 5-fragment file in ≤ 2 clusters: `fragments_of_a_five_fragment_file_stay_in_at_most_two_clusters`;
- range view reuses the tip's placement and context windows start minimized: `read_side_routes_on_a_scripted_repo`
  (`sharedPlacement` is the snapshot view, same world and columns, every cluster rect equal, every context window
  `minimized`, expanding a pill un-minimizes it in place). In the browser, panels-flow on cv serve checks "range view
  shows only changed + context windows" and "context pill expands in place" (F3 evidence below).

Round 5 (layout5, 2026-10-19; `cargo test -p cv-layout`, `-p cv-server --test golden_layout --test incremental`):
- heat graded per lane (DESIGN §4.10.3: Hot holds the first 50% of the lane's own change mass, columns stay shared):
  `heat_is_graded_per_lane`;
- label carry-over (own lane first, then any lane, after a lanes change): `labels_carry_over_into_another_lane`;
- new files: a new-file cluster at the lane's right edge, wrapping inside the band, surviving windows keep their rects:
  `new_and_deleted_files_update_the_view` (incremental, extended); `resolve_overlaps` keeps a grown frame clear of
  neighbouring clusters: `a_grown_frame_keeps_clear_of_neighbouring_clusters` (golden_layout);
- `.cv/grades.toml`: an unknown table keeps every default (`grades::defaults_partial_tables_and_bad_input`).

## B7: Server (`cv-server`)

- **Provides:**
  - every route in `docs/contracts/http.ts`;
  - the WS protocol in `docs/contracts/ws.ts`, including `workspace.updated`;
  - `EventLog`;
  - `IndexScheduler` (viewport-first);
  - token auth;
  - immutable cache headers on blobs;
  - the `cv` CLI.
- **Acceptance:**
  - Contract tests replay C0 fixtures through the real server.
  - WS reconnect with a cursor replays missed events.
  - A new jj op yields `op.changed` and then `view.invalidated` within 2 s.

Evidence (2026-10-18):
- Contract replay of the C0 fixtures through `cv serve`: `scripts/contract-replay.sh` (see C0).
- On a clone of the reference repo, from `jj status` creating the op (n=10, medians with p95 in brackets):
  - `op.changed` arrives after 120 ms (236 ms);
  - `view.invalidated` after 357 ms (1,257 ms);
  - the client has repainted the changed text after 424 ms (max 1,835 ms) ✓.
- Source: `scripts/acceptance/repaint-after-op.mjs`.
- `IndexScheduler` (viewport-first), `cv-server/src/scheduler.rs`, DESIGN §4.1 (2026-10-18, conform):
  - The latest `view.state` is the focus. After an op the focused view is refreshed first; inside it, changed files with a
    window in the predicted viewport (viewport ± one viewport) go first, nearest first. With ≥ 64 changed files (or 1 MiB)
    outside that region the refresh is staged: `index.progress { job: "parse", viewId }` and a `view.invalidated` for the
    windows in view, then a second `view.invalidated` for the rest. Both stages run in the op hook, so read-after-write holds.
  - Test: `cargo test -p cv-server --test scheduler` (96 changed files; the first invalidation names exactly the in-view
    files' windows, the second the rest; the staged result equals a plain refresh; no focus → one pass, no progress).
  - On a clone of the reference repo, 300 changed files per op, the viewport over one of them (n=6 each, medians, noisy
    shared machine; `scripts/acceptance/viewport-first.mjs`): the windows in view are invalidated after
    **359 ms** instead of 2,005 ms; the whole refresh ends after 3.3 s instead of 2.0 s (the second stage pays one more view
    copy and tile-grid rebuild, hence the 64-file threshold).
  - Not scheduled: the first full index of a spec (global layout, no viewport before the bootstrap; `cv serve` still
    indexes `@` before it listens, so its `index.progress` reaches no client). Serving `ViewRef.status: "indexing"` while
    the first index runs needs UI support (F2) and is deferred.
- Second-audit fixes (2026-10-19, `crates/cv-server/tests/evidence_fixes.rs`, `session_expiry.rs`, `workspaces.rs`):
  - N6: snapshot views resolve `rev` with the user's revset aliases (`trunk()`), as ranges do; a revset naming 0 or
    several commits is a 400 `revset-invalid`.
  - N7: `GET …/fragments` and the bootstrap serve the live overlay, so new unsnapshotted files have fragments and
    clusters, as their tiles do.
  - N8: a workspace whose `jj workspace add` checkout is still running (tree state = empty tree) is not flagged stale
    and its files are not counted as edits (a checkout of the reference repo, 5,697 files, took 0.5 s here and ≥ 6 s in the audit on a
    loaded machine; the card showed stale with 97 "unsnapshotted" files meanwhile). A stale card counts 0 unsnapshotted files (they are not overlaid)
    and names the number in its note.
  - N9: per-session agent toggles lapse when the session is gone: no WebSocket open with its `?session=` id and not
    seen or changed for `CV_SESSION_TTL_SECS` (default 600 s, change time persisted, so also across restarts).
    Rows are kept and apply again when the session returns (DESIGN §10).
  - `.cv/grades.toml`: an unknown key voids its whole table, as DESIGN §4.7 says.
- Round 5 (views5, 2026-10-19; `cargo test -p cv-server --test view_specs --test restart_bootstrap`, cv-mock
  `route_tests`, `web/scripts/views-flow.mjs` on cv-mock and cv serve in `scripts/ci-e2e.sh`):
  - view-unknown recovery: an id the server has not loaded answers 404 `view-unknown` on batch, fragments, metrics,
    expand and PATCH layout (GET /views/:id from index.db says `stale`), and the client re-POSTs the spec and reloads in
    place, also after a socket reconnect: `old_view_id_after_restart_says_view_unknown`;
    views-flow "a 404 “view-unknown” re-opens the view";
  - per-session permissions: `agent.permissions` and the camera toggle are per UI session, and the checkbox shows the
    stored row (`diff_fileset_range_options_unknown_views_and_session_permissions` on cv-mock; views-flow
    "camera-follow on in tab A stays off in tab B", "after a reload, tab A's camera checkbox shows the stored
    permission (on)");
  - snapshot spec options: `fileset`, `liveWorkingCopy: false` and `options.workspace` are honoured, each its own view
    (rev defaults to `<ws>@`), bad values 400: `snapshot_spec_options_are_honoured`; range options outside
    `RangeOptions` 400: `search_finds_threads_and_range_options_are_checked`;
  - overlaps count each stack's own commits (Workspaces and Compare): `overlaps_count_each_stacks_own_changes`;
  - thread hits in search on cv serve: `search_finds_threads_and_range_options_are_checked`, views-flow "search lists
    review threads";
  - ETag on GET /views/:id, `cv serve --help` (`serve_help_prints_usage_without_serving`).
- Caveats:
  - `view.invalidated` can lag `op.changed` by up to 1.5 s on large files.
  - The 2 s only holds once the page is idle. Arriving at L3 over a dense cluster can block the main thread for 2–7 s
    (edge tessellation and GC), which delays WS handling.

## B8: Review & edits (`cv-review`)

- **Provides:** `ReviewService`, `EditService` (exact preview through `Vcs::preview`; **amend by default**, branch for immutable targets, absorb in ranges), the mutation queue, conflict tracking and re-anchoring.
- **Key behaviour:** DESIGN §6.
- **Acceptance:**
  - Re-anchoring handles moved lines, changed lines (outdated with the last good commit) and abandoned changes.
  - A suggestion amends exactly its lines.
  - The preview equals the applied result on the golden "insert conflicts" scenario.
  - Stale base and immutable targets are handled as specified.

Evidence (2026-10-19, all pass): `cargo test --release -p cv-review` (13: `anchor::moved_lines_follow`,
`changed_lines_are_outdated`, `block_moved_elsewhere_is_found_by_fingerprint`, `patches_replace_insert_and_delete`,
`suggestion_blocks_parse`, event replay/long-poll; integration `reanchoring_moves_outdates_and_handles_abandon`,
`suggestion_amends_exactly_its_lines_and_the_thread_follows`, `stale_base_merges_or_rejects_and_immutable_only_branches`,
`conflicts_are_tracked_resolved_and_reported`; safety: `apply_refuses_a_preview_that_no_longer_holds`,
`undo_after_external_describe_is_blocked_and_changes_nothing`, `a_busy_queue_answers_locked`, …) and
`cargo test --release -p cv-vcs --test write` (11, against the jj CLI as oracle:
`insert_conflicts_preview_equals_apply_then_resolve_and_revert` on the golden "insert conflicts" repo,
`immutable_targets_only_branch`, `preview_never_writes_and_amend_matches_cli`, `absorb_moves_hunks_into_owning_commits`,
…). End to end, review-flow on cv serve (`scripts/ci-e2e.sh`, 34 ok, 0 skip on 2026-10-19) applies a suggestion as an amend and
resolves its thread, applies a range edit and takes a side of a conflict; panels-flow on cv serve undoes an edit and
shows a blocked undo with the later operations.


Round 5 (loop5, 2026-10-19; `cargo test -p cv-review`, `-p cv-server --test mcp_inprocess`):
- an answer with several patches (or a whole-content rewrite) carries the thread's anchor onto the commit that holds the
  edit, and the thread stays addressed through later jj ops: `anchor::anchors_map_through_several_patches`,
  `anchors_map_through_rewritten_texts`, `multi_patch_answer_keeps_the_thread_addressed_through_later_ops`;
- leaving `outdated` re-anchors on the change's current commit: `reopening_an_outdated_thread_reanchors_it`;
- re-anchoring walks the evolog step by step, and a divergent change follows its evolog successor deterministically:
  `divergent_change_reanchors_along_its_evolog`;
- an undo that would create a new conflict is blocked: `undo_that_would_make_a_new_conflict_is_blocked` (safety);
- end to end, review-flow on cv serve with `scripts/scripted-agent.mjs` answers a comment with a multi-patch
  `edit_code` + reply and checks the reply, the new code and the status live, and that the thread stays addressed
  after jj ops. REQUIRE (`conflicts,ask-agent,agent-resolution,agent-answer,fade` on cv serve; all but
  `agent-answer` on cv-mock) turns a skip of those checks into a failure, and jj-backed Rust tests fail under
  `CI`/`CV_REQUIRE_JJ` instead of skipping (docs6 run, 2026-10-19: review-flow on cv serve 34 ok / 0 skip, on cv-mock
  26 ok / 1 skip, `agent-answer`, which needs a real MCP agent and is not required there).
## B9: MCP (`cv-mcp`)

- **Provides:** every tool in `docs/contracts/mcp.ts` on rmcp 3.x, including `get_metrics`, `get_hotspots`, `list_workspaces` and `bind_workspace`; the profiles, resources and prompts; the stdio shim; `agent.attention` with the bound workspace.
- **Acceptance:**
  - An end-to-end script on a golden repo works with a real MCP client: `bind_workspace`, then `wait_for_events`, then `get_thread`, then `apply_suggestion` (amend), then `reply`.
  - The UI then receives `thread.upserted` and `workspace.updated`.

Evidence (2026-10-18): `scripts/acceptance/mcp-apply-suggestion.mjs` runs on a clone of the reference repo, as a plain HTTP MCP
client (no Claude) on `/mcp/full`, since `bind_workspace` is not in the editor profile. Steps:
- `initialize` → `bind_workspace` → `wait_for_events` → `get_thread` → `apply_suggestion` (amend) → `reply`.
- The UI's socket received `thread.upserted`, `edit.updated` (previewed, then applied), `op.changed`, `view.invalidated`,
  `workspace.updated` and `agent.attention`.
- `jj file show` confirms only the suggested line changed. All 14 checks pass ✓.


Round 5 (loop5, 2026-10-19; `mcp_inprocess::agent_answers_previews_creates_files_and_resyncs`): `edit_code` creates
new files without a thread or `baseBlob` (rev defaults to the workspace's `@`); `edit_code {dryRun, reply}` keeps the
reply for `apply_edit`; `list_conflicts` returns 12-character ids that `get_conflict` takes; `get_diff` marks files
conflicted on either side; a spaced fileset works; resync after a missed cursor.
## F1: Canvas engine

- **Provides:**
  - camera, tiers and culling;
  - lane bands with **textures** and sticky label rails;
  - shared column guides and per-lane row guides;
  - heat **hue** and connectedness **elevation**;
  - **lenses** (heat, connectedness, hotspots, co-change);
  - file chips and fragment links;
  - dynamic stubs;
  - edges, ports, hubs and the minimap.
- **Acceptance:**
  - 60 fps on the 20k view.
  - Lens switches never relayout.
  - Each split file's chip has its own colour from the 10-slot palette.
  - Selecting a file makes its chips and outlines more saturated and brighter, draws fragment links, and marks every fragment on the minimap (via `…/fragments`).
  - Both themes.

Evidence (2026-10-19; cv-mock's 20k synthetic view `v_synth20000`, `vite preview`, headless Chrome on an RTX 3090 Ti
via ANGLE/Vulkan, 1600×1000):
- 60 fps on the 20k view ✓: `node scripts/bench.mjs --url …` (all 9 scenarios: pan/zoom/fling at L1–L3, L0→L3 zoom,
  dives): 59.9–60 fps, p95 frame 16.7–16.8 ms, p99 16.8 ms, 0 long tasks; one frame over 20 ms in all 3,688 (33.4 ms,
  `zoom-l0-l3`). Dark theme (`--dark`, pan-l3 and fling-l1): 60 fps, p95 16.7 ms.
- Lens switches never relayout ✓: panels-flow "lenses never move a window" (134 windows compared on cv-mock, 13 on cv
  serve), plus the per-lens effects (heat glow/fade, connectedness elevation and link weights, hotspots, co-change).
- Chips and fragments ✓ (partly): "selecting a split file loads its 5 fragments", "dotted links join the 2 fragment
  pair(s) that ended up apart", "]f / [f", "chip slots shift apart: 13 colliding cells of 529", "selecting a file dims
  the 6 other windows". The palette and the minimap marks are checked too (`web/scripts/panels-flow.mjs`, the D14
  file-chips block): "the chip palette has 10 distinct slots, and 10 brighter ones for the selected file", "file chips
  wear their slot's colour (N drawn, M of the selected file in the bright palette)" (each drawn chip's colour is compared
  with its slot's), "the minimap marks all N fragments of the selected file in its chip colour" and "clearing the
  selection clears the minimap marks".
- Both themes ✓: since layout5, `scripts/ci-e2e.sh` runs panels-flow dark on cv-mock and light (`LIGHT=1`) on cv serve,
  and review-flow light on cv-mock (docs6 runs, 2026-10-19: panels-flow
  108 ok / 5 skip on cv-mock, 114 ok / 4 skip on cv serve, 0 FAIL, no page errors; review-flow on cv-mock 26 ok / 1 skip).
  One of two full ci-e2e runs that day failed mock-panels-flow with a 30 s click timeout on the Inspector tab right
  after "Compare opens both stacks per commit", at load average ~70 from other agents' builds; the mock half re-run at
  load ~20 passed. Treat it as a load flake until it recurs on a quiet machine.

- Round 5 (layout5, vitest): config-field links between 0.65 and 0.80 are drawn faded with "?", tied to the server's
  `SOLID_SCORE` (`edges.test.ts` "link confidence", Rust `the_canvas_fades_config_links_below_solid_score` keeps the
  two thresholds equal); every built-in lane has its own Hotspots mark shape, docs a cross (`hotspotMarks.test.ts`).

## F2: Data client

- **Provides:** tiles and batches with priorities, cancellation and LRU budgets; worker decode; the WS client; `view.state` including `lens` and `selectedFile`.
- **Acceptance:**
  - No requests outside viewport + margin.
  - Cancelled requests abort.
  - Invalidation refetches only what changed.

Evidence (2026-10-19, panels-flow in `scripts/ci-e2e.sh`, all ✓): "no request outside the viewport + margin" (306
requests during flings on the 20k mock view, 1 on cv serve); "requests the camera leaves are aborted in flight" (42
cancelled while running, 41 fetches aborted, 54 dropped from the queue; on the small cv serve map nothing was left in
flight, so that half skips); "view.invalidated refetches only what changed" (cv serve: an op touching 17 windows → 3
fetches of 1 base tile, 9 outline batches, 11 texts; mock: 2 windows → 1 tile, 1 batch, 2 texts); "window cache stays
bounded past its cap" (cap 400: 394 loaded, peak 613).
The CI soak (layout5, `scripts/ci-e2e.sh` step 3, on a fresh cv-mock): `node scripts/soak.mjs --seconds 390
--warmup 120 --gc-every 5 --max-slope 9 --max-retained 380` (`E2E_SOAK_SECONDS` overrides the length). The retained
heap is sampled after a forced full GC once per zoom round (~33 samples) and its least-squares slope is fitted only
after the 120 s warm-up, because the heap fills its caches for about two minutes (~230 → ~280 MB) and then swings
±15 MB with the walk. Calibration (2026-10-19, recorded in the ci-e2e.sh comment): with no leak the slope of any
≥ 240 s window starting at ≥ 90 s was ≤ 4.7 MB/min (3 runs of this exact config: −3.9, −3.5, −2.6 MB/min); with
`soak.mjs --leak 20` (the page retains 20 MB more per minute, 20.0 MB/min on a blank page) the fitted slope was
15.2–22 MB/min (15.9 in this config), so the 9 MB/min bound passes no leak and fails a 20 MB/min one. A 10 MB/min
leak measured 7.0 MB/min and passes: the gate's detection limit lies between a 10 and a 20 MB/min leak.
Two CI soaks in this config on 2026-10-19 (docs6): slope −3.1 MB/min (load average ~70; 247 frames over 33 ms, max
983 ms) and −3.8 MB/min (load ~20–40; 74 over 33 ms, max 250 ms), both PASS; the frame numbers track machine load.
Longer soaks (`node scripts/soak.mjs --url … --seconds N`, same 20k mock view and machine, other agents' builds running):
- 180 s: 11,410 frames, 2 over 33 ms, 0 over 100 ms, max 67 ms; GC max 31.6 ms; heap 182 → 372 MB (last = max);
  windows at the 16,000 cap (peak 15,996), eviction running.
- 600 s (current record, 2026-10-19 07:02–07:12, same view and machine, `--seconds 600`): 37,637 frames, **0 over
  33 ms, 0 over 100 ms, max frame 33 ms**; retained heap after a full GC 244 → 274 MB (23 samples, 271–301 MB from
  minute 2 on; slope 2.2 MB/min), heap peak 376 MB; windows capped (peak 15,992 of 16,000), eviction running; GC
  2,090 events, 8.9 s in total, 63 pauses over 33 ms, the longest 54 ms. These GC numbers include the 23 forced full
  collections the soak runs to sample the retained heap; frames during a forced GC are left out of the frame
  statistics (`soak.mjs` header), so "0 over 33 ms" is for the frames a user would see between them. Log: the
  session scratchpad's `final6/soak.log`.
- An earlier 600 s run (2026-10-19, an older `soak.mjs` that did not yet sample the retained heap, while other
  agents' release builds were running on the same 24-core machine) had 37,002 frames, 122 over 33 ms, 11 over 100 ms, max
  500 ms, 28 GC pauses over 33 ms (longest 467 ms) and the heap at 420–480 MB with spikes to 628 MB. The two runs
  differ mainly in machine load: the GC pauses that stretched frames scale with how much CPU the page gets, and
  that run competed with parallel `cargo build --release` jobs. (The method changed too: the old soak forced no GCs
  and left no frames out.) It is kept as the worst case seen, not as the
  current state. Under heavy load (load average in the hundreds) the soak's frame numbers are not meaningful.

## F3: Code windows & editor

- **Provides:**
  - L3 windows with file chip, district glyph and ↧/↥ badges;
  - fades and expansion;
  - inline threads and suggestions;
  - CodeMirror activation;
  - a save bar that defaults to amend;
  - an exact preview;
  - conflict windows.
- **Acceptance:**
  - Text crossfades over the silhouette.
  - The save flow goes preview, then apply, then `edit.updated`.
  - Immutable targets show "branch" as the only mode.
  - Range-view context windows render as minimized pills and expand in place on click, without relayout.

Evidence (2026-10-19, `scripts/ci-e2e.sh` review-flow and panels-flow, cv-mock and cv serve, 0 FAIL): "L3 text crossfades
over the silhouette (8 frames between 0 and 1)"; save flow: "suggestion applied (amend) and thread resolved", "edit
applied", "edit.updated from another client reloads the edited window's text", "editor offers keep-descendants (amend,
insert-after, branch, keep-descendants)"; "context pill expands in place", "context display expanded/hidden"; conflict
windows: "conflict marker opens the conflict popover", "taking a side resolves the conflict". Immutable targets: the
server side is tested (`cv-vcs immutable_targets_only_branch`, `cv-review stale_base_merges_or_rejects_and_immutable_only_branches`),
and panels-flow checks the save bar: "an immutable target's save bar offers only "branch"" (exactly one option,
`branch`, selected, the "immutable" warning and a description field). It uses a loaded window whose change is
immutable if `/revisions` reports one; otherwise it shapes that change's `/revisions` row immutable ("(immutable
shaped)" in the line). Every recorded run so far, cv serve included, was shaped (the e2e repo shows no immutable
change on screen), so the browser check covers the save bar's reaction to the flag, not the server's flag itself.


Round 5 (2026-10-19):
- loop5: suggestions apply as amend or insert-after (apply, then reply only when applied); outdated threads show the
  last good commit, hide Preview and offer Still applies / In progress / Won't fix; one open predicate for the filter
  and "Send N" (`reviewLogic.test.ts` "thread predicates"); the conflict popover settles every region of a
  multi-region conflict (`resolveRegions` tests; review-flow "took a side in each of N region(s)"); conflict URLs
  encode each path segment (`conflictUrl`).
- layout5: CodeMirror highlights every pack language with lazily loaded grammars (`codeLang.test.ts`: each pack
  language maps to an editor language and its sample highlights).
## F4: Panels & dialogs

- **Provides:** History, Review, **Workspaces** (cards, overlaps, Open @, Review stack), **Hotspots** (scatter, quadrants, brush, ranked list) and Agent tabs; the lens switcher; lane filters; the range dialog; search; the top bar and status bar.
- **Acceptance:**
  - Workspace cards update from `workspace.updated`.
  - The Hotspots brush filters the canvas.
  - The range dialog shows its states: valid, error with hint, gap warning.
  - Usable at 400 px width.

Evidence (2026-10-19, panels-flow in `scripts/ci-e2e.sh`, 0 FAIL): "workspace cards update from workspace.updated"
(cv serve: 6 events), "a new workspace's card appears live", stale and recovered cards; "the Hotspots brush filters the
canvas" (20k mock: 153 clusters brushed, 73 windows outside fade; cv serve: 3 / 4) and "clearing the brush brings every
window back"; range dialog: "invalid revset shows an error with hint", presets, picker, saved ranges, the not-a-chain
dialog; 400 px: "drawer opens over the canvas within the viewport", "closes again", "no page errors at 400 px". The
gap warning is checked in panels-flow: picking two commits with one between them gives "a revset with a gap warns
“Not contiguous”" (the count in the dialog must equal the server's `gap` warning's `missing.length`), then "“Fill
gap” extends the revset to its descendants" and, on cv serve, "…and the gap warning goes away".


Round 5 (views5, 2026-10-19, views-flow on cv-mock and cv serve): the revision chip opens a picker that refuses a bad
revision and keeps the view; the editor at `@-` offers §6's commit-C rows and previews insert-after there; Workspaces
"Open @" opens `<ws>@` with its live (unsnapshotted) files, and the picker's "@" returns to the @ view; History
"Review range" is older::newer whichever row is picked first; search lists review threads (docs6 run,
2026-10-19: views-flow 12 ok on cv-mock, 15 ok on cv serve, 0 skip, 0 FAIL).
## I1: Integration & performance (phase 3)

On the reference repo:

- index the repo;
- browse lanes and districts;
- review a range;
- have an agent bound to its workspace address a thread and resolve a conflict.

Check the DESIGN §9 budgets, and file issues back to the owning workstreams.

Budgets measured (2026-10-18). Setup: a clone of the reference repo with 5,628 files and 28,633 windows, headless Chrome, harness
`scripts/acceptance/run-runtime.sh`.

| Budget | Measured | Result |
|---|---|---|
| Text, cold ≤ 400 ms | median 72 ms, p95 134 ms (n=15) | ✓ |
| Text, cached ≤ 150 ms | rebuilt from the client cache: median 34 ms, p95 140 ms. Mesh still built: median 15 ms, p95 31 ms (n=15 each) | ✓ |
| Canvas after a jj op < 2 s | median 424 ms, max 1,835 ms (n=10) | ✓ |
| Tile query p95 < 10 ms | every non-empty tile at z0–z3 × 3 rounds (2026-10-19, `tile_bench.py`): p95 z0 0.3, z1 0.5–0.8, z2 1.4–1.5, z3 3.5–3.9 ms (median 1.0, max 6.8); was z3 14.9 ms plain / 24 ms gzip before headers were serialized by reference and tiles sent uncompressed | ✓ |
| Churn < 10 s | 1.1–3.0 s for 2,000 commits | ✓ |

Earlier runs of the same harness fail both text budgets at p95 because of multi-second UI frame stalls:
- Seen in those runs: a 10.1 s cached trial with 8.4 s between frames, 3.8 s of GC, and the JS heap at 1 GB after
  15 trials.
- Cause: the window, cell and edge caches are never evicted.
- Owner: F1/F3.
