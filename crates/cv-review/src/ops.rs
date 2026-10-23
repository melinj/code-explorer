//! The operation log and the git remote operations of the Graph view (http.ts `GET /ops`,
//! `GET /ops/:op`, `POST /ops:restore`, `POST /git:fetch`, `POST /git:push`). Like graph
//! operations they run in the review queue and are followed by a sync, so `op.changed` and the
//! view invalidations go out as for any other jj operation.

use cv_contracts::review::*;
use cv_vcs::write::{MutationPlan, RemoteOutcome};

use crate::graph::rejected;
use crate::{Result, Review, jj_error, short};

/// Most operations one GET /ops returns.
const MAX_OPS: usize = 1000;

fn moves(o: &RemoteOutcome) -> Vec<BookmarkMove> {
    o.moved
        .iter()
        .map(|m| BookmarkMove {
            name: m.name.clone(),
            from: m.from.clone(),
            to: m.to.clone(),
        })
        .collect()
}

fn failed(what: &str, message: String) -> RemoteResult {
    RemoteResult {
        status: "failed".into(),
        summary: format!("{what} failed"),
        output: String::new(),
        moved: vec![],
        op_after: None,
        error: Some(EditError {
            code: "jj-error".into(),
            message,
        }),
    }
}

impl Review {
    /// GET /ops: the newest `limit` operations (default 100).
    pub fn ops(&self, limit: Option<usize>) -> Result<OpLog> {
        let limit = limit.unwrap_or(100).clamp(1, MAX_OPS);
        let (log, truncated) = self.vcs.op_log(limit).map_err(jj_error)?;
        Ok(OpLog {
            current: self.vcs.head_op().map_err(jj_error)?,
            truncated,
            ops: log
                .into_iter()
                .map(|o| OpEntry {
                    op: o.id,
                    parents: o.parents,
                    time: cv_store::iso_from_ms(o.time_ms),
                    description: o.description,
                    user: o.user,
                    snapshot: o.is_snapshot,
                })
                .collect(),
        })
    }

    /// GET /ops/:op: what the operation changed against its first parent.
    pub fn op_detail(&self, op: &str) -> Result<OpDetail> {
        let info = self
            .vcs
            .op_info(op)
            .map_err(|e| crate::not_found(format!("unknown operation {op}: {e}")))?;
        let Some(parent) = info.parents.first() else {
            return Ok(OpDetail {
                op: info.id,
                changed: vec![],
                abandoned: vec![],
                bookmarks: vec![],
            });
        };
        let d = self.vcs.op_diff(parent, &info.id).map_err(jj_error)?;
        Ok(OpDetail {
            op: info.id,
            changed: d.changed,
            abandoned: d.abandoned,
            bookmarks: d
                .bookmarks
                .into_iter()
                .map(|(name, from, to)| BookmarkMove { name, from, to })
                .collect(),
        })
    }

    /// POST /ops:restore: `jj op restore <op>`, previewed with `dryRun`.
    pub fn restore_op(&self, req: RestoreRequest, actor: &Actor) -> Result<GraphOpResult> {
        let _sync = self.queue()?;
        self.sync_locked(None)?;
        let plan = MutationPlan::Restore { op: req.op.clone() };
        if let Err(e) = self.vcs.check(&plan) {
            return Ok(rejected(actor, "invalid", e.0));
        }
        let what = format!("restore the repo to operation {}", short(&req.op));
        if req.dry_run.unwrap_or(false) {
            let out = self.vcs.preview(&plan).map_err(jj_error)?;
            return Ok(self.graph_result("previewed", actor, &what, &out));
        }
        let out = self
            .vcs
            .mutate(&plan, &format!("cv: {what} ({})", actor.label()))
            .map_err(jj_error)?;
        let result = self.graph_result("applied", actor, &what, &out);
        self.sync_locked(Some((actor, None)))?;
        Ok(result)
    }

    /// POST /git:fetch.
    pub fn git_fetch(&self, req: FetchRequest, actor: &Actor) -> Result<RemoteResult> {
        let _sync = self.queue()?;
        self.sync_locked(None)?;
        let from = req
            .remote
            .clone()
            .unwrap_or_else(|| "the default remote".into());
        let out = match self.vcs.git_fetch(req.remote.as_deref()) {
            Ok(o) => o,
            Err(e) => return Ok(failed("fetch", e.0)),
        };
        self.sync_locked(Some((actor, None)))?;
        let summary = match out.moved.len() {
            0 => format!("fetched from {from}: nothing new"),
            n => format!(
                "fetched from {from}: {n} bookmark{} moved",
                if n == 1 { "" } else { "s" }
            ),
        };
        Ok(RemoteResult {
            status: "applied".into(),
            summary,
            moved: moves(&out),
            op_after: (out.op_after != out.op_before).then_some(out.op_after.clone()),
            output: out.output,
            error: None,
        })
    }

    /// POST /git:push (a dry run first, from the dialog).
    pub fn git_push(&self, req: PushRequest, actor: &Actor) -> Result<RemoteResult> {
        let _sync = self.queue()?;
        self.sync_locked(None)?;
        let dry = req.dry_run.unwrap_or(false);
        let out = match self
            .vcs
            .git_push(&req.bookmarks, req.remote.as_deref(), dry)
        {
            Ok(o) => o,
            Err(e) => return Ok(failed(if dry { "push preview" } else { "push" }, e.0)),
        };
        self.sync_locked(if dry { None } else { Some((actor, None)) })?;
        let names = req.bookmarks.join(", ");
        let to = req
            .remote
            .clone()
            .unwrap_or_else(|| "the default remote".into());
        Ok(RemoteResult {
            status: if dry { "previewed" } else { "applied" }.into(),
            summary: if dry {
                format!("would push {names} to {to}")
            } else {
                format!("pushed {names} to {to}")
            },
            moved: moves(&out),
            op_after: (!dry && out.op_after != out.op_before).then_some(out.op_after.clone()),
            output: out.output,
            error: None,
        })
    }
}
