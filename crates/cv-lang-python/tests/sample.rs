//! L1 acceptance on a hand-checked sample of Python files (WORKSTREAMS L1–L5).
//!
//! `tests/fixtures/sample_app/` holds 24 synthetic Python files, a small bookshop service
//! (`src/catalog`, `src/billing`, `src/access`, `src/telemetry`, `src/storefront`) written for
//! this test to cover what it checks: classes, methods, decorated and nested definitions;
//! absolute `src.…`, relative, aliased, function-local, `TYPE_CHECKING` and unresolvable imports;
//! the ref shapes listed below; UTF-16 columns on a non-ASCII line; and f-string key folding.
//! Every file's symbols and imports (with their resolution) are frozen in
//! `tests/sample_python.expected`; each entry there was checked by reading the source, not just
//! snapshotted. `extra_paths.txt` lists the files outside the sample that the sampled imports
//! point at, so resolution sees them.
//!
//! Regenerate the rendering with `cargo test -p cv-lang-python --test sample -- --ignored dump
//! --nocapture` and re-check any diff by hand before updating the expectation.

use cv_lang::keys::{FoldTransform, SiteQuery};
use cv_lang::{LanguagePack, PathIndex, token};
use cv_lang_python::PythonPack;
use std::collections::BTreeMap;
use std::path::Path;

fn root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_app")
}

fn sample() -> BTreeMap<String, String> {
    let root = root();
    let mut out = BTreeMap::new();
    let mut stack = vec![root.clone()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "py") {
                let rel = p
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.insert(rel, std::fs::read_to_string(&p).unwrap());
            }
        }
    }
    out
}

fn render(files: &BTreeMap<String, String>) -> String {
    let extra = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extra_paths.txt"),
    )
    .unwrap();
    let paths: Vec<&str> = files
        .keys()
        .map(|s| s.as_str())
        .chain(extra.lines().filter(|l| !l.trim().is_empty()))
        .collect();
    let idx = PathIndex::new(paths, |p| files.get(p).cloned());
    let pack = PythonPack;
    let mut s = String::new();
    for (p, src) in files {
        assert!(pack.claims(p));
        let f = pack.analyze(p, src);
        s.push_str(&format!(
            "== {p} lines={} errors={}\n",
            f.lines, f.syntax_errors
        ));
        for y in &f.symbols {
            s.push_str(&format!(
                "sym {} {} {}-{}\n",
                y.kind.as_str(),
                y.qualname,
                y.start,
                y.end
            ));
        }
        for i in &f.imports {
            s.push_str(&format!(
                "imp {} {}{} as {:?} -> {}\n",
                i.line,
                i.module,
                i.imported
                    .as_deref()
                    .map(|m| format!(" : {m}"))
                    .unwrap_or_default(),
                i.local,
                pack.resolve_import(p, i, &idx)
                    .unwrap_or_else(|| "-".into())
            ));
        }
    }
    s
}

#[test]
#[ignore]
fn dump() {
    let files = sample();
    print!("{}", render(&files));
    for (p, src) in &files {
        let f = PythonPack.analyze(p, src);
        println!("== {p}");
        for r in &f.refs {
            println!(
                "  ref {} {} q={:?} L{} in={:?}",
                r.kind.as_str(),
                r.name,
                r.qualifier,
                r.line,
                r.enclosing.map(|e| f.symbols[e].qualname.clone())
            );
        }
    }
}

#[test]
fn sample_has_at_least_20_files() {
    assert!(sample().len() >= 20, "{}", sample().len());
}

#[test]
fn symbols_and_imports_match_hand_checked_expectation() {
    let want = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample_python.expected"),
    )
    .unwrap();
    let want: String = want
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| format!("{l}\n"))
        .collect();
    let got = render(&sample());
    if got != want {
        for (i, (g, w)) in got.lines().zip(want.lines()).enumerate() {
            if g != w {
                panic!("first difference at line {}:\n got: {g}\nwant: {w}", i + 1);
            }
        }
        panic!(
            "length differs: got {} lines, want {}",
            got.lines().count(),
            want.lines().count()
        );
    }
}

/// (file, ref name, qualifier, kind, line, enclosing symbol), each read off the source.
type RefRow = (
    &'static str,
    &'static str,
    Option<&'static str>,
    &'static str,
    u32,
    Option<&'static str>,
);

#[test]
fn selected_refs_are_found_with_their_enclosing_symbol() {
    let files = sample();
    let want: &[RefRow] = &[
        // Calls into imported functions, attributed to the enclosing function.
        (
            "src/catalog/search.py",
            "split_query",
            None,
            "call",
            22,
            Some("search_titles"),
        ),
        (
            "src/storefront/checkout.py",
            "allow",
            None,
            "call",
            16,
            Some("checkout"),
        ),
        (
            "src/billing/invoices.py",
            "post_entry",
            None,
            "call",
            63,
            Some("build_invoice"),
        ),
        (
            "src/telemetry/exporter.py",
            "encode_batch",
            None,
            "call",
            60,
            Some("Exporter.flush"),
        ),
        (
            "src/access/api_keys.py",
            "compare_digest",
            Some("hmac"),
            "call",
            25,
            Some("verify"),
        ),
        // A def inside a module-level `except` is a top-level symbol and owns its calls.
        (
            "src/telemetry/__init__.py",
            "nullcontext",
            None,
            "call",
            13,
            Some("span"),
        ),
        // A qualified call that shares its enclosing function's name.
        (
            "src/telemetry/encoding.py",
            "dumps",
            Some("_json"),
            "call",
            18,
            Some("dumps"),
        ),
        (
            "src/telemetry/encoding.py",
            "dumps",
            None,
            "call",
            23,
            Some("encode_batch"),
        ),
        // Function-local imports are still imports; their calls belong to the function.
        (
            "src/telemetry/exporter.py",
            "HttpSink",
            None,
            "call",
            24,
            Some("default_sink"),
        ),
        // Method calls on self, and a call inside a nested closure attributed to the method.
        (
            "src/telemetry/counters.py",
            "_key",
            Some("self"),
            "call",
            24,
            Some("CounterRegistry.increment"),
        ),
        (
            "src/telemetry/counters.py",
            "increment",
            Some("self"),
            "call",
            33,
            Some("CounterRegistry.bound"),
        ),
        (
            "src/telemetry/exporter.py",
            "default_sink",
            None,
            "call",
            36,
            Some("Exporter.__init__"),
        ),
        (
            "src/telemetry/exporter.py",
            "warning",
            Some("logger"),
            "call",
            62,
            Some("Exporter.flush"),
        ),
        (
            "src/billing/currency.py",
            "Decimal",
            None,
            "call",
            35,
            Some("Money.scaled"),
        ),
        // Calls in closures nested two deep in a function (and their decorator) belong to it.
        (
            "src/telemetry/timing.py",
            "wraps",
            Some("functools"),
            "call",
            42,
            Some("timed"),
        ),
        (
            "src/telemetry/timing.py",
            "Stopwatch",
            None,
            "call",
            45,
            Some("timed"),
        ),
        // Inheritance, including multiple bases, stdlib bases and a qualified base.
        (
            "src/catalog/models.py",
            "str",
            None,
            "inherit",
            19,
            Some("Binding"),
        ),
        (
            "src/catalog/models.py",
            "Enum",
            None,
            "inherit",
            19,
            Some("Binding"),
        ),
        (
            "src/access/identity.py",
            "BaseModel",
            None,
            "inherit",
            11,
            Some("Customer"),
        ),
        (
            "src/access/roles.py",
            "StrEnum",
            None,
            "inherit",
            7,
            Some("Role"),
        ),
        (
            "src/access/throttle.py",
            "Protocol",
            None,
            "inherit",
            19,
            Some("BucketStore"),
        ),
        (
            "src/billing/discounts.py",
            "ABC",
            Some("abc"),
            "inherit",
            12,
            Some("DiscountRule"),
        ),
        (
            "src/billing/discounts.py",
            "DiscountRule",
            None,
            "inherit",
            27,
            Some("PercentOff"),
        ),
        // Class-body statements belong to the class; module-level calls to nothing.
        (
            "src/access/identity.py",
            "ConfigDict",
            None,
            "call",
            14,
            Some("Customer"),
        ),
        (
            "src/catalog/models.py",
            "field",
            None,
            "call",
            53,
            Some("Book"),
        ),
        (
            "src/access/session.py",
            "ContextVar",
            None,
            "call",
            14,
            None,
        ),
        (
            "src/catalog/isbn.py",
            "compile",
            Some("re"),
            "call",
            5,
            None,
        ),
        (
            "src/catalog/query_tags.py",
            "getLogger",
            Some("logging"),
            "call",
            11,
            None,
        ),
        (
            "src/access/throttle.py",
            "MemoryBucketStore",
            None,
            "call",
            40,
            None,
        ),
        // Dotted qualifiers.
        (
            "src/access/throttle.py",
            "get",
            Some("self._buckets"),
            "call",
            34,
            Some("MemoryBucketStore.load"),
        ),
        (
            "src/access/session.py",
            "get",
            Some("request.headers"),
            "call",
            38,
            Some("staff_roles"),
        ),
        (
            "src/telemetry/exporter.py",
            "get",
            Some("os.environ"),
            "call",
            24,
            Some("default_sink"),
        ),
        (
            "src/access/session.py",
            "parse_list",
            Some("Role"),
            "call",
            39,
            Some("staff_roles"),
        ),
        (
            "src/access/throttle.py",
            "load",
            Some("_store"),
            "call",
            52,
            Some("allow"),
        ),
        (
            "src/access/throttle.py",
            "Bucket",
            None,
            "call",
            52,
            Some("allow"),
        ),
        // Module-qualified calls: a submodule from `from pkg import mod`, an `import a.b.c as X`
        // alias, and a module object returned by importlib.
        (
            "src/billing/invoices.py",
            "rate_for",
            Some("tax"),
            "call",
            56,
            Some("build_invoice"),
        ),
        (
            "src/catalog/onix.py",
            "fromstring",
            Some("ET"),
            "call",
            26,
            Some("read_editions"),
        ),
        (
            "src/storefront/__init__.py",
            "provider",
            Some("module"),
            "call",
            18,
            Some("load_providers"),
        ),
    ];
    for &(p, name, q, kind, line, enc) in want {
        let f = PythonPack.analyze(p, &files[p]);
        let hit = f.refs.iter().find(|r| {
            r.name == name
                && r.qualifier.as_deref() == q
                && r.kind.as_str() == kind
                && r.line == line
        });
        let Some(r) = hit else {
            panic!("{p}: no {kind} ref {q:?}.{name} on line {line}");
        };
        assert_eq!(
            r.enclosing.map(|e| f.symbols[e].qualname.as_str()),
            enc,
            "{p}:{line} {name}"
        );
    }
}

/// `raise LookupError("no customer for this request — is …")`: the em dash is 3 bytes but 1 UTF-16
/// unit, so the string is 64 UTF-16 units long (66 bytes) and the closing `)` sits at UTF-16
/// column 90 (byte 92).
#[test]
fn columns_are_utf16_on_a_non_ascii_line() {
    let files = sample();
    let f = PythonPack.analyze("src/access/session.py", &files["src/access/session.py"]);
    let line32: Vec<&[u32]> = f.tokens[31].chunks(3).collect();
    assert!(line32.contains(&&[26, 64, token::STRING][..]), "{line32:?}");
    assert!(line32.iter().any(|t| t[0] == 90), "{line32:?}");
    assert!(!line32.iter().any(|t| t[0] == 92), "{line32:?}");
}

fn q(id: &str, query: &str) -> SiteQuery {
    SiteQuery {
        id: id.into(),
        query: query.into(),
        hints: vec![],
    }
}

fn first_arg_of(names: &str) -> SiteQuery {
    q(
        "first-arg",
        &format!(
            r#"(call function: [(identifier) @_f (attribute attribute: (identifier) @_f)]
                 (#match? @_f "^({names})$")
                 arguments: (argument_list . (_) @key))"#
        ),
    )
}

fn folded(src: &str, query: SiteQuery) -> Vec<(u32, String)> {
    let x = PythonPack
        .site_extractor(&[query], &[] as &[FoldTransform])
        .unwrap();
    x.extract("x.py", src)
        .sites
        .iter()
        .map(|s| (s.line, s.key.render()))
        .collect()
}

/// Key folding on the sample: f-strings with plain, attribute and complex interpolations, a `!r`
/// conversion, and a plain string as the first argument.
#[test]
fn folds_fstrings_in_sampled_files() {
    let files = sample();
    let got = folded(&files["src/catalog/query_tags.py"], first_arg_of("warning"));
    assert_eq!(
        got,
        vec![
            (
                34,
                "several lang: tags in one query; searching {filters.language}".to_string()
            ),
            (
                41,
                "year range runs backwards, swapping it: {start}-{end}".to_string()
            ),
            (
                46,
                "ignoring a year tag that is not a year: {{unreadable[-1].group(0)}}".to_string()
            ),
        ]
    );
    let got = folded(
        &files["src/storefront/checkout.py"],
        first_arg_of("CheckoutRejected"),
    );
    assert_eq!(
        got,
        vec![
            (17, "too_many_checkouts".to_string()),
            (19, "empty_basket".to_string())
        ]
    );
    let got = folded(&files["src/catalog/models.py"], first_arg_of("ValueError"));
    // The `!r` conversion does not change what is interpolated.
    assert_eq!(
        got,
        vec![(33, "unrecognised binding label: {label}".to_string())]
    );
}

/// The Python string forms the folder must handle: literals, f-strings, `.format`, `%`, `+`.
#[test]
fn folds_format_percent_and_concatenation() {
    let src = r#"
PREFIX = "world"
def f(kind, x, y):
    emit("a.{}.c".format(kind))
    emit("{kind}.moved.{0}".format(x, kind=kind))
    emit("{{literal}}.{}".format(x))
    emit("a.%s.%d" % (x, y))
    emit("rate=100%%.%(name)s" % {"name": x})
    emit(PREFIX + ".entity." + kind)
    emit(f"{PREFIX}.{kind}")
    emit(f"{{{kind}}}")
    emit("plain" "joined")
"#;
    assert_eq!(
        folded(src, first_arg_of("emit")),
        vec![
            (4, "a.{}.c".to_string()),
            (5, "{}.moved.{}".to_string()),
            (6, "{literal}.{}".to_string()),
            (7, "a.{}.{}".to_string()),
            (8, "rate=100%.{}".to_string()),
            (9, "{PREFIX}.entity.{kind}".to_string()),
            (10, "{PREFIX}.{kind}".to_string()),
            (11, "{{kind}}".to_string()), // `{` + {kind} + `}`
            (12, "plainjoined".to_string()),
        ]
    );
}
