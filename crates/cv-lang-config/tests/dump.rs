//! Parity harness for config-pack changes: `CV_CONFIG_DUMP=<repo dir>:<out.tsv>` dumps every key
//! fact and symbol the pack produces for the repo's config files (one TSV row each), plus parse
//! time per dialect on stderr. Run it before and after a change and diff the two dumps:
//!
//! ```text
//! CV_CONFIG_DUMP=/path/to/repo:/tmp/before.tsv cargo test --release -p cv-lang-config --test dump -- --nocapture
//! ```
//!
//! `<repo dir>` may also be a `.txt` list of absolute file paths. Without the variable the test
//! does nothing.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use cv_lang::LanguagePack;
use cv_lang_config::ConfigPack;

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if matches!(
            name.as_str(),
            ".jj" | ".git" | "node_modules" | "target" | ".venv" | "__pycache__"
        ) {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            walk(root, &p, out);
        } else if ft.is_file() {
            out.push(p.strip_prefix(root).unwrap().to_string_lossy().to_string());
        }
    }
}

#[test]
fn dump() {
    let Ok(spec) = std::env::var("CV_CONFIG_DUMP") else {
        return;
    };
    let (dir, out) = spec
        .rsplit_once(':')
        .expect("CV_CONFIG_DUMP=<dir>:<out.tsv>");
    // A `.txt` instead of a directory: a list of files (absolute paths) to dump.
    let (root, mut files) = if dir.ends_with(".txt") {
        let list = std::fs::read_to_string(dir).unwrap();
        (
            Path::new("/"),
            list.lines()
                .map(|l| l.trim_start_matches('/').to_string())
                .collect(),
        )
    } else {
        let mut files = Vec::new();
        walk(Path::new(dir), Path::new(dir), &mut files);
        (Path::new(dir), files)
    };
    files.sort();
    let pack = ConfigPack;
    let mut tsv = String::new();
    let mut time: BTreeMap<String, (usize, Duration)> = BTreeMap::new();
    for f in files.iter().filter(|f| pack.claims(f)) {
        let Ok(src) = std::fs::read_to_string(root.join(f)) else {
            continue;
        };
        let t = Instant::now();
        let facts = pack.analyze(f, &src);
        let ext = f.rsplit('/').next().unwrap_or(f);
        let ext = ext
            .rsplit_once('.')
            .map(|(_, e)| e)
            .unwrap_or(ext)
            .to_lowercase();
        let e = time.entry(ext).or_default();
        e.0 += 1;
        e.1 += t.elapsed();
        for k in &facts.config_keys {
            let v = k.value.as_deref().unwrap_or("").replace(['\n', '\t'], "⏎");
            writeln!(
                tsv,
                "K\t{f}\t{}\t{}\t{}\t{v}\t{}",
                k.path, k.key, k.line, k.item
            )
            .unwrap();
        }
        for s in &facts.symbols {
            writeln!(
                tsv,
                "S\t{f}\t{}\t{}\t{}\t{}\t{}\t{}\t{:?}",
                s.name,
                s.qualname,
                s.kind.as_str(),
                s.start,
                s.end,
                s.name_line,
                s.parent
            )
            .unwrap();
        }
    }
    std::fs::write(out, tsv).unwrap();
    for (ext, (n, d)) in time {
        eprintln!("{ext:>12} {n:>6} files {:>10.1} ms", d.as_secs_f64() * 1e3);
    }
}
