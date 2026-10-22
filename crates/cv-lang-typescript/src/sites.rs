//! Channel endpoint sites for TypeScript / JavaScript: runs the channel packs' queries and folds
//! captured keys (strings, template literals, `+`, `||`/`??` fallbacks, module constants).

use cv_lang::keys::{
    FoldTransform, KeyExpr, KeyPart, SiteExtractor, SiteFacts, SiteQuery, apply_transform,
};
use cv_lang_sitter::Text;
use cv_lang_sitter::sites::CompiledSites;
use tree_sitter::{Node, Parser};

pub struct TsSites {
    ts: CompiledSites,
    tsx: CompiledSites,
    transforms: Vec<FoldTransform>,
}

impl TsSites {
    pub fn new(queries: &[SiteQuery], transforms: &[FoldTransform]) -> Self {
        TsSites {
            ts: CompiledSites::new(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), queries),
            tsx: CompiledSites::new(&tree_sitter_typescript::LANGUAGE_TSX.into(), queries),
            transforms: transforms.to_vec(),
        }
    }
}

impl SiteExtractor for TsSites {
    fn extract(&self, path: &str, source: &str) -> SiteFacts {
        let plain_ts = path.ends_with(".ts") || path.ends_with(".mts") || path.ends_with(".cts");
        let (compiled, lang) = if plain_ts {
            (&self.ts, tree_sitter_typescript::LANGUAGE_TYPESCRIPT)
        } else {
            (&self.tsx, tree_sitter_typescript::LANGUAGE_TSX)
        };
        // Files without endpoint sites still define constants other files fold in.
        if compiled.is_empty() || (!compiled.wants(source) && !maybe_consts(source)) {
            return SiteFacts::default();
        }
        let mut parser = Parser::new();
        parser
            .set_language(&lang.into())
            .expect("typescript grammar");
        let Some(tree) = parser.parse(source, None) else {
            return SiteFacts::default();
        };
        let text = Text::new(source);
        let f = Folder {
            text: &text,
            transforms: &self.transforms,
        };
        let root = tree.root_node();
        let sites = compiled.run(root, &text, &mut |n| f.fold(n));
        let mut consts = Vec::new();
        f.consts(root, &mut consts);
        SiteFacts { sites, consts }
    }
}

/// Cheap pre-check: could this file define a string constant?
fn maybe_consts(source: &str) -> bool {
    ["= \"", "= '", "= f\"", "= f'", "= `"]
        .iter()
        .any(|p| source.contains(p))
}

struct Folder<'t, 'a> {
    text: &'t Text<'a>,
    transforms: &'t [FoldTransform],
}

impl Folder<'_, '_> {
    fn t(&self, n: Node) -> &str {
        self.text.node_text(n)
    }

    fn fold(&self, n: Node) -> KeyExpr {
        match n.kind() {
            "string" => {
                let mut k = KeyExpr::lit("");
                let mut c = n.walk();
                for p in n.named_children(&mut c) {
                    match p.kind() {
                        "string_fragment" => k.push(KeyPart::Lit(self.t(p).to_string())),
                        "escape_sequence" => {
                            k.push(KeyPart::Lit(self.t(p).trim_start_matches('\\').to_string()))
                        }
                        _ => {}
                    }
                }
                k
            }
            "template_string" => {
                let mut k = KeyExpr::lit("");
                let mut c = n.walk();
                for p in n.named_children(&mut c) {
                    match p.kind() {
                        "string_fragment" => k.push(KeyPart::Lit(self.t(p).to_string())),
                        "escape_sequence" => {
                            k.push(KeyPart::Lit(self.t(p).trim_start_matches('\\').to_string()))
                        }
                        "template_substitution" => match p.named_child(0) {
                            Some(e) if matches!(e.kind(), "identifier" | "member_expression") => {
                                k.push_expr(self.fold(e))
                            }
                            _ => k.push(KeyPart::Unknown {
                                hint: cv_lang::keys::shorten(self.t(p)),
                            }),
                        },
                        _ => {}
                    }
                }
                k
            }
            "parenthesized_expression"
            | "as_expression"
            | "satisfies_expression"
            | "non_null_expression" => match n.named_child(0) {
                Some(x) => self.fold(x),
                None => KeyExpr::unknown(self.t(n)),
            },
            "identifier" => KeyExpr::reference(None, self.t(n)),
            "member_expression" => {
                let (Some(obj), Some(prop)) = (
                    n.child_by_field_name("object"),
                    n.child_by_field_name("property"),
                ) else {
                    return KeyExpr::unknown(self.t(n));
                };
                if matches!(obj.kind(), "identifier" | "member_expression" | "this") {
                    KeyExpr::reference(Some(self.t(obj)), self.t(prop))
                } else {
                    KeyExpr::unknown(self.t(n))
                }
            }
            "binary_expression" => {
                let op = n
                    .child_by_field_name("operator")
                    .map(|o| self.t(o))
                    .unwrap_or("");
                let (Some(l), Some(r)) = (
                    n.child_by_field_name("left"),
                    n.child_by_field_name("right"),
                ) else {
                    return KeyExpr::unknown(self.t(n));
                };
                match op {
                    "+" => {
                        let mut k = self.fold(l);
                        k.push_expr(self.fold(r));
                        k
                    }
                    // `env.X || '/api'`: the literal fallback is what static analysis can see.
                    "||" | "??" => match self.fold(r) {
                        k if k.as_lit().is_some() => k,
                        _ => KeyExpr::unknown(self.t(n)),
                    },
                    _ => KeyExpr::unknown(self.t(n)),
                }
            }
            "call_expression" => {
                let name = n
                    .child_by_field_name("function")
                    .map(|f| match f.kind() {
                        "member_expression" => f
                            .child_by_field_name("property")
                            .map(|p| self.t(p))
                            .unwrap_or(""),
                        _ => self.t(f),
                    })
                    .unwrap_or("");
                let first = n
                    .child_by_field_name("arguments")
                    .and_then(|a| a.named_child(0));
                match (self.transforms.iter().find(|t| t.function == name), first) {
                    (Some(t), Some(a)) => apply_transform(&t.result, self.fold(a)),
                    _ => KeyExpr::unknown(self.t(n)),
                }
            }
            _ => KeyExpr::unknown(self.t(n)),
        }
    }

    /// Top-level `const NAME = value` (also exported).
    fn consts(&self, root: Node, out: &mut Vec<(String, KeyExpr)>) {
        let mut c = root.walk();
        for stmt in root.named_children(&mut c) {
            let decl = if stmt.kind() == "export_statement" {
                match stmt.child_by_field_name("declaration") {
                    Some(d) => d,
                    None => continue,
                }
            } else {
                stmt
            };
            if !matches!(decl.kind(), "lexical_declaration" | "variable_declaration") {
                continue;
            }
            let mut dc = decl.walk();
            for v in decl.named_children(&mut dc) {
                if v.kind() != "variable_declarator" {
                    continue;
                }
                let (Some(name), Some(value)) = (
                    v.child_by_field_name("name"),
                    v.child_by_field_name("value"),
                ) else {
                    continue;
                };
                if name.kind() != "identifier"
                    || !matches!(
                        value.kind(),
                        "string"
                            | "template_string"
                            | "binary_expression"
                            | "identifier"
                            | "member_expression"
                            | "as_expression"
                            | "parenthesized_expression"
                    )
                {
                    continue;
                }
                let k = self.fold(value);
                if k.has_lit() {
                    out.push((self.t(name).to_string(), k));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_templates_and_fallback_constants() {
        let src = r#"
const API_BASE: string = import.meta.env.VITE_API_BASE || '/api';
export async function getNode(nodeId: string, workflow: string) {
  return request(`/node/${nodeId}?workflow=${workflow}`, { method: 'DELETE' });
}
"#;
        let queries = [SiteQuery {
            id: "req".into(),
            query: r#"(call_expression function: (identifier) @_f (#eq? @_f "request")
                arguments: (arguments . (_) @key)) @line"#
                .into(),
            hints: vec![],
        }];
        let s = TsSites::new(&queries, &[]).extract("api.ts", src);
        assert_eq!(s.sites.len(), 1);
        assert_eq!(
            s.sites[0].key.render(),
            "/node/{nodeId}?workflow={workflow}"
        );
        assert_eq!(s.sites[0].line, 4);
        assert_eq!(s.consts[0].0, "API_BASE");
        assert_eq!(s.consts[0].1.render(), "/api");
    }
}
