//! `.scm` query plumbing shared by the code packs (DESIGN §4.2).
//!
//! A pack has two query files, compiled together into one [`PackQuery`] so the tree is walked
//! once:
//!
//! - `highlights.scm`: capture names map to token classes ([`capture_class`]). The tokens are
//!   emitted in the order a depth-first walk meets the captured nodes (start ascending, outer node
//!   first, then pattern order on the same node) and the first capture covering a span wins: a
//!   whole node (comment, string) hides everything inside it, and on one node the earlier pattern
//!   wins. So, as in a hand-written classifier, specific patterns come before general ones. Only
//!   leaves should be captured for per-token classes (a non-leaf capture paints its whole span).
//! - `tags.scm`: the nodes the pack reads symbols, imports and refs from ([`Tag`]s, outermost
//!   first in document order). The pack's code turns them into facts (names, qualnames, scopes).
//!
//! [`anonymous_block`] generates the part of a `highlights.scm` that classifies every anonymous
//! (keyword, punctuation, operator) token of a grammar; packs check in its output and a test keeps
//! it in sync with the grammar.

use cv_lang::{LineTokens, token};
use tree_sitter::{Language, Node, Query, QueryCursor, StreamingIterator};

use crate::{Text, Tokens};

/// Compiles a built-in query; a broken built-in query is a programming error.
pub fn compile(language: &Language, what: &str, src: &str) -> Query {
    Query::new(language, src).unwrap_or_else(|e| panic!("built-in {what} query: {e}"))
}

/// Token class of a highlight capture name (its first dotted part).
pub fn capture_class(name: &str) -> u32 {
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
        "decorator" | "attribute" => token::DECORATOR,
        "tag" => token::TAG,
        "invalid" => token::INVALID,
        _ => token::PLAIN,
    }
}

/// One match of a `tags.scm` pattern: its captures by name.
pub struct Tag<'t> {
    pub pattern: usize,
    pub captures: Vec<(&'static str, Node<'t>)>,
}

impl<'t> Tag<'t> {
    /// The node captured as `name`.
    pub fn get(&self, name: &str) -> Option<Node<'t>> {
        self.captures.iter().find(|c| c.0 == name).map(|c| c.1)
    }

    /// The outermost capture: the pattern's main node (`@call` in `(call function: (_) @callee)
    /// @call`).
    pub fn main(&self) -> (&'static str, Node<'t>) {
        *self
            .captures
            .iter()
            .min_by_key(|c| (c.1.start_byte(), std::cmp::Reverse(c.1.end_byte())))
            .expect("a tags pattern captures its node")
    }
}

/// A pack's `highlights.scm` and `tags.scm`, compiled as one query.
pub struct PackQuery {
    query: Query,
    /// Patterns `0..highlights` come from `highlights.scm`.
    highlights: usize,
    classes: Vec<u32>,
}

impl PackQuery {
    pub fn new(language: &Language, highlights: &str, tags: &str) -> Self {
        let n = compile(language, "highlights", highlights).pattern_count();
        compile(language, "tags", tags);
        let query = compile(language, "pack", &format!("{highlights}\n{tags}"));
        let classes = query
            .capture_names()
            .iter()
            .map(|n| capture_class(n))
            .collect();
        PackQuery {
            query,
            highlights: n,
            classes,
        }
    }

    /// Highlight tokens per line and the tag matches (outermost first in document order).
    pub fn run<'t>(&'static self, root: Node<'t>, text: &Text) -> (Vec<LineTokens>, Vec<Tag<'t>>) {
        let (painter, tags) = self.collect(root, text);
        (painter.paint(text, self, |_, _, cls| cls), tags)
    }

    /// The tag matches, and the highlight captures to paint once the caller has read the tags
    /// (for a [`Painter::paint`] `adjust` hook that needs what the tags said).
    pub fn collect<'t>(&'static self, root: Node<'t>, text: &Text) -> (Painter<'t>, Vec<Tag<'t>>) {
        let names: &'static [&'static str] = self.query.capture_names();
        let mut caps = Vec::new();
        let mut tags = Vec::new();
        let mut cursor = QueryCursor::new();
        let mut it = cursor.matches(&self.query, root, text.src.as_bytes());
        while let Some(m) = it.next() {
            if m.pattern_index < self.highlights {
                for c in m.captures() {
                    let n = c.node;
                    caps.push((
                        n.start_byte(),
                        std::cmp::Reverse(n.end_byte()),
                        m.pattern_index,
                        n,
                        c.index,
                    ));
                }
            } else {
                tags.push(Tag {
                    pattern: m.pattern_index - self.highlights,
                    captures: m
                        .captures()
                        .iter()
                        .map(|c| (names[c.index as usize], c.node))
                        .collect(),
                });
            }
        }
        tags.sort_by_key(|t| {
            let n = t.main().1;
            (n.start_byte(), std::cmp::Reverse(n.end_byte()), t.pattern)
        });
        (Painter { caps }, tags)
    }
}

/// Collected highlight captures, painted into tokens by [`Painter::paint`].
pub struct Painter<'t> {
    caps: Vec<(usize, std::cmp::Reverse<usize>, usize, Node<'t>, u32)>,
}

impl<'t> Painter<'t> {
    /// Tokens per line; `adjust(node, capture name, class)` may change a capture's class (for
    /// facts a query cannot see, such as names the pack resolved while reading its tags).
    pub fn paint<F>(mut self, text: &Text, q: &PackQuery, mut adjust: F) -> Vec<LineTokens>
    where
        F: FnMut(Node<'t>, &str, u32) -> u32,
    {
        let names = q.query.capture_names();
        self.caps.sort_unstable_by_key(|c| (c.0, c.1, c.2));
        let mut toks = Tokens::new(text);
        for (_, _, _, n, i) in self.caps {
            let i = i as usize;
            toks.node(n, adjust(n, names[i], q.classes[i]));
        }
        toks.lines
    }
}

pub const ANONYMOUS_BEGIN: &str =
    "; BEGIN anonymous tokens (generated, see cv_lang_sitter::query::anonymous_block)";
pub const ANONYMOUS_END: &str = "; END anonymous tokens";

/// The generated `highlights.scm` block classifying every visible anonymous token of `language`
/// with `class` (a token class constant): one alternation per class, names sorted. `inner` lists
/// anonymous kinds that are not leaves (aliased sequences such as `not in`): they are left out, so
/// their own tokens are classified instead.
pub fn anonymous_block(language: &Language, inner: &[&str], class: impl Fn(&str) -> u32) -> String {
    let mut by: std::collections::BTreeMap<u32, std::collections::BTreeSet<String>> =
        Default::default();
    for id in 0..language.node_kind_count() as u16 {
        if language.node_kind_is_named(id) || !language.node_kind_is_visible(id) {
            continue;
        }
        let Some(kind) = language.node_kind_for_id(id) else {
            continue;
        };
        if kind.is_empty() || inner.contains(&kind) {
            continue;
        }
        by.entry(class(kind)).or_default().insert(kind.to_string());
    }
    let mut out = String::from(ANONYMOUS_BEGIN);
    out.push('\n');
    for (cls, kinds) in by {
        let name = match cls {
            token::KEYWORD => "keyword",
            token::PUNCTUATION => "punctuation",
            token::OPERATOR => "operator",
            token::PLAIN => continue,
            _ => panic!("anonymous token class {cls} has no capture name"),
        };
        out.push('[');
        for (i, k) in kinds.iter().enumerate() {
            out.push_str(if i % 8 == 0 { "\n " } else { " " });
            out.push('"');
            for ch in k.chars() {
                match ch {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    '\0' => out.push_str("\\0"),
                    c => out.push(c),
                }
            }
            out.push('"');
        }
        out.push_str(&format!("\n] @{name}\n"));
    }
    out.push_str(ANONYMOUS_END);
    out
}

/// The generated block as it stands in a query file (between the markers, inclusive).
pub fn anonymous_block_in(src: &str) -> Option<&str> {
    let s = src.find(ANONYMOUS_BEGIN)?;
    let e = src[s..].find(ANONYMOUS_END)? + s + ANONYMOUS_END.len();
    Some(&src[s..e])
}
