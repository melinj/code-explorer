//! Channel endpoint sites for Python: runs the channel packs' queries and folds captured keys
//! (string literals, f-strings, `%`/`.format`, `+`, module and class constants, transforms).

use cv_lang::keys::{
    FoldTransform, KeyExpr, KeyPart, SiteExtractor, SiteFacts, SiteQuery, apply_transform,
};
use cv_lang_sitter::Text;
use cv_lang_sitter::sites::CompiledSites;
use tree_sitter::{Node, Parser};

pub struct PythonSites {
    compiled: CompiledSites,
    transforms: Vec<FoldTransform>,
}

impl PythonSites {
    pub fn new(queries: &[SiteQuery], transforms: &[FoldTransform]) -> Self {
        PythonSites {
            compiled: CompiledSites::new(&tree_sitter_python::LANGUAGE.into(), queries),
            transforms: transforms.to_vec(),
        }
    }
}

impl SiteExtractor for PythonSites {
    fn extract(&self, _path: &str, source: &str) -> SiteFacts {
        // Files without endpoint sites still define constants other files fold in.
        if self.compiled.is_empty() || (!self.compiled.wants(source) && !maybe_consts(source)) {
            return SiteFacts::default();
        }
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .expect("python grammar");
        let Some(tree) = parser.parse(source, None) else {
            return SiteFacts::default();
        };
        let text = Text::new(source);
        let f = Folder {
            text: &text,
            transforms: &self.transforms,
        };
        let root = tree.root_node();
        let sites = self.compiled.run(root, &text, &mut |n| f.fold(n));
        let mut consts = Vec::new();
        f.consts(root, None, &mut consts);
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
            "string" => self.string(n),
            "concatenated_string" => {
                let mut k = KeyExpr::default();
                let mut c = n.walk();
                for s in n.named_children(&mut c) {
                    k.push_expr(self.fold(s));
                }
                k
            }
            "parenthesized_expression" => match n.named_child(0) {
                Some(x) => self.fold(x),
                None => KeyExpr::unknown(self.t(n)),
            },
            "identifier" => KeyExpr::reference(None, self.t(n)),
            "attribute" => {
                let (Some(obj), Some(attr)) = (
                    n.child_by_field_name("object"),
                    n.child_by_field_name("attribute"),
                ) else {
                    return KeyExpr::unknown(self.t(n));
                };
                if matches!(obj.kind(), "identifier" | "attribute") {
                    KeyExpr::reference(Some(self.t(obj)), self.t(attr))
                } else {
                    KeyExpr::unknown(self.t(n))
                }
            }
            "binary_operator" => {
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
                    // "a.%s.b" % x: every conversion becomes a hole.
                    "%" => holes(self.fold(l), &['%']),
                    _ => KeyExpr::unknown(self.t(n)),
                }
            }
            "boolean_operator" => {
                // `x or "default"`: the literal fallback is what static analysis can see.
                match n.child_by_field_name("right").map(|r| self.fold(r)) {
                    Some(k) if k.as_lit().is_some() => k,
                    _ => KeyExpr::unknown(self.t(n)),
                }
            }
            "call" => self.call(n),
            _ => KeyExpr::unknown(self.t(n)),
        }
    }

    fn call(&self, n: Node) -> KeyExpr {
        let Some(f) = n.child_by_field_name("function") else {
            return KeyExpr::unknown(self.t(n));
        };
        let args = n.child_by_field_name("arguments");
        let name = match f.kind() {
            "identifier" => self.t(f),
            "attribute" => f
                .child_by_field_name("attribute")
                .map(|a| self.t(a))
                .unwrap_or(""),
            _ => "",
        };
        let first_arg = args.and_then(|a| {
            let mut c = a.walk();
            a.named_children(&mut c)
                .find(|x| x.kind() != "keyword_argument" && x.kind() != "comment")
        });
        if let Some(t) = self.transforms.iter().find(|t| t.function == name)
            && let Some(a) = first_arg
        {
            return apply_transform(&t.result, self.fold(a));
        }
        if name == "format"
            && f.kind() == "attribute"
            && let Some(obj) = f.child_by_field_name("object").map(|o| {
                // `("a" "b").format(…)`: the parenthesised literal.
                if o.kind() == "parenthesized_expression" {
                    o.named_child(0).unwrap_or(o)
                } else {
                    o
                }
            })
            && matches!(obj.kind(), "string" | "concatenated_string")
        {
            return holes(self.fold(obj), &['{']);
        }
        KeyExpr::unknown(self.t(n))
    }

    fn string(&self, n: Node) -> KeyExpr {
        let mut k = KeyExpr::default();
        let mut c = n.walk();
        let mut bytes_or_raw = false;
        let mut fstring = false;
        for part in n.children(&mut c) {
            match part.kind() {
                "string_start" => {
                    let p = self.t(part).to_ascii_lowercase();
                    bytes_or_raw = p.contains('b');
                    fstring = p.contains('f');
                }
                // In an f-string `{{` and `}}` are literal braces.
                "string_content" if fstring => k.push(KeyPart::Lit(
                    self.t(part).replace("{{", "{").replace("}}", "}"),
                )),
                "string_content" => k.push(KeyPart::Lit(self.t(part).to_string())),
                "escape_sequence" => k.push(KeyPart::Lit(unescape(self.t(part)))),
                "interpolation" => {
                    let e = part.child_by_field_name("expression");
                    match e.map(|e| (e.kind(), e)) {
                        Some(("identifier" | "attribute", e)) => k.push_expr(self.fold(e)),
                        _ => k.push(KeyPart::Unknown {
                            hint: cv_lang::keys::shorten(self.t(part)),
                        }),
                    }
                }
                _ => {}
            }
        }
        if bytes_or_raw && k.parts.is_empty() {
            return KeyExpr::unknown(self.t(n));
        }
        if k.parts.is_empty() {
            k.parts.push(KeyPart::Lit(String::new()));
        }
        k
    }

    /// Module-level `NAME = value` and class-level `Class.NAME = value` (one level of classes).
    fn consts(&self, n: Node, class: Option<&str>, out: &mut Vec<(String, KeyExpr)>) {
        let mut c = n.walk();
        for stmt in n.named_children(&mut c) {
            match stmt.kind() {
                "expression_statement" => {
                    let Some(a) = stmt.named_child(0) else {
                        continue;
                    };
                    if a.kind() != "assignment" {
                        continue;
                    }
                    let (Some(l), Some(r)) = (
                        a.child_by_field_name("left"),
                        a.child_by_field_name("right"),
                    ) else {
                        continue;
                    };
                    if l.kind() != "identifier" || !foldable(r) {
                        continue;
                    }
                    let k = self.fold(r);
                    if !k.has_lit() && !k.parts.iter().any(|p| matches!(p, KeyPart::Ref { .. })) {
                        continue;
                    }
                    let name = match class {
                        Some(cl) => format!("{cl}.{}", self.t(l)),
                        None => self.t(l).to_string(),
                    };
                    out.push((name, k));
                }
                "class_definition" if class.is_none() => {
                    if let (Some(name), Some(body)) = (
                        stmt.child_by_field_name("name"),
                        stmt.child_by_field_name("body"),
                    ) {
                        self.consts(body, Some(self.t(name)), out);
                    }
                }
                "decorated_definition" if class.is_none() => {
                    if let Some(d) = stmt.child_by_field_name("definition")
                        && d.kind() == "class_definition"
                        && let (Some(name), Some(body)) =
                            (d.child_by_field_name("name"), d.child_by_field_name("body"))
                    {
                        self.consts(body, Some(self.t(name)), out);
                    }
                }
                _ => {}
            }
        }
    }
}

fn foldable(n: Node) -> bool {
    matches!(
        n.kind(),
        "string"
            | "concatenated_string"
            | "binary_operator"
            | "identifier"
            | "attribute"
            | "call"
            | "parenthesized_expression"
    )
}

/// Replaces `%…` conversions or `{…}` fields inside literals with one-segment holes.
fn holes(k: KeyExpr, open: &[char]) -> KeyExpr {
    let mut out = KeyExpr {
        parts: vec![],
        method: k.method.clone(),
    };
    for p in k.parts {
        let KeyPart::Lit(s) = p else {
            out.push(p);
            continue;
        };
        let mut lit = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if open.contains(&'%') && c == '%' {
                if chars.peek() == Some(&'%') {
                    chars.next();
                    lit.push('%');
                    continue;
                }
                // %s, %d, %(name)s, %-5.2f …
                if chars.peek() == Some(&'(') {
                    for x in chars.by_ref() {
                        if x == ')' {
                            break;
                        }
                    }
                }
                while let Some(&x) = chars.peek() {
                    chars.next();
                    if x.is_ascii_alphabetic() {
                        break;
                    }
                }
                out.push(KeyPart::Lit(std::mem::take(&mut lit)));
                out.push(KeyPart::Segment);
            } else if open.contains(&'{') && c == '{' {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    lit.push('{');
                    continue;
                }
                for x in chars.by_ref() {
                    if x == '}' {
                        break;
                    }
                }
                out.push(KeyPart::Lit(std::mem::take(&mut lit)));
                out.push(KeyPart::Segment);
            } else if open.contains(&'{') && c == '}' && chars.peek() == Some(&'}') {
                // `}}` is a literal `}` in a `.format` template.
                chars.next();
                lit.push('}');
            } else {
                lit.push(c);
            }
        }
        out.push(KeyPart::Lit(lit));
    }
    out
}

fn unescape(e: &str) -> String {
    match e {
        "\\n" => "\n".into(),
        "\\t" => "\t".into(),
        "\\\\" => "\\".into(),
        "\\'" => "'".into(),
        "\\\"" => "\"".into(),
        _ => e.trim_start_matches('\\').to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(id: &str, query: &str) -> SiteQuery {
        SiteQuery {
            id: id.into(),
            query: query.into(),
            hints: vec![],
        }
    }

    #[test]
    fn folds_literals_fstrings_constants_and_transforms() {
        let src = r#"
TOPIC = "world.entity.moved"
class EventType:
    BUILD = "map.district." + "build.requested"

def f(self, kind, x):
    bus.emit(EventEnvelope(topic=TOPIC))
    bus.emit(EventEnvelope(topic=f"world.{kind}.moved"))
    bus.emit(EventEnvelope(topic=self.scoped_topic(EventType.BUILD)))
    bus.emit(EventEnvelope(topic=x))
    bus.emit(EventEnvelope(topic="a.%s.c" % kind))
"#;
        let queries = [q(
            "env",
            r#"(call function: (identifier) @_c (#eq? @_c "EventEnvelope")
                 arguments: (argument_list (keyword_argument name: (identifier) @_k (#eq? @_k "topic") value: (_) @key)))"#,
        )];
        let tr = [FoldTransform {
            function: "scoped_topic".into(),
            result: "{0}.*?".into(),
        }];
        let s = PythonSites::new(&queries, &tr).extract("a.py", src);
        let keys: Vec<String> = s.sites.iter().map(|f| f.key.render()).collect();
        assert_eq!(
            keys,
            vec![
                "{TOPIC}",
                "world.{kind}.moved",
                "{EventType.BUILD}…",
                "{x}",
                "a.{}.c"
            ]
        );
        assert_eq!(s.sites[0].line, 7);
        let consts: Vec<(String, String)> = s
            .consts
            .iter()
            .map(|(n, k)| (n.clone(), k.render()))
            .collect();
        assert_eq!(
            consts,
            vec![
                ("TOPIC".into(), "world.entity.moved".into()),
                (
                    "EventType.BUILD".into(),
                    "map.district.build.requested".into()
                )
            ]
        );
    }

    #[test]
    fn folds_str_format() {
        let src = r#"
def f(kind, node_id):
    bus.emit(EventEnvelope(topic="world.{}.moved".format(kind)))
    bus.emit(EventEnvelope(topic="api/{node_id}/runs".format(node_id=node_id)))
    bus.emit(EventEnvelope(topic=("a.{0}" ".b.{1:>3}").format(kind, 1)))
    bus.emit(EventEnvelope(topic="lit.{{x}}".format()))
    bus.emit(EventEnvelope(topic=tmpl.format(kind)))
"#;
        let queries = [q(
            "env",
            r#"(call function: (identifier) @_c (#eq? @_c "EventEnvelope")
                 arguments: (argument_list (keyword_argument name: (identifier) @_k (#eq? @_k "topic") value: (_) @key)))"#,
        )];
        let s = PythonSites::new(&queries, &[]).extract("a.py", src);
        let keys: Vec<String> = s.sites.iter().map(|f| f.key.render()).collect();
        assert_eq!(
            keys,
            vec![
                "world.{}.moved",
                "api/{}/runs",
                "a.{}.b.{}",
                "lit.{x}",
                "{tmpl.format(kind)}"
            ]
        );
    }
}
