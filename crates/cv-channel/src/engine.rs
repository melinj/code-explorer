//! The channel engine: pack registry, site extraction through the language packs, endpoint
//! resolution (constants across imports, prefixes, config rules) and hub linking.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use cv_lang::keys::{FoldTransform, KeyExpr, KeyPart, SiteFacts, SiteQuery};
use rayon::prelude::*;

use crate::{
    ChannelKey, ChannelLink, ChannelPack, EndpointFact, HubOut, KeyFrom, Output, Project, Target,
    path_matches,
};

/// A producer whose key has guessed holes links only when it reaches at most this many hubs;
/// otherwise it ends in a dynamic stub (D11: never a guessed link).
pub const MAX_GUESSED_HUBS: usize = 3;
/// Confidence of a link through a key with guessed holes (drawn faded with "?").
pub const GUESS_CONFIDENCE: f32 = 0.5;
pub const STUB_CONFIDENCE: f32 = 0.3;

pub struct Registry {
    packs: Vec<Box<dyn ChannelPack>>,
    pub warnings: Vec<String>,
    /// Lanes whose files are not endpoints. Tests exercise channels (they publish and subscribe to
    /// check behaviour) but do not define the wiring, so by default they stay out of hubs.
    pub skip_lanes: Vec<String>,
}

impl Default for Registry {
    fn default() -> Self {
        Registry {
            packs: Vec::new(),
            warnings: Vec::new(),
            skip_lanes: vec!["test".into()],
        }
    }
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, pack: Box<dyn ChannelPack>) {
        self.packs.push(pack);
    }

    pub fn packs(&self) -> &[Box<dyn ChannelPack>] {
        &self.packs
    }

    /// Adds project rules (`.cv/links.toml`): each pack reads the tables under its protocol.
    pub fn add_project_rules(&mut self, src: &str) {
        for p in &mut self.packs {
            match crate::RuleSet::parse(src, Some(p.protocol())) {
                Ok(r) => p.rules_mut().extend(r),
                Err(e) => self
                    .warnings
                    .push(format!(".cv/links.toml [{}]: {e}", p.protocol())),
            }
        }
    }

    /// Every endpoint rule as (pack, rule index), in a stable order: the global query ids.
    fn flat(&self) -> Vec<(usize, usize)> {
        self.packs
            .iter()
            .enumerate()
            .flat_map(|(pi, p)| (0..p.rules().endpoints.len()).map(move |ri| (pi, ri)))
            .collect()
    }

    fn fold_transforms(&self) -> Vec<FoldTransform> {
        self.packs
            .iter()
            .flat_map(|p| p.transforms())
            .map(|t| FoldTransform {
                function: t.function.clone(),
                result: t.result.clone(),
            })
            .collect()
    }

    /// Runs every pack's endpoint queries through the language pack that parsed each file.
    /// `pack_of[i]` is the language pack index (in `langs`) of `project.files[i]`.
    pub fn extract(
        &self,
        langs: &cv_lang::Registry,
        project: &Project,
        pack_of: &[Option<usize>],
    ) -> Vec<SiteFacts> {
        let flat = self.flat();
        let transforms = self.fold_transforms();
        let extractors: Vec<_> = langs
            .packs()
            .iter()
            .map(|lp| {
                let mut queries = Vec::new();
                let mut global = Vec::new();
                for (g, &(pi, ri)) in flat.iter().enumerate() {
                    let r = &self.packs[pi].rules().endpoints[ri];
                    if r.lang == lp.id() {
                        queries.push(SiteQuery {
                            id: r.id.clone(),
                            query: r.query.clone(),
                            hints: r.hints.clone(),
                        });
                        global.push(g);
                    }
                }
                if queries.is_empty() {
                    return None;
                }
                lp.site_extractor(&queries, &transforms)
                    .map(|e| (e, global))
            })
            .collect();
        project
            .files
            .par_iter()
            .zip(pack_of.par_iter())
            .map(|(f, lp)| {
                let Some((ex, global)) = lp.and_then(|i| extractors[i].as_ref()) else {
                    return SiteFacts::default();
                };
                let mut s = ex.extract(f.path, f.text);
                for site in &mut s.sites {
                    site.query = global[site.query];
                }
                s
            })
            .collect()
    }

    /// Extracts sites and links every pack.
    pub fn run(
        &self,
        langs: &cv_lang::Registry,
        project: &Project,
        pack_of: &[Option<usize>],
    ) -> Output {
        let sites = self.extract(langs, project, pack_of);
        self.link(project, &sites)
    }

    /// Endpoints per pack, from code sites (with constants and prefixes resolved) and config rules.
    pub fn endpoints(&self, project: &Project, sites: &[SiteFacts]) -> Vec<Vec<EndpointFact>> {
        let flat = self.flat();
        let consts: Vec<HashMap<&str, &KeyExpr>> = sites
            .iter()
            .map(|s| s.consts.iter().map(|(n, k)| (n.as_str(), k)).collect())
            .collect();
        let res = Resolver {
            project,
            consts: &consts,
        };
        let mut eps: Vec<Vec<EndpointFact>> = vec![Vec::new(); self.packs.len()];
        let skip = |fi: usize| self.skip_lanes.iter().any(|l| l == project.files[fi].lane);
        for (fi, sf) in sites.iter().enumerate() {
            if skip(fi) {
                continue;
            }
            let mut prefixes: HashMap<(usize, Option<&str>), KeyExpr> = HashMap::new();
            for s in sf.sites.iter().filter(|s| s.is_prefix) {
                let (pi, _) = flat[s.query];
                let k = res.resolve(fi, &s.key);
                if k.has_lit() && !has_unknown(&k) {
                    prefixes.insert((pi, s.scope.as_deref()), k);
                }
            }
            for s in sf.sites.iter().filter(|s| !s.is_prefix) {
                let (pi, ri) = flat[s.query];
                let pack = &self.packs[pi];
                let rule = &pack.rules().endpoints[ri];
                let mut key = KeyExpr::default();
                if let Some(p) = &rule.prefix {
                    match p.strip_prefix('$') {
                        Some(name) => {
                            let k = res.resolve(fi, &KeyExpr::reference(None, name));
                            if !has_unknown(&k) {
                                key.push_expr(k);
                            }
                        }
                        None => key.push(KeyPart::Lit(p.clone())),
                    }
                }
                if let Some(p) = prefixes.get(&(pi, s.scope.as_deref())) {
                    key.push_expr(p.clone());
                }
                let k = res.resolve(fi, &s.key);
                let only_prefix = !k.has_lit() && !has_unknown(&k) && k.parts.is_empty();
                key.push_expr(k);
                if only_prefix && key.parts.is_empty() {
                    continue;
                }
                key.method = s
                    .method
                    .as_deref()
                    .map(|m| m.trim_matches(['"', '\'', '`']).to_ascii_uppercase())
                    .or_else(|| rule.method.clone());
                if !pack.accepts(&key, rule.role) {
                    continue;
                }
                // Holes are guesses unless the protocol's syntax makes them exact (path parameters).
                let heuristic = !pack.holes_are_exact()
                    && key
                        .parts
                        .iter()
                        .any(|p| matches!(p, KeyPart::Segment | KeyPart::Unknown { .. }));
                eps[pi].push(EndpointFact {
                    rule: rule.id.clone(),
                    file: fi,
                    role: rule.role,
                    key,
                    line: s.line,
                    handler: s.handler.clone(),
                    confidence: rule.confidence,
                    heuristic,
                    from_config: false,
                });
            }
        }
        for (pi, pack) in self.packs.iter().enumerate() {
            let mut claimed: HashSet<(usize, usize)> = HashSet::new();
            for rule in pack.config_rules() {
                for (fi, f) in project.files.iter().enumerate() {
                    if f.config_keys.is_empty() || skip(fi) || !rule.glob.is_match(f.path) {
                        continue;
                    }
                    for (ki, k) in f.config_keys.iter().enumerate() {
                        if k.item != rule.items
                            || !path_matches(&rule.path, &k.path)
                            || claimed.contains(&(fi, ki))
                        {
                            continue;
                        }
                        let text = match rule.key_from {
                            KeyFrom::Leaf => Some(k.key.clone()),
                            KeyFrom::Value => k.value.clone(),
                            KeyFrom::ItemName => k
                                .value
                                .as_deref()
                                .and_then(|v| v.split_once('=').map(|(a, _)| a.trim().to_string()))
                                .or_else(|| k.value.clone()),
                        };
                        let Some(text) = text.filter(|t| !t.is_empty()) else {
                            continue;
                        };
                        if rule
                            .value_pattern
                            .as_ref()
                            .is_some_and(|re| !re.is_match(&text))
                        {
                            continue;
                        }
                        if let Some((wk, wv)) = &rule.when
                            && !when_holds(f.config_keys, ki, wk, wv)
                        {
                            continue;
                        }
                        let key = KeyExpr::lit(text);
                        if !pack.accepts(&key, rule.role) {
                            continue;
                        }
                        claimed.insert((fi, ki));
                        eps[pi].push(EndpointFact {
                            rule: rule.id.clone(),
                            file: fi,
                            role: rule.role,
                            key,
                            line: k.line,
                            handler: None,
                            confidence: rule.confidence,
                            heuristic: false,
                            from_config: true,
                        });
                    }
                }
            }
        }
        eps
    }

    pub fn link(&self, project: &Project, sites: &[SiteFacts]) -> Output {
        let eps = self.endpoints(project, sites);
        let outs: Vec<Output> = self
            .packs
            .par_iter()
            .zip(eps.par_iter())
            .map(|(pack, eps)| {
                pack.link_all(project, eps)
                    .unwrap_or_else(|| hub_link(pack.as_ref(), project, eps))
            })
            .collect();
        let mut out = Output::default();
        for o in outs {
            out.append(o);
        }
        out
    }
}

/// What a stub says about a key: `topic`, `world.{kind}.moved`, `primary_instance_topic(…)`.
fn hint_of(k: &KeyExpr) -> String {
    if k.has_lit() {
        return k.render();
    }
    k.parts
        .iter()
        .map(|p| match p {
            KeyPart::Unknown { hint } => hint.clone(),
            KeyPart::Ref { qualifier, name } => match qualifier {
                Some(q) => format!("{q}.{name}"),
                None => name.clone(),
            },
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn has_unknown(k: &KeyExpr) -> bool {
    k.parts.iter().any(|p| matches!(p, KeyPart::Unknown { .. }))
}

/// The nearest preceding key `wk` whose parent is an ancestor of fact `ki` has a value containing `wv`.
fn when_holds(keys: &[cv_lang::keys::ConfigKeyFact], ki: usize, wk: &str, wv: &str) -> bool {
    let path = &keys[ki].path;
    for k in keys[..ki].iter().rev().take(200) {
        let (parent, leaf) = match k.path.rsplit_once('.') {
            Some((p, l)) => (p, l),
            None => ("", k.path.as_str()),
        };
        if leaf != wk || k.item {
            continue;
        }
        if parent.is_empty() || path.starts_with(&format!("{parent}.")) {
            return k.value.as_deref().is_some_and(|v| v.contains(wv));
        }
    }
    false
}

struct Resolver<'p> {
    project: &'p Project<'p>,
    consts: &'p [HashMap<&'p str, &'p KeyExpr>],
}

impl Resolver<'_> {
    /// Folds constant references in; what stays unresolved becomes `Unknown`.
    fn resolve(&self, file: usize, key: &KeyExpr) -> KeyExpr {
        self.resolve_d(file, key, 0)
    }

    fn resolve_d(&self, file: usize, key: &KeyExpr, depth: u32) -> KeyExpr {
        let mut out = KeyExpr {
            parts: vec![],
            method: key.method.clone(),
        };
        for p in &key.parts {
            match p {
                KeyPart::Ref { qualifier, name } => {
                    match self.lookup(file, qualifier.as_deref(), name, depth) {
                        Some((f2, k)) if depth < 6 => {
                            out.push_expr(self.resolve_d(f2, k, depth + 1))
                        }
                        _ => out.push(KeyPart::Unknown {
                            hint: match qualifier {
                                Some(q) => format!("{q}.{name}"),
                                None => name.clone(),
                            },
                        }),
                    }
                }
                other => out.push(other.clone()),
            }
        }
        out
    }

    fn stem_is(&self, file: usize, name: &str) -> bool {
        let f = &self.project.files[file];
        f.conv.module_name(f.path) == name
    }

    fn lookup(
        &self,
        file: usize,
        q: Option<&str>,
        name: &str,
        depth: u32,
    ) -> Option<(usize, &KeyExpr)> {
        if depth > 6 {
            return None;
        }
        let c = &self.consts[file];
        let f = &self.project.files[file];
        match q {
            None => {
                if let Some(k) = c.get(name) {
                    return Some((file, k));
                }
                for b in &f.imports {
                    if b.local == name
                        && let (Some(t), Some(imp)) = (b.target, b.imported)
                        && imp != "*"
                        && t != file
                        && let Some(r) = self.lookup(t, None, imp, depth + 1)
                    {
                        return Some(r);
                    }
                }
                for b in &f.imports {
                    if b.imported == Some("*")
                        && let Some(t) = b.target
                        && t != file
                        && let Some(r) = self.lookup(t, None, name, depth + 1)
                    {
                        return Some(r);
                    }
                }
                None
            }
            Some(q) => {
                let qualified = format!("{q}.{name}");
                if let Some(k) = c.get(qualified.as_str()) {
                    return Some((file, k));
                }
                if f.conv.is_self_receiver(q) {
                    let suffix = format!(".{name}");
                    let mut hits = c.iter().filter(|(n, _)| n.ends_with(&suffix));
                    if let (Some((_, k)), None) = (hits.next(), hits.next()) {
                        return Some((file, k));
                    }
                    return c.get(name).map(|k| (file, *k));
                }
                for b in &f.imports {
                    if b.local != q {
                        continue;
                    }
                    let Some(t) = b.target else { continue };
                    if t == file {
                        continue;
                    }
                    let r = match b.imported {
                        None => self.lookup(t, None, name, depth + 1),
                        Some(imp) if self.stem_is(t, imp) => self.lookup(t, None, name, depth + 1),
                        Some(imp) => self.lookup(t, Some(imp), name, depth + 1),
                    };
                    if r.is_some() {
                        return r;
                    }
                }
                for b in &f.imports {
                    if b.imported == Some("*")
                        && let Some(t) = b.target
                        && t != file
                        && let Some(r) = self.lookup(t, Some(q), name, depth + 1)
                    {
                        return Some(r);
                    }
                }
                None
            }
        }
    }
}

/// Hub linking (bus, http, env): producers meet consumers at hubs keyed by normalized keys.
pub fn hub_link(pack: &dyn ChannelPack, project: &Project, eps: &[EndpointFact]) -> Output {
    let (from_roles, to_roles) = pack.sides();
    let norm: Vec<ChannelKey> = eps
        .iter()
        .map(|e| {
            if !e.key.has_lit() && e.key.parts.iter().any(|p| !matches!(p, KeyPart::Lit(_))) {
                ChannelKey::Dynamic {
                    hint: e.key.render(),
                }
            } else {
                pack.normalize(&e.key, e.role)
            }
        })
        .collect();
    let is_dyn = |i: usize| matches!(norm[i], ChannelKey::Dynamic { .. });
    let producers: Vec<usize> = (0..eps.len())
        .filter(|&i| from_roles.contains(&eps[i].role) && !is_dyn(i))
        .collect();
    let consumers: Vec<usize> = (0..eps.len())
        .filter(|&i| to_roles.contains(&eps[i].role) && !is_dyn(i))
        .collect();
    let mut known: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut patterns: Vec<usize> = Vec::new();
    for &c in &consumers {
        match &norm[c] {
            ChannelKey::Known(k) => known.entry(k.as_str()).or_default().push(c),
            _ => patterns.push(c),
        }
    }
    // Candidate consumers per producer.
    let cands: Vec<Vec<usize>> = producers
        .par_iter()
        .map(|&p| {
            let mut v: Vec<usize> = match &norm[p] {
                ChannelKey::Known(k) => known.get(k.as_str()).cloned().unwrap_or_default(),
                _ => consumers
                    .iter()
                    .copied()
                    .filter(|&c| {
                        matches!(norm[c], ChannelKey::Known(_)) && pack.matches(&norm[p], &norm[c])
                    })
                    .collect(),
            };
            v.extend(
                patterns
                    .iter()
                    .copied()
                    .filter(|&c| pack.matches(&norm[p], &norm[c])),
            );
            if pack.exact_wins() && v.iter().any(|&c| norm[c].text() == norm[p].text()) {
                v.retain(|&c| norm[c].text() == norm[p].text());
            }
            v
        })
        .collect();
    // Guessed keys reaching too many (or no) hubs end in stubs, on both sides.
    let mut stub: HashSet<usize> = HashSet::new();
    let mut reached: HashMap<usize, BTreeSet<&str>> = HashMap::new();
    for (pi, &p) in producers.iter().enumerate() {
        let keys: BTreeSet<&str> = cands[pi].iter().map(|&c| norm[c].text()).collect();
        if eps[p].heuristic && (keys.is_empty() || keys.len() > MAX_GUESSED_HUBS) {
            stub.insert(p);
            continue;
        }
        for &c in &cands[pi] {
            reached.entry(c).or_default().insert(norm[p].text());
        }
    }
    for &c in &consumers {
        if eps[c].heuristic {
            let n = reached.get(&c).map(|s| s.len()).unwrap_or(0);
            if n == 0 || n > MAX_GUESSED_HUBS {
                stub.insert(c);
            }
        }
    }
    let mut out = Output::default();
    let mut hub_ix: BTreeMap<String, usize> = BTreeMap::new();
    type Sites = BTreeSet<(usize, u32)>;
    let mut hub_sites: Vec<(Sites, Sites)> = Vec::new();
    let mut seen: HashSet<(usize, u32, usize, u32)> = HashSet::new();
    for (pi, &p) in producers.iter().enumerate() {
        if stub.contains(&p) {
            continue;
        }
        for &c in &cands[pi] {
            if stub.contains(&c) {
                continue;
            }
            let (ep, ec) = (&eps[p], &eps[c]);
            if (ep.file, ep.line) == (ec.file, ec.line)
                || !seen.insert((ep.file, ep.line, ec.file, ec.line))
            {
                continue;
            }
            // The hub is the concrete key when there is one.
            let hub_key = match (&norm[p], &norm[c]) {
                (ChannelKey::Known(_), _) if !ep.heuristic => &norm[p],
                (_, k @ ChannelKey::Known(_)) => k,
                (k, _) if !ep.heuristic => k,
                (_, k) => k,
            };
            let label = pack.hub_label(hub_key);
            let hi = *hub_ix.entry(label.clone()).or_insert_with(|| {
                out.hubs.push(HubOut {
                    id: format!("{}:{}", pack.protocol(), hub_key.text()),
                    protocol: pack.protocol(),
                    key: hub_key.text().to_string(),
                    family: None,
                    display: label.clone(),
                    producers: vec![],
                    consumers: vec![],
                });
                hub_sites.push(Default::default());
                out.hubs.len() - 1
            });
            hub_sites[hi].0.insert((ep.file, ep.line));
            hub_sites[hi].1.insert((ec.file, ec.line));
            let guessed = ep.heuristic || ec.heuristic;
            let confidence =
                ep.confidence.min(ec.confidence) * if guessed { GUESS_CONFIDENCE } else { 1.0 };
            out.links.push(ChannelLink {
                protocol: pack.protocol(),
                kind: pack.link_kind(ep, ec),
                from_file: ep.file,
                from_line: ep.line,
                to: Target::Site {
                    file: ec.file,
                    line: ec.line,
                },
                confidence,
                label: if guessed { format!("? {label}") } else { label },
                hub: Some(hi),
                rule: ep.rule.clone(),
            });
        }
    }
    for (h, (p, c)) in out.hubs.iter_mut().zip(hub_sites) {
        h.producers = p.into_iter().collect();
        h.consumers = c.into_iter().collect();
    }
    // Stubs: runtime-only keys, and guesses that reach too many or no hubs.
    let mut stubs = 0;
    for (i, e) in eps.iter().enumerate() {
        if !(from_roles.contains(&e.role) || to_roles.contains(&e.role)) {
            continue;
        }
        let partial = match &norm[i] {
            ChannelKey::Dynamic { .. } => None,
            k if stub.contains(&i) => Some(pack.hub_label(k)),
            _ => continue,
        };
        let hint = cv_lang::keys::shorten(&hint_of(&e.key));
        stubs += 1;
        out.links.push(ChannelLink {
            protocol: pack.protocol(),
            kind: pack.stub_kind(e.role),
            from_file: e.file,
            from_line: e.line,
            label: format!("? {}", partial.as_deref().unwrap_or(&hint)),
            to: Target::Dynamic { hint, partial },
            confidence: STUB_CONFIDENCE,
            hub: None,
            rule: e.rule.clone(),
        });
    }
    // Subscription site → handler definition.
    let mut dispatch = 0;
    if let Some(kind) = pack.dispatch_kind() {
        for e in eps.iter().filter(|e| to_roles.contains(&e.role)) {
            let Some(h) = e.handler.as_deref() else {
                continue;
            };
            let name = h.rsplit('.').next().unwrap_or(h).trim();
            let f = &project.files[e.file];
            let member = match h.split_once('.') {
                None => false,
                Some((q, rest)) if f.conv.is_self_receiver(q) && !rest.contains('.') => true,
                _ => continue,
            };
            let mut defs = f
                .symbols
                .iter()
                .filter(|(n, _, m)| *n == name && *m == member);
            if let (Some(&(_, line, _)), None) = (defs.next(), defs.next())
                && line != e.line
            {
                dispatch += 1;
                out.links.push(ChannelLink {
                    protocol: pack.protocol(),
                    kind,
                    from_file: e.file,
                    from_line: e.line,
                    to: Target::Site { file: e.file, line },
                    confidence: 0.9,
                    label: name.to_string(),
                    hub: None,
                    rule: e.rule.clone(),
                });
            }
        }
    }
    out.notes.push(format!(
        "{}: {} endpoints, {} hubs, {} links, {} dynamic stubs, {} dispatch",
        pack.protocol(),
        eps.len(),
        out.hubs.len(),
        out.links.len() - stubs - dispatch,
        stubs,
        dispatch
    ));
    out
}
