//! L3 config, infra and prose files (DESIGN §4.2). Each grammar-backed dialect parses with its
//! tree-sitter grammar and reads symbols, keys and highlight tokens with `.scm` queries
//! (`queries/<dialect>/{symbols,keys,highlights}.scm`, see [`sitter`]):
//!
//! | Dialect | Grammar | Module |
//! |---|---|---|
//! | YAML | tree-sitter-yaml | [`yaml`]: top-level keys; compose `services` / CI `jobs` children |
//! | TOML | tree-sitter-toml-ng | [`toml`]: `[table]` headers |
//! | JSON | tree-sitter-json | [`json`]: keys only |
//! | Dockerfile | tree-sitter-dockerfile-updated | [`docker`]: one stage per `FROM` |
//! | shell (`.sh`, `.bash`) | tree-sitter-bash | [`shell`]: functions |
//! | HCL / Terraform | tree-sitter-hcl | [`hcl`]: blocks named as Terraform refers to them |
//!
//! Every key path is emitted with its line (`ConfigKeyFact`, DESIGN §4.6). A line scanner
//! ([`keys`], `analyze`) covers the dialects without a grammar crate here (INI/.env, Makefiles and
//! justfiles, Markdown headings) and is the fallback for a YAML document, Dockerfile, script or HCL
//! file its grammar cannot parse cleanly (templated YAML, Dockerfile forms the grammar does not
//! know), so such files keep their keys.

mod docker;
mod hcl;
mod hcl_scan;
mod json;
mod keys;
mod shell;
mod sitter;
mod toml;
mod yaml;

use cv_lang::{
    BlobFacts, Conventions, ImportFact, LanguagePack, ProjectIndex, SymbolFact, SymbolKind,
    split_lines, token,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Dialect {
    Yaml,
    Toml,
    Ini,
    Json,
    Dockerfile,
    Shell,
    Markdown,
    Hcl,
}

fn dialect(path: &str) -> Option<Dialect> {
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    let ext = |e: &str| name.ends_with(e);
    Some(if ext(".yaml") || ext(".yml") {
        Dialect::Yaml
    } else if ext(".toml") {
        Dialect::Toml
    } else if ext(".ini")
        || ext(".cfg")
        || ext(".conf")
        || ext(".properties")
        || name.starts_with(".env")
        || name == ".gitignore"
        || name == ".dockerignore"
        || name == ".python-version"
    {
        Dialect::Ini
    } else if ext(".json") || ext(".jsonc") {
        Dialect::Json
    } else if name.starts_with("dockerfile") || ext(".dockerfile") {
        Dialect::Dockerfile
    } else if ext(".sh") || ext(".bash") || name == "makefile" || name == "justfile" {
        Dialect::Shell
    } else if ext(".tf") || ext(".tfvars") || ext(".hcl") {
        Dialect::Hcl
    } else if ext(".md") || ext(".markdown") || ext(".rst") || ext(".txt") {
        Dialect::Markdown
    } else {
        return None;
    })
}

/// A shell script (Makefiles and justfiles share the shell dialect but have no grammar here).
fn is_script(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    name.ends_with(".sh") || name.ends_with(".bash")
}

pub struct ConfigPack;

impl LanguagePack for ConfigPack {
    fn id(&self) -> &'static str {
        "config"
    }
    fn version(&self) -> &'static str {
        "3"
    }
    fn claims(&self, path: &str) -> bool {
        dialect(path).is_some()
    }
    fn lang(&self, path: &str) -> &'static str {
        match dialect(path) {
            Some(Dialect::Yaml) => "yaml",
            Some(Dialect::Toml) => "toml",
            Some(Dialect::Json) => "json",
            Some(Dialect::Markdown) => "markdown",
            _ => "other",
        }
    }
    fn analyze(&self, path: &str, source: &str) -> BlobFacts {
        let d = dialect(path).unwrap_or(Dialect::Ini);
        // Grammar-backed dialects (DESIGN §4.2). YAML, Dockerfiles and scripts the grammar cannot
        // parse cleanly keep the line scanner; HCL falls back inside `hcl`.
        let parsed = match d {
            Dialect::Hcl => Some(hcl::analyze(source)),
            Dialect::Toml => Some(toml::analyze(source)),
            Dialect::Json => Some(json::analyze(source)),
            Dialect::Yaml => yaml::analyze(source),
            Dialect::Dockerfile => docker::analyze(source),
            Dialect::Shell if is_script(path) => shell::analyze(source),
            _ => None,
        };
        parsed.unwrap_or_else(|| scanned(d, path, source))
    }
    fn resolve_import(&self, _: &str, _: &ImportFact, _: &dyn ProjectIndex) -> Option<String> {
        None
    }
    fn conventions(&self) -> Conventions {
        Conventions {
            // `docs/index.md`, Terraform's root module `main.tf`: named after their directory.
            entry_stems: &["index", "main"],
            ..Conventions::NONE
        }
    }
}

/// The line scanner: INI/.env, Markdown, Makefiles and justfiles, and the fallback for files a
/// grammar cannot parse cleanly.
fn scanned(d: Dialect, path: &str, source: &str) -> BlobFacts {
    let mut f = analyze(d, source);
    f.config_keys = keys::keys(d, path, source);
    if d == Dialect::Yaml {
        nest_groups(&mut f);
    }
    f
}

fn analyze(d: Dialect, source: &str) -> BlobFacts {
    let lines = split_lines(source);
    let mut tokens = Vec::with_capacity(lines.len());
    let mut heads: Vec<(u32, String)> = Vec::new();
    let mut in_fence = false;
    for (i, l) in lines.iter().enumerate() {
        let n = i as u32 + 1;
        let mut t = Line::default();
        match d {
            Dialect::Markdown => {
                let s = l.trim_start();
                if s.starts_with("```") || s.starts_with("~~~") {
                    in_fence = !in_fence;
                    t.span(l, 0, l.len(), token::STRING);
                } else if in_fence {
                    t.span(l, 0, l.len(), token::STRING);
                } else if s.starts_with('#') {
                    t.span(l, 0, l.len(), token::KEYWORD);
                    let level = s.chars().take_while(|&c| c == '#').count();
                    if level <= 2 {
                        heads.push((n, s.trim_start_matches('#').trim().to_string()));
                    }
                } else {
                    inline_code(l, &mut t);
                }
            }
            _ => scan_line(d, l, &mut t, n, &mut heads),
        }
        tokens.push(t.toks);
    }
    let total = lines.len() as u32;
    let mut symbols = Vec::new();
    for (k, (start, name)) in heads.iter().enumerate() {
        let next = heads.get(k + 1).map(|h| h.0 - 1).unwrap_or(total);
        let name = if name.is_empty() {
            format!("L{start}")
        } else {
            name.chars().take(80).collect()
        };
        symbols.push(SymbolFact {
            kind: SymbolKind::ConfigBlock,
            qualname: name.clone(),
            name,
            parent: None,
            start: *start,
            end: next.max(*start),
            name_line: *start,
        });
    }
    BlobFacts {
        lines: total,
        symbols,
        tokens,
        ..Default::default()
    }
}

/// Top-level `services:` (compose) and `jobs:` (CI) blocks become containers with one symbol per
/// service or job, from the second-level key facts.
pub(crate) fn nest_groups(f: &mut BlobFacts) {
    let mut add = Vec::new();
    for (si, s) in f.symbols.iter().enumerate() {
        if s.name != "services" && s.name != "jobs" {
            continue;
        }
        let kids: Vec<(u32, String)> = f
            .config_keys
            .iter()
            .filter(|k| !k.item && k.line > s.start && k.line <= s.end)
            .filter_map(|k| {
                let rest = k.path.strip_prefix(&s.name)?.strip_prefix('.')?;
                (!rest.contains('.')).then(|| (k.line, rest.to_string()))
            })
            .collect();
        for (i, (line, name)) in kids.iter().enumerate() {
            let end = kids.get(i + 1).map(|k| k.0 - 1).unwrap_or(s.end);
            add.push((si, *line, end.max(*line), name.clone()));
        }
    }
    for (parent, start, end, name) in add {
        f.symbols[parent].kind = SymbolKind::Module;
        let qualname = format!("{}.{name}", f.symbols[parent].name);
        f.symbols.push(SymbolFact {
            kind: SymbolKind::ConfigBlock,
            name,
            qualname,
            parent: Some(parent),
            start,
            end,
            name_line: start,
        });
    }
}

#[derive(Default)]
struct Line {
    toks: Vec<u32>,
}

impl Line {
    /// Byte range [a, b) of `l` → UTF-16 triple.
    fn span(&mut self, l: &str, a: usize, b: usize, cls: u32) {
        if b <= a {
            return;
        }
        let col = l[..a].encode_utf16().count() as u32;
        let len = l[a..b].encode_utf16().count() as u32;
        self.toks.extend([col, len, cls]);
    }
}

fn inline_code(l: &str, t: &mut Line) {
    let mut start = None;
    for (i, c) in l.char_indices() {
        if c == '`' {
            match start {
                None => start = Some(i),
                Some(s) => {
                    t.span(l, s, i + 1, token::STRING);
                    start = None;
                }
            }
        }
    }
}

fn scan_line(d: Dialect, l: &str, t: &mut Line, n: u32, heads: &mut Vec<(u32, String)>) {
    let b = l.as_bytes();
    let indent = l.len() - l.trim_start().len();
    let s = &l[indent..];
    if s.is_empty() {
        return;
    }
    let comment_char = Some(b'#');
    if comment_char.is_some_and(|c| s.as_bytes()[0] == c)
        || (d == Dialect::Ini && s.starts_with(';'))
    {
        t.span(l, indent, l.len(), token::COMMENT);
        return;
    }
    let mut i = indent;
    // Line heads: keys, sections, instructions, functions.
    match d {
        Dialect::Yaml => {
            if s.starts_with("- ") {
                t.span(l, i, i + 1, token::PUNCTUATION);
                i += 2;
            }
            if let Some(k) = key_end(l, i, b':')
                && (k + 1 == l.len() || b[k + 1] == b' ')
            {
                t.span(l, i, k, token::PROPERTY);
                t.span(l, k, k + 1, token::PUNCTUATION);
                if indent == 0 && !s.starts_with('-') {
                    heads.push((n, l[..k].trim().to_string()));
                }
                i = k + 1;
            }
        }
        Dialect::Toml | Dialect::Ini => {
            if s.starts_with('[') {
                t.span(l, indent, l.len(), token::TYPE);
                heads.push((
                    n,
                    s.trim_matches(|c| c == '[' || c == ']' || c == ' ')
                        .to_string(),
                ));
                return;
            }
            if let Some(k) = key_end(l, i, b'=').or_else(|| {
                if d == Dialect::Ini {
                    key_end(l, i, b':')
                } else {
                    None
                }
            }) {
                t.span(l, i, k, token::PROPERTY);
                t.span(l, k, k + 1, token::OPERATOR);
                i = k + 1;
            }
        }
        Dialect::Dockerfile => {
            let w = s.split_whitespace().next().unwrap_or("");
            if w.chars().all(|c| c.is_ascii_uppercase()) && !w.is_empty() {
                t.span(l, i, i + w.len(), token::KEYWORD);
                if w == "FROM" {
                    heads.push((n, s.to_string()));
                }
                i += w.len();
            }
        }
        Dialect::Shell => {
            if indent == 0
                && let Some(p) = s.find("()")
                && s[..p]
                    .trim()
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == ' ')
            {
                let name = s[..p].trim().trim_start_matches("function ").trim();
                if !name.is_empty() {
                    heads.push((n, name.to_string()));
                    t.span(l, indent, indent + p, token::FUNCTION);
                    i = indent + p;
                }
            } else if indent == 0
                && let Some(rest) = s.strip_prefix("function ")
                && let Some(name) = rest.split_whitespace().next()
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
            {
                // `function name {`: the bash/ksh form without parentheses.
                heads.push((n, name.to_string()));
                let at = "function ".len() + (rest.len() - rest.trim_start().len());
                t.span(l, 0, "function".len(), token::KEYWORD);
                t.span(l, at, at + name.len(), token::FUNCTION);
                i = at + name.len();
            } else if indent == 0 && d == Dialect::Shell && s.ends_with(':') && !s.contains(' ') {
                // Makefile / justfile targets.
                heads.push((n, s.trim_end_matches(':').to_string()));
                t.span(l, i, l.len(), token::FUNCTION);
                return;
            }
        }
        _ => {}
    }
    // Values: strings, numbers, constants, trailing comments.
    while i < b.len() {
        let c = b[i];
        if c == b'"' || c == b'\'' {
            let mut j = i + 1;
            while j < b.len() && b[j] != c {
                if b[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            let end = (j + 1).min(b.len());
            t.span(l, i, end, token::STRING);
            i = end;
        } else if comment_char == Some(c) && (i == 0 || b[i - 1] == b' ') {
            t.span(l, i, l.len(), token::COMMENT);
            return;
        } else if c.is_ascii_digit()
            && (i == 0 || !b[i - 1].is_ascii_alphanumeric() && b[i - 1] != b'_')
        {
            let mut j = i;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'.' || b[j] == b'_') {
                j += 1;
            }
            t.span(l, i, j, token::NUMBER);
            i = j;
        } else if c.is_ascii_alphabetic() {
            let mut j = i;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            if matches!(
                &l[i..j],
                "true" | "false" | "null" | "yes" | "no" | "True" | "False" | "None" | "on" | "off"
            ) && (i == 0 || !b[i - 1].is_ascii_alphanumeric())
            {
                t.span(l, i, j, token::CONSTANT);
            } else if d == Dialect::Shell
                && matches!(
                    &l[i..j],
                    "if" | "then"
                        | "else"
                        | "fi"
                        | "for"
                        | "do"
                        | "done"
                        | "case"
                        | "esac"
                        | "while"
                        | "function"
                        | "return"
                        | "export"
                        | "local"
                        | "echo"
                        | "set"
                )
            {
                t.span(l, i, j, token::KEYWORD);
            }
            i = j;
        } else {
            i += 1;
        }
    }
}

/// Byte index of the separator ending a bare key that starts at `i` (`key:` / `key =`).
fn key_end(l: &str, i: usize, sep: u8) -> Option<usize> {
    let b = l.as_bytes();
    let mut j = i;
    while j < b.len() {
        let c = b[j];
        if c == sep {
            return (j > i).then_some(j);
        }
        if !(c.is_ascii_alphanumeric()
            || matches!(
                c,
                b'_' | b'-' | b'.' | b' ' | b'"' | b'\'' | b'/' | b'$' | b'{' | b'}'
            ))
        {
            return None;
        }
        j += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_keys_comments_and_blocks() {
        let f = analyze(
            Dialect::Yaml,
            "# top\nserver:\n  port: 8080 # p\n  name: \"x\"\nworkers:\n  - a\n",
        );
        assert_eq!(f.tokens[0], vec![0, 5, token::COMMENT]);
        assert_eq!(
            f.tokens[2],
            vec![
                2,
                4,
                token::PROPERTY,
                6,
                1,
                token::PUNCTUATION,
                8,
                4,
                token::NUMBER,
                13,
                3,
                token::COMMENT
            ]
        );
        let s: Vec<(&str, u32, u32)> = f
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.start, s.end))
            .collect();
        assert_eq!(s, vec![("server", 2, 4), ("workers", 5, 6)]);
    }

    #[test]
    fn markdown_headings_become_blocks() {
        let f = analyze(
            Dialect::Markdown,
            "# T\ntext `x`\n## A\n```\n# not\n```\n### deep\n",
        );
        let s: Vec<(&str, u32, u32)> = f
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.start, s.end))
            .collect();
        assert_eq!(s, vec![("T", 1, 2), ("A", 3, 7)]);
        assert_eq!(f.tokens[1], vec![5, 3, token::STRING]);
    }

    #[test]
    fn compose_services_become_symbols() {
        let f = ConfigPack.analyze(
            "docker-compose.yml",
            "services:\n  api:\n    image: x\n    environment:\n      - PORT=1\n  db:\n    image: y\n",
        );
        let s: Vec<(&str, u32, u32, Option<usize>)> = f
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.start, s.end, s.parent))
            .collect();
        assert_eq!(
            s,
            vec![
                ("services", 1, 7, None),
                ("api", 2, 5, Some(0)),
                ("db", 6, 7, Some(0))
            ]
        );
        assert!(
            f.config_keys
                .iter()
                .any(|k| k.item && k.path == "services.api.environment")
        );
    }

    fn spans(f: &BlobFacts) -> Vec<(&str, u32, u32, Option<usize>)> {
        f.symbols
            .iter()
            .map(|s| (s.name.as_str(), s.start, s.end, s.parent))
            .collect()
    }

    #[test]
    fn dockerfile_stages_become_symbols() {
        let f = ConfigPack.analyze(
            "Dockerfile",
            "# syntax=docker/dockerfile:1\nFROM python:3.12 AS build\nRUN pip install .\n\nFROM gcr.io/distroless AS run\nCOPY --from=build /app /app\nCMD [\"main.py\"]\n",
        );
        assert_eq!(
            spans(&f),
            vec![
                ("FROM python:3.12 AS build", 2, 4, None),
                ("FROM gcr.io/distroless AS run", 5, 7, None)
            ]
        );
        assert_eq!(f.tokens[1][..3], [0, 4, token::KEYWORD]);
    }

    #[test]
    fn shell_functions_become_symbols() {
        let f = ConfigPack.analyze(
            "scripts/dev.sh",
            "#!/bin/sh\nset -e\nsetup() {\n  echo hi\n}\nfunction run_tests() {\n  pytest tests/\n}\nsetup\n",
        );
        assert_eq!(
            spans(&f),
            vec![("setup", 3, 5, None), ("run_tests", 6, 9, None)]
        );
        assert_eq!(f.tokens[2][..3], [0, 5, token::FUNCTION]);
    }

    #[test]
    fn ci_jobs_become_symbols() {
        let f = ConfigPack.analyze(
            ".github/workflows/ci.yml",
            "name: ci\non: [push]\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    steps:\n      - run: ruff check\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - run: pytest tests/\n",
        );
        assert_eq!(
            spans(&f),
            vec![
                ("name", 1, 1, None),
                ("on", 2, 2, None),
                ("jobs", 3, 11, None),
                ("lint", 4, 7, Some(2)),
                ("test", 8, 11, Some(2))
            ]
        );
        assert_eq!(f.symbols[4].qualname, "jobs.test");
        assert_eq!(f.symbols[2].kind, SymbolKind::Module);
    }

    #[test]
    fn yaml_folded_and_literal_blocks_are_values_not_keys() {
        let f = ConfigPack.analyze(
            "agent/prompt.yaml",
            "system: >\n  You are: an agent\n  tone: dry\nuser: |-\n  q: what\nafter: 1\n",
        );
        let k: Vec<(&str, u32)> = f
            .config_keys
            .iter()
            .map(|k| (k.path.as_str(), k.line))
            .collect();
        assert_eq!(k, vec![("system", 1), ("user", 4), ("after", 6)]);
        let v = f.config_keys[0].value.as_deref().unwrap();
        assert!(v.contains("You are: an agent"), "{v}");
        assert_eq!(f.config_keys[1].value.as_deref(), Some("q: what"));
    }

    #[test]
    fn terraform_files_use_the_hcl_dialect() {
        let p = ConfigPack;
        assert!(p.claims("infra/main.tf") && p.claims("prod.tfvars") && p.claims("x.hcl"));
        assert_eq!(p.lang("infra/main.tf"), "other");
        let f = p.analyze(
            "infra/main.tf",
            "resource \"aws_s3_bucket\" \"logs\" {\n  bucket = \"logs\"\n}\n",
        );
        assert_eq!(spans(&f), vec![("aws_s3_bucket.logs", 1, 3, None)]);
        assert_eq!(f.config_keys[0].path, "aws_s3_bucket.logs.bucket");
        let v = p.analyze("prod.tfvars", "region = \"eu\"\n");
        assert_eq!(v.config_keys[0].path, "region");
        assert_eq!(v.config_keys[0].value.as_deref(), Some("eu"));
    }

    #[test]
    fn claims_by_name() {
        let p = ConfigPack;
        assert!(p.claims("a/Dockerfile.dev"));
        assert!(p.claims(".env.example"));
        assert_eq!(p.lang("x/conf.yml"), "yaml");
        assert!(!p.claims("x.py"));
    }
}
