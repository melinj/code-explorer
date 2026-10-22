//! HCL (Terraform `.tf`, `.tfvars`, generic `.hcl`) on tree-sitter-hcl (`queries/hcl/*.scm`).
//!
//! - **Symbols:** every top-level block, named the way Terraform refers to it: `resource "aws_x"
//!   "web"` → `aws_x.web`, `data "a" "b"` → `data.a.b`, `variable "v"` → `var.v`, others join
//!   their type and labels with `.` (`module.m`, `output.o`, `provider.p`, `locals`).
//! - **Keys** (`ConfigKeyFact`): every attribute under its block path, e.g.
//!   `aws_x.web.instance_type`; nested blocks add their type and labels and object elements their
//!   key (`aws_x.web.ingress.from_port`, `aws_x.web.tags.Name`); objects inside lists or calls
//!   key under the attribute. A literal value (an interpolation-free string, a number, a constant)
//!   is the key's value.
//!
//! A file the grammar cannot parse cleanly falls back to the line scanner ([`crate::hcl_scan`]).

use cv_lang::keys::ConfigKeyFact;
use cv_lang::{BlobFacts, SymbolFact, SymbolKind};
use cv_lang_sitter::{Text, count_errors, end_line, line};
use tree_sitter::Node;

use crate::hcl_scan::{block_name, literal};
use crate::sitter::{self, Grammar, cap};

pub(crate) fn analyze(source: &str) -> BlobFacts {
    grammar(source).unwrap_or_else(|| crate::hcl_scan::analyze(source))
}

fn grammar(source: &str) -> Option<BlobFacts> {
    let g = sitter::hcl();
    let text = Text::new(source);
    let lines = text.line_count();
    let tree = g.parse(source)?;
    let root = tree.root_node();
    if root.has_error() {
        return None;
    }
    let mut symbols = Vec::new();
    for (_, caps) in Grammar::matches(&g.symbols, root, source) {
        let Some(b) = cap(&caps, "symbol") else {
            continue;
        };
        let name = block_name(&words(b, source));
        symbols.push(SymbolFact {
            kind: SymbolKind::ConfigBlock,
            qualname: name.clone(),
            name,
            parent: None,
            start: line(b),
            end: end_line(b),
            name_line: line(b),
        });
    }
    Some(BlobFacts {
        lines,
        symbols,
        tokens: g.tokens(root, &text),
        syntax_errors: count_errors(root),
        config_keys: keys(g, root, source),
        ..Default::default()
    })
}

/// A block's type and labels (`resource`, `aws_x`, `web`).
fn words(block: Node, src: &str) -> Vec<String> {
    let mut c = block.walk();
    block
        .named_children(&mut c)
        .take_while(|n| n.kind() != "block_start")
        .filter_map(|n| match n.kind() {
            "identifier" => Some(src[n.byte_range()].to_string()),
            "string_lit" => Some(
                src[n.byte_range()]
                    .trim_start_matches('"')
                    .trim_end_matches('"')
                    .to_string(),
            ),
            _ => None,
        })
        .collect()
}

/// An object element's key: a bare name or an interpolation-free quoted string.
fn elem_key(key: Node, src: &str) -> Option<String> {
    let t = src[key.byte_range()].trim();
    if let Some(q) = t.strip_prefix('"').and_then(|q| q.strip_suffix('"')) {
        return (!q.contains("${") && !q.contains('"')).then(|| q.to_string());
    }
    let ok = t.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    ok.then(|| t.to_string())
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}.{name}")
    }
}

fn keys(g: &'static Grammar, root: Node, src: &str) -> Vec<ConfigKeyFact> {
    let mut out = Vec::new();
    // Enclosing blocks, attributes and elements: (end byte, path; None adds no segment).
    let mut stack: Vec<(usize, Option<String>)> = Vec::new();
    for (_, caps) in Grammar::matches(&g.keys, root, src) {
        let Some(node) = cap(&caps, "block")
            .or_else(|| cap(&caps, "attribute"))
            .or_else(|| cap(&caps, "elem"))
        else {
            continue;
        };
        while stack.last().is_some_and(|(e, _)| *e <= node.start_byte()) {
            stack.pop();
        }
        let parent = stack
            .iter()
            .rev()
            .find_map(|(_, p)| p.as_deref())
            .unwrap_or("");
        let value = cap(&caps, "value").and_then(|v| literal(&src[v.byte_range()]));
        let (name, key_node) = if let Some(b) = cap(&caps, "block") {
            let w = words(b, src);
            let path = if stack.is_empty() {
                block_name(&w)
            } else {
                join(parent, &w.join("."))
            };
            stack.push((node.end_byte(), Some(path)));
            continue;
        } else if let Some(n) = cap(&caps, "name") {
            (Some(src[n.byte_range()].to_string()), n)
        } else if let Some(k) = cap(&caps, "key") {
            (elem_key(k, src), k)
        } else {
            continue;
        };
        let Some(name) = name else {
            stack.push((node.end_byte(), None));
            continue;
        };
        let path = join(parent, &name);
        out.push(ConfigKeyFact {
            path: path.clone(),
            key: name,
            line: line(key_node),
            value,
            item: false,
        });
        stack.push((node.end_byte(), Some(path)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TF: &str = r#"# main
terraform {
  required_version = ">= 1.5"
}

provider "aws" {
  region = var.region # "{ not a brace"
}

/* a { comment
   } still */
resource "aws_instance" "web" {
  ami           = "ami-123"
  count         = 2
  tags = {
    Name = "web-${var.env}"
    "Team" = "core"
  }
  ingress {
    from_port = 80
  }
  user_data = <<-EOF
    #!/bin/sh
    echo "}"
  EOF
  list = [{ inner = 1 }, "x"]
  merged = merge(var.t, { k = 2 })
}

data "aws_ami" "ubuntu" {
  most_recent = true
}

variable "region" {
  default = "eu-north-1"
}

locals { env = "prod" }
"#;

    #[test]
    fn the_grammar_reads_what_the_scanner_reads() {
        let g = grammar(TF).expect("parses");
        let s = crate::hcl_scan::analyze(TF);
        let keys = |f: &BlobFacts| {
            f.config_keys
                .iter()
                .map(|k| (k.path.clone(), k.key.clone(), k.line, k.value.clone()))
                .collect::<Vec<_>>()
        };
        let syms = |f: &BlobFacts| {
            f.symbols
                .iter()
                .map(|s| (s.name.clone(), s.start, s.end))
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(&g), keys(&s));
        assert_eq!(syms(&g), syms(&s));
        assert!(keys(&g).contains(&(
            "aws_instance.web.tags.Team".into(),
            "Team".into(),
            17,
            Some("core".into())
        )));
        assert!(keys(&g).contains(&(
            "aws_instance.web.merged.k".into(),
            "k".into(),
            27,
            Some("2".into())
        )));
        let has = |line: usize, cls: u32| g.tokens[line - 1].chunks(3).any(|t| t[2] == cls);
        use cv_lang::token;
        assert!(has(1, token::COMMENT));
        assert!(has(6, token::KEYWORD) && has(6, token::STRING));
        assert!(has(13, token::PROPERTY));
        assert!(has(10, token::COMMENT));
        assert!(has(23, token::STRING));
        assert!(has(31, token::CONSTANT));
    }
}
