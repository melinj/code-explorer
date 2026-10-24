//! TypeScript generated from the wire types (DESIGN D3): ts-rs renders every `#[derive(TS)]` type
//! here into one module per Rust module. `scripts/gen-contracts.sh` writes the files to
//! `docs/contracts/generated/` and `web/src/contracts/generated/`; the `generated_ts` test fails when
//! the committed copies are stale.
//!
//! **Adding a wire type:** derive `TS` next to `Serialize` and list it in `MODULES` below (the
//! `every_ts_type_is_listed` test catches a forgotten entry), then run `scripts/gen-contracts.sh`.

use ts_rs::{Config, TS};

/// Header written at the top of every generated file.
pub const HEADER: &str = "// Generated from crates/cv-contracts by scripts/gen-contracts.sh (ts-rs). Do not edit.\n// The hand-written spec is docs/contracts/*.ts; check.ts compares the two.\n";

type Decl = fn(&Config) -> (String, String);

fn decl<T: TS>(cfg: &Config) -> (String, String) {
    let mut out = String::new();
    if let Some(docs) = T::docs() {
        out.push_str(&docs);
    }
    out.push_str("export ");
    out.push_str(&T::decl(cfg));
    (T::ident(cfg), out)
}

macro_rules! module {
    ($file:literal, [$($dep:literal),*], $($ty:ty),* $(,)?) => {
        ($file, &[$($dep),*], &[$(decl::<$ty> as Decl),*])
    };
}

use crate::api;
use crate::readside as rs;
use crate::review as rv;

/// (file name, files it imports from, declarations in source order).
#[allow(clippy::type_complexity)]
const MODULES: &[(&str, &[&str], &[Decl])] = &[
    module!("json.ts", [], serde_json::Value),
    module!(
        "wire.ts",
        ["json"],
        crate::Rect,
        crate::Texture,
        crate::LaneDef,
        crate::LaneRow,
        crate::LaneBand,
        crate::GridColumn,
        crate::Cluster,
        crate::SiteRef,
        crate::Hub,
        crate::EdgeBundle,
        crate::ViewStats,
        crate::Resolved,
        crate::ViewRef,
        crate::ChurnSummary,
        crate::ViewBootstrap,
        crate::InRange,
        crate::Heat,
        crate::Connectedness,
        crate::SymbolRef,
        crate::LineSpan,
        crate::LineRange,
        crate::FileFragment,
        crate::WindowCounts,
        crate::WindowFlags,
        crate::DiffStat,
        crate::WindowHeader,
        crate::TileEdge,
        crate::TileResponse,
        crate::WindowBatchRequest,
        crate::Heading,
        crate::Port,
        crate::WindowOutline,
        crate::Link,
        crate::WindowBatchResponse,
        crate::BlobText,
    ),
    module!(
        "readside.ts",
        ["json", "wire"],
        rs::Signature,
        rs::RevisionFlags,
        rs::ShortestPrefix,
        rs::Revision,
        rs::RevisionReview,
        rs::GraphEdge,
        rs::GraphRow,
        rs::RevisionsResponse,
        rs::WorkspaceWc,
        rs::StackSummary,
        rs::AgentBinding,
        rs::OpRef,
        rs::WorkspaceOverlap,
        rs::StaleReason,
        rs::WorkspaceStale,
        rs::WorkspaceSummary,
        rs::RangeWarning,
        rs::RevsetSummary,
        rs::RevsetError,
        rs::RevsetValidation,
        rs::RangeOptions,
        rs::SavedRange,
        rs::DiffLine,
        rs::WindowDiff,
        rs::MetricPoint,
        rs::CoChangePair,
        rs::CoChangeClusterPair,
        rs::MetricsResponse,
        rs::SearchTarget,
        rs::SearchHit,
        rs::ExpandAmount,
        rs::ExpandRequest,
    ),
    module!(
        "review.ts",
        ["json", "wire", "api"],
        rv::Actor,
        rv::Fingerprint,
        rv::Anchor,
        rv::Comment,
        rv::Outdated,
        rv::Thread,
        rv::NewAnchor,
        rv::NewThread,
        rv::LinePatch,
        rv::EditFile,
        rv::EditOrigin,
        rv::EditRequest,
        rv::EditWorkspace,
        rv::EditError,
        rv::EditResult,
        rv::EditedFile,
        rv::EditWarning,
        rv::LaterOp,
        rv::UndoBlocked,
        rv::BookmarkRemote,
        rv::BookmarkInfo,
        rv::RefsResponse,
        rv::LandSource,
        rv::GraphOp,
        rv::GraphOpRequest,
        rv::BookmarkMove,
        rv::GraphOpResult,
        rv::OpEntry,
        rv::OpLog,
        rv::OpDetail,
        rv::RestoreRequest,
        rv::FetchRequest,
        rv::PushRequest,
        rv::RemoteResult,
        rv::ConflictRecord,
        rv::ConflictSide,
        rv::ConflictRegion,
        rv::ConflictDetail,
        rv::ServerEvent,
        rv::ServerFrame,
        rv::ClientMessage,
        rv::EventsResponse,
    ),
    module!(
        "api.ts",
        ["json", "wire", "readside", "review"],
        api::ApiError,
        api::WorkspaceInfo,
        api::SnapshotOptions,
        api::ViewSpec,
        api::Viewport,
        api::ViewSelection,
        api::FileHunk,
        api::FileDiff,
        api::ThreadSummary,
        api::ViewState,
        api::RepoIndex,
        api::RepoCapabilities,
        api::JjInstall,
        api::SetupInfo,
        api::GitFileStat,
        api::GitCommitDetail,
        api::RepoInfo,
        api::TourStep,
        api::Tour,
        api::AgentPermissions,
        api::FragmentRef,
        api::FragmentsResponse,
        api::LayoutMove,
        api::LayoutPatch,
        api::InterdiffResponse,
        api::AgentViewState,
    ),
];

/// The generated files as (file name, contents), in a fixed order. `index.ts` re-exports them all.
pub fn generate() -> Vec<(String, String)> {
    let cfg = Config::new();
    let mut files = Vec::new();
    let mut index = String::from(HEADER);
    for (file, deps, decls) in MODULES {
        let rendered: Vec<(String, String)> = decls.iter().map(|d| d(&cfg)).collect();
        let mut out = String::from(HEADER);
        // Code only: a name mentioned in a doc comment needs no import.
        let body: String = rendered
            .iter()
            .flat_map(|(_, t)| t.lines())
            .filter(|l| {
                let l = l.trim_start();
                !(l.starts_with('*') || l.starts_with("/*") || l.starts_with("//"))
            })
            .collect::<Vec<_>>()
            .join("\n");
        for dep in *deps {
            let names: Vec<String> = names_of(&cfg, dep)
                .into_iter()
                .filter(|n| mentions(&body, n))
                .collect();
            if names.is_empty() {
                continue;
            }
            out.push_str(&format!(
                "import type {{ {} }} from \"./{dep}\";\n",
                names.join(", ")
            ));
        }
        for (_, text) in &rendered {
            out.push('\n');
            out.push_str(text);
            out.push('\n');
        }
        let stem = file.trim_end_matches(".ts");
        index.push_str(&format!("export type * from \"./{stem}\";\n"));
        files.push((file.to_string(), out));
    }
    files.push(("index.ts".into(), index));
    files
}

fn names_of(cfg: &Config, stem: &str) -> Vec<String> {
    MODULES
        .iter()
        .find(|(f, _, _)| f.trim_end_matches(".ts") == stem)
        .map(|(_, _, decls)| decls.iter().map(|d| d(cfg).0).collect())
        .unwrap_or_default()
}

/// Whether `text` uses the identifier `name` (not as part of a longer identifier).
fn mentions(text: &str, name: &str) -> bool {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    text.match_indices(name).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + name.len()..].chars().next();
        !before.is_some_and(ident) && !after.is_some_and(ident)
    })
}

/// Every declared TypeScript name, for the coverage test.
pub fn declared_names() -> Vec<String> {
    let cfg = Config::new();
    MODULES
        .iter()
        .flat_map(|(_, _, decls)| decls.iter().map(|d| d(&cfg).0))
        .collect()
}

/// How strictly `check.ts` compares a generated type with its hand-written namesake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// Mutually assignable (the default).
    Exact,
    /// What Rust sends conforms to the spec (generated assignable to spec): for responses where
    /// Rust always writes a field the spec marks optional.
    Out,
    /// What the spec allows a client to send, Rust accepts (spec assignable to generated): for
    /// request bodies Rust reads with defaults.
    In,
    /// Only the field names and the required fields are compared.
    Keys,
    /// Not compared.
    Skip,
}

/// One `ts-divergences.txt` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Divergence {
    pub name: String,
    pub level: Level,
    /// `keys-differ`: the top-level field names may differ (e.g. a spec field Rust does not have
    /// yet). Otherwise every compared type must also have exactly the spec's field names, since
    /// assignability alone does not notice a missing optional field.
    pub keys_differ: bool,
}

/// Parses `ts-divergences.txt`: `Name [exact|out|in|keys|skip] [keys-differ]  # reason` per line.
pub fn parse_divergences(text: &str) -> Vec<Divergence> {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let name = it.next()?.to_string();
            let mut d = Divergence {
                name,
                level: Level::Exact,
                keys_differ: false,
            };
            for tok in it {
                match tok {
                    "out" => d.level = Level::Out,
                    "in" => d.level = Level::In,
                    "keys" => d.level = Level::Keys,
                    "skip" => d.level = Level::Skip,
                    "keys-differ" => d.keys_differ = true,
                    _ => {}
                }
            }
            Some(d)
        })
        .collect()
}

/// Names exported (non-generic) by a hand-written spec file.
pub fn spec_names(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let rest = line
            .strip_prefix("export interface ")
            .or_else(|| line.strip_prefix("export type "));
        let Some(rest) = rest else { continue };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        let generic = rest[name.len()..].trim_start().starts_with('<');
        if !name.is_empty() && !generic && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// `check.ts`: compile-time assertions for every type that both the generated output and the spec
/// (`specs`: module stem and text of each `docs/contracts/*.ts`) declare. `tsc --noEmit` in `web/`
/// type-checks it, so a drift between Rust and the spec fails the web typecheck.
pub fn check_ts(specs: &[(String, String)], divergences: &[Divergence]) -> String {
    let generated = declared_names();
    let mut out = String::from(HEADER);
    out.push_str(CHECK_INTRO);
    out.push_str("import type * as G from \"./index\";\n");
    let mut used = Vec::new();
    let mut lines = Vec::new();
    let mut skipped = Vec::new();
    let mut rust_only = Vec::new();
    for name in &generated {
        let Some((stem, _)) = specs.iter().find(|(_, t)| spec_names(t).contains(name)) else {
            rust_only.push(name.clone());
            continue;
        };
        let d = divergences.iter().find(|d| &d.name == name);
        let level = d.map(|d| d.level).unwrap_or(Level::Exact);
        let keys_differ = d.is_some_and(|d| d.keys_differ);
        let (g, s) = (format!("G.{name}"), format!("S_{stem}.{name}"));
        let assertion = match level {
            Level::Exact => format!("Exact<U<{g}>, U<{s}>>"),
            Level::Out => format!("Extends<U<{g}>, U<{s}>>"),
            Level::In => format!("Extends<U<{s}>, U<{g}>>"),
            Level::Keys => format!("SameKeys<{g}, {s}>"),
            Level::Skip => {
                skipped.push(name.clone());
                continue;
            }
        };
        lines.push(format!("export type _{name} = Assert<{assertion}>;"));
        if !keys_differ && level != Level::Keys {
            lines.push(format!(
                "export type _{name}Keys = Assert<Exact<AllKeys<{g}>, AllKeys<{s}>>>;"
            ));
        }
        if !used.contains(stem) {
            used.push(stem.clone());
        }
    }
    used.sort();
    for stem in &used {
        out.push_str(&format!("import type * as S_{stem} from \"../{stem}\";\n"));
    }
    out.push_str(CHECK_HELPERS);
    for l in lines {
        out.push_str(&l);
        out.push('\n');
    }
    if !skipped.is_empty() {
        out.push_str(&format!(
            "\n// Not compared (ts-divergences.txt): {}\n",
            skipped.join(", ")
        ));
    }
    if !rust_only.is_empty() {
        out.push_str(&format!(
            "\n// Rust-only (no spec namesake): {}\n",
            rust_only.join(", ")
        ));
    }
    out
}

const CHECK_INTRO: &str = "\
// Compile-time comparison of the generated types (G) with the hand-written spec (S_*). Levels come
// from crates/cv-contracts/ts-divergences.txt: exact = mutually assignable (default); out = Rust's
// output conforms to the spec; in = the spec's input is accepted by Rust; keys = same field names
// and required fields. *Keys assertions also require the same top-level field names.

";

const CHECK_HELPERS: &str = "
export type Assert<T extends true> = T;
export type Extends<A, B> = [A] extends [B] ? true : false;
export type Exact<A, B> = [A] extends [B] ? ([B] extends [A] ? true : false) : false;
// U<T> compares shapes, not refinements: at any depth it drops the compile-time id brands of
// ids.ts, widens literal unions to string / number / boolean (Rust carries those as String,
// integers and bool, validated at runtime), and treats fields Rust keeps as untyped JSON
// (serde_json::Value) as matching anything.
export type U<T> = Exact<T, G.JsonValue> extends true ? any : UD<T>;
type UD<T> = T extends string
  ? string
  : T extends number
    ? number
    : T extends boolean
      ? boolean
      : T extends object
        ? { [K in keyof T]: U<T[K]> }
        : T;
export type AllKeys<T> = T extends unknown ? keyof T : never;
type RequiredKeys<T> = T extends unknown
  ? { [K in keyof T]-?: object extends Pick<T, K> ? never : K }[keyof T]
  : never;
export type SameKeys<A, B> =
  Exact<AllKeys<A>, AllKeys<B>> extends true ? Exact<RequiredKeys<A>, RequiredKeys<B>> : false;

";
