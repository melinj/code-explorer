//! Channel facts that language packs provide (DESIGN §4.6): config key paths, folded key
//! expressions at endpoint sites, and module constants for folding across files.
//!
//! Channel packs (`cv-channel-*`) own the rules (tree-sitter queries as data) and the key algebra;
//! language packs run the queries and fold the captured nodes into [`KeyExpr`]s. Nothing here
//! names a language or a protocol.

/// One key path in a config file (L3 `ConfigKeyFact`), e.g. `services.api.environment.PORT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigKeyFact {
    /// Dotted key path from the document root; list items do not add a segment.
    pub path: String,
    /// The last key as written (it may itself contain dots); empty for a list item.
    pub key: String,
    /// 1-based line of the key.
    pub line: u32,
    /// Scalar value as written (unquoted), or the joined text of a `|`/`>` block. `None` for a
    /// mapping or an empty value.
    pub value: Option<String>,
    /// A plain list item (`- value`) under `path`, not a key. Config-field scoring ignores these.
    pub item: bool,
}

/// A partially evaluated key: literal parts plus holes the folder could not resolve.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KeyExpr {
    pub parts: Vec<KeyPart>,
    pub method: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyPart {
    Lit(String),
    /// One segment (e.g. `{instance}`, `${id}`).
    Segment,
    /// Anything to the end, including nothing (an optional suffix from a transform).
    Tail,
    /// Nothing known: a runtime-built value. `hint` is the source text, shortened.
    Unknown {
        hint: String,
    },
    /// A name the folder could not see from here: a module constant, an imported constant or a
    /// class attribute (`qualifier.name`). The channel core resolves it through imports; an
    /// unresolved reference becomes a hole (inside a template) or unknown (as the whole key).
    Ref {
        qualifier: Option<String>,
        name: String,
    },
}

impl KeyExpr {
    pub fn lit(s: impl Into<String>) -> Self {
        KeyExpr {
            parts: vec![KeyPart::Lit(s.into())],
            method: None,
        }
    }

    pub fn unknown(hint: &str) -> Self {
        KeyExpr {
            parts: vec![KeyPart::Unknown {
                hint: shorten(hint),
            }],
            method: None,
        }
    }

    pub fn reference(qualifier: Option<&str>, name: &str) -> Self {
        KeyExpr {
            parts: vec![KeyPart::Ref {
                qualifier: qualifier.map(String::from),
                name: name.to_string(),
            }],
            method: None,
        }
    }

    /// Appends `other`, merging adjacent literals.
    pub fn push_expr(&mut self, other: KeyExpr) {
        for p in other.parts {
            self.push(p);
        }
    }

    pub fn push(&mut self, p: KeyPart) {
        if let KeyPart::Lit(s) = &p {
            if s.is_empty() {
                return;
            }
            if let Some(KeyPart::Lit(last)) = self.parts.last_mut() {
                last.push_str(s);
                return;
            }
        }
        self.parts.push(p);
    }

    /// The whole key is one literal.
    pub fn as_lit(&self) -> Option<&str> {
        match self.parts.as_slice() {
            [KeyPart::Lit(s)] => Some(s),
            [] => Some(""),
            _ => None,
        }
    }

    pub fn has_lit(&self) -> bool {
        self.parts
            .iter()
            .any(|p| matches!(p, KeyPart::Lit(s) if !s.is_empty()))
    }

    /// Source-like rendering for hints and labels: `world.{}.moved`, `{topic}`.
    pub fn render(&self) -> String {
        let mut s = String::new();
        for p in &self.parts {
            match p {
                KeyPart::Lit(l) => s.push_str(l),
                KeyPart::Segment => s.push_str("{}"),
                KeyPart::Tail => s.push('…'),
                KeyPart::Unknown { hint } => {
                    s.push('{');
                    s.push_str(hint);
                    s.push('}');
                }
                KeyPart::Ref { qualifier, name } => {
                    s.push('{');
                    if let Some(q) = qualifier {
                        s.push_str(q);
                        s.push('.');
                    }
                    s.push_str(name);
                    s.push('}');
                }
            }
        }
        s
    }
}

/// Hints stay short: one line, at most 60 characters.
pub fn shorten(s: &str) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > 60 {
        let mut t: String = one.chars().take(59).collect();
        t.push('…');
        t
    } else {
        one
    }
}

/// An endpoint query handed to a language pack. Captures: `@key` (folded), `@prefix` (folded, a
/// prefix for keys in the same scope), `@scope` (text: groups prefixes with keys, e.g. a router
/// variable or a settings class), `@handler` (text), `@method` (text), `@line` (port line).
#[derive(Debug, Clone)]
pub struct SiteQuery {
    pub id: String,
    pub query: String,
    /// Run only on files containing one of these substrings (cheap pre-filter); empty = always.
    pub hints: Vec<String>,
}

/// A transform function the folder applies when it sees a call to it, e.g. `scoped_topic(x)` with
/// result `{0}.*?` (the argument plus an optional segment).
#[derive(Debug, Clone)]
pub struct FoldTransform {
    pub function: String,
    pub result: String,
}

#[derive(Debug, Clone)]
pub struct SiteFact {
    /// Index into the queries the extractor was built from.
    pub query: usize,
    pub key: KeyExpr,
    /// This capture was `@prefix`, not `@key`.
    pub is_prefix: bool,
    pub scope: Option<String>,
    pub handler: Option<String>,
    pub method: Option<String>,
    pub line: u32,
}

#[derive(Debug, Clone, Default)]
pub struct SiteFacts {
    pub sites: Vec<SiteFact>,
    /// Module-level constants (`NAME`) and class constants (`Class.NAME`) with folded values.
    pub consts: Vec<(String, KeyExpr)>,
}

/// Compiled endpoint queries for one language pack; runs on many files in parallel.
pub trait SiteExtractor: Send + Sync {
    fn extract(&self, path: &str, source: &str) -> SiteFacts;
}

/// Applies a transform template to a folded argument: `{0}` is the argument, `*` one segment,
/// a trailing `.*?` (or `*?`) an optional segment, anything else literal text.
pub fn apply_transform(template: &str, arg: KeyExpr) -> KeyExpr {
    let mut out = KeyExpr {
        parts: vec![],
        method: arg.method.clone(),
    };
    let mut rest = template;
    let mut arg = Some(arg);
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("{0}") {
            if let Some(a) = arg.take() {
                out.push_expr(a);
            }
            rest = r;
        } else if rest == ".*?" || rest == "*?" {
            out.push(KeyPart::Tail);
            rest = "";
        } else if let Some(r) = rest.strip_prefix('*') {
            out.push(KeyPart::Segment);
            rest = r;
        } else {
            let c = rest.chars().next().unwrap();
            out.push(KeyPart::Lit(c.to_string()));
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_merges_literals_and_transforms_apply() {
        let mut k = KeyExpr::lit("a.");
        k.push(KeyPart::Lit("b".into()));
        k.push(KeyPart::Segment);
        assert_eq!(k.render(), "a.b{}");
        let t = apply_transform("{0}.*?", KeyExpr::lit("tool.done"));
        assert_eq!(
            t.parts,
            vec![KeyPart::Lit("tool.done".into()), KeyPart::Tail]
        );
        assert_eq!(shorten("a\n   b"), "a b");
    }
}
