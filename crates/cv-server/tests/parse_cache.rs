//! B3 parse cache: a second parse of the same files reuses every cached result and yields the
//! same facts; a changed blob or a file of another pack is parsed again.

use cv_server::parse_cache::{self, Item};

const PY: &str =
    "import os\n\n\nclass Bus:\n    def emit(self, topic):\n        return os.getenv(topic)\n";
const TS: &str = "export function f(x: number) {\n  return `a/${x}`;\n}\n";
const YAML: &str = "a:\n  b: 1\n  c: >\n    folded\n";

#[test]
fn second_parse_reuses_the_cache() {
    let tmp = tempfile::tempdir().unwrap();
    parse_cache::enable_in(tmp.path());
    let reg = cv_server::index::registry();
    let root = tmp.path().join("repo");
    let items = [
        Item {
            path: "src/bus.py",
            blob: "b1",
            text: PY,
        },
        Item {
            path: "web/f.ts",
            blob: "b2",
            text: TS,
        },
        Item {
            path: "conf/a.yaml",
            blob: "b3",
            text: YAML,
        },
        Item {
            path: "README.unknown",
            blob: "b4",
            text: "plain\n",
        },
    ];
    let (first, s1) = parse_cache::parse_all(&reg, &root, &items, &|| {});
    assert_eq!((s1.hits, s1.parsed), (0, 3));
    let (second, s2) = parse_cache::parse_all(&reg, &root, &items, &|| {});
    assert_eq!((s2.hits, s2.parsed), (3, 0));
    assert_eq!(format!("{first:?}"), format!("{second:?}"));
    assert!(!first[0].1.symbols.is_empty());
    // A new blob at the same path is parsed again.
    let changed = [Item {
        path: "src/bus.py",
        blob: "b9",
        text: "def g():\n    pass\n",
    }];
    let (third, s3) = parse_cache::parse_all(&reg, &root, &changed, &|| {});
    assert_eq!((s3.hits, s3.parsed), (0, 1));
    assert_eq!(third[0].1.symbols[0].name, "g");
    parse_cache::disable();
}
