//! L1 Python pack: tree-sitter-python → symbols (functions, classes, methods), imports, call and
//! inheritance refs, highlight tokens; import resolution against package roots (repo root, `src/`,
//! and every directory holding a `pyproject.toml`, plus its `src/`).

use cv_lang::{
    BlobFacts, Conventions, ImportFact, LaneHints, LanguagePack, ProjectIndex, RefFact, RefKind,
    SymbolFact, SymbolKind, dirname,
};
use cv_lang_sitter::query::{PackQuery, Tag};
use cv_lang_sitter::{Text, count_errors, end_line, line};
use std::sync::OnceLock;
use tree_sitter::{Node, Parser};

pub mod sites;

pub struct PythonPack;

/// Builtin and standard-library names lexical candidates never guess (`Conventions`).
const BUILTIN_METHODS: &[&str] = &[
    "open",
    "is_alive",
    "append",
    "extend",
    "insert",
    "pop",
    "remove",
    "clear",
    "copy",
    "count",
    "index",
    "sort",
    "reverse",
    "add",
    "discard",
    "update",
    "get",
    "items",
    "keys",
    "values",
    "setdefault",
    "popitem",
    "union",
    "intersection",
    "difference",
    "symmetric_difference",
    "issubset",
    "issuperset",
    "isdisjoint",
    "join",
    "split",
    "rsplit",
    "splitlines",
    "strip",
    "lstrip",
    "rstrip",
    "replace",
    "startswith",
    "endswith",
    "lower",
    "upper",
    "casefold",
    "title",
    "capitalize",
    "format",
    "format_map",
    "encode",
    "decode",
    "find",
    "rfind",
    "partition",
    "rpartition",
    "zfill",
    "ljust",
    "rjust",
    "center",
    "expandtabs",
    "isdigit",
    "isalpha",
    "isalnum",
    "isspace",
    "isupper",
    "islower",
    "isidentifier",
    "read",
    "write",
    "readline",
    "readlines",
    "writelines",
    "close",
    "flush",
    "seek",
    "tell",
    "fileno",
    "exists",
    "is_file",
    "is_dir",
    "mkdir",
    "rmdir",
    "unlink",
    "touch",
    "resolve",
    "absolute",
    "iterdir",
    "glob",
    "rglob",
    "read_text",
    "write_text",
    "read_bytes",
    "write_bytes",
    "with_suffix",
    "with_name",
    "with_stem",
    "relative_to",
    "joinpath",
    "expanduser",
    "match",
    "search",
    "fullmatch",
    "findall",
    "finditer",
    "sub",
    "subn",
    "group",
    "groups",
    "groupdict",
    "span",
    "wait",
    "notify",
    "notify_all",
    "acquire",
    "release",
    "is_set",
    "put",
    "get_nowait",
    "put_nowait",
    "task_done",
    "qsize",
    "empty",
    "cancel",
    "cancelled",
    "result",
    "exception",
    "done",
    "add_done_callback",
    "set_result",
    "set_exception",
    "total_seconds",
    "isoformat",
    "strftime",
    "timestamp",
    "astimezone",
    "date",
    "time",
    "weekday",
    "hexdigest",
    "digest",
];
const BUILTIN_RECEIVERS: &[&str] = &[
    "os",
    "sys",
    "re",
    "json",
    "math",
    "time",
    "datetime",
    "logging",
    "asyncio",
    "random",
    "itertools",
    "functools",
    "collections",
    "typing",
    "pathlib",
    "shutil",
    "subprocess",
    "threading",
    "uuid",
    "hashlib",
    "base64",
    "copy",
    "dataclasses",
    "enum",
    "inspect",
    "operator",
    "string",
    "textwrap",
    "traceback",
    "warnings",
    "yaml",
    "np",
    "pd",
    "str",
    "int",
    "float",
    "dict",
    "list",
    "set",
    "tuple",
    "bytes",
    "object",
    "type",
    "super",
];
const BUILTIN_FUNCTIONS: &[&str] = &[
    "print",
    "len",
    "range",
    "enumerate",
    "zip",
    "map",
    "filter",
    "sorted",
    "reversed",
    "sum",
    "min",
    "max",
    "abs",
    "any",
    "all",
    "isinstance",
    "issubclass",
    "hasattr",
    "getattr",
    "setattr",
    "delattr",
    "callable",
    "iter",
    "next",
    "open",
    "repr",
    "str",
    "int",
    "float",
    "bool",
    "dict",
    "list",
    "set",
    "tuple",
    "frozenset",
    "bytes",
    "bytearray",
    "type",
    "id",
    "hash",
    "round",
    "divmod",
    "pow",
    "format",
    "vars",
    "dir",
    "input",
    "ord",
    "chr",
    "bin",
    "hex",
    "oct",
    "super",
    "object",
    "property",
    "staticmethod",
    "classmethod",
];

impl LanguagePack for PythonPack {
    fn id(&self) -> &'static str {
        "python"
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn claims(&self, path: &str) -> bool {
        path.ends_with(".py") || path.ends_with(".pyi")
    }
    fn lang(&self, _: &str) -> &'static str {
        "python"
    }
    fn conventions(&self) -> Conventions {
        Conventions {
            // `self.f()` / `cls.f()` reach a member of the enclosing class.
            self_receivers: &["self", "cls"],
            // `pkg/__init__.py` is module `pkg`.
            module_entry_stems: &["__init__"],
            entry_stems: &["main"],
            // Dunders (`__init__`, `__call__`) are never guessed.
            reserved_prefixes: &["__"],
            builtin_methods: BUILTIN_METHODS,
            builtin_receivers: BUILTIN_RECEIVERS,
            builtin_functions: BUILTIN_FUNCTIONS,
        }
    }
    fn lane_hints(&self) -> LaneHints {
        LaneHints {
            code: vec!["*.py", "*.pyi"],
            test: vec!["test_*.py", "*_test.py", "conftest.py"],
        }
    }
    fn analyze(&self, _path: &str, source: &str) -> BlobFacts {
        analyze(source)
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
        Some(Box::new(sites::PythonSites::new(queries, transforms)))
    }
}

/// `queries/highlights.scm` and `queries/tags.scm` (DESIGN §4.2), compiled once.
fn queries() -> &'static PackQuery {
    static Q: OnceLock<PackQuery> = OnceLock::new();
    Q.get_or_init(|| {
        PackQuery::new(
            &tree_sitter_python::LANGUAGE.into(),
            include_str!("../queries/highlights.scm"),
            include_str!("../queries/tags.scm"),
        )
    })
}

pub fn analyze(source: &str) -> BlobFacts {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .expect("python grammar");
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
    let (tokens, tags) = queries().run(root, &text);
    Facts {
        text: &text,
        facts: &mut facts,
    }
    .read(&tags);
    facts.tokens = tokens;
    facts
}

/// A definition (`def`/`class`) whose body we are in.
struct Open {
    end: usize,
    id: usize,
    /// The class's symbol, when this is a class that is a symbol (its direct members are too).
    class_symbol: Option<usize>,
    /// Innermost symbol inside this definition (itself when it is a symbol).
    enclosing: Option<usize>,
    /// Byte range of a class's superclass list: calls there are not refs.
    bases: Option<(usize, usize)>,
}

struct Facts<'t, 'a, 'f> {
    text: &'t Text<'a>,
    facts: &'f mut BlobFacts,
}

impl Facts<'_, '_, '_> {
    /// Symbols are top-level definitions and the direct members of a class that is a symbol;
    /// nested functions only contribute refs, attributed to the innermost symbol.
    fn read(&mut self, tags: &[Tag]) {
        let mut open: Vec<Open> = Vec::new();
        for tag in tags {
            let (what, node) = tag.main();
            while open.last().is_some_and(|o| o.end <= node.start_byte()) {
                open.pop();
            }
            let enclosing = open.last().and_then(|o| o.enclosing);
            match what {
                "definition.class" | "definition.function" => {
                    self.definition(node, &mut open, enclosing)
                }
                "call" => {
                    let in_bases = open
                        .last()
                        .and_then(|o| o.bases)
                        .is_some_and(|(s, e)| node.start_byte() >= s && node.end_byte() <= e);
                    if !in_bases
                        && let Some(f) = tag.get("callee")
                        && let Some((name, qualifier)) = self.callee(f)
                    {
                        self.facts.refs.push(RefFact {
                            kind: RefKind::Call,
                            name,
                            qualifier,
                            line: line(node),
                            enclosing,
                        });
                    }
                }
                "import" => self.import(node),
                "import.from" => self.import_from(node),
                _ => {}
            }
        }
    }

    fn definition(&mut self, node: Node, open: &mut Vec<Open>, enclosing: Option<usize>) {
        let outer = match node.parent() {
            Some(p) if p.kind() == "decorated_definition" => p,
            _ => node,
        };
        // A direct member of a class that is a symbol: its statement sits in the class body.
        let container = open.last().and_then(|o| {
            let class = o.class_symbol?;
            let block = outer.parent().filter(|b| b.kind() == "block")?;
            (block.parent().map(|c| c.id()) == Some(o.id)).then_some(class)
        });
        let is_symbol = open.is_empty() || container.is_some();
        let class = node.kind() == "class_definition";
        let mut me = enclosing;
        let mut class_symbol = None;
        if is_symbol {
            let name = node
                .child_by_field_name("name")
                .map(|n| self.text.node_text(n).to_string())
                .unwrap_or_default();
            let kind = if class {
                SymbolKind::Class
            } else if container.is_some() {
                SymbolKind::Method
            } else if name.starts_with("test") {
                SymbolKind::Test
            } else {
                SymbolKind::Function
            };
            let qualname = match container {
                Some(c) => format!("{}.{name}", self.facts.symbols[c].qualname),
                None => name.clone(),
            };
            self.facts.symbols.push(SymbolFact {
                kind,
                name,
                qualname,
                parent: container,
                start: line(outer),
                end: end_line(outer),
                name_line: line(node),
            });
            let id = self.facts.symbols.len() - 1;
            me = Some(id);
            if class {
                class_symbol = Some(id);
            }
        }
        let mut bases = None;
        if class && let Some(sup) = node.child_by_field_name("superclasses") {
            bases = Some((sup.start_byte(), sup.end_byte()));
            let mut c = sup.walk();
            for arg in sup.named_children(&mut c) {
                if let Some((name, qualifier)) = self.callee(arg) {
                    self.facts.refs.push(RefFact {
                        kind: RefKind::Inherit,
                        name,
                        qualifier,
                        line: line(arg),
                        enclosing: me,
                    });
                }
            }
        }
        open.push(Open {
            end: node.end_byte(),
            id: node.id(),
            class_symbol,
            enclosing: me,
            bases,
        });
    }

    fn import(&mut self, node: Node) {
        let mut c = node.walk();
        for n in node.children_by_field_name("name", &mut c) {
            let (module, alias) = self.dotted_and_alias(n);
            let local = alias.unwrap_or_else(|| module.split('.').next().unwrap_or("").to_string());
            self.facts.imports.push(ImportFact {
                local,
                module,
                imported: None,
                line: line(node),
            });
        }
    }

    fn import_from(&mut self, node: Node) {
        let module = node
            .child_by_field_name("module_name")
            .map(|n| self.text.node_text(n).replace(char::is_whitespace, ""))
            .unwrap_or_default();
        let mut any = false;
        let mut c = node.walk();
        for n in node.children_by_field_name("name", &mut c) {
            let (name, alias) = self.dotted_and_alias(n);
            self.facts.imports.push(ImportFact {
                local: alias.unwrap_or_else(|| name.clone()),
                module: module.clone(),
                imported: Some(name),
                line: line(node),
            });
            any = true;
        }
        if !any {
            // `from x import *`
            self.facts.imports.push(ImportFact {
                local: String::new(),
                module,
                imported: Some("*".into()),
                line: line(node),
            });
        }
    }

    fn dotted_and_alias(&self, n: Node) -> (String, Option<String>) {
        if n.kind() == "aliased_import" {
            let name = n
                .child_by_field_name("name")
                .map(|x| self.text.node_text(x).to_string())
                .unwrap_or_default();
            let alias = n
                .child_by_field_name("alias")
                .map(|x| self.text.node_text(x).to_string());
            (name, alias)
        } else {
            (self.text.node_text(n).to_string(), None)
        }
    }

    /// `f` → ("f", None); `a.b.f` → ("f", Some("a.b")); `self.f` → ("f", Some("self")).
    fn callee(&self, f: Node) -> Option<(String, Option<String>)> {
        match f.kind() {
            "identifier" => Some((self.text.node_text(f).to_string(), None)),
            "attribute" => {
                let attr = f.child_by_field_name("attribute")?;
                let obj = f.child_by_field_name("object")?;
                let q = matches!(obj.kind(), "identifier" | "attribute")
                    .then(|| self.text.node_text(obj).to_string());
                Some((self.text.node_text(attr).to_string(), q))
            }
            _ => None,
        }
    }
}

// ── Import resolution ────────────────────────────────────────────────────────

fn module_candidates(base: &str) -> [String; 2] {
    let p = base.replace('.', "/");
    [format!("{p}.py"), format!("{p}/__init__.py")]
}

fn roots(project: &dyn ProjectIndex) -> Vec<String> {
    let mut roots = vec![String::new(), "src/".to_string()];
    for p in project.files_named("pyproject.toml") {
        let d = dirname(p);
        if !d.is_empty() {
            roots.push(format!("{d}/"));
            roots.push(format!("{d}/src/"));
        }
    }
    roots
}

fn find_module(module: &str, roots: &[String], project: &dyn ProjectIndex) -> Option<String> {
    for r in roots {
        for c in module_candidates(module) {
            let p = format!("{r}{c}");
            if project.exists(&p) {
                return Some(p);
            }
        }
    }
    None
}

pub fn resolve(from: &str, import: &ImportFact, project: &dyn ProjectIndex) -> Option<String> {
    let m = import.module.as_str();
    let dots = m.chars().take_while(|&c| c == '.').count();
    if dots > 0 {
        // Relative: from the importing file's package, climbing dots - 1 levels.
        let mut pkg: Vec<&str> = dirname(from).split('/').filter(|s| !s.is_empty()).collect();
        for _ in 1..dots {
            pkg.pop()?;
        }
        let rest = &m[dots..];
        let base = if rest.is_empty() {
            pkg.join("/")
        } else if pkg.is_empty() {
            rest.replace('.', "/")
        } else {
            format!("{}/{}", pkg.join("/"), rest.replace('.', "/"))
        };
        let base_mod = base.replace('/', ".");
        let roots = [String::new()];
        if let Some(name) = import.imported.as_deref().filter(|n| *n != "*") {
            let sub = if base_mod.is_empty() {
                name.to_string()
            } else {
                format!("{base_mod}.{name}")
            };
            if let Some(p) = find_module(&sub, &roots, project) {
                return Some(p);
            }
        }
        if base_mod.is_empty() {
            return None;
        }
        return find_module(&base_mod, &roots, project);
    }
    let roots = roots(project);
    if let Some(name) = import.imported.as_deref().filter(|n| *n != "*")
        && let Some(p) = find_module(&format!("{m}.{name}"), &roots, project)
    {
        return Some(p);
    }
    // `import a.b.c` binds `a`, but the dependency is on the deepest module that exists.
    let parts: Vec<&str> = m.split('.').collect();
    (1..=parts.len())
        .rev()
        .find_map(|i| find_module(&parts[..i].join("."), &roots, project))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cv_lang::{PathIndex, token};

    const SAMPLE: &str = r#"import os
import a.b as ab
from .events import bus, Kind as K
from ..core.models import *

@dataclass
class Store(Base, mixins.Saving):
    """Doc."""

    def put(self, x):
        self.flush(x)
        return helper(x)

    def flush(self, x):
        def inner():
            return 1
        ab.emit("é", 42)

def helper(x: int) -> int:
    return Store().put(x)

def test_it():
    assert helper(1)
"#;

    #[test]
    fn symbols_imports_and_refs() {
        let f = analyze(SAMPLE);
        let syms: Vec<(&str, &str, u32, u32)> = f
            .symbols
            .iter()
            .map(|s| (s.qualname.as_str(), s.kind.as_str(), s.start, s.end))
            .collect();
        assert_eq!(
            syms,
            vec![
                ("Store", "class", 6, 17),
                ("Store.put", "method", 10, 12),
                ("Store.flush", "method", 14, 17),
                ("helper", "function", 19, 20),
                ("test_it", "test", 22, 23),
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
                ("os", "os", None),
                ("ab", "a.b", None),
                ("bus", ".events", Some("bus")),
                ("K", ".events", Some("Kind")),
                ("", "..core.models", Some("*")),
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
        assert!(refs.contains(&("Base", None, "inherit", 7)));
        assert!(refs.contains(&("Saving", Some("mixins"), "inherit", 7)));
        assert!(refs.contains(&("flush", Some("self"), "call", 11)));
        assert!(refs.contains(&("helper", None, "call", 12)));
        assert!(refs.contains(&("emit", Some("ab"), "call", 17)));
        assert!(refs.contains(&("Store", None, "call", 20)));
        // The call inside `inner` is attributed to the enclosing method.
        let emit = f.refs.iter().find(|r| r.name == "emit").unwrap();
        assert_eq!(f.symbols[emit.enclosing.unwrap()].qualname, "Store.flush");
        assert_eq!(f.syntax_errors, 0);
        assert_eq!(f.tokens.len(), 23);
    }

    /// The generated anonymous-token block of `highlights.scm` matches the grammar (regenerate:
    /// `cargo test -p cv-lang-python anonymous -- --ignored --nocapture`).
    #[test]
    fn highlights_cover_every_anonymous_token() {
        let src = include_str!("../queries/highlights.scm");
        let want = expected_anonymous_block();
        assert_eq!(
            cv_lang_sitter::query::anonymous_block_in(src),
            Some(want.as_str())
        );
    }

    fn expected_anonymous_block() -> String {
        cv_lang_sitter::query::anonymous_block(
            &tree_sitter_python::LANGUAGE.into(),
            // Aliased two-keyword operators: their keywords are tokens of their own.
            &["is not", "not in"],
            cv_lang_sitter::anonymous_class,
        )
    }

    #[test]
    #[ignore]
    fn print_anonymous_block() {
        println!("{}", expected_anonymous_block());
    }

    #[test]
    fn tokens_use_utf16_columns() {
        let f = analyze("x = \"é😀\" + 1\n");
        // `"é😀"` starts at 4 and is 5 UTF-16 units long (quotes, é = 1, 😀 = 2).
        assert!(
            f.tokens[0].chunks(3).any(|t| t == [4, 5, token::STRING]),
            "{:?}",
            f.tokens[0]
        );
        assert!(
            f.tokens[0].chunks(3).any(|t| t == [12, 1, token::NUMBER]),
            "{:?}",
            f.tokens[0]
        );
    }

    #[test]
    fn resolves_absolute_relative_and_src_rooted_imports() {
        let paths = [
            "src/pkg/__init__.py",
            "src/pkg/events.py",
            "src/pkg/core/models.py",
            "src/pkg/core/sub/x.py",
            "lib/tool/pyproject.toml",
            "lib/tool/src/tool/api.py",
        ];
        let idx = PathIndex::new(paths.iter().copied(), |_| None);
        let imp = |module: &str, imported: Option<&str>| ImportFact {
            local: String::new(),
            module: module.into(),
            imported: imported.map(String::from),
            line: 1,
        };
        let from = "src/pkg/core/sub/x.py";
        assert_eq!(
            resolve(from, &imp("...events", Some("bus")), &idx).as_deref(),
            Some("src/pkg/events.py")
        );
        assert_eq!(
            resolve(from, &imp("..", Some("models")), &idx).as_deref(),
            Some("src/pkg/core/models.py")
        );
        assert_eq!(
            resolve(from, &imp("pkg.core.models", None), &idx).as_deref(),
            Some("src/pkg/core/models.py")
        );
        assert_eq!(
            resolve(from, &imp("pkg", Some("events")), &idx).as_deref(),
            Some("src/pkg/events.py")
        );
        assert_eq!(
            resolve(from, &imp("pkg.nothing", None), &idx).as_deref(),
            Some("src/pkg/__init__.py")
        );
        assert_eq!(
            resolve(from, &imp("tool.api", None), &idx).as_deref(),
            Some("lib/tool/src/tool/api.py")
        );
        assert_eq!(resolve(from, &imp("os", None), &idx), None);
    }
}
