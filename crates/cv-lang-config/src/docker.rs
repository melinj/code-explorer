//! Dockerfiles on tree-sitter-dockerfile (`queries/dockerfile/*.scm`, the maintained
//! `tree-sitter-dockerfile-updated` crate).
//!
//! - **Symbols:** one build stage per `FROM`; a stage runs to the next one.
//! - **Keys** (`keys.scm`): `ENV.NAME` and `ARG.NAME` with their values, and `CMD`, `ENTRYPOINT`,
//!   `RUN` valued with their command (exec form `["a", "b"]` → `a b`; continuation lines joined).
//!
//! The grammar does not know every Dockerfile form (legacy `ENV NAME value with spaces`, newer
//! flags such as `COPY --parents`, `ENV NAME=` with no value) and recovers from them by dropping
//! the rest of the file, so a Dockerfile it cannot parse cleanly falls back to the line scanner
//! (`keys::docker_keys`).

use std::collections::HashMap;

use cv_lang::BlobFacts;
use cv_lang::keys::ConfigKeyFact;
use cv_lang_sitter::{Text, count_errors};
use tree_sitter::Node;

use crate::keys::{exec_form, logical_lines, unquote};
use crate::sitter::{self, Grammar, cap};
use crate::yaml::blocks;

/// `None` when the file does not parse cleanly (the caller falls back to the scanner).
pub(crate) fn analyze(source: &str) -> Option<BlobFacts> {
    let g = sitter::dockerfile();
    let text = Text::new(source);
    let lines = text.line_count();
    // The grammar wants a final newline.
    let owned;
    let parsed = if source.ends_with('\n') {
        source
    } else {
        owned = format!("{source}\n");
        owned.as_str()
    };
    let tree = g.parse(parsed)?;
    let root = tree.root_node();
    if root.has_error() {
        return None;
    }
    let src_lines: Vec<&str> = source.split('\n').collect();
    let heads: Vec<(u32, String)> = Grammar::matches(&g.symbols, root, parsed)
        .into_iter()
        .filter_map(|(_, caps)| {
            let s = cap(&caps, "symbol")?;
            let row = s.start_position().row;
            let l = src_lines.get(row)?.trim_end_matches('\r');
            Some((row as u32 + 1, l.trim_start().to_string()))
        })
        .collect();
    Some(BlobFacts {
        lines,
        symbols: blocks(&heads, lines),
        tokens: g.tokens(root, &text),
        syntax_errors: count_errors(root),
        config_keys: keys(g, root, parsed),
        ..Default::default()
    })
}

fn fact(path: String, key: &str, line: u32, value: Option<String>) -> ConfigKeyFact {
    ConfigKeyFact {
        path,
        key: key.to_string(),
        line,
        value,
        item: false,
    }
}

fn keys(g: &'static Grammar, root: Node, src: &str) -> Vec<ConfigKeyFact> {
    let logical: HashMap<u32, String> = logical_lines(src, '#').into_iter().collect();
    let line = |n: Node| n.start_position().row as u32 + 1;
    let mut out = Vec::new();
    for (_, caps) in Grammar::matches(&g.keys, root, src) {
        let name = cap(&caps, "name").map(|n| &src[n.byte_range()]);
        let value = cap(&caps, "value").map(|v| unquote(&src[v.byte_range()]));
        if let (Some(i), Some(name)) = (cap(&caps, "env"), name) {
            out.push(fact(format!("ENV.{name}"), name, line(i), value));
        } else if let (Some(i), Some(name)) = (cap(&caps, "arg"), name) {
            out.push(fact(format!("ARG.{name}"), name, line(i), value));
        } else if let Some(i) = cap(&caps, "command") {
            let instr = match i.kind() {
                "cmd_instruction" => "CMD",
                "entrypoint_instruction" => "ENTRYPOINT",
                _ => "RUN",
            };
            let rest = logical
                .get(&line(i))
                .and_then(|l| l.split_once(char::is_whitespace))
                .map(|(_, r)| r.trim())
                .unwrap_or("");
            out.push(fact(instr.into(), instr, line(i), Some(exec_form(rest))));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_become_keys_and_stages() {
        let src = "ARG PY=3.12\nFROM python:${PY} AS base\nENV A=1 B=\"2 3\"\nRUN a \\\n  # c\n  && b\nHEALTHCHECK CMD curl -f http://x/ || exit 1\nFROM base\nCMD [\"uv\", \"run\", \"main.py\"]";
        let f = analyze(src).unwrap();
        let got: Vec<(&str, u32, Option<&str>)> = f
            .config_keys
            .iter()
            .map(|k| (k.path.as_str(), k.line, k.value.as_deref()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("ARG.PY", 1, Some("3.12")),
                ("ENV.A", 3, Some("1")),
                ("ENV.B", 3, Some("2 3")),
                ("RUN", 4, Some("a && b")),
                ("CMD", 9, Some("uv run main.py")),
            ]
        );
        let s: Vec<(&str, u32, u32)> = f
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.start, s.end))
            .collect();
        assert_eq!(
            s,
            vec![("FROM python:${PY} AS base", 2, 7), ("FROM base", 8, 9)]
        );
        // The legacy `ENV NAME value with spaces` form is not in the grammar: scanner fallback.
        assert!(analyze("FROM x\nENV MSG hello world\nRUN a\n").is_none());
    }
}
