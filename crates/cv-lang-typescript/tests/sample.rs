//! L2 acceptance on a hand-checked sample of files (WORKSTREAMS L1–L5).
//!
//! `tests/fixtures/` holds 25 files in two parts. `code-viewer/…` is 13 TS/TSX files copied verbatim
//! from this repo's `web/src`. `sample-app/…` is 12 TS/JS files of a small invoicing front end,
//! written for this test mainly to cover what the first part lacks: `.js` and `.ts` importing each
//! other, an ESM `.js` specifier, a `.d.ts` import, default and namespace imports, `export … from`,
//! `extends` on classes and interfaces, top-level `let`, arrow functions held by constants and by
//! object literals, `request(...)` paths as plain and template literals, and a non-ASCII line for
//! UTF-16 columns. Every file's symbols and imports (with their resolution) are frozen in
//! `tests/sample_typescript.expected`; each entry there was checked by reading the source, not just
//! snapshotted. `extra_paths.txt` lists the files outside the sample that the sampled imports point
//! at, so resolution sees them.
//!
//! Regenerate the rendering with `cargo test -p cv-lang-typescript --test sample -- --ignored dump
//! --nocapture` and re-check any diff by hand before updating the expectation.

use cv_lang::keys::{FoldTransform, SiteQuery};
use cv_lang::{LanguagePack, PathIndex, token};
use cv_lang_typescript::TypeScriptPack;
use std::collections::BTreeMap;
use std::path::Path;

fn root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
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
            } else if TypeScriptPack.claims(&p.to_string_lossy()) {
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
    let pack = TypeScriptPack;
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
        let f = TypeScriptPack.analyze(p, src);
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
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample_typescript.expected"),
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
            "sample-app/src/http/drafts.js",
            "createInvoice",
            None,
            "call",
            21,
            Some("submitDraftInvoice"),
        ),
        (
            "sample-app/src/http/client.ts",
            "authorizedFetch",
            None,
            "call",
            96,
            Some("sendWithRetries"),
        ),
        (
            "sample-app/src/http/invoices.ts",
            "assertFields",
            None,
            "call",
            163,
            Some("findInvoice"),
        ),
        (
            "code-viewer/web/src/data/live.ts",
            "token",
            None,
            "call",
            32,
            Some("Live.start"),
        ),
        (
            "code-viewer/web/src/canvas/edgePages.ts",
            "drawEdges",
            None,
            "call",
            83,
            Some("EdgePages.draw"),
        ),
        // Module-level side-effect call.
        (
            "sample-app/src/http/client.ts",
            "restoreSession",
            None,
            "call",
            6,
            None,
        ),
        // Local calls, a `new` expression, and calls on a member of an imported binding (inside a
        // nested arrow function) and of a namespace import.
        (
            "sample-app/src/http/client.ts",
            "sendWithRetries",
            None,
            "call",
            123,
            Some("request"),
        ),
        (
            "sample-app/src/http/client.ts",
            "HttpError",
            None,
            "call",
            98,
            Some("sendWithRetries"),
        ),
        (
            "sample-app/src/lib/overdueWatch.js",
            "getState",
            Some("useBillingStore"),
            "call",
            16,
            Some("watchOverdue"),
        ),
        (
            "sample-app/src/http/drafts.js",
            "toCents",
            Some("money"),
            "call",
            29,
            Some("toLineItem"),
        ),
        (
            "code-viewer/web/src/canvas/camera.test.ts",
            "Camera",
            None,
            "call",
            5,
            Some("cam"),
        ),
        // Calls through `this`, and through `this.x` (dotted qualifiers).
        (
            "code-viewer/web/src/canvas/tooltip.ts",
            "move",
            Some("this"),
            "call",
            17,
            Some("Tooltip.show"),
        ),
        (
            "code-viewer/web/src/canvas/edgePages.ts",
            "newPage",
            Some("this"),
            "call",
            73,
            Some("EdgePages.step"),
        ),
        (
            "code-viewer/web/src/canvas/edgePages.ts",
            "addChild",
            Some("this.root"),
            "call",
            90,
            Some("EdgePages.newPage"),
        ),
        // Class field initialisers belong to the class.
        (
            "code-viewer/web/src/canvas/edgePages.ts",
            "Container",
            None,
            "call",
            24,
            Some("EdgePages"),
        ),
        (
            "code-viewer/web/src/canvas/tooltip.ts",
            "createElement",
            Some("document"),
            "call",
            3,
            Some("Tooltip"),
        ),
        // Arrow functions in an object literal belong to the constant that holds them.
        (
            "sample-app/src/lib/audit.js",
            "record",
            None,
            "call",
            15,
            Some("audit"),
        ),
        (
            "sample-app/src/lib/audit.js",
            "info",
            Some("console"),
            "call",
            9,
            Some("record"),
        ),
        // A module-level arrow function is a function symbol and encloses its calls.
        (
            "sample-app/src/lib/overdueWatch.js",
            "filter",
            Some("invoices"),
            "call",
            5,
            Some("countOverdue"),
        ),
        // extends: classes and interfaces.
        (
            "sample-app/src/http/client.ts",
            "Error",
            None,
            "inherit",
            30,
            Some("HttpError"),
        ),
        (
            "sample-app/src/http/invoices.ts",
            "InvoiceSummary",
            None,
            "inherit",
            36,
            Some("InvoiceDetail"),
        ),
        (
            "sample-app/src/http/client.ts",
            "RequestInit",
            None,
            "inherit",
            21,
            Some("FetchOptions"),
        ),
        // Test bodies inside describe/it callbacks are module-level.
        (
            "code-viewer/web/src/canvas/camera.test.ts",
            "zoomAt",
            Some("c"),
            "call",
            34,
            None,
        ),
    ];
    for &(p, name, q, kind, line, enc) in want {
        let f = TypeScriptPack.analyze(p, &files[p]);
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

/// money.js line 19: `€` is 3 bytes but 1 UTF-16 unit, so the string `'€'` is 3 columns wide, the
/// `,` after it is at UTF-16 column 47 (byte 49) and the `{` at 61 (byte 63).
#[test]
fn columns_are_utf16_on_a_non_ascii_line() {
    let p = "sample-app/src/lib/money.js";
    let f = TypeScriptPack.analyze(p, &sample()[p]);
    use token::*;
    #[rustfmt::skip]
    let want = vec![
        0, 6, KEYWORD, 7, 8, KEYWORD, 16, 11, FUNCTION, 27, 1, PUNCTUATION, 33, 1, PUNCTUATION,
        42, 1, OPERATOR, 44, 3, STRING, 47, 1, PUNCTUATION, 55, 1, OPERATOR, 57, 2, STRING,
        59, 1, PUNCTUATION, 61, 1, PUNCTUATION,
    ];
    assert_eq!(f.tokens[18], want);
}

fn first_arg_of(names: &str) -> SiteQuery {
    SiteQuery {
        id: "first-arg".into(),
        query: format!(
            r#"(call_expression function: [(identifier) @_f (member_expression property: (property_identifier) @_f)]
                 (#match? @_f "^({names})$")
                 arguments: (arguments . (_) @key))"#
        ),
        hints: vec![],
    }
}

fn folded(path: &str, src: &str, query: SiteQuery) -> Vec<(u32, String)> {
    let x = TypeScriptPack
        .site_extractor(&[query], &[] as &[FoldTransform])
        .unwrap();
    x.extract(path, src)
        .sites
        .iter()
        .map(|s| (s.line, s.key.render()))
        .collect()
}

/// Key folding on the sample: string literals and template literals with plain substitutions,
/// including a member (`${query.page}`) and a module constant (the sample app's `request(...)`
/// calls, and the code panel's `fetch(...)`).
#[test]
fn folds_template_literals_in_sampled_files() {
    let files = sample();
    let p = "sample-app/src/http/invoices.ts";
    let got = folded(p, &files[p], first_arg_of("request"));
    let want: Vec<(u32, String)> = vec![
        (56, "/organization"),
        (66, "/invoices?status={status}"),
        (76, "/customers/{customerId}/invoices?page={query.page}"),
        (87, "/customers?q={q}&limit={SEARCH_LIMIT}"),
        (94, "/invoices"),
        (102, "/invoices/{invoiceId}/lines"),
        (110, "/invoices/{invoiceId}"),
        (117, "/invoices/{invoiceId}/send"),
        (128, "/invoices/remind-overdue"),
        (137, "/invoices/{invoiceId}/payments"),
        (145, "/tax-rates"),
        (160, "/invoices/{invoiceId}?expand={expand}"),
    ]
    .into_iter()
    .map(|(l, k)| (l, k.to_string()))
    .collect();
    assert_eq!(got, want);
    let p = "code-viewer/web/src/panels/code.tsx";
    let got = folded(p, &files[p], first_arg_of("fetch"));
    assert_eq!(
        got,
        vec![(
            14,
            "/api/v1/blobs/{blob}/text?from={from}&to={to}".to_string()
        )]
    );
}

/// Template literals with complex substitutions, `+` concatenation and module constants.
#[test]
fn folds_template_forms() {
    let src = r#"
const API = '/api';
export function f(id: string, a: { b: string }) {
  request(`/invoices/${encodeURIComponent(id)}/send`);
  request(API + '/x');
  request(`${API}/y/${a.b}`);
  request('/a' + `/${id}`);
  request(`/plain`);
  request(`\`tick\``);
}
"#;
    let want = [
        // A call in a substitution is an unknown hole (its source text is the hint).
        (4, "/invoices/{${encodeURIComponent(id)}}/send"),
        // Constants stay references for the channel core to resolve through imports.
        (5, "{API}/x"),
        (6, "{API}/y/{a.b}"),
        (7, "/a/{id}"),
        (8, "/plain"),
        (9, "`tick`"),
    ];
    let want: Vec<(u32, String)> = want.iter().map(|&(l, k)| (l, k.to_string())).collect();
    assert_eq!(folded("x.ts", src, first_arg_of("request")), want);
}
