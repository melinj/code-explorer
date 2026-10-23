//! cv-server's integration tests as one test binary (`--test it`): linking a binary per file
//! dominated `cargo test --no-run`. Each file is a module, so filters work as before (a test name,
//! or `<file>::` for one file: `cargo test -p cv-server --test it <file>::`).
//! Separate binaries (they switch process-wide state: the parse cache / index.db location, the
//! session clock): `parse_cache`, `restart_bootstrap`, `session_expiry`, `view_store` (Cargo.toml `[[test]]`).

mod channels;
mod contract;
mod evidence_fixes;
mod extras;
mod git_mode;
mod golden_layout;
mod graph_ops;
mod incremental;
mod lanes;
mod layout;
mod layout_misses;
mod link_tiers;
mod mcp_inprocess;
mod mcp_subscriptions;
mod readside;
mod review_api;
mod review_records;
mod safety_api;
mod scheduler;
mod view;
mod view_specs;
mod workspaces;

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
