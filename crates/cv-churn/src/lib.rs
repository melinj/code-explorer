//! B5 `cv-churn` (minimal): recency-weighted change frequency per file and change-mass quantile
//! grades (DESIGN §4.7, D6), with Wilson intervals on the effective sample size and the stopping rule.
//!
//! Each commit weighs 2^(-age/90d); commits touching > 50 files are down-weighted by
//! 1/log₂(1 + files). A file's rate is its weighted touches over the total weight.
//!
//! - **Renames carry history:** the walk is newest-first, so when a commit renames `old` → `new`,
//!   every older touch of `old` is credited to the file `new` is called at the head.
//! - **Co-change:** weighted Jaccard of the commits two files share, over commits of at most
//!   [`MASS_REFACTOR_FILES`] files (a refactor says nothing about coupling).
//! - **Range heat:** [`range_heat`] counts in-range touches per file ("2 of 3 commits").

use std::collections::HashMap;

pub const HALF_LIFE_DAYS: f64 = 90.0;
const DAY_MS: f64 = 86_400_000.0;
/// Commits touching more files than this are mass refactors: down-weighted, and left out of co-change.
pub const MASS_REFACTOR_FILES: usize = 50;
/// Co-change pairs kept: at least this many shared commits and this weighted Jaccard.
pub const COCHANGE_MIN_SUPPORT: u32 = 2;
pub const COCHANGE_MIN_JACCARD: f64 = 0.2;
/// Cumulative change-mass shares closing Hot, Warm and Mild (the rest with any change is Cool).
pub const MASS_CUTS: [f64; 3] = [0.5, 0.75, 0.9];

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_commits: usize,
    pub max_age_days: f64,
    pub min_commits: usize,
    pub batch: usize,
    /// Cumulative change-mass shares closing Hot, Warm and Mild (D6: configurable, default
    /// [`MASS_CUTS`]).
    pub mass_cuts: [f64; 3],
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_commits: 2000,
            max_age_days: 730.0,
            min_commits: 50,
            batch: 25,
            mass_cuts: MASS_CUTS,
        }
    }
}

/// One commit as the history walk yields it.
#[derive(Debug, Clone, Default)]
pub struct CommitTouch {
    pub time_ms: i64,
    pub files: Vec<String>,
    /// (old path, new path) renames in this commit.
    pub renames: Vec<(String, String)>,
}

/// Two files that change together.
#[derive(Debug, Clone, PartialEq)]
pub struct CoChange {
    pub a: String,
    pub b: String,
    /// Commits touching both (raw count).
    pub support: u32,
    /// Weighted Jaccard: shared commit weight over the weight of commits touching either.
    pub jaccard: f64,
}

/// One stop-rule check (every `batch` commits once `min_commits` are scanned).
#[derive(Debug, Clone, PartialEq)]
pub struct BatchCheck {
    pub scanned: usize,
    pub settled_mass: f64,
    pub bands: [f64; 4],
    pub stable_batches: u32,
}

/// Follows renames newest-first: maps a path as it was at some commit to its name at the head.
#[derive(Debug, Default, Clone)]
pub struct Renames {
    to_head: HashMap<String, String>,
    pub followed: usize,
}

impl Renames {
    /// Records a commit's renames; call before resolving that commit's (older) touches.
    pub fn record(&mut self, renames: &[(String, String)]) {
        for (old, new) in renames {
            let head = self.resolve(new).to_string();
            if head != *old {
                self.to_head.insert(old.clone(), head);
                self.followed += 1;
            }
        }
    }

    pub fn resolve<'a>(&'a self, path: &'a str) -> &'a str {
        self.to_head.get(path).map(String::as_str).unwrap_or(path)
    }

    /// Every followed old path → its name at the head.
    pub fn into_map(self) -> HashMap<String, String> {
        self.to_head
    }
}

#[derive(Debug, Clone, Default)]
pub struct FileHeat {
    /// 0 frozen … 4 hot.
    pub grade: u8,
    pub rate: f64,
    pub ci: [f64; 2],
    pub settled: bool,
    pub touches: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    Settled,
    MaxCommits,
    MaxAge,
    HistoryExhausted,
}

impl Stop {
    pub fn as_str(self) -> &'static str {
        match self {
            Stop::Settled => "settled",
            Stop::MaxCommits => "max-commits",
            Stop::MaxAge => "max-age",
            Stop::HistoryExhausted => "history-exhausted",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChurnResult {
    pub files: HashMap<String, FileHeat>,
    pub scanned: usize,
    pub oldest_ms: Option<i64>,
    pub settled_mass: f64,
    pub stop: Stop,
    /// Ascending rate thresholds: [frozen|cool, cool|mild, mild|warm, warm|hot].
    pub bands: [f64; 4],
    pub effective_sample: f64,
    /// Renames followed during the walk.
    pub renames_followed: usize,
    pub co_change: Vec<CoChange>,
    /// The stop-rule checks, oldest first (for diagnostics).
    pub checks: Vec<BatchCheck>,
}

#[derive(Default)]
struct Acc {
    weighted: HashMap<String, (f64, u32)>,
    w: f64,
    w2: f64,
}

/// Co-change accumulator over interned file ids.
#[derive(Default)]
struct CoAcc {
    ids: HashMap<String, u32>,
    names: Vec<String>,
    /// Per file: weight of the (non-refactor) commits touching it.
    w: Vec<f64>,
    pairs: HashMap<(u32, u32), (f64, u32)>,
}

impl CoAcc {
    fn add(&mut self, files: &[&str], w: f64) {
        let mut ids: Vec<u32> = files
            .iter()
            .map(|f| match self.ids.get(*f) {
                Some(&i) => i,
                None => {
                    let i = self.names.len() as u32;
                    self.ids.insert(f.to_string(), i);
                    self.names.push(f.to_string());
                    self.w.push(0.0);
                    i
                }
            })
            .collect();
        ids.sort_unstable();
        ids.dedup();
        for &i in &ids {
            self.w[i as usize] += w;
        }
        for (k, &a) in ids.iter().enumerate() {
            for &b in &ids[k + 1..] {
                let e = self.pairs.entry((a, b)).or_insert((0.0, 0));
                e.0 += w;
                e.1 += 1;
            }
        }
    }

    fn finish(self) -> Vec<CoChange> {
        let mut out: Vec<CoChange> = self
            .pairs
            .into_iter()
            .filter(|(_, (_, n))| *n >= COCHANGE_MIN_SUPPORT)
            .filter_map(|((a, b), (both, n))| {
                let either = self.w[a as usize] + self.w[b as usize] - both;
                let j = if either > 0.0 { both / either } else { 0.0 };
                (j >= COCHANGE_MIN_JACCARD).then(|| CoChange {
                    a: self.names[a as usize].clone(),
                    b: self.names[b as usize].clone(),
                    support: n,
                    jaccard: j,
                })
            })
            .collect();
        out.sort_by(|x, y| x.a.cmp(&y.a).then_with(|| x.b.cmp(&y.b)));
        out
    }
}

impl Acc {
    fn rates(&self) -> Vec<(&str, f64, u32)> {
        let mut v: Vec<(&str, f64, u32)> = self
            .weighted
            .iter()
            .map(|(p, &(x, n))| (p.as_str(), if self.w > 0.0 { x / self.w } else { 0.0 }, n))
            .collect();
        // Highest rate first; ties by path so grades are deterministic.
        v.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        v
    }

    fn n_eff(&self) -> f64 {
        if self.w2 > 0.0 {
            self.w * self.w / self.w2
        } else {
            0.0
        }
    }
}

/// Grades by change mass: by default Hot holds the first 50% of mass, Warm the next 25%, Mild the
/// next 15%, Cool the rest with any change (`cuts`: the cumulative shares closing Hot, Warm and
/// Mild). Returns grades parallel to `sorted` and the band edges.
fn mass_grades(sorted: &[(&str, f64, u32)], cuts: [f64; 3]) -> (Vec<u8>, [f64; 4]) {
    let total: f64 = sorted.iter().map(|x| x.1).sum();
    let mut acc = 0.0;
    let mut grades = Vec::with_capacity(sorted.len());
    let (mut low, mut high) = ([f64::INFINITY; 5], [0f64; 5]);
    for &(_, r, _) in sorted {
        let g = if r <= 0.0 || total <= 0.0 {
            0
        } else {
            // A small tolerance keeps exact shares (e.g. 10 of 20) from flipping on rounding.
            let share = acc / total + 1e-9;
            if share < cuts[0] {
                4
            } else if share < cuts[1] {
                3
            } else if share < cuts[2] {
                2
            } else {
                1
            }
        };
        acc += r;
        low[g as usize] = low[g as usize].min(r);
        high[g as usize] = high[g as usize].max(r);
        grades.push(g);
    }
    (grades, edges(&low, &high))
}

/// Lower edge of each grade 1..=4: halfway between its lowest rate and the highest rate below it.
/// An empty grade gets the edge of the grade above it (a zero-width band).
fn edges(low: &[f64; 5], high: &[f64; 5]) -> [f64; 4] {
    let mut e = [0.0; 4];
    let mut above = f64::INFINITY;
    for g in (1..=4).rev() {
        if low[g].is_finite() {
            let below = (0..g)
                .rev()
                .find(|&b| b == 0 || low[b].is_finite())
                .map(|b| high[b])
                .unwrap_or(0.0);
            e[g - 1] = (low[g] + below) / 2.0;
            above = e[g - 1];
        } else {
            e[g - 1] = above;
        }
    }
    // Grades absent at the top (no hot files at all) fall back to "unreachable".
    for x in e.iter_mut() {
        if !x.is_finite() {
            *x = f64::MAX;
        }
    }
    e
}

/// Wilson 95% interval for proportion `p` on `n` effective samples.
pub fn wilson(p: f64, n: f64) -> [f64; 2] {
    if n <= 0.0 {
        return [0.0, 1.0];
    }
    let z = 1.96f64;
    let z2 = z * z;
    let denom = 1.0 + z2 / n;
    let center = (p + z2 / (2.0 * n)) / denom;
    let half = z * ((p * (1.0 - p) / n + z2 / (4.0 * n * n)).max(0.0)).sqrt() / denom;
    [(center - half).max(0.0), (center + half).min(1.0)]
}

fn band_of(rate: f64, bands: &[f64; 4]) -> usize {
    bands.iter().filter(|&&b| rate >= b).count()
}

/// Settled when the whole interval falls in the band of the file's grade.
fn settled(ci: [f64; 2], grade: u8, bands: &[f64; 4]) -> bool {
    grade > 0 && band_of(ci[0], bands) == grade as usize && band_of(ci[1], bands) == grade as usize
}

fn settled_mass(sorted: &[(&str, f64, u32)], grades: &[u8], bands: &[f64; 4], n_eff: f64) -> f64 {
    let total: f64 = sorted.iter().map(|x| x.1).sum();
    if total <= 0.0 {
        return 0.0;
    }
    let s: f64 = sorted
        .iter()
        .zip(grades)
        .filter(|((_, r, _), g)| settled(wilson(*r, n_eff), **g, bands))
        .map(|((_, r, _), _)| r)
        .sum();
    s / total
}

/// Pulls commits (newest first) until the grades settle or a limit is hit. `now_ms` anchors ages.
pub fn grade<I>(history: I, now_ms: i64, limits: Limits) -> ChurnResult
where
    I: IntoIterator<Item = CommitTouch>,
{
    let mut acc = Acc::default();
    let mut co = CoAcc::default();
    let mut renames = Renames::default();
    let mut checks = Vec::new();
    let mut scanned = 0;
    let mut oldest = None;
    let mut stop = Stop::HistoryExhausted;
    let mut last_bands: Option<[f64; 4]> = None;
    let mut stable_batches = 0;
    for c in history {
        let age = ((now_ms - c.time_ms) as f64 / DAY_MS).max(0.0);
        if age > limits.max_age_days {
            stop = Stop::MaxAge;
            break;
        }
        if scanned >= limits.max_commits {
            stop = Stop::MaxCommits;
            break;
        }
        scanned += 1;
        oldest = Some(c.time_ms);
        // This commit's own touches use the names after its renames; older commits the new names.
        renames.record(&c.renames);
        let mut files: Vec<&str> = c.files.iter().map(|f| renames.resolve(f)).collect();
        files.sort_unstable();
        files.dedup();
        if !files.is_empty() {
            let n = files.len();
            let w = 2f64.powf(-age / HALF_LIFE_DAYS)
                * if n > MASS_REFACTOR_FILES {
                    1.0 / (1.0 + n as f64).log2()
                } else {
                    1.0
                };
            acc.w += w;
            acc.w2 += w * w;
            for f in &files {
                let e = acc.weighted.entry(f.to_string()).or_insert((0.0, 0));
                e.0 += w;
                e.1 += 1;
            }
            if n <= MASS_REFACTOR_FILES {
                co.add(&files, w);
            }
        }
        if scanned % limits.batch == 0 && scanned >= limits.min_commits {
            let sorted = acc.rates();
            let (grades, bands) = mass_grades(&sorted, limits.mass_cuts);
            let moved_little = last_bands.is_some_and(|lb| {
                lb.iter()
                    .zip(bands)
                    .all(|(a, b)| (a - b).abs() <= 0.1 * a.abs().max(b.abs()))
            });
            stable_batches = if moved_little { stable_batches + 1 } else { 0 };
            last_bands = Some(bands);
            let mass = settled_mass(&sorted, &grades, &bands, acc.n_eff());
            checks.push(BatchCheck {
                scanned,
                settled_mass: mass,
                bands,
                stable_batches,
            });
            if stable_batches >= 2 && mass >= 0.9 {
                stop = Stop::Settled;
                break;
            }
        }
    }
    let sorted = acc.rates();
    let (grades, bands) = mass_grades(&sorted, limits.mass_cuts);
    let n_eff = acc.n_eff();
    let mass = settled_mass(&sorted, &grades, &bands, n_eff);
    let files = sorted
        .iter()
        .zip(&grades)
        .map(|(&(p, r, n), &g)| {
            let ci = wilson(r, n_eff);
            (
                p.to_string(),
                FileHeat {
                    grade: g,
                    rate: r,
                    ci,
                    settled: settled(ci, g, &bands),
                    touches: n,
                },
            )
        })
        .collect();
    ChurnResult {
        files,
        scanned,
        oldest_ms: oldest,
        settled_mass: mass,
        stop,
        bands,
        effective_sample: n_eff,
        renames_followed: renames.followed,
        co_change: co.finish(),
        checks,
    }
}

/// In-range touches per file (head names, renames followed) and the number of commits.
/// `commits` is newest first, like the history walk.
pub fn range_heat<I>(commits: I) -> (HashMap<String, u32>, u32)
where
    I: IntoIterator<Item = CommitTouch>,
{
    let mut renames = Renames::default();
    let mut out: HashMap<String, u32> = HashMap::new();
    let mut n = 0;
    for c in commits {
        n += 1;
        renames.record(&c.renames);
        let mut files: Vec<&str> = c.files.iter().map(|f| renames.resolve(f)).collect();
        files.sort_unstable();
        files.dedup();
        for f in files {
            *out.entry(f.to_string()).or_default() += 1;
        }
    }
    (out, n)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400_000;

    fn run(commits: &[(i64, Vec<&str>)], limits: Limits) -> ChurnResult {
        let touches = commits.iter().map(|(t, f)| CommitTouch {
            time_ms: *t,
            files: f.iter().map(|s| s.to_string()).collect(),
            renames: vec![],
        });
        grade(touches, 1000 * DAY, limits)
    }

    #[test]
    fn mass_quantiles_grade_hot_to_cool() {
        // a: 10 touches, b: 5, c: 3, d: 1, e: 1 — all recent.
        let mut cs = Vec::new();
        for i in 0..10 {
            let mut f = vec!["a"];
            if i < 5 {
                f.push("b");
            }
            if i < 3 {
                f.push("c");
            }
            if i == 0 {
                f.push("d");
                f.push("e");
            }
            cs.push((1000 * DAY - i, f));
        }
        let r = run(&cs, Limits::default());
        let g = |p: &str| r.files[p].grade;
        // Mass 20: a holds the first 50% → hot; b the next 25% → warm; c → mild; d, e → cool.
        assert_eq!((g("a"), g("b"), g("c"), g("d"), g("e")), (4, 3, 2, 1, 1));
        assert_eq!(r.stop, Stop::HistoryExhausted);
        assert!(r.bands[0] <= r.bands[1] && r.bands[1] <= r.bands[2] && r.bands[2] <= r.bands[3]);
        // D6: configurable cut points. Hot holds 80% of the mass, Warm the next 10%, Mild 5%.
        let r = run(
            &cs,
            Limits {
                mass_cuts: [0.8, 0.9, 0.95],
                ..Limits::default()
            },
        );
        let g = |p: &str| r.files[p].grade;
        assert_eq!((g("a"), g("b"), g("c"), g("d"), g("e")), (4, 4, 4, 2, 1));
    }

    #[test]
    fn mass_refactors_are_down_weighted_and_old_commits_fade() {
        let big: Vec<String> = (0..100).map(|i| format!("f{i}")).collect();
        let big: Vec<&str> = big.iter().map(|s| s.as_str()).collect();
        let r = run(
            &[
                (1000 * DAY, vec!["a"]),
                (1000 * DAY - 1, big),
                (1000 * DAY - 180 * DAY, vec!["old"]),
            ],
            Limits::default(),
        );
        assert!(r.files["a"].rate > r.files["f0"].rate * 6.0);
        assert!(r.files["old"].rate < r.files["a"].rate / 3.0);
    }

    #[test]
    fn stable_history_settles_before_500_commits() {
        // Steady rates: core every commit, a every 2nd, b every 10th, c every 50th.
        let cs: Vec<(i64, Vec<&str>)> = (0..2000)
            .map(|i| {
                let mut f = vec!["core"];
                if i % 2 == 0 {
                    f.push("a");
                }
                if i % 10 == 0 {
                    f.push("b");
                }
                if i % 50 == 0 {
                    f.push("c");
                }
                (1000 * DAY - i * 60_000, f)
            })
            .collect();
        let r = run(&cs, Limits::default());
        assert_eq!(
            r.stop,
            Stop::Settled,
            "scanned {} settled {}",
            r.scanned,
            r.settled_mass
        );
        assert!(r.scanned < 500);
    }

    #[test]
    fn drifting_history_does_not_stop_early() {
        // Activity moves to a new file every 20 commits, so the bands keep shifting.
        let names: Vec<String> = (0..200).map(|i| format!("f{i}")).collect();
        let cs: Vec<(i64, Vec<&str>)> = (0..1000)
            .map(|i| {
                (
                    1000 * DAY - i * 60_000,
                    vec![
                        names[(i / 20) as usize].as_str(),
                        names[(i / 7) as usize % 200].as_str(),
                    ],
                )
            })
            .collect();
        let r = run(&cs, Limits::default());
        assert_ne!(r.stop, Stop::Settled, "scanned {}", r.scanned);
    }

    #[test]
    fn renames_carry_history() {
        // Newest first: a → b renamed at t=5; older commits touch a.
        let c = |t: i64, files: &[&str], renames: &[(&str, &str)]| CommitTouch {
            time_ms: 1000 * DAY - t,
            files: files.iter().map(|s| s.to_string()).collect(),
            renames: renames
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
        };
        let hist = vec![
            c(1, &["b", "x"], &[]),
            c(5, &["a", "b"], &[("a", "b")]),
            c(6, &["a"], &[]),
            c(7, &["a", "x"], &[]),
            // Before that, `a` was itself renamed from `old/a`.
            c(8, &["old/a", "a"], &[("old/a", "a")]),
            c(9, &["old/a"], &[]),
        ];
        let r = grade(hist.clone(), 1000 * DAY, Limits::default());
        assert_eq!(r.files["b"].touches, 6);
        assert!(!r.files.contains_key("a") && !r.files.contains_key("old/a"));
        assert_eq!(r.renames_followed, 2);
        // x and b change together in 2 of the commits touching either.
        let pair = r
            .co_change
            .iter()
            .find(|p| p.a == "b" && p.b == "x")
            .unwrap();
        assert_eq!(pair.support, 2);
        assert!((pair.jaccard - 2.0 / 6.0).abs() < 0.01, "{}", pair.jaccard);
        // Range heat over the first three commits, renames followed.
        let (touches, n) = range_heat(hist.into_iter().take(3));
        assert_eq!(n, 3);
        assert_eq!(touches["b"], 3);
        assert_eq!(touches["x"], 1);
    }

    #[test]
    fn co_change_skips_mass_refactors() {
        let big: Vec<String> = (0..60).map(|i| format!("f{i}")).collect();
        let mut hist = Vec::new();
        for i in 0..3 {
            hist.push(CommitTouch {
                time_ms: 1000 * DAY - i,
                files: big.clone(),
                renames: vec![],
            });
        }
        let r = grade(hist, 1000 * DAY, Limits::default());
        assert!(r.co_change.is_empty());
    }

    #[test]
    fn limits_stop_the_walk() {
        let cs: Vec<(i64, Vec<&str>)> = (0..100)
            .map(|i| (1000 * DAY - i * DAY * 10, vec!["x"]))
            .collect();
        let r = run(&cs, Limits::default());
        assert_eq!(r.stop, Stop::MaxAge);
        let r = run(
            &cs,
            Limits {
                max_commits: 10,
                ..Limits::default()
            },
        );
        assert_eq!((r.stop, r.scanned), (Stop::MaxCommits, 10));
    }
}
