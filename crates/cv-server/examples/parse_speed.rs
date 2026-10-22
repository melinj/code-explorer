//! Language-pack acceptance check (WORKSTREAMS L1–L5): median time per 1k lines over the files of
//! each language under the given folders, wall clock, best of 3 runs per file. The acceptance figure
//! is the tree-sitter grammar parse (< 5 ms; C++ is an accepted exception of its grammar); the full
//! `analyze` (parse, symbols, refs, imports, tokens) is shown alongside for tracking.
//!
//! cargo run --release -p cv-server --example parse_speed -- DIR [DIR…] [--min-lines 20]

use std::collections::BTreeMap;
use std::path::Path;

/// Wall-clock milliseconds (thread CPU counters in /proc update too coarsely for single files);
/// best-of-3 per file keeps scheduler noise out.
fn cpu_ms() -> f64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f64()
        * 1000.0
}

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.')
            || matches!(
                &*name,
                "node_modules" | "target" | "dist" | ".venv" | "venv"
            )
        {
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
    let args: Vec<String> = std::env::args().skip(1).collect();
    let min_lines: usize = args
        .iter()
        .position(|a| a == "--min-lines")
        .and_then(|i| args.get(i + 1)?.parse().ok())
        .unwrap_or(20);
    let dirs: Vec<&String> = args
        .iter()
        .enumerate()
        .filter(|(i, a)| !a.starts_with("--") && (*i == 0 || args[i - 1] != "--min-lines"))
        .map(|(_, a)| a)
        .collect();
    let reg = cv_server::index::registry();
    let mut files = Vec::new();
    for d in &dirs {
        walk(Path::new(d), &mut files);
    }
    let mut per_lang: BTreeMap<&'static str, Vec<f64>> = BTreeMap::new();
    // Raw tree-sitter parse of the same files, where the grammar is known.
    let grammar = |lang: &str| -> Option<tree_sitter::Language> {
        Some(match lang {
            "python" => tree_sitter_python::LANGUAGE.into(),
            "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            "tsx" | "javascript" => tree_sitter_typescript::LANGUAGE_TSX.into(),
            "rust" => tree_sitter_rust::LANGUAGE.into(),
            "cpp" | "c" => tree_sitter_cpp::LANGUAGE.into(),
            _ => return None,
        })
    };
    let mut raw: BTreeMap<&'static str, Vec<f64>> = BTreeMap::new();
    for f in &files {
        let path = f.to_string_lossy();
        let Some((_, pack)) = reg.pack_for(&path) else {
            continue;
        };
        let Ok(src) = std::fs::read_to_string(f) else {
            continue;
        };
        let lines = src.lines().count();
        if lines < min_lines || src.len() > 1 << 20 {
            continue;
        }
        let best = (0..3)
            .map(|_| {
                let t = cpu_ms();
                std::hint::black_box(pack.analyze(&path, &src));
                cpu_ms() - t
            })
            .fold(f64::MAX, f64::min);
        let lang = pack.lang(&path);
        per_lang
            .entry(lang)
            .or_default()
            .push(best / (lines as f64 / 1000.0));
        if let Some(g) = grammar(lang) {
            let mut p = tree_sitter::Parser::new();
            p.set_language(&g).unwrap();
            let best = (0..3)
                .map(|_| {
                    let t = cpu_ms();
                    std::hint::black_box(p.parse(&src, None));
                    cpu_ms() - t
                })
                .fold(f64::MAX, f64::min);
            raw.entry(lang)
                .or_default()
                .push(best / (lines as f64 / 1000.0));
        }
    }
    println!(
        "{:<12} {:>6} {:>12} {:>10} {:>10} {:>14}",
        "lang", "files", "full median", "full p90", "grammar", "grammar < 5 ms"
    );
    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    for (lang, mut v) in per_lang {
        v.sort_by(f64::total_cmp);
        let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
        let g = raw.get_mut(lang).map(median);
        let verdict = match g {
            None => "–",
            Some(m) if m < 5.0 => "ok",
            Some(_) if lang == "cpp" || lang == "c" => "exception",
            Some(_) => "MISS",
        };
        let g = g.map(|m| format!("{m:.2}ms")).unwrap_or_else(|| "–".into());
        println!(
            "{lang:<12} {:>6} {:>10.2}ms {:>8.2}ms {g:>10} {verdict:>14}",
            v.len(),
            q(0.5),
            q(0.9)
        );
    }
}
