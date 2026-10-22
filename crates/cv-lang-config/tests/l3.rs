//! L3 acceptance (WORKSTREAMS L1–L5, L3 row): Dockerfile stages, shell functions, CI jobs and
//! YAML `>`/`|` block scalars, through the public `LanguagePack` surface.

use cv_lang::LanguagePack;
use cv_lang::keys::ConfigKeyFact;
use cv_lang_config::ConfigPack;

fn syms(path: &str, src: &str) -> Vec<(String, &'static str, u32, u32, Option<usize>)> {
    ConfigPack
        .analyze(path, src)
        .symbols
        .iter()
        .map(|s| {
            (
                s.qualname.clone(),
                s.kind.as_str(),
                s.start,
                s.end,
                s.parent,
            )
        })
        .collect()
}

fn owned(
    v: &[(&str, &'static str, u32, u32, Option<usize>)],
) -> Vec<(String, &'static str, u32, u32, Option<usize>)> {
    v.iter()
        .map(|&(a, b, c, d, e)| (a.to_string(), b, c, d, e))
        .collect()
}

fn keys(path: &str, src: &str) -> Vec<(String, u32, Option<String>)> {
    ConfigPack
        .analyze(path, src)
        .config_keys
        .iter()
        .filter(|k: &&ConfigKeyFact| !k.item)
        .map(|k| (k.path.clone(), k.line, k.value.clone()))
        .collect()
}

/// Every `FROM` starts a stage window that runs to the line before the next `FROM`.
#[test]
fn dockerfile_stages_become_blocks() {
    let src = "\
# syntax=docker/dockerfile:1
ARG PY=3.12
FROM python:${PY}-slim AS base
ENV PYTHONUNBUFFERED=1
WORKDIR /app

FROM base AS builder
COPY pyproject.toml uv.lock ./
RUN uv sync --frozen

FROM base AS runtime
COPY --from=builder /app/.venv /app/.venv
CMD [\"python\", \"main.py\"]
";
    assert_eq!(
        syms("deploy/Dockerfile.prod", src),
        owned(&[
            ("FROM python:${PY}-slim AS base", "config-block", 3, 6, None),
            ("FROM base AS builder", "config-block", 7, 10, None),
            ("FROM base AS runtime", "config-block", 11, 13, None),
        ])
    );
    let k = keys("deploy/Dockerfile.prod", src);
    assert!(k.contains(&("ENV.PYTHONUNBUFFERED".into(), 4, Some("1".into()))));
    assert!(k.contains(&("CMD".into(), 13, Some("python main.py".into()))));
}

/// Both function forms: `name() {` and `function name {` (plus `function name() {`).
#[test]
fn shell_functions_both_forms() {
    let src = "\
#!/usr/bin/env bash
set -euo pipefail

log() {
  echo \"$*\" >&2
}

function build {
  log building
  cargo build
}

function deploy_all() {
  build
}

deploy_all
";
    assert_eq!(
        syms("scripts/release.sh", src),
        owned(&[
            ("log", "config-block", 4, 7, None),
            ("build", "config-block", 8, 12, None),
            ("deploy_all", "config-block", 13, 17, None),
        ])
    );
}

/// `.github/workflows/*.yml`: `jobs` is a container with one block per job.
#[test]
fn ci_jobs_become_symbols() {
    let src = "\
name: CI
on:
  push:
    branches: [main]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: uv run pytest tests/
  lint:
    runs-on: ubuntu-latest
    steps:
      - run: ruff check .
";
    assert_eq!(
        syms(".github/workflows/ci.yml", src),
        owned(&[
            ("name", "config-block", 1, 1, None),
            ("on", "config-block", 2, 4, None),
            ("jobs", "module", 5, 14, None),
            ("jobs.test", "config-block", 6, 10, Some(2)),
            ("jobs.lint", "config-block", 11, 14, Some(2)),
        ])
    );
}

/// `>` (folded) and `|` (literal) blocks, with chomping indicators, are values: their lines never
/// become key paths, and keys after the block (deeper, equal or shallower) are read again.
#[test]
fn yaml_block_scalars_are_values_not_keys() {
    let src = "\
prompt:
  system: >
    You are: a helper.
    key: not a key
  user: |-
    name: also text

    after_blank: still text
  temperature: 0.2
notes: >+
  trailing: text

model: gpt
";
    assert_eq!(
        keys("agents/prompt.yaml", src),
        vec![
            ("prompt".into(), 1, None),
            (
                "prompt.system".into(),
                2,
                Some("You are: a helper.\nkey: not a key".into())
            ),
            (
                "prompt.user".into(),
                5,
                Some("name: also text\n\nafter_blank: still text".into())
            ),
            ("prompt.temperature".into(), 9, Some("0.2".into())),
            ("notes".into(), 10, Some("trailing: text".into())),
            ("model".into(), 13, Some("gpt".into())),
        ]
    );
    let s = syms("agents/prompt.yaml", src);
    let names: Vec<&str> = s.iter().map(|x| x.0.as_str()).collect();
    assert_eq!(names, vec!["prompt", "notes", "model"]);
}
