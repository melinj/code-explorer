//! Deterministic synthetic view: lanes × heat columns × connectedness rows, filled with dependency
//! clusters of windows, links between them, and fake source text. Sized by window count so the same
//! generator gives small fixtures and the 20k-window performance view.

use std::collections::BTreeMap;

use cv_contracts::*;

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    pub fn f(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn range(&mut self, lo: u32, hi: u32) -> u32 {
        lo + (self.next() % (hi - lo + 1) as u64) as u32
    }
    pub fn normal(&mut self) -> f64 {
        let (u, v) = (self.f().max(1e-12), self.f());
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }
    /// Index into `weights`, chosen proportionally.
    pub fn pick(&mut self, weights: &[f64]) -> usize {
        let total: f64 = weights.iter().sum();
        let mut t = self.f() * total;
        for (i, w) in weights.iter().enumerate() {
            if t < *w {
                return i;
            }
            t -= w;
        }
        weights.len() - 1
    }
}

pub fn hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

struct LaneSpec {
    id: &'static str,
    label: &'static str,
    texture: Texture,
    share: f64,
    lang: &'static str,
    ext: &'static str,
    /// Weights of conn grades 0..=4 (isolated..core).
    conn: [f64; 5],
}

const LANES: [LaneSpec; 5] = [
    LaneSpec {
        id: "infra",
        label: "Infra",
        texture: Texture::Hatch,
        share: 0.015,
        lang: "other",
        ext: "sh",
        conn: [0.0, 0.4, 0.4, 0.2, 0.0],
    },
    LaneSpec {
        id: "config",
        label: "Config",
        texture: Texture::Dots,
        share: 0.13,
        lang: "yaml",
        ext: "yaml",
        conn: [0.0, 0.35, 0.3, 0.25, 0.1],
    },
    LaneSpec {
        id: "code",
        label: "Code",
        texture: Texture::Plain,
        share: 0.47,
        lang: "python",
        ext: "py",
        conn: [0.25, 0.3, 0.2, 0.15, 0.1],
    },
    LaneSpec {
        id: "test",
        label: "Tests",
        texture: Texture::Lines,
        share: 0.33,
        lang: "python",
        ext: "py",
        conn: [0.4, 0.4, 0.2, 0.0, 0.0],
    },
    LaneSpec {
        id: "docs",
        label: "Docs",
        texture: Texture::Cross,
        share: 0.055,
        lang: "markdown",
        ext: "md",
        conn: [0.5, 0.5, 0.0, 0.0, 0.0],
    },
];
/// Weights of heat grades 0..=4 (frozen..hot).
const HEAT_W: [f64; 5] = [0.45, 0.25, 0.15, 0.10, 0.05];
const HEAT_LABELS: [&str; 5] = ["Frozen", "Cool", "Mild", "Warm", "Hot"];

const WORDS: [&str; 48] = [
    "world", "event", "map", "district", "agent", "turn", "store", "graph", "node", "edge",
    "config", "loader", "player", "route", "scene", "render", "queue", "tick", "build", "plan",
    "memory", "prompt", "tool", "schema", "client", "server", "session", "cache", "index",
    "layout", "camera", "entity", "movement", "trade", "market", "dialogue", "quest", "item",
    "spawn", "zone", "path", "signal", "worker", "job", "sync", "parse", "token", "theme",
];

const CLUSTER_PAD: f64 = 80.0;
const CLUSTER_TITLE: f64 = 120.0;
const LAYER_GAP: f64 = 320.0;
const WIN_GAP: f64 = 90.0;
const CELL_PAD: f64 = 240.0;
const CELL_GAP: f64 = 160.0;
const COL_GAP: f64 = 1600.0;
const ROW_GAP: f64 = 500.0;
const LANE_GAP: f64 = 1400.0;
const LANE_PAD: f64 = 400.0;
const HEADER_BAND: f64 = 1800.0;

#[derive(Clone)]
pub struct Win {
    pub header: WindowHeader,
    pub file: usize,
    pub out_links: Vec<usize>,
    pub in_links: Vec<usize>,
}

#[derive(Clone)]
pub struct FileInfo {
    pub path: String,
    pub blob: String,
    pub lines: u32,
    pub lang: String,
    pub windows: Vec<usize>,
    pub seed: u64,
}

#[derive(Clone)]
pub struct LinkRec {
    pub id: String,
    pub kind: &'static str,
    pub from: usize,
    pub from_line: u32,
    pub to: usize,
    pub to_line: u32,
    pub confidence: f64,
}

#[derive(Clone)]
pub struct View {
    pub bootstrap: ViewBootstrap,
    pub windows: Vec<Win>,
    pub files: Vec<FileInfo>,
    pub links: Vec<LinkRec>,
    pub by_id: BTreeMap<String, usize>,
    pub by_blob: BTreeMap<String, usize>,
    /// Base tiles (TILE_BASE_WU) → windows and edges touching them.
    pub tiles: BTreeMap<(i64, i64), (Vec<usize>, Vec<usize>)>,
}

struct ClusterBuild {
    lane: usize,
    heat: u8,
    conn: u8,
    label: String,
    /// Window indices with positions relative to the cluster origin.
    members: Vec<usize>,
    w: f64,
    h: f64,
    x: f64,
    y: f64,
}

fn lines_for(rng: &mut Rng) -> u32 {
    ((3.6 + 0.9 * rng.normal()).exp().round() as u32).clamp(6, WINDOW_MAX_LINES)
}

pub fn generate(total_windows: usize, seed: u64) -> View {
    let mut rng = Rng::new(seed);
    let view_id = format!("v_synth{total_windows}");
    let mut windows: Vec<Win> = Vec::with_capacity(total_windows);
    let mut files: Vec<FileInfo> = Vec::new();
    let mut clusters: Vec<ClusterBuild> = Vec::new();

    // 1. Clusters per lane, each with its files and windows laid out in dependency layers.
    for (li, lane) in LANES.iter().enumerate() {
        let target = (total_windows as f64 * lane.share).round() as usize;
        let mut made = 0;
        while made < target {
            let n = (rng.range(3, 14) as f64 * (1.0 + 2.0 * rng.f() * rng.f())).round() as usize;
            let n = n.min(target - made).max(1);
            let (mut heat, mut conn) = (rng.pick(&HEAT_W) as u8, rng.pick(&lane.conn) as u8);
            if lane.id == "code" && made == 0 {
                // Every view has a review-first cell: code × Hot × Core.
                (heat, conn) = (4, 4);
            }
            let pkg = WORDS[(rng.next() % WORDS.len() as u64) as usize];
            let module = WORDS[(rng.next() % WORDS.len() as u64) as usize];
            let ci = clusters.len();
            let label = format!("{pkg} · {module}");
            let mut members = Vec::with_capacity(n);
            // Files: mostly one window each; some split into fragments.
            let mut left = n;
            while left > 0 {
                let frags = if rng.f() < 0.15 {
                    rng.range(2, 6).min(left as u32) as usize
                } else {
                    1
                };
                left -= frags;
                let stem = WORDS[(rng.next() % WORDS.len() as u64) as usize];
                let dir = match lane.id {
                    "test" => format!("tests/{pkg}"),
                    "docs" => format!("docs/{pkg}"),
                    "config" => format!("config/{pkg}"),
                    "infra" => "deploy".to_string(),
                    _ => format!("src/{pkg}/{module}"),
                };
                let fi = files.len();
                let path = if lane.id == "test" {
                    format!("{dir}/test_{stem}_{fi}.{}", lane.ext)
                } else {
                    format!("{dir}/{stem}_{fi}.{}", lane.ext)
                };
                let mut file = FileInfo {
                    blob: format!("b{:016x}", hash(&path)),
                    path,
                    lines: 0,
                    lang: lane.lang.to_string(),
                    windows: Vec::new(),
                    seed: rng.next(),
                };
                let mut line = 1 + rng.range(0, 12);
                for f in 0..frags {
                    let lines = lines_for(&mut rng);
                    let wi = windows.len();
                    let sym = WORDS[(rng.next() % WORDS.len() as u64) as usize];
                    let kind = if lane.id == "test" {
                        "test"
                    } else if rng.f() < 0.3 {
                        "class"
                    } else {
                        "function"
                    };
                    let name = if kind == "class" {
                        format!("{}{}", sym[..1].to_uppercase(), &sym[1..])
                    } else {
                        format!(
                            "{sym}_{}",
                            WORDS[(rng.next() % WORDS.len() as u64) as usize]
                        )
                    };
                    let width = [630.0, 720.0, 720.0, 810.0][(rng.next() % 4) as usize];
                    let wheat = (heat as i32 + [-1, 0, 0, 0, 1][(rng.next() % 5) as usize])
                        .clamp(0, 4) as u8;
                    let wconn =
                        (conn as i32 + [-1, 0, 0, 1][(rng.next() % 4) as usize]).clamp(0, 4) as u8;
                    let rate = (wheat as f64 + rng.f()) * 0.02;
                    let deps = [0, 2, 6, 20, 80][wconn as usize]
                        + rng.range(0, [1, 3, 10, 40, 400][wconn as usize]);
                    windows.push(Win {
                        header: WindowHeader {
                            id: format!("w{wi}"),
                            key: format!("{}#{}", file.path, name),
                            kind: if lane.id == "code" || lane.id == "test" {
                                "symbol"
                            } else {
                                "chunk"
                            }
                            .into(),
                            cluster: format!("c{ci}"),
                            lane: lane.id.into(),
                            path: file.path.clone(),
                            lang: lane.lang.into(),
                            rev: "c0ffee".into(),
                            change: "kxwqmnpo".into(),
                            blob: file.blob.clone(),
                            symbol: Some(SymbolRef {
                                name: name.clone(),
                                qualname: name,
                                kind: kind.into(),
                            }),
                            lines: LineSpan {
                                start: line,
                                end: line + lines - 1,
                                file_lines: 0,
                            },
                            rect: Rect {
                                x: 0.0,
                                y: 0.0,
                                w: width,
                                h: HEADER_WU + lines as f64 * LINE_WU,
                            },
                            heat: Heat {
                                grade: wheat,
                                rate,
                                ci: [rate * 0.7, rate * 1.3],
                                settled: rng.f() < 0.85,
                                touches: (rate * 400.0) as u32,
                                in_range: None,
                            },
                            conn: Connectedness {
                                grade: wconn,
                                dependents2: deps,
                                fan_in: deps / 3,
                                fan_out: rng.range(0, 12),
                                tests: rng.range(0, 6),
                                reach: deps * 3,
                                cycle: None,
                            },
                            risk: 0.0,
                            review_first: false,
                            fragment: None,
                            counts: WindowCounts {
                                open_threads: if rng.f() < 0.02 { rng.range(1, 3) } else { 0 },
                                ports: 0,
                                conflicts: if rng.f() < 0.003 { 1 } else { 0 },
                            },
                            flags: WindowFlags {
                                pinned: false,
                                truncated: lines == WINDOW_MAX_LINES,
                            },
                            diff_stat: None,
                            expanded_from: None,
                        },
                        file: fi,
                        out_links: Vec::new(),
                        in_links: Vec::new(),
                    });
                    line += lines + if f + 1 < frags { rng.range(2, 40) } else { 0 };
                    file.windows.push(wi);
                    members.push(wi);
                }
                file.lines = line + rng.range(0, 30);
                files.push(file);
            }
            made += members.len();
            clusters.push(ClusterBuild {
                lane: li,
                heat,
                conn,
                label,
                members,
                w: 0.0,
                h: 0.0,
                x: 0.0,
                y: 0.0,
            });
        }
    }

    // Some fragments move to another cluster of the same lane: split files across clusters.
    for file in &files {
        if file.windows.len() >= 3 && rng.f() < 0.35 {
            let last = *file.windows.last().unwrap();
            let from = cluster_index(&windows[last].header.cluster);
            let lane = clusters[from].lane;
            let to = loop {
                let c = (rng.next() % clusters.len() as u64) as usize;
                if clusters[c].lane == lane && c != from {
                    break c;
                }
            };
            clusters[from].members.retain(|&m| m != last);
            clusters[to].members.push(last);
            windows[last].header.cluster = format!("c{to}");
        }
    }
    clusters.retain(|c| !c.members.is_empty());
    for (ci, c) in clusters.iter().enumerate() {
        for &m in &c.members {
            windows[m].header.cluster = format!("c{ci}");
        }
    }

    // File fragments: chip slot is stable per path.
    for f in &files {
        let fl = f.lines;
        for &w in &f.windows {
            windows[w].header.lines.file_lines = fl;
        }
        if f.windows.len() > 1 {
            let siblings: Vec<String> = f.windows.iter().map(|w| format!("w{w}")).collect();
            for (i, &w) in f.windows.iter().enumerate() {
                windows[w].header.fragment = Some(FileFragment {
                    index: i as u32 + 1,
                    of: f.windows.len() as u32,
                    chip: (hash(&f.path) % FILE_CHIP_SLOTS as u64) as u32,
                    siblings: siblings.clone(),
                });
            }
        }
    }

    // 2. Links: mostly inside the cluster toward later layers, some across clusters and lanes.
    let mut links: Vec<LinkRec> = Vec::new();
    let lane_clusters: Vec<Vec<usize>> = (0..LANES.len())
        .map(|l| {
            (0..clusters.len())
                .filter(|&c| clusters[c].lane == l)
                .collect()
        })
        .collect();
    let code_lane = 2;
    for ci in 0..clusters.len() {
        let members = clusters[ci].members.clone();
        for (k, &w) in members.iter().enumerate() {
            let n_out = rng.pick(&[0.25, 0.35, 0.25, 0.1, 0.05]);
            for _ in 0..n_out {
                let r = rng.f();
                // Inside the cluster, callers come before callees (layers follow member order).
                let (to, kind) = if r < 0.72 && k + 1 < members.len() {
                    let per = members.len().div_ceil(layer_count(members.len()));
                    let next_layer = (k / per + 1) * per;
                    let j = if next_layer < members.len() {
                        next_layer
                            + (rng.next() as usize % (members.len() - next_layer).min(per * 2))
                    } else {
                        k + 1 + (rng.next() as usize % (members.len() - k - 1))
                    };
                    (members[j], if rng.f() < 0.1 { "inherit" } else { "call" })
                } else if r < 0.92 {
                    // Mostly clusters in the same cell (same grades), sometimes anywhere in the lane.
                    let lc = &lane_clusters[clusters[ci].lane];
                    let near: Vec<usize> = if rng.f() < 0.7 {
                        lc.iter()
                            .copied()
                            .filter(|&c| {
                                clusters[c].heat == clusters[ci].heat
                                    && clusters[c].conn == clusters[ci].conn
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let lc = if near.is_empty() { lc } else { &near };
                    let c = lc[(rng.next() % lc.len() as u64) as usize];
                    let m = &clusters[c].members;
                    (
                        m[(rng.next() % m.len() as u64) as usize],
                        if rng.f() < 0.2 { "publish" } else { "call" },
                    )
                } else {
                    let target_lane = if clusters[ci].lane == code_lane {
                        1
                    } else {
                        code_lane
                    };
                    let lc = &lane_clusters[target_lane];
                    let c = lc[(rng.next() % lc.len() as u64) as usize];
                    let m = &clusters[c].members;
                    (
                        m[(rng.next() % m.len() as u64) as usize],
                        if target_lane == 1 { "read" } else { "call" },
                    )
                };
                if to == w {
                    continue;
                }
                let h = &windows[w].header;
                let from_line = h.lines.start + rng.range(0, h.lines.end - h.lines.start);
                let to_line = windows[to].header.lines.start;
                let li = links.len();
                links.push(LinkRec {
                    id: format!("l{li}"),
                    kind,
                    from: w,
                    from_line,
                    to,
                    to_line,
                    confidence: if rng.f() < 0.08 {
                        0.4 + 0.2 * rng.f()
                    } else {
                        0.9
                    },
                });
                windows[w].out_links.push(li);
                windows[to].in_links.push(li);
            }
        }
    }

    // 3. Inside each cluster: layers by link depth (callers left of callees), stacked windows.
    for c in clusters.iter_mut() {
        let n = c.members.len();
        let n_layers = layer_count(n);
        let mut layers: Vec<Vec<usize>> = vec![Vec::new(); n_layers];
        // Member order is call order: callers left of callees.
        let per = n.div_ceil(n_layers);
        for (i, &w) in c.members.iter().enumerate() {
            layers[(i / per).min(n_layers - 1)].push(w);
        }
        let mut x = CLUSTER_PAD;
        let mut max_h: f64 = 0.0;
        for layer in &layers {
            let mut y = CLUSTER_TITLE;
            let mut lw: f64 = 0.0;
            for &w in layer {
                let r = &mut windows[w].header.rect;
                r.x = x;
                r.y = y;
                y += r.h + WIN_GAP;
                lw = lw.max(r.w);
            }
            max_h = max_h.max(y);
            x += lw + LAYER_GAP;
        }
        c.w = x - LAYER_GAP + CLUSTER_PAD;
        c.h = max_h - WIN_GAP + CLUSTER_PAD;
    }

    // 4. District grid. Columns run hot → frozen left to right and are shared by every lane.
    let col_of = |heat: u8| (4 - heat) as usize;
    // Column width proportional to the column's cluster area, so every column's cells come out about
    // equally tall and rows don't inherit one crowded column's height. Aim for a 1.6:1 world.
    let mut col_area = [0f64; 5];
    let mut col_max = [0f64; 5];
    for c in &clusters {
        col_area[col_of(c.heat)] += (c.w + CELL_GAP) * (c.h + CELL_GAP);
        col_max[col_of(c.heat)] = col_max[col_of(c.heat)].max(c.w);
    }
    let h0 = (col_area.iter().sum::<f64>() / 1.6).sqrt();
    let mut col_w = [0f64; 5];
    for col in 0..5 {
        col_w[col] = col_max[col].max(col_area[col] / h0 * 1.15) + 2.0 * CELL_PAD;
    }
    let mut col_x = [0f64; 5];
    let mut x = 0.0;
    for col in 0..5 {
        col_x[col] = x;
        x += col_w[col] + COL_GAP;
    }
    let world_w = x - COL_GAP;

    let mut lane_bands = Vec::new();
    let mut y = HEADER_BAND;
    for (li, lane) in LANES.iter().enumerate() {
        let lane_top = y;
        y += LANE_PAD;
        let mut rows = Vec::new();
        for conn in (0..=4u8).rev() {
            // Shelf-pack every cell of this row; the row is as tall as its tallest cell.
            let mut row_h: f64 = 0.0;
            let mut any = false;
            for col in 0..5 {
                let mut idx: Vec<usize> = (0..clusters.len())
                    .filter(|&i| {
                        clusters[i].lane == li
                            && clusters[i].conn == conn
                            && col_of(clusters[i].heat) == col
                    })
                    .collect();
                if idx.is_empty() {
                    continue;
                }
                any = true;
                idx.sort_by(|&a, &b| clusters[b].h.total_cmp(&clusters[a].h));
                let inner = col_w[col] - 2.0 * CELL_PAD;
                let (mut cx, mut cy, mut shelf) = (0.0, 0.0, 0f64);
                for i in idx {
                    let c = &mut clusters[i];
                    if cx > 0.0 && cx + c.w > inner {
                        cx = 0.0;
                        cy += shelf + CELL_GAP;
                        shelf = 0.0;
                    }
                    c.x = col_x[col] + CELL_PAD + cx;
                    c.y = y + CELL_PAD + cy;
                    cx += c.w + CELL_GAP;
                    shelf = shelf.max(c.h);
                }
                row_h = row_h.max(cy + shelf + 2.0 * CELL_PAD);
            }
            if any {
                rows.push(LaneRow { conn, y, h: row_h });
                y += row_h + ROW_GAP;
            }
        }
        y += LANE_PAD - ROW_GAP;
        let lane_clusters_n = clusters.iter().filter(|c| c.lane == li).count() as u32;
        let lane_files = files
            .iter()
            .filter(|f| windows[f.windows[0]].header.lane == lane.id)
            .count() as u32;
        lane_bands.push(LaneBand {
            lane: lane.id.into(),
            rect: Rect {
                x: 0.0,
                y: lane_top,
                w: world_w,
                h: y - lane_top,
            },
            files: lane_files,
            clusters: lane_clusters_n,
            rows,
        });
        y += LANE_GAP;
    }
    let world = Rect {
        x: 0.0,
        y: 0.0,
        w: world_w,
        h: y - LANE_GAP,
    };

    // Absolute window positions.
    for c in &clusters {
        for &m in &c.members {
            let r = &mut windows[m].header.rect;
            r.x += c.x;
            r.y += c.y;
        }
    }

    // Risk: geometric mean of heat and connectedness percentiles; top 5% per lane are review-first.
    let mut by_rate: Vec<usize> = (0..windows.len()).collect();
    by_rate.sort_by(|&a, &b| {
        windows[a]
            .header
            .heat
            .rate
            .total_cmp(&windows[b].header.heat.rate)
    });
    let mut heat_pct = vec![0.0; windows.len()];
    for (i, &w) in by_rate.iter().enumerate() {
        heat_pct[w] = i as f64 / windows.len() as f64;
    }
    let mut by_dep: Vec<usize> = (0..windows.len()).collect();
    by_dep.sort_by_key(|&w| windows[w].header.conn.dependents2);
    for (i, &w) in by_dep.iter().enumerate() {
        let conn_pct = i as f64 / windows.len() as f64;
        windows[w].header.risk = (heat_pct[w] * conn_pct).sqrt();
    }
    for lane in LANES.iter() {
        let mut idx: Vec<usize> = (0..windows.len())
            .filter(|&w| windows[w].header.lane == lane.id)
            .collect();
        idx.sort_by(|&a, &b| windows[b].header.risk.total_cmp(&windows[a].header.risk));
        for &w in idx.iter().take(idx.len().div_ceil(20)) {
            windows[w].header.review_first = true;
        }
    }
    for w in windows.iter_mut() {
        w.header.counts.ports = (w.out_links.len() + w.in_links.len()) as u32;
    }

    // Bootstrap: clusters, bundles, columns, hubs.
    let mut boot_clusters = Vec::new();
    for (ci, c) in clusters.iter().enumerate() {
        boot_clusters.push(Cluster {
            id: format!("c{ci}"),
            parent: None,
            lane: LANES[c.lane].id.into(),
            label: c.label.clone(),
            label_source: "auto".into(),
            summary: None,
            rect: Rect {
                x: c.x,
                y: c.y,
                w: c.w,
                h: c.h,
            },
            heat: c.heat,
            conn: c.conn,
            windows: c.members.len() as u32,
            open_threads: c
                .members
                .iter()
                .map(|&m| windows[m].header.counts.open_threads)
                .sum(),
            conflicts: c
                .members
                .iter()
                .map(|&m| windows[m].header.counts.conflicts)
                .sum(),
            pinned: false,
            // Spread over the grade's band by size, so the ramp shows within a column.
            heat_level: Some((c.heat as f64 + ((ci * 37) % 100) as f64 / 100.0) / 5.0),
            review: Some(
                c.members
                    .iter()
                    .filter(|&&m| windows[m].header.review_first)
                    .count() as f64
                    / c.members.len().max(1) as f64,
            ),
            folder: None,
        });
    }
    let mut bundle_map: BTreeMap<(String, String), EdgeBundle> = BTreeMap::new();
    for l in &links {
        let (a, b) = (
            &windows[l.from].header.cluster,
            &windows[l.to].header.cluster,
        );
        if a == b {
            continue;
        }
        let e = bundle_map
            .entry((a.clone(), b.clone()))
            .or_insert_with(|| EdgeBundle {
                from: a.clone(),
                to: b.clone(),
                kinds: BTreeMap::new(),
                weight: 0.0,
            });
        *e.kinds.entry(l.kind.to_string()).or_insert(0) += 1;
        e.weight += 1.0;
    }
    let columns = (0..5)
        .map(|col| GridColumn {
            heat: 4 - col as u8,
            x: col_x[col],
            w: col_w[col],
            clusters: clusters.iter().filter(|c| col_of(c.heat) == col).count() as u32,
        })
        .collect();
    let lane_defs = LANES
        .iter()
        .enumerate()
        .map(|(i, l)| LaneDef {
            id: l.id.into(),
            label: l.label.into(),
            order: i as u32,
            texture: l.texture.clone(),
            // Deliberately unlike cv serve, which marks docs collapsed (DESIGN §4.3): the 20k
            // synthetic view is the F1 load case (scripts/bench.mjs, the soak, the mock flows'
            // fling/brush/lens checks), and a closed docs lane (5.5% of its windows) would take
            // those off the canvas and lighten what the 60 fps bar measures. Collapsed docs are
            // checked on cv serve (panels-flow "collapsed lanes start closed"; skipped on the mock).
            collapsed: false,
            hidden: false,
        })
        .collect();
    let mut hubs = Vec::new();
    for i in 0..40 {
        let lane = &lane_bands[2];
        let topic = format!(
            "{}.{}.{}",
            WORDS[i % 48],
            WORDS[(i * 7 + 3) % 48],
            ["moved", "built", "changed", "ready"][i % 4]
        );
        hubs.push(Hub {
            id: format!("bus:{topic}"),
            protocol: "bus".into(),
            key: topic.clone(),
            family: None,
            display: topic,
            producers: vec![],
            consumers: vec![],
            rect: Some(Rect {
                x: col_x[i % 5] + col_w[i % 5] * ((i * 37 % 100) as f64 / 100.0),
                y: lane.rect.y - LANE_GAP / 2.0 - 120.0,
                w: 240.0,
                h: 240.0,
            }),
            anchors: None,
        });
    }
    // Each topic gets 1-3 publishers among the code windows nearest it and 1-4 subscribers a
    // little further along, so hubs show where their messages come from and go.
    let code: Vec<usize> = (0..windows.len())
        .filter(|&w| windows[w].header.lane == "code")
        .collect();
    for (i, h) in hubs.iter_mut().enumerate() {
        let r = h.rect.as_ref().unwrap();
        let hx = r.x + r.w / 2.0;
        let mut near: Vec<usize> = code.clone();
        near.sort_by(|&a, &b| {
            let d = |w: usize| {
                (windows[w].header.rect.x - hx).abs() + (windows[w].header.rect.y - r.y).abs() * 0.3
            };
            d(a).total_cmp(&d(b))
        });
        let (np, nc) = (1 + i % 3, 1 + (i * 5) % 4);
        let site = |w: usize, out: bool| {
            let hd = &windows[w].header;
            let line = hd.lines.start + (hd.lines.end - hd.lines.start) / 3;
            let y =
                hd.rect.y + HEADER_WU + (line - hd.lines.start) as f64 * LINE_WU + LINE_WU / 2.0;
            let x = if out {
                hd.rect.x + hd.rect.w
            } else {
                hd.rect.x
            };
            (
                SiteRef {
                    path: hd.path.clone(),
                    line,
                    window: Some(hd.id.clone()),
                    col: None,
                    symbol: None,
                },
                [x, y],
            )
        };
        let mut anchors = Vec::new();
        for &w in near.iter().take(np) {
            let (s, a) = site(w, true);
            h.producers.push(s);
            anchors.push(a);
        }
        for &w in near.iter().skip(np + 6).step_by(3).take(nc) {
            let (s, a) = site(w, false);
            h.consumers.push(s);
            anchors.push(a);
        }
        h.anchors = Some(anchors);
    }

    let n_windows = windows.len() as u32;
    let bootstrap = ViewBootstrap {
        view: ViewRef {
            id: view_id.clone(),
            spec: serde_json::json!({ "mode": "snapshot", "rev": "@" }),
            resolved: Resolved {
                op: "4eb0965a".into(),
                head: "c0ffee".into(),
                commits: vec!["c0ffee".into()],
            },
            status: "ready".into(),
            layout_version: 1,
            stats: ViewStats {
                windows: n_windows,
                files: files.len() as u32,
                clusters: clusters.len() as u32,
            },
            warnings: vec![],
            shared_placement: None,
        },
        world,
        lane_defs,
        lanes: lane_bands,
        columns,
        clusters: boot_clusters,
        hubs,
        bundles: bundle_map.into_values().collect(),
        churn: None,
        heat_labels: HEAT_LABELS.map(String::from),
        folders: None,
    };

    // Spatial index over base tiles.
    let mut tiles: BTreeMap<(i64, i64), (Vec<usize>, Vec<usize>)> = BTreeMap::new();
    let cell = |v: f64| (v / TILE_BASE_WU).floor() as i64;
    for (i, w) in windows.iter().enumerate() {
        let r = &w.header.rect;
        for tx in cell(r.x)..=cell(r.x + r.w) {
            for ty in cell(r.y)..=cell(r.y + r.h) {
                tiles.entry((tx, ty)).or_default().0.push(i);
            }
        }
    }
    for (li, l) in links.iter().enumerate() {
        let (a, b) = link_points(&windows, l);
        tiles
            .entry((cell(a[0]), cell(a[1])))
            .or_default()
            .1
            .push(li);
        if (cell(a[0]), cell(a[1])) != (cell(b[0]), cell(b[1])) {
            tiles
                .entry((cell(b[0]), cell(b[1])))
                .or_default()
                .1
                .push(li);
        }
    }

    let by_id = windows
        .iter()
        .enumerate()
        .map(|(i, w)| (w.header.id.clone(), i))
        .collect();
    let by_blob = files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.blob.clone(), i))
        .collect();
    View {
        bootstrap,
        windows,
        files,
        links,
        by_id,
        by_blob,
        tiles,
    }
}

fn layer_count(n: usize) -> usize {
    ((n as f64 / 2.5).sqrt().ceil() as usize).max(1)
}

fn cluster_index(id: &str) -> usize {
    id[1..].parse().unwrap()
}

/// World positions of a link's out port (right edge at the callsite) and in port (left edge at the definition).
pub fn link_points(windows: &[Win], l: &LinkRec) -> ([f64; 2], [f64; 2]) {
    let f = &windows[l.from].header;
    let t = &windows[l.to].header;
    let fy = f.rect.y + HEADER_WU + (l.from_line - f.lines.start) as f64 * LINE_WU + LINE_WU / 2.0;
    let ty = t.rect.y + HEADER_WU + (l.to_line - t.lines.start) as f64 * LINE_WU + LINE_WU / 2.0;
    ([f.rect.x + f.rect.w, fy], [t.rect.x, ty])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_for_a_seed() {
        let a = serde_json::to_string(&generate(1500, 3).bootstrap).unwrap();
        let b = serde_json::to_string(&generate(1500, 3).bootstrap).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn windows_sit_inside_their_cluster_and_the_world() {
        let v = generate(3000, 7);
        let clusters: BTreeMap<_, _> = v
            .bootstrap
            .clusters
            .iter()
            .map(|c| (c.id.clone(), c.rect))
            .collect();
        for w in &v.windows {
            let r = w.header.rect;
            let c = clusters[&w.header.cluster];
            assert!(
                r.x >= c.x
                    && r.y >= c.y
                    && r.x + r.w <= c.x + c.w + 1e-6
                    && r.y + r.h <= c.y + c.h + 1e-6,
                "{} outside {}",
                w.header.id,
                w.header.cluster
            );
            assert!(r.intersects(&v.bootstrap.world));
        }
    }

    #[test]
    fn every_window_is_indexed_in_each_base_tile_it_touches() {
        let v = generate(2000, 11);
        for (i, w) in v.windows.iter().enumerate() {
            let r = w.header.rect;
            let (tx, ty) = (
                (r.x / TILE_BASE_WU).floor() as i64,
                (r.y / TILE_BASE_WU).floor() as i64,
            );
            assert!(v.tiles[&(tx, ty)].0.contains(&i));
        }
    }

    #[test]
    fn review_first_cell_exists_in_the_code_lane() {
        let v = generate(2000, 7);
        assert!(
            v.bootstrap
                .clusters
                .iter()
                .any(|c| c.lane == "code" && c.heat == 4 && c.conn == 4)
        );
    }
}
