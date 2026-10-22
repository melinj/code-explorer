//! Workspaces are first-class (D12; DESIGN §4.1 "Live working copies", §7 "Workspaces"):
//!
//! - **Live overlay.** A snapshot view of a workspace's `@` (or of `options.workspace`) shows the
//!   files edited on disk but not yet snapshotted with their on-disk text, under `live…` blob ids,
//!   and their windows carry `flags.unsnapshotted`. Nothing is snapshotted on anyone's behalf: the
//!   view in [`AppState::views`] stays the pure snapshot and the overlay is a derived copy
//!   ([`display_view`]) that tiles, window batches and blob text serve.
//! - **Overlaps.** Each workspace's stack (`trunk()..<name>@`) is diffed once per operation;
//!   windows whose lines another workspace's stack changes carry `flags.alsoChangedIn`.
//! - **Cards.** `workspace.updated` carries the full `WorkspaceSummary` (stack, unsnapshotted
//!   files, overlaps, agent activity, ISO times), emitted when @ moves (cv-review, through the
//!   summarizer installed here) and when [`poll_once`] sees a card change (files dirtied, overlap
//!   or agent activity changed).
//! - **History overlay.** [`install`] gives the history panel its review badges (thread count,
//!   "changed since review", touched by agent).
//!
//! The poller ([`start`]) is cheap: one stat walk per workspace per tick; stacks and overlaps are
//! recomputed only when the operation changes. It never touches the op-head watcher.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

use cv_contracts::readside::{
    OpRef, RevisionReview, StackSummary, StaleReason, WorkspaceOverlap, WorkspaceStale,
    WorkspaceSummary, WorkspaceWc,
};
use cv_contracts::review::{Actor, ServerEvent};
use cv_review::Review;
use cv_vcs::read::{self, FileChange, RevsetEnv, WorkspaceState};
use cv_vcs::{CommitInfo, JjRepo};
use serde_json::{Value, json};

use crate::index::View;
use crate::routes::{ApiErr, AppState};

/// How often [`start`]'s thread looks at the working copies.
pub const POLL_INTERVAL: Duration = Duration::from_millis(1000);
/// An agent counts as editing this long after an edit-ish tool call or edit.
const EDITING_FOR: Duration = Duration::from_secs(60);
/// … and as reading this long after any tool call; idle after that.
const READING_FOR: Duration = Duration::from_secs(120);
/// MCP tools that write (mcp.ts): their attention marks the agent as editing.
const EDIT_TOOLS: [&str; 6] = [
    "edit_code",
    "propose_edit",
    "apply_edit",
    "apply_suggestion",
    "resolve_conflict",
    "undo_edit",
];
/// Live files larger than this are flagged but not overlaid.
const MAX_LIVE_BYTES: u64 = 2 << 20;

/// Workspace state held in [`AppState::live`].
#[derive(Default)]
pub struct Live {
    overlays: RwLock<HashMap<String, Arc<Overlay>>>,
    stacks: Mutex<Option<Arc<Stacks>>>,
    also: RwLock<HashMap<String, Arc<AlsoMap>>>,
    cards: Mutex<Option<HashMap<String, Value>>>,
    activity: Mutex<HashMap<String, Activity>>,
    recovered: Mutex<Recoveries>,
    started: AtomicBool,
}

/// `workspace.recovered` bookkeeping for divergence that operations run elsewhere leave.
#[derive(Default)]
struct Recoveries {
    /// Event-log cursor up to which cv-review's own `workspace.recovered` events were read.
    cursor: Option<u64>,
    /// (workspace, divergent commit) pairs already announced (by cv-review or here).
    announced: HashSet<(String, String)>,
    /// Pairs seen once: announced on the next look unless cv-review announced them meanwhile.
    pending: HashSet<(String, String)>,
}

/// A live file of an overlay: (path, on-disk text, lane for a file the view lacks).
type LiveFile = (String, Option<String>, Option<&'static str>);

/// A snapshot view with a workspace's unsnapshotted files overlaid from disk.
pub struct Overlay {
    /// The pure view this was built on (rebuilt when the stored view is replaced).
    base: Arc<View>,
    pub workspace: String,
    sig: u64,
    /// (path, on-disk text, lane); text `None` = deleted, binary or too large (flagged only);
    /// lane `Some` = a file never snapshotted, drawn as new windows ([`crate::incremental`]).
    files: Vec<LiveFile>,
    pub view: Arc<View>,
    /// Windows of the unsnapshotted files.
    pub flagged: HashSet<String>,
}

struct AlsoMap {
    key: String,
    by_window: HashMap<String, Vec<String>>,
}

/// One workspace's stack, as of one operation.
pub struct Stack {
    pub name: String,
    pub wc: String,
    pub summary: StackSummary,
    /// Paths any stack commit touches.
    pub files: BTreeSet<String>,
    /// The paths each stack commit touches, by commit id.
    per_commit: std::collections::BTreeMap<String, Vec<String>>,
    /// Revset of the stack's commits.
    pub revset: String,
}

pub struct Stacks {
    op: String,
    pub list: Vec<Stack>,
    /// Each workspace's newest operation (walking the op log once per operation).
    ops: HashMap<String, read::OpSummary>,
    /// (a, b) → paths both stacks change in commits of their own, sorted ([`own_files`]).
    overlaps: HashMap<(String, String), Vec<String>>,
}

/// Paths the commits of `w`'s stack that are not ancestors of `other@` change: its own work.
/// Two stacks share every commit below where they fork (with no remote, `trunk()` is `root()` and
/// a stack is all of history), so overlap warnings and Compare look at these only.
fn own_files(repo: &JjRepo, env: &RevsetEnv, w: &Stack, other: &Stack) -> BTreeSet<String> {
    let revset = format!(
        "({}) ~ ::{}",
        w.revset,
        read::workspace_wc_revset(&other.name)
    );
    let own = repo.eval(env, &revset, 500).unwrap_or_default();
    let mut out = BTreeSet::new();
    for c in &own {
        match w.per_commit.get(&c.commit) {
            Some(files) => out.extend(files.iter().cloned()),
            None => out.extend(read::files_of(repo, std::slice::from_ref(c)).0),
        }
    }
    out
}

struct Activity {
    at: Instant,
    at_iso: String,
    edit_at: Option<Instant>,
}

fn emit(app: &AppState, events: Vec<ServerEvent>) {
    if events.is_empty() {
        return;
    }
    let review = app.review.lock().unwrap().clone();
    if let Some(r) = review {
        for e in events {
            r.emit(e);
        }
    }
}

// ─── Stacks and cards ───────────────────────────────────────────────────────

impl Stacks {
    /// Every workspace's stack (`trunk()..<name>@`) at the repo's operation.
    pub fn compute(repo: &JjRepo, env: &RevsetEnv, states: &[WorkspaceState]) -> Stacks {
        let list = states
            .iter()
            .map(|w| {
                let revset = format!("trunk()..{}", read::workspace_wc_revset(&w.name));
                let commits = repo.eval(env, &revset, 500).unwrap_or_default();
                let (files, per_commit) = read::files_of(repo, &commits);
                let conflicts = commits
                    .iter()
                    .filter(|c| repo.is_conflicted(&c.commit))
                    .count();
                Stack {
                    name: w.name.clone(),
                    wc: w.wc.commit.clone(),
                    summary: StackSummary {
                        revset: revset.clone(),
                        changes: commits.len() as u32,
                        files: files.len() as u32,
                        conflicts: conflicts as u32,
                    },
                    files,
                    per_commit,
                    revset,
                }
            })
            .collect::<Vec<Stack>>();
        let mut overlaps = HashMap::new();
        for (i, a) in list.iter().enumerate() {
            for b in &list[i + 1..] {
                if a.files.is_disjoint(&b.files) {
                    continue;
                }
                let mine = own_files(repo, env, a, b);
                let theirs = own_files(repo, env, b, a);
                let shared: Vec<String> = mine.intersection(&theirs).cloned().collect();
                if !shared.is_empty() {
                    overlaps.insert((a.name.clone(), b.name.clone()), shared.clone());
                    overlaps.insert((b.name.clone(), a.name.clone()), shared);
                }
            }
        }
        Stacks {
            op: repo.op_id(),
            list,
            ops: repo.last_ops(5000),
            overlaps,
        }
    }
}

/// Every workspace's stack at the repo's operation (cached per operation).
pub fn stacks(
    app: &AppState,
    repo: &JjRepo,
    env: &RevsetEnv,
    states: &[WorkspaceState],
) -> Arc<Stacks> {
    let op = repo.op_id();
    if let Some(s) = app.live.stacks.lock().unwrap().as_ref()
        && s.op == op
        && s.list.len() == states.len()
    {
        return s.clone();
    }
    let s = Arc::new(Stacks::compute(repo, env, states));
    *app.live.stacks.lock().unwrap() = Some(s.clone());
    s
}

/// `WorkspaceSummary` cards (model.ts) without agents ([`crate::agent_api::fill_agents`] adds them).
pub fn summaries(
    states: &[WorkspaceState],
    stacks: &Stacks,
    serving: &str,
) -> Vec<WorkspaceSummary> {
    let ops = &stacks.ops;
    states
        .iter()
        .map(|w| {
            let mine = stacks.list.iter().find(|s| s.name == w.name);
            let files = mine.map(|s| s.files.clone()).unwrap_or_default();
            // On each stack's own commits, not the history both share ([`own_files`]).
            let overlaps = stacks
                .list
                .iter()
                .filter(|o| o.name != w.name)
                .filter_map(|o| {
                    let paths = stacks
                        .overlaps
                        .get(&(w.name.clone(), o.name.clone()))
                        .cloned()
                        .unwrap_or_default();
                    (!paths.is_empty()).then(|| WorkspaceOverlap {
                        workspace: o.name.clone(),
                        paths,
                    })
                })
                .collect();
            let last_op = ops
                .get(&w.name)
                .map(|o| OpRef {
                    op: o.op.clone(),
                    time: cv_store::iso_from_ms(o.time_ms),
                    description: o.description.clone(),
                })
                .unwrap_or(OpRef {
                    op: String::new(),
                    time: String::new(),
                    description: String::new(),
                });
            WorkspaceSummary {
                name: w.name.clone(),
                root: w
                    .root
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                wc: WorkspaceWc {
                    change: w.wc.change.clone(),
                    commit: w.wc.commit.clone(),
                },
                serving: w.name == serving,
                stack: mine.map(|s| s.summary.clone()).unwrap_or(StackSummary {
                    revset: format!("trunk()..{}", read::workspace_wc_revset(&w.name)),
                    changes: 0,
                    files: 0,
                    conflicts: 0,
                }),
                // N8: a stale workspace's files are not overlaid (they belong to an older @), so
                // the card does not count them either; its stale note says how many there are.
                unsnapshotted_files: if w.stale {
                    0
                } else {
                    w.unsnapshotted.unwrap_or(0)
                },
                agent: None,
                last_op,
                overlaps,
                files: files.iter().take(50).cloned().collect(),
                stale: stale_flag(w),
            }
        })
        .collect()
}

fn short(id: &str) -> &str {
    &id[..12.min(id.len())]
}

/// The card's stale flag (D12, DESIGN §6): recovering a workspace that an operation run elsewhere
/// left behind is its owner's job, since cv never snapshots or updates another workspace's files.
fn stale_flag(w: &WorkspaceState) -> Option<WorkspaceStale> {
    if w.stale {
        return Some(WorkspaceStale {
            reason: StaleReason::WorkingCopy,
            note: format!(
                "{}@ was rewritten by an operation run elsewhere; the files on disk are from the old @{}. Run `jj workspace update-stale` in that workspace (edits jj has not snapshotted are kept in a divergent commit).",
                w.name,
                match w.unsnapshotted {
                    Some(n) if n > 0 => format!(
                        " ({n} file{} edited there, not shown until then)",
                        if n == 1 { "" } else { "s" }
                    ),
                    _ => String::new(),
                }
            ),
            commits: w.divergent.clone(),
        });
    }
    let first = w.divergent.first()?;
    Some(WorkspaceStale {
        reason: StaleReason::Divergent,
        note: format!(
            "{}@'s change is divergent: {} is another visible commit of it and may hold edits made before a rewrite (`jj squash --from {} --into {}@` to bring them back, or `jj abandon {}`).",
            w.name,
            short(first),
            short(first),
            w.name,
            short(first)
        ),
        commits: w.divergent.clone(),
    })
}

/// `workspace.recovered` for divergence an operation run elsewhere left (§6 step 3 covers our own
/// edits, which cv-review announces): a (workspace, sibling) pair is announced on its second
/// sighting, one poll later, unless cv-review announced it meanwhile. The first look only records.
fn external_recoveries(app: &AppState, states: &[WorkspaceState]) -> Vec<ServerEvent> {
    let review = app.review.lock().unwrap().clone();
    let mut rec = app.live.recovered.lock().unwrap();
    let primed = rec.cursor.is_some();
    if let Some(r) = &review {
        let mut after = rec.cursor.unwrap_or_else(|| r.events().cursor());
        for _ in 0..20 {
            let page = r.events().since(after, None);
            let Some(&(last, _)) = page.last() else { break };
            for (_, e) in page {
                if let ServerEvent::WorkspaceRecovered {
                    workspace,
                    divergent_commit,
                    ..
                } = e
                {
                    rec.announced.insert((workspace, divergent_commit));
                }
            }
            after = last;
        }
        rec.cursor = Some(after);
    } else {
        rec.cursor = Some(0);
    }
    let now: HashSet<(String, String)> = states
        .iter()
        .flat_map(|w| w.divergent.iter().map(|c| (w.name.clone(), c.clone())))
        .collect();
    let mut out = Vec::new();
    if !primed {
        rec.announced.extend(now);
        return out;
    }
    let mut pending = HashSet::new();
    let mut sorted: Vec<&(String, String)> = now.iter().collect();
    sorted.sort();
    for p in sorted {
        if rec.announced.contains(p) {
            continue;
        }
        if rec.pending.contains(p) {
            out.push(ServerEvent::WorkspaceRecovered {
                workspace: p.0.clone(),
                divergent_commit: p.1.clone(),
                note: format!(
                    "workspace {}'s @ is divergent after an operation run elsewhere; {} is another visible commit of its change and may hold edits made before the rewrite (jj squash --from {} --into {}@ to bring them back, or jj abandon {} to drop them)",
                    p.0,
                    short(&p.1),
                    short(&p.1),
                    p.0,
                    short(&p.1)
                ),
            });
            rec.announced.insert(p.clone());
        } else {
            pending.insert(p.clone());
        }
    }
    rec.pending = pending;
    out
}

/// Cards with agents, as GET /workspaces returns them (computed now, at the head operation).
pub fn cards_now(app: &Arc<AppState>, repo: &JjRepo) -> (Vec<WorkspaceState>, Value) {
    let env = repo.revset_env();
    let states = repo.workspaces(&env);
    let st = stacks(app, repo, &env, &states);
    let list = summaries(&states, &st, app.repo.workspace_name());
    let mut cards = serde_json::to_value(&list).unwrap_or_default();
    crate::agent_api::fill_agents(app, &mut cards);
    (states, cards)
}

// ─── Agent activity ─────────────────────────────────────────────────────────

/// An agent called a tool (agent.attention): reading, or editing for edit tools.
pub fn note_attention(app: &AppState, agent: &str, tool: &str) {
    let mut m = app.live.activity.lock().unwrap();
    let now = Instant::now();
    let e = m.entry(agent.to_string()).or_insert(Activity {
        at: now,
        at_iso: cv_store::now_iso(),
        edit_at: None,
    });
    e.at = now;
    e.at_iso = cv_store::now_iso();
    if EDIT_TOOLS.contains(&tool) {
        e.edit_at = Some(now);
    }
}

/// An agent previewed, applied or resolved an edit.
pub fn note_edit(app: &AppState, who: &Actor) {
    if let Actor::Agent { name, .. } = who {
        note_attention(app, name, "edit_code");
    }
}

/// (activity, last seen ISO) of an agent from its recent tool calls and edits: "editing" within
/// a minute of an edit, "reading" within two minutes of any call, else "idle". `None` when the
/// agent was not seen since the server started.
pub fn activity_of(app: &AppState, agent: &str) -> Option<(&'static str, String)> {
    let m = app.live.activity.lock().unwrap();
    let a = m.get(agent)?;
    let act = if a.edit_at.is_some_and(|t| t.elapsed() < EDITING_FOR) {
        "editing"
    } else if a.at.elapsed() < READING_FOR {
        "reading"
    } else {
        "idle"
    };
    Some((act, a.at_iso.clone()))
}

// ─── Live overlay ───────────────────────────────────────────────────────────

fn live_blob(text: &str) -> String {
    format!("live{:016x}", cv_store::fnv64(text.as_bytes()))
}

fn build_overlay(
    reg: &cv_lang::Registry,
    base: &Arc<View>,
    workspace: String,
    files: Vec<LiveFile>,
    sig: u64,
) -> Overlay {
    let mut v = (**base).clone();
    let mut flagged = HashSet::new();
    let mut any = false;
    let mut new = Vec::new();
    for (path, text, lane) in &files {
        let Some(&fi) = v.by_path.get(path) else {
            // Never snapshotted (§4.1): new windows, placed as incremental updates place them.
            if let (Some(t), Some(lane)) = (text, lane) {
                new.push(crate::incremental::NewFile {
                    path: path.clone(),
                    blob: live_blob(t),
                    lane,
                    text: t.clone(),
                });
            }
            continue;
        };
        flagged.extend(
            v.files[fi]
                .windows
                .iter()
                .map(|&w| v.windows[w].header.id.clone()),
        );
        if let Some(t) = text
            && *t != v.files[fi].text
        {
            crate::review_api::replace_file(&mut v, fi, reg, None, live_blob(t), t.clone());
            any = true;
        }
    }
    if !new.is_empty() {
        let commit = CommitInfo {
            commit: v.bootstrap.base.view.resolved.head.clone(),
            change: v
                .windows
                .first()
                .map(|w| w.header.change.clone())
                .unwrap_or_default(),
            parents: vec![],
            committer_time_ms: 0,
            description: String::new(),
        };
        flagged.extend(crate::incremental::add_files(
            &mut v, reg, None, &commit, new,
        ));
        any = true;
    }
    crate::review_api::finish_refresh(&mut v, base, any);
    Overlay {
        base: base.clone(),
        workspace,
        sig,
        files,
        view: Arc::new(v),
        flagged,
    }
}

/// The view as tiles, batches and blob text serve it: the live overlay when the view's workspace
/// has unsnapshotted files, else the stored view.
pub fn display_view(app: &AppState, id: &str) -> Result<Arc<View>, ApiErr> {
    let pure = app.view(id)?;
    let o = app.live.overlays.read().unwrap().get(id).cloned();
    Ok(match o {
        Some(o) if Arc::ptr_eq(&o.base, &pure) => o.view.clone(),
        Some(o) => {
            // The stored view moved on (an op, a layout patch): same disk texts, new base.
            let n = Arc::new(build_overlay(
                &app.registry,
                &pure,
                o.workspace.clone(),
                o.files.clone(),
                o.sig,
            ));
            let v = n.view.clone();
            app.live.overlays.write().unwrap().insert(id.to_string(), n);
            v
        }
        None => pure,
    })
}

/// The live overlay of a snapshot view just built, made before POST /views answers, so the first
/// tiles a client fetches already show the unsnapshotted files. The poller would build it on its
/// next tick, but its `view.invalidated` can reach the server's event stream before the client has
/// subscribed to the new view, and then the client never hears of the overlay ("Open @" showing a
/// workspace without its live files). The poller then sees the same signature and says nothing.
pub fn overlay_now(app: &AppState, id: &str) {
    if app.live.overlays.read().unwrap().contains_key(id) {
        return;
    }
    let Ok(v) = app.view(id) else { return };
    if v.bootstrap.base.view.spec["mode"] == "range" || app.read.range(id).is_some() {
        return;
    }
    let Ok(repo) = app.head_repo() else { return };
    let states = repo.workspaces(&repo.revset_env());
    let serving = app.repo.workspace_name().to_string();
    let Some((w, root)) = view_workspace(&v, &states, &serving)
        .filter(|w| !w.stale)
        .and_then(|w| w.root.as_ref().map(|r| (w, r)))
    else {
        return;
    };
    let files = live_files(app, &repo, &v, root, &w.unsnapshotted_paths);
    if files.is_empty() {
        return;
    }
    let sig = cv_store::fnv64(serde_json::to_string(&files).unwrap_or_default().as_bytes());
    let o = Arc::new(build_overlay(&app.registry, &v, w.name.clone(), files, sig));
    app.live
        .overlays
        .write()
        .unwrap()
        .entry(id.to_string())
        .or_insert(o);
}

/// An edit made on a live overlay (`baseBlob` is a `live…` id): what the human saw and edited is
/// the on-disk text, so the patches are applied to it and sent as whole content against the
/// snapshot blob the overlay replaced (cv-review then merges three-way as usual).
pub fn rebase_live_edit(app: &AppState, req: &mut cv_contracts::review::EditRequest) {
    let overlays: Vec<Arc<Overlay>> = app
        .live
        .overlays
        .read()
        .unwrap()
        .values()
        .cloned()
        .collect();
    for f in req.files.iter_mut() {
        let Some(b) = f.base_blob.clone().filter(|b| b.starts_with("live")) else {
            continue;
        };
        let Some((o, fi)) = overlays
            .iter()
            .find_map(|o| o.view.by_blob.get(&b).map(|&fi| (o, fi)))
        else {
            continue;
        };
        let live = &o.view.files[fi].text;
        if let Some(ps) = f.patches.take() {
            match cv_review::anchor::apply_patches(live, &ps) {
                Ok(t) => f.content = Some(t),
                Err(_) => {
                    f.patches = Some(ps);
                    continue;
                }
            }
        }
        f.base_blob = o
            .base
            .by_path
            .get(&f.path)
            .map(|&i| o.base.files[i].blob.clone());
    }
}

/// Overlay views (their live blobs are served by GET /blobs/:blob/text).
pub fn live_views(app: &AppState) -> Vec<Arc<View>> {
    app.live
        .overlays
        .read()
        .unwrap()
        .values()
        .map(|o| o.view.clone())
        .collect()
}

/// The overlay of a view, if any (tests, MCP).
pub fn overlay_of(app: &AppState, id: &str) -> Option<Arc<Overlay>> {
    app.live.overlays.read().unwrap().get(id).cloned()
}

/// Adds `flags.unsnapshotted` and `flags.alsoChangedIn` to header JSON (one header or an array).
/// Which of `view`'s windows [`decorate_headers`] changes (unsnapshotted, also changed in …), or
/// `None` when it changes none: lets the tile route serialize every other header by reference.
pub fn decorated_ids(app: &AppState, view: &str) -> Option<impl Fn(&str) -> bool + use<>> {
    let o = app.live.overlays.read().unwrap().get(view).cloned();
    let also = app.live.also.read().unwrap().get(view).cloned();
    if o.as_ref().is_none_or(|o| o.flagged.is_empty())
        && also.as_ref().is_none_or(|a| a.by_window.is_empty())
    {
        return None;
    }
    Some(move |id: &str| {
        o.as_ref().is_some_and(|o| o.flagged.contains(id))
            || also.as_ref().is_some_and(|a| a.by_window.contains_key(id))
    })
}

pub fn decorate_headers(app: &AppState, view: &str, headers: &mut Value) {
    let o = app.live.overlays.read().unwrap().get(view).cloned();
    let also = app.live.also.read().unwrap().get(view).cloned();
    if o.is_none() && also.is_none() {
        return;
    }
    let mut one = |h: &mut Value| {
        let Some(id) = h["id"].as_str().map(str::to_string) else {
            return;
        };
        if o.as_ref().is_some_and(|o| o.flagged.contains(&id)) {
            h["flags"]["unsnapshotted"] = json!(true);
        }
        if let Some(list) = also.as_ref().and_then(|a| a.by_window.get(&id)) {
            h["flags"]["alsoChangedIn"] = json!(list);
        }
    };
    match headers {
        Value::Array(list) => list.iter_mut().for_each(&mut one),
        h @ Value::Object(_) => one(h),
        _ => {}
    }
}

/// The workspace whose live files a snapshot view overlays: `options.workspace`, else the one
/// whose `@` the view shows (the serving workspace first). `None` with `liveWorkingCopy: false`.
fn view_workspace<'a>(
    v: &View,
    states: &'a [WorkspaceState],
    serving: &str,
) -> Option<&'a WorkspaceState> {
    let r = &v.bootstrap.base.view;
    let opts = &r.spec["options"];
    if opts["liveWorkingCopy"] == json!(false) {
        return None;
    }
    if let Some(name) = opts["workspace"].as_str() {
        return states.iter().find(|w| w.name == name);
    }
    let mut at: Vec<&WorkspaceState> = states
        .iter()
        .filter(|w| w.wc.commit == r.resolved.head)
        .collect();
    at.sort_by_key(|w| w.name != serving);
    at.first().copied()
}

fn read_live(root: &std::path::Path, path: &str) -> Option<String> {
    let p = root.join(path);
    let meta = std::fs::symlink_metadata(&p).ok()?;
    if !meta.is_file() || meta.len() > MAX_LIVE_BYTES {
        return None;
    }
    let bytes = std::fs::read(&p).ok()?;
    if bytes[..bytes.len().min(8000)].contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Snapshot views (not ranges) currently stored.
fn snapshot_views(app: &AppState) -> Vec<(String, Arc<View>)> {
    app.views
        .read()
        .unwrap()
        .iter()
        .filter(|(k, v)| {
            v.bootstrap.base.view.spec["mode"] != "range" && app.read.range(k).is_none()
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

// ─── alsoChangedIn ──────────────────────────────────────────────────────────

/// Lines (1-based, inclusive, in `vtext`) that the change `c` touches, mapped through the
/// change's base version; `None` = the whole file (added or deleted in the stack, binary).
fn mapped_lines(repo: &JjRepo, c: &FileChange, vtext: &str) -> Option<Vec<(u32, u32)>> {
    let (Some(b), Some(a)) = (&c.before, &c.after) else {
        return None;
    };
    let bt = repo.read_blob(&c.path, b).ok()?;
    let at = repo.read_blob(&c.path, a).ok()?;
    if bt[..bt.len().min(8000)].contains(&0) || at[..at.len().min(8000)].contains(&0) {
        return None;
    }
    let ba = read::line_runs(&bt, &at, false);
    let bv = read::line_runs(&bt, vtext.as_bytes(), false);
    let mut out = Vec::new();
    for r in ba.iter().filter(|r| !r.same) {
        let (s, e) = (r.old.start, r.old.end);
        for m in &bv {
            let point = s == e;
            let hits = if point {
                m.old.start <= s && s <= m.old.end
            } else {
                s < m.old.end && m.old.start < e
            };
            if !hits {
                continue;
            }
            if m.same && point {
                // An insertion point marks the lines on both sides of it.
                let p = (m.new.start + (s - m.old.start)) as u32;
                out.push((p.max(1), p + 1));
            } else if m.same {
                let (lo, hi) = (s.max(m.old.start), e.min(m.old.end));
                let a0 = m.new.start + (lo - m.old.start);
                let a1 = m.new.start + (hi - m.old.start);
                out.push((a0 as u32 + 1, (a1 as u32).max(a0 as u32 + 1)));
            } else {
                let first = m.new.start as u32 + 1;
                let last = (m.new.end as u32).max(first);
                out.push((first, last));
            }
        }
    }
    Some(out)
}

/// Windows of `v` whose lines another workspace's stack changes → those workspaces.
fn also_changed(
    repo: &JjRepo,
    env: &RevsetEnv,
    v: &View,
    stacks: &Stacks,
    exclude: &HashSet<String>,
) -> HashMap<String, Vec<String>> {
    let head = &v.bootstrap.base.view.resolved.head;
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    // Every file version per path (a per-commit range view has several).
    let mut versions: HashMap<&str, Vec<usize>> = HashMap::new();
    for (fi, f) in v.files.iter().enumerate() {
        versions.entry(f.path.as_str()).or_default().push(fi);
    }
    for st in &stacks.list {
        if exclude.contains(&st.name) || st.wc == *head || st.summary.changes == 0 {
            continue;
        }
        if !st.files.iter().any(|p| v.by_path.contains_key(p)) {
            continue;
        }
        // The stack's own commits: not ancestors of what this view shows.
        let own = repo
            .eval(env, &format!("({}) ~ ::{head}", st.revset), 500)
            .unwrap_or_default();
        let Some(root) = own.last() else {
            continue;
        };
        let changes = repo.range_diff(&root.commit, &st.wc).unwrap_or_default();
        for c in &changes {
            for &fi in versions.get(c.path.as_str()).into_iter().flatten() {
                let f = &v.files[fi];
                let lines = mapped_lines(repo, c, &f.text);
                for &w in &f.windows {
                    let h = &v.windows[w].header;
                    let hit = lines.as_ref().is_none_or(|ls| {
                        ls.iter()
                            .any(|&(a, b)| a <= h.lines.end && b >= h.lines.start)
                    });
                    if hit {
                        let e = out.entry(h.id.clone()).or_default();
                        if !e.contains(&st.name) {
                            e.push(st.name.clone());
                        }
                    }
                }
            }
        }
    }
    out
}

// ─── The poller ─────────────────────────────────────────────────────────────

fn strip_volatile(mut card: Value) -> Value {
    if let Some(a) = card.get_mut("agent").and_then(Value::as_object_mut) {
        a.remove("lastSeen");
    }
    card
}

/// The unsnapshotted files of a workspace that view `v` overlays: files the view has (with their
/// text on disk) and new files in a lane the view shows (classified with the revision's lanes).
fn live_files(
    app: &AppState,
    repo: &JjRepo,
    v: &View,
    root: &std::path::Path,
    paths: &[String],
) -> Vec<LiveFile> {
    let mut out: Vec<LiveFile> = Vec::new();
    let mut new: Vec<&String> = Vec::new();
    let fileset = crate::routes::spec_fileset(&v.bootstrap.base.view.spec);
    for p in paths.iter().filter(|p| fileset.matches(p)) {
        if v.by_path.contains_key(p) {
            out.push((p.clone(), read_live(root, p), None));
        } else {
            new.push(p);
        }
    }
    if new.is_empty() {
        return out;
    }
    let cfg = crate::index::lane_config(repo, &v.bootstrap.base.view.resolved.head);
    let classifier = cv_lang::lanes::LaneClassifier::with_config(&app.registry, &cfg);
    let shown: Vec<&str> = v
        .bootstrap
        .base
        .lanes
        .iter()
        .map(|b| b.lane.as_str())
        .collect();
    for p in new {
        if cfg.is_hidden(classifier.classify(p, b"")) {
            continue;
        }
        let Some(text) = read_live(root, p) else {
            continue;
        };
        let lane = classifier.classify(p, text.as_bytes());
        if shown.contains(&lane) {
            out.push((p.clone(), Some(text), Some(lane)));
        }
    }
    out
}

/// Recomputes `alsoChangedIn` of view `id` (shown as `shown`) unless `key` says nothing moved;
/// returns `view.invalidated { reason: "index" }` for the windows whose list changed.
#[allow(clippy::too_many_arguments)]
fn refresh_also(
    app: &AppState,
    repo: &JjRepo,
    env: &RevsetEnv,
    st: &Stacks,
    id: &str,
    shown: &View,
    key: String,
    exclude: &HashSet<String>,
) -> Option<ServerEvent> {
    let prev = app.live.also.read().unwrap().get(id).cloned();
    if prev.as_ref().is_some_and(|p| p.key == key) {
        return None;
    }
    let by_window = also_changed(repo, env, shown, st, exclude);
    let empty = HashMap::new();
    let before = prev.as_ref().map(|p| &p.by_window).unwrap_or(&empty);
    let mut diff: BTreeSet<String> = BTreeSet::new();
    for (w, l) in by_window.iter() {
        if before.get(w) != Some(l) {
            diff.insert(w.clone());
        }
    }
    for w in before.keys() {
        if !by_window.contains_key(w) {
            diff.insert(w.clone());
        }
    }
    app.live
        .also
        .write()
        .unwrap()
        .insert(id.to_string(), Arc::new(AlsoMap { key, by_window }));
    (!diff.is_empty()).then(|| ServerEvent::ViewInvalidated {
        view_id: id.to_string(),
        layout_version: shown.bootstrap.base.view.layout_version,
        reason: "index".into(),
        windows: Some(diff.into_iter().collect()),
        tiles: None,
    })
}

/// One look at the working copies: refreshes live overlays and `alsoChangedIn`, and emits
/// `view.invalidated { reason: "index" }` for windows whose overlay or flags changed (snapshot and
/// range views), `workspace.updated` for cards that changed (files dirtied, overlaps, agent
/// activity, stale or divergent after an operation run elsewhere) and `workspace.recovered` for
/// divergence such an operation left. Returns the events it emitted.
pub fn poll_once(app: &Arc<AppState>) -> Vec<ServerEvent> {
    let Ok(repo) = app.head_repo() else {
        return vec![];
    };
    let env = repo.revset_env();
    let (states, cards) = cards_now(app, &repo);
    let st = stacks(app, &repo, &env, &states);
    let serving = app.repo.workspace_name().to_string();
    let mut events = Vec::new();

    for (id, v) in snapshot_views(app) {
        // Live overlay. A stale workspace's files belong to an older @ (its card says so): they
        // are not overlaid on this one.
        let ws = view_workspace(&v, &states, &serving);
        let files: Vec<LiveFile> = match ws
            .filter(|w| !w.stale)
            .and_then(|w| w.root.as_ref().map(|r| (w, r)))
        {
            Some((w, root)) => live_files(app, &repo, &v, root, &w.unsnapshotted_paths),
            None => vec![],
        };
        let sig = cv_store::fnv64(serde_json::to_string(&files).unwrap_or_default().as_bytes());
        let old = app.live.overlays.read().unwrap().get(&id).cloned();
        let content_changed =
            old.as_ref().map(|o| o.sig) != Some(sig) && !(old.is_none() && files.is_empty());
        let base_moved = old.as_ref().is_some_and(|o| !Arc::ptr_eq(&o.base, &v));
        if content_changed || base_moved {
            let mut touched: BTreeSet<String> = old
                .as_ref()
                .map(|o| o.flagged.iter().cloned().collect())
                .unwrap_or_default();
            if files.is_empty() {
                app.live.overlays.write().unwrap().remove(&id);
            } else {
                let name = ws.map(|w| w.name.clone()).unwrap_or_default();
                let o = Arc::new(build_overlay(&app.registry, &v, name, files, sig));
                touched.extend(o.flagged.iter().cloned());
                app.live.overlays.write().unwrap().insert(id.clone(), o);
            }
            if content_changed && !touched.is_empty() {
                events.push(ServerEvent::ViewInvalidated {
                    view_id: id.clone(),
                    layout_version: v.bootstrap.base.view.layout_version,
                    reason: "index".into(),
                    windows: Some(touched.into_iter().collect()),
                    tiles: None,
                });
            }
        }

        // alsoChangedIn, recomputed when the op, the view or its overlay changed.
        let shown = display_view(app, &id).unwrap_or(v.clone());
        let overlay_sig = app
            .live
            .overlays
            .read()
            .unwrap()
            .get(&id)
            .map(|o| o.sig)
            .unwrap_or(0);
        let key = format!(
            "{}|{}|{}",
            st.op, v.bootstrap.base.view.resolved.head, overlay_sig
        );
        let mut exclude: HashSet<String> = HashSet::new();
        if let Some(w) = ws {
            exclude.insert(w.name.clone());
        }
        events.extend(refresh_also(
            app, &repo, &env, &st, &id, &shown, key, &exclude,
        ));
    }
    // Range views (D16): hunk windows carry alsoChangedIn too. A range has no live overlay; its
    // own stack is never "another" stack (its commits are ancestors of the range's head).
    for r in app.read.ranges() {
        if r.view_ref["status"] == "stale" {
            continue;
        }
        let rv = &r.view.bootstrap.base.view;
        let key = format!("{}|{}|{}", st.op, rv.resolved.head, rv.layout_version);
        events.extend(refresh_also(
            app,
            &repo,
            &env,
            &st,
            &r.id,
            &r.view,
            key,
            &HashSet::new(),
        ));
    }
    events.extend(external_recoveries(app, &states));

    // Cards: emit the ones that changed since the last look (the first look only records).
    let now: HashMap<String, Value> = cards
        .as_array()
        .map(|l| {
            l.iter()
                .map(|c| (c["name"].as_str().unwrap_or("").to_string(), c.clone()))
                .collect()
        })
        .unwrap_or_default();
    {
        let mut last = app.live.cards.lock().unwrap();
        if let Some(prev) = last.as_ref() {
            for (name, card) in &now {
                let changed = prev
                    .get(name)
                    .is_none_or(|p| strip_volatile(p.clone()) != strip_volatile(card.clone()));
                if changed {
                    events.push(ServerEvent::WorkspaceUpdated {
                        workspace: card.clone(),
                    });
                }
            }
        }
        *last = Some(now);
    }
    crate::readside::remember_cards(app, &cards);
    emit(app, events.clone());
    events
}

/// Polls every [`POLL_INTERVAL`] on a background thread until the server state is dropped
/// (`cv serve`; idempotent).
pub fn start(app: &Arc<AppState>) {
    if app.live.started.swap(true, Ordering::SeqCst) {
        return;
    }
    let weak: Weak<AppState> = Arc::downgrade(app);
    std::thread::Builder::new()
        .name("cv-workspaces".into())
        .spawn(move || {
            loop {
                let Some(app) = weak.upgrade() else { break };
                poll_once(&app);
                drop(app);
                std::thread::sleep(POLL_INTERVAL);
            }
        })
        .expect("spawn workspace poller");
}

// ─── Hooks into cv-review ───────────────────────────────────────────────────

/// Thread counts, review marks and agent touches per change (history overlay).
struct History {
    threads: HashMap<String, (u32, bool)>,
    marks: HashMap<String, String>,
}

fn history(r: &Review) -> History {
    let mut threads: HashMap<String, (u32, bool)> = HashMap::new();
    for t in r.db().list_threads(&Default::default()).unwrap_or_default() {
        let e = threads.entry(t.anchor.change.clone()).or_default();
        if matches!(t.status.as_str(), "open" | "in-progress" | "addressed") {
            e.0 += 1;
        }
        e.1 |= matches!(t.created_by, Actor::Agent { .. })
            || t.comments
                .iter()
                .any(|c| matches!(c.author, Actor::Agent { .. }));
    }
    let marks = r
        .db()
        .with(|c| {
            let mut st =
                c.prepare("SELECT change_id, commit_id FROM review_mark ORDER BY marked_at")?;
            let rows = st.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, cv_store::rusqlite::Error>>()
        })
        .unwrap_or_default()
        .into_iter()
        .collect();
    History { threads, marks }
}

/// The history overlay: `GraphRow.review` for changes with threads or a review mark.
pub fn review_hook(r: Weak<Review>) -> crate::readside::ReviewHook {
    let cache: Mutex<Option<(u64, Instant, Arc<History>)>> = Mutex::new(None);
    Arc::new(move |change: &str, commit: &str| {
        let r = r.upgrade()?;
        let cursor = r.events().cursor();
        let h = {
            let mut c = cache.lock().unwrap();
            match c.as_ref() {
                Some((cur, at, h)) if *cur == cursor && at.elapsed() < Duration::from_secs(1) => {
                    h.clone()
                }
                _ => {
                    let h = Arc::new(history(&r));
                    *c = Some((cursor, Instant::now(), h.clone()));
                    h
                }
            }
        };
        let t = h.threads.get(change).copied();
        let mark = h.marks.get(change).cloned();
        if t.is_none() && mark.is_none() {
            return None;
        }
        let (open, touched) = t.unwrap_or_default();
        Some(RevisionReview {
            change: change.to_string(),
            open_threads: open,
            changed_since_review: mark.as_deref().is_some_and(|m| m != commit),
            reviewed_at: mark,
            touched_by_agent: touched,
        })
    })
}

/// Installs the history overlay and the full-card summarizer on a review service.
pub fn install(state: &Arc<AppState>, r: &Arc<Review>) {
    state.read.set_review_hook(review_hook(Arc::downgrade(r)));
    let weak = Arc::downgrade(state);
    r.set_workspace_summarizer(Box::new(move |name: &str| {
        let app = weak.upgrade()?;
        // Called under cv-review's sync lock: read the head without syncing.
        let repo = app.repo.reload().ok()?;
        let (_, cards) = cards_now(&app, &repo);
        cards
            .as_array()?
            .iter()
            .find(|c| c["name"] == name)
            .cloned()
    }));
}
