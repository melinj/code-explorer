//! Graph view operations (http.ts `GET /refs`, `POST /graph:op`, `POST /graph:undo`): bookmarks,
//! landing a workspace's stack on the trunk bookmark and the common history edits, through the same
//! mutation queue as edits. Each applied operation is followed by a sync, so `op.changed`,
//! `view.invalidated` and `workspace.updated` go out as for any other jj operation.

use cv_contracts::review::*;
use cv_vcs::write::{self as vcs, LandStrategy, MutationOutcome, MutationPlan, RebaseMode};

use crate::{Result, Review, ReviewError, jj_error, short};

/// Local bookmark names trunk() is usually spelled with, in order of preference.
const TRUNK_NAMES: [&str; 3] = ["main", "master", "trunk"];

fn invalid(m: impl Into<String>) -> ReviewError {
    crate::invalid(m)
}

/// A refusal as a result: nothing was written.
pub(crate) fn rejected(actor: &Actor, code: &str, message: String) -> GraphOpResult {
    GraphOpResult {
        status: "rejected".into(),
        actor: actor.clone(),
        summary: message.clone(),
        op_before: None,
        op_after: None,
        created: None,
        rewritten: vec![],
        bookmarks: vec![],
        workspaces: vec![],
        conflicts: vec![],
        divergent: vec![],
        error: Some(EditError {
            code: code.into(),
            message,
        }),
    }
}

/// The error code of a cv-vcs refusal.
fn code_of(message: &str) -> &'static str {
    if message.contains("immutable") {
        "immutable"
    } else {
        "invalid"
    }
}

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

impl Review {
    /// GET /refs: every local bookmark, the one "land" goes to by default, and whether the repo is
    /// colocated with git.
    pub fn refs(&self) -> Result<RefsResponse> {
        let bookmarks: Vec<BookmarkInfo> = self
            .vcs
            .bookmarks()
            .map_err(jj_error)?
            .into_iter()
            .map(|b| BookmarkInfo {
                name: b.name,
                targets: b.targets.clone(),
                remotes: b
                    .remotes
                    .into_iter()
                    .map(|r| {
                        // Ahead / behind for a tracked remote with one target on each side.
                        let (ahead, behind) = match (b.targets.as_slice(), r.targets.as_slice()) {
                            ([l], [t]) if r.tracked && l == t => (Some(0), Some(0)),
                            ([l], [t]) if r.tracked => {
                                let n = |revset: String| {
                                    self.vcs.count(&revset, 1000).ok().map(|n| n as u32)
                                };
                                (n(format!("{t}..{l}")), n(format!("{l}..{t}")))
                            }
                            _ => (None, None),
                        };
                        BookmarkRemote {
                            remote: r.remote,
                            targets: r.targets,
                            tracked: r.tracked,
                            ahead,
                            behind,
                        }
                    })
                    .collect(),
            })
            .collect();
        let local: Vec<&BookmarkInfo> =
            bookmarks.iter().filter(|b| !b.targets.is_empty()).collect();
        let rank = |n: &str| {
            TRUNK_NAMES
                .iter()
                .position(|t| *t == n)
                .unwrap_or(TRUNK_NAMES.len())
        };
        // trunk()'s commit (a remote bookmark when there is one): the local bookmark there.
        let at_trunk = self
            .vcs
            .resolve("trunk()")
            .ok()
            .and_then(|v| v.into_iter().next())
            .and_then(|t| {
                local
                    .iter()
                    .filter(|b| b.targets.contains(&t.commit))
                    .min_by_key(|b| rank(&b.name))
                    .map(|b| b.name.clone())
            });
        let trunk = at_trunk.or_else(|| {
            local
                .iter()
                .filter(|b| rank(&b.name) < TRUNK_NAMES.len())
                .min_by_key(|b| rank(&b.name))
                .map(|b| b.name.clone())
        });
        Ok(RefsResponse {
            bookmarks,
            trunk,
            colocated: self.vcs.colocated(),
            remotes: self.vcs.git_remotes().unwrap_or_default(),
        })
    }

    /// The visible commit of the change a client's commit id names (it may have been rewritten
    /// since the client loaded the graph).
    fn current(&self, commit: &str) -> Result<String> {
        Ok(self.visible_commit(commit)?.commit)
    }

    /// The request as a cv-vcs plan, with commits resolved to their visible ones.
    fn graph_plan(&self, op: &GraphOp) -> Result<(vcs::GraphOp, String)> {
        Ok(match op {
            GraphOp::SetBookmark {
                name,
                to,
                allow_backwards,
            } => {
                let to = self.current(to)?;
                (
                    vcs::GraphOp::SetBookmark {
                        name: name.clone(),
                        to: to.clone(),
                        allow_backwards: allow_backwards.unwrap_or(false),
                    },
                    format!("bookmark {name} → {}", short(&to)),
                )
            }
            GraphOp::DeleteBookmark { name } => (
                vcs::GraphOp::DeleteBookmark { name: name.clone() },
                format!("delete bookmark {name}"),
            ),
            GraphOp::Land {
                source,
                trunk,
                strategy,
                message,
                delete_bookmark,
            } => {
                let strategy = match strategy.as_str() {
                    "fast-forward" => LandStrategy::FastForward,
                    "rebase" => LandStrategy::Rebase,
                    "merge" => LandStrategy::Merge,
                    s => {
                        return Err(invalid(format!(
                            "strategy {s:?}: one of {}",
                            LAND_STRATEGIES.join(", ")
                        )));
                    }
                };
                let (head, carry, what) =
                    match (&source.workspace, &source.bookmark, &source.commit) {
                        (Some(w), None, None) => {
                            let at = self
                                .vcs
                                .workspaces()
                                .map_err(jj_error)?
                                .into_iter()
                                .find(|x| x.name == *w)
                                .ok_or_else(|| {
                                    crate::not_found(format!("no workspace named {w}"))
                                })?;
                            let (head, carry) =
                                self.vcs.stack_head(&at.commit).map_err(jj_error)?;
                            (head, carry, format!("{w}@"))
                        }
                        (None, Some(b), None) => {
                            let info = self
                                .vcs
                                .bookmarks()
                                .map_err(jj_error)?
                                .into_iter()
                                .find(|x| x.name == *b)
                                .ok_or_else(|| {
                                    crate::not_found(format!("no bookmark named {b}"))
                                })?;
                            let [t] = info.targets.as_slice() else {
                                return Err(invalid(format!(
                                    "bookmark {b} is conflicted or deleted; set it first"
                                )));
                            };
                            (t.clone(), None, b.clone())
                        }
                        (None, None, Some(c)) => {
                            let c = self.current(c)?;
                            let what = short(&c).to_string();
                            (c, None, what)
                        }
                        _ => {
                            return Err(invalid(
                                "land source: exactly one of workspace, bookmark or commit",
                            ));
                        }
                    };
                (
                    vcs::GraphOp::Land {
                        head,
                        trunk: trunk.clone(),
                        strategy,
                        message: message.clone(),
                        carry,
                        delete_bookmark: delete_bookmark.clone(),
                    },
                    format!("land {what} on {trunk}"),
                )
            }
            GraphOp::Rebase { source, mode, dest } => {
                let (mode, flag) = match mode.as_str() {
                    "revision" => (RebaseMode::Revision, "-r"),
                    "source" => (RebaseMode::Source, "-s"),
                    "branch" => (RebaseMode::Branch, "-b"),
                    m => {
                        return Err(invalid(format!(
                            "mode {m:?}: one of {}",
                            REBASE_MODES.join(", ")
                        )));
                    }
                };
                let source = self.current(source)?;
                let dest: Vec<String> = dest
                    .iter()
                    .map(|d| self.current(d))
                    .collect::<Result<_>>()?;
                let what = format!(
                    "rebase {flag} {} onto {}",
                    short(&source),
                    dest.iter().map(|d| short(d)).collect::<Vec<_>>().join(", ")
                );
                (vcs::GraphOp::Rebase { source, mode, dest }, what)
            }
            GraphOp::Describe { commit, message } => {
                let commit = self.current(commit)?;
                let what = format!("describe {}", short(&commit));
                (
                    vcs::GraphOp::Describe {
                        commit,
                        message: message.clone(),
                    },
                    what,
                )
            }
            GraphOp::Abandon { commits } => {
                let commits: Vec<String> = commits
                    .iter()
                    .map(|c| self.current(c))
                    .collect::<Result<_>>()?;
                let what = format!("abandon {}", plural(commits.len(), "change"));
                (vcs::GraphOp::Abandon { commits }, what)
            }
            GraphOp::New { parents, message } => {
                let parents: Vec<String> = parents
                    .iter()
                    .map(|p| self.current(p))
                    .collect::<Result<_>>()?;
                let what = format!(
                    "new change on {}",
                    parents
                        .iter()
                        .map(|p| short(p))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                (
                    vcs::GraphOp::New {
                        parents,
                        message: message.clone(),
                    },
                    what,
                )
            }
            GraphOp::Commit {
                workspace,
                message,
                bookmark,
            } => {
                let what = match bookmark {
                    Some(b) => format!("commit {workspace}@ and move {b} to it"),
                    None => format!("commit {workspace}@"),
                };
                (
                    vcs::GraphOp::Commit {
                        workspace: workspace.clone(),
                        message: message.clone(),
                        bookmark: bookmark.clone(),
                    },
                    what,
                )
            }
            GraphOp::Squash {
                source,
                into,
                message,
            } => {
                let source = self.current(source)?;
                let into = into.as_deref().map(|c| self.current(c)).transpose()?;
                let what = match &into {
                    Some(i) => format!("squash {} into {}", short(&source), short(i)),
                    None => format!("squash {} into its parent", short(&source)),
                };
                (
                    vcs::GraphOp::Squash {
                        source,
                        into,
                        message: message.clone(),
                    },
                    what,
                )
            }
        })
    }

    /// POST /graph:op: preview (`dryRun`) or apply one Graph view operation. Refusals (bad names,
    /// immutable commits, a backwards bookmark move, a stack that cannot fast-forward) come back as
    /// status "rejected" with nothing written.
    pub fn graph_op(&self, req: GraphOpRequest, actor: &Actor) -> Result<GraphOpResult> {
        let _sync = self.queue()?;
        self.sync_locked(None)?;
        let (op, what) = match self.graph_plan(&req.op) {
            Ok(x) => x,
            Err(e) if e.status < 500 => return Ok(rejected(actor, e.code, e.message)),
            Err(e) => return Err(e),
        };
        let plan = MutationPlan::Graph(op);
        if let Err(e) = self.vcs.check(&plan) {
            return Ok(rejected(actor, code_of(&e.0), e.0));
        }
        if req.dry_run.unwrap_or(false) {
            let out = self.vcs.preview(&plan).map_err(jj_error)?;
            return Ok(self.graph_result("previewed", actor, &what, &out));
        }
        let out = match self
            .vcs
            .mutate(&plan, &format!("cv: {what} ({})", actor.label()))
        {
            Ok(out) => out,
            Err(e) => {
                let mut r = rejected(actor, "jj-error", e.0);
                r.status = "failed".into();
                return Ok(r);
            }
        };
        let result = self.graph_result("applied", actor, &what, &out);
        self.sync_locked(Some((actor, None)))?;
        self.emit_graph_recoveries(&result);
        Ok(result)
    }

    /// POST /graph:undo: `jj op revert <op>`. Refused ("rejected", error "undo-blocked") when the
    /// revert would leave changes divergent or conflicted, e.g. after later operations rewrote them.
    pub fn undo_graph_op(&self, op: &str, actor: &Actor) -> Result<GraphOpResult> {
        let _sync = self.queue()?;
        self.sync_locked(None)?;
        let plan = MutationPlan::Revert { op: op.to_string() };
        if let Err(e) = self.vcs.check(&plan) {
            return Ok(rejected(actor, "invalid", e.0));
        }
        let what = format!("undo operation {}", short(op));
        let preview = self.vcs.preview(&plan).map_err(jj_error)?;
        if !preview.divergent.is_empty() || !preview.new_conflicts.is_empty() {
            let mut r = rejected(
                actor,
                "undo-blocked",
                format!(
                    "undoing {} would leave {}; later operations changed the same commits",
                    short(op),
                    if preview.divergent.is_empty() {
                        plural(preview.new_conflicts.len(), "new conflict")
                    } else {
                        format!("{} divergent", plural(preview.divergent.len(), "change"))
                    }
                ),
            );
            r.divergent = preview.divergent.clone();
            return Ok(r);
        }
        let out = self
            .vcs
            .mutate(&plan, &format!("cv: {what} ({})", actor.label()))
            .map_err(jj_error)?;
        let result = self.graph_result("applied", actor, &what, &out);
        self.sync_locked(Some((actor, None)))?;
        self.emit_graph_recoveries(&result);
        Ok(result)
    }

    pub(crate) fn graph_result(
        &self,
        status: &str,
        actor: &Actor,
        what: &str,
        out: &MutationOutcome,
    ) -> GraphOpResult {
        let first_seen = out
            .op_after
            .clone()
            .unwrap_or_else(|| out.op_before.clone());
        let mut summary = what.to_string();
        if !out.rewritten.is_empty() {
            summary.push_str(&format!(
                " · {} rewritten",
                plural(out.rewritten.len(), "change")
            ));
        }
        if let Some(c) = &out.created {
            summary.push_str(&format!(" · created {}", short(c)));
        }
        if !out.new_conflicts.is_empty() {
            summary.push_str(&format!(
                " · {}",
                plural(out.new_conflicts.len(), "new conflict")
            ));
        }
        GraphOpResult {
            status: status.into(),
            actor: actor.clone(),
            summary,
            op_before: Some(out.op_before.clone()),
            op_after: out.op_after.clone(),
            created: out.created.clone(),
            rewritten: out.rewritten.clone(),
            bookmarks: out
                .bookmarks
                .iter()
                .map(|(name, from, to)| BookmarkMove {
                    name: name.clone(),
                    from: from.clone(),
                    to: to.clone(),
                })
                .collect(),
            workspaces: out
                .workspaces
                .iter()
                .map(|w| EditWorkspace {
                    name: w.name.clone(),
                    snapshotted: w.snapshotted,
                    updated: w.updated,
                    recovered_into: w.recovered_into.clone(),
                })
                .collect(),
            conflicts: out
                .new_conflicts
                .iter()
                .map(|c| ConflictRecord {
                    change: c.change.clone(),
                    commit: c.commit.clone(),
                    path: c.path.clone(),
                    sides: c.sides,
                    first_seen_op: first_seen.clone(),
                    introduced_by: None,
                })
                .collect(),
            divergent: out.divergent.clone(),
            error: None,
        }
    }

    fn emit_graph_recoveries(&self, r: &GraphOpResult) {
        for w in &r.workspaces {
            if let Some(c) = &w.recovered_into {
                self.emit(ServerEvent::WorkspaceRecovered {
                    workspace: w.name.clone(),
                    divergent_commit: c.clone(),
                    note: crate::recovered_note(&w.name, c),
                });
            }
        }
    }
}
