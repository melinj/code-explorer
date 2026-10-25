use super::*;

const LANES: [&str; 5] = ["infra", "config", "code", "test", "docs"];

/// A synthetic project: owned inputs plus a builder for `Input`.
#[derive(Clone)]
struct Fx {
    files: Vec<(String, usize)>,
    /// (file, start_line, h, rate, conn, dependents2, settled)
    wins: Vec<(usize, u32, f64, f64, u8, u32, bool)>,
    links: Vec<(usize, usize, EdgeKind)>,
    co: Vec<(usize, usize, f64)>,
    pins: Vec<Pin>,
    arrangement: Arrangement,
}

impl Fx {
    fn new() -> Fx {
        Fx {
            files: vec![],
            wins: vec![],
            links: vec![],
            co: vec![],
            pins: vec![],
            arrangement: Arrangement::Districts,
        }
    }

    /// Adds a file with `n` windows; returns the window indices.
    fn file(&mut self, path: &str, lane: usize, n: usize, rate: f64, deps: u32) -> Vec<usize> {
        let f = self.files.len();
        self.files.push((path.to_string(), lane));
        (0..n)
            .map(|i| {
                self.wins.push((
                    f,
                    1 + 40 * i as u32,
                    36.0 + 18.0 * (12 + (i * 7) % 30) as f64,
                    rate,
                    0,
                    deps,
                    true,
                ));
                self.wins.len() - 1
            })
            .collect()
    }

    fn run(&self, prev: Option<&Placement>, full: bool) -> Output {
        let files: Vec<FileIn> = self
            .files
            .iter()
            .map(|(p, l)| FileIn {
                path: p.clone(),
                lane: *l,
                entry: test_entry(p),
            })
            .collect();
        let mut line_of_file: HashMap<usize, u32> = HashMap::new();
        let windows: Vec<WinIn> = self
            .wins
            .iter()
            .map(|&(file, start, h, rate, conn, deps, settled)| {
                *line_of_file.entry(file).or_default() += 1;
                WinIn {
                    file,
                    w: 720.0,
                    h,
                    start_line: start,
                    heat: 0,
                    rate,
                    conn,
                    dependents2: deps,
                    key: format!("{}#L{start}", self.files[file].0),
                    settled,
                }
            })
            .collect();
        let links: Vec<LinkIn> = self
            .links
            .iter()
            .map(|&(from, to, kind)| LinkIn { from, to, kind })
            .collect();
        let co: Vec<CoChangeIn> = self
            .co
            .iter()
            .map(|&(a, b, jaccard)| CoChangeIn { a, b, jaccard })
            .collect();
        layout(&Input {
            lanes: &LANES,
            files: &files,
            windows: &windows,
            links: &links,
            heat_bands: [0.001, 0.015, 0.025, 0.035],
            co_change: &co,
            pins: &self.pins,
            prev,
            full,
            grade_cuts: GradeCuts::default(),
            arrangement: self.arrangement,
        })
    }
}

/// Deterministic pseudo-random numbers.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) % n as u64) as usize
    }
}

/// ~70 code files in 7 dependency groups spread over 3 directories, plus tests, config and docs.
fn project() -> Fx {
    let mut fx = Fx::new();
    let mut rng = Lcg(7);
    let mut groups: Vec<Vec<usize>> = vec![Vec::new(); 7];
    for i in 0..70 {
        let g = i % 7;
        let dir = ["src/core", "src/api", "src/ui"][i % 3];
        let n = 1 + rng.next(4);
        let rate = [0.2, 0.05, 0.01, 0.005, 0.0, 0.0, 0.002][g] * (1.0 + rng.next(3) as f64);
        let ws = fx.file(&format!("{dir}/m{i:02}.py"), 2, n, rate, (7 - g as u32) * 5);
        groups[g].extend(ws);
    }
    for g in &groups {
        for _ in 0..g.len() * 2 {
            let (a, b) = (g[rng.next(g.len())], g[rng.next(g.len())]);
            if a != b {
                fx.links.push((a, b, EdgeKind::Call));
            }
        }
    }
    // A few links across groups.
    for _ in 0..10 {
        let (a, b) = (rng.next(fx.wins.len()), rng.next(fx.wins.len()));
        if a != b {
            fx.links.push((a, b, EdgeKind::Import));
        }
    }
    for i in 0..12 {
        fx.file(&format!("tests/test_{i}.py"), 3, 1 + i % 2, 0.01, 0);
    }
    for i in 0..9 {
        fx.file(&format!("conf/c{i}.yaml"), 1, 1, 0.0, 0);
    }
    fx.file("README.md", 4, 2, 0.001, 0);
    fx
}

fn moved(a: &Output, b: &Output, fa: &Fx, fb: &Fx) -> (usize, usize) {
    let key = |fx: &Fx, w: usize| {
        let (f, start, ..) = fx.wins[w];
        format!("{}#L{start}", fx.files[f].0)
    };
    let before: HashMap<String, Rect> = (0..fa.wins.len())
        .map(|w| (key(fa, w), a.rects[w]))
        .collect();
    let mut total = 0;
    let mut n = 0;
    for w in 0..fb.wins.len() {
        if let Some(r) = before.get(&key(fb, w)) {
            total += 1;
            if (r.x, r.y) != (b.rects[w].x, b.rects[w].y) {
                n += 1;
            }
        }
    }
    (n, total)
}

#[test]
fn deterministic() {
    let fx = project();
    assert_eq!(
        format!("{:?}", fx.run(None, false)),
        format!("{:?}", fx.run(None, false))
    );
}

#[test]
fn clusters_follow_dependencies_not_directories() {
    let fx = project();
    let o = fx.run(None, false);
    // Files of one dependency group (same i % 7) mostly share clusters, across directories.
    let code: Vec<&ClusterOut> = o.clusters.iter().filter(|c| c.lane == 2).collect();
    assert!(code.len() >= 5, "{}", code.len());
    let mut mixed_dirs = 0;
    for c in &code {
        let dirs: BTreeSet<&str> = c
            .members
            .iter()
            .map(|&w| dirname(&fx.files[fx.wins[w].0].0))
            .collect();
        if dirs.len() > 1 {
            mixed_dirs += 1;
        }
        assert!(c.members.len() <= MAX_CLUSTER);
    }
    assert!(mixed_dirs >= 3, "clusters are still directory-shaped");
    // Every window is in exactly one cluster and inside its rect.
    for (i, r) in o.rects.iter().enumerate() {
        let c = o.clusters[o.cluster_of[i]].rect;
        assert!(r.x >= c.x && r.y >= c.y && r.x + r.w <= c.x + c.w + 1e-6);
        assert!(r.y + r.h <= c.y + c.h + 1e-6);
    }
    // Two levels: some clusters have a subsystem parent.
    assert!(o.clusters.iter().any(|c| c.parent.is_some()));
}

#[test]
fn callers_sit_left_of_callees() {
    let mut fx = Fx::new();
    let a = fx.file("src/a.py", 2, 1, 0.1, 3)[0];
    let b = fx.file("src/b.py", 2, 1, 0.1, 3)[0];
    let c = fx.file("src/c.py", 2, 1, 0.1, 3)[0];
    let d = fx.file("src/d.py", 2, 1, 0.1, 3)[0];
    fx.links = vec![
        (a, b, EdgeKind::Call),
        (b, c, EdgeKind::Call),
        (c, a, EdgeKind::Call),
        (d, c, EdgeKind::Call),
    ];
    let o = fx.run(None, false);
    assert_eq!(o.cluster_of[a], o.cluster_of[d]);
    assert!(o.rects[d].x < o.rects[c].x);
    assert!(o.rects[b].x < o.rects[c].x);
}

#[test]
fn review_first_cell_is_top_left_of_the_code_lane() {
    let fx = project();
    let o = fx.run(None, false);
    let code = o.lanes.iter().find(|l| l.lane == "code").unwrap();
    assert_eq!(code.rows[0].conn, 4);
    let top = o
        .clusters
        .iter()
        .filter(|c| c.lane == 2 && c.heat == 4 && c.conn == 4)
        .collect::<Vec<_>>();
    assert!(!top.is_empty(), "no Hot × Core cluster");
    let hot_x = o.columns.iter().find(|c| c.heat == 4).unwrap().x;
    for t in &top {
        assert!(t.rect.y >= code.rows[0].y && t.rect.y < code.rows[0].y + code.rows[0].h);
        assert!(t.rect.x >= hot_x);
    }
    for c in o.clusters.iter().filter(|c| c.lane == 2) {
        assert!(c.rect.y >= code.rows[0].y);
        assert!(c.rect.x >= hot_x);
    }
    assert!(o.review_first.iter().any(|&r| r));
}

#[test]
fn heat_and_conn_grades_are_quantiles_over_clusters() {
    let fx = project();
    let o = fx.run(None, false);
    let code: Vec<&ClusterOut> = o.clusters.iter().filter(|c| c.lane == 2).collect();
    let hot = code.iter().filter(|c| c.heat == 4).count();
    let core = code.iter().filter(|c| c.conn == 4).count();
    // Neither axis collapses into one band.
    assert!(hot < code.len() / 2, "{hot} of {}", code.len());
    assert!(core < code.len() / 2, "{core} of {}", code.len());
    assert!(code.iter().any(|c| c.heat == 0) || code.iter().any(|c| c.heat <= 2));
}

#[test]
fn fragments_of_a_five_fragment_file_stay_in_at_most_two_clusters() {
    let mut fx = project();
    let frag = fx.file("src/core/big.py", 2, 5, 0.05, 20);
    // Each fragment calls into a different dependency group.
    let mut rng = Lcg(3);
    let n = fx.wins.len() - 5;
    for (i, &f) in frag.iter().enumerate() {
        for _ in 0..3 {
            let mut t = rng.next(n);
            while fx.wins[t].0 % 7 != i % 7 || fx.files[fx.wins[t].0].1 != 2 {
                t = rng.next(n);
            }
            fx.links.push((f, t, EdgeKind::Call));
        }
    }
    let o = fx.run(None, false);
    let cs: BTreeSet<usize> = frag.iter().map(|&w| o.cluster_of[w]).collect();
    assert!(cs.len() <= 2, "fragments in {} clusters", cs.len());
}

#[test]
fn one_changed_file_moves_under_5_percent_of_windows() {
    let fx = project();
    let a = fx.run(None, false);
    // One file changes: a window grows, a new fragment appears, and it gains a call.
    let mut fx2 = fx.clone();
    let f = 10;
    let first = (0..fx2.wins.len()).find(|&w| fx2.wins[w].0 == f).unwrap();
    fx2.wins[first].2 += 180.0;
    fx2.wins
        .push((f, 900, 300.0, fx2.wins[first].3, 0, 5, true));
    let new_w = fx2.wins.len() - 1;
    fx2.links.push((new_w, 3, EdgeKind::Call));
    let b = fx2.run(Some(&a.placement), false);
    let (n, total) = moved(&a, &b, &fx, &fx2);
    assert!(
        (n as f64) < 0.05 * total as f64,
        "{n} of {total} windows moved"
    );
    // Unchanged input with the previous placement moves nothing at all.
    let c = fx.run(Some(&a.placement), false);
    assert_eq!(moved(&a, &c, &fx, &fx).0, 0);
    assert_eq!(a.rects, c.rects);
}

#[test]
fn a_cluster_moves_only_when_its_heat_grade_settles() {
    let fx = project();
    let a = fx.run(None, false);
    // Pick a code cluster that is not Hot and make its files the hottest of all.
    let ci = a
        .clusters
        .iter()
        .position(|c| c.lane == 2 && c.heat < 3 && c.members.len() > 1)
        .unwrap();
    let files: BTreeSet<usize> = a.clusters[ci]
        .members
        .iter()
        .map(|&w| fx.wins[w].0)
        .collect();
    let mut hot = fx.clone();
    for w in hot.wins.iter_mut() {
        if files.contains(&w.0) {
            w.3 = 5.0;
            w.6 = false; // not settled yet
        }
    }
    let b = hot.run(Some(&a.placement), false);
    let bi = b.cluster_of[a.clusters[ci].members[0]];
    assert_eq!(b.clusters[bi].heat_now, 4);
    assert!(!b.clusters[bi].heat_settled);
    assert_eq!(
        b.clusters[bi].heat, a.clusters[ci].heat,
        "moved before settling"
    );
    assert_eq!(b.clusters[bi].rect, a.clusters[ci].rect);
    // Once settled, it moves to the Hot column.
    for w in hot.wins.iter_mut() {
        if files.contains(&w.0) {
            w.6 = true;
        }
    }
    let c = hot.run(Some(&b.placement), false);
    let cc = &c.clusters[c.cluster_of[a.clusters[ci].members[0]]];
    assert_eq!(cc.heat, 4);
    assert_ne!(cc.rect, a.clusters[ci].rect);
}

#[test]
fn pins_never_move() {
    let mut fx = project();
    let key = |fx: &Fx, w: usize| format!("{}#L{}", fx.files[fx.wins[w].0].0, fx.wins[w].1);
    fx.pins = vec![
        Pin {
            key: key(&fx, 5),
            x: -5000.0,
            y: 123.0,
        },
        Pin {
            key: key(&fx, 40),
            x: 77.0,
            y: 88.0,
        },
    ];
    let a = fx.run(None, false);
    let b = fx.run(Some(&a.placement), false);
    let c = fx.run(Some(&a.placement), true);
    for o in [&a, &b, &c] {
        assert_eq!((o.rects[5].x, o.rects[5].y), (-5000.0, 123.0));
        assert_eq!((o.rects[40].x, o.rects[40].y), (77.0, 88.0));
    }
}

#[test]
fn human_labels_carry_over_by_member_jaccard() {
    let fx = project();
    let a = fx.run(None, false);
    let ci = a
        .clusters
        .iter()
        .position(|c| c.members.len() >= 4)
        .unwrap();
    let mut p = a.placement.clone();
    assert!(p.set_label(ci, "Event bus", "human"));
    // A full re-layout of the same input re-clusters but finds the same members.
    let b = fx.run(Some(&p), true);
    let bi = b.cluster_of[a.clusters[ci].members[0]];
    assert_eq!(b.clusters[bi].label, "Event bus");
    assert_eq!(b.clusters[bi].label_source, "human");
    // Incremental layouts keep it too.
    let c = fx.run(Some(&b.placement), false);
    assert_eq!(
        c.clusters[c.cluster_of[a.clusters[ci].members[0]]].label,
        "Event bus"
    );
}

/// §4.10.6: a label follows its members into another lane (own lane first, then any lane), e.g.
/// after a `.cv/lanes.toml` rule moved their files.
#[test]
fn labels_carry_over_into_another_lane() {
    let mut fx = project();
    // A tight group of five files, linked in a chain, apart from the rest.
    let mut bus = Vec::new();
    for i in 0..5 {
        let ws = fx.file(&format!("ops/bus/b{i}.py"), 2, 1, 0.01, 2);
        if let Some(&prev) = bus.last() {
            fx.links.push((prev, ws[0], EdgeKind::Call));
            fx.links.push((ws[0], prev, EdgeKind::Inherit));
        }
        bus.push(ws[0]);
    }
    let a = fx.run(None, false);
    let ci = a.cluster_of[bus[0]];
    assert!(bus.iter().all(|&w| a.cluster_of[w] == ci) && a.clusters[ci].lane == 2);
    let mut p = a.placement.clone();
    assert!(p.set_label(ci, "Event bus", "agent"));
    // Its files move to the infra lane; a full re-layout (as after a lanes change) re-clusters
    // them there.
    let first = fx.files.len() - 5;
    for f in first..fx.files.len() {
        fx.files[f].1 = 0;
    }
    let b = fx.run(Some(&p), true);
    let bi = b.cluster_of[bus[0]];
    assert_eq!(b.clusters[bi].lane, 0);
    assert_eq!(
        (
            b.clusters[bi].label.as_str(),
            b.clusters[bi].label_source.as_str()
        ),
        ("Event bus", "agent")
    );
    // Only once: no code-lane cluster took it too.
    assert_eq!(
        b.clusters.iter().filter(|c| c.label == "Event bus").count(),
        1
    );
    // And it stays on the next layout, now in its own lane.
    let c = fx.run(Some(&b.placement), false);
    assert_eq!(c.clusters[c.cluster_of[bus[0]]].label, "Event bus");
}

/// DESIGN §4.10.3: cluster heat is graded per lane (Hot holds the clusters with the first 50% of
/// *the lane's* change mass). A lane that changes far less than the code lane still has a Hot
/// column; graded over every lane at once, all of its clusters would be Cool.
#[test]
fn heat_is_graded_per_lane() {
    let mut fx = Fx::new();
    for d in 0..6 {
        for i in 0..4 {
            let ws = fx.file(&format!("src/d{d}/m{i}.py"), 2, 1, 0.4 + 0.1 * d as f64, 3);
            if i > 0 {
                fx.links.push((ws[0], ws[0] - 1, EdgeKind::Call));
            }
        }
    }
    // The test lane changes 1,000x less, with its own spread: t0 is its hottest directory.
    for d in 0..6 {
        for i in 0..4 {
            let ws = fx.file(
                &format!("tests/t{d}/test_{i}.py"),
                3,
                1,
                0.0006 - 0.0001 * d as f64,
                0,
            );
            if i > 0 {
                fx.links.push((ws[0], ws[0] - 1, EdgeKind::Call));
            }
        }
    }
    let o = fx.run(None, false);
    for lane in [2, 3] {
        let grades: BTreeSet<u8> = o
            .clusters
            .iter()
            .filter(|c| c.lane == lane)
            .map(|c| c.heat)
            .collect();
        assert!(grades.contains(&4), "lane {lane}: {grades:?}");
        assert!(
            grades.len() >= 3,
            "lane {lane} spreads over the columns: {grades:?}"
        );
    }
    // The test lane's hottest directory is in its Hot column, its coldest is not.
    let heat_of = |path: &str| {
        let f = fx.files.iter().position(|(p, _)| p == path).unwrap();
        let w = fx.wins.iter().position(|x| x.0 == f).unwrap();
        o.clusters[o.cluster_of[w]].heat
    };
    assert_eq!(heat_of("tests/t0/test_0.py"), 4);
    assert!(heat_of("tests/t5/test_0.py") < 4);
    // Columns stay shared: a Hot test cluster sits in the same column as the Hot code clusters.
    let hot = o.columns.iter().find(|c| c.heat == 4).unwrap();
    for c in o.clusters.iter().filter(|c| c.heat == 4) {
        assert!(
            c.rect.x >= hot.x && c.rect.x + c.rect.w <= hot.x + hot.w,
            "{c:?}"
        );
    }
}

#[test]
fn co_change_and_cohesion_weights_pull_files_together() {
    let mut fx = Fx::new();
    let a = fx.file("x/a.py", 2, 3, 0.1, 1);
    let b = fx.file("y/b.py", 2, 3, 0.1, 1);
    let c: Vec<usize> = (0..6)
        .flat_map(|i| fx.file(&format!("z/c{i}.py"), 2, 1, 0.1, 1))
        .collect();
    for p in c.windows(2) {
        fx.links.push((p[0], p[1], EdgeKind::Call));
    }
    fx.co = vec![(0, 1, 0.9)];
    let o = fx.run(None, false);
    // a's fragments stay together (cohesion), and co-change joins a with b.
    assert!(a.iter().all(|&w| o.cluster_of[w] == o.cluster_of[a[0]]));
    assert_eq!(o.cluster_of[a[0]], o.cluster_of[b[0]]);
    assert_ne!(o.cluster_of[a[0]], o.cluster_of[c[0]]);
}

/// Crossings between adjacent layers, as the in-cluster order draws them.
fn crossings(layers: &[Vec<usize>], edges: &[(usize, usize)]) -> usize {
    let slot = |v: usize| {
        layers
            .iter()
            .find_map(|l| l.iter().position(|&x| x == v))
            .unwrap()
    };
    let mut n = 0;
    for (i, &(a, b)) in edges.iter().enumerate() {
        for &(c, d) in &edges[i + 1..] {
            if (slot(a) as i64 - slot(c) as i64) * (slot(b) as i64 - slot(d) as i64) < 0 {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn barycenter_sweeps_remove_crossings() {
    // Three layers: 0..3 → 3..6 → 6..9, wired so the starting order crosses a lot.
    let edges = [(0, 5), (1, 4), (2, 3), (3, 8), (4, 6), (5, 7), (1, 5)];
    let mut layer = vec![0; 9];
    for (v, l) in layer.iter_mut().enumerate() {
        *l = v / 3;
    }
    let mut keep: Vec<Vec<usize>> = vec![Vec::new(); 9];
    for &(a, b) in &edges {
        keep[a].push(b);
    }
    let mut layers: Vec<Vec<usize>> = vec![vec![0, 1, 2], vec![3, 4, 5], vec![6, 7, 8]];
    let before = crossings(&layers, &edges);
    reduce_crossings(&mut layers, &layer, &keep);
    let after = crossings(&layers, &edges);
    assert!(before >= 6, "{before}");
    assert!(after < before, "{before} → {after}");
    assert_eq!(after, 0, "{layers:?}");
    // Still the same windows per layer.
    for (l, ws) in layers.iter().enumerate() {
        let mut s = ws.clone();
        s.sort();
        assert_eq!(s, (3 * l..3 * l + 3).collect::<Vec<_>>());
    }
    // Deterministic.
    let mut again: Vec<Vec<usize>> = vec![vec![0, 1, 2], vec![3, 4, 5], vec![6, 7, 8]];
    reduce_crossings(&mut again, &layer, &keep);
    assert_eq!(again, layers);
}

#[test]
fn barycenter_keeps_the_order_when_nothing_crosses() {
    // a0 → b0, a1 → b1: no crossings, so the hotter-first order stays as it is.
    let layer = vec![0, 0, 1, 1];
    let keep = vec![vec![2], vec![3], vec![], vec![]];
    let mut layers = vec![vec![0, 1], vec![2, 3]];
    reduce_crossings(&mut layers, &layer, &keep);
    assert_eq!(layers, vec![vec![0, 1], vec![2, 3]]);
}

#[test]
fn hotter_windows_come_first_within_a_layer() {
    // One caller of four callees with different heat: the callees share layer 1, hottest on top.
    let mut fx = Fx::new();
    let caller = fx.file("src/main.py", 2, 1, 0.01, 1)[0];
    let rates = [0.002, 0.3, 0.05, 0.1];
    let callees: Vec<usize> = rates
        .iter()
        .enumerate()
        .map(|(i, &r)| fx.file(&format!("src/m{i}.py"), 2, 1, r, 1)[0])
        .collect();
    for &c in &callees {
        fx.links.push((caller, c, EdgeKind::Call));
    }
    let files: Vec<FileIn> = fx
        .files
        .iter()
        .map(|(p, l)| FileIn {
            path: p.clone(),
            lane: *l,
            entry: test_entry(p),
        })
        .collect();
    let windows: Vec<WinIn> = fx
        .wins
        .iter()
        .map(|&(file, start, _, rate, ..)| WinIn {
            file,
            w: 720.0,
            h: 100.0,
            start_line: start,
            heat: 0,
            rate,
            conn: 0,
            dependents2: 1,
            key: format!("{}#L{start}", fx.files[file].0),
            settled: true,
        })
        .collect();
    let input = Input {
        lanes: &LANES,
        files: &files,
        windows: &windows,
        links: &[],
        heat_bands: [0.001, 0.015, 0.025, 0.035],
        co_change: &[],
        pins: &[],
        prev: None,
        full: false,
        grade_cuts: GradeCuts::default(),
        arrangement: Arrangement::Districts,
    };
    let mut calls: Vec<Vec<usize>> = vec![Vec::new(); windows.len()];
    calls[caller] = callees.clone();
    let members: Vec<usize> = (0..windows.len()).collect();
    let mut rel = vec![Rect::default(); windows.len()];
    place_cluster(&members, &input, &calls, &mut rel);
    let mut by_y = callees.clone();
    by_y.sort_by(|&a, &b| {
        (rel[a].x, rel[a].y)
            .partial_cmp(&(rel[b].x, rel[b].y))
            .unwrap()
    });
    // Hottest first: rates 0.3, 0.1, 0.05, 0.002.
    assert_eq!(
        by_y,
        vec![callees[1], callees[3], callees[2], callees[0]],
        "{rel:?}"
    );
    assert!(callees.iter().all(|&c| rel[c].x > rel[caller].x));
}

#[test]
fn empty_heat_columns_collapse() {
    // Only Hot and Frozen clusters: Warm, Mild and Cool have no width and no gap.
    let mut fx = Fx::new();
    for i in 0..6 {
        fx.file(&format!("src/hot/h{i}.py"), 2, 1, 0.5, 3);
        fx.file(&format!("lib/cold/c{i}.py"), 2, 1, 0.0, 3);
    }
    let o = fx.run(None, false);
    let col = |heat: u8| o.columns.iter().find(|c| c.heat == heat).unwrap();
    assert_eq!(o.columns.len(), 5);
    assert!(col(4).w > 0.0 && col(0).w > 0.0, "{:?}", o.columns);
    for h in 1..=3 {
        assert_eq!(col(h).w, 0.0, "heat {h} column kept a width");
        assert_eq!(col(h).clusters, 0);
    }
    // Frozen follows Hot at one column gap; the world ends at Frozen's right edge.
    assert_eq!(col(0).x, col(4).x + col(4).w + COL_GAP);
    assert_eq!(o.world.w, col(0).x + col(0).w);
    // Aligned across lanes: a middle column used only by the test lane opens for every lane.
    for i in 0..6 {
        fx.file(&format!("tests/a/test_a{i}.py"), 3, 1, 0.5, 0);
        fx.file(&format!("tests/b/test_b{i}.py"), 3, 1, 0.3, 0);
    }
    let o2 = fx.run(None, false);
    let opened: Vec<u8> = o2
        .clusters
        .iter()
        .filter(|c| (1..=3).contains(&c.heat))
        .map(|c| c.heat)
        .collect();
    assert!(!opened.is_empty(), "{:?}", o2.columns);
    assert!(
        o2.clusters
            .iter()
            .filter(|c| (1..=3).contains(&c.heat))
            .all(|c| c.lane == 3)
    );
    for c in &o2.columns {
        let used = o2.clusters.iter().any(|k| k.heat == c.heat);
        assert_eq!(c.w > 0.0, used, "{:?}", o2.columns);
    }
    let code = o2.lanes.iter().find(|l| l.lane == "code").unwrap();
    assert_eq!(code.rect.w, o2.world.w);
}

/// No two window rects overlap.
fn assert_no_overlap(o: &Output) {
    for i in 0..o.rects.len() {
        for j in i + 1..o.rects.len() {
            assert!(
                !o.rects[i].intersects(&o.rects[j]),
                "windows {i} and {j} overlap: {:?} {:?}",
                o.rects[i],
                o.rects[j]
            );
        }
    }
}

#[test]
fn a_changed_file_moves_no_other_window_of_its_cluster() {
    let fx = project();
    let a = fx.run(None, false);
    // A code cluster holding several files, one of them with several windows.
    let (ci, f) = a
        .clusters
        .iter()
        .enumerate()
        .filter(|(_, c)| c.lane == 2)
        .find_map(|(ci, c)| {
            let files: BTreeSet<usize> = c.members.iter().map(|&w| fx.wins[w].0).collect();
            let f = *files
                .iter()
                .find(|&&f| c.members.iter().filter(|&&w| fx.wins[w].0 == f).count() >= 2)?;
            (files.len() >= 3).then_some((ci, f))
        })
        .unwrap();
    // The file grows a window, gains a fragment and gets much hotter (not settled yet).
    let mut fx2 = fx.clone();
    let first = (0..fx2.wins.len()).find(|&w| fx2.wins[w].0 == f).unwrap();
    fx2.wins[first].2 += 400.0;
    for w in fx2.wins.iter_mut().filter(|w| w.0 == f) {
        w.3 = 1.0;
        w.6 = false;
    }
    fx2.wins.push((f, 900, 420.0, 1.0, 0, 5, false));
    let b = fx2.run(Some(&a.placement), false);
    assert_no_overlap(&a);
    assert_no_overlap(&b);
    // Every other window of the cluster keeps its place; so does the cluster.
    for &w in &a.clusters[ci].members {
        if fx.wins[w].0 != f {
            assert_eq!(a.rects[w], b.rects[w], "window {w} moved");
        }
    }
    let bc = &b.clusters[b.cluster_of[a.clusters[ci].members[0]]];
    assert_eq!(
        (bc.rect.x, bc.rect.y),
        (a.clusters[ci].rect.x, a.clusters[ci].rect.y)
    );
    // The new fragment sits inside its cluster.
    let nw = fx2.wins.len() - 1;
    let r = b.rects[nw];
    assert!(r.x >= bc.rect.x && r.y >= bc.rect.y);
    assert!(r.x + r.w <= bc.rect.x + bc.rect.w && r.y + r.h <= bc.rect.y + bc.rect.h);
    let (n, total) = moved(&a, &b, &fx, &fx2);
    assert!(
        (n as f64) < 0.05 * total as f64,
        "{n} of {total} windows moved"
    );
}

/// Entry files of the test fixtures (what the language packs' conventions say in production).
fn test_entry(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.split('.').next().unwrap_or(name);
    matches!(stem, "lib" | "mod" | "main" | "__init__" | "index")
}

// ─── Folders arrangement ─────────────────────────────────────────────────────

fn inside(r: &Rect, o: &Rect) -> bool {
    r.x >= o.x && r.y >= o.y && r.x + r.w <= o.x + o.w && r.y + r.h <= o.y + o.h
}

#[test]
fn folders_cluster_by_directory_and_nest_frames() {
    let mut fx = Fx::new();
    fx.arrangement = Arrangement::Folders;
    let x = fx.file("a/x.py", 2, 3, 0.02, 2);
    let y = fx.file("a/y.py", 2, 2, 0.0, 1);
    let z = fx.file("a/b/z.py", 2, 2, 0.0, 0);
    let w = fx.file("c/d/e/w.py", 2, 2, 0.0, 0);
    let t = fx.file("tests/test_x.py", 3, 1, 0.0, 0);
    // Links pull x and z together on the grid; folders ignore them for grouping.
    fx.links.push((x[0], z[0], EdgeKind::Call));
    fx.links.push((x[1], w[0], EdgeKind::Call));
    let out = fx.run(None, false);
    let folder_of = |win: usize| out.clusters[out.cluster_of[win]].folder.clone().unwrap();
    assert_eq!(folder_of(x[0]), "a");
    assert_eq!(out.cluster_of[x[0]], out.cluster_of[y[0]]);
    assert_eq!(folder_of(z[0]), "a/b");
    assert_eq!(folder_of(w[1]), "c/d/e");
    assert_eq!(out.lanes.len(), 2, "code and test lanes");
    assert!(out.columns.is_empty() && out.lanes.iter().all(|l| l.rows.is_empty()));
    // a/b nests in a; c/d/e is one folded frame; frames are listed outermost first.
    let frame = |p: &str| {
        out.folders
            .iter()
            .find(|f| f.path == p)
            .unwrap_or_else(|| panic!("no frame {p}"))
    };
    assert!(inside(&frame("a/b").rect, &frame("a").rect));
    assert_eq!(frame("c/d/e").label, "c/d/e");
    assert!(out.folders.iter().all(|f| f.path != "c" && f.path != "c/d"));
    assert_eq!(frame("a").depth, 0);
    assert_eq!(frame("a/b").depth, 1);
    let depths: Vec<u32> = out.folders.iter().map(|f| f.depth).collect();
    assert!(depths.windows(2).all(|p| p[0] <= p[1]));
    // Every cluster sits in its directory's frame and in its lane; windows never overlap.
    for c in &out.clusters {
        let d = c.folder.as_deref().unwrap();
        if let Some(f) = out.folders.iter().find(|f| f.path == d) {
            assert!(inside(&c.rect, &f.rect), "{} outside its frame", c.label);
        }
        let band = out.lanes.iter().find(|l| l.lane == LANES[c.lane]).unwrap();
        assert!(inside(&c.rect, &band.rect));
    }
    for i in 0..out.rects.len() {
        for j in i + 1..out.rects.len() {
            assert!(!out.rects[i].intersects(&out.rects[j]), "{i} overlaps {j}");
        }
    }
    assert_eq!(out.lanes[1].lane, "test");
    assert_eq!(folder_of(t[0]), "tests");
    // Deterministic, and independent of a previous placement.
    let again = fx.run(Some(&out.placement), false);
    assert_eq!(again.rects, out.rects);
    assert_eq!(again.folders, out.folders);
}

#[test]
fn folders_split_a_big_directory_between_files() {
    let mut fx = Fx::new();
    fx.arrangement = Arrangement::Folders;
    for i in 0..10 {
        fx.file(&format!("big/f{i:02}.py"), 2, 10, 0.0, 0);
    }
    let out = fx.run(None, false);
    assert_eq!(
        out.clusters.len(),
        3,
        "100 windows in runs of ≤ 48, files whole"
    );
    assert!(
        out.clusters
            .iter()
            .all(|c| c.members.len() <= folders::FOLDER_MAX)
    );
    let labels: Vec<&str> = out.clusters.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, ["big · 1", "big · 2", "big · 3"]);
}

// ─── Structure ───────────────────────────────────────────────────────────────

mod structure_tests {
    use crate::structure::{FileIn, LinkIn, WinIn, analyse};

    fn file<'a>(path: &'a str, heat: u8, rate: f64, lines: u32) -> FileIn<'a> {
        FileIn {
            path,
            lane: if path.starts_with("tests/") {
                "test"
            } else {
                "code"
            },
            heat,
            rate,
            lines,
        }
    }

    fn link(from: usize, to: usize) -> LinkIn {
        LinkIn {
            from,
            to,
            import: false,
        }
    }

    #[test]
    fn pressure_interface_chain_and_pull() {
        let files = [
            file("core/a.py", 0, 0.0, 100),  // 0
            file("core/b.py", 0, 0.0, 100),  // 1
            file("core/c.py", 0, 0.0, 100),  // 2
            file("app/main.py", 0, 0.0, 50), // 3
            file("app/odd.py", 0, 0.0, 50),  // 4
            file("tests/t.py", 0, 0.0, 50),  // 5
            file("solo/x.py", 0, 0.0, 50),   // 6
        ];
        // One window per file, modules by file except odd.py, grouped with core.
        let windows: Vec<WinIn> = (0..7)
            .map(|f| WinIn {
                key: files[f].path,
                file: f,
                module: Some(if f == 4 { 0 } else { f.min(1) }),
            })
            .collect();
        let links = [
            link(0, 1),
            link(1, 2), // chain a → b → c inside core
            link(3, 0), // app calls core's interface (a)
            link(4, 1),
            link(4, 2), // odd.py talks to core only
            link(5, 2), // tests are left out
            link(6, 0),
            link(6, 1), // a lone file is never pulled
            LinkIn {
                from: 3,
                to: 0,
                import: true, // same pair as a call: not counted twice
            },
        ];
        let s = analyse(&files, &windows, &links);
        assert_eq!(s.summary.links, 7);
        assert_eq!(s.summary.cross_folder_links, 5);
        let solo = s.files.iter().find(|f| f.path == "solo/x.py").unwrap();
        assert_eq!((solo.external, solo.pull.is_none()), (2, true));
        let core = s.folders.iter().find(|f| f.path == "core").unwrap();
        assert_eq!((core.internal, core.incoming, core.outgoing), (2, 5, 0));
        assert_eq!(
            core.interface, 3,
            "a, b and c are each reached from outside"
        );
        assert_eq!(core.chain, 3);
        assert_eq!(core.partners, 2);
        assert_eq!(
            (core.gateways, core.spread),
            (3, 1.0),
            "every core file is reached from outside"
        );
        let app = s.folders.iter().find(|f| f.path == "app").unwrap();
        assert_eq!((app.gateways, app.spread), (2, 1.0));
        let w = |p: &str| s.windows.iter().find(|w| w.path == p).unwrap();
        assert_eq!(
            (
                w("core/a.py").internal,
                w("core/a.py").external,
                w("core/a.py").incoming
            ),
            (1, 2, 2)
        );
        assert_eq!((w("app/odd.py").external, w("app/odd.py").incoming), (2, 0));
        assert!(s.windows.iter().all(|w| w.path != "tests/t.py"));
        assert!((core.pressure - 5.0 / 7.0).abs() < 1e-3);
        assert!((core.inside - 2.0 / 7.0).abs() < 1e-3);
        let odd = s.files.iter().find(|f| f.path == "app/odd.py").unwrap();
        assert_eq!(
            odd.pull.as_ref().map(|p| (p.folder.as_str(), p.links)),
            Some(("core", 2))
        );
        let main = s.files.iter().find(|f| f.path == "app/main.py").unwrap();
        assert!(main.pull.is_none(), "one link is not a pull");
        assert!(s.files.iter().all(|f| f.lane != "test"));
        assert!(s.summary.divergence.unwrap() > 0.0);
        assert!(s.summary.misgrouped.unwrap() > 0.0);
    }

    #[test]
    fn heat_tension_marks_busy_files_among_quiet_ones() {
        let files = [
            file("mix/hot.py", 4, 0.9, 100),
            file("mix/cold1.py", 0, 0.0, 400),
            file("mix/cold2.py", 1, 0.01, 500),
            file("busy/a.py", 4, 0.5, 100),
            file("busy/b.py", 4, 0.5, 100),
        ];
        let windows: Vec<WinIn> = (0..5)
            .map(|f| WinIn {
                key: files[f].path,
                file: f,
                module: None,
            })
            .collect();
        let s = analyse(&files, &windows, &[]);
        let mix = s.folders.iter().find(|f| f.path == "mix").unwrap();
        let busy = s.folders.iter().find(|f| f.path == "busy").unwrap();
        assert!(mix.tension > 0.8, "{}", mix.tension);
        assert_eq!(mix.hot_files, ["mix/hot.py"]);
        assert_eq!(
            s.windows
                .iter()
                .find(|w| w.path == "mix/hot.py")
                .unwrap()
                .tension,
            1.0
        );
        assert_eq!(
            s.windows.len(),
            1,
            "only the tense window: there are no links"
        );
        assert_eq!(busy.tension, 0.0, "uniformly busy is not tension");
        let hot = s.files.iter().find(|f| f.path == "mix/hot.py").unwrap();
        assert_eq!(hot.tension, 1.0);
        assert!(s.summary.divergence.is_none());
        assert!(s.summary.heat_tension > 0.0 && s.summary.heat_tension < mix.tension);
    }
}

#[test]
fn flow_order_keeps_linked_items_together() {
    // The chain starts at the most linked item (3), takes its partner (0), then 4 (linked to 3)
    // and 1 (linked to 4); 2 has no links and keeps its place after them.
    let w: HashMap<(usize, usize), f64> = [((0, 3), 5.0), ((1, 4), 2.0), ((3, 4), 1.0)].into();
    assert_eq!(folders::flow_order(5, &w), [3, 0, 4, 1, 2]);
    assert_eq!(folders::flow_order(3, &HashMap::new()), [0, 1, 2]);
}

#[test]
fn flow_arrangement_puts_linked_directories_side_by_side() {
    let mut fx = Fx::new();
    let a = fx.file("a/x.py", 2, 2, 0.0, 0);
    fx.file("b/y.py", 2, 2, 0.0, 0);
    fx.file("c/z.py", 2, 2, 0.0, 0);
    let d = fx.file("d/w.py", 2, 2, 0.0, 0);
    fx.links.push((a[0], d[0], EdgeKind::Call));
    fx.links.push((a[1], d[1], EdgeKind::Call));
    let order = |out: &Output| {
        let mut fs: Vec<&cv_contracts::FolderFrame> = out.folders.iter().collect();
        fs.sort_by(|p, q| {
            (p.rect.y, p.rect.x)
                .partial_cmp(&(q.rect.y, q.rect.x))
                .unwrap()
        });
        fs.iter().map(|f| f.path.clone()).collect::<Vec<_>>()
    };
    fx.arrangement = Arrangement::Folders;
    assert_eq!(order(&fx.run(None, false)), ["a", "b", "c", "d"]);
    fx.arrangement = Arrangement::Flow;
    let flow = fx.run(None, false);
    assert_eq!(order(&flow), ["a", "d", "b", "c"]);
    assert!(flow.clusters.iter().all(|c| c.folder.is_some()));
}

mod depth_tests {
    use crate::structure::{FileIn, LinkIn, WinIn, analyse};

    #[test]
    fn a_link_into_a_subfolder_stays_inside_the_window_s_module() {
        let files = [
            FileIn {
                path: "m/api.py",
                lane: "code",
                heat: 0,
                rate: 0.0,
                lines: 10,
            },
            FileIn {
                path: "m/impl/core.py",
                lane: "code",
                heat: 0,
                rate: 0.0,
                lines: 10,
            },
            FileIn {
                path: "other/use.py",
                lane: "code",
                heat: 0,
                rate: 0.0,
                lines: 10,
            },
        ];
        let windows: Vec<WinIn> = (0..3)
            .map(|f| WinIn {
                key: files[f].path,
                file: f,
                module: None,
            })
            .collect();
        let link = |from, to| LinkIn {
            from,
            to,
            import: false,
        };
        // other → m/api (the interface), m/api → m/impl/core (below it).
        let s = analyse(&files, &windows, &[link(2, 0), link(0, 1)]);
        let w = |p: &str| s.windows.iter().find(|w| w.path == p).unwrap();
        assert_eq!((w("m/api.py").internal, w("m/api.py").external), (1, 1));
        // From m/impl the call comes from its parent: outside its module.
        assert_eq!(
            (w("m/impl/core.py").internal, w("m/impl/core.py").external),
            (0, 1)
        );
        let m = s.folders.iter().find(|f| f.path == "m").unwrap();
        assert_eq!((m.internal, m.incoming, m.outgoing), (1, 1, 0));
        assert_eq!(m.inside, 0.5);
        let imp = s.folders.iter().find(|f| f.path == "m/impl").unwrap();
        assert_eq!(imp.inside, 0.0, "its only link comes from its parent");
    }
}

#[test]
fn review_priority_weighs_up_code_at_a_module_s_edge() {
    let mut fx = Fx::new();
    // p and q: equal heat and connectedness; p also links out of m/, q only within it.
    let p = fx.file("m/p.py", 2, 1, 0.02, 3)[0];
    let q = fx.file("m/q.py", 2, 1, 0.02, 3)[0];
    let r = fx.file("n/r.py", 2, 1, 0.0, 0)[0];
    fx.links.push((p, r, EdgeKind::Call));
    fx.links.push((q, p, EdgeKind::Call));
    let out = fx.run(None, false);
    // p: 1 inside, 1 outside → × 0.75; q: 1 inside → × 0.5.
    assert!(out.risk[q] > 0.0);
    assert!(
        (out.risk[p] / out.risk[q] - 1.5).abs() < 1e-9,
        "{} vs {}",
        out.risk[p],
        out.risk[q]
    );
    assert_eq!(structure::boundary_factor(0, 0), 0.5);
    assert_eq!(structure::boundary_factor(0, 4), 1.0);
}
