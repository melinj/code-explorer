//! Parity check for language-pack changes: dumps every pack's full `BlobFacts` (symbols, refs,
//! imports, highlight tokens, syntax errors, config keys) for each file under the given folders,
//! one record per file, sorted by path. Run it before and after a pack change and diff the
//! outputs (DESIGN §4.2).
//!
//! cargo run --release -p cv-server --example facts_dump -- DIR [DIR…] > facts.txt

use std::io::Write;
use std::path::Path;

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || matches!(&*name, "node_modules" | "target" | "dist") {
            continue;
        }
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn main() {
    let reg = cv_server::index::registry();
    let mut files = Vec::new();
    for d in std::env::args().skip(1) {
        walk(Path::new(&d), &mut files);
    }
    files.sort();
    let out = std::io::stdout();
    let mut out = std::io::BufWriter::new(out.lock());
    for f in &files {
        let path = f.to_string_lossy();
        let Some((_, pack)) = reg.pack_for(&path) else {
            continue;
        };
        let Ok(src) = std::fs::read_to_string(f) else {
            continue;
        };
        if src.len() > 1 << 20 {
            continue;
        }
        let facts = pack.analyze(&path, &src);
        writeln!(out, "== {path} [{}]", pack.id()).unwrap();
        writeln!(out, "lines={} errors={}", facts.lines, facts.syntax_errors).unwrap();
        for s in &facts.symbols {
            writeln!(out, "sym {s:?}").unwrap();
        }
        for i in &facts.imports {
            writeln!(out, "imp {i:?}").unwrap();
        }
        for r in &facts.refs {
            writeln!(out, "ref {r:?}").unwrap();
        }
        for k in &facts.config_keys {
            writeln!(out, "key {k:?}").unwrap();
        }
        for (i, t) in facts.tokens.iter().enumerate() {
            if !t.is_empty() {
                writeln!(out, "tok {} {t:?}", i + 1).unwrap();
            }
        }
    }
}
