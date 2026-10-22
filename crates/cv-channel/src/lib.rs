//! B4 `cv-channel`: the channel core (DESIGN §4.6, D11, D13).
//!
//! A [`ChannelPack`] says where endpoints are (rules as data, per language pack id), how keys
//! normalize and how they match. It never parses code: language packs run the rule queries and
//! fold the captured keys ([`cv_lang::keys`]); config packs report key paths. This crate turns
//! those facts into endpoints, resolves constants across imports, groups producers and consumers
//! at hubs, and ends keys that cannot be resolved statically in dynamic stubs (never a guess).
//!
//! Nothing here names a protocol or a language: both come from the registered packs.

pub mod engine;
pub mod keyalg;
pub mod rules;

use std::collections::HashMap;

pub use cv_lang::keys::{ConfigKeyFact, KeyExpr, KeyPart, SiteFacts};
pub use engine::Registry;
pub use rules::{ConfigRule, EndpointRule, KeyFrom, KeyTransform, RuleSet, path_matches};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Producer,
    Consumer,
    Client,
    Server,
    /// Config or infra file that defines a key (env var, setting).
    Define,
    /// Code that reads a key.
    Read,
    /// Infra that runs or builds code (Dockerfile CMD, CI run:).
    Deploy,
}

impl Role {
    pub fn parse(s: &str) -> Option<Role> {
        Some(match s {
            "producer" => Role::Producer,
            "consumer" => Role::Consumer,
            "client" => Role::Client,
            "server" => Role::Server,
            "define" => Role::Define,
            "read" => Role::Read,
            "deploy" => Role::Deploy,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ChannelKey {
    /// Normalized key that can meet others at a hub, e.g. "world.entity.moved", "GET /api/node/{}".
    Known(String),
    /// Pattern with wildcards, e.g. "world.turn.requested.*".
    Pattern(String),
    /// Nothing static to match: drawn as an open stub with this hint.
    Dynamic { hint: String },
}

impl ChannelKey {
    pub fn text(&self) -> &str {
        match self {
            ChannelKey::Known(s) | ChannelKey::Pattern(s) => s,
            ChannelKey::Dynamic { hint } => hint,
        }
    }
}

/// One endpoint: a code site found by a rule query, or a config key found by a config rule.
#[derive(Debug, Clone)]
pub struct EndpointFact {
    pub rule: String,
    /// Index into [`Project::files`].
    pub file: usize,
    pub role: Role,
    /// Resolved key (constants folded in, prefixes applied).
    pub key: KeyExpr,
    pub line: u32,
    pub handler: Option<String>,
    pub confidence: f32,
    /// The key has holes the folder could not resolve (a runtime value inside a template).
    pub heuristic: bool,
    pub from_config: bool,
}

/// What the channel core needs to know about one file.
pub struct FileIn<'a> {
    pub path: &'a str,
    pub lane: &'a str,
    /// Language pack id that parsed the file.
    pub lang: Option<&'a str>,
    /// That pack's naming conventions (self receivers, entry modules).
    pub conv: cv_lang::Conventions,
    pub text: &'a str,
    pub config_keys: &'a [ConfigKeyFact],
    pub imports: Vec<Binding<'a>>,
    /// (name, name line, is a class member) of functions, classes and methods, for handler
    /// resolution: `self.x` names a member, a bare `x` a top-level function.
    pub symbols: Vec<(&'a str, u32, bool)>,
}

/// An import as the core sees it: the local name, the imported member (None = the module itself,
/// `*` = star) and the resolved file.
#[derive(Debug, Clone, Copy)]
pub struct Binding<'a> {
    pub local: &'a str,
    pub imported: Option<&'a str>,
    pub target: Option<usize>,
}

pub struct Project<'a> {
    pub files: &'a [FileIn<'a>],
    by_path: HashMap<&'a str, usize>,
}

impl<'a> Project<'a> {
    pub fn new(files: &'a [FileIn<'a>]) -> Self {
        let by_path = files.iter().enumerate().map(|(i, f)| (f.path, i)).collect();
        Project { files, by_path }
    }

    pub fn file(&self, path: &str) -> Option<usize> {
        self.by_path.get(path).copied()
    }

    /// Files under a directory (`dir/…`), sorted by path.
    pub fn files_under(&self, dir: &str) -> Vec<usize> {
        let pre = format!("{}/", dir.trim_end_matches('/'));
        let mut v: Vec<usize> = (0..self.files.len())
            .filter(|&i| self.files[i].path.starts_with(&pre))
            .collect();
        v.sort_by_key(|&i| self.files[i].path);
        v
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Site {
        file: usize,
        line: u32,
    },
    /// Only known at runtime (D11): an open stub with what is known.
    Dynamic {
        hint: String,
        partial: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct ChannelLink {
    pub protocol: &'static str,
    /// model.ts LinkKind, chosen by the pack.
    pub kind: &'static str,
    pub from_file: usize,
    pub from_line: u32,
    pub to: Target,
    pub confidence: f32,
    /// Port text after the arrow, e.g. "world.entity.moved", "GET /api/node/{}", "? topic".
    pub label: String,
    /// Index into [`Output::hubs`].
    pub hub: Option<usize>,
    pub rule: String,
}

#[derive(Debug, Clone, Default)]
pub struct HubOut {
    pub id: String,
    pub protocol: &'static str,
    pub key: String,
    pub family: Option<String>,
    pub display: String,
    pub producers: Vec<(usize, u32)>,
    pub consumers: Vec<(usize, u32)>,
}

#[derive(Debug, Default)]
pub struct Output {
    pub links: Vec<ChannelLink>,
    pub hubs: Vec<HubOut>,
    /// Per-pack summary lines for logs.
    pub notes: Vec<String>,
}

impl Output {
    pub fn append(&mut self, mut other: Output) {
        let off = self.hubs.len();
        for l in &mut other.links {
            if let Some(h) = l.hub.as_mut() {
                *h += off;
            }
        }
        self.links.append(&mut other.links);
        self.hubs.append(&mut other.hubs);
        self.notes.append(&mut other.notes);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mention {
    None,
    FileName,
    Path,
}

/// Repository-wide signals for heuristic matching.
pub trait LinkContext {
    /// Inverse document frequency of an identifier across code files (rare keys score higher).
    fn idf(&self, token: &str) -> f32;
    /// 0 = same directory, 1 = sibling directories, larger = further apart.
    fn dir_distance(&self, a: &str, b: &str) -> u32;
    /// Does `code` mention `config` by relative path or by file name (a loader for that family)?
    fn mentions(&self, code: &str, config: &str) -> Mention;
}

pub trait ChannelPack: Send + Sync {
    /// model.ts Protocol: "bus", "http", "env", "deploy", "config", …
    fn protocol(&self) -> &'static str;
    fn rules(&self) -> &RuleSet;
    fn rules_mut(&mut self) -> &mut RuleSet;

    fn endpoint_rules(&self, lang: &str) -> Vec<&EndpointRule> {
        self.rules()
            .endpoints
            .iter()
            .filter(|r| r.lang == lang)
            .collect()
    }
    fn config_rules(&self) -> &[ConfigRule] {
        &self.rules().config
    }
    fn transforms(&self) -> &[KeyTransform] {
        &self.rules().transforms
    }

    /// Roles on the producing and the consuming side of a hub.
    fn sides(&self) -> (&'static [Role], &'static [Role]);
    /// Is this resolved key an endpoint of the protocol at all (e.g. a path for HTTP, a name for
    /// env)? Keys rejected here are dropped silently, never stubbed.
    fn accepts(&self, key: &KeyExpr, role: Role) -> bool {
        let _ = (key, role);
        true
    }
    /// Where an exact key meets a hub, pattern matches are dropped (HTTP routing precedence).
    fn exact_wins(&self) -> bool {
        false
    }
    fn normalize(&self, key: &KeyExpr, role: Role) -> ChannelKey;
    /// Does a producer-side key reach a consumer-side key (MQTT wildcards, path templates, …)?
    fn matches(&self, producer: &ChannelKey, consumer: &ChannelKey) -> bool;
    fn hub_label(&self, key: &ChannelKey) -> String;
    /// model.ts LinkKind of a producer → consumer link.
    fn link_kind(&self, producer: &EndpointFact, consumer: &EndpointFact) -> &'static str;
    /// LinkKind of a dynamic stub at an endpoint of this role.
    fn stub_kind(&self, role: Role) -> &'static str;
    /// LinkKind from a subscription site to its handler definition, if the pack draws those.
    fn dispatch_kind(&self) -> Option<&'static str> {
        None
    }
    /// Holes left by folding are protocol syntax (HTTP path parameters), not guesses.
    fn holes_are_exact(&self) -> bool {
        false
    }
    /// Link confidence 0..1, or None for no link. Exact packs keep the default; heuristic packs
    /// (config fields) score key rarity, directory proximity and loader evidence from `ctx`.
    fn affinity(
        &self,
        producer: &EndpointFact,
        consumer: &EndpointFact,
        ctx: &dyn LinkContext,
    ) -> Option<f32> {
        let _ = ctx;
        let (p, c) = (
            self.normalize(&producer.key, producer.role),
            self.normalize(&consumer.key, consumer.role),
        );
        self.matches(&p, &c).then_some(1.0)
    }
    /// Hub grouping for producers: config fields use one hub per (file family, key).
    fn family(&self, producer: &EndpointFact, project: &Project) -> Option<String> {
        let _ = (producer, project);
        None
    }
    /// Packs that do not meet at hubs (deploy) or score every pair (config fields) link here.
    fn link_all(&self, project: &Project, endpoints: &[EndpointFact]) -> Option<Output> {
        let _ = (project, endpoints);
        None
    }
}
