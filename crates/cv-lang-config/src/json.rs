//! JSON on tree-sitter-json (`queries/json/*.scm`).
//!
//! - **Keys** (`keys.scm` finds the document's value; the reader descends): every key whose name
//!   looks like an identifier, recursing into the first 50 items of arrays, as `config_fields.py`
//!   reads `json.loads` output. A key with a non-identifier name hides its subtree. A document
//!   `json.loads` would reject (a syntax error, comments, more than one value) has no keys.
//! - **Symbols:** none; JSON files are chunked by size.

use cv_lang::BlobFacts;
use cv_lang::keys::ConfigKeyFact;
use cv_lang_sitter::{Text, count_errors};
use tree_sitter::Node;

use crate::keys::ident_key;
use crate::sitter::{self, Grammar, cap};

pub(crate) fn analyze(source: &str) -> BlobFacts {
    let g = sitter::json();
    let text = Text::new(source);
    let lines = text.line_count();
    // A byte-order mark parses as three spaces (same byte offsets).
    let bom;
    let parsed = match source.strip_prefix('\u{feff}') {
        Some(rest) => {
            bom = format!("   {rest}");
            bom.as_str()
        }
        None => source,
    };
    let Some(tree) = g.parse(parsed) else {
        return BlobFacts {
            lines,
            tokens: vec![Vec::new(); lines as usize],
            ..Default::default()
        };
    };
    let root = tree.root_node();
    BlobFacts {
        lines,
        tokens: g.tokens(root, &text),
        syntax_errors: count_errors(root),
        config_keys: keys(g, root, parsed).unwrap_or_default(),
        ..Default::default()
    }
}

fn keys(g: &'static Grammar, root: Node, src: &str) -> Option<Vec<ConfigKeyFact>> {
    let mut c = root.walk();
    if root.has_error() || root.named_child_count() != 1 {
        return None;
    }
    if root.named_children(&mut c).any(|n| n.kind() == "comment") {
        return None;
    }
    let ms = Grammar::matches(&g.keys, root, src);
    let v = ms.first().and_then(|(_, caps)| cap(caps, "value"))?;
    let mut out = Vec::new();
    value(v, src, "", true, 0, &mut out)?;
    Some(out)
}

/// A JSON string's value (escapes decoded).
fn string(n: Node, src: &str) -> Option<String> {
    let raw = &src[n.byte_range()];
    if raw.contains('\\') {
        serde_json::from_str::<String>(raw).ok()
    } else {
        Some(raw.get(1..raw.len().checked_sub(1)?)?.to_string())
    }
}

/// Reads one value: records keys (when `emit`) and returns a scalar's text. `None` (the outer
/// `Option`) when the document cannot be read (comments inside, nesting deeper than 200).
fn value(
    n: Node,
    src: &str,
    prefix: &str,
    emit: bool,
    depth: u32,
    out: &mut Vec<ConfigKeyFact>,
) -> Option<Option<String>> {
    if depth > 200 {
        return None;
    }
    let mut c = n.walk();
    Some(match n.kind() {
        "object" => {
            for p in n.named_children(&mut c) {
                if p.kind() == "comment" {
                    return None;
                }
                let (Some(k), Some(v)) =
                    (p.child_by_field_name("key"), p.child_by_field_name("value"))
                else {
                    continue;
                };
                let k = string(k, src)?;
                let keep = emit && ident_key(&k);
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                let slot = keep.then(|| {
                    out.push(ConfigKeyFact {
                        path: path.clone(),
                        key: k.clone(),
                        line: p.start_position().row as u32 + 1,
                        value: None,
                        item: false,
                    });
                    out.len() - 1
                });
                let val = value(v, src, &path, keep, depth + 1, out)?;
                if let Some(s) = slot {
                    out[s].value = val;
                }
            }
            None
        }
        "array" => {
            for (i, x) in n.named_children(&mut c).enumerate() {
                if x.kind() == "comment" {
                    return None;
                }
                value(x, src, prefix, emit && i < 50, depth + 1, out)?;
            }
            None
        }
        "string" => string(n, src),
        _ => Some(src[n.byte_range()].to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_follow_the_reference() {
        let f = analyze(
            "{\n \"a\": {\"b-c\": 1, \"9x\": {\"no\": 2}},\n \"list\": [{\"k\": \"v\\u0021\"}, {\"k\": 2}]\n}",
        );
        let got: Vec<(&str, u32, Option<&str>)> = f
            .config_keys
            .iter()
            .map(|k| (k.path.as_str(), k.line, k.value.as_deref()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("a", 2, None),
                ("a.b-c", 2, Some("1")),
                ("list", 3, None),
                ("list.k", 3, Some("v!")),
                ("list.k", 3, Some("2")),
            ]
        );
        assert!(analyze("{ // c\n}").config_keys.is_empty());
        assert!(analyze("{\"a\": }").config_keys.is_empty());
        assert_eq!(analyze("\u{feff}{\"a\": 1}").config_keys.len(), 1);
        assert_eq!(f.tokens[1][..3], [1, 3, cv_lang::token::PROPERTY]);
    }
}
