//! L4 Rust pack: tree-sitter-rust → symbols (fns, impl members, structs, enums, traits, inline
//! modules, consts, `#[test]` fns), imports (`use` trees, `mod` declarations, `extern crate`, and
//! the module paths that qualified calls go through), call / implement / inherit refs, highlight
//! tokens, key folding (`format!`, `concat!`, literals, file constants) and channel endpoint sites
//! ([`sites`]: axum routes and `nest` prefixes, reqwest clients, `std::env::var` / `env!`).
//!
//! **Fact conventions** (so the language-neutral linker in cv-link resolves Rust without knowing it):
//! - An `impl` block is a container symbol of kind `Class` named after its self type, so `self.f()`
//!   resolves to a method of the enclosing impl. Inherent impls are listed before the type's
//!   definition, so `Type::f()` and `use …::Type` reach the impl that holds `f`.
//! - `Self::f()` is reported with the impl's type as qualifier.
//! - A call through a module path (`crate::a::f()`, `lang::chunk::f()`, `other_crate::f()`) adds an
//!   implicit import whose local name is the qualifier as written, bound to that module (or to the
//!   type when the path ends in a CamelCase segment).
//! - Calls on any other receiver (`x.len()`, `foo().bar()`) keep a non-binding qualifier (`x`,
//!   `<expr>`) so they never resolve to a same-named free function.
//! - `impl Trait for T` → an `implement` ref to `Trait`; supertraits → `inherit` refs.
//! - Calls inside macro arguments (`assert_eq!(f(x), …)`) are recovered from the token trees.
//!
//! **Import resolution** follows the `mod` tree from the crate root of the importing file (`lib.rs`,
//! `main.rs`, `src/bin/*`, `tests/*`, `examples/*`, `benches/*`), `crate` / `self` / `super`, other
//! crates by package name (every `Cargo.toml` with a `[package]`, which covers the workspace
//! members, honouring `[lib] path`), and follows `pub use` re-exports to the defining file.

mod sites;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use cv_lang::{
    BlobFacts, Conventions, ImportFact, LaneHints, LanguagePack, ProjectIndex, RefFact, RefKind,
    SymbolFact, SymbolKind, dirname, hash, normalize_path,
};
use cv_lang_sitter::query::{PackQuery, Tag};
use cv_lang_sitter::{Text, count_errors, end_line, line};
use std::sync::OnceLock;
use tree_sitter::{Node, Parser, Point, Tree};

#[derive(Default)]
pub struct RustPack {
    cache: Mutex<Cache>,
}

#[derive(Default)]
struct Cache {
    /// Hash of the Cargo manifests (paths and contents) the crate map was built from.
    crates_key: u64,
    crates: Arc<HashMap<String, String>>,
    /// Module file → (content hash, what it defines and re-exports).
    modules: HashMap<String, (u64, Arc<ModInfo>)>,
}

/// Builtin and standard-library names lexical candidates never guess (`Conventions`).
const BUILTIN_METHODS: &[&str] = &[
    "push",
    "pop",
    "insert",
    "remove",
    "get",
    "get_mut",
    "iter",
    "iter_mut",
    "into_iter",
    "map",
    "filter",
    "filter_map",
    "flat_map",
    "collect",
    "unwrap",
    "expect",
    "clone",
    "len",
    "is_empty",
    "contains",
    "contains_key",
    "entry",
    "or_insert",
    "or_insert_with",
    "or_default",
    "extend",
    "as_str",
    "as_ref",
    "as_mut",
    "to_string",
    "to_owned",
    "push_str",
    "split",
    "trim",
    "starts_with",
    "ends_with",
    "join",
    "lock",
    "read",
    "write",
    "send",
    "recv",
    "ok",
    "err",
    "and_then",
    "map_err",
    "unwrap_or",
    "unwrap_or_default",
    "unwrap_or_else",
    "sort",
    "sort_by",
    "sort_by_key",
    "sort_unstable",
    "dedup",
    "retain",
    "drain",
    "take",
    "skip",
    "first",
    "last",
    "chars",
    "bytes",
    "lines",
    "parse",
    "keys",
    "values",
    "enumerate",
    "zip",
    "rev",
    "fold",
    "any",
    "all",
    "find",
    "position",
    "count",
    "sum",
    "max",
    "min",
    "cloned",
    "copied",
    "as_deref",
    "is_some",
    "is_none",
    "is_ok",
    "is_err",
    "truncate",
    "split_once",
    "rsplit",
    "strip_prefix",
    "strip_suffix",
    "replace",
    "to_lowercase",
    "to_uppercase",
];
const BUILTIN_RECEIVERS: &[&str] = &[
    "std", "core", "alloc", "String", "Vec", "HashMap", "HashSet", "BTreeMap", "BTreeSet", "Box",
    "Rc", "Arc", "Option", "Result", "Some", "Ok", "Err",
];
const BUILTIN_FUNCTIONS: &[&str] = &["Some", "Ok", "Err", "drop"];

impl LanguagePack for RustPack {
    fn id(&self) -> &'static str {
        "rust"
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn claims(&self, path: &str) -> bool {
        path.ends_with(".rs")
    }
    fn lang(&self, _: &str) -> &'static str {
        "rust"
    }
    fn conventions(&self) -> Conventions {
        Conventions {
            self_receivers: &["self"],
            // `a/mod.rs` is module `a`.
            module_entry_stems: &["mod"],
            // Crate roots.
            entry_stems: &["lib", "main"],
            reserved_prefixes: &["__"],
            builtin_methods: BUILTIN_METHODS,
            builtin_receivers: BUILTIN_RECEIVERS,
            builtin_functions: BUILTIN_FUNCTIONS,
        }
    }
    fn lane_hints(&self) -> LaneHints {
        LaneHints {
            code: vec!["*.rs"],
            // `tests/` is already a test dir; these catch split-out unit-test files.
            test: vec!["*_test.rs", "*_tests.rs", "tests.rs"],
        }
    }
    fn analyze(&self, _path: &str, source: &str) -> BlobFacts {
        analyze(source)
    }
    fn site_extractor(
        &self,
        queries: &[cv_lang::keys::SiteQuery],
        transforms: &[cv_lang::keys::FoldTransform],
    ) -> Option<Box<dyn cv_lang::keys::SiteExtractor>> {
        Some(Box::new(sites::RustSites::new(queries, transforms)))
    }
    fn resolve_import(
        &self,
        from: &str,
        import: &ImportFact,
        project: &dyn ProjectIndex,
    ) -> Option<String> {
        Resolver {
            pack: self,
            project,
        }
        .resolve(from, import)
    }
}

pub(crate) fn parse(source: &str) -> Option<Tree> {
    let mut p = Parser::new();
    p.set_language(&tree_sitter_rust::LANGUAGE.into())
        .expect("rust grammar");
    p.parse(source, None)
}

/// `queries/highlights.scm` and `queries/tags.scm` (DESIGN §4.2), compiled once.
fn queries() -> &'static PackQuery {
    static Q: OnceLock<PackQuery> = OnceLock::new();
    Q.get_or_init(|| {
        PackQuery::new(
            &tree_sitter_rust::LANGUAGE.into(),
            include_str!("../queries/highlights.scm"),
            include_str!("../queries/tags.scm"),
        )
    })
}

pub fn analyze(source: &str) -> BlobFacts {
    let text = Text::new(source);
    let Some(tree) = parse(source) else {
        return BlobFacts {
            lines: text.line_count(),
            tokens: vec![Vec::new(); text.line_count() as usize],
            ..Default::default()
        };
    };
    let root = tree.root_node();
    let mut facts = BlobFacts {
        lines: text.line_count(),
        syntax_errors: count_errors(root),
        ..Default::default()
    };
    let (tokens, tags) = queries().run(root, &text);
    let mut w = Walker {
        text: &text,
        facts: &mut facts,
        inherent: Vec::new(),
        paths: HashSet::new(),
    };
    w.read(root, &tags);
    let inherent = std::mem::take(&mut w.inherent);
    let paths = std::mem::take(&mut w.paths);
    add_path_imports(&mut facts, &paths);
    reorder_inherent_impls(&mut facts, &inherent);
    dedup_qualnames(&mut facts);
    facts.tokens = tokens;
    facts
}

/// Where items are being declared.
#[derive(Default)]
struct Scope {
    /// Enclosing container symbol (impl, trait or inline mod).
    parent: Option<usize>,
    /// Qualname prefix, e.g. `tests::` or `Foo::`.
    prefix: String,
    /// Inside an impl or trait: functions are methods.
    in_type: bool,
    /// The impl's self type, for `Self::`.
    self_ty: Option<String>,
    /// Inline-module depth (each level turns one leading `super` into `self`).
    mod_depth: usize,
}

/// A node whose tags are read in a particular way, until its end.
struct Open {
    end: usize,
    what: Region,
}

enum Region {
    /// A container (the file, an impl, a trait, an inline module): items in its body (node id,
    /// byte range) are read in the scope with this index.
    Items((usize, usize, usize), usize),
    /// An item: refs inside this byte range (a body, a value) belong to the symbol; nothing else
    /// inside it is read.
    Refs(Option<(usize, usize)>, Option<usize>, usize),
}

impl Open {
    /// Nothing inside is read.
    fn skip(n: Node) -> Self {
        Open {
            end: n.end_byte(),
            what: Region::Refs(None, None, 0),
        }
    }
}

#[derive(Clone, Copy)]
enum Ctx {
    Items((usize, usize, usize), usize),
    Refs(Option<usize>, usize),
}

struct Walker<'t, 'a, 'f> {
    text: &'t Text<'a>,
    facts: &'f mut BlobFacts,
    /// Top-level inherent impl symbols (listed before type definitions at the end).
    inherent: Vec<usize>,
    /// Qualifiers that are `::` paths (not receivers).
    paths: HashSet<String>,
}

/// Last path segment of a type, without generics or references: `&mut a::Foo<T>` → `Foo`.
fn type_name(n: Node, text: &Text) -> String {
    match n.kind() {
        "type_identifier" | "identifier" | "primitive_type" => text.node_text(n).to_string(),
        "generic_type" | "reference_type" | "pointer_type" => n
            .child_by_field_name("type")
            .map(|t| type_name(t, text))
            .unwrap_or_default(),
        "scoped_type_identifier" | "scoped_identifier" => n
            .child_by_field_name("name")
            .map(|t| type_name(t, text))
            .unwrap_or_default(),
        _ => text
            .node_text(n)
            .split('<')
            .next()
            .unwrap_or("")
            .trim()
            .to_string(),
    }
}

/// Attributes and `///` doc comments directly above an item: (first line, is a test).
fn leading_attrs(n: Node, text: &Text) -> (u32, bool) {
    let mut start = line(n);
    let mut test = false;
    let mut prev = n.prev_sibling();
    while let Some(p) = prev {
        let doc = matches!(p.kind(), "line_comment" | "block_comment") && {
            let t = text.node_text(p);
            t.starts_with("///") || t.starts_with("/**")
        };
        if (p.kind() != "attribute_item" && !doc) || end_line(p) + 1 < start {
            break;
        }
        if p.kind() == "attribute_item" {
            let t = text.node_text(p);
            let inner = t
                .trim_start_matches("#[")
                .trim_end_matches(']')
                .split('(')
                .next()
                .unwrap_or("")
                .trim();
            if inner == "test" || inner.ends_with("::test") || inner == "rstest" {
                test = true;
            }
        }
        start = line(p);
        prev = p.prev_sibling();
    }
    (start, test)
}

fn is_camel(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && s.chars().any(|c| c.is_lowercase())
}

impl Walker<'_, '_, '_> {
    fn push(
        &mut self,
        kind: SymbolKind,
        name: String,
        qualname: String,
        node: Node,
        scope: &Scope,
        name_line: u32,
    ) -> usize {
        let (start, _) = leading_attrs(node, self.text);
        self.facts.symbols.push(SymbolFact {
            kind,
            name,
            qualname,
            parent: scope.parent,
            start,
            end: end_line(node),
            name_line,
        });
        self.facts.symbols.len() - 1
    }

    fn name_of(&self, n: Node) -> (String, u32) {
        n.child_by_field_name("name")
            .map(|x| (self.text.node_text(x).to_string(), line(x)))
            .unwrap_or_else(|| (String::new(), line(n)))
    }

    /// Reads the tags in document order. Symbols are the items of the file, of inline modules and
    /// of impl and trait bodies; items nested in a function body only contribute refs (like nested
    /// Python functions), except `use`, which still imports. Refs come from function bodies and
    /// const/static values, attributed to the item, and from any other module-level node (a
    /// macro call, an error), attributed to nothing.
    fn read(&mut self, root: Node, tags: &[Tag]) {
        let mut scopes = vec![Scope::default()];
        let mut open: Vec<Open> = Vec::new();
        let root_body = (root.id(), 0, root.end_byte());
        for tag in tags {
            let (what, node) = tag.main();
            while open.last().is_some_and(|o| o.end <= node.start_byte()) {
                open.pop();
            }
            let within = |r: (usize, usize)| node.start_byte() >= r.0 && node.end_byte() <= r.1;
            let ctx = match open.last().map(|o| &o.what) {
                None => Ctx::Items(root_body, 0),
                Some(&Region::Items(body, scope)) if within((body.1, body.2)) => {
                    Ctx::Items(body, scope)
                }
                Some(&Region::Refs(Some(r), enclosing, scope)) if within(r) => {
                    Ctx::Refs(enclosing, scope)
                }
                _ => continue,
            };
            let (enclosing, scope) = match ctx {
                Ctx::Refs(e, s) => (e, s),
                Ctx::Items(_, s) => (None, s),
            };
            // An item directly in the file or a container's body (not nested in another node).
            let item_level = match ctx {
                Ctx::Items((body, ..), _) => node.parent().map(|p| p.id()) == Some(body),
                Ctx::Refs(..) => false,
            };
            match what {
                "call" => {
                    if let Some(f) = tag.get("callee") {
                        self.call(node, f, enclosing, &scopes[scope]);
                    }
                }
                "macro" => self.macro_call(node, enclosing, &scopes[scope]),
                "import" => self.use_declaration(node, &scopes[scope]),
                "import.crate" if item_level => self.extern_crate(node),
                "attribute" if item_level => open.push(Open::skip(node)),
                _ if item_level => {
                    if let Some(o) = self.item(node, scope, &mut scopes) {
                        open.push(o);
                    }
                }
                _ => {}
            }
        }
    }

    /// A symbol-making item; the region its tags are read in, if any.
    fn item(&mut self, n: Node, scope_id: usize, scopes: &mut Vec<Scope>) -> Option<Open> {
        let scope = &scopes[scope_id];
        let end = n.end_byte();
        let range = |x: Option<Node>| x.map(|x| (x.start_byte(), x.end_byte()));
        match n.kind() {
            "function_item" | "function_signature_item" => {
                let (name, nl) = self.name_of(n);
                let (_, test) = leading_attrs(n, self.text);
                let kind = if test {
                    SymbolKind::Test
                } else if scope.in_type {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                let q = format!("{}{name}", scope.prefix);
                let id = self.push(kind, name, q, n, scope, nl);
                let body = range(n.child_by_field_name("body"));
                Some(Open {
                    end,
                    what: Region::Refs(body, Some(id), scope_id),
                })
            }
            "impl_item" => {
                let Some(inner) = self.impl_item(n, scope) else {
                    return Some(Open::skip(n));
                };
                Some(self.container(n, inner, scopes))
            }
            "trait_item" => {
                let (name, nl) = self.name_of(n);
                let q = format!("{}{name}", scope.prefix);
                let id = self.push(SymbolKind::Interface, name.clone(), q.clone(), n, scope, nl);
                if let Some(bounds) = n.child_by_field_name("bounds") {
                    let mut c = bounds.walk();
                    for b in bounds.named_children(&mut c) {
                        if matches!(
                            b.kind(),
                            "type_identifier" | "scoped_type_identifier" | "generic_type"
                        ) {
                            let (bn, bq) = self.type_path(b);
                            self.facts.refs.push(RefFact {
                                kind: RefKind::Inherit,
                                name: bn,
                                qualifier: bq,
                                line: line(b),
                                enclosing: Some(id),
                            });
                        }
                    }
                }
                let inner = Scope {
                    parent: Some(id),
                    prefix: format!("{q}::"),
                    in_type: true,
                    self_ty: Some(name),
                    mod_depth: scope.mod_depth,
                };
                Some(self.container(n, inner, scopes))
            }
            "struct_item" | "union_item" | "enum_item" | "type_item" | "const_item"
            | "static_item" | "macro_definition" => {
                let (name, nl) = self.name_of(n);
                let kind = match n.kind() {
                    "enum_item" => SymbolKind::Enum,
                    "type_item" => SymbolKind::Type,
                    "const_item" | "static_item" => SymbolKind::Constant,
                    "macro_definition" => SymbolKind::Function,
                    _ => SymbolKind::Struct,
                };
                let q = format!("{}{name}", scope.prefix);
                let id = self.push(kind, name, q, n, scope, nl);
                let value = range(n.child_by_field_name("value"));
                Some(Open {
                    end,
                    what: Region::Refs(value, Some(id), scope_id),
                })
            }
            "mod_item" => {
                let (name, nl) = self.name_of(n);
                if n.child_by_field_name("body").is_none() {
                    self.facts.imports.push(ImportFact {
                        local: name.clone(),
                        module: format!("self::{name}"),
                        imported: None,
                        line: nl,
                    });
                    return None;
                }
                let q = format!("{}{name}", scope.prefix);
                let id = self.push(SymbolKind::Module, name, q.clone(), n, scope, nl);
                let inner = Scope {
                    parent: Some(id),
                    prefix: format!("{q}::"),
                    in_type: false,
                    self_ty: None,
                    mod_depth: scope.mod_depth + 1,
                };
                Some(self.container(n, inner, scopes))
            }
            _ => None,
        }
    }

    /// An impl, trait or inline module: its body's items are read in `inner`.
    fn container(&self, n: Node, inner: Scope, scopes: &mut Vec<Scope>) -> Open {
        scopes.push(inner);
        let body = n
            .child_by_field_name("body")
            .map(|b| (b.id(), b.start_byte(), b.end_byte()))
            .unwrap_or((usize::MAX, 0, 0));
        Open {
            end: n.end_byte(),
            what: Region::Items(body, scopes.len() - 1),
        }
    }

    fn use_declaration(&mut self, n: Node, scope: &Scope) {
        if let Some(arg) = n.child_by_field_name("argument") {
            let mut out = Vec::new();
            use_tree(arg, self.text, &[], &mut out);
            for (mut path, alias, glob) in out {
                fix_super(&mut path, scope.mod_depth);
                self.push_use(path, alias, glob, line(n));
            }
        }
    }

    fn extern_crate(&mut self, n: Node) {
        let (name, nl) = self.name_of(n);
        let local = n
            .child_by_field_name("alias")
            .map(|a| self.text.node_text(a).to_string())
            .unwrap_or_else(|| name.clone());
        self.facts.imports.push(ImportFact {
            local,
            module: name,
            imported: None,
            line: nl,
        });
    }

    /// The impl's symbol and implement ref; the scope of its body (`None` without a self type).
    fn impl_item(&mut self, n: Node, scope: &Scope) -> Option<Scope> {
        let ty = n.child_by_field_name("type")?;
        let tname = type_name(ty, self.text);
        let trait_node = n.child_by_field_name("trait");
        let trait_name = trait_node.map(|t| type_name(t, self.text));
        let q = match &trait_name {
            Some(t) => format!("{}impl {t} for {tname}", scope.prefix),
            None => format!("{}impl {tname}", scope.prefix),
        };
        let id = self.push(SymbolKind::Class, tname.clone(), q, n, scope, line(n));
        if trait_node.is_none() && scope.parent.is_none() {
            self.inherent.push(id);
        }
        if let Some(t) = trait_node {
            let (name, qualifier) = self.type_path(t);
            self.facts.refs.push(RefFact {
                kind: RefKind::Implement,
                name,
                qualifier,
                line: line(t),
                enclosing: Some(id),
            });
        }
        let prefix = match &trait_name {
            Some(t) => format!("{}<{tname} as {t}>::", scope.prefix),
            None => format!("{}{tname}::", scope.prefix),
        };
        Some(Scope {
            parent: Some(id),
            prefix,
            in_type: true,
            self_ty: Some(tname),
            mod_depth: scope.mod_depth,
        })
    }

    fn push_use(&mut self, path: Vec<String>, alias: Option<String>, glob: bool, line: u32) {
        let Some(last) = path.last().cloned() else {
            return;
        };
        if glob {
            self.facts.imports.push(ImportFact {
                local: String::new(),
                module: path.join("::"),
                imported: Some("*".into()),
                line,
            });
        } else if path.len() == 1 {
            // `use serde;`
            self.facts.imports.push(ImportFact {
                local: alias.unwrap_or(last.clone()),
                module: last,
                imported: None,
                line,
            });
        } else {
            self.facts.imports.push(ImportFact {
                local: alias.unwrap_or(last.clone()),
                module: path[..path.len() - 1].join("::"),
                imported: Some(last),
                line,
            });
        }
    }

    /// `a::b::Trait<T>` → ("Trait", Some("a::b")).
    fn type_path(&self, n: Node) -> (String, Option<String>) {
        match n.kind() {
            "generic_type" => n
                .child_by_field_name("type")
                .map(|t| self.type_path(t))
                .unwrap_or_default(),
            "scoped_type_identifier" | "scoped_identifier" => {
                let name = n
                    .child_by_field_name("name")
                    .map(|x| self.text.node_text(x).to_string())
                    .unwrap_or_default();
                let q = n
                    .child_by_field_name("path")
                    .map(|p| self.text.node_text(p).to_string());
                (name, q)
            }
            _ => (type_name(n, self.text), None),
        }
    }

    /// The callee of a call: (name, qualifier).
    fn callee(&mut self, f: Node, scope: &Scope) -> Option<(String, Option<String>)> {
        match f.kind() {
            "identifier" => Some((self.text.node_text(f).to_string(), None)),
            "generic_function" => self.callee(f.child_by_field_name("function")?, scope),
            "scoped_identifier" => {
                let name = self
                    .text
                    .node_text(f.child_by_field_name("name")?)
                    .to_string();
                let Some(path) = f.child_by_field_name("path") else {
                    return Some((name, None));
                };
                let q = self_type(strip_generics(self.text.node_text(path)), scope);
                self.paths.insert(q.clone());
                // `self::f()` is the current module.
                Some((name, (q != "self").then_some(q)))
            }
            "field_expression" => {
                let name = self
                    .text
                    .node_text(f.child_by_field_name("field")?)
                    .to_string();
                let v = f.child_by_field_name("value")?;
                let q = match v.kind() {
                    "self" => "self".to_string(),
                    "identifier" => self.text.node_text(v).to_string(),
                    _ => "<expr>".to_string(),
                };
                Some((name, Some(q)))
            }
            _ => None,
        }
    }

    fn call(&mut self, n: Node, f: Node, enclosing: Option<usize>, scope: &Scope) {
        if let Some((name, qualifier)) = self.callee(f, scope) {
            self.facts.refs.push(RefFact {
                kind: RefKind::Call,
                name,
                qualifier,
                line: line(n),
                enclosing,
            });
        }
    }

    /// A macro call is a call (unless it is a std macro), and so are the calls written inside
    /// its token trees.
    fn macro_call(&mut self, n: Node, enclosing: Option<usize>, scope: &Scope) {
        if let Some(m) = n.child_by_field_name("macro") {
            let name = self.text.node_text(m);
            if m.kind() == "identifier" && !STD_MACROS.contains(&name) {
                self.facts.refs.push(RefFact {
                    kind: RefKind::Call,
                    name: name.to_string(),
                    qualifier: None,
                    line: line(n),
                    enclosing,
                });
            }
        }
        let mut c = n.walk();
        let tts: Vec<Node> = n
            .named_children(&mut c)
            .filter(|k| k.kind() == "token_tree")
            .collect();
        for tt in tts {
            self.token_tree_calls(tt, enclosing, scope);
        }
    }

    /// `f (…)`, `a::b (…)`, `self.f (…)`, `x.f (…)` patterns inside a macro's token tree.
    fn token_tree_calls(&mut self, tt: Node, enclosing: Option<usize>, scope: &Scope) {
        let mut c = tt.walk();
        let kids: Vec<Node> = tt.children(&mut c).collect();
        for (i, k) in kids.iter().enumerate() {
            if k.kind() != "token_tree" {
                continue;
            }
            if i > 0
                && kids[i - 1].kind() == "identifier"
                && self.text.node_text(*k).starts_with('(')
            {
                let name = self.text.node_text(kids[i - 1]).to_string();
                let mut j = i - 1;
                let mut segs: Vec<&str> = Vec::new();
                while j >= 2
                    && kids[j - 1].kind() == "::"
                    && matches!(
                        kids[j - 2].kind(),
                        "identifier" | "self" | "crate" | "super"
                    )
                {
                    segs.push(self.text.node_text(kids[j - 2]));
                    j -= 2;
                }
                let qualifier = if !segs.is_empty() {
                    segs.reverse();
                    let q = self_type(segs.join("::"), scope);
                    self.paths.insert(q.clone());
                    (q != "self").then_some(q)
                } else if j >= 1 && kids[j - 1].kind() == "." {
                    Some(match j.checked_sub(2).map(|x| kids[x]) {
                        Some(p) if p.kind() == "self" => "self".into(),
                        Some(p) if p.kind() == "identifier" => self.text.node_text(p).to_string(),
                        _ => "<expr>".into(),
                    })
                } else {
                    None
                };
                self.facts.refs.push(RefFact {
                    kind: RefKind::Call,
                    name,
                    qualifier,
                    line: line(*k),
                    enclosing,
                });
            }
            self.token_tree_calls(*k, enclosing, scope);
        }
    }
}

/// `Self` / `Self::Assoc` → the impl's type.
fn self_type(q: String, scope: &Scope) -> String {
    match (&scope.self_ty, q.as_str()) {
        (Some(t), "Self") => t.clone(),
        (Some(t), _) if q.starts_with("Self::") => format!("{t}{}", &q[4..]),
        _ => q,
    }
}

/// Macros from std and core: never linked as calls.
const STD_MACROS: &[&str] = &[
    "println",
    "print",
    "eprintln",
    "eprint",
    "format",
    "format_args",
    "write",
    "writeln",
    "panic",
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "vec",
    "concat",
    "stringify",
    "include_str",
    "include_bytes",
    "include",
    "env",
    "option_env",
    "matches",
    "todo",
    "unimplemented",
    "unreachable",
    "cfg",
    "line",
    "column",
    "file",
    "module_path",
    "dbg",
    "compile_error",
    "thread_local",
    "macro_rules",
];

fn strip_generics(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for ch in s.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth -= 1,
            c if depth == 0 && !c.is_whitespace() => out.push(c),
            _ => {}
        }
    }
    out.trim_end_matches("::").to_string()
}

type UseEntry = (Vec<String>, Option<String>, bool);

/// Flattens a use tree into (path, alias, glob) entries.
fn use_tree(n: Node, text: &Text, prefix: &[String], out: &mut Vec<UseEntry>) {
    let path_of = |n: Node| -> Vec<String> {
        text.node_text(n)
            .split("::")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };
    let join = |rest: Vec<String>| -> Vec<String> {
        let mut p = prefix.to_vec();
        p.extend(rest);
        p
    };
    match n.kind() {
        "identifier" | "scoped_identifier" | "crate" | "super" | "metavariable" => {
            out.push((join(path_of(n)), None, false))
        }
        "self" => {
            // `{self}` inside a group: the module itself.
            let p = if prefix.is_empty() {
                vec!["self".into()]
            } else {
                prefix.to_vec()
            };
            out.push((p, None, false));
        }
        "use_as_clause" => {
            let path = n
                .child_by_field_name("path")
                .map(path_of)
                .unwrap_or_default();
            let alias = n
                .child_by_field_name("alias")
                .map(|a| text.node_text(a).to_string());
            let mut p = join(path);
            // `use x::{self as y}`
            if p.len() > 1 && p.last().is_some_and(|s| s == "self") {
                p.pop();
            }
            out.push((p, alias.filter(|a| a != "_"), false));
        }
        "use_wildcard" => {
            let mut c = n.walk();
            let path = n
                .named_children(&mut c)
                .next()
                .map(path_of)
                .unwrap_or_default();
            out.push((join(path), None, true));
        }
        "scoped_use_list" => {
            let p = join(
                n.child_by_field_name("path")
                    .map(path_of)
                    .unwrap_or_default(),
            );
            if let Some(list) = n.child_by_field_name("list") {
                use_tree(list, text, &p, out);
            }
        }
        "use_list" => {
            let mut c = n.walk();
            for k in n.named_children(&mut c) {
                use_tree(k, text, prefix, out);
            }
        }
        _ => {}
    }
}

/// Inside `mod tests { use super::*; }` the first `super` is the file's own module.
fn fix_super(path: &mut [String], mod_depth: usize) {
    for seg in path.iter_mut().take(mod_depth) {
        if seg != "super" {
            break;
        }
        *seg = "self".into();
    }
}

/// Adds an implicit import for every module path a qualified call goes through (see crate docs).
fn add_path_imports(facts: &mut BlobFacts, paths: &HashSet<String>) {
    let bound: HashMap<String, (String, Option<String>)> = facts
        .imports
        .iter()
        .filter(|i| !i.local.is_empty())
        .map(|i| (i.local.clone(), (i.module.clone(), i.imported.clone())))
        .collect();
    let top_names: HashSet<&str> = facts
        .symbols
        .iter()
        .filter(|s| s.parent.is_none())
        .map(|s| s.name.as_str())
        .collect();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut extra = Vec::new();
    for r in &facts.refs {
        let Some(q) = r.qualifier.as_deref() else {
            continue;
        };
        if !paths.contains(q) || !seen.insert(q) {
            continue;
        }
        let segs: Vec<&str> = q.split("::").collect();
        if segs.len() == 1
            && (bound.contains_key(q)
                || top_names.contains(q)
                || is_camel(q)
                || !matches!(r.kind, RefKind::Call)
                || !r.name.chars().next().is_some_and(|c| c.is_lowercase()))
        {
            // A local item, an import, a CamelCase (std) type or a receiver variable: nothing to
            // add. Lowercase single segments that name nothing local may be crates.
            continue;
        }
        // Expand a first segment bound by `use` to its full path.
        let mut full: Vec<String> = match bound.get(segs[0]) {
            Some((m, Some(imp))) if imp != "*" => m
                .split("::")
                .map(String::from)
                .chain([imp.clone()])
                .collect(),
            Some((m, None)) => m.split("::").map(String::from).collect(),
            _ => vec![segs[0].to_string()],
        };
        full.extend(segs[1..].iter().map(|s| s.to_string()));
        let (module, imported) = match full.last() {
            Some(last) if full.len() > 1 && is_camel(last) => {
                (full[..full.len() - 1].join("::"), Some(last.clone()))
            }
            _ => (full.join("::"), None),
        };
        extra.push(ImportFact {
            local: q.to_string(),
            module,
            imported,
            line: r.line,
        });
    }
    facts.imports.extend(extra);
}

/// Lists top-level inherent impls first (cv-link binds a name to its first top-level symbol), and
/// remaps parents and ref enclosures.
fn reorder_inherent_impls(facts: &mut BlobFacts, inherent: &[usize]) {
    if inherent.is_empty() {
        return;
    }
    let first: HashSet<usize> = inherent.iter().copied().collect();
    let mut order: Vec<usize> = inherent.to_vec();
    order.extend((0..facts.symbols.len()).filter(|i| !first.contains(i)));
    let mut new_of = vec![0usize; order.len()];
    for (new, &old) in order.iter().enumerate() {
        new_of[old] = new;
    }
    let old = std::mem::take(&mut facts.symbols);
    facts.symbols = order
        .iter()
        .map(|&o| {
            let mut s = old[o].clone();
            s.parent = s.parent.map(|p| new_of[p]);
            s
        })
        .collect();
    for r in &mut facts.refs {
        r.enclosing = r.enclosing.map(|e| new_of[e]);
    }
}

/// Window keys come from qualnames; a repeated qualname (two `impl Foo` blocks, cfg-gated twins)
/// gets a stable ordinal suffix in document order.
fn dedup_qualnames(facts: &mut BlobFacts) {
    let mut idx: Vec<usize> = (0..facts.symbols.len()).collect();
    idx.sort_by_key(|&i| (facts.symbols[i].start, i));
    let mut count: HashMap<String, u32> = HashMap::new();
    for i in idx {
        let q = facts.symbols[i].qualname.clone();
        let n = count.entry(q.clone()).or_insert(0);
        *n += 1;
        if *n > 1 {
            facts.symbols[i].qualname = format!("{q}#{n}");
        }
    }
}

// ── Key folding ──────────────────────────────────────────────────────────────

/// A folded key part ([`fold_key`]; channel sites convert these to `cv_lang::keys::KeyPart`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyPart {
    Lit(String),
    /// One unknown segment, e.g. a `{}` hole filled at runtime.
    Segment,
    /// Nothing static: the hint says what the key is built from.
    Unknown(String),
    /// A name the folder cannot see in this file (an imported or other-module constant), as
    /// (module qualifier, name). Only produced for channel sites, which resolve it across files.
    Ref(Option<String>, String),
}

/// Folds the outermost expression that starts at (1-based line, UTF-16 column): string, raw and
/// char literals with escapes, `format!` / `format_args!` (`{}` / `{0}` / `{name}` holes, `{{`),
/// `concat!`, `+` on strings, `String::from` / `.to_string()` / `.to_owned()` / `.into()`, `&x`,
/// and file constants (`const` / `static`). Adjacent literal parts are merged; when nothing literal
/// is left the result is a single `Unknown`.
pub fn fold_key(source: &str, line: u32, col16: u32) -> Option<Vec<KeyPart>> {
    let tree = parse(source)?;
    let text = Text::new(source);
    let row = line.checked_sub(1)? as usize;
    let p = Point {
        row,
        column: byte_col(source, row, col16)?,
    };
    let mut n = tree.root_node().named_descendant_for_point_range(p, p)?;
    while let Some(parent) = n.parent() {
        if parent.start_position() != p
            || matches!(
                parent.kind(),
                "expression_statement" | "let_declaration" | "source_file" | "block" | "arguments"
            )
        {
            break;
        }
        n = parent;
    }
    if n.start_position() != p {
        return None;
    }
    let consts = file_consts(tree.root_node(), &text);
    let mut parts = Vec::new();
    Folder {
        text: &text,
        consts: &consts,
        depth: 0,
        refs: false,
    }
    .fold(n, &mut parts);
    Some(finish(parts, text.node_text(n)))
}

fn byte_col(src: &str, row: usize, col16: u32) -> Option<usize> {
    let start: usize = src.split_inclusive('\n').take(row).map(|l| l.len()).sum();
    let line = src.get(start..)?.split('\n').next()?;
    let mut units = 0u32;
    for (i, ch) in line.char_indices() {
        if units >= col16 {
            return Some(i);
        }
        units += ch.len_utf16() as u32;
    }
    (units >= col16).then_some(line.len())
}

fn finish(parts: Vec<KeyPart>, src: &str) -> Vec<KeyPart> {
    let mut out: Vec<KeyPart> = Vec::new();
    for p in parts {
        match (out.last_mut(), p) {
            (_, KeyPart::Lit(b)) if b.is_empty() => {}
            (Some(KeyPart::Lit(a)), KeyPart::Lit(b)) => a.push_str(&b),
            (_, p) => out.push(p),
        }
    }
    if !out.iter().any(|p| matches!(p, KeyPart::Lit(_))) {
        let hint: String = src.chars().take(60).collect();
        return vec![KeyPart::Unknown(format!("built at runtime: {hint}"))];
    }
    out
}

/// `const NAME: … = <expr>;` and `static` items anywhere in the file, by name.
pub(crate) fn file_consts<'a>(root: Node<'a>, text: &Text) -> HashMap<String, Node<'a>> {
    let mut out = HashMap::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "const_item" | "static_item")
            && let (Some(name), Some(v)) = (
                n.child_by_field_name("name"),
                n.child_by_field_name("value"),
            )
        {
            out.entry(text.node_text(name).to_string()).or_insert(v);
        }
        let mut c = n.walk();
        stack.extend(n.named_children(&mut c));
    }
    out
}

pub(crate) struct Folder<'t, 'a, 'c> {
    pub(crate) text: &'t Text<'a>,
    pub(crate) consts: &'c HashMap<String, Node<'c>>,
    pub(crate) depth: u32,
    /// Names that are not file constants become [`KeyPart::Ref`]s instead of holes.
    pub(crate) refs: bool,
}

impl Folder<'_, '_, '_> {
    fn fold(&mut self, n: Node, out: &mut Vec<KeyPart>) {
        if self.depth > 16 {
            out.push(KeyPart::Segment);
            return;
        }
        self.depth += 1;
        self.fold_inner(n, out);
        self.depth -= 1;
    }

    fn fold_inner(&mut self, n: Node, out: &mut Vec<KeyPart>) {
        let t = self.text.node_text(n);
        match n.kind() {
            "string_literal" => out.push(KeyPart::Lit(unescape(string_body(t)))),
            "raw_string_literal" => {
                let mut c = n.walk();
                let body = n
                    .named_children(&mut c)
                    .find(|k| k.kind() == "string_content")
                    .map(|k| self.text.node_text(k))
                    .unwrap_or("");
                out.push(KeyPart::Lit(body.to_string()));
            }
            "char_literal" => out.push(KeyPart::Lit(unescape(
                t.trim_start_matches('b').trim_matches('\''),
            ))),
            "integer_literal" | "float_literal" | "boolean_literal" => {
                out.push(KeyPart::Lit(t.to_string()))
            }
            "parenthesized_expression" | "reference_expression" => {
                let mut c = n.walk();
                match n.named_children(&mut c).last() {
                    Some(inner) => self.fold(inner, out),
                    None => out.push(KeyPart::Segment),
                }
            }
            "binary_expression" => {
                let op = n
                    .child_by_field_name("operator")
                    .map(|o| self.text.node_text(o));
                match (
                    op,
                    n.child_by_field_name("left"),
                    n.child_by_field_name("right"),
                ) {
                    (Some("+"), Some(l), Some(r)) => {
                        self.fold(l, out);
                        self.fold(r, out);
                    }
                    _ => out.push(KeyPart::Segment),
                }
            }
            "identifier" | "scoped_identifier" => {
                let name = t.rsplit("::").next().unwrap_or(t);
                match self.consts.get(name) {
                    Some(&v) => self.fold(v, out),
                    None if self.refs => {
                        // `paths::API` → qualifier `paths` (the module as written last).
                        let q = t
                            .rsplit_once("::")
                            .map(|(m, _)| m.rsplit("::").next().unwrap_or(m).to_string())
                            .filter(|m| !matches!(m.as_str(), "self" | "crate" | "super"));
                        out.push(KeyPart::Ref(q, name.to_string()))
                    }
                    None => out.push(KeyPart::Segment),
                }
            }
            "call_expression" => self.fold_call(n, out),
            "macro_invocation" => {
                let name = n
                    .child_by_field_name("macro")
                    .map(|m| self.text.node_text(m))
                    .unwrap_or("");
                let name = name.rsplit("::").next().unwrap_or(name);
                let mut c = n.walk();
                let tt = n.named_children(&mut c).find(|k| k.kind() == "token_tree");
                let args = tt.map(macro_args).unwrap_or_default();
                match name {
                    "concat" => {
                        for a in args {
                            self.fold_tokens(&a, out);
                        }
                    }
                    "format" | "format_args" => self.fold_format(&args, out),
                    _ => out.push(KeyPart::Segment),
                }
            }
            _ => out.push(KeyPart::Segment),
        }
    }

    fn fold_call(&mut self, n: Node, out: &mut Vec<KeyPart>) {
        let f = n.child_by_field_name("function");
        let args: Vec<Node> = n
            .child_by_field_name("arguments")
            .map(|a| {
                let mut c = a.walk();
                a.named_children(&mut c).collect()
            })
            .unwrap_or_default();
        let method = f
            .filter(|f| f.kind() == "field_expression")
            .and_then(|f| f.child_by_field_name("field"))
            .map(|x| self.text.node_text(x));
        let ftext = f.map(|f| self.text.node_text(f)).unwrap_or("");
        if matches!(
            method,
            Some("to_string" | "to_owned" | "into" | "as_str" | "clone")
        ) && args.is_empty()
        {
            match f.and_then(|f| f.child_by_field_name("value")) {
                Some(v) => self.fold(v, out),
                None => out.push(KeyPart::Segment),
            }
        } else if matches!(ftext, "String::from" | "Cow::from" | "Cow::Borrowed") && args.len() == 1
        {
            self.fold(args[0], out);
        } else {
            out.push(KeyPart::Segment);
        }
    }

    /// One macro argument: a single literal / identifier / path folds; anything else is a hole.
    fn fold_tokens(&mut self, toks: &[Node], out: &mut Vec<KeyPart>) {
        let toks: Vec<Node> = toks.iter().copied().filter(|t| t.kind() != "&").collect();
        match toks.as_slice() {
            [one] => self.fold(*one, out),
            // `a::B` path inside a token tree: fold by its last identifier.
            [.., p, last] if p.kind() == "::" && last.kind() == "identifier" => {
                self.fold(*last, out)
            }
            _ => out.push(KeyPart::Segment),
        }
    }

    fn fold_format(&mut self, args: &[Vec<Node>], out: &mut Vec<KeyPart>) {
        let fmt = match args.first().map(|a| a.as_slice()) {
            Some([s]) if matches!(s.kind(), "string_literal" | "raw_string_literal") => {
                let mut v = Vec::new();
                self.fold(*s, &mut v);
                match v.as_slice() {
                    [KeyPart::Lit(x)] => x.clone(),
                    _ => String::new(),
                }
            }
            _ => {
                out.push(KeyPart::Segment);
                return;
            }
        };
        let mut positional: Vec<&[Node]> = Vec::new();
        let mut named: HashMap<&str, &[Node]> = HashMap::new();
        for a in &args[1..] {
            if a.len() >= 3 && a[0].kind() == "identifier" && a[1].kind() == "=" {
                named.insert(self.text.node_text(a[0]), &a[2..]);
            } else {
                positional.push(a.as_slice());
            }
        }
        let mut next_pos = 0usize;
        let mut lit = String::new();
        let mut chars = fmt.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    lit.push('{');
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    lit.push('}');
                }
                '{' => {
                    let spec: String = chars.by_ref().take_while(|&c| c != '}').collect();
                    let arg = spec.split(':').next().unwrap_or("").trim().to_string();
                    let toks: Option<&[Node]> = if arg.is_empty() {
                        next_pos += 1;
                        positional.get(next_pos - 1).copied()
                    } else if let Ok(i) = arg.parse::<usize>() {
                        positional.get(i).copied()
                    } else {
                        named.get(arg.as_str()).copied()
                    };
                    out.push(KeyPart::Lit(std::mem::take(&mut lit)));
                    match toks {
                        Some(t) => self.fold_tokens(t, out),
                        // Inline capture `{name}`: a constant folds, a variable is a hole.
                        None => match self.consts.get(arg.as_str()) {
                            Some(&v) => self.fold(v, out),
                            None => out.push(KeyPart::Segment),
                        },
                    }
                }
                c => lit.push(c),
            }
        }
        out.push(KeyPart::Lit(lit));
    }
}

/// Splits a macro token tree into comma-separated argument token lists.
fn macro_args(tt: Node) -> Vec<Vec<Node>> {
    let mut c = tt.walk();
    let kids: Vec<Node> = tt.children(&mut c).collect();
    let inner = if kids.len() >= 2 {
        &kids[1..kids.len() - 1]
    } else {
        &kids[..]
    };
    let mut out: Vec<Vec<Node>> = vec![Vec::new()];
    for k in inner {
        if k.kind() == "," {
            out.push(Vec::new());
        } else if let Some(last) = out.last_mut() {
            last.push(*k);
        }
    }
    if out.last().is_some_and(|v| v.is_empty()) {
        out.pop();
    }
    out
}

pub(crate) fn string_body(t: &str) -> &str {
    let t = t.strip_prefix('b').unwrap_or(t);
    t.strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(t)
}

pub(crate) fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            Some('\n') => {
                // Line continuation: leading whitespace on the next line is dropped.
                while it.peek().is_some_and(|c| c.is_whitespace()) {
                    it.next();
                }
            }
            Some('x') => {
                let h: String = it.by_ref().take(2).collect();
                if let Some(ch) = u32::from_str_radix(&h, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            Some('u') => {
                if it.peek() == Some(&'{') {
                    it.next();
                }
                let h: String = it.by_ref().take_while(|&c| c != '}').collect();
                if let Some(ch) = u32::from_str_radix(&h, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            Some(o) => {
                out.push('\\');
                out.push(o);
            }
            None => out.push('\\'),
        }
    }
    out
}

// ── Import resolution ────────────────────────────────────────────────────────

/// What a module file defines at top level and what it re-exports.
#[derive(Default)]
struct ModInfo {
    defines: HashSet<String>,
    /// `pub use` entries: (local, module, imported); `imported` is `*` for globs.
    reexports: Vec<(String, String, String)>,
}

fn mod_info(source: &str) -> ModInfo {
    let mut info = ModInfo::default();
    let Some(tree) = parse(source) else {
        return info;
    };
    let text = Text::new(source);
    let root = tree.root_node();
    let mut c = root.walk();
    for n in root.named_children(&mut c) {
        let mut cc = n.walk();
        let public = n
            .children(&mut cc)
            .any(|k| k.kind() == "visibility_modifier");
        if n.kind() == "use_declaration" {
            if !public {
                continue;
            }
            let Some(arg) = n.child_by_field_name("argument") else {
                continue;
            };
            let mut out = Vec::new();
            use_tree(arg, &text, &[], &mut out);
            for (path, alias, glob) in out {
                let Some(last) = path.last().cloned() else {
                    continue;
                };
                if glob {
                    info.reexports
                        .push((String::new(), path.join("::"), "*".into()));
                } else {
                    let module = path[..path.len() - 1].join("::");
                    info.reexports
                        .push((alias.unwrap_or(last.clone()), module, last));
                }
            }
        } else if let Some(name) = n.child_by_field_name("name") {
            info.defines.insert(text.node_text(name).to_string());
        }
    }
    info
}

/// Minimal Cargo.toml reading: `[package] name`, `[lib] path`.
fn manifest(text: &str) -> (Option<String>, Option<String>) {
    let mut section = String::new();
    let (mut name, mut lib) = (None, None);
    for l in text.lines() {
        let l = l.trim();
        if l.starts_with('[') {
            section = l.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            continue;
        }
        let Some((k, v)) = l.split_once('=') else {
            continue;
        };
        let v = v.trim().trim_matches('"').to_string();
        match (section.as_str(), k.trim()) {
            ("package", "name") => name = Some(v),
            ("lib", "path") => lib = Some(v),
            _ => {}
        }
    }
    (name, lib)
}

fn join(dir: &str, rel: &str) -> String {
    if dir.is_empty() {
        rel.to_string()
    } else {
        format!("{dir}/{rel}")
    }
}

struct Resolver<'p> {
    pack: &'p RustPack,
    project: &'p dyn ProjectIndex,
}

impl Resolver<'_> {
    fn lock(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.pack.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Crate name (with `_`) → crate root file, from every `Cargo.toml` with a `[package]`.
    fn crates(&self) -> Arc<HashMap<String, String>> {
        let texts: Vec<(&String, String)> = self
            .project
            .files_named("Cargo.toml")
            .iter()
            .filter_map(|p| self.project.read(p).map(|t| (p, t)))
            .collect();
        let key = texts.iter().fold(1u64, |h, (p, t)| {
            h.rotate_left(7) ^ hash(p) ^ hash(t).rotate_left(3)
        });
        {
            let cache = self.lock();
            if cache.crates_key == key {
                return cache.crates.clone();
            }
        }
        let mut map = HashMap::new();
        for (p, t) in &texts {
            let (Some(name), lib) = manifest(t) else {
                continue;
            };
            let dir = dirname(p);
            let root = [
                lib.and_then(|l| normalize_path(&join(dir, &l))),
                Some(join(dir, "src/lib.rs")),
                Some(join(dir, "src/main.rs")),
            ]
            .into_iter()
            .flatten()
            .find(|r| self.project.exists(r));
            if let Some(root) = root {
                map.insert(name.replace('-', "_"), root);
            }
        }
        let map = Arc::new(map);
        let mut cache = self.lock();
        cache.crates_key = key;
        cache.crates = map.clone();
        map
    }

    fn info(&self, path: &str) -> Arc<ModInfo> {
        let Some(text) = self.project.read(path) else {
            return Arc::new(ModInfo::default());
        };
        let h = hash(&text);
        if let Some((ch, info)) = self.lock().modules.get(path)
            && *ch == h
        {
            return info.clone();
        }
        let info = Arc::new(mod_info(&text));
        self.lock()
            .modules
            .insert(path.to_string(), (h, info.clone()));
        info
    }

    /// The crate root of the crate target a file belongs to.
    fn crate_root(&self, from: &str) -> Option<String> {
        // The deepest directory with a Cargo.toml that contains `from`.
        let pkg = self
            .project
            .files_named("Cargo.toml")
            .iter()
            .map(|m| dirname(m))
            .filter(|d| d.is_empty() || from.starts_with(&format!("{d}/")))
            .max_by_key(|d| d.len())
            .unwrap_or("");
        let rel = if pkg.is_empty() {
            from
        } else {
            &from[pkg.len() + 1..]
        };
        let parts: Vec<&str> = rel.split('/').collect();
        // Separate targets: src/bin/x.rs, tests/x.rs, examples/x.rs, benches/x.rs (or x/main.rs).
        let target_depth = match parts.as_slice() {
            ["src", "bin", ..] => Some(2),
            ["tests" | "examples" | "benches", ..] => Some(1),
            _ => None,
        };
        if let Some(k) = target_depth {
            if parts.len() == k + 1 {
                return Some(from.to_string());
            }
            let main = join(
                pkg,
                &format!("{}/{}/main.rs", parts[..k].join("/"), parts[k]),
            );
            if self.project.exists(&main) {
                return Some(main);
            }
        }
        if rel == "src/main.rs" {
            return Some(from.to_string());
        }
        let lib = self
            .project
            .read(&join(pkg, "Cargo.toml"))
            .and_then(|t| manifest(&t).1);
        [
            lib.and_then(|l| normalize_path(&join(pkg, &l))),
            Some(join(pkg, "src/lib.rs")),
            Some(join(pkg, "src/main.rs")),
        ]
        .into_iter()
        .flatten()
        .find(|p| self.project.exists(p))
    }

    /// A child module file of a module file (`name.rs` or `name/mod.rs` in its module directory).
    fn child(&self, file: &str, root: &str, name: &str) -> Option<String> {
        let base = file.rsplit('/').next().unwrap_or(file);
        let dir = if file == root || matches!(base, "mod.rs" | "lib.rs" | "main.rs") {
            dirname(file).to_string()
        } else {
            join(dirname(file), base.trim_end_matches(".rs"))
        };
        [
            join(&dir, &format!("{name}.rs")),
            join(&dir, &format!("{name}/mod.rs")),
        ]
        .into_iter()
        .find(|p| self.project.exists(p))
    }

    fn parent(&self, file: &str, root: &str) -> Option<String> {
        if file == root {
            return None;
        }
        let base = file.rsplit('/').next().unwrap_or(file);
        // The directory this module sits in as an entry.
        let entry_dir = if base == "mod.rs" {
            dirname(dirname(file))
        } else {
            dirname(file)
        };
        if entry_dir == dirname(root) {
            return Some(root.to_string());
        }
        let name = entry_dir.rsplit('/').next().unwrap_or(entry_dir);
        [
            join(dirname(entry_dir), &format!("{name}.rs")),
            join(entry_dir, "mod.rs"),
        ]
        .into_iter()
        .find(|p| self.project.exists(p))
    }

    /// Walks a module path as far as files go: (file, segments consumed).
    fn walk(&self, from: &str, segs: &[&str]) -> Option<(String, usize)> {
        let root = self.crate_root(from)?;
        let (mut cur, mut cur_root, mut i) = match *segs.first()? {
            "crate" => (root.clone(), root.clone(), 1),
            "self" => (from.to_string(), root.clone(), 1),
            "super" => {
                let mut cur = from.to_string();
                let mut i = 0;
                while segs.get(i) == Some(&"super") {
                    cur = self.parent(&cur, &root)?;
                    i += 1;
                }
                (cur, root.clone(), i)
            }
            name => {
                // 2018 paths: a child of the current module, then a crate, then (2015) the root.
                if let Some(c) = self.child(from, &root, name) {
                    (c, root.clone(), 1)
                } else if let Some(c) = self.crates().get(name) {
                    (c.clone(), c.clone(), 1)
                } else {
                    (self.child(&root, &root, name)?, root.clone(), 1)
                }
            }
        };
        while i < segs.len() {
            let Some(c) = self.child(&cur, &cur_root, segs[i]) else {
                break;
            };
            cur = c;
            cur_root.clear();
            i += 1;
        }
        Some((cur, i))
    }

    fn resolve(&self, from: &str, import: &ImportFact) -> Option<String> {
        let mut segs: Vec<&str> = import
            .module
            .split("::")
            .filter(|s| !s.is_empty())
            .collect();
        if let Some(it) = import.imported.as_deref().filter(|n| *n != "*") {
            segs.push(it);
        }
        let (file, used) = self.walk(from, &segs)?;
        if used == segs.len() {
            return Some(file);
        }
        // The rest names an item in `file`; follow `pub use` re-exports to its definition.
        Some(self.follow(&file, segs[used], 0).unwrap_or(file))
    }

    fn follow(&self, file: &str, name: &str, depth: u32) -> Option<String> {
        if depth > 4 {
            return None;
        }
        let info = self.info(file);
        if info.defines.contains(name) {
            return Some(file.to_string());
        }
        for (local, module, imported) in &info.reexports {
            let glob = imported == "*";
            if !glob && local != name {
                continue;
            }
            let mut segs: Vec<&str> = module.split("::").filter(|s| !s.is_empty()).collect();
            segs.push(if glob { name } else { imported });
            let Some((f, used)) = self.walk(file, &segs) else {
                continue;
            };
            if used == segs.len() {
                return Some(f);
            }
            if let Some(found) = self.follow(&f, segs[used], depth + 1) {
                return Some(found);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests;
