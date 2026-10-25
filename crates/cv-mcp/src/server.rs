//! The MCP server: tools of docs/contracts/mcp.ts gated by profile, resources, prompts, session
//! binding and agent.attention. Works on any [`McpBackend`].
//!
//! Outputs are for a model's context: compact JSON, short ids where jj accepts prefixes, rounded
//! floats, and code as numbered text blocks. The HTTP API the UI uses is unchanged; only what MCP
//! returns is slimmed.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmcp::handler::server::common::schema_for_type;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, GetPromptRequestParams,
    GetPromptResponse, GetPromptResult, Implementation, ListPromptsResult, ListResourcesResult,
    ListToolsResult, PaginatedRequestParams, Prompt, PromptArgument, PromptMessage,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, Role, ServerCapabilities, ServerConfig, SubscribeRequestParams,
    SubscriptionFilter, Tool, UnsubscribeRequestParams,
};
use rmcp::service::{RequestContext, SubscriptionContext};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::annotate::{self, LineNote};
use crate::backend::{AgentId, BackendError, McpBackend, ThreadQuery};
use crate::params::*;
use crate::subscribe::{SUBSCRIBABLE, Subscriptions};

// ─── Profiles ────────────────────────────────────────────────────────────────

/// Tool profiles (mcp.ts MCP_PROFILES) keep the agent's tool list short.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Reader,
    Reviewer,
    Editor,
    /// Every tool.
    Full,
}

pub const READER: &[&str] = &[
    "get_view_state",
    "focus",
    "search",
    "read_window",
    "read_code",
    "get_links",
    "get_hub",
    "get_metrics",
    "get_hotspots",
    "get_structure",
    "list_workspaces",
    "bind_workspace",
    "list_revisions",
    "get_diff",
    "get_interdiff",
    "wait_for_events",
];

pub const REVIEWER: &[&str] = &[
    "get_view_state",
    "set_view",
    "focus",
    "create_tour",
    "search",
    "read_window",
    "read_code",
    "get_links",
    "get_hub",
    "get_metrics",
    "get_hotspots",
    "get_structure",
    "list_workspaces",
    "bind_workspace",
    "list_threads",
    "get_thread",
    "create_thread",
    "reply",
    "list_revisions",
    "get_diff",
    "get_interdiff",
    "declare_link",
    "rate_link",
    "label_cluster",
    "wait_for_events",
];

/// Addressing review threads end to end: find them (with their code), edit and answer in one call,
/// resolve conflicts, post findings. Graph, metrics, tours and navigation live in reviewer/full.
pub const EDITOR: &[&str] = &[
    "list_threads",
    "get_thread",
    "reply",
    "create_thread",
    "edit_code",
    "apply_suggestion",
    "read_code",
    "search",
    "get_links",
    "get_diff",
    "list_conflicts",
    "get_conflict",
    "resolve_conflict",
    "wait_for_events",
    "set_view",
];

pub const FULL: &[&str] = &[
    "get_view_state",
    "set_view",
    "focus",
    "create_tour",
    "search",
    "read_window",
    "read_code",
    "get_links",
    "get_hub",
    "get_metrics",
    "get_hotspots",
    "get_structure",
    "list_workspaces",
    "bind_workspace",
    "list_threads",
    "get_thread",
    "create_thread",
    "reply",
    "set_thread_status",
    "apply_suggestion",
    "list_revisions",
    "get_diff",
    "get_interdiff",
    "edit_code",
    "propose_edit",
    "apply_edit",
    "list_conflicts",
    "get_conflict",
    "resolve_conflict",
    "undo_edit",
    "declare_link",
    "rate_link",
    "label_cluster",
    "wait_for_events",
];

impl Profile {
    pub fn tools(self) -> &'static [&'static str] {
        match self {
            Profile::Reader => READER,
            Profile::Reviewer => REVIEWER,
            Profile::Editor => EDITOR,
            Profile::Full => FULL,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Profile::Reader => "reader",
            Profile::Reviewer => "reviewer",
            Profile::Editor => "editor",
            Profile::Full => "full",
        }
    }
    fn has(self, tool: &str) -> bool {
        self.tools().contains(&tool)
    }
}

impl std::str::FromStr for Profile {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "reader" => Ok(Profile::Reader),
            "reviewer" => Ok(Profile::Reviewer),
            "editor" => Ok(Profile::Editor),
            "full" => Ok(Profile::Full),
            _ => Err(format!(
                "unknown profile {s:?} (reader, reviewer, editor or full)"
            )),
        }
    }
}

/// A schemars schema without generator boilerplate: no `$schema`, `title`, `format`,
/// `minimum: 0` or `default: null`; `["T", "null"]` and `anyOf [X, null]` become plain optional
/// properties (they are not in `required`); `$ref`s are inlined.
pub fn slim_schema(schema: Value) -> Value {
    fn go(v: Value, defs: &Map<String, Value>, depth: usize) -> Value {
        let Value::Object(mut o) = v else {
            return match v {
                Value::Array(a) => {
                    Value::Array(a.into_iter().map(|x| go(x, defs, depth)).collect())
                }
                x => x,
            };
        };
        if let Some(Value::String(r)) = o.remove("$ref")
            && depth < 8
            && let Some(d) = r.strip_prefix("#/$defs/").and_then(|n| defs.get(n))
            && let Value::Object(inner) = go(d.clone(), defs, depth + 1)
        {
            for (k, x) in inner {
                // The property's own description wins over the type's.
                o.entry(k).or_insert(x);
            }
        }
        for k in ["$schema", "title", "format", "$defs"] {
            o.remove(k);
        }
        if o.get("default") == Some(&Value::Null) {
            o.remove("default");
        }
        if o.get("minimum").and_then(Value::as_f64) == Some(0.0) {
            o.remove("minimum");
        }
        if let Some(Value::Array(ts)) = o.get("type") {
            let ts: Vec<Value> = ts.iter().filter(|t| *t != "null").cloned().collect();
            o.insert(
                "type".into(),
                if ts.len() == 1 {
                    ts[0].clone()
                } else {
                    Value::Array(ts)
                },
            );
        }
        for k in ["anyOf", "oneOf"] {
            if let Some(Value::Array(alts)) = o.get(k) {
                let rest: Vec<Value> = alts
                    .iter()
                    .filter(|a| a.get("type") != Some(&json!("null")))
                    .cloned()
                    .collect();
                if rest.len() == 1 && rest.len() < alts.len() {
                    o.remove(k);
                    if let Value::Object(inner) = go(rest[0].clone(), defs, depth + 1) {
                        for (k, x) in inner {
                            o.entry(k).or_insert(x);
                        }
                    }
                }
            }
        }
        // A nullable enum lists null among its values.
        if let Some(Value::Array(e)) = o.get_mut("enum") {
            e.retain(|x| !x.is_null());
        }
        Value::Object(
            o.into_iter()
                .map(|(k, x)| {
                    let x = match k.as_str() {
                        "properties" | "$defs" => match x {
                            Value::Object(ps) => Value::Object(
                                ps.into_iter()
                                    .map(|(n, p)| (n, go(p, defs, depth)))
                                    .collect(),
                            ),
                            x => x,
                        },
                        "items" | "anyOf" | "oneOf" | "allOf" | "additionalProperties" => {
                            go(x, defs, depth)
                        }
                        _ => x,
                    };
                    (k, x)
                })
                .collect(),
        )
    }
    let defs = schema
        .get("$defs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    go(schema, &defs, 0)
}

/// Every tool with its description and input schema.
pub fn all_tools() -> Vec<Tool> {
    fn t<P: schemars::JsonSchema + 'static>(name: &'static str, desc: &'static str) -> Tool {
        let schema = slim_schema(Value::Object((*schema_for_type::<P>()).clone()));
        let Value::Object(schema) = schema else {
            unreachable!("object schema")
        };
        Tool::new(name, desc, Arc::new(schema))
    }
    vec![
        t::<Empty>(
            "get_view_state",
            "What the human is looking at (view, viewport, focused window, selection). { view: null } without a UI.",
        ),
        t::<SetView>(
            "set_view",
            "Switch the human's view to a ViewSpec. Only when the human allows agent navigation.",
        ),
        t::<Focus>(
            "focus",
            "Point the human at a target: moves their camera when they follow you, else shows a suggestion.",
        ),
        t::<CreateTour>(
            "create_tour",
            "Create a guided tour: ordered steps the human walks through.",
        ),
        t::<Search>(
            "search",
            "Find symbols, files, hubs, threads and revisions. Hits carry path:line and a window id.",
        ),
        t::<ReadWindow>(
            "read_window",
            "Read one window (symbol or chunk) as numbered code with links (ports) and threads inline.",
        ),
        t::<ReadCode>(
            "read_code",
            "Read lines of a file as numbered code with links (ports) and threads inline. Its blob is the baseBlob for edit_code.",
        ),
        t::<GetLinks>(
            "get_links",
            "Links into and out of a symbol (calls, imports, hub producers/consumers).",
        ),
        t::<GetHub>(
            "get_hub",
            "A channel hub (event topic, HTTP endpoint, config key) with its producers and consumers.",
        ),
        t::<GetMetrics>(
            "get_metrics",
            "Heat (change frequency), connectedness and co-change partners of a window or file.",
        ),
        t::<GetHotspots>(
            "get_hotspots",
            "Review priority ranking: geometric mean of heat and connectedness percentiles, weighted up for code whose links leave its folder (outside share).",
        ),
        t::<GetStructure>(
            "get_structure",
            "How the folder tree fits the code, for refactoring: leaky directories (flow pressure = share of their links crossing the boundary; good modules have a narrow interface and long chains inside), files to split out (busy among quiet siblings), files pulled toward another directory, and how far folders diverge from the dependency clusters.",
        ),
        t::<Empty>(
            "list_workspaces",
            "jj workspaces with their stacks, unsnapshotted files, bound agents and overlaps.",
        ),
        t::<BindWorkspace>(
            "bind_workspace",
            "Work in another workspace: reads, edits and default revsets follow it (default: the served one).",
        ),
        t::<ListThreads>(
            "list_threads",
            "Review threads, filtered. withCode: each with its comments and the code around it, ready for edit_code {thread}.",
        ),
        t::<GetThread>(
            "get_thread",
            "A thread with its comments and the code of the enclosing symbol (numbered, ≤ 80 lines).",
        ),
        t::<CreateThread>(
            "create_thread",
            "Post a new thread anchored to lines of a file (a ```suggestion block proposes a replacement).",
        ),
        t::<Reply>("reply", "Reply in a thread and/or set its status."),
        t::<SetThreadStatus>("set_thread_status", "Set a thread's status."),
        t::<ApplySuggestion>(
            "apply_suggestion",
            "Apply the thread's ```suggestion to its anchored lines, optionally replying in the same call.",
        ),
        t::<ListRevisions>(
            "list_revisions",
            "Revisions of a revset with their review state.",
        ),
        t::<GetDiff>(
            "get_diff",
            "Unified diff of a range (default: the workspace's stack), with each file's status.",
        ),
        t::<GetInterdiff>(
            "get_interdiff",
            "What changed in a change since the commit the human reviewed.",
        ),
        t::<EditCode>(
            "edit_code",
            "Edit a file with line patches (against baseBlob) or whole content; with thread, rev/path/baseBlob come from it and reply answers it in the same call.",
        ),
        t::<EditCode>(
            "propose_edit",
            "edit_code with dryRun: the predicted result and its id for apply_edit.",
        ),
        t::<ApplyEdit>("apply_edit", "Apply an edit previewed with dryRun."),
        t::<ListConflicts>("list_conflicts", "Conflicted files in a revset."),
        t::<GetConflict>(
            "get_conflict",
            "A conflicted file: text with markers and the sides of each region.",
        ),
        t::<ResolveConflict>(
            "resolve_conflict",
            "Resolve a conflicted file with its whole new content.",
        ),
        t::<ApplyEdit>(
            "undo_edit",
            "Undo an applied edit (reverts its jj operation).",
        ),
        t::<DeclareLink>(
            "declare_link",
            "Declare a link the indexer missed (e.g. dynamic dispatch) between two sites or a site and a hub.",
        ),
        t::<RateLink>("rate_link", "Confirm or reject an inferred link."),
        t::<LabelCluster>(
            "label_cluster",
            "Name a cluster of the map and summarize it.",
        ),
        t::<WaitForEvents>(
            "wait_for_events",
            "Long-poll (≤ 60 s) for events after a cursor (thread.upserted, review.submitted, op.changed, …). Pass the returned cursor to the next call.",
        ),
    ]
}

/// Event kinds wait_for_events waits for by default: every type except presence noise
/// (agent.attention, index.progress; hello is not an event).
const DEFAULT_KINDS: &[&str] = &[
    "op.changed",
    "view.invalidated",
    "thread.upserted",
    "review.submitted",
    "edit.updated",
    "conflicts.changed",
    "focus.request",
    "tour.created",
    "agent.permissions",
    "workspace.updated",
    "workspace.recovered",
    "view.requested",
];

/// Every ServerEvent type an agent can wait for.
const EVENT_KINDS: &[&str] = &[
    "op.changed",
    "view.invalidated",
    "thread.upserted",
    "review.submitted",
    "edit.updated",
    "conflicts.changed",
    "focus.request",
    "tour.created",
    "agent.permissions",
    "workspace.updated",
    "workspace.recovered",
    "view.requested",
    "agent.attention",
    "index.progress",
];

pub const RESOURCE_VIEW: &str = "cv://view/state";
pub const RESOURCE_OPEN_THREADS: &str = "cv://threads?status=open";
pub const RESOURCE_CONFLICTS: &str = "cv://conflicts";

/// Lines of code shown with a thread by default (the enclosing symbol, trimmed around the anchor).
const THREAD_CODE_LINES: u32 = 80;
/// Diff lines get_diff prints before it lists the remaining files without hunks.
const MAX_DIFF_LINES: usize = 600;

// ─── Server ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct McpConfig {
    pub profile: Profile,
    /// Agent name for attribution; default derived from the MCP client ("claude-code" → "claude").
    pub agent: Option<String>,
    /// Workspace to bind at start (same as calling bind_workspace).
    pub workspace: Option<String>,
    /// The client's working directory (stdio shim): its jj workspace is the default one.
    pub cwd: Option<PathBuf>,
}

impl Default for McpConfig {
    fn default() -> Self {
        McpConfig {
            profile: Profile::Editor,
            agent: None,
            workspace: None,
            cwd: None,
        }
    }
}

/// What a view's headers were fetched at: (layoutVersion, resolved head commit). Snapshot views
/// keep their id while their revision moves (the server refreshes them after each operation), so
/// the head is part of the stamp.
type Stamp = (u64, String);

/// (stamp, WindowHeaders) of one view.
type CachedWindows = (Stamp, Arc<Vec<Value>>);

/// State shared by every session of one server (HTTP sessions come and go; bindings stay).
pub struct Shared {
    backend: Arc<dyn McpBackend>,
    config: McpConfig,
    /// agent name → bound workspace.
    bindings: Mutex<HashMap<String, String>>,
    /// The workspace of agents that did not bind one: the cwd's, the served one, or the only one.
    default_ws: tokio::sync::OnceCell<Option<String>>,
    /// view id → its latest ViewRef (from the last create_view that returned it).
    views: Mutex<HashMap<String, Value>>,
    /// view id → (layoutVersion, its WindowHeaders).
    windows: Mutex<HashMap<String, CachedWindows>>,
    /// (agent, path) → the blob of the code last shown to the agent: edit_code {thread}'s default
    /// baseBlob, so an edit against code the agent has not seen since is rejected as stale.
    shown: Mutex<HashMap<(String, String), String>>,
    /// previewed edit id → the thread answer (thread, reply, status) its edit_code {dryRun} asked
    /// for: apply_edit posts it once the edit applied (b13).
    answers: Mutex<HashMap<String, Answer>>,
    /// Resource subscribers of every session and the watcher that notifies them (N10).
    subs: Arc<Subscriptions>,
}

/// A thread answer posted once an edit applies: (thread, reply, status on the wire).
type Answer = (String, Option<String>, Option<String>);

#[derive(Clone)]
pub struct CvMcp {
    shared: Arc<Shared>,
    profile: Profile,
    /// This session's resources/subscribe registration in [`Shared::subs`].
    session_sub: Arc<Mutex<Option<u64>>>,
}

type ToolOut = Result<Out, String>;

tokio::task_local! {
    /// Set by [`CvMcp::attend`] during one tool call: `call` emits a generic agent.attention for
    /// tools that did not record a more specific one, so every tool call is visible (§8).
    static ATTENDED: std::cell::Cell<bool>;
}

/// Most window ids one agent.attention event carries.
const ATTENTION_WINDOWS: usize = 64;

enum Out {
    Json(Value),
    /// AnnotatedCode: metadata as JSON plus the annotated text as its own block.
    Code(Value),
    /// Text blocks as they are (JSON metadata and code, alternating).
    Blocks(Vec<String>),
}

fn err(e: BackendError) -> String {
    e.to_string()
}

fn compact(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// "claude-code" → "claude", "opencode" → "opencode", else the client name.
pub fn agent_name_for_client(client: &str) -> String {
    let c = client.to_lowercase();
    if c.contains("claude") {
        "claude".into()
    } else if c.contains("opencode") {
        "opencode".into()
    } else {
        client.to_string()
    }
}

/// The default workspace among `workspaces`: the one whose root holds `cwd`, else the served one,
/// else the only one.
pub fn pick_default_workspace(
    workspaces: &[Value],
    cwd: Option<&std::path::Path>,
) -> Option<String> {
    let name = |w: &Value| w["name"].as_str().map(String::from);
    if let Some(cwd) = cwd {
        let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        let inside = workspaces
            .iter()
            .filter_map(|w| {
                let root = PathBuf::from(w["root"].as_str()?);
                let root = root.canonicalize().unwrap_or(root);
                cwd.starts_with(&root)
                    .then(|| (root.components().count(), w))
            })
            .max_by_key(|(n, _)| *n);
        if let Some((_, w)) = inside {
            return name(w);
        }
    }
    if let Some(w) = workspaces.iter().find(|w| w["serving"] == true) {
        return name(w);
    }
    match workspaces {
        [w] => name(w),
        _ => None,
    }
}

fn lines_overlap(h: &Value, start: u32, end: u32) -> bool {
    let s = h["lines"]["start"].as_u64().unwrap_or(0) as u32;
    let e = h["lines"]["end"].as_u64().unwrap_or(0) as u32;
    s <= end && start <= e
}

fn id_of(h: &Value) -> String {
    h["id"].as_str().unwrap_or("").to_string()
}

fn short(s: &Value, n: usize) -> Value {
    match s.as_str() {
        Some(x) => json!(x.chars().take(n).collect::<String>()),
        None => s.clone(),
    }
}

/// Floats rounded to 3 significant digits, recursively.
pub fn round_floats(v: &Value) -> Value {
    match v {
        Value::Number(n) if n.is_f64() => {
            let x = n.as_f64().unwrap_or(0.0);
            if x == 0.0 || !x.is_finite() {
                return json!(x);
            }
            let mag = x.abs().log10().floor() as i32;
            let f = 10f64.powi(2 - mag);
            let r = (x * f).round() / f;
            // Whole numbers print without ".0".
            if r.fract() == 0.0 && r.abs() < 1e15 {
                json!(r as i64)
            } else {
                json!(r)
            }
        }
        Value::Array(a) => Value::Array(a.iter().map(round_floats).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), round_floats(x)))
                .collect(),
        ),
        x => x.clone(),
    }
}

/// "path:line", "hub:<id>" or the raw value of a link end.
fn site_str(end: &Value) -> String {
    if let Some(p) = end["path"].as_str() {
        return match end["line"].as_u64() {
            Some(l) => format!("{p}:{l}"),
            None => p.to_string(),
        };
    }
    if let Some(h) = end["hub"].as_str() {
        return format!("hub:{h}");
    }
    end.to_string()
}

/// A Link as the agent needs it: id (for rate_link), kind, label and both ends as "path:line".
fn slim_link(l: &Value) -> Value {
    let mut o = json!({ "id": l["id"], "kind": l["kind"], "from": site_str(&l["from"]), "to": site_str(&l["to"]) });
    if let Some(lb) = l["label"].as_str() {
        o["label"] = json!(lb);
    }
    if let Some(c) = l["confidence"].as_f64()
        && c < 1.0
    {
        o["confidence"] = round_floats(&json!(c));
    }
    if !l["feedback"].is_null() {
        o["feedback"] = l["feedback"].clone();
    }
    o
}

/// { id, status, comments: n }: what a thread write returns to the agent.
fn thread_ack(t: &Value) -> Value {
    json!({
        "id": t["id"], "status": t["status"],
        "comments": t["comments"].as_array().map(|c| c.len()).unwrap_or(0),
    })
}

/// A thread without fingerprints, author objects, comment ids or timestamps.
fn thread_meta(t: &Value) -> Value {
    let a = &t["anchor"];
    let mut m = json!({
        "id": t["id"], "status": t["status"], "kind": t["kind"],
        "path": a["path"], "start": a["start"], "end": a["end"], "change": short(&a["change"], 12),
    });
    for k in ["assignee", "severity"] {
        if !t[k].is_null() {
            m[k] = t[k].clone();
        }
    }
    if !t["outdated"].is_null() {
        m["outdated"] = t["outdated"]["reason"].clone();
    }
    m["comments"] = json!(
        t["comments"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| json!({ "by": c["author"]["name"], "body": c["body"] }))
            .collect::<Vec<_>>()
    );
    m
}

/// An EditResult for the agent: no actor or op ids, short change ids, workspaces only when
/// something happened to them besides the expected update.
fn slim_edit(r: &Value) -> Value {
    let mut o = json!({ "id": r["id"], "status": r["status"] });
    if !r["created"].is_null() {
        o["created"] = short(&r["created"], 12);
    }
    if let Some(rw) = r["rewritten"].as_array()
        && !rw.is_empty()
    {
        o["rewritten"] = json!(rw.iter().map(|c| short(c, 12)).collect::<Vec<_>>());
    }
    if let Some(files) = r["files"].as_array()
        && !files.is_empty()
    {
        o["files"] = json!(files);
    }
    if let Some(cs) = r["conflicts"].as_array()
        && !cs.is_empty()
    {
        o["conflicts"] = json!(
            cs.iter()
                .map(|c| json!({ "change": short(&c["change"], 12), "path": c["path"], "sides": c["sides"] }))
                .collect::<Vec<_>>()
        );
    }
    let ws: Vec<Value> = r["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| !w["recoveredInto"].is_null())
        .cloned()
        .collect();
    if !ws.is_empty() {
        o["workspaces"] = json!(ws);
    }
    if !r["error"].is_null() {
        o["error"] = r["error"].clone();
    }
    o
}

/// A rejected or failed EditResult as a one-line, actionable tool error.
fn edit_error(r: &Value) -> Option<String> {
    if !matches!(r["status"].as_str(), Some("rejected") | Some("failed")) {
        return None;
    }
    let code = r["error"]["code"].as_str().unwrap_or("failed");
    let msg = r["error"]["message"].as_str().unwrap_or("");
    let hint = match code {
        "stale-base" => {
            "re-read the file (get_thread / read_code) and retry with its blob and line numbers"
        }
        "immutable" => "the target is immutable: retry with mode \"branch\"",
        "invalid-target" => "rev must name one visible change (e.g. the thread's anchor change)",
        "locked" => "another edit holds the mutation queue: retry in a moment",
        _ => "check the patches (line numbers of the shown blob) and retry",
    };
    Some(format!(
        "{code}: {hint} ({msg}) [edit {}]",
        r["id"].as_str().unwrap_or("?")
    ))
}

/// A WorkspaceSummary without the op hash and the absolute root.
fn slim_workspace(w: &Value) -> Value {
    let mut w = w.clone();
    if let Some(o) = w.as_object_mut() {
        o.remove("root");
    }
    if let Some(o) = w["lastOp"].as_object_mut() {
        o.remove("op");
    }
    w
}

/// A server event for the agent: threads as ThreadSummary, edits slimmed, op hashes shortened.
fn slim_event(e: &Value) -> Value {
    match e["type"].as_str() {
        Some("thread.upserted") => {
            json!({ "type": e["type"], "thread": annotate::summary(&e["thread"]) })
        }
        Some("edit.updated") => json!({ "type": e["type"], "edit": slim_edit(&e["edit"]) }),
        Some("op.changed") => {
            let changes: Vec<Value> = e["changes"]
                .as_array()
                .into_iter()
                .flatten()
                .take(10)
                .map(|c| short(c, 12))
                .collect();
            json!({ "type": e["type"], "op": short(&e["op"], 12), "changes": changes, "actor": e["actor"] })
        }
        Some("workspace.updated") => {
            json!({ "type": e["type"], "workspace": slim_workspace(&e["workspace"]) })
        }
        Some("tour.created") => json!({ "type": e["type"], "tour": {
            "id": e["tour"]["id"], "title": e["tour"]["title"],
            "steps": e["tour"]["steps"].as_array().map(|s| s.len()).unwrap_or(0) } }),
        _ => e.clone(),
    }
}

/// The innermost heading span holding [s, e]: from a heading to the line before the next heading
/// at the same or a shallower depth (or `limit`).
fn enclosing(headings: &[(u32, u64)], s: u32, e: u32, limit: u32) -> Option<(u32, u32)> {
    let mut best: Option<(u32, u32)> = None;
    for (i, &(line, depth)) in headings.iter().enumerate() {
        if line > s {
            break;
        }
        let end = headings[i + 1..]
            .iter()
            .find(|(_, d)| *d <= depth)
            .map(|(l, _)| l.saturating_sub(1))
            .unwrap_or(limit)
            .max(line);
        if end >= e && best.is_none_or(|(b, _)| line >= b) {
            best = Some((line, end));
        }
    }
    best
}

/// What a `target` string resolved to.
struct Target {
    path: String,
    line: Option<u32>,
    windows: Vec<Value>,
    /// A line or symbol inside a file (get_links narrows to it), not a window or whole file.
    pointed: bool,
}

impl CvMcp {
    pub fn new(backend: Arc<dyn McpBackend>, config: McpConfig) -> Self {
        let profile = config.profile;
        Self::from_shared(Self::shared(backend, config), profile)
    }

    /// Shared state for a server whose sessions are created later (Streamable HTTP).
    pub fn shared(backend: Arc<dyn McpBackend>, config: McpConfig) -> Arc<Shared> {
        let mut bindings = HashMap::new();
        if let (Some(a), Some(w)) = (&config.agent, &config.workspace) {
            bindings.insert(a.clone(), w.clone());
        }
        Arc::new(Shared {
            backend,
            config,
            bindings: Mutex::new(bindings),
            default_ws: tokio::sync::OnceCell::new(),
            views: Mutex::new(HashMap::new()),
            windows: Mutex::new(HashMap::new()),
            shown: Mutex::new(HashMap::new()),
            answers: Mutex::new(HashMap::new()),
            subs: Arc::new(Subscriptions::default()),
        })
    }

    pub fn from_shared(shared: Arc<Shared>, profile: Profile) -> Self {
        CvMcp {
            shared,
            profile,
            session_sub: Arc::new(Mutex::new(None)),
        }
    }

    /// Resource subscribers registered on this server (all sessions).
    pub fn subscriber_count(&self) -> usize {
        self.shared.subs.len()
    }

    pub fn profile(&self) -> Profile {
        self.profile
    }

    fn backend(&self) -> &dyn McpBackend {
        self.shared.backend.as_ref()
    }

    /// The workspace of an agent that did not bind one (resolved once per server).
    async fn default_workspace(&self) -> Option<String> {
        self.shared
            .default_ws
            .get_or_try_init(|| async {
                let ws = self.backend().workspaces().await.map_err(|_| ())?;
                Ok::<_, ()>(pick_default_workspace(
                    &ws,
                    self.shared.config.cwd.as_deref(),
                ))
            })
            .await
            .ok()
            .cloned()
            .flatten()
    }

    /// The agent behind this request. Over HTTP, `X-CV-Agent` and `X-CV-Workspace` request
    /// headers name the agent and its workspace (as they do for the /api/v1 routes).
    async fn who(&self, ctx: Option<&RequestContext<RoleServer>>) -> AgentId {
        let client = ctx.and_then(|c| c.client_info()).map(|i| i.name);
        let header = |name: &str| {
            ctx.and_then(|c| c.extensions.get::<axum::http::request::Parts>())
                .and_then(|p| p.headers.get(name))
                .and_then(|v| v.to_str().ok())
                .filter(|v| !v.is_empty())
                .map(String::from)
        };
        let name = self
            .shared
            .config
            .agent
            .clone()
            .or_else(|| header("x-cv-agent"))
            .or_else(|| client.as_deref().map(agent_name_for_client))
            .unwrap_or_else(|| "agent".into());
        let bound = self.shared.bindings.lock().unwrap().get(&name).cloned();
        let workspace = match bound
            .or_else(|| header("x-cv-workspace"))
            .or_else(|| self.shared.config.workspace.clone())
        {
            Some(w) => Some(w),
            None => self.default_workspace().await,
        };
        AgentId {
            name,
            client,
            workspace,
        }
    }

    /// After a write: forget what the views resolved to, so the next read re-resolves them.
    fn invalidate(&self) {
        self.shared.views.lock().unwrap().clear();
    }

    fn remember(&self, who: &AgentId, path: &str, blob: &Value) {
        if let Some(b) = blob.as_str().filter(|b| !b.is_empty()) {
            self.shared
                .shown
                .lock()
                .unwrap()
                .insert((who.name.clone(), path.to_string()), b.to_string());
        }
    }

    fn shown_blob(&self, who: &AgentId, path: &str) -> Option<String> {
        self.shared
            .shown
            .lock()
            .unwrap()
            .get(&(who.name.clone(), path.to_string()))
            .cloned()
    }

    /// POST /views, remembering the ViewRef so window headers are refetched when it moved.
    async fn create_view(&self, spec: Value) -> Result<Value, BackendError> {
        let v = self.backend().create_view(spec).await?;
        if let Some(id) = v["id"].as_str() {
            self.shared
                .views
                .lock()
                .unwrap()
                .insert(id.to_string(), v.clone());
        }
        Ok(v)
    }

    fn stamp(&self, view: &str) -> Option<Stamp> {
        self.shared.views.lock().unwrap().get(view).map(|v| {
            (
                v["layoutVersion"].as_u64().unwrap_or(0),
                v["resolved"]["head"].as_str().unwrap_or("").to_string(),
            )
        })
    }

    /// The snapshot view the agent works in: its workspace's @, else the served @. Resolved on
    /// every call, so reads follow @ across the agent's own edits and other people's operations;
    /// the server answers with the same view id (and window ids) as long as the spec is the same.
    async fn view(&self, who: &AgentId) -> Result<Value, String> {
        let key = who
            .workspace
            .as_ref()
            .map(|w| format!("{w}@"))
            .unwrap_or_else(|| "@".into());
        let mut spec = json!({ "mode": "snapshot", "rev": key });
        if let Some(w) = &who.workspace {
            spec["options"] = json!({ "workspace": w });
        }
        match self.create_view(spec).await {
            Ok(v) => Ok(v),
            // A server that cannot resolve "<ws>@" still serves its own @.
            Err(_) if key != "@" => self
                .create_view(json!({ "mode": "snapshot", "rev": "@" }))
                .await
                .map_err(err),
            Err(e) => Err(err(e)),
        }
    }

    async fn view_id(&self, who: &AgentId) -> Result<String, String> {
        Ok(self.view(who).await?["id"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    /// Every WindowHeader of a view, refetched only when the view's stamp changed (a full-repo
    /// header list can be tens of MB).
    async fn windows(&self, view: &str) -> Result<Arc<Vec<Value>>, String> {
        let stamp = self.stamp(view);
        if let Some(st) = &stamp
            && let Some((s, w)) = self.shared.windows.lock().unwrap().get(view)
            && s == st
        {
            return Ok(w.clone());
        }
        let w = Arc::new(self.backend().view_windows(view).await.map_err(err)?);
        if let Some(st) = stamp {
            self.shared
                .windows
                .lock()
                .unwrap()
                .insert(view.to_string(), (st, w.clone()));
        }
        Ok(w)
    }

    async fn path_windows(&self, view: &str, path: &str) -> Result<Vec<Value>, String> {
        let mut ws: Vec<Value> = self
            .windows(view)
            .await?
            .iter()
            .filter(|h| h["path"] == path)
            .cloned()
            .collect();
        ws.sort_by_key(|h| h["lines"]["start"].as_u64().unwrap_or(0));
        Ok(ws)
    }

    /// Window ids of `paths` in the agent's view (for agent.attention), at most
    /// [`ATTENTION_WINDOWS`]. Empty when the view or its windows are unavailable.
    async fn windows_of_paths(&self, who: &AgentId, paths: &[String]) -> Vec<String> {
        let Ok(view) = self.view_id(who).await else {
            return Vec::new();
        };
        let Ok(all) = self.windows(&view).await else {
            return Vec::new();
        };
        let wanted: HashSet<&str> = paths.iter().map(String::as_str).collect();
        all.iter()
            .filter(|h| h["path"].as_str().is_some_and(|p| wanted.contains(p)))
            .map(id_of)
            .take(ATTENTION_WINDOWS)
            .collect()
    }

    async fn find_thread(&self, id: &str, path: Option<&str>) -> Result<Value, String> {
        let q = ThreadQuery {
            path: path.map(String::from),
            ..Default::default()
        };
        self.backend()
            .threads(&q)
            .await
            .map_err(err)?
            .into_iter()
            .find(|t| t["id"] == id)
            .ok_or_else(|| format!("unknown thread {id}"))
    }

    /// (path, line) of a symbol by qualname or name, through the symbol index search uses:
    /// "stored_attempts", "SurfaceMechanics.families_for", "Type::method".
    async fn symbol_site(&self, view: &str, target: &str) -> Option<(String, u32)> {
        let norm = target.replace("::", ".");
        let last = norm.rsplit('.').next().unwrap_or(&norm).to_string();
        let mut queries = vec![target.to_string()];
        for q in [norm.clone(), last.clone()] {
            if !queries.contains(&q) {
                queries.push(q);
            }
        }
        let suffix = format!(".{norm}");
        for q in queries {
            let Ok(hits) = self
                .backend()
                .search(&q, Some("symbol"), Some(view), 20)
                .await
            else {
                continue;
            };
            let best = hits
                .iter()
                .filter(|h| h["kind"] == "symbol")
                .filter_map(|h| {
                    let label = h["label"].as_str()?.replace("::", ".");
                    let rank = if label == norm {
                        0
                    } else if label.ends_with(&suffix) {
                        1
                    } else {
                        return None;
                    };
                    Some((rank, h))
                })
                .min_by_key(|(r, _)| *r)
                .map(|(_, h)| h);
            if let Some(h) = best {
                let site = &h["target"]["site"];
                if let (Some(p), Some(l)) = (site["path"].as_str(), site["line"].as_u64()) {
                    return Some((p.to_string(), l as u32));
                }
                let (p, l) = h["detail"].as_str()?.rsplit_once(':')?;
                return Some((p.to_string(), l.parse().ok()?));
            }
        }
        None
    }

    /// Resolve a WindowId, ThreadId, "path:line", path or qualname.
    async fn resolve(&self, view: &str, target: &str) -> Result<Target, String> {
        let all = self.windows(view).await?;
        if let Some(h) = all.iter().find(|h| h["id"] == target) {
            return Ok(Target {
                path: h["path"].as_str().unwrap_or("").into(),
                line: h["lines"]["start"].as_u64().map(|l| l as u32),
                windows: vec![h.clone()],
                pointed: false,
            });
        }
        let with = |path: &str, line: Option<u32>| Target {
            path: path.to_string(),
            line,
            windows: all
                .iter()
                .filter(|h| h["path"] == path && line.is_none_or(|l| lines_overlap(h, l, l)))
                .cloned()
                .collect(),
            pointed: line.is_some(),
        };
        if let Some((p, l)) = target.rsplit_once(':')
            && let Ok(l) = l.parse::<u32>()
        {
            return Ok(with(p, Some(l)));
        }
        if all.iter().any(|h| h["path"] == target) {
            return Ok(with(target, None));
        }
        let by_symbol: Vec<Value> = all
            .iter()
            .filter(|h| {
                let q = h["symbol"]["qualname"].as_str().unwrap_or("");
                let n = h["symbol"]["name"].as_str().unwrap_or("");
                !q.is_empty() && (q == target || n == target || q.ends_with(&format!(".{target}")))
            })
            .cloned()
            .collect();
        if let Some(h) = by_symbol.first() {
            return Ok(Target {
                path: h["path"].as_str().unwrap_or("").into(),
                line: h["lines"]["start"].as_u64().map(|l| l as u32),
                windows: by_symbol,
                pointed: true,
            });
        }
        if let Some((p, l)) = self.symbol_site(view, target).await {
            return Ok(with(&p, Some(l)));
        }
        if let Ok(t) = self.find_thread(target, None).await {
            let a = &t["anchor"];
            let p = a["path"].as_str().unwrap_or("");
            return Ok(with(p, a["start"].as_u64().map(|l| l as u32)));
        }
        Err(format!(
            "unknown target {target:?}: use \"path:line\", a qualname (search finds it), a path, a WindowId or a ThreadId"
        ))
    }

    /// Record what the agent is looking at (agent.attention).
    async fn attend(&self, who: &AgentId, tool: &str, windows: Vec<String>, note: Option<String>) {
        let _ = ATTENDED.try_with(|a| a.set(true));
        let mut windows = windows;
        windows.dedup();
        windows.truncate(ATTENTION_WINDOWS);
        let mut ev = json!({
            "type": "agent.attention", "agent": who.name, "tool": tool,
            "windows": windows, "at": now_iso(),
        });
        if let Some(w) = &who.workspace {
            ev["workspace"] = json!(w);
        }
        if let Some(n) = note {
            ev["note"] = json!(n);
        }
        if let Err(e) = self.backend().attention(ev).await {
            eprintln!("cv-mcp: agent.attention not delivered: {e}");
        }
    }

    /// Headings (line, depth) of the windows of `path` overlapping [s, e], and those windows'
    /// line range.
    async fn headings(
        &self,
        view: &str,
        path: &str,
        s: u32,
        e: u32,
    ) -> Result<(Vec<(u32, u64)>, Option<(u32, u32)>), String> {
        let ws: Vec<Value> = self
            .path_windows(view, path)
            .await?
            .into_iter()
            .filter(|h| lines_overlap(h, s, e))
            .collect();
        if ws.is_empty() {
            return Ok((Vec::new(), None));
        }
        let range = (
            ws.iter()
                .filter_map(|h| h["lines"]["start"].as_u64())
                .min()
                .unwrap_or(1) as u32,
            ws.iter()
                .filter_map(|h| h["lines"]["end"].as_u64())
                .max()
                .unwrap_or(e as u64) as u32,
        );
        let ids: Vec<String> = ws.iter().map(id_of).collect();
        let mut hs: Vec<(u32, u64)> =
            match self.backend().window_batch(view, &ids, &["outline"]).await {
                Ok(b) => b["outlines"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .flat_map(|o| o["headings"].as_array().cloned().unwrap_or_default())
                    .filter_map(|h| Some((h["line"].as_u64()? as u32, h["depth"].as_u64()?)))
                    .collect(),
                Err(_) => Vec::new(),
            };
        hs.sort();
        hs.dedup();
        Ok((hs, Some(range)))
    }

    /// The symbol around [s, e] (innermost heading span), if one encloses it.
    async fn symbol_span(&self, view: &str, path: &str, s: u32, e: u32) -> Option<(u32, u32)> {
        let (hs, range) = self.headings(view, path, s, e).await.ok()?;
        enclosing(&hs, s, e, range?.1)
    }

    /// The lines shown with a thread: the enclosing symbol (else its windows), at most
    /// [`THREAD_CODE_LINES`] lines around the anchor.
    async fn code_span(
        &self,
        view: &str,
        path: &str,
        s: u32,
        e: u32,
    ) -> Result<(u32, u32), String> {
        let (hs, range) = self.headings(view, path, s, e).await?;
        let Some(range) = range else {
            return Ok((s.saturating_sub(5).max(1), e + 5));
        };
        let (mut a, mut b) = enclosing(&hs, s, e, range.1).unwrap_or(range);
        a = a.min(s);
        b = b.max(e);
        if b - a + 1 > THREAD_CODE_LINES {
            let room = THREAD_CODE_LINES.saturating_sub(e - s + 1);
            a = a.max(s.saturating_sub(room / 3));
            b = b.min(e.max(a + THREAD_CODE_LINES - 1));
        }
        Ok((a.max(1), b))
    }

    /// AnnotatedCode for lines [start, end] of `path` in the agent's view. `skip` leaves out one
    /// thread's line (the thread the code is shown with).
    async fn annotated(
        &self,
        view: &str,
        path: &str,
        start: u32,
        end: u32,
        annotate: &[Annotation],
        skip: Option<&str>,
    ) -> Result<(Value, Vec<String>), String> {
        let ws = self.path_windows(view, path).await?;
        let h0 = ws
            .first()
            .ok_or_else(|| format!("{path} is not in the view (unknown path or filtered out)"))?;
        let blob = h0["blob"].as_str().unwrap_or("").to_string();
        let file_lines = h0["lines"]["fileLines"].as_u64().unwrap_or(u64::MAX) as u32;
        let start = start.max(1).min(file_lines.max(1));
        let end = end.min(file_lines).max(start);
        let text = self
            .backend()
            .blob_text(&blob, Some(start), Some(end))
            .await
            .map_err(err)?;
        let from = text["from"].as_u64().unwrap_or(start as u64) as u32;
        let lines: Vec<String> = text["lines"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|l| l.as_str().unwrap_or("").to_string())
                    .collect()
            })
            .unwrap_or_default();
        let to = from + lines.len().saturating_sub(1) as u32;
        let shown: Vec<String> = ws
            .iter()
            .filter(|h| lines_overlap(h, from, to))
            .map(id_of)
            .collect();
        let mut notes = Vec::new();
        let want = |a: Annotation| annotate.contains(&a);
        if (want(Annotation::Ports) || want(Annotation::Diff)) && !shown.is_empty() {
            let mut parts = vec!["outline"];
            if want(Annotation::Diff) {
                parts.push("diff");
            }
            let b = self
                .backend()
                .window_batch(view, &shown, &parts)
                .await
                .map_err(err)?;
            if want(Annotation::Ports) {
                let links: HashMap<&str, &Value> = b["links"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|l| Some((l["id"].as_str()?, l)))
                    .collect();
                for o in b["outlines"].as_array().into_iter().flatten() {
                    for p in o["ports"].as_array().into_iter().flatten() {
                        let label = p["label"].as_str().unwrap_or("");
                        let ids = p["links"].as_array().cloned().unwrap_or_default();
                        let text = match ids.as_slice() {
                            // One destination: name where it is, so the agent can read it next.
                            [id] => {
                                let other = links.get(id.as_str().unwrap_or("")).map(|l| {
                                    if p["side"] == "in" {
                                        &l["from"]
                                    } else {
                                        &l["to"]
                                    }
                                });
                                match other
                                    .and_then(|o| Some((o["path"].as_str()?, o["line"].as_u64()?)))
                                {
                                    Some((op, ol)) if op == path => format!("{label} (:{ol})"),
                                    Some((op, ol)) => format!("{label} ({op}:{ol})"),
                                    None => label.to_string(),
                                }
                            }
                            [] => label.to_string(),
                            // "← 3 callers" already counts them.
                            many if label.contains(&many.len().to_string()) => label.to_string(),
                            many => format!("{label} ({} links)", many.len()),
                        };
                        notes.push(LineNote {
                            line: p["line"].as_u64().unwrap_or(0) as u32,
                            text,
                        });
                    }
                }
            }
            for d in b["diffs"].as_array().into_iter().flatten() {
                for l in d["lines"].as_array().into_iter().flatten() {
                    match l["kind"].as_str() {
                        Some("add") => notes.push(LineNote {
                            line: l["newLine"].as_u64().unwrap_or(0) as u32,
                            text: format!("+ added ({})", l["owner"].as_str().unwrap_or("?")),
                        }),
                        Some("del") => notes.push(LineNote {
                            line: l["oldLine"].as_u64().unwrap_or(0) as u32,
                            text: format!("- removed: {}", l["text"].as_str().unwrap_or("")),
                        }),
                        _ => {}
                    }
                }
            }
        }
        let threads: Vec<Value> = if want(Annotation::Threads) {
            let q = ThreadQuery {
                path: Some(path.to_string()),
                ..Default::default()
            };
            // Threads are an annotation: a server without the review routes still serves code.
            let threads = match self.backend().threads(&q).await {
                Err(BackendError::Unsupported(_)) => Vec::new(),
                r => r.map_err(err)?,
            };
            threads
                .into_iter()
                .filter(|t| {
                    let a = &t["anchor"];
                    a["path"] == path
                        && skip.is_none_or(|s| t["id"] != s)
                        && !matches!(t["status"].as_str(), Some("resolved") | Some("wontfix"))
                        && a["start"].as_u64().unwrap_or(0) as u32 <= to
                        && a["end"].as_u64().unwrap_or(0) as u32 >= from
                })
                .collect()
        } else {
            Vec::new()
        };
        let code = json!({
            "path": path, "change": h0["change"], "commit": h0["rev"], "blob": blob,
            "start": from, "end": to, "fileLines": file_lines, "windows": shown,
            "text": annotate::format(from, &lines, &notes, &threads),
        });
        Ok((code, shown))
    }

    fn args<P: DeserializeOwned>(args: &Value) -> Result<P, String> {
        serde_json::from_value(args.clone()).map_err(|e| format!("invalid arguments: {e}"))
    }

    /// Run one tool. `ctx` is None in direct (non-MCP) calls from tests.
    pub async fn call(
        &self,
        name: &str,
        args: Value,
        ctx: Option<&RequestContext<RoleServer>>,
    ) -> CallToolResult {
        if !self.profile.has(name) {
            return CallToolResult::error(vec![ContentBlock::text(format!(
                "tool {name} is not available in the {} profile",
                self.profile.name()
            ))]);
        }
        let who = self.who(ctx).await;
        let args = if args.is_null() { json!({}) } else { args };
        let (out, attended) = ATTENDED
            .scope(std::cell::Cell::new(false), async {
                let out = self.dispatch(name, &args, &who).await;
                (out, ATTENDED.with(|a| a.get()))
            })
            .await;
        if !attended {
            // Tools without windows of their own (writes, waits, failures): still show the call.
            let note = out.as_ref().err().map(|e| {
                let first = e.lines().next().unwrap_or("");
                format!("failed: {}", first.chars().take(120).collect::<String>())
            });
            self.attend(&who, name, Vec::new(), note).await;
        }
        match out {
            Ok(Out::Json(v)) => CallToolResult::success(vec![ContentBlock::text(compact(&v))]),
            Ok(Out::Code(mut v)) => {
                let text = v
                    .as_object_mut()
                    .and_then(|o| o.remove("text"))
                    .and_then(|t| t.as_str().map(String::from))
                    .unwrap_or_default();
                CallToolResult::success(vec![
                    ContentBlock::text(compact(&v)),
                    ContentBlock::text(text),
                ])
            }
            Ok(Out::Blocks(bs)) => {
                CallToolResult::success(bs.into_iter().map(ContentBlock::text).collect())
            }
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e)]),
        }
    }

    async fn dispatch(&self, name: &str, a: &Value, who: &AgentId) -> ToolOut {
        match name {
            "get_view_state" => {
                let out = self.get_view_state().await?;
                if let Out::Json(v) = &out {
                    let focus: Vec<String> =
                        [&v["state"]["focus"], &v["state"]["selection"]["window"]]
                            .into_iter()
                            .filter_map(|w| w.as_str().map(String::from))
                            .collect();
                    self.attend(who, "get_view_state", focus, None).await;
                }
                Ok(out)
            }
            "set_view" => {
                let p: SetView = Self::args(a)?;
                let r = self.backend().set_view(who, p.spec).await.map_err(err)?;
                Ok(Out::Json(r))
            }
            "focus" => self.focus(Self::args(a)?, who).await,
            "create_tour" => self.create_tour(Self::args(a)?, who).await,
            "search" => self.search(Self::args(a)?, who).await,
            "read_window" => self.read_window(Self::args(a)?, who).await,
            "read_code" => self.read_code(Self::args(a)?, who).await,
            "get_links" => self.get_links(Self::args(a)?, who).await,
            "get_hub" => self.get_hub(Self::args(a)?, who).await,
            "get_metrics" => self.get_metrics(Self::args(a)?, who).await,
            "get_hotspots" => self.get_hotspots(Self::args(a)?, who).await,
            "get_structure" => self.get_structure(Self::args(a)?, who).await,
            "list_workspaces" => self.list_workspaces(who).await,
            "bind_workspace" => self.bind_workspace(Self::args(a)?, who).await,
            "list_threads" => self.list_threads(Self::args(a)?, who).await,
            "get_thread" => self.get_thread(Self::args(a)?, who).await,
            "create_thread" => self.create_thread(Self::args(a)?, who).await,
            "reply" => {
                let p: Reply = Self::args(a)?;
                let status = p.set_status.map(|s| wire(&s));
                let t = match (&p.body, &status) {
                    (Some(b), _) => self
                        .backend()
                        .comment(who, &p.thread, b, status.as_deref())
                        .await
                        .map_err(err)?,
                    (None, Some(s)) => self
                        .backend()
                        .patch_thread(who, &p.thread, json!({ "status": s }))
                        .await
                        .map_err(err)?,
                    (None, None) => return Err("pass body, setStatus or both".into()),
                };
                Ok(Out::Json(thread_ack(&t)))
            }
            "set_thread_status" => {
                let p: SetThreadStatus = Self::args(a)?;
                let t = self
                    .backend()
                    .patch_thread(who, &p.thread, json!({ "status": wire(&p.status) }))
                    .await
                    .map_err(err)?;
                Ok(Out::Json(thread_ack(&t)))
            }
            "apply_suggestion" => self.apply_suggestion(Self::args(a)?, who).await,
            "list_revisions" => {
                let p: ListRevisions = Self::args(a)?;
                let revset = p.revset.or_else(|| who.workspace.as_ref().map(stack_of));
                let r = self
                    .backend()
                    .revisions(revset.as_deref(), p.limit)
                    .await
                    .map_err(err)?;
                let rows: Vec<Value> = r["rows"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|row| {
                        let mut rev = row["rev"].clone();
                        if !row["review"].is_null() {
                            rev["review"] = row["review"].clone();
                        }
                        rev
                    })
                    .collect();
                let note = format!(
                    "{} revisions of {}",
                    rows.len(),
                    revset.as_deref().unwrap_or("the log")
                );
                self.attend(who, "list_revisions", Vec::new(), Some(note))
                    .await;
                Ok(Out::Json(json!(rows)))
            }
            "get_diff" => self.get_diff(Self::args(a)?, who).await,
            "get_interdiff" => {
                let p: GetInterdiff = Self::args(a)?;
                let r = self
                    .backend()
                    .interdiff(&p.change, &p.from)
                    .await
                    .map_err(err)?;
                let files = r["files"].as_array().cloned().unwrap_or_default();
                let paths: Vec<String> = files
                    .iter()
                    .filter_map(|f| f["path"].as_str().map(String::from))
                    .collect();
                let windows = self.windows_of_paths(who, &paths).await;
                let note = format!("interdiff of {} from {}", p.change, p.from);
                self.attend(who, "get_interdiff", windows, Some(note)).await;
                Ok(Out::Blocks(render_diff(&files)))
            }
            "edit_code" => self.edit_code(Self::args(a)?, who, None, "edit_code").await,
            "propose_edit" => {
                self.edit_code(Self::args(a)?, who, Some(true), "propose_edit")
                    .await
            }
            "apply_edit" => {
                let p: ApplyEdit = Self::args(a)?;
                let r = self.backend().apply_edit(who, &p.edit).await.map_err(err)?;
                self.after_edit(who, "apply_edit", &r, &[]).await;
                // The answer edit_code {dryRun, reply} asked for (b13).
                let answer = match r["status"].as_str() {
                    Some("applied") => self.shared.answers.lock().unwrap().remove(&p.edit),
                    _ => None,
                };
                self.finish_edit(&r, answer, who).await
            }
            "undo_edit" => {
                let p: ApplyEdit = Self::args(a)?;
                let r = self.backend().undo_edit(who, &p.edit).await.map_err(err)?;
                self.invalidate();
                self.finish_edit(&r, None, who).await
            }
            "list_conflicts" => {
                let p: ListConflicts = Self::args(a)?;
                let revset = p.revset.or_else(|| who.workspace.as_ref().map(stack_of));
                let r = self
                    .backend()
                    .conflicts(revset.as_deref())
                    .await
                    .map_err(err)?;
                let mut paths: Vec<String> = r
                    .iter()
                    .filter_map(|c| c["path"].as_str().map(String::from))
                    .collect();
                paths.sort();
                paths.dedup();
                let windows = self.windows_of_paths(who, &paths).await;
                let note = format!("{} conflicted files", r.len());
                self.attend(who, "list_conflicts", windows, Some(note))
                    .await;
                // Short ids, as every other tool returns (get_conflict takes them back).
                let r: Vec<Value> = r
                    .iter()
                    .map(|c| {
                        let mut c = c.clone();
                        for (k, n) in [("change", 12), ("commit", 12), ("firstSeenOp", 12)] {
                            if !c[k].is_null() {
                                c[k] = short(&c[k], n);
                            }
                        }
                        c
                    })
                    .collect();
                Ok(Out::Json(json!(r)))
            }
            "get_conflict" => {
                let p: GetConflict = Self::args(a)?;
                let commit = self.conflict_commit(&p.rev, &p.path).await?;
                let d = self
                    .backend()
                    .conflict(&commit, &p.path)
                    .await
                    .map_err(err)?;
                let view = self.view_id(who).await?;
                let ws = self.path_windows(&view, &p.path).await.unwrap_or_default();
                self.attend(who, "get_conflict", ws.iter().map(id_of).collect(), None)
                    .await;
                Ok(Out::Json(d))
            }
            "resolve_conflict" => {
                let p: ResolveConflict = Self::args(a)?;
                let commit = self.conflict_commit(&p.rev, &p.path).await?;
                let r = self
                    .backend()
                    .resolve_conflict(who, &commit, &p.path, &p.content)
                    .await
                    .map_err(err)?;
                self.after_edit(who, "resolve_conflict", &r, &[p.path])
                    .await;
                self.finish_edit(&r, None, who).await
            }
            "declare_link" => {
                let p: DeclareLink = Self::args(a)?;
                let body = json!({ "from": p.from, "to": p.to, "kind": p.kind, "label": p.label, "note": p.note });
                let l = self.backend().declare_link(who, body).await.map_err(err)?;
                Ok(Out::Json(slim_link(&l)))
            }
            "rate_link" => {
                let p: RateLink = Self::args(a)?;
                let body = json!({ "verdict": wire(&p.verdict), "note": p.note });
                let l = self
                    .backend()
                    .rate_link(who, &p.link, body)
                    .await
                    .map_err(err)?;
                Ok(Out::Json(slim_link(&l)))
            }
            "label_cluster" => {
                let p: LabelCluster = Self::args(a)?;
                let body = json!({ "label": p.label, "summary": p.summary });
                Ok(Out::Json(
                    self.backend()
                        .label_cluster(who, &p.cluster, body)
                        .await
                        .map_err(err)?,
                ))
            }
            "wait_for_events" => self.wait_for_events(Self::args(a)?).await,
            other => Err(format!("unknown tool {other}")),
        }
    }

    // ── Orientation ──

    async fn get_view_state(&self) -> ToolOut {
        match self.backend().view_state().await {
            Ok(v) => Ok(Out::Json(v)),
            Err(BackendError::Unsupported(_)) => Ok(Out::Json(json!({ "view": null }))),
            Err(e) => Err(err(e)),
        }
    }

    async fn site(&self, view: &str, target: &str) -> Result<(Value, Vec<String>), String> {
        let t = self.resolve(view, target).await?;
        let line = t
            .line
            .or_else(|| {
                t.windows.first()?["lines"]["start"]
                    .as_u64()
                    .map(|l| l as u32)
            })
            .unwrap_or(1);
        let mut site = json!({ "path": t.path, "line": line });
        if let Some(h) = t.windows.first() {
            site["window"] = h["id"].clone();
            if let Some(q) = h["symbol"]["qualname"].as_str() {
                site["symbol"] = json!(q);
            }
        }
        Ok((site, t.windows.iter().map(id_of).collect()))
    }

    async fn focus(&self, p: Focus, who: &AgentId) -> ToolOut {
        let view = self.view_id(who).await?;
        let (site, windows) = self.site(&view, &p.target).await?;
        self.attend(who, "focus", windows, p.note.clone()).await;
        let r = self
            .backend()
            .focus(
                who,
                json!({ "target": site, "lines": p.lines, "note": p.note }),
            )
            .await
            .map_err(err)?;
        Ok(Out::Json(r))
    }

    async fn create_tour(&self, p: CreateTour, who: &AgentId) -> ToolOut {
        let view = self.view_id(who).await?;
        let mut steps = Vec::new();
        for s in p.steps {
            let (site, _) = self.site(&view, &s.target).await?;
            steps.push(json!({ "target": site, "lines": s.lines, "note": s.note }));
        }
        let r = self
            .backend()
            .create_tour(who, json!({ "title": p.title, "steps": steps }))
            .await
            .map_err(err)?;
        Ok(Out::Json(r))
    }

    // ── Reading ──

    async fn search(&self, p: Search, who: &AgentId) -> ToolOut {
        let view = self.view_id(who).await.ok();
        let kinds = p
            .kinds
            .as_ref()
            .map(|k| k.iter().map(wire).collect::<Vec<_>>());
        let mut hits = self
            .backend()
            .search(
                &p.query,
                kinds.as_ref().map(|k| k.join(",")).as_deref(),
                view.as_deref(),
                p.limit.unwrap_or(10),
            )
            .await
            .map_err(err)?;
        if let Some(k) = &kinds {
            hits.retain(|h| k.iter().any(|k| h["kind"] == k.as_str()));
        }
        let windows: Vec<String> = hits
            .iter()
            .filter_map(|h| h["target"]["window"].as_str().map(String::from))
            .take(10)
            .collect();
        self.attend(
            who,
            "search",
            windows,
            Some(format!("search {:?}", p.query)),
        )
        .await;
        // { kind, label, detail ("path:line" for code), window } and the other target fields.
        let slim: Vec<Value> = hits
            .iter()
            .map(|h| {
                let mut o = json!({ "kind": h["kind"], "label": h["label"] });
                if !h["detail"].is_null() {
                    o["detail"] = h["detail"].clone();
                }
                for (k, v) in h["target"].as_object().into_iter().flatten() {
                    if k != "site" {
                        o[k] = v.clone();
                    }
                }
                if o["window"].is_null() && !h["target"]["site"]["window"].is_null() {
                    o["window"] = h["target"]["site"]["window"].clone();
                }
                o
            })
            .collect();
        Ok(Out::Json(json!(slim)))
    }

    async fn read_window(&self, p: ReadWindow, who: &AgentId) -> ToolOut {
        let view = self.view_id(who).await?;
        let all = self.windows(&view).await?;
        let h = all
            .iter()
            .find(|h| h["id"] == p.window.as_str())
            .ok_or_else(|| format!("unknown window {} in view {view}", p.window))?
            .clone();
        let annotate = p
            .annotate
            .unwrap_or_else(|| vec![Annotation::Ports, Annotation::Threads]);
        let (s, e) = (
            h["lines"]["start"].as_u64().unwrap_or(1) as u32,
            h["lines"]["end"].as_u64().unwrap_or(1) as u32,
        );
        let path = h["path"].as_str().unwrap_or("").to_string();
        let (mut code, _) = self.annotated(&view, &path, s, e, &annotate, None).await?;
        self.remember(who, &path, &code["blob"]);
        code["window"] = json!(p.window);
        if let Some(sym) = h.get("symbol") {
            code["symbol"] = sym.clone();
        }
        self.attend(who, "read_window", vec![p.window], None).await;
        Ok(Out::Code(code))
    }

    async fn read_code(&self, p: ReadCode, who: &AgentId) -> ToolOut {
        let view = match &p.rev {
            Some(rev) => {
                let v = self
                    .create_view(json!({ "mode": "snapshot", "rev": rev }))
                    .await
                    .map_err(err)?;
                v["id"].as_str().unwrap_or_default().to_string()
            }
            None => self.view_id(who).await?,
        };
        let start = p.start.unwrap_or(1).max(1);
        let end = p.end.unwrap_or(start + 199).max(start);
        let annotate = p
            .annotate
            .unwrap_or_else(|| vec![Annotation::Ports, Annotation::Threads]);
        let (code, windows) = self
            .annotated(&view, &p.path, start, end, &annotate, None)
            .await?;
        self.remember(who, &p.path, &code["blob"]);
        self.attend(who, "read_code", windows, None).await;
        Ok(Out::Code(code))
    }

    async fn get_links(&self, p: GetLinks, who: &AgentId) -> ToolOut {
        let view = self.view_id(who).await?;
        let dir = match p.dir.unwrap_or(Dir::Both) {
            Dir::In => "in",
            Dir::Out => "out",
            Dir::Both => "both",
        };
        let kinds = p.kinds.as_ref().map(|k| k.join(","));
        let t = match self.resolve(&view, &p.target).await {
            Ok(t) => Some(t),
            // Not code (e.g. a HubId): the server resolves it or says it does not know it.
            Err(e) => {
                if !p.target.contains(':') && !p.target.starts_with('w') {
                    return Err(e);
                }
                None
            }
        };
        let ids: Vec<String> = t
            .as_ref()
            .map(|t| t.windows.iter().map(id_of).collect())
            .unwrap_or_default();
        // The span the links must start or end in: the symbol at the line, else the line.
        let span = match &t {
            Some(t) if t.pointed => {
                let l = t.line.unwrap_or(1);
                Some(
                    self.symbol_span(&view, &t.path, l, l)
                        .await
                        .unwrap_or((l, l)),
                )
            }
            _ => None,
        };
        self.attend(who, "get_links", ids.clone(), None).await;
        // The server knows window ids and "path:line"; a qualname went through search.
        let server_target = match (&t, ids.first()) {
            (Some(t), _) if t.pointed => format!("{}:{}", t.path, t.line.unwrap_or(1)),
            (Some(_), Some(w)) if ids.len() == 1 => w.clone(),
            _ => p.target.clone(),
        };
        let (links, hubs) = match self
            .backend()
            .links(
                &view,
                &server_target,
                dir,
                kinds.as_deref(),
                p.depth.unwrap_or(1),
            )
            .await
        {
            Ok(r) => (
                r["links"].as_array().cloned().unwrap_or_default(),
                r["hubs"].as_array().cloned().unwrap_or_default(),
            ),
            Err(BackendError::Unsupported(_)) => {
                // Fallback: the ports of the target's windows (depth 1).
                let b = self
                    .backend()
                    .window_batch(&view, &ids, &["outline"])
                    .await
                    .map_err(err)?;
                let links = b["links"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|l| {
                        let from_here = ids.iter().any(|i| l["from"]["window"] == i.as_str());
                        let to_here = ids.iter().any(|i| l["to"]["window"] == i.as_str());
                        let dir_ok = match dir {
                            "out" => from_here,
                            "in" => to_here,
                            _ => from_here || to_here,
                        };
                        let kind_ok = p
                            .kinds
                            .as_ref()
                            .is_none_or(|k| k.iter().any(|k| l["kind"] == k.as_str()));
                        dir_ok && kind_ok
                    })
                    .cloned()
                    .collect();
                (links, Vec::new())
            }
            Err(e) => return Err(err(e)),
        };
        let links = match (&t, span) {
            (Some(t), Some(span)) => narrow_links(links, &t.path, span, &ids, dir),
            _ => links,
        };
        let total = links.len();
        let limit = p.limit.unwrap_or(30).max(1);
        let kept: Vec<Value> = links.iter().take(limit).map(slim_link).collect();
        let hub_ids: HashSet<String> = links
            .iter()
            .take(limit)
            .flat_map(|l| [l["from"]["hub"].as_str(), l["to"]["hub"].as_str()])
            .flatten()
            .map(String::from)
            .collect();
        let hubs: Vec<Value> = hubs
            .iter()
            .filter(|h| h["id"].as_str().is_some_and(|i| hub_ids.contains(i)))
            .map(|h| json!({ "id": h["id"], "protocol": h["protocol"], "key": h["key"] }))
            .collect();
        let mut out = json!({ "links": kept });
        if !hubs.is_empty() {
            out["hubs"] = json!(hubs);
        }
        if let Some((a, b)) = span {
            out["span"] = json!(format!(
                "{}:{a}-{b}",
                t.as_ref().map(|t| t.path.as_str()).unwrap_or("")
            ));
        }
        if total > limit {
            out["more"] = json!(total - limit);
        }
        Ok(Out::Json(out))
    }

    async fn get_hub(&self, p: GetHub, who: &AgentId) -> ToolOut {
        let view = self.view_id(who).await?;
        let hub = match self.backend().hub(&view, &p.key).await {
            Ok(h) => Some(h),
            Err(BackendError::Unsupported(_)) => None,
            Err(BackendError::Api { status: 404, .. }) => None,
            Err(e) => return Err(err(e)),
        };
        let hub = match hub {
            Some(h) => h,
            None => self
                .backend()
                .view_hubs(&view)
                .await
                .map_err(err)?
                .into_iter()
                .find(|h| {
                    (h["id"] == p.key.as_str() || h["key"] == p.key.as_str())
                        && p.protocol
                            .as_ref()
                            .is_none_or(|pr| h["protocol"] == pr.as_str())
                })
                .ok_or_else(|| format!("unknown hub {}", p.key))?,
        };
        let mut h = hub;
        if let Some(o) = h.as_object_mut() {
            o.remove("rect");
            o.remove("anchors");
        }
        let windows: Vec<String> = ["producers", "consumers"]
            .iter()
            .flat_map(|k| h[*k].as_array().into_iter().flatten())
            .filter_map(|s| s["window"].as_str().map(String::from))
            .collect();
        let note = format!(
            "hub {}",
            h["display"]
                .as_str()
                .or(h["key"].as_str())
                .unwrap_or(&p.key)
        );
        self.attend(who, "get_hub", windows, Some(note)).await;
        Ok(Out::Json(h))
    }

    async fn get_metrics(&self, p: GetMetrics, who: &AgentId) -> ToolOut {
        let view = self.view_id(who).await?;
        let t = self.resolve(&view, &p.target).await?;
        let h = t
            .windows
            .iter()
            .max_by(|a, b| {
                a["risk"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .total_cmp(&b["risk"].as_f64().unwrap_or(0.0))
            })
            .ok_or_else(|| format!("no window for {}", p.target))?;
        let m = self
            .backend()
            .metrics(&view, "file", None, 2000)
            .await
            .map_err(err)?;
        let co: Vec<Value> = m["cochange"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| {
                let other = if c["a"] == t.path.as_str() {
                    &c["b"]
                } else if c["b"] == t.path.as_str() {
                    &c["a"]
                } else {
                    return None;
                };
                Some(json!({ "path": other, "jaccard": c["jaccard"] }))
            })
            .take(10)
            .collect();
        // Structure (snapshots): the window, its file and its directory (absent in range views).
        let dir = t.path.rsplit_once('/').map_or("", |(d, _)| d).to_string();
        let structure = self
            .backend()
            .structure(&view, Some(&dir))
            .await
            .ok()
            .map(|s| {
                let find = |list: &str, k: &str, v: &Value| {
                    s[list]
                        .as_array()
                        .and_then(|a| a.iter().find(|x| x[k] == *v).cloned())
                };
                let pick = |x: Option<Value>, keys: &[&str]| {
                    x.map(|x| {
                        Value::Object(
                            keys.iter()
                                .map(|k| (k.to_string(), x[*k].clone()))
                                .collect(),
                        )
                    })
                };
                let mut out = serde_json::Map::new();
                for (name, v) in [
                    (
                        "window",
                        pick(
                            find("windows", "key", &h["key"]),
                            &["internal", "external", "incoming"],
                        ),
                    ),
                    (
                        "file",
                        pick(
                            find("files", "path", &json!(t.path)),
                            &["internal", "external", "exports", "tension", "pull"],
                        ),
                    ),
                    (
                        "folder",
                        pick(
                            find("folders", "path", &json!(dir)),
                            &[
                                "path",
                                "files",
                                "pressure",
                                "inside",
                                "gateways",
                                "spread",
                                "interfaceShare",
                                "chain",
                                "tension",
                            ],
                        ),
                    ),
                ] {
                    if let Some(v) = v {
                        out.insert(name.into(), v);
                    }
                }
                Value::Object(out)
            })
            .filter(|s| s.as_object().is_some_and(|o| !o.is_empty()));
        self.attend(who, "get_metrics", vec![id_of(h)], None).await;
        // No timeline: no route serves per-window months yet (mcp.ts marks it optional).
        let mut out = json!({
            "window": h["id"], "path": t.path, "heat": h["heat"], "conn": h["conn"], "risk": h["risk"],
            "reviewFirst": h["reviewFirst"], "coChanged": co,
        });
        if let Some(s) = structure {
            out["structure"] = s;
        }
        Ok(Out::Json(round_floats(&out)))
    }

    async fn get_hotspots(&self, p: GetHotspots, who: &AgentId) -> ToolOut {
        let view = self.view_id(who).await?;
        let m = self
            .backend()
            .metrics(&view, "file", p.lane.as_deref(), 2000)
            .await
            .map_err(err)?;
        let mut pts: Vec<Value> = m["points"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|pt| p.lane.as_ref().is_none_or(|l| pt["lane"] == l.as_str()))
            .cloned()
            .collect();
        pts.sort_by(|a, b| {
            b["risk"]
                .as_f64()
                .unwrap_or(0.0)
                .total_cmp(&a["risk"].as_f64().unwrap_or(0.0))
        });
        pts.truncate(p.limit.unwrap_or(10));
        // Compact rows: what to read (label, window) and why (risk and both percentiles).
        let rows: Vec<Value> = pts
            .iter()
            .map(|pt| {
                round_floats(&json!({
                    "label": pt["label"], "window": pt["window"], "lane": pt["lane"],
                    "risk": pt["risk"], "heatPct": pt["heatPct"], "connPct": pt["connPct"], "outside": pt["outside"],
                    "touches": pt["heat"]["touches"], "fanIn": pt["conn"]["fanIn"],
                }))
            })
            .collect();
        let windows: Vec<String> = rows
            .iter()
            .filter_map(|r| r["window"].as_str().map(String::from))
            .collect();
        self.attend(
            who,
            "get_hotspots",
            windows,
            p.lane.clone().map(|l| format!("lane {l}")),
        )
        .await;
        Ok(Out::Json(json!(rows)))
    }

    async fn get_structure(&self, p: GetStructure, who: &AgentId) -> ToolOut {
        let view = self.view_id(who).await?;
        let s = self
            .backend()
            .structure(&view, p.folder.as_deref())
            .await
            .map_err(err)?;
        let limit = p.limit.unwrap_or(10);
        let num = |v: &Value, k: &str| v[k].as_f64().unwrap_or(0.0);
        // The `limit` rows scoring highest, leaving out those scoring 0.
        let top = |rows: &[&Value], score: &dyn Fn(&Value) -> f64| -> Vec<Value> {
            let mut rows: Vec<(f64, &Value)> = rows
                .iter()
                .map(|r| (score(r), *r))
                .filter(|(k, _)| *k > 0.0)
                .collect();
            rows.sort_by(|a, b| b.0.total_cmp(&a.0));
            rows.into_iter()
                .take(limit)
                .map(|(_, r)| r.clone())
                .collect()
        };
        let folders: Vec<&Value> = s["folders"].as_array().into_iter().flatten().collect();
        let files: Vec<&Value> = s["files"].as_array().into_iter().flatten().collect();
        // Leaky: high pressure over many boundary links (a sealed or tiny folder scores 0).
        let leaky: Vec<Value> = top(&folders, &|f| {
            let boundary = num(f, "outgoing") + num(f, "incoming");
            if boundary < 3.0 {
                0.0
            } else {
                num(f, "pressure") * boundary.ln_1p()
            }
        })
        .iter()
        .map(|f| {
            json!({
                "path": f["path"], "files": f["files"], "pressure": f["pressure"], "inside": f["inside"],
                "internal": f["internal"], "out": f["outgoing"], "in": f["incoming"],
                "interface": f["interface"], "interfaceShare": f["interfaceShare"],
                "partners": f["partners"], "gateways": f["gateways"], "spread": f["spread"],
                "chain": f["chain"], "modules": f["modules"],
            })
        })
        .collect();
        let tense: Vec<Value> = top(&folders, &|f| num(f, "tension") * num(f, "mass"))
            .iter()
            .map(|f| json!({ "path": f["path"], "tension": f["tension"], "hotFiles": f["hotFiles"] }))
            .collect();
        let split_out: Vec<Value> = top(&files, &|f| num(f, "tension") * num(f, "rate"))
            .iter()
            .map(|f| {
                json!({ "path": f["path"], "folder": f["folder"], "heat": f["heat"], "tension": f["tension"] })
            })
            .collect();
        let moves: Vec<Value> = top(&files, &|f| num(&f["pull"], "links"))
            .iter()
            .map(|f| {
                json!({ "path": f["path"], "from": f["folder"], "to": f["pull"]["folder"],
                        "links": f["pull"]["links"], "internal": f["internal"] })
            })
            .collect();
        let windows: Vec<&Value> = s["windows"].as_array().into_iter().flatten().collect();
        let gateways: Vec<Value> = top(&windows, &|w| num(w, "external"))
            .iter()
            .map(|w| {
                json!({ "key": w["key"], "external": w["external"], "incoming": w["incoming"], "internal": w["internal"] })
            })
            .collect();
        self.attend(who, "get_structure", vec![], p.folder.clone())
            .await;
        Ok(Out::Json(round_floats(&json!({
            "summary": s["summary"], "leaky": leaky, "tense": tense, "splitOut": split_out, "move": moves,
            "gateways": gateways,
        }))))
    }

    // ── Workspaces ──

    async fn list_workspaces(&self, who: &AgentId) -> ToolOut {
        let mut ws = self.backend().workspaces().await.map_err(err)?;
        let bindings = self.shared.bindings.lock().unwrap().clone();
        for w in ws.iter_mut() {
            let name = w["name"].as_str().unwrap_or("").to_string();
            for (agent, bound) in &bindings {
                if *bound == name && w["agent"]["name"] != agent.as_str() {
                    let client = if *agent == who.name {
                        who.client.clone()
                    } else {
                        None
                    };
                    w["agent"] = json!({ "name": agent, "client": client, "activity": "idle", "lastSeen": now_iso() });
                }
            }
            if who.workspace.as_deref() == Some(name.as_str()) {
                w["boundToThisSession"] = json!(true);
            }
            *w = slim_workspace(w);
        }
        let note = format!("{} workspaces", ws.len());
        self.attend(who, "list_workspaces", Vec::new(), Some(note))
            .await;
        Ok(Out::Json(json!(ws)))
    }

    async fn bind_workspace(&self, p: BindWorkspace, who: &AgentId) -> ToolOut {
        let all = self.backend().workspaces().await.map_err(err)?;
        let Some(mut summary) = all
            .iter()
            .find(|w| w["name"] == p.workspace.as_str())
            .cloned()
        else {
            let names: Vec<&str> = all.iter().filter_map(|w| w["name"].as_str()).collect();
            return Err(format!(
                "unknown workspace {:?}; workspaces: {}",
                p.workspace,
                names.join(", ")
            ));
        };
        let mut bound = who.clone();
        bound.workspace = Some(p.workspace.clone());
        match self.backend().bind_workspace(&bound, &p.workspace).await {
            Ok(s) if s.is_object() => summary = s,
            Ok(_) | Err(BackendError::Unsupported(_)) => {}
            Err(e) => return Err(err(e)),
        }
        self.shared
            .bindings
            .lock()
            .unwrap()
            .insert(who.name.clone(), p.workspace.clone());
        self.invalidate();
        self.attend(
            &bound,
            "bind_workspace",
            Vec::new(),
            Some(format!("bound to {}", p.workspace)),
        )
        .await;
        let stack = summary["stack"]["revset"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| stack_of(&p.workspace));
        Ok(Out::Json(json!({ "bound": p.workspace, "stack": stack })))
    }

    // ── Review ──

    async fn threads_matching(&self, p: &ListThreads) -> Result<Vec<Value>, String> {
        let q = ThreadQuery {
            status: p.status.iter().flatten().map(wire).collect(),
            assignee: p.assignee.map(|a| wire(&a)),
            revset: p.revset.clone(),
            path: p.path.clone(),
        };
        let threads = self.backend().threads(&q).await.map_err(err)?;
        // Servers may ignore filters (cv-mock filters by path only): apply them here too.
        Ok(threads
            .into_iter()
            .filter(|t| q.status.is_empty() || q.status.iter().any(|s| t["status"] == s.as_str()))
            .filter(|t| {
                q.assignee
                    .as_ref()
                    .is_none_or(|a| t["assignee"] == a.as_str())
            })
            .filter(|t| {
                q.path
                    .as_ref()
                    .is_none_or(|p| t["anchor"]["path"] == p.as_str())
            })
            .collect())
    }

    async fn list_threads(&self, p: ListThreads, who: &AgentId) -> ToolOut {
        let threads = self.threads_matching(&p).await?;
        if !p.with_code.unwrap_or(false) || threads.is_empty() {
            return Ok(Out::Json(json!(
                threads.iter().map(annotate::summary).collect::<Vec<_>>()
            )));
        }
        let view = self.view_id(who).await?;
        let mut blocks = Vec::new();
        let mut windows = Vec::new();
        for t in &threads {
            match self.thread_code(&view, t, None, who).await {
                Ok((meta, text, ws)) => {
                    blocks.push(compact(&meta));
                    blocks.push(text);
                    windows.extend(ws);
                }
                Err(e) => {
                    let mut meta = thread_meta(t);
                    meta["code"] = json!(format!("unavailable: {e}"));
                    blocks.push(compact(&meta));
                }
            }
        }
        self.attend(
            who,
            "list_threads",
            windows,
            Some(format!("reading {} threads", threads.len())),
        )
        .await;
        Ok(Out::Blocks(blocks))
    }

    /// A thread for the agent: (metadata with comments, numbered code around the anchor, windows).
    async fn thread_code(
        &self,
        view: &str,
        t: &Value,
        context: Option<u32>,
        who: &AgentId,
    ) -> Result<(Value, String, Vec<String>), String> {
        let a = &t["anchor"];
        let path = a["path"].as_str().unwrap_or("").to_string();
        let (s, e) = (
            a["start"].as_u64().unwrap_or(1) as u32,
            a["end"].as_u64().unwrap_or(1) as u32,
        );
        let (from, to) = match context {
            Some(c) => (s.saturating_sub(c).max(1), e + c),
            None => self.code_span(view, &path, s, e).await?,
        };
        let (code, windows) = self
            .annotated(
                view,
                &path,
                from,
                to,
                &[Annotation::Ports, Annotation::Threads],
                t["id"].as_str(),
            )
            .await?;
        self.remember(who, &path, &code["blob"]);
        let mut meta = thread_meta(t);
        meta["blob"] = code["blob"].clone();
        meta["lines"] = json!([code["start"], code["end"]]);
        if let Some(patch) = annotate::suggestion_patch(t) {
            meta["suggestion"] = patch;
        }
        let text = code["text"].as_str().unwrap_or("").to_string();
        Ok((meta, text, windows))
    }

    async fn get_thread(&self, p: GetThread, who: &AgentId) -> ToolOut {
        let t = self.find_thread(&p.thread, None).await?;
        let view = self.view_id(who).await?;
        let (meta, text, _) = self.thread_code(&view, &t, p.context_lines, who).await?;
        let a = &t["anchor"];
        let (s, e) = (
            a["start"].as_u64().unwrap_or(1) as u32,
            a["end"].as_u64().unwrap_or(1) as u32,
        );
        let path = a["path"].as_str().unwrap_or("");
        let anchored: Vec<String> = self
            .path_windows(&view, path)
            .await?
            .iter()
            .filter(|h| lines_overlap(h, s, e))
            .map(id_of)
            .collect();
        self.attend(
            who,
            "get_thread",
            anchored,
            Some(format!("reading thread {}", p.thread)),
        )
        .await;
        Ok(Out::Blocks(vec![compact(&meta), text]))
    }

    async fn create_thread(&self, p: CreateThread, who: &AgentId) -> ToolOut {
        let view = self.view_id(who).await?;
        let ws = self.path_windows(&view, &p.anchor.path).await?;
        let rev = match p.anchor.rev {
            Some(r) => r,
            None => ws
                .first()
                .and_then(|h| h["change"].as_str())
                .map(String::from)
                .ok_or_else(|| format!("{} is not in the view; pass anchor.rev", p.anchor.path))?,
        };
        let mut body = json!({
            "anchor": { "path": p.anchor.path, "rev": rev, "start": p.anchor.start, "end": p.anchor.end,
                         "side": wire(&p.anchor.side.unwrap_or(Side::New)) },
            "body": p.body,
            "kind": wire(&p.kind.unwrap_or(ThreadKind::Finding)),
        });
        if let Some(s) = p.severity {
            body["severity"] = json!(wire(&s));
        }
        let t = self.backend().create_thread(who, body).await.map_err(err)?;
        let windows = ws
            .iter()
            .filter(|h| lines_overlap(h, p.anchor.start, p.anchor.end))
            .map(id_of)
            .collect();
        self.attend(who, "create_thread", windows, None).await;
        let mut out = thread_ack(&t);
        out["kind"] = t["kind"].clone();
        out["createdBy"] = t["createdBy"]["kind"].clone();
        Ok(Out::Json(out))
    }

    async fn base_blob(&self, who: &AgentId, path: &str) -> Result<Option<String>, String> {
        let view = self.view_id(who).await?;
        Ok(self
            .path_windows(&view, path)
            .await?
            .first()
            .and_then(|h| h["blob"].as_str())
            .map(String::from))
    }

    async fn apply_suggestion(&self, p: ApplySuggestion, who: &AgentId) -> ToolOut {
        let t = self.find_thread(&p.thread, None).await?;
        let patch = annotate::suggestion_patch(&t).ok_or_else(|| {
            format!(
                "thread {} has no ```suggestion block; use edit_code {{thread, patches}} instead",
                p.thread
            )
        })?;
        let a = &t["anchor"];
        let path = a["path"].as_str().unwrap_or("").to_string();
        let mode = wire(&p.mode.unwrap_or(SuggestionMode::Amend));
        let mut file = json!({ "path": path, "patches": [patch] });
        if let Some(b) = self.base_blob(who, &path).await? {
            file["baseBlob"] = json!(b);
        }
        let dry = p.dry_run.unwrap_or(false);
        let req = json!({
            "target": a["change"], "files": [file], "mode": mode,
            "dryRun": dry, "origin": { "thread": p.thread },
        });
        let r = self.backend().edit(who, req).await.map_err(err)?;
        self.after_edit(who, "apply_suggestion", &r, &[path]).await;
        let answer = (p.thread.clone(), p.reply, p.set_status.map(|s| wire(&s)));
        self.finish_edit(&r, Some(answer), who).await
    }

    async fn edit_code(
        &self,
        p: EditCode,
        who: &AgentId,
        force_dry: Option<bool>,
        tool: &str,
    ) -> ToolOut {
        let thread = match &p.thread {
            Some(id) => Some(self.find_thread(id, None).await?),
            None => None,
        };
        let anchor = thread.as_ref().map(|t| &t["anchor"]);
        let path = p
            .path
            .clone()
            .or_else(|| anchor.and_then(|a| a["path"].as_str().map(String::from)))
            .ok_or("pass path (or thread)")?;
        // Without a thread the default is the agent's working copy (`<workspace>@`).
        let rev = p
            .rev
            .clone()
            .or_else(|| anchor.and_then(|a| a["change"].as_str().map(String::from)))
            .or_else(|| who.workspace.as_ref().map(|w| format!("{w}@")))
            .ok_or("pass rev (or thread)")?;
        // The code the agent last saw for this path; a later change to it is a stale base.
        let base = match p.base_blob.clone() {
            Some(b) => Some(b),
            None => match self.shown_blob(who, &path) {
                Some(b) => Some(b),
                None if thread.is_some() => self.base_blob(who, &path).await?,
                // A new file has no base to be stale against (b1: e.g. extracting a module).
                None if self.file_absent(who, &rev, &path).await => None,
                None => {
                    return Err(format!(
                        "{path} exists at {rev}: pass baseBlob (the blob of the code you read, from read_code) or thread"
                    ));
                }
            },
        };
        let mut file = json!({ "path": path });
        if let Some(b) = base {
            file["baseBlob"] = json!(b);
        }
        match (&p.patches, &p.content) {
            (Some(ps), None) => {
                file["patches"] = json!(
                    ps.iter()
                        .map(|x| json!({ "start": x.start, "end": x.end, "text": x.text }))
                        .collect::<Vec<_>>()
                )
            }
            (None, Some(c)) => file["content"] = json!(c),
            _ => return Err("pass exactly one of patches or content".into()),
        }
        let dry = force_dry.or(p.dry_run).unwrap_or(false);
        let mut req = json!({
            "target": rev, "files": [file],
            "mode": p.mode.map(|m| wire(&m)).unwrap_or_else(|| "amend".into()),
            "dryRun": dry,
        });
        if let Some(m) = &p.message {
            req["message"] = json!(m);
        }
        if let Some(t) = &p.thread {
            req["origin"] = json!({ "thread": t });
        }
        let mut r = self.backend().edit(who, req.clone()).await.map_err(err)?;
        // mcp.ts: the default mode is amend for mutable targets and branch for immutable ones.
        if p.mode.is_none() && r["status"] == "rejected" && r["error"]["code"] == "immutable" {
            req["mode"] = json!("branch");
            r = self.backend().edit(who, req).await.map_err(err)?;
        }
        self.after_edit(who, tool, &r, &[path]).await;
        let answer = p
            .thread
            .clone()
            .map(|t| (t, p.reply, p.set_status.map(|s| wire(&s))));
        self.finish_edit(&r, answer, who).await
    }

    /// Whether `path` is absent at `rev` (a new file). Asks the backend for the file in rev's
    /// tree; a backend that cannot diff two trees falls back to the agent's view having no window
    /// for the path.
    async fn file_absent(&self, who: &AgentId, rev: &str, path: &str) -> bool {
        let quoted = path.replace('\\', "\\\\").replace('"', "\\\"");
        let fileset = format!("root-file:\"{quoted}\"");
        match self
            .backend()
            .tree_diff("root()", rev, Some(&fileset))
            .await
        {
            Ok(d) => d["files"].as_array().is_none_or(|f| f.is_empty()),
            Err(BackendError::Unsupported(_)) => match self.view_id(who).await {
                Ok(view) => self
                    .path_windows(&view, path)
                    .await
                    .is_ok_and(|w| w.is_empty()),
                Err(_) => false,
            },
            Err(_) => false,
        }
    }

    /// The tool result of an edit: an error for rejected/failed edits, else the slim result plus
    /// the thread answer (reply and/or status) when one was asked for and the edit applied.
    async fn finish_edit(&self, r: &Value, answer: Option<Answer>, who: &AgentId) -> ToolOut {
        if let Some(e) = edit_error(r) {
            return Err(e);
        }
        let mut out = slim_edit(r);
        let answer = answer.filter(|(_, reply, status)| reply.is_some() || status.is_some());
        // A preview keeps its answer for apply_edit (b13).
        if r["status"] == "previewed"
            && let (Some(a), Some(id)) = (&answer, r["id"].as_str())
        {
            self.shared
                .answers
                .lock()
                .unwrap()
                .insert(id.to_string(), a.clone());
            out["note"] = json!(format!(
                "apply_edit {{edit: {id}}} applies it and then answers thread {}",
                a.0
            ));
        }
        if r["status"] == "applied"
            && let Some((thread, reply, status)) = answer
        {
            let status = status.or_else(|| reply.is_some().then(|| "addressed".to_string()));
            let t = match &reply {
                Some(body) => {
                    self.backend()
                        .comment(who, &thread, body, status.as_deref())
                        .await
                }
                None => {
                    self.backend()
                        .patch_thread(who, &thread, json!({ "status": status }))
                        .await
                }
            };
            match t {
                Ok(t) => out["thread"] = thread_ack(&t),
                Err(e) => {
                    out["threadError"] = json!(format!("the edit applied, the reply did not: {e}"))
                }
            }
        }
        Ok(Out::Json(out))
    }

    /// After a write: drop cached views when it landed, and show where the agent wrote.
    async fn after_edit(&self, who: &AgentId, tool: &str, r: &Value, paths: &[String]) {
        let mut windows = Vec::new();
        if r["status"] == "applied" {
            self.invalidate();
        }
        if let Ok(view) = self.view_id(who).await {
            for p in paths {
                if let Ok(ws) = self.path_windows(&view, p).await {
                    windows.extend(ws.iter().map(id_of));
                }
            }
        }
        let note = format!(
            "edit {} {}",
            r["id"].as_str().unwrap_or("?"),
            r["status"].as_str().unwrap_or("?")
        );
        self.attend(who, tool, windows, Some(note)).await;
    }

    async fn get_diff(&self, p: GetDiff, who: &AgentId) -> ToolOut {
        // {from, to}: two revisions' trees, as `jj diff --from --to` (a missing side is the
        // agent's working copy, as in jj). {revset}: the cumulative diff of that range.
        if p.from.is_some() || p.to.is_some() {
            if p.revset.is_some() {
                return Err("get_diff: pass revset, or from/to, not both".into());
            }
            let here = who
                .workspace
                .as_ref()
                .map(|w| format!("{w}@"))
                .unwrap_or_else(|| "@".into());
            let from = p.from.clone().unwrap_or_else(|| here.clone());
            let to = p.to.clone().unwrap_or(here);
            let files = match self
                .backend()
                .tree_diff(&from, &to, p.fileset.as_deref())
                .await
            {
                Ok(r) => r["files"].as_array().cloned().unwrap_or_default(),
                Err(BackendError::Unsupported(_)) => {
                    return Err(format!(
                        "get_diff from/to: this server has no two-revision diff; \
                         use revset \"{from}..{to}\" for the commits in between"
                    ));
                }
                Err(e) => return Err(err(e)),
            };
            let paths: Vec<String> = files
                .iter()
                .filter_map(|f| f["path"].as_str().map(String::from))
                .collect();
            let windows = self.windows_of_paths(who, &paths).await;
            self.attend(
                who,
                "get_diff",
                windows,
                Some(format!("diff --from {from} --to {to}")),
            )
            .await;
            return Ok(Out::Blocks(render_diff(&files)));
        }
        let revset = p.revset.clone().unwrap_or_else(|| {
            who.workspace
                .as_ref()
                .map(stack_of)
                .unwrap_or_else(|| "trunk()..@".into())
        });
        let files = match self
            .backend()
            .range_diff(&revset, p.fileset.as_deref())
            .await
        {
            Ok(r) => r["files"].as_array().cloned().unwrap_or_default(),
            Err(BackendError::Unsupported(_)) => {
                self.diff_from_view(&revset, p.fileset.as_deref()).await?
            }
            Err(e) => return Err(err(e)),
        };
        let paths: Vec<String> = files
            .iter()
            .filter_map(|f| f["path"].as_str().map(String::from))
            .collect();
        let windows = self.windows_of_paths(who, &paths).await;
        self.attend(who, "get_diff", windows, Some(format!("diff of {revset}")))
            .await;
        Ok(Out::Blocks(render_diff(&files)))
    }

    /// FileDiffs from a range view's hunk windows (servers without a range-diff route): the tip
    /// blob gives the text of added and context lines. Files the range removed are not in it.
    async fn diff_from_view(
        &self,
        revset: &str,
        fileset: Option<&str>,
    ) -> Result<Vec<Value>, String> {
        let mut spec = json!({ "mode": "range", "revset": revset });
        if let Some(f) = fileset {
            spec["fileset"] = json!(f);
        }
        let v = self.create_view(spec).await.map_err(err)?;
        let view = v["id"].as_str().unwrap_or_default().to_string();
        let windows = self.backend().view_windows(&view).await.map_err(err)?;
        let hunks: Vec<&Value> = windows.iter().filter(|h| h["kind"] != "context").collect();
        let ids: Vec<String> = hunks.iter().map(|h| id_of(h)).collect();
        let b = self
            .backend()
            .window_batch(&view, &ids, &["diff"])
            .await
            .map_err(err)?;
        let mut files: std::collections::BTreeMap<String, (Value, Vec<Value>)> = Default::default();
        for d in b["diffs"].as_array().into_iter().flatten() {
            let Some(h) = hunks.iter().find(|h| h["id"] == d["id"]) else {
                continue;
            };
            let mut lines = d["lines"].as_array().cloned().unwrap_or_default();
            if lines.is_empty() {
                continue;
            }
            let news: Vec<u32> = lines
                .iter()
                .filter_map(|l| l["newLine"].as_u64().map(|n| n as u32))
                .collect();
            if let (Some(&lo), Some(&hi), Some(blob)) =
                (news.iter().min(), news.iter().max(), h["blob"].as_str())
                && let Ok(t) = self.backend().blob_text(blob, Some(lo), Some(hi)).await
            {
                let from = t["from"].as_u64().unwrap_or(lo as u64) as u32;
                let text = t["lines"].as_array().cloned().unwrap_or_default();
                for l in lines.iter_mut() {
                    if l["text"].is_null()
                        && let Some(n) = l["newLine"].as_u64()
                        && let Some(x) = text.get((n as u32).saturating_sub(from) as usize)
                    {
                        l["text"] = x.clone();
                    }
                }
            }
            let path = h["path"].as_str().unwrap_or("").to_string();
            let e = files
                .entry(path)
                .or_insert_with(|| (h["lines"]["fileLines"].clone(), Vec::new()));
            e.1.push(json!({ "lines": lines }));
        }
        Ok(files
            .into_iter()
            .map(|(path, (file_lines, hunks))| {
                let all: Vec<&Value> = hunks
                    .iter()
                    .flat_map(|h| h["lines"].as_array().into_iter().flatten())
                    .collect();
                let added = all.iter().all(|l| l["kind"] == "add")
                    && file_lines.as_u64() == Some(all.len() as u64);
                let status = if added { "added" } else { "modified" };
                json!({ "path": path, "status": status, "hunks": hunks })
            })
            .collect())
    }

    /// The commit holding a conflict, from a change id, commit id or prefix.
    async fn conflict_commit(&self, rev: &str, path: &str) -> Result<String, String> {
        let list = self.backend().conflicts(None).await.map_err(err)?;
        Ok(list
            .iter()
            .find(|c| {
                c["path"] == path
                    && (c["change"].as_str().is_some_and(|x| x.starts_with(rev))
                        || c["commit"].as_str().is_some_and(|x| x.starts_with(rev)))
            })
            .and_then(|c| c["commit"].as_str())
            .unwrap_or(rev)
            .to_string())
    }

    async fn wait_for_events(&self, p: WaitForEvents) -> ToolOut {
        let kinds: Vec<String> = match &p.kinds {
            Some(k) => {
                // A typo would wait for an event that never comes.
                if let Some(bad) = k.iter().find(|k| !EVENT_KINDS.contains(&k.as_str())) {
                    return Err(format!(
                        "unknown event type {bad:?}; types: {}",
                        EVENT_KINDS.join(", ")
                    ));
                }
                k.clone()
            }
            None => DEFAULT_KINDS.iter().map(|s| s.to_string()).collect(),
        };
        let timeout = Duration::from_secs(p.timeout_s.unwrap_or(30).clamp(1, 60));
        let r = self
            .backend()
            .events(p.cursor.as_deref(), &kinds, timeout)
            .await
            .map_err(err)?;
        let events = r["events"].as_array().cloned().unwrap_or_default();
        if r["resync"] == true
            || events
                .iter()
                .any(|e| e["type"] == "op.changed" || e["type"] == "view.invalidated")
        {
            self.invalidate();
        }
        // The newest `limit` events; the cursor still moves past all of them.
        let limit = p.limit.unwrap_or(20).max(1);
        let skipped = events.len().saturating_sub(limit);
        let mut out = json!({
            "cursor": r["cursor"],
            "events": events[skipped..].iter().map(slim_event).collect::<Vec<_>>(),
        });
        if r["resync"] == true {
            // The cursor is older than the event log keeps: events were missed.
            out["resync"] = json!(true);
        }
        if skipped > 0 {
            out["skipped"] = json!(skipped);
        }
        Ok(Out::Json(out))
    }

    // ── Resources and prompts ──

    async fn read_uri(&self, uri: &str) -> Result<Value, String> {
        match uri {
            RESOURCE_VIEW => match self.get_view_state().await? {
                Out::Json(v) | Out::Code(v) => Ok(v),
                Out::Blocks(b) => Ok(json!(b.join("\n"))),
            },
            RESOURCE_OPEN_THREADS => {
                let p = ListThreads {
                    status: Some(vec![ThreadStatus::Open]),
                    ..Default::default()
                };
                let ts = self.threads_matching(&p).await?;
                Ok(json!(ts.iter().map(annotate::summary).collect::<Vec<_>>()))
            }
            RESOURCE_CONFLICTS => Ok(json!(self.backend().conflicts(None).await.map_err(err)?)),
            _ => Err(format!("unknown resource {uri}")),
        }
    }

    fn instructions(&self) -> String {
        let mut s = format!(
            "Code viewer for reviewing jj repositories with a human ({} profile). You work in the \
             served workspace (or your cwd's) unless you bind another. Code comes back as numbered \
             lines with links (ports, → callee / ← callers) and threads inline; patches use those \
             line numbers. The human sees what you read.",
            self.profile.name()
        );
        if self.profile.has("edit_code") {
            s.push_str(
                " Review loop: list_threads {assignee: \"agent\", status: [\"open\"], withCode: \
                 true}, then per thread one edit_code {thread, patches, reply} (rev, path and \
                 baseBlob come from the thread; the reply marks it addressed), or apply_suggestion \
                 {thread, reply} when its ```suggestion is right as written.",
            );
        } else if self.profile.has("create_thread") {
            s.push_str(" Post findings with create_thread; answer threads with reply.");
        }
        s
    }
}

/// Links of a "path:line" or qualname target narrowed to the symbol (or line) `span` of `path`:
/// first-hop links must start (out) or end (in) inside it; deeper hops are kept when they continue
/// from a kept link's other end.
fn narrow_links(
    links: Vec<Value>,
    path: &str,
    span: (u32, u32),
    start: &[String],
    dir: &str,
) -> Vec<Value> {
    let inside = |end: &Value| {
        end["path"] == path
            && end["line"]
                .as_u64()
                .is_some_and(|l| l as u32 >= span.0 && l as u32 <= span.1)
    };
    let at_start = |end: &Value| {
        end["window"]
            .as_str()
            .is_some_and(|w| start.iter().any(|s| s == w))
            || end["path"] == path
    };
    let (out, inn) = (dir != "in", dir != "out");
    let mut kept: Vec<Value> = Vec::new();
    let mut reach: HashSet<String> = HashSet::new();
    let mut rest: Vec<Value> = Vec::new();
    for l in links {
        let first = at_start(&l["from"]) || at_start(&l["to"]);
        if !first {
            rest.push(l);
            continue;
        }
        let ok = (out && inside(&l["from"])) || (inn && inside(&l["to"]));
        if ok {
            let other = if inside(&l["from"]) {
                &l["to"]
            } else {
                &l["from"]
            };
            if let Some(w) = other["window"].as_str() {
                reach.insert(w.to_string());
            }
            kept.push(l);
        }
    }
    loop {
        let before = kept.len();
        rest.retain(|l| {
            let from = l["from"]["window"].as_str().unwrap_or("");
            let to = l["to"]["window"].as_str().unwrap_or("");
            let next = if out && reach.contains(from) {
                Some(to)
            } else if inn && reach.contains(to) {
                Some(from)
            } else {
                None
            };
            match next {
                Some(n) => {
                    reach.insert(n.to_string());
                    kept.push(l.clone());
                    false
                }
                None => true,
            }
        });
        if kept.len() == before {
            break;
        }
    }
    kept
}

/// FileDiffs as ([file summary JSON], unified diff text). Prints at most [`MAX_DIFF_LINES`] diff
/// lines; later files are listed with their counts only.
fn render_diff(files: &[Value]) -> Vec<String> {
    let mut summary = Vec::new();
    let mut text = String::new();
    let mut printed = 0usize;
    let mut omitted = 0usize;
    for f in files {
        let path = f["path"].as_str().unwrap_or("");
        let hunks = f["hunks"].as_array().cloned().unwrap_or_default();
        let lines = |h: &Value| h["lines"].as_array().cloned().unwrap_or_default();
        let count = |k: &str| -> usize {
            hunks
                .iter()
                .map(|h| lines(h).iter().filter(|l| l["kind"] == k).count())
                .sum()
        };
        let (added, removed) = (count("add"), count("del"));
        let mut row =
            json!({ "path": path, "status": f["status"], "added": added, "removed": removed });
        if f["conflicted"] == true {
            row["conflicted"] = json!(true);
        }
        summary.push(row);
        let n: usize = hunks.iter().map(|h| lines(h).len()).sum();
        if printed > 0 && printed + n > MAX_DIFF_LINES {
            omitted += 1;
            continue;
        }
        text.push_str(&format!(
            "--- {} {path} (+{added} -{removed})\n",
            f["status"].as_str().unwrap_or("modified")
        ));
        if f["conflicted"] == true {
            text.push_str("(conflicted: read it with get_conflict)\n");
        }
        for h in &hunks {
            let ls = lines(h);
            let first = |k: &str| ls.iter().find_map(|l| l[k].as_u64()).unwrap_or(0);
            let olds = ls.iter().filter(|l| l["kind"] != "add").count();
            let news = ls.iter().filter(|l| l["kind"] != "del").count();
            text.push_str(&format!(
                "@@ -{},{olds} +{},{news} @@\n",
                first("oldLine"),
                first("newLine")
            ));
            for l in &ls {
                let mark = match l["kind"].as_str() {
                    Some("add") => '+',
                    Some("del") => '-',
                    _ => ' ',
                };
                text.push(mark);
                text.push_str(l["text"].as_str().unwrap_or(""));
                text.push('\n');
            }
            printed += ls.len();
        }
    }
    if omitted > 0 {
        text.push_str(&format!(
            "({omitted} more files not printed: pass fileset to see them)\n"
        ));
    }
    if files.is_empty() {
        text.push_str("(no changes)\n");
    }
    vec![compact(&json!(summary)), text]
}

pub fn stack_of(ws: &String) -> String {
    format!("trunk()..{ws}@")
}

fn prompt_text(name: &str, args: &serde_json::Map<String, Value>) -> Option<String> {
    let revset = args
        .get("revset")
        .and_then(|v| v.as_str())
        .unwrap_or("your workspace's stack");
    Some(match name {
        "address_review" => "Address the review threads assigned to you.\n\
            1. list_threads {assignee: \"agent\", status: [\"open\", \"in-progress\"], withCode: true}: every thread with its comments and the code around it. If there are none, wait_for_events {kinds: [\"thread.upserted\", \"review.submitted\"]} and list again.\n\
            2. For each thread, change the code and answer in one call: edit_code {thread, patches (line numbers as shown), reply: what you changed}. rev, path and baseBlob come from the thread; the reply marks it addressed. When its ```suggestion is right as written, apply_suggestion {thread, reply} instead. Read more (read_code, search, get_links) only when the code shown is not enough.\n\
            3. Several threads in one file: keep using the line numbers shown; the server rebases a patch over your earlier edits unless they overlap (then re-read with get_thread).\n\
            4. A thread you should not change: reply {thread, body: why, setStatus: \"wontfix\"}.".to_string(),
        "review_range" => format!(
            "Review {revset}.\n\
            1. get_diff for the range; get_hotspots to decide what to read first.\n\
            2. Read the changed code window by window (read_window / read_code) and follow links (get_links) where behaviour crosses files.\n\
            3. Post each problem as create_thread (kind \"finding\", a severity, and a ```suggestion block when the fix is local).\n\
            4. Finish with create_tour over the findings in reading order."
        ),
        "resolve_conflicts" => format!(
            "Resolve the conflicts in {revset}.\n\
            1. list_conflicts.\n\
            2. For each: get_conflict, read both sides and the base, decide what each side meant.\n\
            3. resolve_conflict with the whole resolved file (no markers).\n\
            4. list_conflicts again: descendants may have new conflicts after the rewrite."
        ),
        _ => return None,
    })
}

impl ServerHandler for CvMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_resources_subscribe()
                .enable_prompts()
                .build(),
        )
        .with_server_info(Implementation::new("cv", env!("CARGO_PKG_VERSION")))
        .with_instructions(self.instructions())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(
            all_tools()
                .into_iter()
                .filter(|t| self.profile.has(t.name.as_ref()))
                .collect(),
        ))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        if !self.profile.has(name) {
            return None;
        }
        all_tools().into_iter().find(|t| t.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let args = request.arguments.map(Value::Object).unwrap_or(json!({}));
        Ok(self.call(&request.name, args, Some(&context)).await.into())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(vec![
            Resource::new(RESOURCE_VIEW, "view-state")
                .with_description("What the human is looking at (view and ViewState).")
                .with_mime_type("application/json"),
            Resource::new(RESOURCE_OPEN_THREADS, "open-threads")
                .with_description("Open review threads (ThreadSummary[]).")
                .with_mime_type("application/json"),
            Resource::new(RESOURCE_CONFLICTS, "conflicts")
                .with_description("Conflicted files (ConflictRecord[]).")
                .with_mime_type("application/json"),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let v = self
            .read_uri(&request.uri)
            .await
            .map_err(|e| ErrorData::resource_not_found(e, None))?;
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(compact(&v), request.uri).with_mime_type("application/json"),
        ])
        .into())
    }

    #[allow(deprecated)]
    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        if !SUBSCRIBABLE.contains(&request.uri.as_str()) {
            return Err(ErrorData::resource_not_found(
                format!(
                    "{} is not subscribable; resources: {}",
                    request.uri,
                    SUBSCRIBABLE.join(", ")
                ),
                None,
            ));
        }
        let start = self.shared.backend.cursor().await.ok().flatten();
        self.shared.subs.subscribe(
            &self.shared.backend,
            &self.session_sub,
            context.peer.clone(),
            &request.uri,
            start,
        );
        Ok(())
    }

    #[allow(deprecated)]
    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.shared
            .subs
            .unsubscribe(&self.session_sub, &request.uri);
        Ok(())
    }

    fn accepted_subscription_filter(
        &self,
        _requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        Some(
            SubscriptionFilter::builder()
                .resource_subscriptions(SUBSCRIBABLE.iter().copied())
                .build(),
        )
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), ErrorData> {
        let uris = context
            .accepted()
            .resource_subscriptions
            .clone()
            .unwrap_or_default()
            .into_iter()
            .collect();
        let start = self.shared.backend.cursor().await.ok().flatten();
        let _guard =
            self.shared
                .subs
                .listen(&self.shared.backend, uris, context.sink().clone(), start);
        context.cancelled().await;
        Ok(())
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        let revset = || {
            vec![
                PromptArgument::new("revset")
                    .with_description("Revset, default your workspace's stack")
                    .with_required(false),
            ]
        };
        Ok(ListPromptsResult::with_all_items(vec![
            Prompt::new(
                "address_review",
                Some(
                    "Fetch the open threads assigned to the agent with their code, fix and answer each in one call.",
                ),
                None,
            ),
            Prompt::new(
                "review_range",
                Some(
                    "Review a revset: read the range diff window by window and post findings as threads.",
                ),
                Some(revset()),
            ),
            Prompt::new(
                "resolve_conflicts",
                Some(
                    "List conflicts in a revset, resolve each from its sides, re-check descendants.",
                ),
                Some(revset()),
            ),
        ]))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        let args = request.arguments.unwrap_or_default();
        let text = prompt_text(&request.name, &args).ok_or_else(|| {
            ErrorData::invalid_params(format!("unknown prompt {}", request.name), None)
        })?;
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)]).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_are_slim() {
        for t in all_tools() {
            let s = serde_json::to_string(&*t.input_schema).unwrap();
            for junk in [
                "$schema",
                "\"title\":\"",
                "\"format\":",
                "\"null\"",
                "$ref",
                "$defs",
            ] {
                assert!(!s.contains(junk), "{} schema has {junk}: {s}", t.name);
            }
        }
        let lt = all_tools()
            .into_iter()
            .find(|t| t.name == "list_threads")
            .unwrap();
        let status = &lt.input_schema["properties"]["status"]["items"];
        assert_eq!(status["enum"][0], "open", "{status}");
        let ec = all_tools()
            .into_iter()
            .find(|t| t.name == "edit_code")
            .unwrap();
        assert_eq!(
            ec.input_schema["properties"]["mode"]["enum"][1],
            "insert-after"
        );
        assert_eq!(
            ec.input_schema["properties"]["patches"]["items"]["required"],
            json!(["start", "end", "text"])
        );
    }

    #[test]
    fn floats_round_to_three_digits() {
        assert_eq!(
            round_floats(
                &json!({ "a": 0.0020288100342832995, "b": [0.9985753670621581, 3.0], "c": 7 })
            ),
            json!({ "a": 0.00203, "b": [0.999, 3], "c": 7 })
        );
    }

    #[test]
    fn enclosing_symbol_spans() {
        // def at 121, 132, 137 (depth 0), a method at 140 (depth 1) inside 137's class.
        let hs = [(121, 0), (132, 0), (137, 0), (140, 1), (148, 0)];
        assert_eq!(enclosing(&hs, 133, 134, 170), Some((132, 136)));
        assert_eq!(enclosing(&hs, 141, 141, 170), Some((140, 147)));
        assert_eq!(enclosing(&hs, 150, 152, 170), Some((148, 170)));
        assert_eq!(enclosing(&hs, 100, 100, 170), None);
    }

    #[test]
    fn default_workspace() {
        let ws = [
            json!({ "name": "default", "root": "/r/main", "serving": true }),
            json!({ "name": "feature", "root": "/r/feature" }),
        ];
        let cwd = std::path::Path::new("/r/feature/src");
        assert_eq!(
            pick_default_workspace(&ws, Some(cwd)).as_deref(),
            Some("feature")
        );
        assert_eq!(
            pick_default_workspace(&ws, None).as_deref(),
            Some("default")
        );
        assert_eq!(
            pick_default_workspace(&[json!({ "name": "solo" })], None).as_deref(),
            Some("solo")
        );
    }
}
