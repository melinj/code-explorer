//! Shell scripts (`.sh`, `.bash`) on tree-sitter-bash (`queries/bash/*.scm`).
//!
//! - **Symbols:** top-level function definitions; a block runs to the next one.
//! - **Keys** (`keys.scm`): `export.NAME` per exported assignment, `env.NAME` per environment
//!   prefix of a command (`NAME=value cmd …`), and one `sh` fact per logical line (backslash
//!   continuations joined) on which a command starts, valued with that line, for the deploy pack.
//!
//! A script the grammar cannot parse cleanly falls back to the line scanner (`keys::shell_keys`),
//! which reads every non-comment line.

use cv_lang::BlobFacts;
use cv_lang::keys::ConfigKeyFact;
use cv_lang_sitter::{Text, count_errors};
use tree_sitter::Node;

use crate::keys::{logical_lines, shell_keys, unquote};
use crate::sitter::{self, Grammar, cap};
use crate::yaml::blocks;

/// `None` when the script does not parse cleanly (the caller falls back to the scanner).
pub(crate) fn analyze(source: &str) -> Option<BlobFacts> {
    let g = sitter::bash();
    let text = Text::new(source);
    let lines = text.line_count();
    let tree = g.parse(source)?;
    let root = tree.root_node();
    if root.has_error() {
        return None;
    }
    let heads: Vec<(u32, String)> = Grammar::matches(&g.symbols, root, source)
        .into_iter()
        .filter_map(|(_, caps)| {
            let (s, n) = (cap(&caps, "symbol")?, cap(&caps, "name")?);
            (s.start_position().column == 0).then(|| {
                (
                    s.start_position().row as u32 + 1,
                    text.node_text(n).to_string(),
                )
            })
        })
        .collect();
    Some(BlobFacts {
        lines,
        symbols: blocks(&heads, lines),
        tokens: g.tokens(root, &text),
        syntax_errors: count_errors(root),
        config_keys: keys(g, root, source),
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
    let logical = logical_lines(src, '#');
    // Logical line containing a 1-based line.
    let containing = |line: u32| -> Option<usize> {
        let i = logical.partition_point(|(start, _)| *start <= line);
        i.checked_sub(1)
    };
    let mut out = Vec::new();
    let mut commands: Vec<usize> = Vec::new();
    let line = |n: Node| n.start_position().row as u32 + 1;
    let mut export_lines: Vec<usize> = Vec::new();
    for (_, caps) in Grammar::matches(&g.keys, root, src) {
        let name = cap(&caps, "name").map(|n| &src[n.byte_range()]);
        let value = cap(&caps, "value").map(|v| unquote(&src[v.byte_range()]));
        if let (Some(a), Some(name)) = (cap(&caps, "export"), name) {
            match containing(line(a)) {
                // A line of its own: read as written (continuations joined, words split).
                Some(i) if logical[i].1.starts_with("export ") => export_lines.push(i),
                _ => out.push(fact(
                    format!("export.{name}"),
                    name,
                    line(a),
                    Some(value.unwrap_or_default()),
                )),
            }
        } else if let (Some(a), Some(name)) = (cap(&caps, "env"), name) {
            out.push(fact(format!("env.{name}"), name, line(a), value));
        } else if let Some(c) = cap(&caps, "command")
            && let Some(i) = containing(line(c))
        {
            commands.push(i);
        }
    }
    export_lines.dedup();
    for i in export_lines {
        let (start, text) = &logical[i];
        out.extend(shell_keys(&format!("{text}\n")).into_iter().map(|mut k| {
            k.line = *start;
            k
        }));
    }
    commands.sort_unstable();
    commands.dedup();
    for i in commands {
        let (start, text) = &logical[i];
        if !text.starts_with("export ") {
            out.push(fact("sh".into(), "sh", *start, Some(text.clone())));
        }
    }
    out.sort_by_key(|k| k.line);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_env_prefixes_and_command_lines() {
        let src = "#!/bin/sh\nset -e\nexport PORT=8000 B=\"x y\"\nCV_TOKEN=\"$t\" cv serve . \\\n  --port 1 &\nsetup() {\n  [ -n \"$X\" ] && export DEBUG=1\n}\ncat <<EOF\nnot: a command\npython fake.py\nEOF\n";
        let f = analyze(src).unwrap();
        let got: Vec<(&str, u32, Option<&str>)> = f
            .config_keys
            .iter()
            .map(|k| (k.path.as_str(), k.line, k.value.as_deref()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("sh", 2, Some("set -e")),
                ("export.PORT", 3, Some("8000")),
                ("export.B", 3, Some("x y")),
                ("env.CV_TOKEN", 4, Some("$t")),
                ("sh", 4, Some("CV_TOKEN=\"$t\" cv serve . --port 1 &")),
                ("export.DEBUG", 7, Some("1")),
                ("sh", 9, Some("cat <<EOF")),
            ]
        );
        let s: Vec<(&str, u32, u32)> = f
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.start, s.end))
            .collect();
        assert_eq!(s, vec![("setup", 6, 12)]);
        // A script the grammar cannot parse falls back to the scanner.
        assert!(analyze("if then fi fi (\n").is_none());
    }
}
