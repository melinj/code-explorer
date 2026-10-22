//! The grammar-backed dialects' shared plumbing: one [`Grammar`] per tree-sitter grammar with its
//! `.scm` queries (`queries/<dialect>/{highlights,symbols,keys}.scm`), compiled once and shared by
//! every thread.
//!
//! - `highlights.scm` captures map to token classes by name (`@comment`, `@string`, `@number`,
//!   `@constant`, `@property`, `@keyword`, `@function`, `@type`, `@punctuation`, `@operator`,
//!   `@variable`).
//! - `symbols.scm` captures `@symbol` (the block) and `@name` (its name) for top-level blocks.
//! - `keys.scm` captures the nodes the dialect's key reader turns into `ConfigKeyFact`s; the
//!   capture names are the dialect's own.

use std::sync::OnceLock;

use cv_lang::{LineTokens, token};
use cv_lang_sitter::{Text, Tokens};
use tree_sitter::{Language, Node, Parser, Query, QueryCursor, StreamingIterator, Tree};

pub(crate) struct Grammar {
    pub language: Language,
    pub highlights: Query,
    pub symbols: Query,
    pub keys: Query,
}

impl Grammar {
    fn new(language: Language, highlights: &str, symbols: &str, keys: &str) -> Self {
        let q = |what: &str, src: &str| {
            Query::new(&language, src).unwrap_or_else(|e| panic!("built-in {what} query: {e}"))
        };
        Grammar {
            highlights: q("highlights", highlights),
            symbols: q("symbols", symbols),
            keys: q("keys", keys),
            language,
        }
    }

    pub fn parse(&self, source: &str) -> Option<Tree> {
        let mut p = Parser::new();
        p.set_language(&self.language).ok()?;
        p.parse(source, None)
    }

    /// Highlight tokens per line from `highlights.scm`.
    pub fn tokens(&self, root: Node, text: &Text) -> Vec<LineTokens> {
        let names = self.highlights.capture_names();
        let classes: Vec<u32> = names.iter().map(|n| class(n)).collect();
        let mut toks = Tokens::new(text);
        let mut cursor = QueryCursor::new();
        let mut caps = cursor.captures(&self.highlights, root, text.src.as_bytes());
        while let Some((m, i)) = caps.next() {
            let c = m.captures()[*i];
            toks.node(c.node, classes[c.index as usize]);
        }
        toks.lines
    }

    /// Every match of `query` as (pattern index, captures by name), outermost first in document
    /// order (start byte ascending, then end byte descending).
    pub fn matches<'t>(
        query: &'static Query,
        root: Node<'t>,
        src: &str,
    ) -> Vec<(usize, Vec<(&'static str, Node<'t>)>)> {
        let names: &'static [&'static str] = query.capture_names();
        let mut out = Vec::new();
        let mut cursor = QueryCursor::new();
        let mut it = cursor.matches(query, root, src.as_bytes());
        while let Some(m) = it.next() {
            let caps: Vec<(&'static str, Node<'t>)> = m
                .captures()
                .iter()
                .map(|c| (names[c.index as usize], c.node))
                .collect();
            out.push((m.pattern_index, caps));
        }
        let span = |caps: &[(&str, Node)]| {
            let s = caps.iter().map(|c| c.1.start_byte()).min().unwrap_or(0);
            let e = caps.iter().map(|c| c.1.end_byte()).max().unwrap_or(0);
            (s, std::cmp::Reverse(e))
        };
        out.sort_by_key(|(p, caps)| (span(caps), *p));
        out
    }
}

/// The node captured as `name` in one match.
pub(crate) fn cap<'t>(caps: &[(&str, Node<'t>)], name: &str) -> Option<Node<'t>> {
    caps.iter().find(|c| c.0 == name).map(|c| c.1)
}

/// Token class of a highlight capture name (its first dotted part).
fn class(name: &str) -> u32 {
    match name.split('.').next().unwrap_or(name) {
        "comment" => token::COMMENT,
        "string" => token::STRING,
        "number" => token::NUMBER,
        "constant" | "boolean" => token::CONSTANT,
        "property" => token::PROPERTY,
        "keyword" => token::KEYWORD,
        "function" => token::FUNCTION,
        "type" => token::TYPE,
        "punctuation" => token::PUNCTUATION,
        "operator" => token::OPERATOR,
        "variable" => token::VARIABLE,
        "tag" => token::TAG,
        _ => token::PLAIN,
    }
}

macro_rules! grammar {
    ($fn:ident, $lang:expr, $dir:literal) => {
        pub(crate) fn $fn() -> &'static Grammar {
            static G: OnceLock<Grammar> = OnceLock::new();
            G.get_or_init(|| {
                Grammar::new(
                    $lang,
                    include_str!(concat!("../queries/", $dir, "/highlights.scm")),
                    include_str!(concat!("../queries/", $dir, "/symbols.scm")),
                    include_str!(concat!("../queries/", $dir, "/keys.scm")),
                )
            })
        }
    };
}

grammar!(yaml, tree_sitter_yaml::LANGUAGE.into(), "yaml");
grammar!(toml, tree_sitter_toml_ng::LANGUAGE.into(), "toml");
grammar!(json, tree_sitter_json::LANGUAGE.into(), "json");
grammar!(bash, tree_sitter_bash::LANGUAGE.into(), "bash");
grammar!(hcl, tree_sitter_hcl::LANGUAGE.into(), "hcl");
grammar!(
    dockerfile,
    tree_sitter_dockerfile_updated::language(),
    "dockerfile"
);

#[cfg(test)]
mod tests {
    #[test]
    fn every_grammar_and_query_compiles() {
        for g in [
            super::yaml(),
            super::toml(),
            super::json(),
            super::bash(),
            super::hcl(),
            super::dockerfile(),
        ] {
            assert!(g.parse("").is_some());
            assert!(g.highlights.pattern_count() > 0);
        }
    }
}
