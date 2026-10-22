//! DESIGN §4.10.6: stored window pins, cluster pins and cluster labels whose target is gone count
//! consecutive full indexes without a match and are dropped after `MAX_MISSES` (20); a match
//! resets the count. Drives `layout_api::track_drift` on a real index of a scripted jj repo.
//! Skipped when `jj` is not on PATH.

use std::path::Path;
use std::process::Command;

use cv_server::index::{self, Options};
use cv_server::layout_api::{ClusterLabel, ClusterPin, LayoutStore, MAX_MISSES, track_drift};

fn jj(dir: &Path, args: &[&str]) {
    let out = Command::new("jj")
        .args(args)
        .current_dir(dir)
        .env("JJ_CONFIG", dir.join("../jjconfig.toml"))
        .env("JJ_USER", "Test")
        .env("JJ_EMAIL", "test@example.com")
        .output()
        .expect("run jj");
    assert!(
        out.status.success(),
        "jj {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn unmatched_pins_and_labels_drop_after_20_misses() {
    if !Command::new("jj")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("jj not found; skipping");
        return;
    }
    assert_eq!(MAX_MISSES, 20);
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("jjconfig.toml"), "").unwrap();
    let repo_path = tmp.path().join("repo");
    std::fs::create_dir_all(repo_path.join("src")).unwrap();
    jj(&repo_path, &["git", "init"]);
    for i in 0..3 {
        std::fs::write(
            repo_path.join(format!("src/m{i}.py")),
            format!("def f{i}(x):\n    return x + {i}\n"),
        )
        .unwrap();
    }
    jj(&repo_path, &["commit", "-m", "init"]);
    let repo = cv_vcs::JjRepo::open(&repo_path).unwrap();
    let head = repo.resolve_one("@").unwrap();
    let v = index::build_view(
        &repo,
        &index::registry(),
        serde_json::json!({ "mode": "snapshot", "rev": "@" }),
        &head,
        Options { max_commits: 50 },
    )
    .unwrap();
    let live = v.windows[0].header.key.clone();
    let cluster = &v.bootstrap.base.clusters[0];
    let members: Vec<String> = {
        let mut k: Vec<String> = v
            .windows
            .iter()
            .filter(|w| w.header.cluster == cluster.id)
            .map(|w| w.header.key.clone())
            .collect();
        k.sort();
        k
    };
    let gone = vec!["src/gone.py#f".to_string()];
    let mut store = LayoutStore::default();
    store.pins.insert(live.clone(), [1.0, 2.0]);
    store.pins.insert("src/gone.py#f".into(), [3.0, 4.0]);
    store.labels.push(ClusterLabel {
        lane: cluster.lane.clone(),
        members: members.clone(),
        label: "kept".into(),
        label_source: "human".into(),
        summary: None,
        missed: 7,
    });
    store.labels.push(ClusterLabel {
        lane: "code".into(),
        members: gone.clone(),
        label: "orphan".into(),
        label_source: "agent".into(),
        summary: None,
        missed: 0,
    });
    store.cluster_pins.push(ClusterPin {
        key: "cluster:orphan".into(),
        lane: "code".into(),
        members: gone,
        x: 0.0,
        y: 0.0,
        missed: 0,
    });

    for n in 1..MAX_MISSES {
        track_drift(&v, &mut store);
        assert_eq!(store.pin_missed.get("src/gone.py#f"), Some(&n), "index {n}");
        assert!(store.pins.contains_key("src/gone.py#f"));
        let orphan = store.labels.iter().find(|l| l.label == "orphan").unwrap();
        assert_eq!(orphan.missed, n);
        assert_eq!(store.cluster_pins[0].missed, n);
    }
    // The 20th consecutive miss drops them; matched records stay with their count reset.
    assert!(track_drift(&v, &mut store));
    assert!(!store.pins.contains_key("src/gone.py#f"));
    assert!(store.pin_missed.is_empty());
    assert!(store.cluster_pins.is_empty());
    assert_eq!(store.labels.len(), 1);
    assert_eq!(
        (store.labels[0].label.as_str(), store.labels[0].missed),
        ("kept", 0)
    );
    assert_eq!(store.pins.get(&live), Some(&[1.0, 2.0]));
    // Nothing left to drift: a further index changes nothing.
    assert!(!track_drift(&v, &mut store));
}
