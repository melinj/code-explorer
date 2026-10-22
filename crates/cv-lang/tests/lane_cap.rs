//! DESIGN §4.3: at most 32 new lanes are read from `.cv/lanes.toml`; later new lanes are skipped
//! with a warning, while built-in ids past the cap still apply.

use cv_lang::Registry;
use cv_lang::lanes::{LANES, LaneClassifier, LaneConfig};

#[test]
fn at_most_32_new_lanes_are_read() {
    let mut toml = String::new();
    for i in 0..40 {
        toml.push_str(&format!(
            "[[lane]]\nid = \"l{i:02}\"\nglobs = [\"l{i:02}/**\"]\n\n"
        ));
        if i == 35 {
            // A built-in id after the cap still counts: it is not a new lane.
            toml.push_str("[[lane]]\nid = \"docs\"\nlabel = \"Notes\"\nglobs = [\"notes/**\"]\n\n");
        }
    }
    let cfg = LaneConfig::parse(&toml);

    let new: Vec<&str> = cfg
        .lanes
        .iter()
        .map(|l| l.id)
        .filter(|id| !LANES.iter().any(|b| b.id == *id))
        .collect();
    let expected: Vec<String> = (0..32).map(|i| format!("l{i:02}")).collect();
    assert_eq!(new, expected);
    assert_eq!(cfg.lanes.len(), LANES.len() + 32);

    // One warning per skipped lane, naming it.
    let capped: Vec<&String> = cfg
        .warnings
        .iter()
        .filter(|w| w.contains("new lanes"))
        .collect();
    assert_eq!(capped.len(), 8, "{:?}", cfg.warnings);
    for i in 32..40 {
        assert!(
            capped.iter().any(|w| w.contains(&format!("`l{i:02}`"))),
            "no warning for l{i:02}: {capped:?}"
        );
    }

    // Accepted lanes classify, skipped lanes' globs do not, the built-in override still applies.
    let reg = Registry::new();
    let c = LaneClassifier::with_config(&reg, &cfg);
    assert_eq!(c.classify("l31/a.txt", b""), "l31");
    assert_ne!(c.classify("l32/a.txt", b""), "l32");
    assert_eq!(c.classify("notes/a.txt", b""), "docs");
    let docs = cfg.lanes.iter().find(|l| l.id == "docs").unwrap();
    assert_eq!(docs.label, "Notes");
}
