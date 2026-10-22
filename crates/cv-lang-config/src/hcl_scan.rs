//! HCL fallback (Terraform `.tf`, `.tfvars`, generic `.hcl`) for files tree-sitter-hcl cannot parse
//! cleanly (`hcl.rs`): a small line scanner that keeps state across
//! lines (block comments, heredocs, open brackets).
//!
//! - **Symbols:** every top-level block, named the way Terraform refers to it: `resource "aws_x"
//!   "web"` → `aws_x.web`, `data "a" "b"` → `data.a.b`, `module "m"` → `module.m`, `variable "v"`
//!   → `var.v`, `output "o"` → `output.o`, `provider "p"` → `provider.p`, `locals`, `terraform`.
//!   Other block types join their type and labels with `.`.
//! - **Keys** (`ConfigKeyFact`): every attribute under its block path, e.g.
//!   `aws_x.web.instance_type`; nested blocks and object values add their name
//!   (`aws_x.web.ingress.from_port`, `aws_x.web.tags.Name`). List elements add no key facts.
//! - **Tokens:** comments (`#`, `//`, `/* */`), strings and heredocs, block types as keywords,
//!   attribute names as properties, numbers and `true`/`false`/`null`.
//!
//! Braces inside strings, comments and heredocs never open or close blocks.

use cv_lang::keys::ConfigKeyFact;
use cv_lang::{BlobFacts, SymbolFact, SymbolKind, split_lines, token};

use crate::Line;

#[derive(Clone, Copy, PartialEq)]
enum FrameKind {
    /// A block or an object value: its attributes are keys.
    Keyed,
    /// A list, a call or a parenthesised expression: no keys inside.
    Expr,
}

struct Frame {
    kind: FrameKind,
    path: String,
    sym: Option<usize>,
}

/// Terraform's name for a top-level block, else type and labels joined by `.`.
pub(crate) fn block_name(words: &[String]) -> String {
    let labels = &words[1..];
    match (words[0].as_str(), labels.len()) {
        ("resource", 2) => format!("{}.{}", labels[0], labels[1]),
        ("data", 2) => format!("data.{}.{}", labels[0], labels[1]),
        ("variable", 1) => format!("var.{}", labels[0]),
        _ => words.join("."),
    }
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}.{name}")
    }
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-')
}

/// A literal value as written: an interpolation-free string (unquoted), a number or a constant.
pub(crate) fn literal(rest: &str) -> Option<String> {
    let v = rest.trim_start();
    let b = v.as_bytes();
    if b.first() == Some(&b'"') {
        let e = string_end(b, 0);
        let s = v.get(1..e.checked_sub(1)?)?;
        return (e >= 2 && !s.contains("${") && !s.contains('"')).then(|| s.to_string());
    }
    let e = v
        .find(|c: char| c.is_whitespace() || matches!(c, ',' | '}' | ']' | ')'))
        .unwrap_or(v.len());
    let v = &v[..e];
    let simple =
        !v.is_empty() && (v.parse::<f64>().is_ok() || matches!(v, "true" | "false" | "null"));
    simple.then(|| v.to_string())
}

/// End (exclusive) of the string starting at `b[i] == '"'`; `${…}` may hold nested strings.
fn string_end(b: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    let mut depth = 0u32;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 1,
            b'$' | b'%' if b.get(j + 1) == Some(&b'{') => {
                depth += 1;
                j += 1;
            }
            b'}' if depth > 0 => depth -= 1,
            b'"' if depth == 0 => return j + 1,
            b'"' => {
                // A string nested in an interpolation.
                j = string_end(b, j) - 1;
            }
            _ => {}
        }
        j += 1;
    }
    b.len()
}

pub(crate) fn analyze(source: &str) -> BlobFacts {
    let lines = split_lines(source);
    let total = lines.len() as u32;
    let mut tokens = Vec::with_capacity(lines.len());
    let mut symbols: Vec<SymbolFact> = Vec::new();
    let mut keys: Vec<ConfigKeyFact> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut in_comment = false;
    let mut heredoc: Option<String> = None;

    for (li, l) in lines.iter().enumerate() {
        let n = li as u32 + 1;
        let mut t = Line::default();
        if let Some(term) = &heredoc {
            t.span(l, 0, l.len(), token::STRING);
            if l.trim() == term {
                heredoc = None;
            }
            tokens.push(t.toks);
            continue;
        }
        let b = l.as_bytes();
        // The statement being read on this line: leading words/labels, and the attribute once
        // `=` is seen.
        let mut words: Vec<String> = Vec::new();
        let mut attr: Option<String> = None;
        let mut first_word_at: Option<(usize, usize)> = None;
        let mut i = 0;
        while i < b.len() {
            let c = b[i];
            if in_comment {
                let end = l[i..].find("*/").map(|p| i + p + 2);
                t.span(l, i, end.unwrap_or(b.len()), token::COMMENT);
                match end {
                    Some(e) => {
                        in_comment = false;
                        i = e;
                        continue;
                    }
                    None => break,
                }
            }
            let keyed = stack.last().is_none_or(|f| f.kind == FrameKind::Keyed);
            match c {
                b'#' => {
                    t.span(l, i, b.len(), token::COMMENT);
                    break;
                }
                b'/' if b.get(i + 1) == Some(&b'/') => {
                    t.span(l, i, b.len(), token::COMMENT);
                    break;
                }
                b'/' if b.get(i + 1) == Some(&b'*') => {
                    in_comment = true;
                    t.span(l, i, i + 2, token::COMMENT);
                    i += 2;
                }
                b'"' => {
                    let e = string_end(b, i);
                    t.span(l, i, e, token::STRING);
                    if attr.is_none() && keyed {
                        let s = &l[i + 1..e.saturating_sub(1).max(i + 1)];
                        words.push(s.to_string());
                    }
                    i = e;
                }
                b'<' if b.get(i + 1) == Some(&b'<') => {
                    let mut j = i + 2;
                    if b.get(j) == Some(&b'-') {
                        j += 1;
                    }
                    let s = j;
                    while j < b.len() && is_ident(b[j]) {
                        j += 1;
                    }
                    if j > s {
                        heredoc = Some(l[s..j].to_string());
                        t.span(l, i, b.len(), token::STRING);
                        break;
                    }
                    t.span(l, i, i + 2, token::OPERATOR);
                    i += 2;
                }
                b'=' | b':'
                    if keyed
                        && attr.is_none()
                        && words.len() == 1
                        && b.get(i + 1) != Some(&b'=')
                        && b.get(i + 1) != Some(&b'>') =>
                {
                    let name = words.pop().unwrap_or_default();
                    if let Some((a, e)) = first_word_at {
                        t.span(l, a, e, token::PROPERTY);
                    }
                    t.span(l, i, i + 1, token::OPERATOR);
                    let prefix = stack.last().map_or("", |f| f.path.as_str());
                    let path = join(prefix, &name);
                    keys.push(ConfigKeyFact {
                        key: name.clone(),
                        path,
                        line: n,
                        value: literal(&l[i + 1..]),
                        item: false,
                    });
                    attr = Some(name);
                    i += 1;
                }
                b'{' => {
                    t.span(l, i, i + 1, token::PUNCTUATION);
                    let prefix = stack.last().map_or(String::new(), |f| f.path.clone());
                    let frame = match (&attr, keyed && !words.is_empty()) {
                        (Some(a), _) => Frame {
                            kind: FrameKind::Keyed,
                            path: join(&prefix, a),
                            sym: None,
                        },
                        (None, true) => {
                            if let Some((a, e)) = first_word_at {
                                t.span(l, a, e, token::KEYWORD);
                            }
                            if stack.is_empty() {
                                let name = block_name(&words);
                                symbols.push(SymbolFact {
                                    kind: SymbolKind::ConfigBlock,
                                    name: name.clone(),
                                    qualname: name.clone(),
                                    parent: None,
                                    start: n,
                                    end: total,
                                    name_line: n,
                                });
                                Frame {
                                    kind: FrameKind::Keyed,
                                    path: name,
                                    sym: Some(symbols.len() - 1),
                                }
                            } else {
                                Frame {
                                    kind: FrameKind::Keyed,
                                    path: join(&prefix, &words.join(".")),
                                    sym: None,
                                }
                            }
                        }
                        // An object inside a list or expression: keys under the list's path.
                        (None, false) => Frame {
                            kind: FrameKind::Keyed,
                            path: prefix,
                            sym: None,
                        },
                    };
                    stack.push(frame);
                    words.clear();
                    attr = None;
                    first_word_at = None;
                    i += 1;
                }
                b'[' | b'(' => {
                    t.span(l, i, i + 1, token::PUNCTUATION);
                    let prefix = stack.last().map_or(String::new(), |f| f.path.clone());
                    stack.push(Frame {
                        kind: FrameKind::Expr,
                        path: attr.as_deref().map_or(prefix.clone(), |a| join(&prefix, a)),
                        sym: None,
                    });
                    // Inside, only object elements carry keys again.
                    attr = None;
                    words.clear();
                    first_word_at = None;
                    i += 1;
                }
                b'}' | b']' | b')' => {
                    t.span(l, i, i + 1, token::PUNCTUATION);
                    if let Some(f) = stack.pop()
                        && let Some(s) = f.sym
                    {
                        symbols[s].end = n;
                    }
                    words.clear();
                    attr = None;
                    first_word_at = None;
                    i += 1;
                }
                b',' => {
                    t.span(l, i, i + 1, token::PUNCTUATION);
                    words.clear();
                    attr = None;
                    first_word_at = None;
                    i += 1;
                }
                c if c.is_ascii_digit() => {
                    let mut j = i;
                    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'.') {
                        j += 1;
                    }
                    t.span(l, i, j, token::NUMBER);
                    i = j;
                }
                c if is_ident_start(c) => {
                    let mut j = i;
                    while j < b.len() && (is_ident(b[j]) || b[j] == b'.') {
                        j += 1;
                    }
                    let w = &l[i..j];
                    if matches!(w, "true" | "false" | "null") {
                        t.span(l, i, j, token::CONSTANT);
                    } else if attr.is_none() && keyed {
                        if words.is_empty() {
                            first_word_at = Some((i, j));
                        }
                        words.push(w.to_string());
                    } else if matches!(w, "for" | "in" | "if" | "for_each" | "each") {
                        t.span(l, i, j, token::KEYWORD);
                    }
                    i = j;
                }
                _ => i += 1,
            }
        }
        tokens.push(t.toks);
    }
    BlobFacts {
        lines: total,
        symbols,
        tokens,
        config_keys: keys,
        ..Default::default()
    }
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
  instance_type = "t3.micro"
  count         = 2
  tags = {
    Name = "web-${var.env}"
    Team = "core"
  }
  ingress {
    from_port = 80
  }
  user_data = <<-EOF
    #!/bin/sh
    echo "}"
  EOF
  list = [{ inner = 1 }, "x"]
}

data "aws_ami" "ubuntu" {
  most_recent = true
}

module "vpc" {
  source = "./vpc"
}

variable "region" {
  default = "eu-north-1"
}

output "ip" {
  value = aws_instance.web.public_ip
}

locals {
  env = "prod"
}
"#;

    #[test]
    fn terraform_blocks_are_symbols_with_their_spans() {
        let f = analyze(TF);
        let s: Vec<(&str, u32, u32)> = f
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.start, s.end))
            .collect();
        assert_eq!(
            s,
            vec![
                ("terraform", 2, 4),
                ("provider.aws", 6, 8),
                ("aws_instance.web", 12, 28),
                ("data.aws_ami.ubuntu", 30, 32),
                ("module.vpc", 34, 36),
                ("var.region", 38, 40),
                ("output.ip", 42, 44),
                ("locals", 46, 48),
            ]
        );
        assert!(f.symbols.iter().all(|s| s.kind == SymbolKind::ConfigBlock));
    }

    #[test]
    fn attributes_are_keys_under_their_block() {
        let f = analyze(TF);
        let k: Vec<(&str, u32, Option<&str>)> = f
            .config_keys
            .iter()
            .map(|k| (k.path.as_str(), k.line, k.value.as_deref()))
            .collect();
        for want in [
            ("terraform.required_version", 3, Some(">= 1.5")),
            ("provider.aws.region", 7, None),
            ("aws_instance.web.ami", 13, Some("ami-123")),
            ("aws_instance.web.instance_type", 14, Some("t3.micro")),
            ("aws_instance.web.count", 15, Some("2")),
            ("aws_instance.web.tags", 16, None),
            ("aws_instance.web.tags.Name", 17, None),
            ("aws_instance.web.tags.Team", 18, Some("core")),
            ("aws_instance.web.ingress.from_port", 21, Some("80")),
            ("aws_instance.web.user_data", 23, None),
            ("aws_instance.web.list", 27, None),
            ("aws_instance.web.list.inner", 27, Some("1")),
            ("data.aws_ami.ubuntu.most_recent", 31, Some("true")),
            ("var.region.default", 39, Some("eu-north-1")),
            ("locals.env", 47, Some("prod")),
        ] {
            assert!(k.contains(&want), "missing {want:?} in {k:?}");
        }
        // Nothing from the comment or the heredoc.
        assert!(
            !k.iter()
                .any(|(p, _, _)| p.contains("echo") || p.contains("still"))
        );
        assert_eq!(
            f.config_keys.iter().find(|k| k.line == 17).unwrap().key,
            "Name"
        );
    }

    #[test]
    fn tokens_mark_comments_strings_keywords_and_properties() {
        let f = analyze(TF);
        let has = |line: usize, cls: u32| f.tokens[line - 1].chunks(3).any(|t| t[2] == cls);
        assert!(has(1, token::COMMENT));
        assert!(has(6, token::KEYWORD)); // provider
        assert!(has(6, token::STRING)); // "aws"
        assert!(has(13, token::PROPERTY)); // ami
        assert!(has(10, token::COMMENT)); // inside /* */
        assert!(has(24, token::STRING)); // heredoc body
        assert!(has(31, token::CONSTANT)); // true
    }
}
