//! P4 acceptance: CI `run:` steps link to the tests they run (single-line steps, `python -m
//! pytest`, and `|` blocks), with key facts from the config pack as in the index pipeline.

use cv_channel::{FileIn, Project, Target};
use cv_lang::LanguagePack;

const CI: &str = "name: ci
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: uv run pytest -n 4 tests/test_api.py
      - run: python -m pytest tests/
      - name: unit
        run: |
          uv sync
          uv run pytest tests/unit/test_x.py::test_one
      - run: echo done
";

#[test]
fn ci_run_steps_link_to_tests() {
    let langs = cv_lang::Registry::new();
    let mut chan = cv_channel::Registry::new();
    chan.register(Box::new(cv_channel_deploy::DeployPack::new()));
    let ci_keys = cv_lang_config::ConfigPack.analyze(".github/workflows/ci.yml", CI);
    let paths = [
        ("tests/conftest.py", "test"),
        ("tests/test_api.py", "test"),
        ("tests/unit/test_x.py", "test"),
    ];
    let mut files = vec![FileIn {
        path: ".github/workflows/ci.yml",
        lane: "infra",
        lang: Some("config"),
        conv: Default::default(),
        text: CI,
        config_keys: &ci_keys.config_keys,
        imports: vec![],
        symbols: vec![],
    }];
    for (path, lane) in paths {
        files.push(FileIn {
            path,
            lane,
            lang: None,
            conv: Default::default(),
            text: "def test_one():\n    pass\n",
            config_keys: &[],
            imports: vec![],
            symbols: vec![],
        });
    }
    let project = Project::new(&files);
    let out = chan.run(&langs, &project, &vec![None; files.len()]);
    let mut got: Vec<(u32, &str)> = out
        .links
        .iter()
        .filter(|l| l.from_file == 0)
        .filter_map(|l| match l.to {
            Target::Site { file, .. } => Some((l.from_line, files[file].path)),
            _ => None,
        })
        .collect();
    got.sort_unstable();
    got.dedup();
    assert_eq!(
        got,
        vec![
            (8, "tests/test_api.py"),
            (9, "tests/conftest.py"),
            (11, "tests/unit/test_x.py"),
        ],
        "{:?}",
        out.notes
    );
    assert!(out.links.iter().all(|l| l.kind == "deploy"));
}
