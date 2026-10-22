//! Channel links in the index pipeline (DESIGN §4.6, §5): runs the channel packs (bus, http, env,
//! deploy, config fields) over the parsed snapshot, maps endpoints to windows, and places hubs on
//! lane boundaries for the bootstrap.
//!
//! [`build`] is called where code links are built; [`hubs`] where the bootstrap is filled.

use std::collections::{BTreeMap, HashMap};

use cv_channel::{Binding, ChannelLink, FileIn, HubOut, Project, Target};
use cv_contracts::{HEADER_WU, Hub, LINE_WU, LaneBand, Rect, SiteRef};
use cv_lang::chunk::WindowSpec;
use cv_lang::{BlobFacts, Registry};

use crate::index::{FileRec, LinkRec, Win};

/// What a channel link adds to a [`LinkRec`]: the port text and, for runtime-only keys, the
/// `{ dynamic: { protocol, hint, partial } }` target.
#[derive(Debug, Clone)]
pub struct ChanInfo {
    /// "⇢ world.entity.moved", "⇢ GET /api/node/{}", "⇢ ? topic".
    pub port: String,
    pub dynamic: Option<serde_json::Value>,
    pub provenance: String,
}

pub fn registry() -> cv_channel::Registry {
    let mut r = cv_channel::Registry::new();
    r.register(Box::new(cv_channel_bus::BusPack::new()));
    r.register(Box::new(cv_channel_http::HttpPack::new()));
    r.register(Box::new(cv_channel_env::EnvPack::new()));
    r.register(Box::new(cv_channel_deploy::DeployPack::new()));
    r.register(Box::new(cv_channel_config::ConfigFieldsPack::new()));
    r
}

/// A link before ids are assigned (window indices, lines, kind, confidence, label).
pub struct Draft {
    pub kind: &'static str,
    pub from: usize,
    pub from_line: u32,
    pub to: usize,
    pub to_line: u32,
    pub confidence: f64,
    pub label: String,
    pub info: ChanInfo,
    /// The consumer depends on the producer, as (consumer, producer) window indices, for
    /// connectedness (confident links only).
    pub dep: Option<(usize, usize)>,
}

pub struct HubDraft {
    pub hub: HubOut,
}

pub struct Channels {
    pub links: Vec<Draft>,
    pub hubs: Vec<HubDraft>,
    pub notes: Vec<String>,
}

fn window_at(windows: &[(u32, u32, usize)], line: u32) -> Option<usize> {
    let i = windows.partition_point(|w| w.0 <= line);
    (i > 0 && windows[i - 1].1 >= line).then(|| windows[i - 1].2)
}

/// Runs every channel pack and maps the links to windows. One link per (from window, to window,
/// kind) at the first site, keeping the best confidence; one stub per (window, line).
pub fn build(
    reg: &Registry,
    files: &[FileRec],
    parsed: &[(Option<usize>, BlobFacts)],
    resolved: &[Vec<Option<usize>>],
    win_specs: &[(usize, WindowSpec)],
) -> Channels {
    if std::env::var("CV_CHANNELS").is_ok_and(|v| v == "off") {
        return Channels {
            links: vec![],
            hubs: vec![],
            notes: vec!["channels: off (CV_CHANNELS=off)".into()],
        };
    }
    let mut chan = registry();
    if let Some(f) = files.iter().find(|f| f.path == ".cv/links.toml") {
        chan.add_project_rules(&f.text);
    }
    let inputs: Vec<FileIn> = files
        .iter()
        .enumerate()
        .map(|(fi, f)| {
            let facts = &parsed[fi].1;
            FileIn {
                path: &f.path,
                lane: f.lane,
                lang: parsed[fi].0.map(|i| reg.pack(i).id()),
                conv: parsed[fi]
                    .0
                    .map(|i| reg.pack(i).conventions())
                    .unwrap_or_default(),
                text: &f.text,
                config_keys: &facts.config_keys,
                imports: facts
                    .imports
                    .iter()
                    .zip(resolved[fi].iter().chain(std::iter::repeat(&None)))
                    .map(|(imp, t)| Binding {
                        local: &imp.local,
                        imported: imp.imported.as_deref(),
                        target: *t,
                    })
                    .collect(),
                symbols: facts
                    .symbols
                    .iter()
                    .map(|s| (s.name.as_str(), s.name_line, s.parent.is_some()))
                    .collect(),
            }
        })
        .collect();
    let project = Project::new(&inputs);
    let pack_of: Vec<Option<usize>> = parsed.iter().map(|p| p.0).collect();
    let out = chan.run(reg, &project, &pack_of);
    if let Ok(dump) = std::env::var("CV_CHANNEL_DUMP") {
        dump_links(&dump, files, &out.links);
    }
    let mut notes = out.notes;
    notes.extend(chan.warnings.iter().cloned());

    let wins: Vec<Vec<(u32, u32, usize)>> = files
        .iter()
        .map(|f| {
            f.windows
                .iter()
                .map(|&w| (win_specs[w].1.start, win_specs[w].1.end, w))
                .collect()
        })
        .collect();
    let mut best: BTreeMap<(usize, usize, &'static str, bool), Draft> = BTreeMap::new();
    for l in &out.links {
        let Some(from) = window_at(&wins[l.from_file], l.from_line) else {
            continue;
        };
        let d = match &l.to {
            Target::Site { file, line } => {
                let Some(to) = window_at(&wins[*file], *line) else {
                    continue;
                };
                if to == from {
                    continue;
                }
                draft(l, from, to, *line, None)
            }
            Target::Dynamic { hint, partial } => {
                let mut dynamic = serde_json::json!({ "protocol": l.protocol, "hint": hint });
                if let Some(p) = partial {
                    dynamic["partial"] = serde_json::json!(p);
                }
                draft(
                    l,
                    from,
                    from,
                    l.from_line,
                    Some(serde_json::json!({ "dynamic": dynamic })),
                )
            }
        };
        let key = if d.info.dynamic.is_some() {
            (from, d.from_line as usize, d.kind, true)
        } else {
            (from, d.to, d.kind, false)
        };
        match best.get_mut(&key) {
            Some(b) if b.confidence >= d.confidence => {}
            Some(b) => *b = d,
            None => {
                best.insert(key, d);
            }
        }
    }
    let links = thin_config_reads(best.into_values().collect());
    Channels {
        links,
        hubs: out.hubs.into_iter().map(|hub| HubDraft { hub }).collect(),
        notes,
    }
}

/// Debug aid: every channel link as TSV (protocol, kind, confidence, from, to, label, rule).
fn dump_links(path: &str, files: &[FileRec], links: &[ChannelLink]) {
    use std::fmt::Write;
    let mut s = String::new();
    for l in links {
        let to = match &l.to {
            Target::Site { file, line } => format!("{}:{line}", files[*file].path),
            Target::Dynamic { hint, partial } => {
                format!("?{hint}|{}", partial.as_deref().unwrap_or(""))
            }
        };
        let _ = writeln!(
            s,
            "{}\t{}\t{:.2}\t{}:{}\t{}\t{}\t{}",
            l.protocol,
            l.kind,
            l.confidence,
            files[l.from_file].path,
            l.from_line,
            to,
            l.label,
            l.rule
        );
    }
    if let Err(e) = std::fs::write(path, s) {
        eprintln!("cv: cannot write {path}: {e}");
    }
}

/// Config-field reads drawn per window: the strongest few out of each config window and into each
/// code window. Every scored reader stays in its hub; the canvas keeps the map readable (a generic
/// loader is read by hundreds of files of one family).
const MAX_READS_OUT: usize = 4;
const MAX_READS_IN: usize = 6;

fn thin_config_reads(mut links: Vec<Draft>) -> Vec<Draft> {
    let mut order: Vec<usize> = (0..links.len()).collect();
    order.sort_by(|&a, &b| links[b].confidence.total_cmp(&links[a].confidence));
    let (mut out_n, mut in_n): (HashMap<usize, usize>, HashMap<usize, usize>) = Default::default();
    let mut keep = vec![true; links.len()];
    for i in order {
        let d = &links[i];
        if d.kind != "read" || d.info.dynamic.is_some() {
            continue;
        }
        let o = out_n.entry(d.from).or_default();
        let n = in_n.entry(d.to).or_default();
        if *o >= MAX_READS_OUT || *n >= MAX_READS_IN {
            keep[i] = false;
        } else {
            *o += 1;
            *n += 1;
        }
    }
    let mut k = keep.into_iter();
    links.retain(|_| k.next().unwrap());
    links
}

fn draft(
    l: &ChannelLink,
    from: usize,
    to: usize,
    to_line: u32,
    dynamic: Option<serde_json::Value>,
) -> Draft {
    // Who depends on whom: subscribers on publishers, readers on definers, clients on routes,
    // infra on what it runs.
    let dep = match (&l.to, l.kind) {
        (Target::Site { .. }, "http-request" | "deploy") => Some((from, to)),
        (Target::Site { .. }, _) => Some((to, from)),
        _ => None,
    };
    Draft {
        kind: l.kind,
        from,
        from_line: l.from_line,
        to,
        to_line,
        confidence: f64::from(l.confidence),
        label: l.label.clone(),
        info: ChanInfo {
            port: format!("⇢ {}", l.label),
            dynamic,
            provenance: format!("rule:{}", l.rule),
        },
        dep: dep.filter(|_| l.confidence >= 0.8),
    }
}

impl Channels {
    /// Extra file dependencies for connectedness (DESIGN §4.8: bus and HTTP hubs, config keys).
    /// (dependent, dependency) window pairs of the confident channel links (§4.8).
    pub fn window_dependencies(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.links
            .iter()
            .filter_map(|d| d.dep)
            .filter(|(a, b)| a != b)
    }

    /// LinkRecs for the channel links, ids continuing after `first`.
    pub fn link_recs(&self, first: usize) -> impl Iterator<Item = LinkRec> + '_ {
        self.links.iter().enumerate().map(move |(i, d)| LinkRec {
            id: if d.info.dynamic.is_some() {
                format!("dyn-{}", first + i)
            } else {
                format!("l{}", first + i)
            },
            kind: d.kind,
            from: d.from,
            from_line: d.from_line,
            to: d.to,
            to_line: d.to_line,
            confidence: d.confidence,
            label: d.label.clone(),
            channel: Some(Box::new(d.info.clone())),
        })
    }
}

/// Whether a link of a view is a channel link (from [`Channels::link_recs`]), not a code link.
pub fn is_channel(l: &LinkRec) -> bool {
    l.channel
        .as_ref()
        .is_some_and(|c| c.provenance.starts_with("rule:"))
}

/// The (dependent, dependency) window pair a channel link of a view adds to connectedness, as
/// [`Channels::window_dependencies`] gives it while indexing: confident (≥ 0.8) links to a site.
pub fn link_dependency(l: &LinkRec) -> Option<(usize, usize)> {
    let c = l.channel.as_ref()?;
    if !c.provenance.starts_with("rule:")
        || c.dynamic.is_some()
        || l.from == l.to
        || (l.confidence as f32) < 0.8
    {
        return None;
    }
    Some(if matches!(l.kind, "http-request" | "deploy") {
        (l.from, l.to)
    } else {
        (l.to, l.from)
    })
}

/// Hub side lengths (wu): channel hubs, and the many small config-field hubs.
const HUB: f64 = 240.0;
const CONFIG_HUB: f64 = 140.0;
/// Rows of hubs inside the gap between two lanes, as offsets from the gap's centre.
const ROWS: [f64; 4] = [-160.0, 160.0, -480.0, 480.0];
/// Drawn config-field hubs, busiest first.
const MAX_DRAWN_CONFIG_HUBS: usize = 120;
/// Hub labels are drawn at 80 wu to the right of the diamond (web backdrop).
const LABEL_WU_PER_CHAR: f64 = 46.0;

/// Bootstrap hubs: producers and consumers as sites, placed on the lane boundary above the lowest
/// lane they touch (DESIGN §5), near the mean x of their windows, packed into rows.
pub fn hubs(
    chan: &Channels,
    files: &[FileRec],
    windows: &[Win],
    lanes: &[LaneBand],
    lane_index: impl Fn(&str) -> usize,
) -> Vec<Hub> {
    let wins: Vec<Vec<(u32, u32, usize)>> = files
        .iter()
        .map(|f| {
            f.windows
                .iter()
                .map(|&w| {
                    let l = &windows[w].header.lines;
                    (l.start, l.end, w)
                })
                .collect()
        })
        .collect();
    let site = |&(file, line): &(usize, u32)| {
        let w = window_at(&wins[file], line);
        (
            SiteRef {
                path: files[file].path.clone(),
                line,
                window: w.map(|w| windows[w].header.id.clone()),
                col: None,
                symbol: None,
            },
            w,
        )
    };
    // Boundary y per lane index: the middle of the gap above the lane.
    let lane_y = |li: usize| -> f64 {
        let band = lanes.get(li).map(|b| b.rect.y).unwrap_or(0.0);
        let prev = li
            .checked_sub(1)
            .and_then(|p| lanes.get(p))
            .map(|b| b.rect.y + b.rect.h);
        match prev {
            Some(p) => (p + band) / 2.0,
            None => band - 700.0,
        }
    };
    let mut placed: Vec<(Hub, usize, f64, f64)> = Vec::new();
    for h in &chan.hubs {
        let h = &h.hub;
        let (producers, pw): (Vec<SiteRef>, Vec<Option<usize>>) =
            h.producers.iter().map(site).unzip();
        let (consumers, cw): (Vec<SiteRef>, Vec<Option<usize>>) =
            h.consumers.iter().map(site).unzip();
        // Spoke ends: a producer's out port (right edge) and a consumer's in port (left edge).
        let port = |s: &SiteRef, w: Option<usize>, out: bool| {
            w.map(|w| {
                let h = &windows[w].header;
                let y = h.rect.y
                    + HEADER_WU
                    + (s.line.clamp(h.lines.start, h.lines.end) - h.lines.start) as f64 * LINE_WU
                    + LINE_WU / 2.0;
                [if out { h.rect.x + h.rect.w } else { h.rect.x }, y]
            })
        };
        let anchors: Vec<[f64; 2]> = producers
            .iter()
            .zip(&pw)
            .filter_map(|(s, &w)| port(s, w, true))
            .chain(
                consumers
                    .iter()
                    .zip(&cw)
                    .filter_map(|(s, &w)| port(s, w, false)),
            )
            .collect();
        let ws: Vec<usize> = pw.into_iter().chain(cw).flatten().collect();
        let size = if h.family.is_some() { CONFIG_HUB } else { HUB };
        let (x, lane) = if ws.is_empty() {
            (0.0, 0)
        } else {
            let x = ws
                .iter()
                .map(|&w| windows[w].header.rect.x + windows[w].header.rect.w / 2.0)
                .sum::<f64>()
                / ws.len() as f64;
            let lane = ws
                .iter()
                .map(|&w| lane_index(&windows[w].header.lane))
                .max()
                .unwrap_or(0);
            (x, lane)
        };
        placed.push((
            Hub {
                id: h.id.clone(),
                protocol: h.protocol.into(),
                key: h.key.clone(),
                family: h.family.clone(),
                display: h.display.clone(),
                producers,
                consumers,
                rect: None,
                anchors: Some(anchors),
            },
            lane,
            x,
            size,
        ));
    }
    // Config-field hubs are many (one per family and key): only the busiest are drawn; the rest
    // stay in the list (search, MCP) without a rect.
    let mut cfg: Vec<usize> = (0..placed.len())
        .filter(|&i| placed[i].0.family.is_some())
        .collect();
    cfg.sort_by_key(|&i| {
        std::cmp::Reverse(placed[i].0.producers.len() + placed[i].0.consumers.len())
    });
    let hidden: std::collections::HashSet<usize> =
        cfg.into_iter().skip(MAX_DRAWN_CONFIG_HUBS).collect();
    // Pack per boundary: each hub takes the row where it lands closest to its wanted x, leaving
    // room for its label.
    let mut order: Vec<usize> = (0..placed.len()).filter(|i| !hidden.contains(i)).collect();
    order.sort_by(|&a, &b| {
        (placed[a].1, placed[a].2)
            .partial_cmp(&(placed[b].1, placed[b].2))
            .unwrap()
    });
    let mut rows: HashMap<usize, [f64; ROWS.len()]> = HashMap::new();
    for i in order {
        let (_, lane, x, size) = (&placed[i].0, placed[i].1, placed[i].2, placed[i].3);
        let gap = 80.0 + LABEL_WU_PER_CHAR * placed[i].0.display.chars().count() as f64;
        let ends = rows.entry(lane).or_insert([f64::MIN; ROWS.len()]);
        let (row, at) = ends
            .iter()
            .enumerate()
            .map(|(r, &end)| (r, (x - size / 2.0).max(end + gap)))
            .min_by(|a, b| (a.1 - x).abs().total_cmp(&(b.1 - x).abs()))
            .unwrap();
        ends[row] = at + size;
        let cy = lane_y(lane) + ROWS[row];
        placed[i].0.rect = Some(Rect {
            x: at,
            y: cy - size / 2.0,
            w: size,
            h: size,
        });
    }
    placed
        .into_iter()
        .map(|mut p| {
            // Only drawn hubs need spoke ends.
            if p.0.rect.is_none() {
                p.0.anchors = None;
            }
            p.0
        })
        .collect()
}
