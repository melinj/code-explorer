//! YAML on tree-sitter-yaml (`queries/yaml/*.scm`).
//!
//! - **Symbols:** each top-level key of a document (`symbols.scm`); a block runs to the next one.
//!   Compose `services` and CI `jobs` get one child per service or job (`nest_groups`).
//! - **Keys** (`keys.scm`): every block-mapping key whose name looks like an identifier, with the
//!   dotted path of its enclosing keys; list items add no segment and plain list items are `item`
//!   facts. This is the key set of `docs/analysis/config_fields.py` (P5 parity), read from the
//!   tree instead of by indentation: flow mappings (`{a: 1}`) and quoted keys add no facts, and
//!   `|`/`>` block text is never read as keys. A key's value is the rest of its line as written
//!   (quotes and a trailing comment removed), or the dedented text of a block scalar.

use cv_lang::keys::ConfigKeyFact;
use cv_lang::{BlobFacts, SymbolFact, SymbolKind};
use cv_lang_sitter::{Text, count_errors};
use tree_sitter::Node;

use crate::keys::unquote;
use crate::sitter::{self, Grammar, cap};

/// `None` when the document does not parse cleanly (templated YAML such as Jinja `{% … %}`
/// recipes): the caller falls back to the line scanner, which reads keys the way
/// `config_fields.py` does whatever the syntax.
pub(crate) fn analyze(source: &str) -> Option<BlobFacts> {
    let g = sitter::yaml();
    let text = Text::new(source);
    let lines = text.line_count();
    let tree = g.parse(source)?;
    let root = tree.root_node();
    if root.has_error() {
        return None;
    }
    let mut heads = Vec::new();
    for (_, caps) in Grammar::matches(&g.symbols, root, source) {
        let (Some(sym), Some(name)) = (cap(&caps, "symbol"), cap(&caps, "name")) else {
            continue;
        };
        if sym.start_position().column == 0 {
            heads.push((
                sym.start_position().row as u32 + 1,
                text.node_text(name).trim().to_string(),
            ));
        }
    }
    let mut f = BlobFacts {
        lines,
        symbols: blocks(&heads, lines),
        tokens: g.tokens(root, &text),
        syntax_errors: count_errors(root),
        config_keys: keys(g, root, source),
        ..Default::default()
    };
    crate::nest_groups(&mut f);
    Some(f)
}

/// Heads (line, name) → blocks that run to the line before the next head.
pub(crate) fn blocks(heads: &[(u32, String)], total: u32) -> Vec<SymbolFact> {
    heads
        .iter()
        .enumerate()
        .map(|(k, (start, name))| {
            let next = heads.get(k + 1).map(|h| h.0 - 1).unwrap_or(total);
            let name = if name.is_empty() {
                format!("L{start}")
            } else {
                name.chars().take(80).collect()
            };
            SymbolFact {
                kind: SymbolKind::ConfigBlock,
                qualname: name.clone(),
                name,
                parent: None,
                start: *start,
                end: next.max(*start),
                name_line: *start,
            }
        })
        .collect()
}

/// `[A-Za-z_][\w.-]*`, the key names `config_fields.py` reads.
fn ident(k: &str) -> bool {
    let mut c = k.chars();
    c.next()
        .is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
        && c.all(|x| x.is_alphanumeric() || matches!(x, '_' | '.' | '-'))
}

fn keys(g: &'static Grammar, root: Node, src: &str) -> Vec<ConfigKeyFact> {
    let lines: Vec<&str> = src
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    // The rest of a node's first line, from the node on.
    let rest = |n: Node| -> &str {
        let p = n.start_position();
        lines
            .get(p.row)
            .and_then(|l| l.get(p.column..))
            .unwrap_or("")
    };
    let mut out = Vec::new();
    // Enclosing pairs: (end byte, path; None for a key that adds no segment).
    let mut stack: Vec<(usize, Option<String>)> = Vec::new();
    for (_, caps) in Grammar::matches(&g.keys, root, src) {
        let Some(node) = cap(&caps, "pair").or_else(|| cap(&caps, "item")) else {
            continue;
        };
        // The previous sibling's path (the last pair closed here).
        let mut sibling: Option<Option<String>> = None;
        while stack.last().is_some_and(|(e, _)| *e <= node.start_byte()) {
            sibling = stack.pop().map(|(_, p)| p);
        }
        let parent = stack.iter().rev().find_map(|(_, p)| p.as_deref());
        let Some(key) = cap(&caps, "key") else {
            // A plain list item: `- value`.
            out.push(ConfigKeyFact {
                path: parent.unwrap_or("").to_string(),
                key: String::new(),
                line: node.start_position().row as u32 + 1,
                value: Some(unquote(rest(node))),
                item: true,
            });
            continue;
        };
        let name = &src[key.byte_range()];
        let plain = key
            .named_child(0)
            .is_some_and(|k| k.kind() == "plain_scalar");
        if !(plain && ident(name)) {
            // Not a key `config_fields.py` reads; like its indent stack, the previous sibling
            // stays open, so this pair's children are read under the sibling's path.
            stack.push((node.end_byte(), sibling.flatten()));
            continue;
        }
        let path = match parent {
            Some(p) => format!("{p}.{name}"),
            None => name.to_string(),
        };
        let value = cap(&caps, "value").and_then(|v| {
            if v.start_position().row != key.start_position().row {
                return None;
            }
            match v.named_child(0) {
                Some(b) if b.kind() == "block_scalar" => block_text(&src[b.byte_range()]),
                _ if v.kind() == "block_node" => None,
                // A one-line quoted scalar, without what follows it (a comment).
                Some(q)
                    if v.named_child_count() == 1
                        && matches!(q.kind(), "double_quote_scalar" | "single_quote_scalar")
                        && q.start_position().row == q.end_position().row =>
                {
                    Some(unquote(&src[q.byte_range()]))
                }
                _ => Some(unquote(rest(v))).filter(|s| !s.is_empty()),
            }
        });
        out.push(ConfigKeyFact {
            path: path.clone(),
            key: name.to_string(),
            line: key.start_position().row as u32 + 1,
            value,
            item: false,
        });
        stack.push((node.end_byte(), Some(path)));
    }
    out
}

/// A `|`/`>` block scalar's text: the lines after the header, dedented, comment lines left out.
fn block_text(raw: &str) -> Option<String> {
    let body: Vec<&str> = raw
        .split('\n')
        .skip(1)
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .filter(|l| !l.trim_start().starts_with('#') || l.trim().is_empty())
        .collect();
    let indent = |l: &str| l.chars().count() - l.trim_start().chars().count();
    let min = body
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| indent(l))
        .min()
        .unwrap_or(0);
    let lines: Vec<String> = body
        .iter()
        .map(|l| {
            if l.trim().is_empty() {
                String::new()
            } else {
                l.chars().skip(min).collect()
            }
        })
        .collect();
    let joined = lines.join("\n").trim_end().to_string();
    (!joined.is_empty()).then_some(joined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cv_lang::token;

    fn paths(f: &BlobFacts) -> Vec<(String, u32, Option<String>)> {
        f.config_keys
            .iter()
            .filter(|k| !k.item)
            .map(|k| (k.path.clone(), k.line, k.value.clone()))
            .collect()
    }

    #[test]
    fn keys_follow_the_reference_from_the_tree() {
        let src = "name: x\npool:\n  - id: a\n    params:\n      topic: \"w.e.moved\" # c\n    prompt: |\n      not: a key\n      # hash\n      still: text\n  - id: b\nenv:\n  - FOO=1\nlast: 2 # c\n\"q\": {a: 1}\n";
        let f = analyze(src).unwrap();
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
                ("pool.id".into(), 10, Some("b".into())),
                ("env".into(), 11, None),
                ("last".into(), 13, Some("2".into())),
            ]
        );
        let items: Vec<(&str, &str)> = f
            .config_keys
            .iter()
            .filter(|k| k.item)
            .map(|k| (k.path.as_str(), k.value.as_deref().unwrap()))
            .collect();
        assert_eq!(items, vec![("env", "FOO=1")]);
        let s: Vec<(&str, u32, u32)> = f
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.start, s.end))
            .collect();
        assert_eq!(
            s,
            vec![
                ("name", 1, 1),
                ("pool", 2, 10),
                ("env", 11, 12),
                ("last", 13, 13),
                ("\"q\"", 14, 14)
            ]
        );
    }

    #[test]
    fn a_non_identifier_key_keeps_its_previous_sibling_open() {
        // `config_fields.py` reads `updated[].id:` as no key and does not pop its indent stack.
        let src = "outputs:\n  plan:\n    name: plan\n  updated[].id:\n    name: id\n";
        let f = analyze(src).unwrap();
        let p: Vec<&str> = f.config_keys.iter().map(|k| k.path.as_str()).collect();
        assert_eq!(
            p,
            vec![
                "outputs",
                "outputs.plan",
                "outputs.plan.name",
                "outputs.plan.name"
            ]
        );
    }

    #[test]
    fn tokens_and_templated_yaml_falls_back() {
        let f = analyze("# top\nserver:\n  port: 8080 # p\n  name: \"x\"\n").unwrap();
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
        assert_eq!(f.tokens[3][6..], [8, 3, token::STRING]);
        // Jinja-templated YAML is not YAML: the caller keeps the line scanner.
        assert!(analyze("{% set v = 1 %}\npackage:\n  name: x\n").is_none());
    }
}
