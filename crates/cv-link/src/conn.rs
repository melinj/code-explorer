//! Connectedness over typed links (DESIGN §4.8, D7).
//!
//! The graph's nodes are windows and its edges are typed dependencies, dependent → dependency:
//! calls, inheritance and implementation (code links of confidence ≥ [`MIN_DEP_CONFIDENCE`], so
//! lexical guesses do not count), imports (the window holding the import line → the imported
//! member's window, or the imported file's first window for module and star imports), and the
//! channel links the caller adds (bus, http, env, deploy and config hubs at confidence ≥ 0.8:
//! consumers depend on producers).
//!
//! Per window:
//! - `dependents2`: windows depending on it directly or through one intermediary. A code-lane
//!   window counts code-lane dependents only; a test-lane window counts test-lane dependents (its
//!   fixtures' users); any other window counts dependents from every lane but tests.
//! - `tests`: test-lane files with a window depending on it directly ("covered by 12 tests"),
//!   kept apart so a heavily tested module does not look central.
//! - `fan_in` / `fan_out`: distinct direct dependents / dependencies (any lane).
//! - `reach` and `cycle` (details): files depending on its file transitively (tests excluded),
//!   and the size of the import cycle (strongly connected component of the file import graph)
//!   its file sits in, when > 1.
//! - `grade`: rank quantiles per lane among windows with at least one dependent ([`ConnCuts`]).

use rayon::prelude::*;

use crate::{CODE_LANE, FileIn, LinkOut, TEST_LANE, window_at};

/// Code links below this confidence (lexical candidates, ≤ 0.5) are not dependencies.
pub const MIN_DEP_CONFIDENCE: f64 = 0.6;

/// Rank-quantile shares closing Core, High and Medium among windows with dependents (the rest is
/// Low). D6: configurable; the default is Core top 10%, High next 15%, Medium next 25%.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConnCuts {
    pub core: f64,
    pub high: f64,
    pub medium: f64,
}

impl ConnCuts {
    /// The rank quantiles opening Medium, High and Core (ascending), e.g. [0.5, 0.75, 0.9].
    pub fn rank_cuts(&self) -> [f64; 3] {
        let core = 1.0 - self.core;
        let high = core - self.high;
        [high - self.medium, high, core]
    }
}

impl Default for ConnCuts {
    fn default() -> Self {
        ConnCuts {
            core: 0.10,
            high: 0.15,
            medium: 0.25,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct WinConn {
    pub dependents2: u32,
    pub fan_in: u32,
    pub fan_out: u32,
    pub tests: u32,
    pub reach: u32,
    /// Files in this window's file's dependency cycle; 0 when it is in none.
    pub cycle: u32,
    /// 0 isolated … 4 core.
    pub grade: u8,
}

/// The dependency graph: typed window dependencies, and the file import graph for cycle sizes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Deps {
    /// (dependent window, dependency window).
    pub windows: Vec<(usize, usize)>,
    /// (importing file, imported file), file indices as in `file_of`.
    pub imports: Vec<(usize, usize)>,
}

/// Typed dependencies from code links and imports.
pub fn typed_deps(files: &[FileIn], links: &[LinkOut]) -> Deps {
    let mut imports: Vec<(usize, usize)> = Vec::new();
    let mut out: Vec<(usize, usize)> = links
        .iter()
        .filter(|l| l.confidence >= MIN_DEP_CONFIDENCE && l.from_window != l.to_window)
        .map(|l| (l.from_window, l.to_window))
        .collect();
    let syms: Vec<std::collections::HashMap<&str, u32>> = files
        .iter()
        .map(|f| {
            let mut m = std::collections::HashMap::new();
            if let Some(facts) = f.facts {
                for s in facts.symbols.iter().filter(|s| s.parent.is_none()) {
                    m.entry(s.name.as_str()).or_insert(s.name_line);
                }
            }
            m
        })
        .collect();
    for (fi, f) in files.iter().enumerate() {
        let Some(facts) = f.facts else { continue };
        for (imp, target) in facts.imports.iter().zip(&f.imports) {
            let Some(t) = *target else { continue };
            if t == fi || files[t].windows.is_empty() {
                continue;
            }
            imports.push((fi, t));
            let Some(from) = window_at(&f.windows, imp.line).or(f.windows.first().map(|w| w.2))
            else {
                continue;
            };
            let to = imp
                .imported
                .as_deref()
                .filter(|n| *n != "*")
                .and_then(|n| syms[t].get(n))
                .and_then(|&line| window_at(&files[t].windows, line))
                .unwrap_or(files[t].windows[0].2);
            if from != to {
                out.push((from, to));
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    imports.sort_unstable();
    imports.dedup();
    Deps {
        windows: out,
        imports,
    }
}

/// Window connectedness. `lanes[w]` and `file_of[w]` describe window `w`; duplicate and
/// self-loop dependencies are ignored.
pub fn window_connectedness(
    lanes: &[&str],
    file_of: &[usize],
    deps: &Deps,
    cuts: ConnCuts,
) -> Vec<WinConn> {
    let n = lanes.len();
    let mut d: Vec<(usize, usize)> = deps
        .windows
        .iter()
        .copied()
        .filter(|&(a, b)| a != b && a < n && b < n)
        .collect();
    d.sort_unstable();
    d.dedup();
    let mut fwd: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut rev: Vec<Vec<u32>> = vec![Vec::new(); n];
    for &(a, b) in &d {
        fwd[a].push(b as u32);
        rev[b].push(a as u32);
    }
    #[derive(Clone, Copy, PartialEq)]
    enum L {
        Code,
        Test,
        Other,
    }
    let lane: Vec<L> = lanes
        .iter()
        .map(|l| match *l {
            CODE_LANE => L::Code,
            TEST_LANE => L::Test,
            _ => L::Other,
        })
        .collect();
    let keep = |own: L, a: usize| match own {
        L::Code => lane[a] == L::Code,
        L::Test => lane[a] == L::Test,
        L::Other => lane[a] != L::Test,
    };
    let is_test: Vec<bool> = lane.iter().map(|l| *l == L::Test).collect();
    let (reach, cycle) = file_reach(file_of, &d, &deps.imports, &is_test);
    let mut out: Vec<WinConn> = (0..n)
        .into_par_iter()
        .map_init(
            || (vec![u32::MAX; n], Vec::<u32>::new()),
            |(stamp, test_files), i| {
                let own = lane[i];
                let mark = i as u32;
                stamp[i] = mark;
                let mut count = 0u32;
                test_files.clear();
                for &a in &rev[i] {
                    let a = a as usize;
                    if lane[a] == L::Test && own != L::Test {
                        test_files.push(file_of[a] as u32);
                    }
                    if !keep(own, a) {
                        continue;
                    }
                    if stamp[a] != mark {
                        stamp[a] = mark;
                        count += 1;
                    }
                    for &x in &rev[a] {
                        let x = x as usize;
                        if keep(own, x) && stamp[x] != mark {
                            stamp[x] = mark;
                            count += 1;
                        }
                    }
                }
                test_files.sort_unstable();
                test_files.dedup();
                let f = file_of[i];
                WinConn {
                    dependents2: count,
                    fan_in: rev[i].len() as u32,
                    fan_out: fwd[i].len() as u32,
                    tests: test_files.len() as u32,
                    reach: reach.get(f).copied().unwrap_or(0),
                    cycle: cycle.get(f).copied().unwrap_or(0),
                    grade: 0,
                }
            },
        )
        .collect();
    grade(lanes, &mut out, cuts);
    out
}

/// File-level connectedness on the file import graph alone: appendix C's reference definition
/// (`docs/analysis/axes.py`), kept so the study stays reproducible next to the window-level
/// design. Nodes are files (`lanes[f]`), `imports` are (importing file, imported file); the
/// per-file numbers mean what [`WinConn`]'s do per window (2-hop code dependents, direct test
/// importers, fan-in/out, reach, cycle, rank-quantile grade).
pub fn file_import_connectedness(
    lanes: &[&str],
    imports: &[(usize, usize)],
    cuts: ConnCuts,
) -> Vec<WinConn> {
    let file_of: Vec<usize> = (0..lanes.len()).collect();
    let deps = Deps {
        windows: imports.to_vec(),
        imports: imports.to_vec(),
    };
    window_connectedness(lanes, &file_of, &deps, cuts)
}

/// Rank-quantile grades per lane among windows with any dependent.
fn grade(lanes: &[&str], out: &mut [WinConn], cuts: ConnCuts) {
    let mut ids: Vec<&str> = lanes.to_vec();
    ids.sort_unstable();
    ids.dedup();
    for lane in ids {
        let mut nz: Vec<u32> = (0..out.len())
            .filter(|&i| lanes[i] == lane && out[i].dependents2 > 0)
            .map(|i| out[i].dependents2)
            .collect();
        if nz.is_empty() {
            continue;
        }
        nz.sort_unstable();
        // The tolerance keeps exact shares (e.g. 0.1 of 10) from rounding down a rank.
        let q = |p: f64| {
            let r = (p.clamp(0.0, 1.0) * nz.len() as f64 + 1e-9) as usize;
            nz[r.min(nz.len() - 1)]
        };
        let [r_med, r_high, r_core] = cuts.rank_cuts();
        let (t_core, t_high, t_med) = (q(r_core), q(r_high), q(r_med));
        for (i, c) in out.iter_mut().enumerate() {
            if lanes[i] != lane {
                continue;
            }
            let v = c.dependents2;
            c.grade = if v == 0 {
                0
            } else if v >= t_core {
                4
            } else if v >= t_high {
                3
            } else if v >= t_med {
                2
            } else {
                1
            };
        }
    }
}

/// Per file: transitive dependent files (test files excluded) on the file graph the window
/// dependencies induce, and the size of its import cycle (0 when none).
fn file_reach(
    file_of: &[usize],
    deps: &[(usize, usize)],
    imports: &[(usize, usize)],
    is_test: &[bool],
) -> (Vec<u32>, Vec<u32>) {
    let nf = file_of
        .iter()
        .map(|&f| f + 1)
        .chain(imports.iter().map(|&(a, b)| a.max(b) + 1))
        .max()
        .unwrap_or(0);
    let mut cycle = vec![0u32; nf];
    let mut import_rev: Vec<Vec<usize>> = vec![Vec::new(); nf];
    for &(a, b) in imports {
        if a != b {
            import_rev[b].push(a);
        }
    }
    for c in tarjan(&import_rev) {
        if c.len() > 1 {
            for &v in &c {
                cycle[v] = c.len() as u32;
            }
        }
    }
    let mut test_file = vec![false; nf];
    let mut has_win = vec![false; nf];
    for (w, &f) in file_of.iter().enumerate() {
        has_win[f] = true;
        if is_test[w] {
            test_file[f] = true;
        }
    }
    // dependency → dependents
    let mut rev: Vec<Vec<usize>> = vec![Vec::new(); nf];
    for &(a, b) in deps {
        let (fa, fb) = (file_of[a], file_of[b]);
        if fa != fb {
            rev[fb].push(fa);
        }
    }
    for r in rev.iter_mut() {
        r.sort_unstable();
        r.dedup();
    }
    let comps = tarjan(&rev);
    let mut comp_of = vec![0usize; nf];
    for (ci, c) in comps.iter().enumerate() {
        for &v in c {
            comp_of[v] = ci;
        }
    }
    // Tarjan emits a component after every component it reaches (its dependents here), so one
    // pass in emission order sees each dependent's reach first. Bitsets over non-test files.
    let counted: Vec<usize> = (0..nf).filter(|&f| has_win[f] && !test_file[f]).collect();
    let mut bit = vec![usize::MAX; nf];
    for (i, &f) in counted.iter().enumerate() {
        bit[f] = i;
    }
    let words = counted.len().div_ceil(64);
    let mut reach = vec![0u32; nf];
    if words == 0 || comps.len().saturating_mul(words) > 64 << 20 {
        return (reach, cycle);
    }
    let mut sets: Vec<Vec<u64>> = Vec::with_capacity(comps.len());
    for (ci, c) in comps.iter().enumerate() {
        let mut s = vec![0u64; words];
        for &v in c {
            for &d in &rev[v] {
                let dc = comp_of[d];
                if dc != ci {
                    for (x, y) in s.iter_mut().zip(&sets[dc]) {
                        *x |= *y;
                    }
                }
                if bit[d] != usize::MAX {
                    s[bit[d] / 64] |= 1 << (bit[d] % 64);
                }
            }
        }
        let total: u32 = s.iter().map(|w| w.count_ones()).sum();
        for &v in c {
            let own = bit[v] != usize::MAX && s[bit[v] / 64] & (1 << (bit[v] % 64)) != 0;
            reach[v] = total - u32::from(own);
        }
        sets.push(s);
    }
    (reach, cycle)
}

/// Strongly connected components (iterative Tarjan), each emitted after every component reachable
/// from it.
fn tarjan(g: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let n = g.len();
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut on = vec![false; n];
    let mut stack = Vec::new();
    let mut comps = Vec::new();
    let mut next = 0;
    let mut call: Vec<(usize, usize)> = Vec::new();
    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        call.push((root, 0));
        while let Some(&(v, ei)) = call.last() {
            if ei == 0 && index[v] == usize::MAX {
                index[v] = next;
                low[v] = next;
                next += 1;
                stack.push(v);
                on[v] = true;
            }
            if let Some(&w) = g[v].get(ei) {
                if let Some(top) = call.last_mut() {
                    top.1 += 1;
                }
                if index[w] == usize::MAX {
                    call.push((w, 0));
                } else if on[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            call.pop();
            if let Some(&(p, _)) = call.last() {
                low[p] = low[p].min(low[v]);
            }
            if low[v] == index[v] {
                let mut c = Vec::new();
                while let Some(x) = stack.pop() {
                    on[x] = false;
                    c.push(x);
                    if x == v {
                        break;
                    }
                }
                comps.push(c);
            }
        }
    }
    comps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_hops_code_only_tests_apart_and_cycles() {
        // Windows (file): a0(0) b1(1) c2(2) d3(3) code; t4(4) test; y5(5) config.
        // a → b → c → d (a depends on b, …); t → d; d → y (code reads config).
        let lanes = ["code", "code", "code", "code", "test", "config"];
        let file_of = [0, 1, 2, 3, 4, 5];
        let deps = Deps {
            windows: vec![(0, 1), (1, 2), (2, 3), (4, 3), (3, 5)],
            imports: vec![(0, 1)],
        };
        let c = window_connectedness(&lanes, &file_of, &deps, ConnCuts::default());
        assert_eq!(c[3].dependents2, 2); // c and b; not a (3 hops); not t (test lane)
        assert_eq!(c[3].tests, 1);
        assert_eq!((c[3].fan_in, c[3].fan_out), (2, 1));
        assert_eq!(c[5].dependents2, 2); // d, c: config counts any lane but tests
        assert_eq!(c[0].dependents2, 0);
        assert_eq!(c[0].grade, 0);
        assert!(c[3].grade >= c[1].grade && c[1].grade >= 1);
        assert_eq!(c[3].reach, 3); // a, b, c (t is a test file)
        assert_eq!(c[5].reach, 4);
        assert!(c.iter().all(|x| x.cycle == 0));

        // An import cycle of three files: b → c → d → b. Call cycles do not count as import cycles.
        let deps = Deps {
            windows: vec![(0, 1), (1, 2), (2, 3), (3, 1), (5, 0), (0, 5)],
            imports: vec![(1, 2), (2, 3), (3, 1)],
        };
        let c = window_connectedness(&lanes, &file_of, &deps, ConnCuts::default());
        assert_eq!((c[1].cycle, c[2].cycle, c[3].cycle), (3, 3, 3));
        assert_eq!((c[0].cycle, c[5].cycle), (0, 0));
        // The cycle does not saturate the 2-hop count.
        assert_eq!(c[3].dependents2, 2);
    }

    #[test]
    fn file_import_graph_matches_the_reference_definition() {
        // Files: a(0) b(1) c(2) d(3) code, t(4) test. a→b→c→d imports, t→d, t→c.
        let lanes = ["code", "code", "code", "code", "test"];
        let imports = [(0, 1), (1, 2), (2, 3), (4, 3), (4, 2)];
        let c = file_import_connectedness(&lanes, &imports, ConnCuts::default());
        assert_eq!(c[3].dependents2, 2); // c, b (axes.py two_hop: code dependents within 2 hops)
        assert_eq!(c[2].dependents2, 2); // b, a
        assert_eq!(c[0].dependents2, 0);
        assert_eq!((c[3].tests, c[3].fan_in), (1, 2)); // tests_direct, fan_in over code + tests
        assert_eq!(c[3].reach, 3);
        assert_eq!(c[0].grade, 0);
        assert!(c[3].grade >= c[1].grade);
    }

    #[test]
    fn grade_cut_points_are_configurable() {
        // A star: window i has i dependents (i = 1..=10), 11 leaves.
        let mut lanes = vec!["code"; 11];
        let mut file_of: Vec<usize> = (0..11).collect();
        let mut deps = Deps::default();
        for i in 1..=10usize {
            for _ in 0..i {
                let leaf = lanes.len();
                lanes.push("code");
                file_of.push(leaf);
                deps.windows.push((leaf, i));
            }
        }
        let c = window_connectedness(&lanes, &file_of, &deps, ConnCuts::default());
        let g: Vec<u8> = (1..=10).map(|i| c[i].grade).collect();
        assert_eq!(g, vec![1, 1, 1, 1, 1, 2, 2, 3, 3, 4]);
        let wide = ConnCuts {
            core: 0.5,
            high: 0.2,
            medium: 0.2,
        };
        let c = window_connectedness(&lanes, &file_of, &deps, wide);
        let g: Vec<u8> = (1..=10).map(|i| c[i].grade).collect();
        assert_eq!(g, vec![1, 2, 2, 3, 3, 4, 4, 4, 4, 4]);
    }
}
