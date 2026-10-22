//! L3 key facts (`ConfigKeyFact`, DESIGN §4.6) of the line scanner: `.env`/INI and Makefiles, and
//! the fallback for YAML, Dockerfiles and shell scripts their grammar cannot parse cleanly (the
//! grammar-backed readers are `yaml.rs`, `toml.rs`, `json.rs`, `docker.rs`, `shell.rs`, `hcl.rs`;
//! they share this module's value helpers).
//!
//! - YAML: an indent-aware line reader that skips `|`/`>` block text (the block becomes the key's
//!   value). Key paths follow `docs/analysis/config_fields.py` exactly (list items add no segment),
//!   so config-field scores reproduce the reference. Plain list items are reported as `item`s.
//! - `.env`/INI: `KEY=value` (with `[section]` prefixes for INI).
//! - Dockerfile: `ENV.NAME`, `ARG.NAME`, and `CMD`/`ENTRYPOINT`/`RUN` with the command as value.
//! - Shell: `export.NAME` and one `sh` fact per command line.

use cv_lang::keys::ConfigKeyFact;

use crate::Dialect;

pub(crate) fn keys(d: Dialect, path: &str, source: &str) -> Vec<ConfigKeyFact> {
    match d {
        Dialect::Yaml => yaml_keys(source),
        Dialect::Ini => ini_keys(path, source),
        Dialect::Dockerfile => docker_keys(source),
        Dialect::Shell => shell_keys(source),
        // Grammar-backed only (`toml.rs`, `json.rs`, `hcl.rs`), or no keys.
        Dialect::Toml | Dialect::Json | Dialect::Hcl | Dialect::Markdown => vec![],
    }
}

fn fact(path: String, line: u32, value: Option<String>) -> ConfigKeyFact {
    let key = path.rsplit('.').next().unwrap_or(&path).to_string();
    keyed(path, key, line, value)
}

fn keyed(path: String, key: String, line: u32, value: Option<String>) -> ConfigKeyFact {
    ConfigKeyFact {
        path,
        key,
        line,
        value,
        item: false,
    }
}

// ── YAML ─────────────────────────────────────────────────────────────────────

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `^(\s*)(-\s+)?([A-Za-z_][\w.-]*)\s*:(?:\s+(.*))?$` → (indent, dash length, key, value).
fn yaml_key_line(line: &str) -> Option<(usize, usize, &str, Option<&str>)> {
    let trimmed = line.trim_start();
    let indent = line.chars().count() - trimmed.chars().count();
    let mut rest = trimmed;
    let mut dash = 0;
    if let Some(r) = rest.strip_prefix('-') {
        let r2 = r.trim_start();
        if r2.len() < r.len() {
            dash = 1 + (r.chars().count() - r2.chars().count());
            rest = r2;
        }
    }
    let mut chars = rest.char_indices();
    let (_, first) = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let mut end = rest.len();
    for (i, c) in chars {
        if !(is_word(c) || c == '.' || c == '-') {
            end = i;
            break;
        }
    }
    let key = &rest[..end];
    let after = rest[end..].trim_start();
    let after = after.strip_prefix(':')?;
    if after.is_empty() {
        return Some((indent, dash, key, None));
    }
    let v = after.trim_start();
    if v.len() == after.len() {
        return None; // `key:value` is not a key
    }
    Some((indent, dash, key, Some(v)))
}

pub(crate) fn unquote(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2
        && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')))
    {
        return v[1..v.len() - 1].to_string();
    }
    // Trailing comment on a plain scalar.
    match v.find(" #") {
        Some(i) if !v.starts_with('"') && !v.starts_with('\'') => v[..i].trim_end().to_string(),
        _ => v.to_string(),
    }
}

pub(crate) fn yaml_keys(text: &str) -> Vec<ConfigKeyFact> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<ConfigKeyFact> = Vec::new();
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut block: Option<(usize, usize)> = None; // (indent, index of the owning fact)
    let mut block_text: Vec<&str> = Vec::new();
    let mut block_min: usize = usize::MAX;
    let flush =
        |out: &mut Vec<ConfigKeyFact>, b: Option<(usize, usize)>, t: &mut Vec<&str>, m: usize| {
            if let Some((_, fi)) = b {
                let s: Vec<String> = t
                    .iter()
                    .map(|l| l.chars().skip(m.min(l.chars().count())).collect::<String>())
                    .collect();
                let joined = s.join("\n").trim_end().to_string();
                out[fi].value = (!joined.is_empty()).then_some(joined);
            }
            t.clear();
        };
    for (i, raw) in lines.iter().enumerate() {
        let no = i as u32 + 1;
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            if block.is_some() && line.trim().is_empty() {
                block_text.push("");
            }
            continue;
        }
        let indent = line.chars().count() - line.trim_start().chars().count();
        if let Some((b, _)) = block {
            if indent > b {
                block_text.push(line);
                block_min = block_min.min(indent);
                continue;
            }
            flush(&mut out, block, &mut block_text, block_min);
            block = None;
        }
        let Some((ind, dash, key, value)) = yaml_key_line(line) else {
            // A plain list item: `- value`.
            if let Some(v) = line.trim_start().strip_prefix("- ") {
                let eff = indent + 2;
                let path: Vec<&str> = stack
                    .iter()
                    .filter(|(e, _)| *e < eff)
                    .map(|(_, k)| k.as_str())
                    .collect();
                out.push(ConfigKeyFact {
                    path: path.join("."),
                    key: String::new(),
                    line: no,
                    value: Some(unquote(v)),
                    item: true,
                });
            }
            continue;
        };
        let eff = ind + dash;
        while stack.last().is_some_and(|(e, _)| *e >= eff) {
            stack.pop();
        }
        stack.push((eff, key.to_string()));
        let path: Vec<&str> = stack.iter().map(|(_, k)| k.as_str()).collect();
        let v = value.map(str::trim).unwrap_or("");
        let is_block = v.starts_with('|') || v.starts_with('>');
        out.push(keyed(
            path.join("."),
            key.to_string(),
            no,
            (!v.is_empty() && !is_block).then(|| unquote(v)),
        ));
        if is_block {
            block = Some((eff, out.len() - 1));
            block_min = usize::MAX;
        }
    }
    flush(&mut out, block, &mut block_text, block_min);
    out
}

// ── TOML ─────────────────────────────────────────────────────────────────────

pub(crate) fn ident_key(k: &str) -> bool {
    let mut c = k.chars();
    c.next()
        .is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
        && c.all(|x| is_word(x) || x == '.' || x == '-')
}

pub(crate) fn toml_value_str(v: &toml_edit::Value) -> Option<String> {
    match v {
        toml_edit::Value::String(s) => Some(s.value().clone()),
        toml_edit::Value::Integer(i) => Some(i.value().to_string()),
        toml_edit::Value::Float(f) => Some(f.value().to_string()),
        toml_edit::Value::Boolean(b) => Some(b.value().to_string()),
        toml_edit::Value::Array(a) => {
            let parts: Vec<String> = a.iter().filter_map(toml_value_str).collect();
            Some(parts.join(" "))
        }
        _ => None,
    }
}

// ── .env / INI ───────────────────────────────────────────────────────────────

fn ini_keys(path: &str, text: &str) -> Vec<ConfigKeyFact> {
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    if name.starts_with(".gitignore") || name.starts_with(".dockerignore") {
        return vec![];
    }
    let dotenv = name.starts_with(".env");
    let mut out = Vec::new();
    let mut section = String::new();
    for (i, l) in text.lines().enumerate() {
        let s = l.trim();
        // A commented-out assignment in a dotenv file documents an optional variable.
        if dotenv
            && let Some(c) = s.strip_prefix('#')
            && let Some((k, v)) = c.trim_start().split_once('=')
            && k.starts_with(|c: char| c.is_ascii_uppercase())
            && k.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            out.push(keyed(
                format!("#.{k}"),
                k.to_string(),
                i as u32 + 1,
                Some(unquote(v)),
            ));
            continue;
        }
        if s.is_empty() || s.starts_with('#') || s.starts_with(';') {
            continue;
        }
        if s.starts_with('[') && s.ends_with(']') {
            section = s.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            continue;
        }
        let s = s.strip_prefix("export ").unwrap_or(s).trim_start();
        let Some(p) = s.find(['=', ':']) else {
            continue;
        };
        let key = s[..p].trim();
        if key.is_empty() || !key.chars().all(|c| is_word(c) || c == '.' || c == '-') {
            continue;
        }
        let path = if section.is_empty() {
            key.to_string()
        } else {
            format!("{section}.{key}")
        };
        let v = unquote(&s[p + 1..]);
        out.push(fact(path, i as u32 + 1, (!v.is_empty()).then_some(v)));
    }
    out
}

// ── Dockerfile ───────────────────────────────────────────────────────────────

/// Logical lines (backslash continuations joined) with their first line number.
pub(crate) fn logical_lines(text: &str, comment: char) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    let mut cur: Option<(u32, String)> = None;
    for (i, l) in text.lines().enumerate() {
        let t = l.trim();
        if cur.is_none() && (t.is_empty() || t.starts_with(comment)) {
            continue;
        }
        if cur.is_some() && t.starts_with(comment) {
            continue;
        }
        let (cont, body) = match t.strip_suffix('\\') {
            Some(b) => (true, b.trim_end()),
            None => (false, t),
        };
        match &mut cur {
            Some((_, s)) => {
                s.push(' ');
                s.push_str(body);
            }
            None => cur = Some((i as u32 + 1, body.to_string())),
        }
        if !cont && let Some(c) = cur.take() {
            out.push(c);
        }
    }
    out.extend(cur);
    out
}

/// `["a", "b"]` (exec form) → `a b`; anything else unchanged.
pub(crate) fn exec_form(v: &str) -> String {
    let t = v.trim();
    if t.starts_with('[')
        && let Ok(parts) = serde_json::from_str::<Vec<String>>(t)
    {
        return parts.join(" ");
    }
    t.to_string()
}

pub(crate) fn docker_keys(text: &str) -> Vec<ConfigKeyFact> {
    let mut out = Vec::new();
    for (line, l) in logical_lines(text, '#') {
        let (instr, rest) = match l.split_once(char::is_whitespace) {
            Some((a, b)) => (a.to_ascii_uppercase(), b.trim()),
            None => (l.to_ascii_uppercase(), ""),
        };
        match instr.as_str() {
            "ENV" | "ARG" => {
                if rest.contains('=') {
                    for kv in shell_words(rest) {
                        if let Some((k, v)) = kv.split_once('=') {
                            out.push(fact(format!("{instr}.{k}"), line, Some(unquote(v))));
                        } else if instr == "ARG" {
                            out.push(fact(format!("ARG.{kv}"), line, None));
                        }
                    }
                } else if let Some((k, v)) = rest.split_once(char::is_whitespace) {
                    out.push(fact(format!("{instr}.{k}"), line, Some(unquote(v))));
                } else if !rest.is_empty() {
                    out.push(fact(format!("{instr}.{rest}"), line, None));
                }
            }
            "CMD" | "ENTRYPOINT" | "RUN" => out.push(fact(instr, line, Some(exec_form(rest)))),
            _ => {}
        }
    }
    out
}

/// Splits on whitespace outside quotes.
fn shell_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => {
                quote = None;
                cur.push(c);
            }
            (None, '"' | '\'') => {
                quote = Some(c);
                cur.push(c);
            }
            (None, c) if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

// ── Shell ────────────────────────────────────────────────────────────────────

pub(crate) fn shell_keys(text: &str) -> Vec<ConfigKeyFact> {
    let mut out = Vec::new();
    for (line, l) in logical_lines(text, '#') {
        if let Some(rest) = l.strip_prefix("export ") {
            for kv in shell_words(rest) {
                if let Some((k, v)) = kv.split_once('=')
                    && !k.is_empty()
                    && k.chars().all(is_word)
                {
                    out.push(fact(format!("export.{k}"), line, Some(unquote(v))));
                }
            }
            continue;
        }
        out.push(fact("sh".into(), line, Some(l)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(f: &[ConfigKeyFact]) -> Vec<(String, u32, Option<String>)> {
        f.iter()
            .filter(|k| !k.item)
            .map(|k| (k.path.clone(), k.line, k.value.clone()))
            .collect()
    }

    #[test]
    fn yaml_is_indent_aware_and_skips_block_text() {
        let src = "name: x\npool:\n  - id: a\n    params:\n      topic: \"w.e.moved\"\n    prompt: |\n      not: a key\n      still: text\n  - id: b\nenv:\n  - FOO=1\nlast: 2 # c\n";
        let f = yaml_keys(src);
        assert_eq!(
            paths(&f),
            vec![
                ("name".into(), 1, Some("x".into())),
                ("pool".into(), 2, None),
                ("pool.id".into(), 3, Some("a".into())),
                ("pool.params".into(), 4, None),
                ("pool.params.topic".into(), 5, Some("w.e.moved".into())),
                (
                    "pool.prompt".into(),
                    6,
                    Some("not: a key\nstill: text".into())
                ),
                ("pool.id".into(), 9, Some("b".into())),
                ("env".into(), 10, None),
                ("last".into(), 12, Some("2".into())),
            ]
        );
        let items: Vec<(&str, &str)> = f
            .iter()
            .filter(|k| k.item)
            .map(|k| (k.path.as_str(), k.value.as_deref().unwrap()))
            .collect();
        assert_eq!(items, vec![("env", "FOO=1")]);
        assert!(yaml_key_line("key:value").is_none());
        assert!(yaml_key_line("- http://x").is_none());
    }

    #[test]
    fn env_docker_and_shell() {
        let e = ini_keys(".env.example", "# c\nA=1\nexport B=\"two\"\n#C=3\n");
        assert_eq!(
            paths(&e),
            vec![
                ("A".into(), 2, Some("1".into())),
                ("B".into(), 3, Some("two".into())),
                ("#.C".into(), 4, Some("3".into()))
            ]
        );
        let d = docker_keys(
            "FROM x AS b\nENV A=1 B=\"2 3\"\nARG V\nCMD [\"uv\", \"run\", \"python\", \"main.py\"]\nRUN a \\\n  && b\n",
        );
        assert_eq!(
            paths(&d),
            vec![
                ("ENV.A".into(), 2, Some("1".into())),
                ("ENV.B".into(), 2, Some("2 3".into())),
                ("ARG.V".into(), 3, None),
                ("CMD".into(), 4, Some("uv run python main.py".into())),
                ("RUN".into(), 5, Some("a && b".into())),
            ]
        );
        let s = shell_keys("#!/bin/sh\nexport PORT=8000\nuv run pytest tests/\n");
        assert_eq!(
            paths(&s),
            vec![
                ("export.PORT".into(), 2, Some("8000".into())),
                ("sh".into(), 3, Some("uv run pytest tests/".into()))
            ]
        );
    }
}
