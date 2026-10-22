//! cv-review's integration tests as one test binary (`--test it`): linking a binary per file
//! dominated `cargo test --no-run`. Each file is a module, so filters work as before (a test name,
//! or `<file>::` for one file: `cargo test -p cv-review --test it <file>::`).

mod review;
mod safety;

/// `autotests = false`: a new file in tests/ only runs once it is a module here (or a `[[test]]`).
#[test]
fn every_test_file_is_built() {
    let src = include_str!("main.rs");
    let manifest = include_str!("../Cargo.toml");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    for e in std::fs::read_dir(dir).unwrap() {
        let name = e.unwrap().file_name().into_string().unwrap();
        let Some(stem) = name.strip_suffix(".rs") else {
            continue;
        };
        if stem == "main" {
            continue;
        }
        assert!(
            src.contains(&format!("\nmod {stem};\n"))
                || manifest.contains(&format!("path = \"tests/{name}\"")),
            "tests/{name} is not built: add `mod {stem};` to tests/main.rs"
        );
    }
}
