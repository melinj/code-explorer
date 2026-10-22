//! B4 `cv-channel` and the channel packs (`cv-channel-bus`, `-http`, `-env`, `-deploy`).
//! A pack says where endpoints are (rules as data, per language pack id) and how keys match.
//! It never parses code: language packs run the queries and fold the keys.

use crate::ids::RepoPath;
use crate::lang::Span;

/// A partially evaluated key: literal parts plus holes the folder could not resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyExpr {
    pub parts: Vec<KeyPart>,
    pub method: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyPart {
    Lit(String),
    /// One segment (e.g. `{instance}`, `${id}`).
    Segment,
    /// Anything to the end (e.g. a trailing optional suffix from a transform).
    Tail,
    /// Nothing known: a runtime-built key.
    Unknown { hint: String },
    /// (implemented) A constant the folder could not see from here (`NAME`, `Class.NAME`,
    /// `module.NAME`): the channel core resolves it through imports; unresolved it becomes a hole
    /// inside a template or `Unknown` as the whole key.
    Ref { qualifier: Option<String>, name: String },
}

/// (implemented, `cv_lang::keys`) L3 key fact: every key path in a config file with its line.
pub struct ConfigKeyFact {
    pub path: String,
    /// The last key as written (it may contain dots).
    pub key: String,
    pub line: u32,
    pub value: Option<String>,
    /// A plain list item (`- value`), not a key.
    pub item: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

pub struct EndpointRule {
    pub id: String,
    /// Language pack id the query is written for.
    pub lang: String,
    pub role: Role,
    /// tree-sitter query; captures @key (required), @handler, @method, @call (port line).
    pub query: String,
    pub method: Option<String>,
    pub prefix: Option<String>,
}

pub struct ConfigRule {
    pub id: String,
    pub glob: String,
    /// JSONPath-like path into YAML/TOML/JSON.
    pub path: String,
    pub role: Role,
}

pub struct KeyTransform {
    /// Function that rewrites a key, e.g. "scoped_topic".
    pub function: String,
    /// Result template, e.g. "{0}.*?" (an optional extra segment).
    pub result: String,
}

pub struct EndpointFact {
    pub rule: String,
    pub path: RepoPath,
    pub role: Role,
    pub key: KeyExpr,
    pub at: Span,
    pub handler: Option<String>,
    pub confidence: f32,
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

// Implementation notes (crates/cv-channel):
// - KeyExpr, KeyPart and ConfigKeyFact live in `cv_lang::keys` so language packs can fold keys
//   without depending on the channel core; `LanguagePack::site_extractor(queries, transforms)`
//   compiles the rule queries (captures @key, @name, @prefix, @scope, @handler, @method, @line).
// - EndpointFact carries a file index and a line instead of RepoPath + Span, plus `heuristic`
//   (guessed holes: links at 0.5, or a dynamic stub when they reach > 3 hubs or none) and
//   `from_config`.
// - The trait adds `rules()`, `sides()`, `accepts()`, `exact_wins()`, `holes_are_exact()`,
//   `link_kind()`, `stub_kind()`, `dispatch_kind()` and `link_all()` (deploy and config fields
//   link without the generic hub engine); `affinity()` and `family()` are as sketched.
pub trait ChannelPack: Send + Sync {
    /// "bus", "http", "env", "deploy", …
    fn protocol(&self) -> &'static str;
    fn endpoint_rules(&self, lang: &str) -> &[EndpointRule];
    fn config_rules(&self) -> &[ConfigRule];
    fn transforms(&self) -> &[KeyTransform];
    fn normalize(&self, key: &KeyExpr, role: Role) -> ChannelKey;
    /// Does a producer-side key reach a consumer-side key (MQTT wildcards, path templates, …)?
    fn matches(&self, producer: &ChannelKey, consumer: &ChannelKey) -> bool;
    fn hub_label(&self, key: &ChannelKey) -> String;
    /// Link confidence 0..1, or None for no link. Exact packs keep the default; heuristic packs
    /// (config fields) score key rarity, directory proximity and loader evidence from `ctx`.
    fn affinity(&self, producer: &EndpointFact, consumer: &EndpointFact, ctx: &dyn LinkContext) -> Option<f32> {
        let _ = ctx;
        let (p, c) = (self.normalize(&producer.key, producer.role), self.normalize(&consumer.key, consumer.role));
        self.matches(&p, &c).then_some(1.0)
    }
    /// Hub grouping for producers: config fields use one hub per (file family, key), e.g. "node.yaml#capacity".
    fn family(&self, producer: &EndpointFact) -> Option<String> {
        let _ = producer;
        None
    }
}

/// Repository-wide signals for heuristic matching (provided by `cv-link`).
pub trait LinkContext {
    /// Inverse document frequency of an identifier across code files (rare keys score higher).
    fn idf(&self, token: &str) -> f32;
    /// 0 = same directory, 1 = sibling directories, larger = further apart.
    fn dir_distance(&self, a: &RepoPath, b: &RepoPath) -> u32;
    /// Does `code` mention `config` by relative path or by file name (a loader for that file family)?
    fn mentions(&self, code: &RepoPath, config: &RepoPath) -> Mention;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mention {
    None,
    FileName,
    Path,
}
