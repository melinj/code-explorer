//! Range filesets are jj filesets parsed by jj-lib (paths with spaces, root-file:, globs, ~).
use cv_vcs::Fileset;

fn m(f: &str, p: &str) -> bool {
    Fileset::parse(Some(f)).unwrap().matches(p)
}

#[test]
fn jj_fileset_syntax() {
    assert!(Fileset::parse(None).unwrap().matches("any/thing.py"));
    assert!(Fileset::parse(Some("  ")).unwrap().matches("x"));
    // Bare prefix, relative to the repo root.
    assert!(m("src/", "src/a.py") && m("src", "src/a/b.py") && !m("src", "srcx/a.py"));
    // Paths with spaces: quoted, or root-file: as Compare builds them.
    assert!(m(r#""my dir/a b.py""#, "my dir/a b.py"));
    assert!(m(r#"root-file:"my dir/a b.py" | root-file:"c.py""#, "c.py"));
    assert!(!m(r#"root-file:"my dir/a b.py""#, "my dir/a.py"));
    assert!(m(r#"glob:"**/*.py""#, "x/y/z.py") && !m(r#"glob:"**/*.py""#, "x/y/z.rs"));
    assert!(m("src ~ src/gen", "src/a.py") && !m("src ~ src/gen", "src/gen/a.py"));
    assert!(m("all()", "anything"));
    assert!(Fileset::parse(Some("src/ (")).is_err());
}
