//! Runs channel endpoint queries (rules as data, DESIGN §4.6) on a parsed tree. Captures:
//! `@key` / `@prefix` are folded by the language pack, `@name` is a literal key (its text), `@scope`, `@handler` and `@method` are
//! taken as text, `@line` sets the port line. Captures starting with `_` are for predicates only.

use cv_lang::keys::{KeyExpr, SiteFact, SiteQuery};
use tree_sitter::{Language, Node, Query, QueryCursor, StreamingIterator};

use crate::{Text, line};

struct Compiled {
    index: usize,
    query: Query,
    key: Option<u32>,
    name: Option<u32>,
    prefix: Option<u32>,
    scope: Option<u32>,
    handler: Option<u32>,
    method: Option<u32>,
    line: Option<u32>,
    hints: Vec<String>,
}

/// Endpoint queries compiled for one grammar. Queries that fail to compile are skipped with a
/// warning on stderr (a bad project rule must not stop indexing).
pub struct CompiledSites {
    queries: Vec<Compiled>,
}

impl CompiledSites {
    pub fn new(lang: &Language, queries: &[SiteQuery]) -> Self {
        let mut out = Vec::new();
        for (index, q) in queries.iter().enumerate() {
            match Query::new(lang, &q.query) {
                Ok(query) => {
                    let idx = |n: &str| query.capture_index_for_name(n);
                    out.push(Compiled {
                        index,
                        key: idx("key"),
                        name: idx("name"),
                        prefix: idx("prefix"),
                        scope: idx("scope"),
                        handler: idx("handler"),
                        method: idx("method"),
                        line: idx("line"),
                        hints: q.hints.clone(),
                        query,
                    });
                }
                Err(e) => eprintln!("cv: channel rule {} does not compile: {e}", q.id),
            }
        }
        CompiledSites { queries: out }
    }

    pub fn is_empty(&self) -> bool {
        self.queries.is_empty()
    }

    /// Does any query want this file (its hints occur in the text)?
    pub fn wants(&self, source: &str) -> bool {
        self.queries
            .iter()
            .any(|q| q.hints.is_empty() || q.hints.iter().any(|h| source.contains(h.as_str())))
    }

    pub fn run(
        &self,
        root: Node,
        text: &Text,
        fold: &mut dyn FnMut(Node) -> KeyExpr,
    ) -> Vec<SiteFact> {
        self.run_scoped(root, text, fold, &mut |_| None)
    }

    /// Like [`CompiledSites::run`]; a site whose rule captures no `@scope` takes
    /// `scope_of(key node)` (a pack convention, e.g. the enclosing function or binding).
    pub fn run_scoped(
        &self,
        root: Node,
        text: &Text,
        fold: &mut dyn FnMut(Node) -> KeyExpr,
        scope_of: &mut dyn FnMut(Node) -> Option<String>,
    ) -> Vec<SiteFact> {
        let mut out = Vec::new();
        let mut cursor = QueryCursor::new();
        for q in &self.queries {
            if !q.hints.is_empty() && !q.hints.iter().any(|h| text.src.contains(h.as_str())) {
                continue;
            }
            let mut it = cursor.matches(&q.query, root, text.src.as_bytes());
            while let Some(m) = it.next() {
                let first = |ix: Option<u32>| ix.and_then(|i| m.nodes_for_capture_index(i).next());
                let txt = |ix: Option<u32>| first(ix).map(|n| text.node_text(n).to_string());
                let (node, is_prefix, literal) =
                    match (first(q.key), first(q.name), first(q.prefix)) {
                        (Some(n), _, _) => (n, false, false),
                        (None, Some(n), _) => (n, false, true),
                        (None, None, Some(n)) => (n, true, false),
                        _ => continue,
                    };
                let at = first(q.line).unwrap_or(node);
                let key = if literal {
                    KeyExpr::lit(text.node_text(node))
                } else {
                    fold(node)
                };
                out.push(SiteFact {
                    query: q.index,
                    key,
                    is_prefix,
                    scope: txt(q.scope).or_else(|| scope_of(node)),
                    handler: txt(q.handler),
                    method: txt(q.method),
                    line: line(at),
                });
            }
        }
        // Several patterns of one rule can match the same node: keep one fact per (query, line, key,
        // method), preferring the one that captured a method or handler. Distinct methods on one
        // line (a chained `get(a).post(b)` method router) stay separate sites.
        out.sort_by(|a, b| {
            (
                a.query,
                a.line,
                a.method.is_none(),
                &a.method,
                a.handler.is_none(),
            )
                .cmp(&(
                    b.query,
                    b.line,
                    b.method.is_none(),
                    &b.method,
                    b.handler.is_none(),
                ))
        });
        out.dedup_by(|later, kept| {
            kept.query == later.query
                && kept.line == later.line
                && kept.key == later.key
                && (later.method.is_none() || later.method == kept.method)
        });
        out
    }
}
