//! P3 end to end with the Rust pack: `std::env::var`, `env::var_os` (after `use std::env`), `env!`,
//! `option_env!` and a constant name read variables that `.env`, a Dockerfile and a shell script
//! define; each variable is one `env:NAME` hub.

use cv_channel::{FileIn, Project, Target};
use cv_lang::LanguagePack;

const MAIN_RS: &str = r#"use std::env;

const UI_VAR: &str = "CV_UI_DIR";

fn main() {
    let token = std::env::var("CV_TOKEN").ok();
    let ui = env::var_os(UI_VAR);
    let v = env!("CARGO_PKG_VERSION");
    let dbg = option_env!("CV_DEBUG");
    let none = std::env::var(name_at_runtime());
}
"#;

#[test]
fn rust_env_readers_meet_definers() {
    let config = cv_lang_config::ConfigPack;
    let mut langs = cv_lang::Registry::new();
    langs.register(Box::new(cv_lang_rust::RustPack::default()));
    let mut chan = cv_channel::Registry::new();
    chan.register(Box::new(cv_channel_env::EnvPack::new()));
    let dotenv = config.analyze(".env", "CV_TOKEN=secret\n").config_keys;
    let docker = config
        .analyze("Dockerfile", "FROM rust\nENV CV_UI_DIR=/srv/ui\n")
        .config_keys;
    let shell = config
        .analyze("scripts/run.sh", "#!/bin/sh\nexport CV_DEBUG=1\n")
        .config_keys;
    let file = |path: &'static str, text: &'static str, keys| FileIn {
        path,
        lane: "code",
        lang: None,
        conv: Default::default(),
        text,
        config_keys: keys,
        imports: vec![],
        symbols: vec![],
    };
    let mut files = vec![
        file("src/main.rs", MAIN_RS, &[]),
        file(".env", "", &dotenv),
        file("Dockerfile", "", &docker),
        file("scripts/run.sh", "", &shell),
    ];
    files[0].lang = Some("rust");
    let project = Project::new(&files);
    let out = chan.run(&langs, &project, &[Some(0), None, None, None]);
    let mut hubs: Vec<&str> = out.hubs.iter().map(|h| h.display.as_str()).collect();
    hubs.sort_unstable();
    assert_eq!(
        hubs,
        vec!["env:CV_DEBUG", "env:CV_TOKEN", "env:CV_UI_DIR"],
        "{:?}",
        out.notes
    );
    let mut links: Vec<(usize, u32, usize, u32)> = out
        .links
        .iter()
        .filter_map(|l| match l.to {
            Target::Site { file, line } => Some((l.from_file, l.from_line, file, line)),
            _ => None,
        })
        .collect();
    links.sort_unstable();
    assert_eq!(links, vec![(1, 1, 0, 6), (2, 2, 0, 7), (3, 2, 0, 9)]);
    // The runtime-built name ends in a stub.
    assert!(
        out.links
            .iter()
            .any(|l| l.from_line == 10 && matches!(l.to, Target::Dynamic { .. }))
    );
}
