//! B3 `cv-lang` and the language packs (`cv-lang-python`, `-typescript`, `-rust`, `-cpp`, `-config`).
//! tree-sitter types never cross this boundary: packs hand back spans, captures and facts.

use std::any::Any;
use std::ops::Range;

use crate::channel::KeyExpr;
use crate::ids::{BlobId, LaneId, RepoPath};

pub trait LanguagePack: Send + Sync {
    /// "python", "typescript", "rust", "cpp", "yaml", "dockerfile", …
    fn id(&self) -> &'static str;
    fn claims(&self, path: &RepoPath, head: &[u8]) -> bool;
    fn parse(&self, source: &[u8]) -> ParsedBlob;
    /// Symbols, refs, imports and the silhouette, driven by the pack's .scm queries.
    fn facts(&self, parsed: &ParsedBlob) -> BlobFacts;
    /// Runs a query from a channel rule. Query text is written against this pack's grammar.
    fn captures(&self, parsed: &ParsedBlob, query: &str) -> Vec<CaptureSet>;
    /// Folds a key expression (literals, f-strings, template literals, format!, constants).
    fn fold_key(&self, parsed: &ParsedBlob, at: Span, scope: &dyn ConstScope) -> KeyExpr;
    fn tokens(&self, parsed: &ParsedBlob, lines: Range<u32>) -> Vec<LineTokens>;
    fn resolver(&self) -> &dyn ImportResolver;
    /// Symbol kinds that may be split into member windows (class, impl, namespace).
    fn containers(&self) -> &[SymbolKind];
}

/// Opaque parse result owned by the pack (holds its tree-sitter tree).
pub struct ParsedBlob {
    pub blob: BlobId,
    pub lines: u32,
    pub inner: Box<dyn Any + Send + Sync>,
}

#[derive(Debug, Clone, Copy)]
pub struct Span {
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

pub struct CaptureSet {
    /// Capture name without "@" ("key", "handler", "call", "method") → span and text.
    pub captures: Vec<(String, Span, String)>,
}

/// Resolves a constant name to its value across files (B4 provides the implementation).
pub trait ConstScope {
    fn lookup(&self, name: &str) -> Option<KeyExpr>;
}

pub trait ImportResolver: Send + Sync {
    /// Project files the resolver reads (tsconfig.json, Cargo.toml, compile_commands.json, …).
    fn project_files(&self) -> &[&'static str];
    fn resolve(&self, from: &RepoPath, import: &ImportFact, project: &dyn ProjectIndex) -> Vec<RepoPath>;
}

pub trait ProjectIndex {
    fn exists(&self, path: &RepoPath) -> bool;
    fn read_project_file(&self, path: &RepoPath) -> Option<Vec<u8>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Interface,
    Struct,
    Enum,
    Module,
    Constant,
    Type,
    Test,
    ConfigBlock,
}

pub struct BlobFacts {
    pub blob: BlobId,
    pub lines: u32,
    pub pack_version: String,
    pub symbols: Vec<SymbolFact>,
    pub refs: Vec<RefFact>,
    pub imports: Vec<ImportFact>,
    /// Config languages only (YAML, TOML, JSON, .env): every key path with its line.
    pub config_keys: Vec<ConfigKeyFact>,
    /// OutlineEncoding (docs/contracts/wire.ts), 3 bytes per line.
    pub silhouette: Vec<u8>,
    pub syntax_errors: u32,
}

/// One key of a config file, e.g. path "inputs.capacity", leaf "capacity", at line 12.
pub struct ConfigKeyFact {
    pub path: String,
    pub leaf: String,
    pub line: u32,
    pub value: ConfigValueKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigValueKind {
    Scalar,
    Mapping,
    Sequence,
    /// `|` or `>` block: its lines are text, never keys.
    Block,
}

pub struct SymbolFact {
    pub id: u32,
    pub kind: SymbolKind,
    pub name: String,
    pub qualname: String,
    pub parent: Option<u32>,
    pub lines: Range<u32>,
    pub sig_line: u32,
    pub signature: String,
}

pub enum RefKind {
    Call,
    Inherit,
    Implement,
    Import,
    Type,
    Decorator,
}

pub struct RefFact {
    pub kind: RefKind,
    pub name: String,
    pub qualifier: Option<String>,
    pub at: Span,
    pub enclosing: Option<u32>,
}

pub struct ImportFact {
    pub local: String,
    pub module: String,
    pub imported: Option<String>,
    pub line: u32,
}

/// Flat triples [start_col, len, token_class, …] per line (wire.ts LineTokens).
pub type LineTokens = Vec<u32>;

/// Ordered lane rules from defaults + `.cv/lanes.toml`; first match wins.
pub trait LaneClassifier: Send + Sync {
    fn lanes(&self) -> &[LaneRuleSet];
    fn classify(&self, path: &RepoPath, head: &[u8]) -> LaneId;
}

pub struct LaneRuleSet {
    pub lane: LaneId,
    pub globs: Vec<String>,
    /// Content sniffs, e.g. YAML with top-level `apiVersion` and `kind` → infra.
    pub sniffs: Vec<String>,
}
