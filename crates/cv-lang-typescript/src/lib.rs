//! L2 TypeScript pack: TS, TSX and JS/JSX (parsed with the TSX grammar) → symbols, imports
//! (`import`, `export … from`, `require`, dynamic `import()`), call / extends / implements refs and
//! highlight tokens. Imports resolve relative specifiers and `tsconfig.json` `paths`/`baseUrl`.

pub mod sites;

use cv_lang::{
    BlobFacts, Conventions, ImportFact, LaneHints, LanguagePack, ProjectIndex, RefFact, RefKind,
    SymbolFact, SymbolKind, dirname, normalize_path,
};
use cv_lang_sitter::query::{PackQuery, Tag};
use cv_lang_sitter::{Text, count_errors, end_line, line};
use std::sync::OnceLock;
use tree_sitter::{Node, Parser};

pub struct TypeScriptPack;

const EXTS: [&str; 7] = [".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs"];

/// Builtin and standard-library names lexical candidates never guess (`Conventions`).
const BUILTIN_METHODS: &[&str] = &[
    "push",
    "pop",
    "shift",
    "unshift",
    "slice",
    "splice",
    "concat",
    "join",
    "reverse",
    "sort",
    "indexOf",
    "lastIndexOf",
    "includes",
    "find",
    "findIndex",
    "findLast",
    "findLastIndex",
    "filter",
    "map",
    "forEach",
    "reduce",
    "reduceRight",
    "some",
    "every",
    "flat",
    "flatMap",
    "fill",
    "keys",
    "values",
    "entries",
    "at",
    "get",
    "set",
    "has",
    "delete",
    "clear",
    "add",
    "size",
    "then",
    "catch",
    "finally",
    "resolve",
    "reject",
    "all",
    "allSettled",
    "race",
    "any",
    "split",
    "trim",
    "trimStart",
    "trimEnd",
    "startsWith",
    "endsWith",
    "toLowerCase",
    "toUpperCase",
    "replace",
    "replaceAll",
    "padStart",
    "padEnd",
    "charAt",
    "charCodeAt",
    "codePointAt",
    "substring",
    "substr",
    "match",
    "matchAll",
    "search",
    "repeat",
    "localeCompare",
    "normalize",
    "toString",
    "toFixed",
    "toPrecision",
    "valueOf",
    "toISOString",
    "getTime",
    "toJSON",
    "addEventListener",
    "removeEventListener",
    "dispatchEvent",
    "preventDefault",
    "stopPropagation",
    "querySelector",
    "querySelectorAll",
    "getElementById",
    "appendChild",
    "removeChild",
    "setAttribute",
    "getAttribute",
    "json",
    "text",
    "blob",
    "arrayBuffer",
    "test",
    "exec",
];
const BUILTIN_RECEIVERS: &[&str] = &[
    "console",
    "JSON",
    "Math",
    "Object",
    "Array",
    "Promise",
    "Number",
    "String",
    "Boolean",
    "Date",
    "Reflect",
    "Symbol",
    "Map",
    "Set",
    "WeakMap",
    "WeakSet",
    "RegExp",
    "Error",
    "Intl",
    "window",
    "document",
    "navigator",
    "location",
    "history",
    "localStorage",
    "sessionStorage",
    "performance",
    "crypto",
    "process",
    "Buffer",
    "URL",
    "URLSearchParams",
    "globalThis",
    "structuredClone",
];
const BUILTIN_FUNCTIONS: &[&str] = &[
    "parseInt",
    "parseFloat",
    "isNaN",
    "isFinite",
    "setTimeout",
    "clearTimeout",
    "setInterval",
    "clearInterval",
    "requestAnimationFrame",
    "cancelAnimationFrame",
    "queueMicrotask",
    "fetch",
    "alert",
    "confirm",
    "encodeURIComponent",
    "decodeURIComponent",
    "encodeURI",
    "decodeURI",
    "require",
    "structuredClone",
];

impl LanguagePack for TypeScriptPack {
    fn id(&self) -> &'static str {
        "typescript"
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn claims(&self, path: &str) -> bool {
        EXTS.iter().any(|e| path.ends_with(e)) || path.ends_with(".cjs")
    }
    fn lang(&self, path: &str) -> &'static str {
        if path.ends_with(".tsx") {
            "tsx"
        } else if path.ends_with(".ts") || path.ends_with(".mts") || path.ends_with(".cts") {
            "typescript"
        } else {
            "javascript"
        }
    }
    fn conventions(&self) -> Conventions {
        Conventions {
            self_receivers: &["this"],
            // `dir/index.ts` is module `dir`.
            module_entry_stems: &["index"],
            entry_stems: &["main"],
            // `__dirname`, `__proto__`, bundler internals.
            reserved_prefixes: &["__"],
            builtin_methods: BUILTIN_METHODS,
            builtin_receivers: BUILTIN_RECEIVERS,
            builtin_functions: BUILTIN_FUNCTIONS,
        }
    }
    fn lane_hints(&self) -> LaneHints {
        LaneHints {
            code: vec![
                "*.ts", "*.tsx", "*.mts", "*.cts", "*.js", "*.jsx", "*.mjs", "*.cjs",
            ],
            test: vec![
                "*.test.[jt]s",
                "*.test.[jt]sx",
                "*.spec.[jt]s",
                "*.spec.[jt]sx",
            ],
        }
    }
    fn analyze(&self, path: &str, source: &str) -> BlobFacts {
        analyze(path, source)
    }
    fn resolve_import(
        &self,
        from: &str,
        import: &ImportFact,
        project: &dyn ProjectIndex,
    ) -> Option<String> {
        resolve(from, import, project)
    }
    fn site_extractor(
        &self,
        queries: &[cv_lang::keys::SiteQuery],
        transforms: &[cv_lang::keys::FoldTransform],
    ) -> Option<Box<dyn cv_lang::keys::SiteExtractor>> {
        Some(Box::new(sites::TsSites::new(queries, transforms)))
    }
}

/// `queries/highlights.scm` (with `queries/jsx.scm` for TSX) and `queries/tags.scm`
/// (DESIGN §4.2), compiled once per grammar.
fn queries(tsx: bool) -> &'static PackQuery {
    static TS: OnceLock<PackQuery> = OnceLock::new();
    static TSX: OnceLock<PackQuery> = OnceLock::new();
    let hl = include_str!("../queries/highlights.scm");
    let tags = include_str!("../queries/tags.scm");
    if tsx {
        TSX.get_or_init(|| {
            let hl = hl.replace("; @jsx\n", include_str!("../queries/jsx.scm"));
            PackQuery::new(&tree_sitter_typescript::LANGUAGE_TSX.into(), &hl, tags)
        })
    } else {
        TS.get_or_init(|| {
            PackQuery::new(
                &tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                hl,
                tags,
            )
        })
    }
}

pub fn analyze(path: &str, source: &str) -> BlobFacts {
    let mut parser = Parser::new();
    let tsx = !(path.ends_with(".ts") || path.ends_with(".mts") || path.ends_with(".cts"));
    let lang = if tsx {
        tree_sitter_typescript::LANGUAGE_TSX
    } else {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT
    };
    parser
        .set_language(&lang.into())
        .expect("typescript grammar");
    let text = Text::new(source);
    let Some(tree) = parser.parse(source, None) else {
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
    let (tokens, tags) = queries(tsx).run(root, &text);
    Facts {
        text: &text,
        facts: &mut facts,
    }
    .read(root, &tags);
    facts.tokens = tokens;
    facts
}

fn is_function_value(n: Node) -> bool {
    matches!(
        n.kind(),
        "arrow_function" | "function_expression" | "function" | "generator_function"
    )
}

/// A declaration whose body we are in.
struct Open {
    end: usize,
    id: usize,
    /// Innermost symbol inside this declaration.
    enclosing: Option<usize>,
    /// For a class read as a declaration: its symbol (when it is one) and its body's byte range.
    /// Only the heritage clause and the body of such a class are read: anything else in it
    /// (decorators, the heritage expressions) contributes no facts.
    class: Option<(Option<usize>, usize, usize)>,
}

struct Facts<'t, 'a, 'f> {
    text: &'t Text<'a>,
    facts: &'f mut BlobFacts,
}

impl<'a> Facts<'_, 'a, '_> {
    fn t(&self, n: Node) -> &'a str {
        self.text.node_text(n)
    }

    fn push_symbol(
        &mut self,
        kind: SymbolKind,
        name: &str,
        container: Option<usize>,
        outer: Node,
        name_node: Node,
    ) -> usize {
        let qualname = match container {
            Some(c) => format!("{}.{name}", self.facts.symbols[c].qualname),
            None => name.to_string(),
        };
        self.facts.symbols.push(SymbolFact {
            kind,
            name: name.to_string(),
            qualname,
            parent: container,
            start: line(outer),
            end: end_line(outer),
            name_line: line(name_node),
        });
        self.facts.symbols.len() - 1
    }

    /// The node whose lines a declaration covers: include `export` / `export default` wrappers.
    fn outer(n: Node) -> Node {
        match n.parent() {
            Some(p) if p.kind() == "export_statement" => p,
            _ => n,
        }
    }

    /// Symbols: module-level declarations (functions, classes, interfaces, types, enums,
    /// namespaces, single-variable `const`/`let`/`var`) and the methods of module-level classes.
    /// Refs and imports come from everywhere, attributed to the innermost symbol.
    fn read(&mut self, root: Node, tags: &[Tag]) {
        let mut open: Vec<Open> = Vec::new();
        // Module-level statements: the program's children and what its `export`s wrap.
        let mut top = std::collections::HashSet::new();
        let mut c = root.walk();
        for n in root.children(&mut c) {
            if n.kind() == "export_statement" {
                let mut cc = n.walk();
                top.extend(n.children(&mut cc).map(|x| x.id()));
            } else {
                top.insert(n.id());
            }
        }
        for tag in tags {
            let (what, node) = tag.main();
            while open.last().is_some_and(|o| o.end <= node.start_byte()) {
                open.pop();
            }
            let inner = open.last();
            if let Some(&Open {
                class: Some((_, s, e)),
                ..
            }) = inner
                && (node.start_byte() < s || node.end_byte() > e)
            {
                continue;
            }
            let enclosing = inner.and_then(|o| o.enclosing);
            match what {
                "declaration" if top.contains(&node.id()) => {
                    if let Some(id) = self.declaration(node) {
                        open.push(Open {
                            end: node.end_byte(),
                            id: node.id(),
                            enclosing: Some(id),
                            class: None,
                        });
                    }
                }
                "definition.class" => {
                    let is_top = top.contains(&node.id());
                    if is_top || enclosing.is_none() {
                        let me = match node.child_by_field_name("name") {
                            Some(nn) if is_top => {
                                let name = self.t(nn).to_string();
                                Some(self.push_symbol(
                                    SymbolKind::Class,
                                    &name,
                                    None,
                                    Self::outer(node),
                                    nn,
                                ))
                            }
                            _ => enclosing,
                        };
                        let mut body = (node.end_byte(), node.end_byte());
                        let mut c = node.walk();
                        for child in node.children(&mut c) {
                            match child.kind() {
                                "class_heritage" => self.heritage(child, me),
                                "class_body" => body = (child.start_byte(), child.end_byte()),
                                _ => {}
                            }
                        }
                        let class = if me != enclosing { me } else { None };
                        open.push(Open {
                            end: node.end_byte(),
                            id: node.id(),
                            enclosing: me,
                            class: Some((class, body.0, body.1)),
                        });
                    }
                }
                "definition.member" => {
                    // A method of a class read as a declaration that is a symbol.
                    let class = inner.and_then(|o| match o.class {
                        Some((Some(class), _, _))
                            if node.parent().and_then(|b| b.parent()).map(|c| c.id())
                                == Some(o.id) =>
                        {
                            Some(class)
                        }
                        _ => None,
                    });
                    let is_method = node.kind() == "method_definition"
                        || node
                            .child_by_field_name("value")
                            .is_some_and(is_function_value);
                    if is_method
                        && let Some(class) = class
                        && let Some(nn) = node.child_by_field_name("name")
                    {
                        let name = self.t(nn).to_string();
                        let id = self.push_symbol(SymbolKind::Method, &name, Some(class), node, nn);
                        open.push(Open {
                            end: node.end_byte(),
                            id: node.id(),
                            enclosing: Some(id),
                            class: None,
                        });
                    }
                }
                "call" => {
                    let Some(f) = tag.get("callee") else { continue };
                    if f.kind() == "import" || (f.kind() == "identifier" && self.t(f) == "require")
                    {
                        if let Some(args) = node.child_by_field_name("arguments")
                            && let Some(s) = args.named_child(0)
                            && s.kind() == "string"
                        {
                            let module = string_value(self.t(s));
                            self.facts.imports.push(ImportFact {
                                local: String::new(),
                                module,
                                imported: None,
                                line: line(node),
                            });
                        }
                    } else {
                        self.call(node, f, enclosing);
                    }
                }
                "new" => {
                    if let Some(f) = tag.get("callee") {
                        self.call(node, f, enclosing);
                    }
                }
                "import" => self.import(node),
                "export.from" => {
                    if let Some(src) = node.child_by_field_name("source") {
                        self.import_from_source(node, src);
                    }
                }
                _ => {}
            }
        }
    }

    /// A module-level declaration other than a class: its symbol, if it makes one.
    fn declaration(&mut self, node: Node) -> Option<usize> {
        let kind = node.kind();
        match kind {
            "function_declaration" | "generator_function_declaration" => {
                let nn = node.child_by_field_name("name")?;
                let name = self.t(nn).to_string();
                let k = if name.starts_with("test") {
                    SymbolKind::Test
                } else {
                    SymbolKind::Function
                };
                Some(self.push_symbol(k, &name, None, Self::outer(node), nn))
            }
            "interface_declaration"
            | "type_alias_declaration"
            | "enum_declaration"
            | "internal_module"
            | "module" => {
                let nn = node.child_by_field_name("name")?;
                let k = match kind {
                    "interface_declaration" => SymbolKind::Interface,
                    "enum_declaration" => SymbolKind::Enum,
                    "type_alias_declaration" => SymbolKind::Type,
                    _ => SymbolKind::Module,
                };
                let name = self.t(nn).to_string();
                let id = self.push_symbol(k, &name, None, Self::outer(node), nn);
                if kind == "interface_declaration" {
                    let mut c = node.walk();
                    for child in node.children(&mut c) {
                        if child.kind() == "extends_type_clause" {
                            self.heritage(child, Some(id));
                        }
                    }
                }
                Some(id)
            }
            "lexical_declaration" | "variable_declaration" => {
                let mut c = node.walk();
                let decls: Vec<Node> = node
                    .named_children(&mut c)
                    .filter(|d| d.kind() == "variable_declarator")
                    .collect();
                if decls.len() != 1 {
                    return None;
                }
                let nn = decls[0]
                    .child_by_field_name("name")
                    .filter(|n| n.kind() == "identifier")?;
                let value = decls[0].child_by_field_name("value");
                let k = if value.is_some_and(is_function_value) {
                    SymbolKind::Function
                } else {
                    SymbolKind::Constant
                };
                let name = self.t(nn).to_string();
                Some(self.push_symbol(k, &name, None, Self::outer(node), nn))
            }
            _ => None,
        }
    }

    fn call(&mut self, node: Node, f: Node, enclosing: Option<usize>) {
        if let Some((name, q)) = self.callee(f) {
            self.facts.refs.push(RefFact {
                kind: RefKind::Call,
                name,
                qualifier: q,
                line: line(node),
                enclosing,
            });
        }
    }

    fn heritage(&mut self, n: Node, me: Option<usize>) {
        let mut stack = vec![n];
        while let Some(x) = stack.pop() {
            let kind = match x.kind() {
                "extends_clause" | "extends_type_clause" => Some(RefKind::Inherit),
                "implements_clause" => Some(RefKind::Implement),
                _ => None,
            };
            if let Some(kind) = kind {
                let mut c = x.walk();
                for v in x.named_children(&mut c) {
                    let v = if v.kind() == "generic_type" {
                        v.child_by_field_name("name").unwrap_or(v)
                    } else {
                        v
                    };
                    if let Some((name, q)) = self.callee(v) {
                        self.facts.refs.push(RefFact {
                            kind,
                            name,
                            qualifier: q,
                            line: line(v),
                            enclosing: me,
                        });
                    }
                }
            } else {
                let mut c = x.walk();
                stack.extend(x.named_children(&mut c));
            }
        }
    }

    /// `f` → ("f", None); `a.b.f` → ("f", Some("a.b")); `this.f` → ("f", Some("this")).
    fn callee(&self, f: Node) -> Option<(String, Option<String>)> {
        match f.kind() {
            "identifier" | "type_identifier" => Some((self.t(f).to_string(), None)),
            "member_expression" | "nested_type_identifier" => {
                let prop = f
                    .child_by_field_name("property")
                    .or_else(|| f.child_by_field_name("name"))?;
                let obj = f
                    .child_by_field_name("object")
                    .or_else(|| f.child_by_field_name("module"))?;
                let q = matches!(
                    obj.kind(),
                    "identifier" | "this" | "member_expression" | "nested_identifier"
                )
                .then(|| self.t(obj).to_string());
                Some((self.t(prop).to_string(), q))
            }
            _ => None,
        }
    }

    fn import(&mut self, node: Node) {
        let Some(src) = node.child_by_field_name("source") else {
            // `import x = require("y")`
            let mut c = node.walk();
            for ch in node.named_children(&mut c) {
                if ch.kind() == "import_require_clause"
                    && let Some(s) = ch.child_by_field_name("source")
                {
                    let local = ch
                        .named_child(0)
                        .map(|i| self.t(i).to_string())
                        .unwrap_or_default();
                    self.facts.imports.push(ImportFact {
                        local,
                        module: string_value(self.t(s)),
                        imported: None,
                        line: line(node),
                    });
                }
            }
            return;
        };
        let module = string_value(self.t(src));
        let l = line(node);
        let mut any = false;
        let mut c = node.walk();
        for clause in node.named_children(&mut c) {
            if clause.kind() != "import_clause" {
                continue;
            }
            let mut cc = clause.walk();
            for part in clause.named_children(&mut cc) {
                match part.kind() {
                    "identifier" => {
                        any = true;
                        self.facts.imports.push(ImportFact {
                            local: self.t(part).into(),
                            module: module.clone(),
                            imported: Some("default".into()),
                            line: l,
                        });
                    }
                    "namespace_import" => {
                        any = true;
                        let local = part
                            .named_child(0)
                            .map(|i| self.t(i).to_string())
                            .unwrap_or_default();
                        self.facts.imports.push(ImportFact {
                            local,
                            module: module.clone(),
                            imported: None,
                            line: l,
                        });
                    }
                    "named_imports" => {
                        let mut sc = part.walk();
                        for spec in part.named_children(&mut sc) {
                            if spec.kind() != "import_specifier" {
                                continue;
                            }
                            let Some(name) = spec.child_by_field_name("name") else {
                                continue;
                            };
                            let local = spec.child_by_field_name("alias").unwrap_or(name);
                            any = true;
                            self.facts.imports.push(ImportFact {
                                local: self.t(local).into(),
                                module: module.clone(),
                                imported: Some(string_value(self.t(name))),
                                line: l,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        if !any {
            self.facts.imports.push(ImportFact {
                local: String::new(),
                module,
                imported: None,
                line: l,
            });
        }
    }

    fn import_from_source(&mut self, node: Node, src: Node) {
        let module = string_value(self.t(src));
        self.facts.imports.push(ImportFact {
            local: String::new(),
            module,
            imported: Some("*".into()),
            line: line(node),
        });
    }
}

fn string_value(s: &str) -> String {
    s.trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .to_string()
}

// ── Import resolution ────────────────────────────────────────────────────────

const RESOLVE_EXTS: [&str; 9] = [
    "", ".ts", ".tsx", ".d.ts", ".js", ".jsx", ".mjs", ".mts", ".cjs",
];

fn try_file(base: &str, project: &dyn ProjectIndex) -> Option<String> {
    for e in RESOLVE_EXTS {
        let p = format!("{base}{e}");
        // A bare path only counts when it already names a source file.
        if (!e.is_empty() || has_known_ext(&p)) && project.exists(&p) {
            return Some(p);
        }
    }
    // ESM style: "./x.js" written for "./x.ts".
    for (js, ts) in [
        (".js", ".ts"),
        (".js", ".tsx"),
        (".jsx", ".tsx"),
        (".mjs", ".mts"),
    ] {
        if let Some(stem) = base.strip_suffix(js) {
            let p = format!("{stem}{ts}");
            if project.exists(&p) {
                return Some(p);
            }
        }
    }
    for e in &RESOLVE_EXTS[1..] {
        let p = format!("{base}/index{e}");
        if project.exists(&p) {
            return Some(p);
        }
    }
    None
}

fn has_known_ext(p: &str) -> bool {
    EXTS.iter().any(|e| p.ends_with(e))
        || p.ends_with(".cjs")
        || p.ends_with(".json")
        || p.ends_with(".css")
}

/// `compilerOptions.baseUrl` and `paths` of the nearest tsconfig.json above `from`.
type PathMap = Vec<(String, Vec<String>)>;

fn tsconfig_paths(from: &str, project: &dyn ProjectIndex) -> Option<(String, PathMap)> {
    let mut dir = dirname(from).to_string();
    loop {
        let cand = if dir.is_empty() {
            "tsconfig.json".to_string()
        } else {
            format!("{dir}/tsconfig.json")
        };
        if project.exists(&cand) {
            let text = project.read(&cand)?;
            let json: serde_json::Value = serde_json::from_str(&strip_json_comments(&text)).ok()?;
            let opts = json.get("compilerOptions")?;
            let base_url = opts.get("baseUrl").and_then(|b| b.as_str()).unwrap_or(".");
            let base = normalize_path(&format!("{dir}/{base_url}"))?;
            let mut paths = Vec::new();
            if let Some(map) = opts.get("paths").and_then(|p| p.as_object()) {
                for (k, v) in map {
                    let targets = v
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|t| t.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    paths.push((k.clone(), targets));
                }
            }
            return Some((base, paths));
        }
        if dir.is_empty() {
            return None;
        }
        dir = dirname(&dir).to_string();
    }
}

fn strip_json_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let (mut i, mut in_str) = (0, false);
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c as char);
            if c == b'\\' && i + 1 < b.len() {
                out.push(b[i + 1] as char);
                i += 1;
            } else if c == b'"' {
                in_str = false;
            }
        } else if c == b'"' {
            in_str = true;
            out.push('"');
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
            continue;
        } else if c == b',' {
            // Drop trailing commas before } or ].
            let rest = s[i + 1..].trim_start();
            if !(rest.starts_with('}') || rest.starts_with(']')) {
                out.push(',');
            }
        } else {
            out.push(c as char);
        }
        i += 1;
    }
    out
}

pub fn resolve(from: &str, import: &ImportFact, project: &dyn ProjectIndex) -> Option<String> {
    let spec = import.module.as_str();
    if spec.starts_with("./") || spec.starts_with("../") || spec == "." || spec == ".." {
        let joined = normalize_path(&format!("{}/{spec}", dirname(from)))?;
        return try_file(&joined, project);
    }
    let (base, paths) = tsconfig_paths(from, project)?;
    for (pattern, targets) in &paths {
        let captured = match pattern.split_once('*') {
            Some((pre, post)) => spec.strip_prefix(pre).and_then(|r| r.strip_suffix(post)),
            None => (spec == pattern).then_some(""),
        };
        if let Some(cap) = captured {
            for t in targets {
                let p = normalize_path(&format!("{base}/{}", t.replace('*', cap)))?;
                if let Some(f) = try_file(&p, project) {
                    return Some(f);
                }
            }
        }
    }
    // baseUrl-relative bare specifiers.
    let p = if base.is_empty() {
        spec.to_string()
    } else {
        format!("{base}/{spec}")
    };
    try_file(&p, project)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cv_lang::PathIndex;

    const SAMPLE: &str = r#"import React, { useState as useS } from "react";
import * as api from "./api";
import "./styles.css";
export { thing } from "../shared/thing";

export interface Props extends Base { id: string }

export class Graph extends Component<Props> implements Drawable {
  private nodes = new Map();

  render() {
    this.layout();
    return <Node id={api.get("x")} />;
  }

  layout = () => {
    helper(1);
  };
}

export const helper = (n: number): number => n + 1;
const LIMIT = 10;
function main() {
  new Graph();
}
"#;

    /// The generated anonymous-token block of `highlights.scm` matches the TypeScript grammar
    /// (regenerate: `cargo test -p cv-lang-typescript anonymous -- --ignored --nocapture`), and
    /// `jsx.scm` classifies the two tokens only TSX has.
    #[test]
    fn highlights_cover_every_anonymous_token() {
        let src = include_str!("../queries/highlights.scm");
        let ts = block(tree_sitter_typescript::LANGUAGE_TYPESCRIPT);
        assert_eq!(
            cv_lang_sitter::query::anonymous_block_in(src),
            Some(ts.as_str())
        );
        let toks = |b: &str| -> std::collections::BTreeSet<String> {
            b.split_whitespace()
                .filter(|t| t.starts_with('"'))
                .map(String::from)
                .collect()
        };
        let tsx = block(tree_sitter_typescript::LANGUAGE_TSX);
        let extra: Vec<String> = toks(&tsx).difference(&toks(&ts)).cloned().collect();
        assert_eq!(extra, ["\"/>\"", "\"</\""]);
        assert!(include_str!("../queries/jsx.scm").contains("[\"</\" \"/>\"] @operator"));
    }

    fn block(lang: impl Into<tree_sitter::Language>) -> String {
        cv_lang_sitter::query::anonymous_block(&lang.into(), &[], cv_lang_sitter::anonymous_class)
    }

    #[test]
    #[ignore]
    fn print_anonymous_block() {
        println!("{}", block(tree_sitter_typescript::LANGUAGE_TYPESCRIPT));
    }

    #[test]
    fn symbols_imports_and_refs() {
        let f = analyze("x.tsx", SAMPLE);
        let syms: Vec<(&str, &str, u32, u32)> = f
            .symbols
            .iter()
            .map(|s| (s.qualname.as_str(), s.kind.as_str(), s.start, s.end))
            .collect();
        assert_eq!(
            syms,
            vec![
                ("Props", "interface", 6, 6),
                ("Graph", "class", 8, 19),
                ("Graph.render", "method", 11, 14),
                ("Graph.layout", "method", 16, 18),
                ("helper", "function", 21, 21),
                ("LIMIT", "constant", 22, 22),
                ("main", "function", 23, 25),
            ]
        );
        let imps: Vec<(&str, &str, Option<&str>)> = f
            .imports
            .iter()
            .map(|i| (i.local.as_str(), i.module.as_str(), i.imported.as_deref()))
            .collect();
        assert_eq!(
            imps,
            vec![
                ("React", "react", Some("default")),
                ("useS", "react", Some("useState")),
                ("api", "./api", None),
                ("", "./styles.css", None),
                ("", "../shared/thing", Some("*")),
            ]
        );
        let refs: Vec<(&str, Option<&str>, &str, u32)> = f
            .refs
            .iter()
            .map(|r| {
                (
                    r.name.as_str(),
                    r.qualifier.as_deref(),
                    r.kind.as_str(),
                    r.line,
                )
            })
            .collect();
        assert!(refs.contains(&("Base", None, "inherit", 6)), "{refs:?}");
        assert!(
            refs.contains(&("Component", None, "inherit", 8)),
            "{refs:?}"
        );
        assert!(
            refs.contains(&("Drawable", None, "implement", 8)),
            "{refs:?}"
        );
        assert!(
            refs.contains(&("layout", Some("this"), "call", 12)),
            "{refs:?}"
        );
        assert!(refs.contains(&("get", Some("api"), "call", 13)), "{refs:?}");
        assert!(refs.contains(&("helper", None, "call", 17)), "{refs:?}");
        assert!(refs.contains(&("Graph", None, "call", 24)), "{refs:?}");
        assert_eq!(f.syntax_errors, 0);
    }

    #[test]
    fn resolves_relative_esm_index_and_tsconfig_paths() {
        let paths = [
            "web/tsconfig.json",
            "web/src/app/main.ts",
            "web/src/app/util.ts",
            "web/src/lib/index.tsx",
            "web/src/data/client.ts",
        ];
        let idx = PathIndex::new(paths.iter().copied(), |p| {
            (p == "web/tsconfig.json").then(|| {
                r#"{ // comment
                  "compilerOptions": { "baseUrl": ".", "paths": { "@/*": ["src/*"], }, },
                }"#
                .to_string()
            })
        });
        let imp = |m: &str| ImportFact {
            local: String::new(),
            module: m.into(),
            imported: None,
            line: 1,
        };
        let from = "web/src/app/main.ts";
        assert_eq!(
            resolve(from, &imp("./util"), &idx).as_deref(),
            Some("web/src/app/util.ts")
        );
        assert_eq!(
            resolve(from, &imp("./util.js"), &idx).as_deref(),
            Some("web/src/app/util.ts")
        );
        assert_eq!(
            resolve(from, &imp("../lib"), &idx).as_deref(),
            Some("web/src/lib/index.tsx")
        );
        assert_eq!(
            resolve(from, &imp("@/data/client"), &idx).as_deref(),
            Some("web/src/data/client.ts")
        );
        assert_eq!(resolve(from, &imp("react"), &idx), None);
    }
}
