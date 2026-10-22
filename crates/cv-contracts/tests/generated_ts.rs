//! D3 golden test: the TypeScript generated from the wire types must equal the committed copies in
//! `docs/contracts/generated/` and `web/src/contracts/generated/`. With `CV_GEN_CONTRACTS=1` (what
//! `scripts/gen-contracts.sh` sets) it writes them instead.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

const OUT_DIRS: [&str; 2] = ["docs/contracts/generated", "web/src/contracts/generated"];

/// `check.ts` from the hand-written spec in `docs/contracts/*.ts` and the allow-list of known
/// divergences.
fn check_file() -> String {
    let dir = repo_root().join("docs/contracts");
    let mut specs: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let stem = name.strip_suffix(".ts")?.to_string();
            Some((stem, std::fs::read_to_string(e.path()).ok()?))
        })
        .collect();
    specs.sort();
    let div =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("ts-divergences.txt"))
            .unwrap_or_default();
    cv_contracts::ts::check_ts(&specs, &cv_contracts::ts::parse_divergences(&div))
}

#[test]
fn generated_ts_is_current() {
    let mut files = cv_contracts::ts::generate();
    files.push(("check.ts".into(), check_file()));
    let write = std::env::var("CV_GEN_CONTRACTS").is_ok_and(|v| v == "1");
    let mut stale = Vec::new();
    for dir in OUT_DIRS {
        let dir = repo_root().join(dir);
        if write {
            std::fs::create_dir_all(&dir).unwrap();
            // Drop files a removed module left behind.
            for e in std::fs::read_dir(&dir).unwrap().flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.ends_with(".ts") && !files.iter().any(|(f, _)| *f == name) {
                    std::fs::remove_file(e.path()).unwrap();
                }
            }
        }
        for (name, text) in &files {
            let path = dir.join(name);
            if write {
                std::fs::write(&path, text).unwrap();
            } else if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
                stale.push(path.display().to_string());
            }
        }
    }
    assert!(
        stale.is_empty(),
        "generated TypeScript is stale; run scripts/gen-contracts.sh and commit the result:\n  {}",
        stale.join("\n  ")
    );
}

/// Every type that derives `TS` in the crate must be listed in `ts::MODULES`, so a new wire type
/// cannot silently miss the generated output.
#[test]
fn every_ts_type_is_listed() {
    let names = cv_contracts::ts::declared_names();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut missing = Vec::new();
    for e in std::fs::read_dir(&src).unwrap().flatten() {
        let text = std::fs::read_to_string(e.path()).unwrap();
        let mut derives_ts = false;
        for line in text.lines() {
            let t = line.trim();
            if t.starts_with("#[derive(") {
                derives_ts = t.contains(" TS)") || t.contains(" TS,") || t.contains("(TS");
            } else if derives_ts && (t.starts_with("pub struct ") || t.starts_with("pub enum ")) {
                let name = t.split_whitespace().nth(2).unwrap_or("");
                let name: String = name
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !names.contains(&name) {
                    missing.push(format!("{} ({})", name, e.file_name().to_string_lossy()));
                }
                derives_ts = false;
            }
        }
    }
    assert!(
        missing.is_empty(),
        "add these types to MODULES in crates/cv-contracts/src/ts.rs: {missing:?}"
    );
}

/// The web app compiles against hand copies of the spec (`web/src/contracts/*.ts`): they must be
/// byte-identical to `docs/contracts/*.ts`, file for file (D3: the docs copy is the spec).
#[test]
fn web_spec_copies_match_docs() {
    let ts_files = |dir: &str| -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = std::fs::read_dir(repo_root().join(dir))
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".ts"))
            .map(|n| {
                let text = std::fs::read_to_string(repo_root().join(dir).join(&n)).unwrap();
                (n, text)
            })
            .collect();
        v.sort();
        v
    };
    let docs = ts_files("docs/contracts");
    let web = ts_files("web/src/contracts");
    let names = |v: &[(String, String)]| v.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
    assert_eq!(
        names(&docs),
        names(&web),
        "docs/contracts and web/src/contracts hold different spec files"
    );
    let differ: Vec<&String> = docs
        .iter()
        .zip(&web)
        .filter(|(d, w)| d.1 != w.1)
        .map(|(d, _)| &d.0)
        .collect();
    assert!(
        differ.is_empty(),
        "web/src/contracts differs from docs/contracts (the spec) in {differ:?}; copy docs/contracts/<file>.ts over the web copy"
    );
}
