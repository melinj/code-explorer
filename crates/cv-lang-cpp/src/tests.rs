use super::*;
use cv_lang::PathIndex;
use std::collections::HashMap;
use std::path::Path;

fn fixture() -> HashMap<String, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/geo");
    let mut out = HashMap::new();
    let mut stack = vec![root.clone()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read_to_string(&p).unwrap());
            }
        }
    }
    out
}

#[test]
#[ignore]
fn dump() {
    let files = fixture();
    let mut paths: Vec<&String> = files.keys().collect();
    paths.sort();
    let idx = PathIndex::new(files.keys().map(|s| s.as_str()), |p| files.get(p).cloned());
    let pack = CppPack::new();
    for p in paths {
        if !pack.claims(p) {
            continue;
        }
        let f = analyze(&files[p]);
        println!("== {p} errors={}", f.syntax_errors);
        for s in &f.symbols {
            println!(
                "  sym {:?} {} [{}] {}-{} @{} parent={:?}",
                s.kind, s.name, s.qualname, s.start, s.end, s.name_line, s.parent
            );
        }
        for r in &f.refs {
            println!(
                "  ref {:?} {} q={:?} L{} in={:?}",
                r.kind,
                r.name,
                r.qualifier,
                r.line,
                r.enclosing.map(|e| f.symbols[e].qualname.clone())
            );
        }
        for i in &f.imports {
            println!(
                "  imp {} -> {:?}",
                i.module,
                pack.resolve_import(p, i, &idx)
            );
        }
    }
}

fn project(
    files: &HashMap<String, String>,
) -> PathIndex<impl Fn(&str) -> Option<String> + Sync + '_> {
    PathIndex::new(files.keys().map(|s| s.as_str()), move |p| {
        files.get(p).cloned()
    })
}

fn syms(src: &str) -> Vec<(String, &'static str, u32, u32)> {
    analyze(src)
        .symbols
        .iter()
        .map(|s| (s.qualname.clone(), s.kind.as_str(), s.start, s.end))
        .collect()
}

fn owned(v: &[(&str, &'static str, u32, u32)]) -> Vec<(String, &'static str, u32, u32)> {
    v.iter()
        .map(|&(a, b, c, d)| (a.to_string(), b, c, d))
        .collect()
}

/// Every include of every fixture file, hand-checked: (file, as written, resolved).
#[test]
fn includes_resolve_relative_conventional_cmake_and_fallback() {
    let files = fixture();
    let idx = project(&files);
    let pack = CppPack::new();
    let mut got: Vec<(String, String, Option<String>)> = Vec::new();
    let mut paths: Vec<&String> = files.keys().filter(|p| pack.claims(p)).collect();
    paths.sort();
    assert!(paths.len() >= 20, "{} C++ files", paths.len());
    for p in paths {
        for i in analyze(&files[p]).imports {
            assert_eq!(i.imported.as_deref(), Some("*"));
            got.push((
                p.clone(),
                i.module.clone(),
                pack.resolve_import(p, &i, &idx),
            ));
        }
    }
    let want: &[(&str, &str, Option<&str>)] = &[
        ("examples/demo.c", "<stdio.h>", None),
        (
            "examples/demo.c",
            "../include/geo/version.h",
            Some("include/geo/version.h"),
        ),
        ("include/geo/canvas.hpp", "<memory>", None),
        ("include/geo/canvas.hpp", "<vector>", None),
        (
            "include/geo/canvas.hpp",
            "geo/shape.hpp",
            Some("include/geo/shape.hpp"),
        ),
        (
            "include/geo/circle.hpp",
            "geo/shape.hpp",
            Some("include/geo/shape.hpp"),
        ),
        (
            "include/geo/rect.hpp",
            "shape.hpp",
            Some("include/geo/shape.hpp"),
        ),
        ("include/geo/registry.hpp", "<map>", None),
        ("include/geo/registry.hpp", "<string>", None),
        ("include/geo/shape.hpp", "<string>", None),
        ("include/geo/util.hpp", "<cstdint>", None),
        (
            "src/canvas.cpp",
            "<geo/canvas.hpp>",
            Some("include/geo/canvas.hpp"),
        ),
        // Only through target_include_directories(${CMAKE_SOURCE_DIR}/third_party/tinyfmt/inc).
        (
            "src/canvas.cpp",
            "<tinyfmt/format.hpp>",
            Some("third_party/tinyfmt/inc/tinyfmt/format.hpp"),
        ),
        // Only through ${CMAKE_CURRENT_SOURCE_DIR}/detail.
        ("src/canvas.cpp", "clock.h", Some("src/detail/clock.h")),
        ("src/detail/clock.h", "<chrono>", None),
        ("src/detail/math.cpp", "math.h", Some("src/detail/math.h")),
        (
            "src/main.cpp",
            "<geo/canvas.hpp>",
            Some("include/geo/canvas.hpp"),
        ),
        (
            "src/main.cpp",
            "<geo/circle.hpp>",
            Some("include/geo/circle.hpp"),
        ),
        ("src/main.cpp", "<iostream>", None),
        (
            "src/main.cpp",
            "geo/version.h",
            Some("include/geo/version.h"),
        ),
        (
            "src/registry.cpp",
            "geo/registry.hpp",
            Some("include/geo/registry.hpp"),
        ),
        ("src/registry.cpp", "<string>", None),
        (
            "src/shapes.cpp",
            "geo/circle.hpp",
            Some("include/geo/circle.hpp"),
        ),
        (
            "src/shapes.cpp",
            "geo/rect.hpp",
            Some("include/geo/rect.hpp"),
        ),
        ("src/shapes.cpp", "detail/math.h", Some("src/detail/math.h")),
        ("src/util.cc", "geo/util.hpp", Some("include/geo/util.hpp")),
        (
            "tests/canvas_test.cpp",
            "<gtest/gtest.h>",
            Some("third_party/gtest_lite/gtest/gtest.h"),
        ),
        (
            "tests/canvas_test.cpp",
            "<geo/canvas.hpp>",
            Some("include/geo/canvas.hpp"),
        ),
        (
            "tests/canvas_test.cpp",
            "<geo/circle.hpp>",
            Some("include/geo/circle.hpp"),
        ),
        (
            "tests/registry_test.cpp",
            "gtest/gtest.h",
            Some("third_party/gtest_lite/gtest/gtest.h"),
        ),
        (
            "tests/registry_test.cpp",
            "geo/registry.hpp",
            Some("include/geo/registry.hpp"),
        ),
        (
            "tests/shapes_test.cpp",
            "<gtest/gtest.h>",
            Some("third_party/gtest_lite/gtest/gtest.h"),
        ),
        (
            "tests/shapes_test.cpp",
            "geo/circle.hpp",
            Some("include/geo/circle.hpp"),
        ),
        (
            "tests/shapes_test.cpp",
            "geo/rect.hpp",
            Some("include/geo/rect.hpp"),
        ),
        ("tests/units_test.cpp", "<catch2/catch.hpp>", None),
        (
            "tests/units_test.cpp",
            "geo/util.hpp",
            Some("include/geo/util.hpp"),
        ),
        (
            "third_party/tinyfmt/inc/tinyfmt/detail.hpp",
            "<string>",
            None,
        ),
        (
            "third_party/tinyfmt/inc/tinyfmt/format.hpp",
            "<cstdio>",
            None,
        ),
        (
            "third_party/tinyfmt/inc/tinyfmt/format.hpp",
            "<string>",
            None,
        ),
        (
            "third_party/tinyfmt/inc/tinyfmt/format.hpp",
            "detail.hpp",
            Some("third_party/tinyfmt/inc/tinyfmt/detail.hpp"),
        ),
    ];
    let want: Vec<(String, String, Option<String>)> = want
        .iter()
        .map(|(a, b, c)| (a.to_string(), b.to_string(), c.map(String::from)))
        .collect();
    assert_eq!(got, want);
}

#[test]
fn cmake_scrape_understands_variables_and_generator_expressions() {
    let files = fixture();
    let idx = project(&files);
    let dirs = scrape_cmake(idx.files_named("CMakeLists.txt"), &idx);
    assert_eq!(
        dirs,
        vec![
            "include",
            "include",
            "src/detail",
            "third_party/tinyfmt/inc",
            "third_party/gtest_lite"
        ]
        .into_iter()
        .fold(Vec::<String>::new(), |mut v, d| {
            if !v.iter().any(|x| x == d) {
                v.push(d.to_string());
            }
            v
        })
    );
    let args = cmake_include_args(
        "# include_directories(commented)\nINCLUDE_DIRECTORIES(BEFORE a \"b c\")\ntarget_include_directories(t INTERFACE $<INSTALL_INTERFACE:x> $<BUILD_INTERFACE:y>)\n",
    );
    assert_eq!(args, vec!["y", "a", "b", "c"]);
}

#[test]
fn angle_includes_never_resolve_relative_or_by_bare_name() {
    let paths = ["lib/a/string.h", "lib/a/x.cpp", "vendor/z/zlib/zconf.h"];
    let idx = PathIndex::new(paths.iter().copied(), |_| None);
    assert_eq!(resolve_with("lib/a/x.cpp", "<string.h>", &idx, &[]), None);
    assert_eq!(
        resolve_with("lib/a/x.cpp", "string.h", &idx, &[]).as_deref(),
        Some("lib/a/string.h")
    );
    // Unique trailing-path fallback for a path with a directory part.
    assert_eq!(
        resolve_with("lib/a/x.cpp", "<zlib/zconf.h>", &idx, &[]).as_deref(),
        Some("vendor/z/zlib/zconf.h")
    );
    assert_eq!(resolve_with("lib/a/x.cpp", "/abs/x.h", &idx, &[]), None);
}

#[test]
fn symbols_of_headers_sources_and_tests() {
    let files = fixture();
    let f = |p: &str| syms(&files[p]);
    assert_eq!(
        f("include/geo/shape.hpp"),
        owned(&[
            ("geo::Shape", "class", 6, 14),
            ("geo::Shape::~Shape", "method", 8, 8),
            ("geo::Shape::area", "method", 9, 9),
            ("geo::Shape::name", "method", 10, 10),
        ])
    );
    assert_eq!(
        f("include/geo/canvas.hpp"),
        owned(&[
            // The template line belongs to the function.
            ("geo::clamp", "function", 9, 12),
            ("geo::Canvas", "class", 14, 23),
            ("geo::Canvas::add", "method", 16, 16),
            ("geo::Canvas::total_area", "method", 17, 17),
            ("geo::Canvas::draw", "method", 18, 18),
            ("geo::Canvas::log", "method", 21, 21),
        ])
    );
    assert_eq!(
        f("include/geo/rect.hpp"),
        owned(&[
            ("geo::Size", "struct", 6, 9),
            ("geo::Rect", "class", 11, 15),
            ("geo::Rect::Rect", "method", 13, 13),
            ("geo::Rect::area", "method", 14, 14),
        ])
    );
    assert_eq!(
        f("include/geo/util.hpp"),
        owned(&[
            ("geo::util::Unit", "enum", 7, 7),
            ("geo::util::Bits", "struct", 9, 12),
            ("geo::util::Fixed", "struct", 14, 18),
            ("geo::util::Fixed::size", "method", 17, 17),
            ("geo::util::to_mm", "function", 20, 20),
            ("geo::util::popcount", "function", 21, 21),
        ])
    );
    assert_eq!(
        f("include/geo/registry.hpp"),
        owned(&[
            ("geo::reg::Registry", "class", 9, 16),
            ("geo::reg::Registry::put", "method", 11, 11),
            ("geo::reg::Registry::get", "method", 12, 12),
        ])
    );
    assert_eq!(
        f("include/geo/circle.hpp"),
        owned(&[
            ("geo::Circle", "class", 6, 14),
            ("geo::Circle::Circle", "method", 8, 8),
            ("geo::Circle::area", "method", 9, 9),
            ("geo::Circle::radius", "method", 10, 10),
        ])
    );
    assert!(f("include/geo/version.h").is_empty());
    // Out-of-line definitions: top-level in their namespace, qualified by class.
    assert_eq!(
        f("src/shapes.cpp"),
        owned(&[
            ("geo::Shape::name", "method", 7, 7),
            ("geo::Circle::area", "method", 9, 11),
            ("geo::Rect::Rect", "method", 13, 13),
            ("geo::Rect::area", "method", 15, 17),
        ])
    );
    assert_eq!(
        f("src/canvas.cpp"),
        owned(&[
            ("geo::Canvas::add", "method", 7, 10),
            ("geo::Canvas::total_area", "method", 12, 16),
            ("geo::Canvas::draw", "method", 18, 22),
            ("geo::Canvas::log", "method", 24, 26),
        ])
    );
    assert_eq!(
        f("src/registry.cpp"),
        owned(&[
            ("geo::reg::Registry::put", "method", 12, 14),
            ("geo::reg::Registry::get", "method", 16, 19),
        ])
    );
    assert_eq!(
        f("src/util.cc"),
        owned(&[
            ("geo::util::(anonymous)::factor", "function", 6, 13),
            ("geo::util::to_mm", "function", 16, 16),
            ("geo::util::popcount", "function", 18, 22),
        ])
    );
    assert_eq!(
        f("src/detail/math.h"),
        owned(&[
            ("geo::detail::square", "function", 5, 5),
            ("geo::detail::pi", "function", 7, 7),
        ])
    );
    assert_eq!(
        f("src/detail/math.cpp"),
        owned(&[("geo::detail::square", "function", 5, 5)])
    );
    assert_eq!(
        f("src/detail/clock.h"),
        owned(&[("geo::detail::now_ms", "function", 6, 9)])
    );
    assert_eq!(f("src/main.cpp"), owned(&[("main", "function", 7, 13)]));
    assert_eq!(
        f("examples/demo.c"),
        owned(&[
            ("point", "struct", 4, 6),
            ("dot", "function", 8, 8),
            ("main", "function", 10, 14),
        ])
    );
    assert_eq!(
        f("third_party/tinyfmt/inc/tinyfmt/format.hpp"),
        owned(&[
            ("tinyfmt::format", "function", 9, 12),
            ("tinyfmt::print", "function", 14, 14),
        ])
    );
    assert_eq!(
        f("third_party/tinyfmt/inc/tinyfmt/detail.hpp"),
        owned(&[("tinyfmt::detail::render", "function", 6, 8),])
    );
    assert_eq!(
        f("third_party/gtest_lite/gtest/gtest.h"),
        owned(&[
            ("testing::Test", "class", 9, 12),
            ("testing::Test::SetUp", "method", 11, 11),
        ])
    );
    // gtest and Catch2 test macros.
    assert_eq!(
        f("tests/shapes_test.cpp"),
        owned(&[
            ("Shapes.CircleArea", "test", 6, 9),
            ("Shapes.RectArea", "test", 11, 14),
        ])
    );
    assert_eq!(
        f("tests/canvas_test.cpp"),
        owned(&[
            ("CanvasTest", "class", 5, 9),
            ("CanvasTest::SetUp", "method", 7, 7),
            ("CanvasTest.TotalArea", "test", 11, 13),
        ])
    );
    assert_eq!(
        f("tests/registry_test.cpp"),
        owned(&[("Registry.PutGet", "test", 4, 8)])
    );
    assert_eq!(
        f("tests/units_test.cpp"),
        owned(&[
            ("units convert to millimetres", "test", 5, 7),
            ("bits count", "test", 9, 13),
        ])
    );
    // Catch2's `TEST_CASE("…") { … }` parses with a MISSING `;`; everything else is clean.
    for (p, src) in &files {
        if CppPack::new().claims(p) && !p.ends_with("units_test.cpp") {
            assert_eq!(analyze(src).syntax_errors, 0, "{p}");
        }
    }
}

type Ref = (String, Option<String>, &'static str, u32, Option<String>);

fn refs(src: &str) -> Vec<Ref> {
    let f = analyze(src);
    f.refs
        .iter()
        .map(|r| {
            (
                r.name.clone(),
                r.qualifier.clone(),
                r.kind.as_str(),
                r.line,
                r.enclosing.map(|e| f.symbols[e].qualname.clone()),
            )
        })
        .collect()
}

fn r(name: &str, q: Option<&str>, kind: &'static str, line: u32, enc: &str) -> Ref {
    (
        name.into(),
        q.map(String::from),
        kind,
        line,
        Some(enc.into()),
    )
}

#[test]
fn calls_methods_and_inheritance() {
    let files = fixture();
    let c = refs(&files["src/canvas.cpp"]);
    // `this->log()` inside an out-of-line method names its class, for cv-link.
    assert!(c.contains(&r("log", Some("Canvas"), "call", 9, "geo::Canvas::add")));
    assert!(c.contains(&r("area", Some("s"), "call", 14, "geo::Canvas::total_area")));
    assert!(c.contains(&r("clamp", None, "call", 15, "geo::Canvas::total_area")));
    assert!(c.contains(&r("now_ms", None, "call", 19, "geo::Canvas::draw")));
    assert!(c.contains(&r("format", None, "call", 20, "geo::Canvas::draw")));
    assert!(c.contains(&r("print", None, "call", 25, "geo::Canvas::log")));
    // A call on a call result is not a bare name.
    assert!(!c.iter().any(|x| x.0 == "c_str"));
    let s = refs(&files["src/shapes.cpp"]);
    assert_eq!(
        s,
        vec![
            r("pi", None, "call", 10, "geo::Circle::area"),
            r("square", None, "call", 10, "geo::Circle::area"),
        ]
    );
    let rect = refs(&files["include/geo/rect.hpp"]);
    assert_eq!(
        rect,
        vec![
            r("Shape", None, "inherit", 11, "geo::Rect"),
            r("Size", None, "inherit", 11, "geo::Rect"),
        ]
    );
    let t = refs(&files["tests/canvas_test.cpp"]);
    assert!(t.contains(&r("Test", None, "inherit", 5, "CanvasTest")));
    assert!(t.contains(&r(
        "total_area",
        Some("canvas"),
        "call",
        12,
        "CanvasTest.TotalArea"
    )));
    let u = refs(&files["tests/units_test.cpp"]);
    assert!(u.contains(&r("to_mm", None, "call", 6, "units convert to millimetres")));
    assert!(u.contains(&r("popcount", None, "call", 12, "bits count")));
    let util = refs(&files["src/util.cc"]);
    assert_eq!(
        util,
        vec![r("factor", None, "call", 16, "geo::util::to_mm")]
    );
    let demo = refs(&files["examples/demo.c"]);
    assert!(demo.contains(&r("dot", None, "call", 12, "main")));
    let fmt = refs(&files["third_party/tinyfmt/inc/tinyfmt/format.hpp"]);
    assert!(fmt.contains(&r("render", None, "call", 11, "tinyfmt::format")));

    let x = refs(
        "class A : public ns::B<int> {\n  void f() { this->g(); g(); new Widget(1); }\n  void g();\n};\n",
    );
    // Namespace-like scopes are dropped; std:: and class-like scopes stay.
    assert!(x.contains(&r("B", None, "inherit", 1, "A")));
    let y = refs("void f() { std::move(x); Circle::make(); geo::Circle::make(); }\n");
    assert!(y.contains(&r("move", Some("std"), "call", 1, "f")));
    assert!(y.contains(&r("make", Some("Circle"), "call", 1, "f")));
    assert!(y.contains(&r("make", Some("geo::Circle"), "call", 1, "f")));
    assert!(x.contains(&r("g", Some("this"), "call", 2, "A::f")));
    assert!(x.contains(&r("g", None, "call", 2, "A::f")));
    assert!(x.contains(&r("Widget", None, "call", 2, "A::f")));
}

#[test]
fn pack_claims_and_lanes() {
    let p = CppPack::new();
    for path in ["a.cpp", "a.cc", "a.CXX", "a.h", "a.hpp", "x/y.ipp", "a.c"] {
        assert!(p.claims(path), "{path}");
    }
    assert!(!p.claims("a.cs") && !p.claims("a.py"));
    assert_eq!(
        (p.lang("a.c"), p.lang("a.h"), p.lang("a.cc")),
        ("c", "cpp", "cpp")
    );
    assert_eq!(p.id(), "cpp");
    let mut reg = cv_lang::Registry::new();
    reg.register(Box::new(CppPack::new()));
    let lanes = cv_lang::lanes::LaneClassifier::new(&reg);
    assert_eq!(lanes.classify("src/geo/shape.cpp", b""), "code");
    assert_eq!(lanes.classify("src/geo/shape_test.cc", b""), "test");
    assert_eq!(lanes.classify("src/geo/test_shape.cpp", b""), "test");
    assert_eq!(lanes.classify("tests/whatever.hpp", b""), "test");
}

#[test]
fn tokens_use_utf16_columns() {
    let f = analyze("auto s = \"é😀\"; // ü\nint f() { return g(42); }\n");
    // `"é😀"` starts at 9 and is 5 UTF-16 units long (quotes, é = 1, 😀 = 2).
    assert!(
        f.tokens[0].chunks(3).any(|t| t == [9, 5, token::STRING]),
        "{:?}",
        f.tokens[0]
    );
    // The comment starts after `;` and a space: 9 + 5 + 2.
    assert!(
        f.tokens[0].chunks(3).any(|t| t == [16, 4, token::COMMENT]),
        "{:?}",
        f.tokens[0]
    );
    let l1 = &f.tokens[1];
    assert!(l1.chunks(3).any(|t| t == [0, 3, token::TYPE]), "{l1:?}");
    assert!(l1.chunks(3).any(|t| t == [4, 1, token::FUNCTION]), "{l1:?}");
    assert!(l1.chunks(3).any(|t| t == [10, 6, token::KEYWORD]), "{l1:?}");
    assert!(
        l1.chunks(3).any(|t| t == [17, 1, token::FUNCTION]),
        "{l1:?}"
    );
    assert!(l1.chunks(3).any(|t| t == [19, 2, token::NUMBER]), "{l1:?}");
    let pre = analyze("#include <vector>\n#define K \"x\"\n");
    assert!(
        pre.tokens[0].chunks(3).any(|t| t == [0, 8, token::KEYWORD]),
        "{:?}",
        pre.tokens[0]
    );
    assert!(
        pre.tokens[0].chunks(3).any(|t| t == [9, 8, token::STRING]),
        "{:?}",
        pre.tokens[0]
    );
    assert!(
        pre.tokens[1]
            .chunks(3)
            .any(|t| t == [8, 1, token::CONSTANT]),
        "{:?}",
        pre.tokens[1]
    );
    assert_eq!(f.tokens.len(), 2);
}

#[test]
fn key_folding_handles_cpp_string_forms() {
    use KeyPart::*;
    let lit = |s: &str| Lit(s.to_string());
    let src = r#"#define TOPIC_ROOT "world" "."
constexpr const char* kPrefix = "geo/";
static const std::string kSuffix("/items");
void f(const std::string& id, int n) {
  bus.emit("a\tb\x41é");
  bus.emit("world." "entity." "moved");
  bus.emit(TOPIC_ROOT "turn");
  bus.emit(std::string(kPrefix) + id + kSuffix);
  bus.emit(R"x(raw/"path")x");
  bus.emit(u8"utf8/" + std::to_string(n));
  bus.emit(id);
  bus.emit("é😀/" + id);
  bus.emit("lit"s);
}
"#;
    let at = |line: u32, needle: &str| {
        let l = src.lines().nth(line as usize - 1).unwrap();
        let byte = l.find(needle).unwrap();
        let col = l[..byte].encode_utf16().count() as u32;
        fold_key(src, line, col).unwrap()
    };
    assert_eq!(at(5, "\"a"), vec![lit("a\tbAé")]);
    assert_eq!(at(6, "\"world"), vec![lit("world.entity.moved")]);
    assert_eq!(at(7, "TOPIC_ROOT"), vec![lit("world.turn")]);
    assert_eq!(
        at(8, "std::string"),
        vec![lit("geo/"), Segment, lit("/items")]
    );
    assert_eq!(at(9, "R\""), vec![lit("raw/\"path\"")]);
    assert_eq!(at(10, "u8"), vec![lit("utf8/"), Segment]);
    assert_eq!(at(11, "id)"), vec![Unknown("id".into())]);
    assert_eq!(at(12, "\"é"), vec![lit("é😀/"), Segment]);
    assert_eq!(at(13, "\"lit"), vec![lit("lit")]);
    // A position inside a literal folds the expression that holds it.
    let l6 = src.lines().nth(5).unwrap();
    let col = l6.find("entity").unwrap() as u32;
    assert_eq!(
        fold_key(src, 6, col).unwrap(),
        vec![lit("world.entity.moved")]
    );
    assert_eq!(fold_key(src, 99, 0), None);
}

/// Median analysis time per 1k lines: a regression guard (loose bounds). The WORKSTREAMS budget is on
/// the grammar parse, where C++ is an accepted exception (tree-sitter-cpp parses at ~7 ms per 1k lines).
#[test]
fn parse_speed() {
    let files = fixture();
    let mut keys: Vec<&String> = files.keys().filter(|p| CppPack::new().claims(p)).collect();
    keys.sort();
    let one: String = keys
        .iter()
        .map(|k| files[*k].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let mut src = String::new();
    while src.lines().count() < 2000 {
        src.push_str(&one);
        src.push('\n');
    }
    let lines = src.lines().count() as f64;
    let mut times: Vec<f64> = (0..15)
        .map(|_| {
            let (t, cpu) = (std::time::Instant::now(), thread_cpu_ms());
            let f = analyze(&src);
            assert!(!f.symbols.is_empty());
            // CPU time of this thread when the OS reports it (a loaded machine skews wall time).
            let ms = match (cpu, thread_cpu_ms()) {
                (Some(a), Some(b)) => b - a,
                _ => t.elapsed().as_secs_f64() * 1000.0,
            };
            ms / (lines / 1000.0)
        })
        .collect();
    times.sort_by(f64::total_cmp);
    let median = times[times.len() / 2];
    println!(
        "cpp parse: median {median:.2}, best {:.2} ms per 1k lines ({lines} lines)",
        times[0]
    );
    // Loose bounds: a loaded CI box must not make this flaky (budgets: `examples/parse_speed`).
    let bound = if cfg!(debug_assertions) { 100.0 } else { 25.0 };
    assert!(median < bound, "{median} ms per 1k lines");
}

/// On-CPU time of the current thread in ms (Linux `schedstat`), if available.
fn thread_cpu_ms() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/thread-self/schedstat").ok()?;
    let ns: f64 = s.split_whitespace().next()?.parse().ok()?;
    Some(ns / 1e6)
}

/// End to end with cv-link: include resolution + star bindings + top-level namespace members.
#[test]
fn links_resolve_through_includes() {
    let files = fixture();
    let idx = project(&files);
    let pack = CppPack::new();
    let mut paths: Vec<&String> = files.keys().filter(|p| pack.claims(p)).collect();
    paths.sort();
    let facts: Vec<BlobFacts> = paths.iter().map(|p| analyze(&files[*p])).collect();
    let pos = |p: &str| paths.iter().position(|x| *x == p);
    let mut next = 0;
    let mut owner = Vec::new();
    let inputs: Vec<cv_link::FileIn> = paths
        .iter()
        .zip(&facts)
        .map(|(p, f)| {
            let mut ws = cv_lang::chunk::chunk(f.lines, &f.symbols);
            cv_lang::chunk::assign_keys(&mut ws, &f.symbols);
            let windows = ws
                .iter()
                .map(|w| {
                    owner.push(p.as_str());
                    next += 1;
                    (w.start, w.end, next - 1)
                })
                .collect();
            cv_link::FileIn {
                path: p,
                lane: "code",
                lang: Some("cpp"),
                conv: pack.conventions(),
                facts: Some(f),
                imports: f
                    .imports
                    .iter()
                    .map(|i| pack.resolve_import(p, i, &idx).and_then(|t| pos(&t)))
                    .collect(),
                windows,
            }
        })
        .collect();
    let links = cv_link::links(&inputs);
    let got: Vec<(&str, &str, &str, &str)> = links
        .iter()
        .map(|l| {
            (
                owner[l.from_window],
                owner[l.to_window],
                l.kind.as_str(),
                l.label.as_str(),
            )
        })
        .collect();
    for want in [
        (
            "src/canvas.cpp",
            "src/detail/clock.h",
            "call",
            "geo::detail::now_ms",
        ),
        (
            "src/canvas.cpp",
            "include/geo/canvas.hpp",
            "call",
            "geo::clamp",
        ),
        (
            "src/canvas.cpp",
            "third_party/tinyfmt/inc/tinyfmt/format.hpp",
            "call",
            "tinyfmt::format",
        ),
        (
            "src/shapes.cpp",
            "src/detail/math.h",
            "call",
            "geo::detail::pi",
        ),
        (
            "include/geo/circle.hpp",
            "include/geo/shape.hpp",
            "inherit",
            "geo::Shape",
        ),
        (
            "tests/units_test.cpp",
            "include/geo/util.hpp",
            "call",
            "geo::util::to_mm",
        ),
        (
            "tests/canvas_test.cpp",
            "third_party/gtest_lite/gtest/gtest.h",
            "inherit",
            "testing::Test",
        ),
    ] {
        assert!(got.contains(&want), "missing {want:?} in {got:#?}");
    }
    // Calls into std never link.
    assert!(!got.iter().any(|g| g.3 == "move" || g.3 == "make_unique"));
}

/// The generated anonymous-token block of `highlights.scm` matches the grammar (regenerate:
/// `cargo test -p cv-lang-cpp anonymous -- --ignored --nocapture`). Preprocessor directives
/// (`#include`, `#define`, …) are keywords.
#[test]
fn highlights_cover_every_anonymous_token() {
    let src = include_str!("../queries/highlights.scm");
    let want = anonymous_block();
    assert_eq!(
        cv_lang_sitter::query::anonymous_block_in(src),
        Some(want.as_str())
    );
}

fn anonymous_block() -> String {
    cv_lang_sitter::query::anonymous_block(&tree_sitter_cpp::LANGUAGE.into(), &[], |k| {
        if k.starts_with('#') {
            token::KEYWORD
        } else {
            cv_lang_sitter::anonymous_class(k)
        }
    })
}

#[test]
#[ignore]
fn print_anonymous_block() {
    println!("{}", anonymous_block());
}
