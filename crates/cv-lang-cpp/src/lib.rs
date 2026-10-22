//! L5 C++ pack: tree-sitter-cpp → symbols (namespaces, classes, structs, unions and enums as
//! containers; functions, methods, out-of-line definitions, gtest/Catch2 tests), `#include`
//! imports, call and inheritance refs, highlight tokens and key folding.
//!
//! **Includes** (D17: heuristics only, never `compile_commands.json`). An include makes every
//! top-level name of the header visible, so each one is a star import (`imported: Some("*")`) and
//! cv-link resolves unqualified calls into included headers at star confidence. The written header
//! is kept as `module`: `a/b.hpp` for `"a/b.hpp"` and `<a/b.hpp>` (brackets kept) for angle
//! includes, so the resolver knows the form. Resolution order:
//! 1. quoted includes only: relative to the including file's directory;
//! 2. conventional include dirs: `include/`, `src/`, the repo root, and `include/` under every
//!    ancestor directory of the including file;
//! 3. `include_directories()` / `target_include_directories()` scraped from every `CMakeLists.txt`
//!    (`${CMAKE_CURRENT_SOURCE_DIR}`, `${CMAKE_CURRENT_LIST_DIR}`, `${PROJECT_SOURCE_DIR}`,
//!    `${CMAKE_SOURCE_DIR}` and `$<BUILD_INTERFACE:…>` understood; other variables skipped);
//! 4. the unique project file whose path ends with the written path (angle includes only when the
//!    written path has a directory part, so `<string.h>` never lands on a vendored file).
//!
//! **Out-of-line methods** (`void Cls::f() {…}`) are top-level `Method` symbols named `f` with
//! qualname `Cls::f` (prefixed by the enclosing namespaces). Inside them `this->g()` is reported
//! with qualifier `Cls`, so cv-link resolves it when `Cls` is defined in the same file.

pub mod sites;

use std::collections::HashSet;
use std::sync::Mutex;

use cv_lang::{
    BlobFacts, Conventions, ImportFact, LaneHints, LanguagePack, ProjectIndex, RefFact, RefKind,
    SymbolFact, SymbolKind, dirname, normalize_path, token,
};
use cv_lang_sitter::query::{PackQuery, Tag};
use cv_lang_sitter::{Text, count_errors, end_line, line};
use std::sync::OnceLock;
use tree_sitter::{Node, Parser, Point, Tree};

const EXTS: &[&str] = &[
    ".c", ".cc", ".cpp", ".cxx", ".c++", ".h", ".hh", ".hpp", ".hxx", ".ipp", ".inl",
];

type CmakeCache = Option<(usize, Vec<String>, Vec<String>)>;

/// The C++ pack. It caches the CMake include-directory scrape per project index.
#[derive(Default)]
pub struct CppPack {
    cmake: Mutex<CmakeCache>,
}

impl CppPack {
    pub fn new() -> Self {
        Self::default()
    }

    /// CMake include dirs for this project, cached by the identity of its CMakeLists list.
    fn cmake_dirs(&self, project: &dyn ProjectIndex) -> Vec<String> {
        let lists = project.files_named("CMakeLists.txt");
        if lists.is_empty() {
            return Vec::new();
        }
        let key = lists.as_ptr() as usize;
        let mut g = self.cmake.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((k, paths, dirs)) = g.as_ref()
            && *k == key
            && paths.as_slice() == lists
        {
            return dirs.clone();
        }
        let dirs = scrape_cmake(lists, project);
        *g = Some((key, lists.to_vec(), dirs.clone()));
        dirs
    }
}

/// Builtin and standard-library names lexical candidates never guess (`Conventions`).
const BUILTIN_METHODS: &[&str] = &[
    "push_back",
    "emplace_back",
    "pop_back",
    "push_front",
    "pop_front",
    "size",
    "empty",
    "begin",
    "end",
    "cbegin",
    "cend",
    "rbegin",
    "rend",
    "find",
    "insert",
    "erase",
    "clear",
    "at",
    "front",
    "back",
    "c_str",
    "data",
    "substr",
    "reserve",
    "resize",
    "count",
    "emplace",
    "lock",
    "unlock",
    "reset",
    "get",
    "swap",
    "append",
    "length",
    "capacity",
    "shrink_to_fit",
    "compare",
];
const BUILTIN_RECEIVERS: &[&str] = &["std"];
const BUILTIN_FUNCTIONS: &[&str] = &[
    "printf", "fprintf", "sprintf", "snprintf", "malloc", "free", "memcpy", "memset", "strlen",
    "strcmp", "strcpy", "assert",
];

impl LanguagePack for CppPack {
    fn id(&self) -> &'static str {
        "cpp"
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn claims(&self, path: &str) -> bool {
        let p = path.to_ascii_lowercase();
        EXTS.iter().any(|e| p.ends_with(e))
    }
    fn lang(&self, path: &str) -> &'static str {
        if path.to_ascii_lowercase().ends_with(".c") {
            "c"
        } else {
            "cpp"
        }
    }
    fn conventions(&self) -> Conventions {
        Conventions {
            // `this->f()` outside a known class keeps `this` as its qualifier.
            self_receivers: &["this"],
            module_entry_stems: &[],
            entry_stems: &["main"],
            // Reserved identifiers (`__builtin_*`, `__attribute__`).
            reserved_prefixes: &["__"],
            builtin_methods: BUILTIN_METHODS,
            builtin_receivers: BUILTIN_RECEIVERS,
            builtin_functions: BUILTIN_FUNCTIONS,
        }
    }
    fn lane_hints(&self) -> LaneHints {
        LaneHints {
            code: vec![
                "*.c", "*.cc", "*.cpp", "*.cxx", "*.c++", "*.h", "*.hh", "*.hpp", "*.hxx", "*.ipp",
                "*.inl",
            ],
            test: vec![
                "*_test.cc",
                "*_test.cpp",
                "*_test.cxx",
                "*_tests.cc",
                "*_tests.cpp",
                "*_unittest.cc",
                "*_unittest.cpp",
                "test_*.c",
                "test_*.cc",
                "test_*.cpp",
                "*.test.cpp",
                "*.test.cc",
                "*_spec.cpp",
            ],
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
        Some(Box::new(sites::CppSites::new(queries, transforms)))
    }
    fn resolve_import(
        &self,
        from: &str,
        import: &ImportFact,
        project: &dyn ProjectIndex,
    ) -> Option<String> {
        let cmake = self.cmake_dirs(project);
        resolve_with(from, &import.module, project, &cmake)
    }
}

fn parse(source: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_cpp::LANGUAGE.into())
        .expect("cpp grammar");
    parser.parse(source, None)
}

/// `queries/highlights.scm` and `queries/tags.scm` (DESIGN §4.2), compiled once.
fn queries() -> &'static PackQuery {
    static Q: OnceLock<PackQuery> = OnceLock::new();
    Q.get_or_init(|| {
        PackQuery::new(
            &tree_sitter_cpp::LANGUAGE.into(),
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
    let q = queries();
    let (painter, tags) = q.collect(root, &text);
    let mut w = Walker {
        text: &text,
        facts: &mut facts,
        fn_names: HashSet::new(),
        macro_names: HashSet::new(),
    };
    w.read(&tags);
    // Reading the tags marks function and macro names, which the highlights then colour.
    let (fn_names, macro_names) = (w.fn_names, w.macro_names);
    facts.tokens = painter.paint(&text, q, |n, capture, cls| match capture {
        "name.function" if fn_names.contains(&n.id()) => token::FUNCTION,
        "property.member" if fn_names.contains(&n.id()) => token::FUNCTION,
        "name" if fn_names.contains(&n.id()) => token::FUNCTION,
        "name" if macro_names.contains(&n.id()) => token::CONSTANT,
        _ => cls,
    });
    facts
}

/// gtest-style test macros: `TEST(Suite, Name) { … }` parses as a function definition.
const GTEST: &[&str] = &[
    "TEST",
    "TEST_F",
    "TEST_P",
    "TYPED_TEST",
    "TYPED_TEST_P",
    "BENCHMARK",
];
#[derive(Clone, Default)]
struct Ctx {
    /// Namespace or class whose body we are in (members become its children).
    container: Option<usize>,
    /// Innermost symbol (for refs).
    enclosing: Option<usize>,
    /// Class of an out-of-line method body: `this->f()` is reported with this qualifier.
    this_class: Option<String>,
    /// Enclosing namespaces, e.g. `geo::detail` (namespaces scope names but are not symbols).
    ns: String,
}

struct Walker<'t, 'a, 'f> {
    text: &'t Text<'a>,
    facts: &'f mut BlobFacts,
    /// Node ids of names that are called or declared as functions.
    fn_names: HashSet<usize>,
    /// Node ids of `#define` names.
    macro_names: HashSet<usize>,
}

/// The name leaf of a callee or declarator (`a::b<T>`, `x.f`, `~X`, `operator==`).
fn name_leaf(n: Node) -> Option<Node> {
    match n.kind() {
        "identifier" | "field_identifier" | "destructor_name" | "operator_name" => Some(n),
        "template_function" | "template_method" | "qualified_identifier" => {
            name_leaf(n.child_by_field_name("name")?)
        }
        "field_expression" => name_leaf(n.child_by_field_name("field")?),
        _ => None,
    }
}

/// Segments of a (possibly nested) qualified name and its leaf node.
fn split_qualified<'n>(n: Node<'n>, text: &Text) -> (Vec<String>, Node<'n>) {
    let mut scope = Vec::new();
    let mut cur = n;
    while cur.kind() == "qualified_identifier" {
        if let Some(s) = cur.child_by_field_name("scope") {
            let t = match s.kind() {
                "template_type" => s
                    .child_by_field_name("name")
                    .map(|x| text.node_text(x))
                    .unwrap_or(""),
                _ => text.node_text(s),
            };
            scope.push(t.to_string());
        }
        match cur.child_by_field_name("name") {
            Some(x) => cur = x,
            None => break,
        }
    }
    (scope, cur)
}

/// Text of a name leaf (`template_function` → its name; `~X` and `operator==` kept).
fn leaf_name(n: Node, text: &Text) -> String {
    match n.kind() {
        "template_function" | "template_method" | "template_type" => n
            .child_by_field_name("name")
            .map(|x| text.node_text(x).to_string())
            .unwrap_or_default(),
        _ => text.node_text(n).split_whitespace().collect(),
    }
}

/// Qualifier of a scoped name as reported to cv-link. Namespaces are not symbols (their members
/// are top-level, see [`Ctx::ns`]), so a namespace-looking scope (`geo::util::f`: every segment
/// lowercase-initial) is dropped and the name resolves like an unqualified one: in this file, then
/// through included headers. `std::` and class-looking scopes (`Circle::make`) are kept.
fn qualifier(scope: &[String]) -> Option<String> {
    if scope.is_empty() {
        return None;
    }
    let namespace_like = scope
        .iter()
        .all(|s| s.starts_with(|c: char| c.is_ascii_lowercase() || c == '_'));
    if namespace_like && scope[0] != "std" {
        None
    } else {
        Some(scope.join("::"))
    }
}

/// The `function_declarator` under a declarator (through pointer / reference wrappers).
fn function_declarator(mut d: Node) -> Option<Node> {
    loop {
        match d.kind() {
            "function_declarator" => return Some(d),
            "pointer_declarator" | "reference_declarator" | "parenthesized_declarator" => {
                let mut c = d.walk();
                d = d
                    .named_children(&mut c)
                    .find(|x| x.kind().ends_with("declarator"))?;
            }
            _ => return None,
        }
    }
}

/// Declarations that wrap others: a symbol's lines include the wrappers it sits in.
const WRAPPERS: &[&str] = &[
    "template_declaration",
    "declaration",
    "field_declaration",
    "type_definition",
];

/// Nodes whose lines extend to the wrapper around them.
const WRAPPED: &[&str] = &[
    "class_specifier",
    "struct_specifier",
    "union_specifier",
    "enum_specifier",
    "function_definition",
    "template_declaration",
    "declaration",
];

/// First and last line of a declaration with the wrappers around it (`template <…>`,
/// `typedef struct {…} X;`).
fn outer_lines(n: Node) -> (u32, u32) {
    let (mut s, mut e) = (line(n), end_line(n));
    let mut cur = n;
    while WRAPPED.contains(&cur.kind())
        && let Some(p) = cur.parent()
        && WRAPPERS.contains(&p.kind())
    {
        s = s.min(line(p));
        e = e.max(end_line(p));
        cur = p;
    }
    (s, e)
}

/// A node whose tags are read in a particular way, until its end.
struct Open {
    end: usize,
    /// Byte ranges inside it that are read, in `ctx`; nothing else inside it is.
    read: Vec<(usize, usize)>,
    ctx: Ctx,
}

impl Open {
    fn skip(n: Node) -> Self {
        Open {
            end: n.end_byte(),
            read: Vec::new(),
            ctx: Ctx::default(),
        }
    }
}

impl Walker<'_, '_, '_> {
    fn qual(&self, ctx: &Ctx, name: &str) -> String {
        match ctx.container {
            Some(c) => format!("{}::{name}", self.facts.symbols[c].qualname),
            None if !ctx.ns.is_empty() => format!("{}::{name}", ctx.ns),
            None => name.to_string(),
        }
    }

    /// Symbols are declared at namespace / class scope only, never inside function bodies.
    fn at_decl_scope(ctx: &Ctx) -> bool {
        ctx.enclosing.is_none() || ctx.container == ctx.enclosing
    }

    fn push(
        &mut self,
        kind: SymbolKind,
        name: String,
        qualname: String,
        node: Node,
        ctx: &Ctx,
    ) -> usize {
        let (start, end) = outer_lines(node);
        self.facts.symbols.push(SymbolFact {
            kind,
            name,
            qualname,
            parent: ctx.container,
            start,
            end,
            name_line: line(node),
        });
        self.facts.symbols.len() - 1
    }

    /// Reads the tags in document order. Symbols are declared at namespace or class scope, never
    /// inside function bodies. A namespace and a class are read in their bodies only, a
    /// function definition in its body and constructor initializers, and an include or a `friend`
    /// declaration not at all.
    fn read(&mut self, tags: &[Tag]) {
        let mut open: Vec<Open> = Vec::new();
        let root = Ctx::default();
        for tag in tags {
            let (what, node) = tag.main();
            while open.last().is_some_and(|o| o.end <= node.start_byte()) {
                open.pop();
            }
            let ctx = match open.last() {
                None => root.clone(),
                Some(o) => {
                    let inside = o
                        .read
                        .iter()
                        .any(|&(s, e)| node.start_byte() >= s && node.end_byte() <= e);
                    if !inside {
                        continue;
                    }
                    o.ctx.clone()
                }
            };
            match what {
                // Nothing inside an include (`#include MACRO(…)`) or a friend declaration is read.
                "include" => {
                    self.include(node);
                    open.push(Open::skip(node));
                }
                "friend" => open.push(Open::skip(node)),
                "catch" if Self::at_decl_scope(&ctx) => {
                    let (Some(call), Some(body)) = (
                        tag.get("catch.macro").and_then(|m| m.parent()),
                        tag.get("catch.body"),
                    ) else {
                        continue;
                    };
                    let name = call
                        .child_by_field_name("arguments")
                        .and_then(|a| a.named_child(0))
                        .map(|a| self.text.node_text(a).trim_matches('"').to_string())
                        .unwrap_or_default();
                    let qualname = self.qual(&ctx, &name);
                    self.facts.symbols.push(SymbolFact {
                        kind: SymbolKind::Test,
                        name,
                        qualname,
                        parent: ctx.container,
                        start: line(node),
                        end: end_line(body),
                        name_line: line(node),
                    });
                    let me = self.facts.symbols.len() - 1;
                    // The statement itself is not read; the block is the test's body.
                    open.push(Open {
                        end: body.end_byte(),
                        read: vec![(body.start_byte(), body.end_byte())],
                        ctx: Ctx {
                            container: None,
                            enclosing: Some(me),
                            this_class: None,
                            ns: ctx.ns.clone(),
                        },
                    });
                    open.push(Open::skip(node));
                }
                "declaration" if Self::at_decl_scope(&ctx) => {
                    // A function prototype declares a symbol of its own.
                    if node.kind() != "template_declaration"
                        && let Some(d) = node.child_by_field_name("declarator")
                        && let Some(fd) = function_declarator(d)
                    {
                        self.function(node, fd, &ctx);
                    }
                }
                "definition.namespace" if Self::at_decl_scope(&ctx) => {
                    let name = node
                        .child_by_field_name("name")
                        .map(|n| {
                            self.text
                                .node_text(n)
                                .split_whitespace()
                                .collect::<String>()
                        })
                        .unwrap_or_else(|| "(anonymous)".to_string());
                    let ns = self.qual(&ctx, &name);
                    let body = node.child_by_field_name("body");
                    open.push(Open {
                        end: node.end_byte(),
                        read: body
                            .map(|b| (b.start_byte(), b.end_byte()))
                            .into_iter()
                            .collect(),
                        ctx: Ctx { ns, ..ctx },
                    });
                }
                "definition.class" if Self::at_decl_scope(&ctx) => {
                    let Some(body) = node.child_by_field_name("body") else {
                        continue;
                    };
                    let inner = self.class(node, &ctx);
                    open.push(Open {
                        end: node.end_byte(),
                        read: vec![(body.start_byte(), body.end_byte())],
                        ctx: inner,
                    });
                }
                "definition.function" => {
                    let Some(fd) = node
                        .child_by_field_name("declarator")
                        .and_then(function_declarator)
                    else {
                        continue;
                    };
                    let Some(inner) = self.function(node, fd, &ctx) else {
                        open.push(Open::skip(node));
                        continue;
                    };
                    // The body, and constructor initializers (they call base constructors and
                    // helpers).
                    let mut read = Vec::new();
                    let mut c = node.walk();
                    for child in node.children(&mut c) {
                        if child.kind() == "field_initializer_list" {
                            read.push((child.start_byte(), child.end_byte()));
                        }
                    }
                    if let Some(b) = node.child_by_field_name("body") {
                        read.push((b.start_byte(), b.end_byte()));
                    }
                    open.push(Open {
                        end: node.end_byte(),
                        read,
                        ctx: inner,
                    });
                }
                "define" => {
                    if let Some(n) = node.child_by_field_name("name") {
                        self.macro_names.insert(n.id());
                    }
                }
                "call" => {
                    let Some(f) = tag.get("callee") else { continue };
                    if let Some(leaf) = name_leaf(f) {
                        self.fn_names.insert(leaf.id());
                    }
                    if let Some((name, qualifier)) = self.callee(f, &ctx) {
                        self.facts.refs.push(RefFact {
                            kind: RefKind::Call,
                            name,
                            qualifier,
                            line: line(node),
                            enclosing: ctx.enclosing,
                        });
                    }
                }
                "new" => {
                    if let Some(t) = node.child_by_field_name("type")
                        && let Some((name, qualifier)) = self.type_name(t)
                    {
                        self.facts.refs.push(RefFact {
                            kind: RefKind::Call,
                            name,
                            qualifier,
                            line: line(node),
                            enclosing: ctx.enclosing,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    fn include(&mut self, node: Node) {
        if let Some(p) = node.child_by_field_name("path") {
            let raw = self.text.node_text(p).trim();
            let module = if p.kind() == "system_lib_string" {
                raw.to_string()
            } else {
                raw.trim_matches('"').to_string()
            };
            self.facts.imports.push(ImportFact {
                local: String::new(),
                module,
                imported: Some("*".into()),
                line: line(node),
            });
        }
    }

    /// A class, struct, union or enum with a body at declaration scope: its symbol (if named) and
    /// base-class refs; the context its body is read in.
    fn class(&mut self, node: Node, ctx: &Ctx) -> Ctx {
        let kind = match node.kind() {
            "class_specifier" => SymbolKind::Class,
            "enum_specifier" => SymbolKind::Enum,
            _ => SymbolKind::Struct,
        };
        let mut me = ctx.enclosing;
        let mut container = ctx.container;
        if let Some(nn) = node.child_by_field_name("name") {
            let (_, leaf) = split_qualified(nn, self.text);
            let name = leaf_name(leaf, self.text);
            let qualname = self.qual(ctx, &name);
            let id = self.push(kind, name, qualname, node, ctx);
            me = Some(id);
            container = Some(id);
        }
        let mut c = node.walk();
        for child in node.children(&mut c) {
            if child.kind() == "base_class_clause" {
                let mut cc = child.walk();
                for b in child.named_children(&mut cc) {
                    if let Some((name, qualifier)) = self.type_name(b) {
                        self.facts.refs.push(RefFact {
                            kind: RefKind::Inherit,
                            name,
                            qualifier,
                            line: line(b),
                            enclosing: me,
                        });
                    }
                }
            }
        }
        Ctx {
            container,
            enclosing: me,
            this_class: None,
            ns: ctx.ns.clone(),
        }
    }

    /// A function definition or prototype: its symbol (at declaration scope); the context its
    /// body is read in, `None` when nothing of it is read.
    fn function(&mut self, node: Node, fd: Node, ctx: &Ctx) -> Option<Ctx> {
        let decl = fd.child_by_field_name("declarator")?;
        if let Some(l) = name_leaf(decl) {
            self.fn_names.insert(l.id());
        }
        let (scope, leaf) = split_qualified(decl, self.text);
        let leaf_text = leaf_name(leaf, self.text);
        let mut me = ctx.enclosing;
        let mut this_class = ctx.this_class.clone();
        if !scope.is_empty() {
            this_class = scope.last().cloned();
        }
        if Self::at_decl_scope(ctx) {
            let in_class = ctx.container.is_some_and(|c| {
                matches!(
                    self.facts.symbols[c].kind,
                    SymbolKind::Class | SymbolKind::Struct
                )
            });
            let (kind, name, qualname) = if scope.is_empty() && GTEST.contains(&leaf_text.as_str())
            {
                // TEST(Suite, Name): the "parameters" are the suite and test names.
                let mut parts = Vec::new();
                if let Some(ps) = fd.child_by_field_name("parameters") {
                    let mut c = ps.walk();
                    for p in ps.named_children(&mut c) {
                        parts.push(self.text.node_text(p).trim().to_string());
                    }
                }
                let name = parts.join(".");
                let q = self.qual(ctx, &name);
                (SymbolKind::Test, name, q)
            } else if !scope.is_empty() {
                let q = format!("{}::{leaf_text}", scope.join("::"));
                (SymbolKind::Method, leaf_text.clone(), self.qual(ctx, &q))
            } else if in_class {
                let q = self.qual(ctx, &leaf_text);
                (SymbolKind::Method, leaf_text.clone(), q)
            } else {
                let q = self.qual(ctx, &leaf_text);
                (SymbolKind::Function, leaf_text.clone(), q)
            };
            if name.is_empty() {
                return None;
            }
            me = Some(self.push(kind, name, qualname, node, ctx));
        }
        Some(Ctx {
            container: None,
            enclosing: me,
            this_class,
            ns: ctx.ns.clone(),
        })
    }

    /// Base classes and `new T` types: `T` → ("T", None), `a::T<U>` → ("T", Some("a")).
    fn type_name(&self, n: Node) -> Option<(String, Option<String>)> {
        match n.kind() {
            "type_identifier" | "identifier" => Some((self.text.node_text(n).to_string(), None)),
            "template_type" => Some((
                n.child_by_field_name("name")
                    .map(|x| self.text.node_text(x).to_string())?,
                None,
            )),
            "qualified_identifier" => {
                let (scope, leaf) = split_qualified(n, self.text);
                let name = leaf_name(leaf, self.text);
                (!name.is_empty()).then(|| (name, qualifier(&scope)))
            }
            _ => None,
        }
    }

    fn callee(&self, f: Node, ctx: &Ctx) -> Option<(String, Option<String>)> {
        match f.kind() {
            "identifier" => Some((self.text.node_text(f).to_string(), None)),
            "template_function" => Some((leaf_name(f, self.text), None)),
            "qualified_identifier" => {
                let (scope, leaf) = split_qualified(f, self.text);
                if !matches!(leaf.kind(), "identifier" | "template_function") {
                    return None;
                }
                let name = leaf_name(leaf, self.text);
                Some((name, qualifier(&scope)))
            }
            "field_expression" => {
                let field = f.child_by_field_name("field")?;
                let name = leaf_name(field, self.text);
                let arg = f.child_by_field_name("argument")?;
                // A complex receiver (`f().g()`, `a[i].g()`) is never reported as a bare name.
                let q = match arg.kind() {
                    "this" => ctx.this_class.clone().unwrap_or_else(|| "this".into()),
                    "identifier" | "field_identifier" => self.text.node_text(arg).to_string(),
                    "field_expression" | "qualified_identifier" => {
                        self.text.node_text(arg).split_whitespace().collect()
                    }
                    _ => return None,
                };
                Some((name, Some(q)))
            }
            _ => None,
        }
    }
}

// ── Import resolution ────────────────────────────────────────────────────────

fn join(dir: &str, rel: &str) -> Option<String> {
    if dir.is_empty() {
        normalize_path(rel)
    } else {
        normalize_path(&format!("{dir}/{rel}"))
    }
}

/// Resolves an include as written (`a/b.h`, or `<a/b.h>` for angle includes) given the scraped
/// CMake include dirs.
pub fn resolve_with(
    from: &str,
    module: &str,
    project: &dyn ProjectIndex,
    cmake_dirs: &[String],
) -> Option<String> {
    let (written, angle) = match module.strip_prefix('<').and_then(|m| m.strip_suffix('>')) {
        Some(m) => (m.trim(), true),
        None => (module.trim(), false),
    };
    if written.is_empty() || written.starts_with('/') {
        return None;
    }
    let found = |dir: &str| join(dir, written).filter(|p| project.exists(p));
    // 1. Relative to the including file.
    if !angle && let Some(p) = found(dirname(from)) {
        return Some(p);
    }
    // 2. Conventional include dirs.
    let mut conventional: Vec<String> = vec!["include".into(), "src".into(), String::new()];
    let mut d = dirname(from);
    while !d.is_empty() {
        conventional.push(format!("{d}/include"));
        d = dirname(d);
    }
    if let Some(p) = conventional.iter().find_map(|dir| found(dir)) {
        return Some(p);
    }
    // 3. CMake include dirs.
    if let Some(p) = cmake_dirs.iter().find_map(|dir| found(dir)) {
        return Some(p);
    }
    // 4. A unique file with the same trailing path.
    if angle && !written.contains('/') {
        return None;
    }
    let tail = normalize_path(written)?;
    let name = tail.rsplit('/').next()?;
    let suffix = format!("/{tail}");
    let mut hits = project
        .files_named(name)
        .iter()
        .filter(|p| p.ends_with(&suffix) || **p == tail);
    let first = hits.next()?;
    hits.next().is_none().then(|| first.clone())
}

/// Include dirs from every CMakeLists.txt, as repo paths, in list order.
pub fn scrape_cmake(lists: &[String], project: &dyn ProjectIndex) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // The shallowest CMakeLists is the project (source) root.
    let root = lists
        .iter()
        .min_by_key(|p| (p.matches('/').count(), (*p).clone()))
        .map(|p| dirname(p).to_string())
        .unwrap_or_default();
    for list in lists {
        let Some(text) = project.read(list) else {
            continue;
        };
        let here = dirname(list);
        for d in cmake_include_args(&text) {
            let d = d
                .replace("${CMAKE_CURRENT_SOURCE_DIR}", "\u{1}")
                .replace("${CMAKE_CURRENT_LIST_DIR}", "\u{1}")
                .replace("${PROJECT_SOURCE_DIR}", "\u{2}")
                .replace("${CMAKE_SOURCE_DIR}", "\u{2}");
            if d.contains('$') || d.starts_with('/') {
                continue;
            }
            let (base, rest) = if let Some(r) = d.strip_prefix('\u{1}') {
                (here, r)
            } else if let Some(r) = d.strip_prefix('\u{2}') {
                (root.as_str(), r)
            } else {
                (here, d.as_str())
            };
            let rest = rest.trim_start_matches('/');
            let p = if rest.is_empty() {
                Some(base.to_string())
            } else {
                join(base, rest)
            };
            if let Some(p) = p
                && !out.contains(&p)
            {
                out.push(p);
            }
        }
    }
    out
}

/// Directory arguments of `include_directories(...)` and `target_include_directories(...)`.
fn cmake_include_args(text: &str) -> Vec<String> {
    let clean: String = text
        .lines()
        .map(|l| match l.find('#') {
            Some(i) if !l[..i].contains('"') => &l[..i],
            _ => l,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let lower = clean.to_ascii_lowercase();
    let mut out = Vec::new();
    for (cmd, skip_target) in [
        ("target_include_directories", true),
        ("include_directories", false),
    ] {
        let mut from = 0;
        while let Some(i) = lower[from..].find(cmd) {
            let at = from + i;
            from = at + cmd.len();
            // `include_directories` also matches inside `target_include_directories`.
            if lower[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                continue;
            }
            if !lower[from..].trim_start().starts_with('(') {
                continue;
            }
            let open = from + lower[from..].find('(').unwrap_or(0) + 1;
            let Some(len) = clean[open..].find(')') else {
                continue;
            };
            let mut toks: Vec<String> = clean[open..open + len]
                .split_whitespace()
                .map(|t| t.trim_matches('"').to_string())
                .collect();
            if skip_target && !toks.is_empty() {
                toks.remove(0);
            }
            for t in toks {
                if matches!(
                    t.to_ascii_uppercase().as_str(),
                    "SYSTEM" | "BEFORE" | "AFTER" | "PUBLIC" | "PRIVATE" | "INTERFACE"
                ) {
                    continue;
                }
                let t = match t.strip_prefix("$<") {
                    Some(inner) => match inner.split_once(':') {
                        Some(("BUILD_INTERFACE", rest)) => rest.trim_end_matches('>').to_string(),
                        _ => continue,
                    },
                    None => t,
                };
                if !t.is_empty() {
                    out.push(t);
                }
            }
        }
    }
    out
}

// ── Key folding ──────────────────────────────────────────────────────────────

/// A folded key: literal parts and holes (pack-local until a shared `KeyExpr` lands in cv-lang).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyPart {
    Lit(String),
    /// One unknown segment (a variable operand).
    Segment,
    /// Nothing literal: a runtime-built key, with a hint.
    Unknown(String),
}

/// Folds the outermost expression starting at (1-based line, UTF-16 column): string literals with
/// escapes, adjacent-literal concatenation, raw strings, `u8`/`L`/`u`/`U` prefixes, `+` on
/// strings, `std::string(..)` / `std::string_view(..)`, `"…"s` literals and file-local constants
/// (`const`/`constexpr` declarations and `#define NAME "…"`). Any other operand is a `Segment`;
/// an expression with no literal part folds to `[Unknown(hint)]`. Adjacent literals are merged.
pub fn fold_key(source: &str, line: u32, col16: u32) -> Option<Vec<KeyPart>> {
    let tree = parse(source)?;
    let root = tree.root_node();
    let row = line.checked_sub(1)? as usize;
    let line_text = source.split('\n').nth(row)?;
    let mut units = 0u32;
    let mut byte_col = line_text.len();
    for (b, ch) in line_text.char_indices() {
        if units >= col16 {
            byte_col = b;
            break;
        }
        units += ch.len_utf16() as u32;
    }
    let p = Point::new(row, byte_col);
    let mut n = root.named_descendant_for_point_range(p, p)?;
    if n.start_position() == p {
        while let Some(par) = n.parent()
            && par.start_position() == p
            && is_expr(par.kind())
        {
            n = par;
        }
    }
    while !is_expr(n.kind()) {
        n = n.parent()?;
    }
    // Inside a literal: the key is the whole run of adjacent literals.
    while let Some(par) = n.parent()
        && matches!(par.kind(), "concatenated_string" | "user_defined_literal")
    {
        n = par;
    }
    let folder = Folder { root, src: source };
    let mut parts = Vec::new();
    folder.fold(n, &mut parts, 0);
    Some(finish(parts, folder.text(n)))
}

fn is_expr(k: &str) -> bool {
    matches!(
        k,
        "string_literal"
            | "concatenated_string"
            | "raw_string_literal"
            | "char_literal"
            | "binary_expression"
            | "call_expression"
            | "identifier"
            | "qualified_identifier"
            | "parenthesized_expression"
            | "field_expression"
            | "user_defined_literal"
            | "conditional_expression"
            | "subscript_expression"
            | "number_literal"
            | "template_function"
    )
}

fn finish(parts: Vec<KeyPart>, text: &str) -> Vec<KeyPart> {
    let mut out: Vec<KeyPart> = Vec::new();
    for p in parts {
        match (out.last_mut(), p) {
            (Some(KeyPart::Lit(a)), KeyPart::Lit(b)) => a.push_str(&b),
            (_, p) => out.push(p),
        }
    }
    if !out.iter().any(|p| matches!(p, KeyPart::Lit(_))) {
        let hint: String = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(60)
            .collect();
        return vec![KeyPart::Unknown(hint)];
    }
    out
}

struct Folder<'t, 'a> {
    root: Node<'t>,
    src: &'a str,
}

impl<'t> Folder<'t, '_> {
    fn text(&self, n: Node) -> &str {
        &self.src[n.byte_range()]
    }

    fn fold(&self, n: Node<'t>, out: &mut Vec<KeyPart>, depth: u32) {
        if depth > 16 {
            out.push(KeyPart::Segment);
            return;
        }
        match n.kind() {
            "string_literal" => out.push(KeyPart::Lit(self.string(n))),
            "raw_string_literal" => {
                let mut c = n.walk();
                let content = n
                    .children(&mut c)
                    .find(|x| x.kind() == "raw_string_content")
                    .map(|x| self.text(x).to_string())
                    .unwrap_or_default();
                out.push(KeyPart::Lit(content));
            }
            "char_literal" => {
                let t = self.text(n);
                let inner = t
                    .trim_start_matches(['u', 'U', 'L', '8'])
                    .trim_matches('\'');
                out.push(KeyPart::Lit(unescape(inner)));
            }
            "concatenated_string" | "parenthesized_expression" => {
                let mut c = n.walk();
                let kids: Vec<Node> = n.named_children(&mut c).collect();
                for x in kids {
                    self.fold(x, out, depth + 1);
                }
            }
            "user_defined_literal" => match n.named_child(0) {
                Some(s) => self.fold(s, out, depth + 1),
                None => out.push(KeyPart::Segment),
            },
            "binary_expression" => {
                let op = n.child_by_field_name("operator").map(|o| self.text(o));
                match (
                    op,
                    n.child_by_field_name("left"),
                    n.child_by_field_name("right"),
                ) {
                    (Some("+"), Some(l), Some(r)) => {
                        self.fold(l, out, depth + 1);
                        self.fold(r, out, depth + 1);
                    }
                    _ => out.push(KeyPart::Segment),
                }
            }
            "call_expression" => {
                let f = n
                    .child_by_field_name("function")
                    .map(|f| self.text(f))
                    .unwrap_or("");
                let ctor = matches!(
                    f.trim_start_matches("std::"),
                    "string" | "string_view" | "wstring" | "u8string"
                );
                match n
                    .child_by_field_name("arguments")
                    .and_then(|a| a.named_child(0))
                {
                    Some(a) if ctor => self.fold(a, out, depth + 1),
                    _ => out.push(KeyPart::Segment),
                }
            }
            "identifier" | "qualified_identifier" => {
                let name = match n.kind() {
                    "qualified_identifier" => n
                        .child_by_field_name("name")
                        .map(|x| self.text(x))
                        .unwrap_or(""),
                    _ => self.text(n),
                };
                match self.constant(name, depth) {
                    Some(parts) => out.extend(parts),
                    None => out.push(KeyPart::Segment),
                }
            }
            _ => out.push(KeyPart::Segment),
        }
    }

    fn string(&self, n: Node) -> String {
        let mut s = String::new();
        let mut c = n.walk();
        for x in n.children(&mut c) {
            match x.kind() {
                "string_content" => s.push_str(self.text(x)),
                "escape_sequence" => s.push_str(&unescape(self.text(x))),
                _ => {}
            }
        }
        s
    }

    /// Value of a file-scope constant, folded, when it is entirely literal.
    fn constant(&self, name: &str, depth: u32) -> Option<Vec<KeyPart>> {
        let mut stack = vec![self.root];
        while let Some(x) = stack.pop() {
            match x.kind() {
                "preproc_def" => {
                    if x.child_by_field_name("name").map(|n| self.text(n)) == Some(name)
                        && let Some(v) = x.child_by_field_name("value")
                    {
                        let src = format!("auto __cv_key = {};\n", self.text(v).trim());
                        let tree = parse(&src)?;
                        let root = tree.root_node();
                        let val =
                            find_kind(root, "init_declarator")?.child_by_field_name("value")?;
                        let f = Folder { root, src: &src };
                        let mut out = Vec::new();
                        f.fold(val, &mut out, depth + 1);
                        return literal_only(out);
                    }
                }
                "init_declarator" => {
                    let mut d = x.child_by_field_name("declarator");
                    while let Some(dd) = d
                        && matches!(dd.kind(), "pointer_declarator" | "reference_declarator")
                    {
                        d = dd
                            .child_by_field_name("declarator")
                            .or_else(|| dd.named_child(0));
                    }
                    let named = d.is_some_and(|d| self.text(d) == name);
                    let is_const = x.parent().is_some_and(|p| {
                        let mut c = p.walk();
                        p.children(&mut c).any(|k| {
                            k.kind() == "type_qualifier"
                                && matches!(self.text(k), "const" | "constexpr" | "constinit")
                        })
                    });
                    if named
                        && is_const
                        && let Some(v) = x.child_by_field_name("value")
                    {
                        let v = if matches!(v.kind(), "initializer_list" | "argument_list") {
                            v.named_child(0)?
                        } else {
                            v
                        };
                        let mut out = Vec::new();
                        self.fold(v, &mut out, depth + 1);
                        return literal_only(out);
                    }
                }
                "function_definition" => continue,
                _ => {}
            }
            let mut c = x.walk();
            let kids: Vec<Node> = x.named_children(&mut c).collect();
            stack.extend(kids.into_iter().rev());
        }
        None
    }
}

fn literal_only(parts: Vec<KeyPart>) -> Option<Vec<KeyPart>> {
    parts
        .iter()
        .all(|p| matches!(p, KeyPart::Lit(_)))
        .then_some(parts)
}

fn find_kind<'t>(n: Node<'t>, kind: &str) -> Option<Node<'t>> {
    if n.kind() == kind {
        return Some(n);
    }
    let mut c = n.walk();
    let kids: Vec<Node> = n.named_children(&mut c).collect();
    kids.into_iter().find_map(|k| find_kind(k, kind))
}

/// C escape sequences → text (`\n`, `\t`, `\\`, `\"`, `\x41`, `é`, octal).
fn unescape(s: &str) -> String {
    let mut out = String::new();
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
            Some(d @ '0'..='7') => {
                let mut v = d.to_digit(8).unwrap_or(0);
                for _ in 0..2 {
                    match it.peek().and_then(|c| c.to_digit(8)) {
                        Some(x) => {
                            v = v * 8 + x;
                            it.next();
                        }
                        None => break,
                    }
                }
                out.extend(char::from_u32(v));
            }
            Some('x') => {
                let mut v = 0u32;
                while let Some(x) = it.peek().and_then(|c| c.to_digit(16)) {
                    v = v * 16 + x;
                    it.next();
                }
                out.extend(char::from_u32(v));
            }
            Some(u @ ('u' | 'U')) => {
                let n = if u == 'u' { 4 } else { 8 };
                let hex: String = (0..n).filter_map(|_| it.next()).collect();
                out.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
            }
            Some(o) => out.push(o),
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests;
