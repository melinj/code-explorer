//! B3 `cv-lang`: the language-neutral core of parsing.
//!
//! - [`LanguagePack`]: what a language crate provides (facts, tokens, import resolution, lane hints).
//! - [`Registry`]: the installed packs; the first pack that claims a path parses it.
//! - [`lanes`]: the default lane classifier (DESIGN §4.3), with code/test hints from the packs.
//! - [`chunk`]: snapshot chunking into windows (DESIGN §4.4) and file-fragment chip slots.
//! - [`silhouette`]: the 3-byte OutlineEncoding records (wire.ts).
//!
//! This crate never names a language and never depends on a grammar (DESIGN D13).

pub mod chunk;
pub mod codec;
pub mod keys;

pub const FILE_CHIP_SLOTS: u32 = 10;
pub mod lanes;
pub mod silhouette;

use std::collections::HashMap;

// wire.ts TokenClass
pub mod token {
    pub const PLAIN: u32 = 0;
    pub const KEYWORD: u32 = 1;
    pub const STRING: u32 = 2;
    pub const COMMENT: u32 = 3;
    pub const NUMBER: u32 = 4;
    pub const FUNCTION: u32 = 5;
    pub const TYPE: u32 = 6;
    pub const VARIABLE: u32 = 7;
    pub const OPERATOR: u32 = 8;
    pub const PUNCTUATION: u32 = 9;
    pub const DECORATOR: u32 = 10;
    pub const CONSTANT: u32 = 11;
    pub const PROPERTY: u32 = 12;
    pub const TAG: u32 = 13;
    pub const INVALID: u32 = 14;
    pub const BLANK: u32 = 15;
}

/// Flat triples `[start_col, len, TokenClass, …]` per line, UTF-16 columns (wire.ts LineTokens).
pub type LineTokens = Vec<u32>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

impl SymbolKind {
    /// model.ts SymbolKind spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            SymbolKind::Class => "class",
            SymbolKind::Interface => "interface",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Module => "module",
            SymbolKind::Constant => "constant",
            SymbolKind::Type => "type",
            SymbolKind::Test => "test",
            SymbolKind::ConfigBlock => "config-block",
        }
    }

    /// Kinds whose members may become their own windows when the container is too big.
    pub fn is_container(self) -> bool {
        matches!(
            self,
            SymbolKind::Class | SymbolKind::Interface | SymbolKind::Struct | SymbolKind::Module
        )
    }
}

#[derive(Debug, Clone)]
pub struct SymbolFact {
    pub kind: SymbolKind,
    pub name: String,
    pub qualname: String,
    /// Index of the enclosing symbol in `BlobFacts::symbols`.
    pub parent: Option<usize>,
    /// 1-based inclusive lines, including decorators / export keywords.
    pub start: u32,
    pub end: u32,
    /// Line of the name (the `def`/`class` line, after decorators).
    pub name_line: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefKind {
    Call,
    Inherit,
    Implement,
}

impl RefKind {
    /// model.ts LinkKind spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            RefKind::Call => "call",
            RefKind::Inherit => "inherit",
            RefKind::Implement => "implement",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RefFact {
    pub kind: RefKind,
    pub name: String,
    /// The receiver or module the name is reached through, e.g. `self`, `bus`, `utils`.
    pub qualifier: Option<String>,
    pub line: u32,
    /// Innermost symbol containing the reference.
    pub enclosing: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct ImportFact {
    /// Name bound in the importing file ("" when nothing is bound).
    pub local: String,
    /// Module specifier as written (relative forms keep their dots / `./`).
    pub module: String,
    /// Imported member; `None` when the binding is the module itself, `Some("*")` for a star.
    pub imported: Option<String>,
    pub line: u32,
}

#[derive(Debug, Clone, Default)]
pub struct BlobFacts {
    pub lines: u32,
    pub symbols: Vec<SymbolFact>,
    pub refs: Vec<RefFact>,
    pub imports: Vec<ImportFact>,
    /// One entry per line.
    pub tokens: Vec<LineTokens>,
    pub syntax_errors: u32,
    /// Config packs: every key path with its line (DESIGN §4.6, `ConfigKeyFact`).
    pub config_keys: Vec<keys::ConfigKeyFact>,
}

/// What a resolver may ask about the project.
pub trait ProjectIndex: Sync {
    fn exists(&self, path: &str) -> bool;
    /// Every path whose file name is `name`, sorted.
    fn files_named(&self, name: &str) -> &[String];
    fn read(&self, path: &str) -> Option<String>;
}

/// Path globs a pack contributes to the lane rules.
#[derive(Debug, Clone, Default)]
pub struct LaneHints {
    /// Globs of source files for the code lane, e.g. `*.ext`.
    pub code: Vec<&'static str>,
    /// Extra globs for the test lane, e.g. `test_*.ext`.
    pub test: Vec<&'static str>,
}

/// Naming conventions a pack contributes as data, so the language-neutral core (the linker, the
/// channel resolver, cluster labels) can treat its files and refs correctly without naming the
/// language (DESIGN D13). Packs fill these in; the core only compares strings against them.
#[derive(Debug, Clone, Copy, Default)]
pub struct Conventions {
    /// Ref qualifiers that denote the enclosing class's instance or class (`<q>.f()` → a member
    /// of the enclosing class).
    pub self_receivers: &'static [&'static str],
    /// File stems that stand for their directory when imported as a module (`pkg/<stem>.ext` is
    /// module `pkg`).
    pub module_entry_stems: &'static [&'static str],
    /// File stems that are their directory's entry point and are labelled by the directory in the
    /// layout (in addition to `module_entry_stems`).
    pub entry_stems: &'static [&'static str],
    /// Name prefixes of reserved names that are never guessed as lexical link candidates.
    pub reserved_prefixes: &'static [&'static str],
    /// Methods of the language's builtin and standard-library types (`append`, `push`,
    /// `startsWith`): a call `x.name()` on a receiver other than a self receiver is never guessed
    /// to be a project method of that name (it is far more often the builtin).
    pub builtin_methods: &'static [&'static str],
    /// Receivers that are builtin globals or standard modules (`console`, `Math`, `std`): calls
    /// through them are never guessed. Matched against the qualifier's first segment.
    pub builtin_receivers: &'static [&'static str],
    /// Builtin functions (`len`, `print`, `parseInt`): a bare call is never guessed.
    pub builtin_functions: &'static [&'static str],
}

impl Conventions {
    /// No conventions: nothing is a self receiver, an entry module or reserved.
    pub const NONE: Conventions = Conventions {
        self_receivers: &[],
        module_entry_stems: &[],
        entry_stems: &[],
        reserved_prefixes: &[],
        builtin_methods: &[],
        builtin_receivers: &[],
        builtin_functions: &[],
    };

    pub fn is_self_receiver(&self, qualifier: &str) -> bool {
        self.self_receivers.contains(&qualifier)
    }

    pub fn is_module_entry(&self, stem: &str) -> bool {
        self.module_entry_stems.contains(&stem)
    }

    pub fn is_entry(&self, stem: &str) -> bool {
        self.entry_stems.contains(&stem) || self.is_module_entry(stem)
    }

    pub fn is_reserved(&self, name: &str) -> bool {
        self.reserved_prefixes.iter().any(|p| name.starts_with(p))
    }

    /// Is a call of `name` through `qualifier` (None for a bare call) a builtin that lexical
    /// candidates must not guess?
    pub fn is_builtin_call(&self, qualifier: Option<&str>, name: &str) -> bool {
        match qualifier {
            None => self.builtin_functions.contains(&name),
            Some(q) if self.is_self_receiver(q) => false,
            Some(q) => {
                let first = q.split(['.', ':']).next().unwrap_or(q);
                self.builtin_receivers.contains(&first) || self.builtin_methods.contains(&name)
            }
        }
    }

    /// The module name a file stands for: its stem, or its directory's name for an entry module.
    pub fn module_name<'p>(&self, path: &'p str) -> &'p str {
        let stem = file_stem(path);
        if self.is_module_entry(stem) {
            let dir = dirname(path);
            dir.rsplit('/').next().unwrap_or(dir)
        } else {
            stem
        }
    }
}

/// File name up to its first dot: `a/b.test.ts` → `b`.
pub fn file_stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.split('.').next().unwrap_or(name)
}

pub trait LanguagePack: Send + Sync {
    /// Pack id, e.g. the language name. Only packs know these.
    fn id(&self) -> &'static str;
    fn version(&self) -> &'static str;
    fn claims(&self, path: &str) -> bool;
    /// model.ts `Lang` for a claimed path (packs may cover several dialects).
    fn lang(&self, path: &str) -> &'static str;
    fn analyze(&self, path: &str, source: &str) -> BlobFacts;
    /// Repo path of the file an import refers to, if it is in the project.
    fn resolve_import(
        &self,
        from: &str,
        import: &ImportFact,
        project: &dyn ProjectIndex,
    ) -> Option<String>;
    fn lane_hints(&self) -> LaneHints {
        LaneHints::default()
    }
    /// Naming conventions the core needs to treat this pack's files and refs (see [`Conventions`]).
    fn conventions(&self) -> Conventions {
        Conventions::NONE
    }
    /// Compiles channel endpoint queries (written for this pack's grammar) into an extractor that
    /// folds captured keys (DESIGN §4.6). Packs without channel support return `None`.
    fn site_extractor(
        &self,
        queries: &[keys::SiteQuery],
        transforms: &[keys::FoldTransform],
    ) -> Option<Box<dyn keys::SiteExtractor>> {
        let _ = (queries, transforms);
        None
    }
}

#[derive(Default)]
pub struct Registry {
    packs: Vec<Box<dyn LanguagePack>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, pack: Box<dyn LanguagePack>) {
        self.packs.push(pack);
    }

    pub fn packs(&self) -> &[Box<dyn LanguagePack>] {
        &self.packs
    }

    pub fn pack_for(&self, path: &str) -> Option<(usize, &dyn LanguagePack)> {
        self.packs
            .iter()
            .enumerate()
            .find(|(_, p)| p.claims(path))
            .map(|(i, p)| (i, p.as_ref()))
    }

    pub fn pack(&self, i: usize) -> &dyn LanguagePack {
        self.packs[i].as_ref()
    }

    /// The conventions of the pack with this id (none for an unknown or missing id).
    pub fn conventions(&self, id: Option<&str>) -> Conventions {
        id.and_then(|id| self.packs.iter().find(|p| p.id() == id))
            .map(|p| p.conventions())
            .unwrap_or_default()
    }
}

/// FNV-1a, the hash used for chip slots and ids everywhere (same as cv-mock).
pub fn hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Splits text into lines the way editors number them (`\n`, `\r\n`; no phantom last line).
pub fn split_lines(text: &str) -> Vec<&str> {
    let mut v: Vec<&str> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    if v.len() > 1 && v.last().is_some_and(|l| l.is_empty()) {
        v.pop();
    }
    v
}

/// Simple in-memory [`ProjectIndex`] over a set of paths and a content reader.
pub struct PathIndex<F: Fn(&str) -> Option<String> + Sync> {
    paths: std::collections::HashSet<String>,
    by_name: HashMap<String, Vec<String>>,
    reader: F,
}

impl<F: Fn(&str) -> Option<String> + Sync> PathIndex<F> {
    pub fn new<'a>(paths: impl IntoIterator<Item = &'a str>, reader: F) -> Self {
        let mut set = std::collections::HashSet::new();
        let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
        for p in paths {
            set.insert(p.to_string());
            let name = p.rsplit('/').next().unwrap_or(p);
            by_name
                .entry(name.to_string())
                .or_default()
                .push(p.to_string());
        }
        for v in by_name.values_mut() {
            v.sort();
        }
        PathIndex {
            paths: set,
            by_name,
            reader,
        }
    }
}

impl<F: Fn(&str) -> Option<String> + Sync> ProjectIndex for PathIndex<F> {
    fn exists(&self, path: &str) -> bool {
        self.paths.contains(path)
    }
    fn files_named(&self, name: &str) -> &[String] {
        self.by_name.get(name).map(|v| v.as_slice()).unwrap_or(&[])
    }
    fn read(&self, path: &str) -> Option<String> {
        (self.reader)(path)
    }
}

/// Normalizes `a/b/../c/./d` → `a/c/d`; `None` if it climbs above the root.
pub fn normalize_path(p: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for part in p.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            x => out.push(x),
        }
    }
    Some(out.join("/"))
}

/// Directory of a repo path ("" at the root).
pub fn dirname(p: &str) -> &str {
    p.rfind('/').map(|i| &p[..i]).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_and_paths() {
        assert_eq!(split_lines("a\nb\n"), vec!["a", "b"]);
        assert_eq!(split_lines("a\r\nb"), vec!["a", "b"]);
        assert_eq!(split_lines(""), vec![""]);
        assert_eq!(normalize_path("a/b/../c/./d").as_deref(), Some("a/c/d"));
        assert_eq!(normalize_path("../x"), None);
        assert_eq!(dirname("a/b/c.py"), "a/b");
        assert_eq!(dirname("c.py"), "");
    }
}
