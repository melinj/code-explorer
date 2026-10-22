//! Channel endpoint sites for Rust (DESIGN §4.6): runs the channel packs' queries (`lang = "rust"`
//! rules such as `rust-axum.toml` and `rust-std.toml`) and folds the captured keys with this pack's
//! folder: literals, raw strings, `format!` / `concat!`, `+`, `String::from` / `.to_string()`, and
//! constants. File constants fold in place; other names become references that the channel core
//! resolves through `use` imports, and every top-level `const` / `static` (and `Type::CONST` in an
//! impl) is exported for other files to fold.
//!
//! **Scopes.** A site whose rule captures no `@scope` is scoped by where it is built, so router
//! prefixes compose: inside the arguments of a `.nest(prefix, …)` call it shares that call's scope;
//! otherwise it takes the nearest enclosing `let` binding name, else the enclosing function name.
//! `.nest("/api", api())` and `.nest("/api", api)` scope their prefix by the callee or binding
//! name, so the routes built in `fn api()` (or bound to `let api = …`) get the prefix.

use cv_lang::keys::{self, FoldTransform, KeyExpr, SiteExtractor, SiteFacts, SiteQuery};
use cv_lang_sitter::Text;
use cv_lang_sitter::sites::CompiledSites;
use tree_sitter::Node;

use crate::{Folder, KeyPart, file_consts, parse};

pub struct RustSites {
    compiled: CompiledSites,
}

impl RustSites {
    pub fn new(queries: &[SiteQuery], _transforms: &[FoldTransform]) -> Self {
        RustSites {
            compiled: CompiledSites::new(&tree_sitter_rust::LANGUAGE.into(), queries),
        }
    }
}

/// Cheap pre-check: could this file define a string constant other files fold in?
fn maybe_consts(source: &str) -> bool {
    (source.contains("const ") || source.contains("static ")) && source.contains(": &")
}

impl SiteExtractor for RustSites {
    fn extract(&self, _path: &str, source: &str) -> SiteFacts {
        if self.compiled.is_empty() || (!self.compiled.wants(source) && !maybe_consts(source)) {
            return SiteFacts::default();
        }
        let Some(tree) = parse(source) else {
            return SiteFacts::default();
        };
        let root = tree.root_node();
        let text = Text::new(source);
        let consts = file_consts(root, &text);
        let fold = |n: Node| -> KeyExpr {
            let mut parts = Vec::new();
            Folder {
                text: &text,
                consts: &consts,
                depth: 0,
                refs: true,
            }
            .fold(n, &mut parts);
            to_key(parts, text.node_text(n))
        };
        let sites = if self.compiled.wants(source) {
            self.compiled
                .run_scoped(root, &text, &mut |n| fold(n), &mut |n| scope_of(n, &text))
        } else {
            Vec::new()
        };
        let mut out = Vec::new();
        export_consts(root, &text, None, &fold, &mut out);
        SiteFacts { sites, consts: out }
    }
}

/// Pack key parts → the shared key algebra. A key with neither a literal nor a reference is a
/// runtime value.
fn to_key(parts: Vec<KeyPart>, src: &str) -> KeyExpr {
    if !parts
        .iter()
        .any(|p| matches!(p, KeyPart::Lit(s) if !s.is_empty()) || matches!(p, KeyPart::Ref(..)))
    {
        return KeyExpr::unknown(src);
    }
    let mut k = KeyExpr::default();
    for p in parts {
        k.push(match p {
            KeyPart::Lit(s) => keys::KeyPart::Lit(s),
            KeyPart::Segment => keys::KeyPart::Segment,
            KeyPart::Unknown(h) => keys::KeyPart::Unknown {
                hint: keys::shorten(&h),
            },
            KeyPart::Ref(qualifier, name) => keys::KeyPart::Ref { qualifier, name },
        });
    }
    k
}

/// Top-level (and inline-module) `const` / `static` items as `NAME`, impl constants as
/// `Type.NAME`, with their folded values.
fn export_consts(
    body: Node,
    text: &Text,
    owner: Option<&str>,
    fold: &dyn Fn(Node) -> KeyExpr,
    out: &mut Vec<(String, KeyExpr)>,
) {
    let mut c = body.walk();
    for item in body.named_children(&mut c) {
        match item.kind() {
            "const_item" | "static_item" => {
                let (Some(name), Some(value)) = (
                    item.child_by_field_name("name"),
                    item.child_by_field_name("value"),
                ) else {
                    continue;
                };
                let k = fold(value);
                if k.has_lit() {
                    let n = text.node_text(name);
                    out.push((
                        match owner {
                            Some(o) => format!("{o}.{n}"),
                            None => n.to_string(),
                        },
                        k,
                    ));
                }
            }
            "impl_item" if owner.is_none() => {
                let ty = item
                    .child_by_field_name("type")
                    .map(|t| text.node_text(t))
                    .map(|t| t.split('<').next().unwrap_or(t).trim().to_string());
                if let (Some(ty), Some(b)) = (ty, item.child_by_field_name("body")) {
                    export_consts(b, text, Some(&ty), fold, out);
                }
            }
            "mod_item" if owner.is_none() => {
                if let Some(b) = item.child_by_field_name("body") {
                    export_consts(b, text, None, fold, out);
                }
            }
            _ => {}
        }
    }
}

/// Is `call` a `.nest(…)` method call?
fn is_nest(call: Node, text: &Text) -> bool {
    call.kind() == "call_expression"
        && call
            .child_by_field_name("function")
            .filter(|f| f.kind() == "field_expression")
            .and_then(|f| f.child_by_field_name("field"))
            .is_some_and(|f| text.node_text(f) == "nest")
}

/// Where a site is built (see the module docs): a `.nest` call's arguments, a `let` binding or a
/// function.
fn scope_of(node: Node, text: &Text) -> Option<String> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        match n.kind() {
            "arguments" if n.parent().is_some_and(|p| is_nest(p, text)) => {
                let p = n.parent()?;
                return Some(format!("nest@{}", p.start_byte()));
            }
            "let_declaration" => {
                if let Some(pat) = n.child_by_field_name("pattern")
                    && pat.kind() == "identifier"
                {
                    return Some(text.node_text(pat).to_string());
                }
            }
            "function_item" => {
                return n
                    .child_by_field_name("name")
                    .map(|x| text.node_text(x).to_string());
            }
            _ => {}
        }
        cur = n.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use cv_lang::keys::KeyPart as K;

    fn q(id: &str, query: &str) -> SiteQuery {
        SiteQuery {
            id: id.into(),
            query: query.into(),
            hints: vec![],
        }
    }

    #[test]
    fn folds_format_concat_and_constants_and_exports_consts() {
        let src = r#"
const BASE: &str = "/api/v1";
pub const REPO: &str = concat!("/api/v1", "/repo");
struct Paths;
impl Paths { const LANES: &'static str = "/api/v1/lanes"; }
fn f(client: &C, id: &str) {
    client.get(format!("{BASE}/views/{}", id));
    client.post(paths::TILES);
    client.get(url);
}
"#;
        let queries = [q(
            "c",
            r#"(call_expression function: (field_expression value: (identifier) @_o field: (field_identifier) @method)
                arguments: (arguments . (_) @key) (#eq? @_o "client"))"#,
        )];
        let s = RustSites::new(&queries, &[]).extract("x.rs", src);
        let got: Vec<(String, Option<&str>, u32)> = s
            .sites
            .iter()
            .map(|x| (x.key.render(), x.method.as_deref(), x.line))
            .collect();
        assert_eq!(
            got,
            vec![
                ("/api/v1/views/{id}".to_string(), Some("get"), 7),
                ("{paths.TILES}".to_string(), Some("post"), 8),
                ("{url}".to_string(), Some("get"), 9),
            ]
        );
        assert_eq!(
            s.sites[1].key.parts,
            vec![K::Ref {
                qualifier: Some("paths".into()),
                name: "TILES".into()
            }]
        );
        let consts: Vec<(&str, String)> = s
            .consts
            .iter()
            .map(|(n, k)| (n.as_str(), k.render()))
            .collect();
        assert_eq!(
            consts,
            vec![
                ("BASE", "/api/v1".to_string()),
                ("REPO", "/api/v1/repo".to_string()),
                ("Paths.LANES", "/api/v1/lanes".to_string()),
            ]
        );
    }

    #[test]
    fn scopes_follow_nest_bindings_and_functions() {
        let src = r#"
fn app() -> Router {
    let api = Router::new().route("/a", get(a));
    Router::new()
        .route("/top", get(t))
        .nest("/v1", api)
        .nest("/v2", v2())
        .nest("/v3", Router::new().route("/c", get(c)))
}
fn v2() -> Router { Router::new().route("/b", get(b)) }
"#;
        let queries = [
            q(
                "route",
                r#"(call_expression function: (field_expression field: (field_identifier) @_r)
                    arguments: (arguments . (string_literal) @key) (#eq? @_r "route"))"#,
            ),
            q(
                "nest",
                r#"(call_expression function: (field_expression field: (field_identifier) @_n)
                    arguments: (arguments . (_) @prefix . [(identifier) @scope (call_expression function: (identifier) @scope) (call_expression function: (field_expression))])
                    (#eq? @_n "nest"))"#,
            ),
        ];
        let s = RustSites::new(&queries, &[]).extract("x.rs", src);
        let mut got: Vec<(bool, String, String)> = s
            .sites
            .iter()
            .map(|x| {
                (
                    x.is_prefix,
                    x.key.render(),
                    x.scope.clone().unwrap_or_default(),
                )
            })
            .collect();
        got.sort();
        let v3 = got[2].2.clone();
        assert!(v3.starts_with("nest@"), "{v3}");
        assert_eq!(
            got,
            vec![
                (false, "/a".into(), "api".into()),
                (false, "/b".into(), "v2".into()),
                (false, "/c".into(), v3.clone()),
                (false, "/top".into(), "app".into()),
                (true, "/v1".into(), "api".into()),
                (true, "/v2".into(), "v2".into()),
                (true, "/v3".into(), v3),
            ]
        );
    }
}
