//! TOML on tree-sitter-toml-ng (`queries/toml/*.scm`).
//!
//! - **Symbols:** each `[table]` and `[[array of tables]]` header; a block runs to the next one.
//! - **Keys** (`keys.scm` finds the headers and top-level pairs; the reader descends): every key
//!   whose name looks like an identifier, as `config_fields.py` reads `tomllib` output: a table
//!   (explicit, implicit or dotted) is one key at its first occurrence, an array of tables is one
//!   key with its elements' keys repeated, inline tables add their keys and arrays recurse into
//!   their first 50 items. A key with a non-identifier name hides its subtree. Lines are where
//!   the key is written. A document `toml_edit` rejects has no keys (the reference skips it too).

use std::collections::{HashMap, HashSet};

use cv_lang::BlobFacts;
use cv_lang::keys::ConfigKeyFact;
use cv_lang_sitter::{Text, count_errors};
use tree_sitter::Node;

use crate::keys::{ident_key, toml_value_str};
use crate::sitter::{self, Grammar, cap};
use crate::yaml::blocks;

pub(crate) fn analyze(source: &str) -> BlobFacts {
    let g = sitter::toml();
    let text = Text::new(source);
    let lines = text.line_count();
    let Some(tree) = g.parse(source) else {
        return BlobFacts {
            lines,
            tokens: vec![Vec::new(); lines as usize],
            ..Default::default()
        };
    };
    let root = tree.root_node();
    let heads: Vec<(u32, String)> = Grammar::matches(&g.symbols, root, source)
        .into_iter()
        .filter_map(|(_, caps)| {
            let (s, n) = (cap(&caps, "symbol")?, cap(&caps, "name")?);
            Some((
                s.start_position().row as u32 + 1,
                text.node_text(n).trim().to_string(),
            ))
        })
        .collect();
    let valid = source.parse::<toml_edit::DocumentMut>().is_ok();
    BlobFacts {
        lines,
        symbols: blocks(&heads, lines),
        tokens: g.tokens(root, &text),
        syntax_errors: count_errors(root),
        config_keys: if valid {
            keys(g, root, source)
        } else {
            Vec::new()
        },
        ..Default::default()
    }
}

struct Reader<'s> {
    src: &'s str,
    out: Vec<ConfigKeyFact>,
}

/// Key segments of a key node (`a`, `"a b"`, `a.b.c`), unquoted.
fn segments(n: Node, src: &str, out: &mut Vec<String>) {
    match n.kind() {
        "bare_key" => out.push(src[n.byte_range()].to_string()),
        "quoted_key" => {
            let t = &src[n.byte_range()];
            out.push(match t.parse::<toml_edit::Value>() {
                Ok(toml_edit::Value::String(s)) => s.value().clone(),
                _ => t.trim_matches(['"', '\'']).to_string(),
            });
        }
        "dotted_key" => {
            let mut c = n.walk();
            for k in n.named_children(&mut c) {
                segments(k, src, out);
            }
        }
        _ => {}
    }
}

fn join(prefix: &str, k: &str) -> String {
    if prefix.is_empty() {
        k.to_string()
    } else {
        format!("{prefix}.{k}")
    }
}

impl Reader<'_> {
    fn emit(&mut self, path: String, key: &str, at: Node, value: Option<String>) {
        self.out.push(ConfigKeyFact {
            path,
            key: key.to_string(),
            line: at.start_position().row as u32 + 1,
            value,
            item: false,
        });
    }

    /// Emits the table keys a header or dotted key opens (each once per `seen` scope); returns
    /// the innermost path, or None when a segment is not an identifier.
    fn open(
        &mut self,
        prefix: &str,
        segs: &[String],
        at: Node,
        seen: &mut HashSet<String>,
    ) -> Option<String> {
        let mut path = prefix.to_string();
        for s in segs {
            if !ident_key(s) {
                return None;
            }
            path = join(&path, s);
            if seen.insert(path.clone()) {
                self.emit(path.clone(), s, at, None);
            }
        }
        Some(path)
    }

    fn pair(&mut self, pair: Node, prefix: &str, seen: &mut HashSet<String>) {
        let mut c = pair.walk();
        let kids: Vec<Node> = pair
            .named_children(&mut c)
            .filter(|k| k.kind() != "comment")
            .collect();
        let (Some(&key), Some(&value)) = (kids.first(), kids.last()) else {
            return;
        };
        if key.id() == value.id() {
            return;
        }
        let mut segs = Vec::new();
        segments(key, self.src, &mut segs);
        let Some((last, outer)) = segs.split_last() else {
            return;
        };
        let Some(base) = self.open(prefix, outer, key, seen) else {
            return;
        };
        if !ident_key(last) {
            return;
        }
        let path = join(&base, last);
        let v = self.src[value.byte_range()]
            .parse::<toml_edit::Value>()
            .ok()
            .and_then(|v| toml_value_str(&v));
        self.emit(path.clone(), last, key, v);
        self.value(value, &path);
    }

    fn value(&mut self, v: Node, path: &str) {
        let mut c = v.walk();
        match v.kind() {
            "inline_table" => {
                let mut seen = HashSet::new();
                for p in v.named_children(&mut c).filter(|p| p.kind() == "pair") {
                    self.pair(p, path, &mut seen);
                }
            }
            "array" => {
                for x in v
                    .named_children(&mut c)
                    .filter(|x| x.kind() != "comment")
                    .take(50)
                {
                    self.value(x, path);
                }
            }
            _ => {}
        }
    }
}

fn keys(g: &'static Grammar, root: Node, src: &str) -> Vec<ConfigKeyFact> {
    let mut r = Reader {
        src,
        out: Vec::new(),
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut elements: HashMap<String, usize> = HashMap::new();
    for (_, caps) in Grammar::matches(&g.keys, root, src) {
        if let Some(p) = cap(&caps, "pair") {
            r.pair(p, "", &mut seen);
            continue;
        }
        let (Some(name), table) = (
            cap(&caps, "name"),
            cap(&caps, "table").or_else(|| cap(&caps, "array_table")),
        ) else {
            continue;
        };
        let Some(table) = table else { continue };
        let mut segs = Vec::new();
        segments(name, src, &mut segs);
        let Some(path) = r.open("", &segs, name, &mut seen) else {
            continue;
        };
        if cap(&caps, "array_table").is_some() {
            // A new element: its sub-tables are new keys again; only the first 50 are read.
            let pre = format!("{path}.");
            seen.retain(|s| !s.starts_with(&pre));
            let n = elements.entry(path.clone()).or_default();
            *n += 1;
            if *n > 50 {
                continue;
            }
        }
        let mut c = table.walk();
        let pairs: Vec<Node> = table
            .named_children(&mut c)
            .filter(|p| p.kind() == "pair")
            .collect();
        for p in pairs {
            r.pair(p, &path, &mut seen);
        }
    }
    r.out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys_of(src: &str) -> Vec<(String, u32, Option<String>)> {
        analyze(src)
            .config_keys
            .into_iter()
            .map(|k| (k.path, k.line, k.value))
            .collect()
    }

    #[test]
    fn tables_arrays_and_dotted_keys_follow_the_reference() {
        let got = keys_of(
            "top = 1\n[tool.x]\nname = \"n\"\na.b = [1, 2]\nt = { i = 1, l = [{ k = \"v\" }] }\n[[bin]]\npath = \"p\"\n[[bin]]\npath = \"q\"\n[tool.y]\n\"bad key\" = { hidden = 1 }\n",
        );
        let s = |p: &str, l: u32, v: Option<&str>| (p.to_string(), l, v.map(String::from));
        assert_eq!(
            got,
            vec![
                s("top", 1, Some("1")),
                s("tool", 2, None),
                s("tool.x", 2, None),
                s("tool.x.name", 3, Some("n")),
                s("tool.x.a", 4, None),
                s("tool.x.a.b", 4, Some("1 2")),
                s("tool.x.t", 5, None),
                s("tool.x.t.i", 5, Some("1")),
                s("tool.x.t.l", 5, Some("")),
                s("tool.x.t.l.k", 5, Some("v")),
                s("bin", 6, None),
                s("bin.path", 7, Some("p")),
                s("bin.path", 9, Some("q")),
                s("tool.y", 10, None),
            ]
        );
        let f = analyze("[tool.x]\nname = 1\n[[bin]]\n");
        let heads: Vec<(&str, u32, u32)> = f
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.start, s.end))
            .collect();
        assert_eq!(heads, vec![("tool.x", 1, 2), ("bin", 3, 3)]);
        // Invalid TOML (a duplicate key) has no keys, as `tomllib` reads nothing.
        assert!(analyze("a = 1\na = 2\n").config_keys.is_empty());
    }
}
